use std::sync::OnceLock;
use windows::{
    Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT},
    core::PCWSTR,
};

pub fn play_shutter() {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    let bytes = WAV.get_or_init(shutter_wav);
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(bytes.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

fn shutter_wav() -> Vec<u8> {
    let mut samples = Vec::with_capacity(2880);
    let mut seed = 0x474c4e54u32;
    let mut filtered = 0.0f32;
    for i in 0..2880 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
        filtered += 0.45 * (noise - filtered);
        let t = i as f32 / 48_000.0;
        let burst = |start: f32| {
            if t >= start {
                (-(t - start) * 220.0).exp() * ((t - start) * 4000.0).min(1.0)
            } else {
                0.0
            }
        };
        samples.push(filtered * (burst(0.0) + 0.7 * burst(0.022)));
    }
    let peak = samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let gain = 10f32.powf(-12.0 / 20.0) * i16::MAX as f32 / peak;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36u32 + samples.len() as u32 * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&48_000u32.to_le_bytes());
    wav.extend_from_slice(&96_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&((sample * gain).round() as i16).to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header_and_peak() {
        let wav = super::shutter_wav();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(wav.len(), 44 + 2880 * 2);
        let peak = wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!((8230..=8232).contains(&peak));
    }
}
