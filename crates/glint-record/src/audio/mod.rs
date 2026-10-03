mod format;
mod mixer;
mod resample;
mod schedule;
mod wasapi;

pub(crate) use mixer::{Mixer, Source};
pub(crate) use wasapi::run_source;

pub(crate) const SAMPLE_RATE: u32 = 48_000;
pub(crate) const CHANNELS: usize = 2;
pub(crate) const AAC_BYTES_PER_SECOND: u32 = 192_000 / 8;
/// The mixer stays this far behind real time so late WASAPI packets still land in the mix.
pub(crate) const MIX_DELAY_HNS: i64 = 1_500_000;
