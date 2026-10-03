//! scRGB (HDR desktop) -> sRGB 8-bit conversion that removes the HDR look.
//! The algorithm is specified in DESIGN.md §4; keep this file in sync with it.

use std::sync::OnceLock;

use half::f16;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::geom::RectI;
use crate::image::{HdrImage, Image};

const HISTOGRAM_BINS: usize = 2048;
const CURVE_LUT_SIZE: usize = 4096;
const QUANTIZER_LUT_SIZE: usize = 4096;
const LOG_MIN: f32 = -16.0;
const LOG_MAX: f32 = 8.0;
const HDR_THRESHOLD: f32 = 1.0 + 0.5 / 255.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToneMapMode {
    /// SDR content stays bit-exact; HDR highlights are compressed with the BT.2390 EETF.
    #[default]
    Auto,
    /// SDR content stays bit-exact; anything brighter than SDR white is clamped per channel.
    Clip,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToneMapParams {
    pub mode: ToneMapMode,
    pub exposure_stops: f32,
}

impl Default for ToneMapParams {
    fn default() -> Self {
        Self {
            mode: ToneMapMode::Auto,
            exposure_stops: 0.0,
        }
    }
}

/// Brightness statistics, in units of SDR white (1.0 = the monitor's SDR white level), before exposure.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HdrStats {
    /// 99.9th percentile of max(r, g, b).
    pub peak: f32,
    /// Maximum of max(r, g, b).
    pub max: f32,
    /// Fraction of pixels with max(r, g, b) above SDR white (+0.5/255 tolerance).
    pub hdr_fraction: f32,
    /// Fraction of pixels with any negative channel (outside BT.709).
    pub out_of_gamut_fraction: f32,
}

impl HdrStats {
    /// True when the content has highlights brighter than SDR white worth tone mapping.
    pub fn has_hdr_content(&self) -> bool {
        self.peak > HDR_THRESHOLD
    }
}

pub fn analyze(img: &HdrImage) -> HdrStats {
    analyze_bounds(img, img.bounds())
}

pub fn analyze_region(img: &HdrImage, region: RectI) -> HdrStats {
    let Some(region) = region.intersect(&img.bounds()) else {
        return HdrStats::default();
    };
    analyze_bounds(img, region)
}

/// Tone maps the whole image, analysing it first in Auto mode.
pub fn tonemap(img: &HdrImage, params: &ToneMapParams) -> Image {
    let stats = analyze(img);
    tonemap_with_stats(img, params, &stats)
}

/// Tone maps with precomputed stats (e.g. stats of a larger area for a stable preview).
pub fn tonemap_with_stats(img: &HdrImage, params: &ToneMapParams, stats: &HdrStats) -> Image {
    let mut output = Image::new(img.width, img.height);
    if img.width == 0 || img.height == 0 {
        return output;
    }

    let input_stride = img.width as usize * 4;
    assert_eq!(img.data.len(), input_stride * img.height as usize);

    let exposure = 2.0_f32.powf(params.exposure_stops);
    let scale = sdr_normalization(img.sdr_white_nits) * exposure;
    let curve = (params.mode == ToneMapMode::Auto && stats.peak * exposure > HDR_THRESHOLD)
        .then(|| CurveLut::new(stats.peak * exposure, img.sdr_white_nits));
    let half_values = f16_values();

    img.data
        .par_chunks_exact(input_stride)
        .zip(output.data.par_chunks_exact_mut(input_stride))
        .for_each(|(input_row, output_row)| {
            for (input, output) in input_row
                .chunks_exact(4)
                .zip(output_row.chunks_exact_mut(4))
            {
                let r_bits = input[0].to_bits();
                let g_bits = input[1].to_bits();
                let b_bits = input[2].to_bits();
                let r = half_values[r_bits as usize];
                let g = half_values[g_bits as usize];
                let b = half_values[b_bits as usize];
                let mut rgb = [r * scale, g * scale, b * scale];
                let has_negative = (r_bits | g_bits | b_bits) & 0x8000 != 0;
                if has_negative {
                    fix_negative_gamut(&mut rgb);
                }

                if let Some(curve) = &curve {
                    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
                    if maximum > 0.0 {
                        let mapped = curve.map(maximum);
                        let ratio = mapped / maximum;
                        for channel in &mut rgb {
                            *channel *= ratio;
                        }
                    }
                }

                output[0] = quantize_srgb(rgb[2].clamp(0.0, 1.0));
                output[1] = quantize_srgb(rgb[1].clamp(0.0, 1.0));
                output[2] = quantize_srgb(rgb[0].clamp(0.0, 1.0));
                output[3] = 255;
            }
        });

    output
}

/// Crops `region` and tone maps it with the region's own stats.
/// This is what a final snip uses, so an SDR-only region stays bit-exact even when HDR video is elsewhere on screen.
pub fn tonemap_region(img: &HdrImage, region: RectI, params: &ToneMapParams) -> Image {
    let cropped = img.crop(region);
    tonemap(&cropped, params)
}

fn analyze_bounds(img: &HdrImage, region: RectI) -> HdrStats {
    if region.is_empty() || img.width == 0 || img.height == 0 {
        return HdrStats::default();
    }

    let row_stride = img.width as usize * 4;
    assert_eq!(img.data.len(), row_stride * img.height as usize);
    let first_row = region.y as usize;
    let last_row = first_row + region.h as usize;
    let first_component = region.x as usize * 4;
    let component_count = region.w as usize * 4;
    let normalization = sdr_normalization(img.sdr_white_nits);
    let half_values = f16_values();

    let accumulated = (first_row..last_row)
        .into_par_iter()
        .fold(StatsAccumulator::new, |mut accumulator, row| {
            let start = row * row_stride + first_component;
            accumulator.add_pixels(
                &img.data[start..start + component_count],
                normalization,
                half_values,
            );
            accumulator
        })
        .reduce(StatsAccumulator::new, StatsAccumulator::merge);

    accumulated.finish()
}

struct StatsAccumulator {
    histogram: Box<[u64; HISTOGRAM_BINS]>,
    count: u64,
    hdr_count: u64,
    out_of_gamut_count: u64,
    maximum: f32,
}

impl StatsAccumulator {
    fn new() -> Self {
        Self {
            histogram: Box::new([0; HISTOGRAM_BINS]),
            count: 0,
            hdr_count: 0,
            out_of_gamut_count: 0,
            maximum: 0.0,
        }
    }

    fn add_pixels(&mut self, pixels: &[f16], normalization: f32, half_values: &[f32; 65_536]) {
        for pixel in pixels.chunks_exact(4) {
            let r = half_values[pixel[0].to_bits() as usize];
            let g = half_values[pixel[1].to_bits() as usize];
            let b = half_values[pixel[2].to_bits() as usize];
            let maximum = r.max(g).max(b).max(0.0) * normalization;
            let bin = histogram_bin(maximum);

            self.histogram[bin] += 1;
            self.count += 1;
            self.hdr_count += u64::from(maximum > HDR_THRESHOLD);
            self.out_of_gamut_count += u64::from(r < 0.0 || g < 0.0 || b < 0.0);
            self.maximum = self.maximum.max(maximum);
        }
    }

    fn merge(mut self, other: Self) -> Self {
        for (target, value) in self.histogram.iter_mut().zip(other.histogram.iter()) {
            *target += value;
        }
        self.count += other.count;
        self.hdr_count += other.hdr_count;
        self.out_of_gamut_count += other.out_of_gamut_count;
        self.maximum = self.maximum.max(other.maximum);
        self
    }

    fn finish(self) -> HdrStats {
        if self.count == 0 {
            return HdrStats::default();
        }

        let percentile_rank = (self.count * 999).div_ceil(1000);
        let mut cumulative = 0;
        let mut percentile_bin = 0;
        for (index, count) in self.histogram.iter().enumerate() {
            cumulative += count;
            if cumulative >= percentile_rank {
                percentile_bin = index;
                break;
            }
        }
        let peak = if self.maximum <= 0.0 {
            0.0
        } else {
            let bin_width = (LOG_MAX - LOG_MIN) / HISTOGRAM_BINS as f32;
            2.0_f32.powf(LOG_MIN + (percentile_bin as f32 + 0.5) * bin_width)
        };
        let count = self.count as f32;

        HdrStats {
            peak,
            max: self.maximum,
            hdr_fraction: self.hdr_count as f32 / count,
            out_of_gamut_fraction: self.out_of_gamut_count as f32 / count,
        }
    }
}

struct CurveLut {
    values: Box<[f32; CURVE_LUT_SIZE]>,
}

impl CurveLut {
    fn new(source_peak: f32, sdr_white_nits: f32) -> Self {
        let mut values = Box::new([0.0; CURVE_LUT_SIZE]);
        let step = (LOG_MAX - LOG_MIN) / (CURVE_LUT_SIZE - 1) as f32;
        for (index, value) in values.iter_mut().enumerate() {
            let maximum = 2.0_f32.powf(LOG_MIN + index as f32 * step);
            *value = bt2390(maximum, source_peak, sdr_white_nits);
        }
        Self { values }
    }

    fn map(&self, maximum: f32) -> f32 {
        if maximum <= 2.0_f32.powf(LOG_MIN) {
            return maximum;
        }
        let position = ((fast_log2(maximum) - LOG_MIN) * (CURVE_LUT_SIZE - 1) as f32
            / (LOG_MAX - LOG_MIN))
            .clamp(0.0, (CURVE_LUT_SIZE - 1) as f32);
        let lower = position as usize;
        let upper = (lower + 1).min(CURVE_LUT_SIZE - 1);
        let fraction = position - lower as f32;
        self.values[lower] + (self.values[upper] - self.values[lower]) * fraction
    }
}

fn bt2390(maximum: f32, source_peak: f32, sdr_white_nits: f32) -> f32 {
    let source_white = pq_encode(source_peak * sdr_white_nits);
    let target_white = pq_encode(sdr_white_nits);
    if source_white <= 0.0 {
        return maximum.min(1.0);
    }

    let normalized = (pq_encode(maximum * sdr_white_nits) / source_white).clamp(0.0, 1.0);
    let maximum_luminance = (target_white / source_white).clamp(0.0, 1.0);
    let knee = (1.5 * maximum_luminance - 0.5).max(0.0);
    if normalized <= knee || knee >= 1.0 {
        return maximum.min(1.0);
    }

    let t = (normalized - knee) / (1.0 - knee);
    let t2 = t * t;
    let t3 = t2 * t;
    let mapped = (2.0 * t3 - 3.0 * t2 + 1.0) * knee
        + (t3 - 2.0 * t2 + t) * (1.0 - knee)
        + (-2.0 * t3 + 3.0 * t2) * maximum_luminance;
    (pq_decode(mapped * source_white) / sdr_white_nits).clamp(0.0, 1.0)
}

fn pq_encode(nits: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 32.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 128.0;
    const C3: f32 = 2392.0 / 128.0;

    let luminance = (nits.max(0.0) / 10_000.0).powf(M1);
    ((C1 + C2 * luminance) / (1.0 + C3 * luminance)).powf(M2)
}

fn pq_decode(signal: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 32.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 128.0;
    const C3: f32 = 2392.0 / 128.0;

    let power = signal.max(0.0).powf(1.0 / M2);
    let numerator = (power - C1).max(0.0);
    let denominator = (C2 - C3 * power).max(f32::MIN_POSITIVE);
    10_000.0 * (numerator / denominator).powf(1.0 / M1)
}

fn fix_negative_gamut(rgb: &mut [f32; 3]) {
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    if minimum >= 0.0 {
        return;
    }

    let luminance = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    if luminance <= 0.0 {
        *rgb = [0.0; 3];
        return;
    }

    let saturation = luminance / (luminance - minimum);
    for channel in rgb {
        *channel = luminance + (*channel - luminance) * saturation;
    }
}

fn quantize_srgb(linear: f32) -> u8 {
    if linear >= 1.0 {
        return 255;
    }

    let index = (linear * QUANTIZER_LUT_SIZE as f32) as usize;
    let entry = srgb_quantizer()[index];
    let code = entry as u8;
    if entry & 0x100 != 0 {
        code + u8::from(linear >= srgb_midpoints()[code as usize])
    } else {
        code
    }
}

fn srgb_eotf(encoded: f32) -> f32 {
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_midpoints() -> &'static [f32; 255] {
    static MIDPOINTS: OnceLock<Box<[f32; 255]>> = OnceLock::new();
    MIDPOINTS.get_or_init(|| {
        let mut values = Box::new([0.0; 255]);
        for (code, value) in values.iter_mut().enumerate() {
            *value = srgb_eotf((code as f32 + 0.5) / 255.0);
        }
        values
    })
}

fn srgb_quantizer() -> &'static [u16; QUANTIZER_LUT_SIZE] {
    static ENTRIES: OnceLock<Box<[u16; QUANTIZER_LUT_SIZE]>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let thresholds = srgb_midpoints();
        let mut entries = vec![0; QUANTIZER_LUT_SIZE].into_boxed_slice();
        for (index, entry) in entries.iter_mut().enumerate() {
            let lower = index as f32 / QUANTIZER_LUT_SIZE as f32;
            let upper = (index + 1) as f32 / QUANTIZER_LUT_SIZE as f32;
            let code = thresholds.partition_point(|threshold| lower >= *threshold);
            let contains_threshold = code < 255 && thresholds[code] < upper;
            *entry = code as u16 | if contains_threshold { 0x100 } else { 0 };
        }
        match entries.try_into() {
            Ok(entries) => entries,
            Err(_) => unreachable!(),
        }
    })
}

fn f16_values() -> &'static [f32; 65_536] {
    static VALUES: OnceLock<Box<[f32; 65_536]>> = OnceLock::new();
    VALUES.get_or_init(|| {
        let mut values = vec![0.0; 65_536].into_boxed_slice();
        for (bits, value) in values.iter_mut().enumerate() {
            let converted = f16::from_bits(bits as u16).to_f32();
            *value = if converted.is_nan() {
                0.0
            } else {
                converted.clamp(-65_504.0, 65_504.0)
            };
        }
        match values.try_into() {
            Ok(values) => values,
            Err(_) => unreachable!(),
        }
    })
}

fn sdr_normalization(sdr_white_nits: f32) -> f32 {
    if sdr_white_nits.is_finite() && sdr_white_nits > 0.0 {
        80.0 / sdr_white_nits
    } else {
        1.0
    }
}

fn histogram_bin(maximum: f32) -> usize {
    if maximum <= 0.0 {
        return 0;
    }
    (((fast_log2(maximum) - LOG_MIN) * HISTOGRAM_BINS as f32 / (LOG_MAX - LOG_MIN)) as usize)
        .min(HISTOGRAM_BINS - 1)
}

fn fast_log2(value: f32) -> f32 {
    const INDEX_BITS: u32 = 11;
    const FRACTION_BITS: u32 = 23 - INDEX_BITS;
    const INDEX_MASK: u32 = (1 << INDEX_BITS) - 1;
    const FRACTION_MASK: u32 = (1 << FRACTION_BITS) - 1;

    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127;
    let mantissa = bits & 0x7f_ffff;
    let index = ((mantissa >> FRACTION_BITS) & INDEX_MASK) as usize;
    let fraction = (mantissa & FRACTION_MASK) as f32 / (1 << FRACTION_BITS) as f32;
    let logarithms = mantissa_log2();
    exponent as f32 + logarithms[index] + (logarithms[index + 1] - logarithms[index]) * fraction
}

fn mantissa_log2() -> &'static [f32; 2049] {
    static LOGARITHMS: OnceLock<Box<[f32; 2049]>> = OnceLock::new();
    LOGARITHMS.get_or_init(|| {
        let mut logarithms = Box::new([0.0; 2049]);
        for (index, logarithm) in logarithms.iter_mut().enumerate() {
            *logarithm = (1.0 + index as f32 / 2048.0).log2();
        }
        logarithms
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use super::*;

    fn hdr_image(
        width: u32,
        height: u32,
        sdr_white_nits: f32,
        values: impl IntoIterator<Item = f32>,
    ) -> HdrImage {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for value in values {
            let value = f16::from_f32(value);
            data.extend_from_slice(&[value, value, value, f16::ONE]);
        }
        HdrImage {
            width,
            height,
            data,
            sdr_white_nits,
            display_peak_nits: 2_000.0,
        }
    }

    #[test]
    fn sdr_codes_round_trip_exactly() {
        for sdr_white_nits in [80.0, 120.0, 200.0, 240.0, 250.5, 480.0] {
            let repeats = 16;
            let mut values = Vec::with_capacity(256 * repeats + 1);
            for code in 0..=255 {
                values.extend(std::iter::repeat_n(
                    srgb_eotf(code as f32 / 255.0) * sdr_white_nits / 80.0,
                    repeats,
                ));
            }
            values.push(1_000.0 / 80.0);
            let image = hdr_image(values.len() as u32, 1, sdr_white_nits, values);

            for mode in [ToneMapMode::Auto, ToneMapMode::Clip] {
                let mapped = tonemap(
                    &image,
                    &ToneMapParams {
                        mode,
                        exposure_stops: 0.0,
                    },
                );
                for code in 0..=255_u32 {
                    let pixel = mapped.pixel(code * repeats as u32, 0);
                    assert_eq!(
                        pixel,
                        [code as u8, code as u8, code as u8, 255],
                        "white={sdr_white_nits}, mode={mode:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn fast_srgb_quantizer_matches_all_midpoint_thresholds() {
        let thresholds = srgb_midpoints();
        for sample in 0..=1_000_000 {
            let linear = sample as f32 / 1_000_000.0;
            let expected = thresholds.partition_point(|threshold| linear >= *threshold) as u8;
            assert_eq!(quantize_srgb(linear), expected, "linear={linear}");
        }
    }

    #[test]
    fn auto_curve_is_monotonic_and_preserves_highlight_gradation() {
        let sdr_white_nits = 200.0;
        let samples = 16_384;
        let values = (0..samples).map(|index| {
            let nits = 2_000.0 * index as f32 / (samples - 1) as f32;
            nits / 80.0
        });
        let image = hdr_image(samples, 1, sdr_white_nits, values);
        let mapped = tonemap(&image, &ToneMapParams::default());
        let codes: Vec<u8> = mapped.data.chunks_exact(4).map(|pixel| pixel[0]).collect();

        assert!(codes.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(codes.last(), Some(&255));
        let half_white = quantize_srgb(0.5);
        let highlights: HashSet<u8> = codes
            .into_iter()
            .filter(|code| *code > half_white)
            .collect();
        assert!(
            highlights.len() >= 40,
            "only {} highlight codes",
            highlights.len()
        );
    }

    #[test]
    fn negative_gamut_fix_preserves_luminance() {
        let mut rgb = [-0.2, 0.8, 0.3];
        let before = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        fix_negative_gamut(&mut rgb);
        let after = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];

        assert!(rgb.into_iter().all(|channel| channel >= 0.0));
        assert!(((after - before) / before).abs() < 0.01);
    }

    #[test]
    fn analysis_uses_the_999th_percentile_histogram_bin() {
        let mut values = vec![0.5; 10_000];
        values[9_980..9_990].fill(2.0);
        values[9_990..].fill(8.0);
        let image = hdr_image(10_000, 1, 80.0, values);
        let stats = analyze(&image);
        let bin_width = (LOG_MAX - LOG_MIN) / HISTOGRAM_BINS as f32;

        assert!((stats.peak.log2() - 2.0_f32.log2()).abs() <= bin_width);
        assert!((stats.max - 8.0).abs() < f32::EPSILON);
        assert!((stats.hdr_fraction - 0.002).abs() < 0.000_001);
    }

    #[test]
    fn region_uses_its_own_statistics_and_crop() {
        let image = hdr_image(2, 1, 80.0, [0.5, 10.0]);
        let mapped = tonemap_region(&image, RectI::new(0, 0, 1, 1), &ToneMapParams::default());
        assert_eq!(mapped.width, 1);
        assert_eq!(mapped.height, 1);
        assert_eq!(mapped.pixel(0, 0), [188, 188, 188, 255]);
    }

    #[test]
    #[ignore = "release-only performance budget"]
    fn tone_maps_4k_under_60_ms_in_release() {
        let width = 3_840;
        let height = 2_160;
        let pixel = [
            f16::from_f32(10.0),
            f16::from_f32(5.0),
            f16::from_f32(2.5),
            f16::ONE,
        ];
        let image = HdrImage {
            width,
            height,
            data: pixel.repeat(width as usize * height as usize),
            sdr_white_nits: 200.0,
            display_peak_nits: 2_000.0,
        };
        let params = ToneMapParams::default();
        let stats = analyze(&image);
        let _ = tonemap_with_stats(&hdr_image(1, 1, 200.0, [10.0]), &params, &stats);

        let (output, elapsed) = (0..3)
            .map(|_| {
                let start = Instant::now();
                let output = tonemap_with_stats(&image, &params, &stats);
                (output, start.elapsed())
            })
            .min_by_key(|(_, elapsed)| *elapsed)
            .unwrap();
        assert_eq!(output.data.len(), width as usize * height as usize * 4);
        eprintln!("4K tone map: {:.3} ms", elapsed.as_secs_f64() * 1_000.0);
        assert!(elapsed.as_millis() < 60, "4K tone map took {elapsed:?}");
    }
}
