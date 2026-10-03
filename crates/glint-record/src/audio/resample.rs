use std::f64::consts::PI;

/// Taps on each side of the interpolation point at full bandwidth; widened when downsampling.
const HALF_TAPS: usize = 16;
const KERNEL_STEPS_PER_TAP: usize = 256;

/// Streaming windowed-sinc resampler for interleaved stereo f32.
pub(crate) struct Resampler {
    /// Input frames advanced per output frame.
    step: f64,
    /// Position of the next output frame, in frames of `history`.
    position: f64,
    history: Vec<f32>,
    half_taps: usize,
    kernel: Vec<f64>,
    passthrough: bool,
}

impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        let step = input_rate as f64 / output_rate as f64;
        let cutoff = (output_rate as f64 / input_rate as f64).min(1.0);
        let half_taps = (HALF_TAPS as f64 / cutoff).ceil() as usize;
        let kernel = (0..=half_taps * KERNEL_STEPS_PER_TAP + 1)
            .map(|i| {
                let distance = i as f64 / KERNEL_STEPS_PER_TAP as f64;
                sinc(cutoff * distance) * blackman(distance / half_taps as f64)
            })
            .collect();
        Self {
            step,
            position: half_taps as f64,
            history: vec![0.0; half_taps * 2],
            half_taps,
            kernel,
            passthrough: input_rate == output_rate,
        }
    }

    /// Appends interleaved stereo `input` and writes every output frame that can be computed to `output`.
    /// Returns where the first written output frame sits relative to the first new input frame, in input
    /// frames (negative when it interpolates older input).
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> f64 {
        if self.passthrough {
            output.extend_from_slice(input);
            return 0.0;
        }
        let first_new_frame = self.history.len() / 2;
        let first_output_offset = self.position - first_new_frame as f64;
        self.history.extend_from_slice(input);
        let frames = self.history.len() / 2;
        let half_taps = self.half_taps;
        while self.position as usize + half_taps < frames {
            let center = self.position as usize;
            let mut left = 0.0;
            let mut right = 0.0;
            let mut weight_sum = 0.0;
            for tap in center + 1 - half_taps..=center + half_taps {
                let weight = self.kernel_at((self.position - tap as f64).abs());
                left += weight * self.history[tap * 2] as f64;
                right += weight * self.history[tap * 2 + 1] as f64;
                weight_sum += weight;
            }
            output.push((left / weight_sum) as f32);
            output.push((right / weight_sum) as f32);
            self.position += self.step;
        }
        let consumed = (self.position as usize).saturating_sub(half_taps);
        self.history.drain(..consumed * 2);
        self.position -= consumed as f64;
        first_output_offset
    }

    fn kernel_at(&self, distance: f64) -> f64 {
        let scaled = distance * KERNEL_STEPS_PER_TAP as f64;
        let index = scaled as usize;
        let fraction = scaled - index as f64;
        self.kernel[index] + (self.kernel[index + 1] - self.kernel[index]) * fraction
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) }
}

/// Blackman window over t in [-1, 1].
fn blackman(t: f64) -> f64 {
    if t.abs() >= 1.0 { 0.0 } else { 0.42 + 0.5 * (PI * t).cos() + 0.08 * (2.0 * PI * t).cos() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_tone(frequency: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * PI * frequency * i as f64 / rate as f64).sin() as f32;
                [v, -v]
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|&s| s as f64 * s as f64).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn equal_rates_pass_through() {
        let input = stereo_tone(440.0, 48_000, 1000);
        let mut output = Vec::new();
        Resampler::new(48_000, 48_000).process(&input, &mut output);
        assert_eq!(output, input);
    }

    #[test]
    fn output_length_follows_the_rate_ratio() {
        let mut resampler = Resampler::new(44_100, 48_000);
        let mut output = Vec::new();
        resampler.process(&vec![0.0; 44_100 * 2], &mut output);
        let frames = output.len() / 2;
        assert!((48_000 - 20..=48_000).contains(&frames), "{frames}");
    }

    #[test]
    fn upsampled_tone_matches_the_ideal_signal() {
        let input = stereo_tone(1000.0, 44_100, 44_100);
        let mut output = Vec::new();
        Resampler::new(44_100, 48_000).process(&input, &mut output);
        let expected = stereo_tone(1000.0, 48_000, output.len() / 2);
        let worst = output.iter().zip(&expected).skip(2 * 100).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "max error {worst}");
    }

    #[test]
    fn dc_level_is_preserved() {
        let mut output = Vec::new();
        Resampler::new(96_000, 48_000).process(&vec![0.5; 96_000], &mut output);
        assert!(output[200..].iter().all(|&s| (s - 0.5).abs() < 1e-4));
    }

    #[test]
    fn downsampling_removes_content_above_the_new_nyquist() {
        let mut passband = Vec::new();
        Resampler::new(96_000, 48_000).process(&stereo_tone(1000.0, 96_000, 96_000), &mut passband);
        let mut stopband = Vec::new();
        Resampler::new(96_000, 48_000).process(&stereo_tone(30_000.0, 96_000, 96_000), &mut stopband);
        assert!(rms(&passband[400..]) > 0.68, "{}", rms(&passband[400..]));
        assert!(rms(&stopband[400..]) < 0.01, "{}", rms(&stopband[400..]));
    }

    #[test]
    fn chunked_processing_matches_one_shot() {
        let input = stereo_tone(3000.0, 44_100, 10_000);
        let mut whole = Vec::new();
        Resampler::new(44_100, 48_000).process(&input, &mut whole);
        let mut chunked = Vec::new();
        let mut resampler = Resampler::new(44_100, 48_000);
        for chunk in input.chunks(441 * 2) {
            resampler.process(chunk, &mut chunked);
        }
        assert_eq!(whole.len(), chunked.len());
        assert!(whole.iter().zip(&chunked).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn first_output_offset_points_into_the_new_input() {
        let mut resampler = Resampler::new(44_100, 48_000);
        let mut output = Vec::new();
        assert_eq!(resampler.process(&[0.0; 882], &mut output), 0.0);
        let offset = resampler.process(&[0.0; 882], &mut output);
        assert!((-(HALF_TAPS as f64)..=0.0).contains(&offset), "{offset}");
    }
}
