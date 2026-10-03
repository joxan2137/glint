use anyhow::{anyhow, ensure};
use glint_core::RectI;

/// Widest frame common H.264 encoders (NVENC, AMF, QSV, Microsoft software) accept.
const MAX_ENCODED_SIDE: u32 = 4096;
/// H.264 level 5.1/5.2 frame-size limit (36 864 macroblocks).
const MAX_ENCODED_PIXELS: u64 = 4096 * 2304;
const BITS_PER_PIXEL_PER_FRAME: f64 = 0.15;

/// Clamps `region` (relative to the monitor origin) to the monitor and snaps its size down to even numbers.
pub(crate) fn snap_region(region: RectI, monitor_width: i32, monitor_height: i32) -> anyhow::Result<RectI> {
    let monitor = RectI::new(0, 0, monitor_width, monitor_height);
    let clamped = region.intersect(&monitor).ok_or_else(|| {
        anyhow!("recording region {region:?} lies outside the {monitor_width}x{monitor_height} monitor")
    })?;
    let snapped = RectI::new(clamped.x, clamped.y, clamped.w & !1, clamped.h & !1);
    ensure!(!snapped.is_empty(), "recording region {region:?} is smaller than 2x2 pixels");
    Ok(snapped)
}

/// Size of the encoded video: the region size, uniformly scaled down (keeping even sides) when it exceeds
/// what hardware H.264 encoders accept.
pub(crate) fn encoded_size(width: u32, height: u32) -> (u32, u32) {
    let pixels = width as u64 * height as u64;
    let scale = (MAX_ENCODED_SIDE as f64 / width as f64)
        .min(MAX_ENCODED_SIDE as f64 / height as f64)
        .min((MAX_ENCODED_PIXELS as f64 / pixels as f64).sqrt());
    if scale >= 1.0 {
        return (width, height);
    }
    let fit = |side: u32| (((side as f64 * scale) as u32) & !1).max(2);
    (fit(width), fit(height))
}

/// ≈ 0.15 bits per pixel per frame.
pub(crate) fn video_bitrate(width: u32, height: u32, fps: u32) -> u32 {
    (width as f64 * height as f64 * fps as f64 * BITS_PER_PIXEL_PER_FRAME).min(u32::MAX as f64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_is_snapped_to_even_size() {
        let snapped = snap_region(RectI::new(10, 20, 301, 199), 1920, 1080).unwrap();
        assert_eq!(snapped, RectI::new(10, 20, 300, 198));
    }

    #[test]
    fn region_is_clamped_to_the_monitor() {
        let snapped = snap_region(RectI::new(-10, 1000, 500, 500), 1920, 1080).unwrap();
        assert_eq!(snapped, RectI::new(0, 1000, 490, 80));
        let full = snap_region(RectI::new(0, 0, 2561, 1441), 2561, 1441).unwrap();
        assert_eq!(full, RectI::new(0, 0, 2560, 1440));
    }

    #[test]
    fn degenerate_regions_are_rejected() {
        assert!(snap_region(RectI::new(5000, 0, 10, 10), 1920, 1080).is_err());
        assert!(snap_region(RectI::new(0, 0, 1, 100), 1920, 1080).is_err());
        assert!(snap_region(RectI::new(0, 0, 0, 0), 1920, 1080).is_err());
    }

    #[test]
    fn common_sizes_are_encoded_unscaled() {
        assert_eq!(encoded_size(1920, 1080), (1920, 1080));
        assert_eq!(encoded_size(3840, 2160), (3840, 2160));
        assert_eq!(encoded_size(2, 2), (2, 2));
    }

    #[test]
    fn oversized_regions_are_scaled_to_encoder_limits() {
        let (w, h) = encoded_size(5120, 1440);
        assert!(w <= MAX_ENCODED_SIDE && w % 2 == 0 && h % 2 == 0, "{w}x{h}");
        assert!((w as f64 / h as f64 - 5120.0 / 1440.0).abs() < 0.01);
        let (w, h) = encoded_size(5120, 2880);
        assert!(w as u64 * h as u64 <= MAX_ENCODED_PIXELS && w <= MAX_ENCODED_SIDE, "{w}x{h}");
    }

    #[test]
    fn bitrate_is_point_15_bits_per_pixel_per_frame() {
        assert_eq!(video_bitrate(1920, 1080, 30), 9_331_200);
        assert_eq!(video_bitrate(2560, 1440, 60), 33_177_600);
        assert_eq!(video_bitrate(u32::MAX, u32::MAX, 60), u32::MAX);
    }
}
