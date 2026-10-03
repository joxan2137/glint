//! From a captured GPU texture to CPU images. FP16 frames are tone mapped on the GPU (statistics pass, curve, tone map
//! pass) while the raw FP16 copy streams back in parallel; the CPU tone map of glint-core is the fallback for rotated
//! outputs and for devices without compute shaders.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use glint_core::settings::HdrSettings;
use glint_core::tonemap::{ToneMapMode, ToneMapParams, analyze, tonemap_with_stats};
use glint_core::{HdrImage, HdrStats, Image, MonitorInfo, f16};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT};

use crate::gpu::{Gpu, Mapped, stage};
use crate::orient::Rotation;
use crate::tonemap_gpu::{
    HDR_THRESHOLD, Params, Queued, STATS_WORDS, lut_step, percentile_rank, sdr_normalization, stats_from_counters,
};

static GPU_TONEMAP: AtomicBool = AtomicBool::new(true);

const DEFAULT_SDR_WHITE_NITS: f32 = 80.0;
const DEFAULT_DISPLAY_PEAK_NITS: f32 = 1000.0;

/// Switches the GPU tone map off (the CPU tone map of glint-core is used instead) or back on; on by default.
pub fn set_gpu_tonemap(enabled: bool) {
    GPU_TONEMAP.store(enabled, Ordering::Relaxed);
}

/// Wall time the capture thread spent per stage. GPU work that overlaps CPU work shows up only where the CPU waits.
#[derive(Clone, Copy, Debug, Default)]
pub struct StageTimings {
    /// Creating the duplication, or the capture item, frame pool and session.
    pub setup: Duration,
    /// Waiting for the first usable frame.
    pub acquire: Duration,
    /// Queueing GPU copies and blocking until the GPU finished one that the CPU needs.
    pub copy_map: Duration,
    /// CPU copies out of mapped memory (and the alpha fix of BGRA frames).
    pub convert: Duration,
    /// Statistics, curve and tone map orchestration (CPU tone map on the fallback path).
    pub tonemap: Duration,
}

/// Images of one monitor capture. `hdr` and `stats` are present for FP16 frames.
pub struct Grabbed {
    pub sdr: Image,
    pub hdr: Option<HdrImage>,
    pub stats: Option<HdrStats>,
}

/// A frame whose GPU copies are queued; the source texture may be released right after `begin`.
pub enum InFlight {
    Bgra { staging: ID3D11Texture2D, rotation: Rotation },
    Fp16Cpu { staging: ID3D11Texture2D, rotation: Rotation },
    Fp16Gpu(Box<GpuFp16>),
}

pub struct GpuFp16 {
    width: u32,
    height: u32,
    queued: Queued,
    /// CPU-readable copy of the raw FP16 pixels, queued behind the tone map.
    raw: ID3D11Texture2D,
}

/// Queues every GPU command needed for `source` in one batch: for FP16 the statistics, curve and tone map passes with
/// their readback copies and then the raw copy, so there is a single wait for the GPU; the source may be released
/// right after this returns.
pub fn begin(
    gpu: &Gpu,
    source: &ID3D11Texture2D,
    monitor: &MonitorInfo,
    rotation: Rotation,
    hdr: &HdrSettings,
    clock: &mut StageTimings,
) -> Result<InFlight> {
    let started = Instant::now();
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: GetDesc only fills the passed struct.
    unsafe { source.GetDesc(&mut desc) };
    let (desktop_w, desktop_h) = rotation.desktop_size(desc.Width as usize, desc.Height as usize);
    ensure!(
        (desktop_w as i32, desktop_h as i32) == (monitor.rect.w, monitor.rect.h),
        "captured desktop is {desktop_w}x{desktop_h} but the monitor rect is {}x{}",
        monitor.rect.w,
        monitor.rect.h
    );

    let _commands = gpu.commands();
    let in_flight = match desc.Format {
        DXGI_FORMAT_B8G8R8A8_UNORM => InFlight::Bgra { staging: stage(gpu, source)?, rotation },
        DXGI_FORMAT_R16G16B16A16_FLOAT => {
            let use_gpu = rotation == Rotation::Identity && GPU_TONEMAP.load(Ordering::Relaxed);
            let gpu_frame = use_gpu.then(|| begin_gpu(gpu, source, &desc, monitor, hdr, clock));
            match gpu_frame {
                Some(Ok(frame)) => InFlight::Fp16Gpu(Box::new(frame)),
                failed => {
                    if let Some(Err(error)) = failed {
                        log::warn!("{}: GPU tone map unavailable ({error:#}); using the CPU", monitor.device_name);
                    }
                    InFlight::Fp16Cpu { staging: stage(gpu, source)?, rotation }
                }
            }
        }
        other => bail!("unsupported capture format {}", other.0),
    };
    // SAFETY: plain flush on a live context; starts the queued copies.
    unsafe { gpu.context().Flush() };
    clock.copy_map += started.elapsed();
    Ok(in_flight)
}

fn begin_gpu(
    gpu: &Gpu,
    source: &ID3D11Texture2D,
    source_desc: &D3D11_TEXTURE2D_DESC,
    monitor: &MonitorInfo,
    hdr: &HdrSettings,
    clock: &mut StageTimings,
) -> Result<GpuFp16> {
    let tone_mapper = gpu.tone_mapper()?;
    let work_desc = D3D11_TEXTURE2D_DESC {
        MipLevels: 1,
        ArraySize: 1,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
        ..*source_desc
    };
    let mut work_texture = None;
    // SAFETY: the descriptor is fully initialised and the out pointer is valid.
    unsafe { gpu.device().CreateTexture2D(&work_desc, None, Some(&mut work_texture)) }.context("create work texture")?;
    let work_texture = work_texture.context("CreateTexture2D returned no texture")?;
    let mut work = None;
    // SAFETY: a null description views the whole texture.
    unsafe { gpu.device().CreateShaderResourceView(&work_texture, None, Some(&mut work)) }
        .context("create work texture view")?;
    let work = work.context("no work texture view")?;
    // SAFETY: both textures belong to this device and have identical size and format.
    unsafe { gpu.context().CopyResource(&work_texture, source) };

    let started = Instant::now();
    let (width, height) = (source_desc.Width, source_desc.Height);
    let constants = Constants::new(monitor, hdr, width, height);
    let queued = tone_mapper.queue(gpu, &work, &constants.params(hdr.mode == ToneMapMode::Auto))?;
    clock.tonemap += started.elapsed();
    let raw = stage(gpu, &work_texture)?;
    Ok(GpuFp16 { width, height, queued, raw })
}

/// Per-capture tone map constants, computed exactly as glint-core does.
struct Constants {
    width: u32,
    height: u32,
    sdr_white_nits: f32,
    normalization: f32,
    exposure: f32,
}

impl Constants {
    fn new(monitor: &MonitorInfo, hdr: &HdrSettings, width: u32, height: u32) -> Self {
        let sdr_white_nits = monitor.hdr.map_or(DEFAULT_SDR_WHITE_NITS, |info| info.sdr_white_nits);
        Self {
            width,
            height,
            sdr_white_nits,
            normalization: sdr_normalization(sdr_white_nits),
            exposure: 2.0_f32.powf(hdr.exposure_stops),
        }
    }

    fn params(&self, auto_mode: bool) -> Params {
        Params {
            width: self.width,
            height: self.height,
            percentile_rank: percentile_rank(u64::from(self.width) * u64::from(self.height)),
            auto_mode: u32::from(auto_mode),
            scale: self.normalization * self.exposure,
            normalization: self.normalization,
            hdr_threshold: HDR_THRESHOLD,
            exposure: self.exposure,
            sdr_white_nits: self.sdr_white_nits,
            lut_step: lut_step(),
            unused: [0.0; 2],
        }
    }
}

/// Waits for the queued work, tone maps and reads everything back. `on_sdr` runs as soon as the `sdr` image exists,
/// before the raw FP16 data is copied out.
pub fn finish(
    in_flight: InFlight,
    gpu: &Gpu,
    monitor: &MonitorInfo,
    hdr: &HdrSettings,
    on_sdr: &dyn Fn(&Image),
    clock: &mut StageTimings,
) -> Result<Grabbed> {
    match in_flight {
        InFlight::Bgra { staging, rotation } => {
            let (pixels, width, height) = read_texture::<[u8; 4]>(gpu, &staging, rotation, clock)?;
            let started = Instant::now();
            let mut data = pixels.into_flattened();
            data.chunks_exact_mut(4).for_each(|pixel| pixel[3] = 255);
            let sdr = Image::from_bgra(width as u32, height as u32, data);
            clock.convert += started.elapsed();
            on_sdr(&sdr);
            Ok(Grabbed { sdr, hdr: None, stats: None })
        }
        InFlight::Fp16Cpu { staging, rotation } => {
            let (pixels, width, height) = read_texture::<[f16; 4]>(gpu, &staging, rotation, clock)?;
            let started = Instant::now();
            let image = hdr_image(monitor, width as u32, height as u32, pixels.into_flattened());
            let params = ToneMapParams { mode: hdr.mode, exposure_stops: hdr.exposure_stops };
            let stats = analyze(&image);
            let sdr = tonemap_with_stats(&image, &params, &stats);
            clock.tonemap += started.elapsed();
            on_sdr(&sdr);
            Ok(Grabbed { sdr, hdr: Some(image), stats: Some(stats) })
        }
        InFlight::Fp16Gpu(frame) => finish_gpu(*frame, gpu, monitor, on_sdr, clock),
    }
}

fn finish_gpu(
    frame: GpuFp16,
    gpu: &Gpu,
    monitor: &MonitorInfo,
    on_sdr: &dyn Fn(&Image),
    clock: &mut StageTimings,
) -> Result<Grabbed> {
    let GpuFp16 { width, height, queued, raw } = frame;
    let pixel_count = width as usize * height as usize;

    let started = Instant::now();
    let mapped = Mapped::new(gpu.context(), &queued.output_readback, 1)?;
    clock.copy_map += started.elapsed();
    let started = Instant::now();
    let sdr = Image::from_bgra(width, height, mapped.bytes(pixel_count * 4).to_vec());
    drop(mapped);
    clock.convert += started.elapsed();
    on_sdr(&sdr);

    let started = Instant::now();
    let counters = {
        let mapped = Mapped::new(gpu.context(), &queued.stats_readback, 1)?;
        let bytes = mapped.bytes(STATS_WORDS * 4);
        bytes.chunks_exact(4).map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]])).collect::<Vec<_>>()
    };
    let stats = stats_from_counters(&counters, pixel_count as u64);
    clock.copy_map += started.elapsed();

    let (pixels, _, _) = read_texture::<[f16; 4]>(gpu, &raw, Rotation::Identity, clock)?;
    let started = Instant::now();
    let image = hdr_image(monitor, width, height, pixels.into_flattened());
    clock.convert += started.elapsed();
    Ok(Grabbed { sdr, hdr: Some(image), stats: Some(stats) })
}

fn read_texture<P: Copy + Default>(
    gpu: &Gpu,
    staging: &ID3D11Texture2D,
    rotation: Rotation,
    clock: &mut StageTimings,
) -> Result<(Vec<P>, usize, usize)> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: GetDesc only fills the passed struct.
    unsafe { staging.GetDesc(&mut desc) };
    let (width, height) = (desc.Width as usize, desc.Height as usize);
    let started = Instant::now();
    let mapped = Mapped::new(gpu.context(), staging, height)?;
    clock.copy_map += started.elapsed();
    let started = Instant::now();
    let pixels = mapped.pixels::<P>(width, height, rotation);
    clock.convert += started.elapsed();
    Ok(pixels)
}

pub fn hdr_image(monitor: &MonitorInfo, width: u32, height: u32, data: Vec<f16>) -> HdrImage {
    let sdr_white_nits = monitor.hdr.map_or(DEFAULT_SDR_WHITE_NITS, |hdr| hdr.sdr_white_nits);
    let display_peak_nits = monitor
        .hdr
        .map(|hdr| hdr.max_nits)
        .filter(|nits| *nits > 0.0)
        .unwrap_or(DEFAULT_DISPLAY_PEAK_NITS);
    HdrImage { width, height, data, sdr_white_nits, display_peak_nits }
}

#[cfg(test)]
mod tests {
    use glint_core::tonemap::{analyze, tonemap_with_stats};
    use glint_core::{HdrInfo, RectI};
    use windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA;

    use super::*;

    const SDR_WHITES: [f32; 4] = [80.0, 120.0, 200.0, 280.0];

    fn monitor(width: u32, height: u32, sdr_white_nits: f32) -> MonitorInfo {
        MonitorInfo {
            handle: 0,
            device_name: "test".into(),
            friendly_name: "test".into(),
            rect: RectI::new(0, 0, width as i32, height as i32),
            work_rect: RectI::new(0, 0, width as i32, height as i32),
            dpi: 96,
            primary: true,
            hdr: Some(HdrInfo { sdr_white_nits, max_nits: 1000.0, max_full_frame_nits: 600.0, min_nits: 0.0 }),
        }
    }

    fn synthetic(width: u32, height: u32, sdr_white_nits: f32, pixel: impl Fn(u32, u32) -> [f32; 3]) -> HdrImage {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = pixel(x, y);
                data.extend([f16::from_f32(r), f16::from_f32(g), f16::from_f32(b), f16::ONE]);
            }
        }
        HdrImage { width, height, data, sdr_white_nits, display_peak_nits: 1000.0 }
    }

    fn upload(gpu: &Gpu, image: &HdrImage) -> ID3D11Texture2D {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: image.width,
            Height: image.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: image.data.as_ptr().cast(),
            SysMemPitch: image.width * 8,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        // SAFETY: the pixel data matches the descriptor; the out pointer is valid.
        unsafe { gpu.device().CreateTexture2D(&desc, Some(&data), Some(&mut texture)) }.unwrap();
        texture.unwrap()
    }

    fn on_gpu(gpu: &Gpu, image: &HdrImage, settings: &HdrSettings) -> Grabbed {
        let monitor = monitor(image.width, image.height, image.sdr_white_nits);
        let mut clock = StageTimings::default();
        let source = upload(gpu, image);
        let in_flight = begin(gpu, &source, &monitor, Rotation::Identity, settings, &mut clock).unwrap();
        assert!(matches!(in_flight, InFlight::Fp16Gpu(_)), "GPU path was not taken");
        finish(in_flight, gpu, &monitor, settings, &|_| {}, &mut clock).unwrap()
    }

    fn on_cpu(image: &HdrImage, settings: &HdrSettings) -> (Image, HdrStats) {
        let stats = analyze(image);
        let params = ToneMapParams { mode: settings.mode, exposure_stops: settings.exposure_stops };
        (tonemap_with_stats(image, &params, &stats), stats)
    }

    fn settings(mode: ToneMapMode, exposure_stops: f32) -> HdrSettings {
        HdrSettings { mode, exposure_stops, show_badge: true }
    }

    fn srgb_eotf(encoded: f32) -> f32 {
        if encoded <= 0.04045 { encoded / 12.92 } else { ((encoded + 0.055) / 1.055).powf(2.4) }
    }

    fn max_code_difference(a: &Image, b: &Image) -> (u8, usize) {
        assert_eq!((a.width, a.height), (b.width, b.height));
        let mut max = 0;
        let mut differing = 0;
        for (left, right) in a.data.iter().zip(&b.data) {
            let diff = left.abs_diff(*right);
            max = max.max(diff);
            differing += usize::from(diff != 0);
        }
        (max, differing)
    }

    #[test]
    fn sdr_codes_round_trip_exactly_on_the_gpu() {
        let gpu = Gpu::standalone().unwrap();
        let repeats = 16;
        for sdr_white_nits in SDR_WHITES {
            let image = synthetic(256 * repeats + 1, 3, sdr_white_nits, |x, _| {
                let value = if x == 256 * repeats {
                    1000.0 / 80.0
                } else {
                    srgb_eotf((x / repeats) as f32 / 255.0) * sdr_white_nits / 80.0
                };
                [value; 3]
            });
            for mode in [ToneMapMode::Auto, ToneMapMode::Clip] {
                let mapped = on_gpu(&gpu, &image, &settings(mode, 0.0)).sdr;
                for code in 0..=255u32 {
                    for y in 0..3 {
                        assert_eq!(
                            mapped.pixel(code * repeats, y),
                            [code as u8, code as u8, code as u8, 255],
                            "white={sdr_white_nits} mode={mode:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn sdr_gradients_match_the_cpu_exactly() {
        let gpu = Gpu::standalone().unwrap();
        for sdr_white_nits in SDR_WHITES {
            let top = sdr_white_nits / 80.0;
            let image = synthetic(1013, 61, sdr_white_nits, |x, y| {
                let t = (y * 1013 + x) as f32 / (61.0 * 1013.0);
                [t * top, (1.0 - t) * top, (t * 7.0).fract() * top]
            });
            for mode in [ToneMapMode::Auto, ToneMapMode::Clip] {
                let settings = settings(mode, 0.0);
                let gpu_result = on_gpu(&gpu, &image, &settings);
                let (cpu, _) = on_cpu(&image, &settings);
                assert_eq!(max_code_difference(&gpu_result.sdr, &cpu), (0, 0), "white={sdr_white_nits} mode={mode:?}");
            }
        }
    }

    #[test]
    fn hdr_content_is_within_one_code_of_the_cpu() {
        let gpu = Gpu::standalone().unwrap();
        for sdr_white_nits in [80.0, 240.0] {
            let image = synthetic(641, 359, sdr_white_nits, |x, y| {
                let (u, v) = (x as f32 / 640.0, y as f32 / 358.0);
                [u * 20.0, v * 10.0, ((x * y) % 257) as f32 / 257.0 * 5.0 - 0.5]
            });
            for (mode, exposure) in [(ToneMapMode::Auto, 0.0), (ToneMapMode::Auto, 1.0), (ToneMapMode::Clip, -0.5)] {
                let settings = settings(mode, exposure);
                let gpu_result = on_gpu(&gpu, &image, &settings);
                let (cpu, cpu_stats) = on_cpu(&image, &settings);
                let (max, differing) = max_code_difference(&gpu_result.sdr, &cpu);
                eprintln!("white={sdr_white_nits} mode={mode:?} exposure={exposure}: max diff {max}, {differing} bytes differ");
                assert!(max <= 1, "white={sdr_white_nits} mode={mode:?} exposure={exposure}: max diff {max}");

                let gpu_stats = gpu_result.stats.unwrap();
                assert_eq!(gpu_stats.max, cpu_stats.max);
                assert_eq!(gpu_stats.hdr_fraction, cpu_stats.hdr_fraction);
                assert_eq!(gpu_stats.out_of_gamut_fraction, cpu_stats.out_of_gamut_fraction);
                assert!((gpu_stats.peak / cpu_stats.peak - 1.0).abs() < 0.009, "{gpu_stats:?} vs {cpu_stats:?}");
            }
        }
    }

    #[test]
    fn raw_fp16_pixels_come_back_untouched() {
        let gpu = Gpu::standalone().unwrap();
        let image = synthetic(300, 40, 200.0, |x, y| [x as f32 / 30.0, y as f32 / 4.0, -0.25]);
        let result = on_gpu(&gpu, &image, &settings(ToneMapMode::Auto, 0.0));
        assert_eq!(result.hdr.unwrap().data, image.data);
    }
}
