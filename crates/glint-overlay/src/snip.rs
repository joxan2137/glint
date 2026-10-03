//! Cropping, region tone mapping and multi-monitor stitching of frozen captures.

use glint_core::settings::HdrSettings;
use glint_core::tonemap::{analyze, tonemap_region, tonemap_with_stats};
use glint_core::{CaptureMode, HdrImage, Image, MonitorInfo, RectI, ToneMapParams};

use crate::Snip;
use crate::geometry::largest_overlap;

/// One monitor's frozen pixels, borrowed from wherever they live.
pub struct Source<'a> {
    pub monitor: &'a MonitorInfo,
    pub sdr: &'a Image,
    pub hdr: Option<&'a HdrImage>,
}

pub fn tone_map_params(hdr: &HdrSettings) -> ToneMapParams {
    ToneMapParams { mode: hdr.mode, exposure_stops: hdr.exposure_stops }
}

/// `rect_px` is clipped to the area the monitors cover. One monitor: crop (re-tone-mapped with the region's own stats
/// when HDR, raw `HdrImage` included). Several: every part side by side on a transparent canvas, `hdr` = None.
pub fn snip_from(sources: &[Source], rect_px: RectI, hdr: &HdrSettings, mode: CaptureMode) -> Option<Snip> {
    let parts: Vec<(usize, RectI)> =
        sources.iter().enumerate().filter_map(|(i, s)| s.monitor.rect.intersect(&rect_px).map(|r| (i, r))).collect();
    let covered = parts.iter().map(|(_, r)| *r).reduce(|a, b| a.union(&b))?;
    let rects: Vec<RectI> = sources.iter().map(|s| s.monitor.rect).collect();
    let main = largest_overlap(&rects, covered)?;
    let params = tone_map_params(hdr);
    let local = |i: usize, r: RectI| r.offset(-sources[i].monitor.rect.x, -sources[i].monitor.rect.y);

    if let [(i, part)] = parts[..] {
        let source = &sources[i];
        let (image, raw, stats) = match source.hdr {
            Some(raw) => {
                let cropped = raw.crop(local(i, part));
                let stats = analyze(&cropped);
                (tonemap_with_stats(&cropped, &params, &stats), Some(cropped), Some(stats))
            }
            None => (source.sdr.crop(local(i, part)), None, None),
        };
        return Some(Snip { image, hdr: raw, hdr_stats: stats, rect_px: part, monitor: source.monitor.clone(), mode });
    }

    let mut canvas = Image::new(covered.w as u32, covered.h as u32);
    for &(i, part) in &parts {
        let piece = match sources[i].hdr {
            Some(raw) => tonemap_region(raw, local(i, part), &params),
            None => sources[i].sdr.crop(local(i, part)),
        };
        blit(&mut canvas, &piece, part.x - covered.x, part.y - covered.y);
    }
    Some(Snip { image: canvas, hdr: None, hdr_stats: None, rect_px: covered, monitor: sources[main].monitor.clone(), mode })
}

fn blit(canvas: &mut Image, piece: &Image, x: i32, y: i32) {
    let row_bytes = piece.width as usize * 4;
    for row in 0..piece.height as usize {
        let src = row * row_bytes;
        let dst = ((y as usize + row) * canvas.width as usize + x as usize) * 4;
        canvas.data[dst..dst + row_bytes].copy_from_slice(&piece.data[src..src + row_bytes]);
    }
}

#[cfg(test)]
mod tests {
    use glint_core::tonemap::tonemap;
    use glint_core::{MonitorCapture, f16};

    use super::*;
    use crate::snip_rect;

    fn monitor(rect: RectI, primary: bool) -> MonitorInfo {
        MonitorInfo {
            handle: rect.x as isize,
            device_name: format!("\\\\.\\DISPLAY{}", rect.x),
            friendly_name: "Test".into(),
            rect,
            work_rect: rect,
            dpi: 96,
            primary,
            hdr: None,
        }
    }

    fn gradient(w: u32, h: u32, seed: u8) -> Image {
        let mut image = Image::new(w, h);
        for (i, px) in image.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            px.copy_from_slice(&[x as u8, y as u8, seed, 255]);
        }
        image
    }

    fn sdr_capture(rect: RectI, seed: u8) -> MonitorCapture {
        MonitorCapture { monitor: monitor(rect, seed == 0), sdr: gradient(rect.w as u32, rect.h as u32, seed), hdr: None, hdr_stats: None }
    }

    #[test]
    fn single_monitor_crop_is_exact() {
        let captures = [sdr_capture(RectI::new(-100, 0, 100, 80), 0)];
        let snip = snip_rect(&captures, RectI::new(-90, 10, 20, 30), &HdrSettings::default(), CaptureMode::Rectangle).unwrap();
        assert_eq!((snip.image.width, snip.image.height), (20, 30));
        assert_eq!(snip.image.pixel(0, 0), [10, 10, 0, 255]);
        assert_eq!(snip.image.pixel(19, 29), [29, 39, 0, 255]);
        assert_eq!(snip.rect_px, RectI::new(-90, 10, 20, 30));
        assert!(snip.hdr.is_none() && snip.hdr_stats.is_none());
    }

    #[test]
    fn rect_is_clipped_to_the_desktop() {
        let captures = [sdr_capture(RectI::new(0, 0, 50, 50), 0)];
        let snip = snip_rect(&captures, RectI::new(40, -10, 30, 30), &HdrSettings::default(), CaptureMode::Window).unwrap();
        assert_eq!(snip.rect_px, RectI::new(40, 0, 10, 20));
        assert_eq!(snip.mode, CaptureMode::Window);
        assert!(snip_rect(&captures, RectI::new(60, 60, 5, 5), &HdrSettings::default(), CaptureMode::Window).is_none());
    }

    #[test]
    fn spanning_rect_stitches_monitors_and_leaves_gaps_transparent() {
        let captures = [sdr_capture(RectI::new(0, 0, 40, 40), 0), sdr_capture(RectI::new(40, 10, 60, 40), 1)];
        let snip = snip_rect(&captures, RectI::new(30, 0, 20, 30), &HdrSettings::default(), CaptureMode::Rectangle).unwrap();
        assert_eq!(snip.rect_px, RectI::new(30, 0, 20, 30));
        assert_eq!(snip.image.pixel(0, 0), [30, 0, 0, 255], "left part from monitor 0");
        assert_eq!(snip.image.pixel(10, 10), [0, 0, 1, 255], "right part from monitor 1 at its origin");
        assert_eq!(snip.image.pixel(15, 5)[3], 0, "no monitor covers it");
        assert_eq!(snip.monitor.rect, captures[0].monitor.rect, "monitor 0 holds 10x30 vs 10x20");
        assert!(snip.hdr.is_none());
    }

    fn hdr_monitor_capture() -> MonitorCapture {
        let (w, h) = (64u32, 32u32);
        let sdr_white = 240.0;
        let white = sdr_white / 80.0;
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let level = if x >= 32 { white * 4.0 } else { white * (x as f32 / 31.0) * (y as f32 / 31.0).max(0.2) };
                data.extend([level, level * 0.9, level * 0.8, 1.0].map(f16::from_f32));
            }
        }
        let raw = HdrImage { width: w, height: h, data, sdr_white_nits: sdr_white, display_peak_nits: 1000.0 };
        let params = ToneMapParams::default();
        let sdr = tonemap(&raw, &params);
        let mut info = monitor(RectI::new(0, 0, w as i32, h as i32), true);
        info.hdr = Some(glint_core::HdrInfo { sdr_white_nits: sdr_white, max_nits: 1000.0, max_full_frame_nits: 600.0, min_nits: 0.0 });
        MonitorCapture { monitor: info, sdr, hdr_stats: Some(analyze(&raw)), hdr: Some(raw) }
    }

    #[test]
    fn hdr_region_is_re_tone_mapped_with_its_own_stats() {
        let capture = hdr_monitor_capture();
        let region = RectI::new(0, 0, 32, 32);
        let settings = HdrSettings::default();
        let snip = snip_rect(std::slice::from_ref(&capture), region, &settings, CaptureMode::Rectangle).unwrap();
        let expected = tonemap_region(capture.hdr.as_ref().unwrap(), region, &tone_map_params(&settings));
        assert_eq!(snip.image, expected);
        assert_ne!(snip.image, capture.sdr.crop(region), "whole-monitor preview compresses SDR white");
        assert_eq!(snip.image.pixel(31, 31)[2], 255, "SDR white stays exact");
        assert!(capture.sdr.pixel(31, 31)[2] < 255);
        let raw = snip.hdr.as_ref().unwrap();
        assert_eq!((raw.width, raw.height), (32, 32));
        assert!(!snip.hdr_stats.unwrap().has_hdr_content());
        let bright = snip_rect(std::slice::from_ref(&capture), RectI::new(30, 0, 34, 32), &settings, CaptureMode::Rectangle).unwrap();
        assert!(bright.hdr_stats.unwrap().has_hdr_content());
    }
}
