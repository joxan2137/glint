use std::collections::VecDeque;

use super::{CHANNELS, SAMPLE_RATE};

/// Packets whose timestamp is within this many frames of the end of the buffered audio are appended
/// back to back; larger jumps (silence gaps from loopback, clock drift) re-align the source.
const ALIGNMENT_TOLERANCE_FRAMES: u64 = SAMPLE_RATE as u64 * 30 / 1000;
const METER_DECAY_HNS: f64 = 3_000_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    System,
    Microphone,
}

/// One source's 48 kHz stereo audio that has arrived but is not mixed yet.
#[derive(Default)]
struct Track {
    /// Timeline frame index of the first buffered frame.
    start: u64,
    samples: VecDeque<f32>,
}

impl Track {
    fn frames(&self) -> u64 {
        (self.samples.len() / CHANNELS) as u64
    }

    fn end(&self) -> u64 {
        self.start + self.frames()
    }

    fn push(&mut self, at: u64, samples: &[f32], mixed_until: u64) {
        if self.samples.is_empty() {
            self.start = at;
        } else if at > self.end() + ALIGNMENT_TOLERANCE_FRAMES {
            let gap = (at - self.end()) as usize;
            self.samples.extend(std::iter::repeat_n(0.0, gap * CHANNELS));
        } else if at + ALIGNMENT_TOLERANCE_FRAMES < self.end() {
            let keep = at.saturating_sub(self.start) as usize;
            self.samples.truncate(keep * CHANNELS);
            if self.samples.is_empty() {
                self.start = at;
            }
        }
        self.samples.extend(samples);
        self.discard_before(mixed_until);
    }

    fn discard_before(&mut self, frame: u64) {
        if frame <= self.start {
            return;
        }
        let stale = (frame - self.start).min(self.frames());
        self.samples.drain(..stale as usize * CHANNELS);
        self.start = frame;
    }

    fn add_into(&self, mix: &mut [f32], from: u64) {
        let to = from + (mix.len() / CHANNELS) as u64;
        let overlap_start = self.start.max(from);
        let overlap_end = self.end().min(to);
        for frame in overlap_start..overlap_end {
            let source = (frame - self.start) as usize * CHANNELS;
            let target = (frame - from) as usize * CHANNELS;
            for channel in 0..CHANNELS {
                mix[target + channel] += self.samples[source + channel];
            }
        }
    }
}

/// Holds both sources on the shared timeline and mixes them into 16-bit PCM on demand.
/// Frames no source delivered (loopback during silence, a missing microphone) come out as silence.
pub(crate) struct Mixer {
    system: Track,
    microphone: Track,
    microphone_enabled: bool,
    mixed_until: u64,
    meter: PeakMeter,
}

impl Mixer {
    pub fn new(microphone_enabled: bool) -> Self {
        Self {
            system: Track::default(),
            microphone: Track::default(),
            microphone_enabled,
            mixed_until: 0,
            meter: PeakMeter::default(),
        }
    }

    pub fn microphone_enabled(&self) -> bool {
        self.microphone_enabled
    }

    pub fn set_microphone_enabled(&mut self, enabled: bool) {
        self.microphone_enabled = enabled;
        if !enabled {
            self.microphone = Track::default();
        }
    }

    /// Adds interleaved stereo `samples` from `source` starting at timeline frame `at`.
    pub fn push(&mut self, source: Source, at: u64, samples: &[f32], now: i64) {
        if source == Source::Microphone && !self.microphone_enabled {
            return;
        }
        let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        self.meter.observe(peak.min(1.0), now);
        let mixed_until = self.mixed_until;
        self.track_mut(source).push(at, samples, mixed_until);
    }

    pub fn mixed_until(&self) -> u64 {
        self.mixed_until
    }

    /// Mixes timeline frames `[mixed_until, end)` into interleaved stereo 16-bit PCM.
    pub fn mix_until(&mut self, end: u64) -> Vec<i16> {
        if end <= self.mixed_until {
            return Vec::new();
        }
        let from = self.mixed_until;
        let mut mix = vec![0.0f32; (end - from) as usize * CHANNELS];
        self.system.add_into(&mut mix, from);
        if self.microphone_enabled {
            self.microphone.add_into(&mut mix, from);
        }
        self.system.discard_before(end);
        self.microphone.discard_before(end);
        self.mixed_until = end;
        mix.into_iter().map(to_pcm16).collect()
    }

    /// Recent peak amplitude, 0..1 linear.
    pub fn level(&self, now: i64) -> f32 {
        self.meter.level(now)
    }

    fn track_mut(&mut self, source: Source) -> &mut Track {
        match source {
            Source::System => &mut self.system,
            Source::Microphone => &mut self.microphone,
        }
    }
}

#[derive(Default)]
struct PeakMeter {
    peak: f32,
    at: i64,
}

impl PeakMeter {
    fn observe(&mut self, peak: f32, now: i64) {
        if peak >= self.level(now) {
            self.peak = peak;
            self.at = now;
        }
    }

    fn level(&self, now: i64) -> f32 {
        let age = (now - self.at).max(0) as f64;
        self.peak * (-age / METER_DECAY_HNS).exp() as f32
    }
}

pub(crate) fn to_pcm16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(frames: usize, value: f32) -> Vec<f32> {
        vec![value; frames * CHANNELS]
    }

    fn pcm(value: f32) -> i16 {
        to_pcm16(value)
    }

    #[test]
    fn silence_fills_frames_nobody_delivered() {
        let mut mixer = Mixer::new(false);
        let out = mixer.mix_until(480);
        assert_eq!(out.len(), 960);
        assert!(out.iter().all(|&s| s == 0));
        assert_eq!(mixer.mixed_until(), 480);
        assert!(mixer.mix_until(100).is_empty());
    }

    #[test]
    fn sources_are_summed_and_clipped() {
        let mut mixer = Mixer::new(true);
        mixer.push(Source::System, 0, &stereo(100, 0.25), 0);
        mixer.push(Source::Microphone, 0, &stereo(100, 0.5), 0);
        mixer.push(Source::Microphone, 100, &stereo(100, 0.9), 0);
        mixer.push(Source::System, 100, &stereo(100, 0.9), 0);
        let out = mixer.mix_until(200);
        assert_eq!(out[0], pcm(0.75));
        assert_eq!(out[199 * 2], i16::MAX);
    }

    #[test]
    fn disabled_microphone_is_muted() {
        let mut mixer = Mixer::new(true);
        mixer.push(Source::Microphone, 0, &stereo(100, 0.5), 0);
        mixer.set_microphone_enabled(false);
        mixer.push(Source::Microphone, 100, &stereo(100, 0.5), 0);
        assert!(mixer.mix_until(200).iter().all(|&s| s == 0));
        mixer.set_microphone_enabled(true);
        mixer.push(Source::Microphone, 200, &stereo(10, 0.5), 0);
        assert_eq!(mixer.mix_until(210)[0], pcm(0.5));
    }

    #[test]
    fn audio_lands_at_its_timestamp() {
        let mut mixer = Mixer::new(false);
        mixer.push(Source::System, 1000, &stereo(10, 0.5), 0);
        let out = mixer.mix_until(1010);
        assert!(out[..2000].iter().all(|&s| s == 0));
        assert!(out[2000..].iter().all(|&s| s == pcm(0.5)));
    }

    #[test]
    fn small_timestamp_jitter_keeps_packets_contiguous() {
        let mut mixer = Mixer::new(false);
        mixer.push(Source::System, 0, &stereo(480, 0.5), 0);
        mixer.push(Source::System, 470, &stereo(480, 0.25), 0);
        let out = mixer.mix_until(960);
        assert_eq!(out[479 * 2], pcm(0.5));
        assert_eq!(out[480 * 2], pcm(0.25));
        assert_eq!(out[959 * 2], pcm(0.25));
    }

    #[test]
    fn a_gap_after_silence_is_filled_with_zeros() {
        let mut mixer = Mixer::new(false);
        mixer.push(Source::System, 0, &stereo(480, 0.5), 0);
        mixer.push(Source::System, 48_000, &stereo(480, 0.5), 0);
        let out = mixer.mix_until(48_480);
        assert_eq!(out[479 * 2], pcm(0.5));
        assert_eq!(out[480 * 2], 0);
        assert_eq!(out[47_999 * 2], 0);
        assert_eq!(out[48_000 * 2], pcm(0.5));
    }

    #[test]
    fn a_source_running_ahead_is_realigned() {
        let mut mixer = Mixer::new(false);
        mixer.push(Source::System, 0, &stereo(10_000, 0.5), 0);
        mixer.push(Source::System, 5_000, &stereo(100, 0.25), 0);
        let out = mixer.mix_until(5_100);
        assert_eq!(out[4_999 * 2], pcm(0.5));
        assert_eq!(out[5_000 * 2], pcm(0.25));
        assert_eq!(mixer.mix_until(6_000).iter().filter(|&&s| s != 0).count(), 0);
    }

    #[test]
    fn late_audio_is_dropped() {
        let mut mixer = Mixer::new(false);
        mixer.mix_until(1000);
        mixer.push(Source::System, 900, &stereo(200, 0.5), 0);
        let out = mixer.mix_until(1100);
        assert!(out.iter().all(|&s| s == pcm(0.5)));
    }

    #[test]
    fn meter_holds_peaks_and_decays() {
        let mut mixer = Mixer::new(false);
        mixer.push(Source::System, 0, &[0.0, -0.8, 0.2, 0.1], 0);
        assert!((mixer.level(0) - 0.8).abs() < 1e-6);
        mixer.push(Source::System, 2, &[0.1, 0.1], 1_000);
        assert!(mixer.level(1_000) > 0.79);
        assert!(mixer.level(30_000_000) < 0.01);
    }

    #[test]
    fn pcm_conversion_clamps() {
        assert_eq!(to_pcm16(2.0), i16::MAX);
        assert_eq!(to_pcm16(-2.0), -i16::MAX);
        assert_eq!(to_pcm16(0.0), 0);
    }
}
