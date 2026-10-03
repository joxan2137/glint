use anyhow::bail;

const SPEAKER_FRONT_LEFT: u32 = 0x1;
const SPEAKER_FRONT_RIGHT: u32 = 0x2;
const SPEAKER_FRONT_CENTER: u32 = 0x4;
const SPEAKER_LOW_FREQUENCY: u32 = 0x8;
const SPEAKER_BACK_LEFT: u32 = 0x10;
const SPEAKER_BACK_RIGHT: u32 = 0x20;
const SPEAKER_FRONT_LEFT_OF_CENTER: u32 = 0x40;
const SPEAKER_FRONT_RIGHT_OF_CENTER: u32 = 0x80;
const SPEAKER_BACK_CENTER: u32 = 0x100;
const SPEAKER_SIDE_LEFT: u32 = 0x200;
const SPEAKER_SIDE_RIGHT: u32 = 0x400;
const MINUS_3_DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleKind {
    F32,
    U8,
    I16,
    I24,
    I32,
}

impl SampleKind {
    pub fn from_bits(float: bool, bits: u16) -> anyhow::Result<Self> {
        Ok(match (float, bits) {
            (true, 32) => SampleKind::F32,
            (false, 8) => SampleKind::U8,
            (false, 16) => SampleKind::I16,
            (false, 24) => SampleKind::I24,
            (false, 32) => SampleKind::I32,
            _ => bail!("unsupported audio sample format ({bits}-bit, float: {float})"),
        })
    }

    pub fn bytes(self) -> usize {
        match self {
            SampleKind::U8 => 1,
            SampleKind::I16 => 2,
            SampleKind::I24 => 3,
            SampleKind::F32 | SampleKind::I32 => 4,
        }
    }

    fn decode(self, bytes: &[u8]) -> f32 {
        match self {
            SampleKind::F32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            SampleKind::U8 => (bytes[0] as f32 - 128.0) / 128.0,
            SampleKind::I16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32_768.0,
            SampleKind::I24 => i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]) as f32 / 2_147_483_648.0,
            SampleKind::I32 => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / 2_147_483_648.0,
        }
    }
}

/// Layout of one endpoint's shared-mode stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreamFormat {
    pub rate: u32,
    pub channels: usize,
    pub kind: SampleKind,
    pub block_align: usize,
    /// WAVEFORMATEXTENSIBLE dwChannelMask, 0 when unknown.
    pub channel_mask: u32,
}

/// Decodes any WASAPI sample layout and folds its channels down to interleaved stereo f32.
pub(crate) struct StereoDownmix {
    format: StreamFormat,
    weights: Vec<[f32; 2]>,
}

impl StereoDownmix {
    pub fn new(format: StreamFormat) -> Self {
        Self { format, weights: stereo_weights(format.channels, format.channel_mask) }
    }

    pub fn convert(&self, data: &[u8], frames: usize, output: &mut Vec<f32>) {
        let sample_bytes = self.format.kind.bytes();
        output.reserve(frames * 2);
        for frame in data.chunks_exact(self.format.block_align).take(frames) {
            let mut left = 0.0;
            let mut right = 0.0;
            for (channel, [to_left, to_right]) in self.weights.iter().enumerate() {
                let offset = channel * sample_bytes;
                let sample = self.format.kind.decode(&frame[offset..offset + sample_bytes]);
                left += sample * to_left;
                right += sample * to_right;
            }
            output.push(left);
            output.push(right);
        }
    }
}

/// Per input channel: its gain into the left and right output (ITU-R BS.775 style downmix).
fn stereo_weights(channels: usize, channel_mask: u32) -> Vec<[f32; 2]> {
    match channels {
        1 => return vec![[1.0, 1.0]],
        2 => return vec![[1.0, 0.0], [0.0, 1.0]],
        _ => {}
    }
    if channel_mask == 0 {
        let mut weights = vec![[0.0, 0.0]; channels];
        weights[0] = [1.0, 0.0];
        weights[1] = [0.0, 1.0];
        return weights;
    }
    let speakers = (0..32).map(|bit| 1u32 << bit).filter(|speaker| channel_mask & speaker != 0);
    let mut weights: Vec<[f32; 2]> = speakers
        .map(|speaker| match speaker {
            SPEAKER_FRONT_LEFT | SPEAKER_FRONT_LEFT_OF_CENTER => [1.0, 0.0],
            SPEAKER_FRONT_RIGHT | SPEAKER_FRONT_RIGHT_OF_CENTER => [0.0, 1.0],
            SPEAKER_FRONT_CENTER => [MINUS_3_DB, MINUS_3_DB],
            SPEAKER_LOW_FREQUENCY => [0.0, 0.0],
            SPEAKER_BACK_LEFT | SPEAKER_SIDE_LEFT => [MINUS_3_DB, 0.0],
            SPEAKER_BACK_RIGHT | SPEAKER_SIDE_RIGHT => [0.0, MINUS_3_DB],
            SPEAKER_BACK_CENTER => [0.5, 0.5],
            _ => [0.0, 0.0],
        })
        .take(channels)
        .collect();
    weights.resize(channels, [0.0, 0.0]);
    weights
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(channels: usize, kind: SampleKind, channel_mask: u32) -> StreamFormat {
        StreamFormat { rate: 48_000, channels, kind, block_align: channels * kind.bytes(), channel_mask }
    }

    fn f32_bytes(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn mono_is_duplicated() {
        let mut out = Vec::new();
        StereoDownmix::new(format(1, SampleKind::F32, 0)).convert(&f32_bytes(&[0.25, -0.5]), 2, &mut out);
        assert_eq!(out, [0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn stereo_is_unchanged() {
        let mut out = Vec::new();
        StereoDownmix::new(format(2, SampleKind::F32, 3)).convert(&f32_bytes(&[0.1, 0.2, 0.3, 0.4]), 2, &mut out);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn surround_folds_center_and_drops_lfe() {
        let mut out = Vec::new();
        let five_one = format(6, SampleKind::F32, 0x3F);
        StereoDownmix::new(five_one).convert(&f32_bytes(&[0.0, 0.0, 1.0, 1.0, 0.0, 0.0]), 1, &mut out);
        assert!((out[0] - MINUS_3_DB).abs() < 1e-6 && (out[1] - MINUS_3_DB).abs() < 1e-6, "{out:?}");
        out.clear();
        StereoDownmix::new(five_one).convert(&f32_bytes(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0]), 1, &mut out);
        assert!((out[0] - MINUS_3_DB).abs() < 1e-6 && out[1] == 0.0, "{out:?}");
    }

    #[test]
    fn unknown_layout_keeps_the_first_two_channels() {
        let mut out = Vec::new();
        StereoDownmix::new(format(4, SampleKind::F32, 0)).convert(&f32_bytes(&[0.1, 0.2, 0.9, 0.9]), 1, &mut out);
        assert_eq!(out, [0.1, 0.2]);
    }

    #[test]
    fn integer_samples_are_normalized() {
        let mut out = Vec::new();
        let i16_data: Vec<u8> = [i16::MIN, 16_384].iter().flat_map(|s| s.to_le_bytes()).collect();
        StereoDownmix::new(format(2, SampleKind::I16, 0)).convert(&i16_data, 1, &mut out);
        assert_eq!(out, [-1.0, 0.5]);
        out.clear();
        let i24_data = [0x00, 0x00, 0x40, 0x00, 0x00, 0xC0];
        StereoDownmix::new(format(2, SampleKind::I24, 0)).convert(&i24_data, 1, &mut out);
        assert_eq!(out, [0.5, -0.5]);
        out.clear();
        StereoDownmix::new(format(1, SampleKind::U8, 0)).convert(&[192], 1, &mut out);
        assert_eq!(out, [0.5, 0.5]);
    }

    #[test]
    fn unsupported_formats_are_rejected() {
        assert!(SampleKind::from_bits(true, 64).is_err());
        assert_eq!(SampleKind::from_bits(false, 24).unwrap(), SampleKind::I24);
    }
}
