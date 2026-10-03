use glint_core::{HdrInfo, ToneMapMode, ToneMapParams};

const SCRGB_NITS: f64 = 80.0;
const SDR_TOLERANCE: f64 = 1.0 + 0.5 / 255.0;

/// Shader `mode` values; must match convert.hlsl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum CurveMode {
    /// BGRA8 source: copy.
    Sdr = 0,
    /// scRGB source: clamp at SDR white.
    Clip = 1,
    /// scRGB source: BT.2390 EETF from the display peak down to SDR white.
    Auto = 2,
}

/// Per-recording constants of the DESIGN §4 tone curve with a fixed source peak.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ToneCurve {
    pub mode: CurveMode,
    /// `80 / sdr_white_nits * 2^exposure`: scRGB → units of SDR white.
    pub exposure_scale: f32,
    pub sdr_white_nits: f32,
    /// `Lw = PQ(source peak in nits)`.
    pub source_pq: f32,
    /// `Lmax / Lw`.
    pub max_lum: f32,
    /// `KS`.
    pub knee: f32,
}

impl ToneCurve {
    pub fn new(hdr: Option<&HdrInfo>, params: &ToneMapParams) -> Self {
        let Some(hdr) = hdr else {
            return Self {
                mode: CurveMode::Sdr,
                exposure_scale: 1.0,
                sdr_white_nits: 80.0,
                source_pq: 1.0,
                max_lum: 1.0,
                knee: 1.0,
            };
        };
        let sdr_white = if hdr.sdr_white_nits > 0.0 { hdr.sdr_white_nits as f64 } else { SCRGB_NITS };
        let exposure = 2f64.powf(params.exposure_stops as f64);
        let source_peak = hdr.max_nits as f64 / sdr_white * exposure;
        let clip = Self {
            mode: CurveMode::Clip,
            exposure_scale: (SCRGB_NITS / sdr_white * exposure) as f32,
            sdr_white_nits: sdr_white as f32,
            source_pq: 1.0,
            max_lum: 1.0,
            knee: 1.0,
        };
        if params.mode == ToneMapMode::Clip || source_peak <= SDR_TOLERANCE {
            return clip;
        }
        let source_pq = pq_encode(source_peak * sdr_white);
        let max_lum = pq_encode(sdr_white) / source_pq;
        Self {
            mode: CurveMode::Auto,
            source_pq: source_pq as f32,
            max_lum: max_lum as f32,
            knee: (1.5 * max_lum - 0.5).max(0.0) as f32,
            ..clip
        }
    }
}

/// SMPTE ST 2084 inverse EOTF, 10 000-nit reference.
pub(crate) fn pq_encode(nits: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 4096.0 * 128.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 4096.0 * 32.0;
    const C3: f64 = 2392.0 / 4096.0 * 32.0;
    let y = (nits / 10_000.0).clamp(0.0, 1.0).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(sdr_white_nits: f32, max_nits: f32) -> HdrInfo {
        HdrInfo { sdr_white_nits, max_nits, max_full_frame_nits: max_nits, min_nits: 0.0 }
    }

    fn auto(exposure_stops: f32) -> ToneMapParams {
        ToneMapParams { mode: ToneMapMode::Auto, exposure_stops }
    }

    #[test]
    fn pq_matches_reference_values() {
        assert!((pq_encode(10_000.0) - 1.0).abs() < 1e-9);
        assert!(pq_encode(0.0) < 1e-6);
        assert!((pq_encode(100.0) - 0.5081).abs() < 1e-3);
        assert!((pq_encode(1000.0) - 0.7518).abs() < 1e-3);
    }

    #[test]
    fn sdr_monitors_copy_pixels() {
        assert_eq!(ToneCurve::new(None, &auto(0.0)).mode, CurveMode::Sdr);
    }

    #[test]
    fn clip_mode_only_normalizes_and_exposes() {
        let curve =
            ToneCurve::new(Some(&hdr(200.0, 1000.0)), &ToneMapParams { mode: ToneMapMode::Clip, exposure_stops: 1.0 });
        assert_eq!(curve.mode, CurveMode::Clip);
        assert!((curve.exposure_scale - 0.8).abs() < 1e-6);
    }

    #[test]
    fn auto_uses_the_display_peak_as_fixed_source_peak() {
        let curve = ToneCurve::new(Some(&hdr(200.0, 1000.0)), &auto(0.0));
        assert_eq!(curve.mode, CurveMode::Auto);
        assert!((curve.source_pq as f64 - pq_encode(1000.0)).abs() < 1e-6);
        assert!((curve.max_lum as f64 - pq_encode(200.0) / pq_encode(1000.0)).abs() < 1e-6);
        assert!((curve.knee - (1.5 * curve.max_lum - 0.5)).abs() < 1e-6);
        assert!(curve.knee < curve.max_lum && curve.max_lum < 1.0);
    }

    #[test]
    fn auto_falls_back_to_clip_when_the_peak_is_not_above_sdr_white() {
        assert_eq!(ToneCurve::new(Some(&hdr(300.0, 280.0)), &auto(0.0)).mode, CurveMode::Clip);
        assert_eq!(ToneCurve::new(Some(&hdr(200.0, 1000.0)), &auto(-3.0)).mode, CurveMode::Clip);
    }

    #[test]
    fn missing_sdr_white_defaults_to_80_nits() {
        let curve = ToneCurve::new(Some(&hdr(0.0, 1000.0)), &auto(0.0));
        assert_eq!(curve.sdr_white_nits, 80.0);
        assert_eq!(curve.exposure_scale, 1.0);
    }
}
