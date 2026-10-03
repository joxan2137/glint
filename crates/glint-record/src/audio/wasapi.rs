use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};

use super::format::{SampleKind, StereoDownmix, StreamFormat};
use super::resample::Resampler;
use super::schedule::PlayoutQueue;
use super::{CHANNELS, SAMPLE_RATE, Source};
use crate::clock::{HNS_PER_SECOND, audio_frame_at, now_hns};
use crate::com::ComApartment;
use crate::state::{Shared, lock};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const RETRY_INTERVAL: Duration = Duration::from_millis(500);
const ENDPOINT_BUFFER_HNS: i64 = 2_000_000;

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::System => "system audio",
            Source::Microphone => "microphone",
        }
    }
}

/// Captures one default endpoint into the shared mixer until the video thread no longer needs audio.
/// Device failures are reported once as warnings and retried; the mixer fills the gaps with silence.
pub(crate) fn run_source(source: Source, shared: Arc<Shared>) {
    let _apartment = match ComApartment::join_multithreaded() {
        Ok(apartment) => apartment,
        Err(error) => {
            shared.warn(format!("{} unavailable: {error:#}", source.label()));
            return;
        }
    };
    let mut reported = false;
    while !shared.audio_stopped() {
        if !is_wanted(source, &shared) {
            thread::sleep(POLL_INTERVAL * 5);
            continue;
        }
        if let Err(error) = capture(source, &shared) {
            if reported {
                log::debug!("{} capture failed again: {error:#}", source.label());
            } else {
                shared.warn(format!("{} capture failed: {error:#}", source.label()));
                reported = true;
            }
            let retry_at = Instant::now() + RETRY_INTERVAL;
            while Instant::now() < retry_at && !shared.audio_stopped() {
                thread::sleep(POLL_INTERVAL * 5);
            }
        }
    }
}

fn is_wanted(source: Source, shared: &Shared) -> bool {
    source == Source::System || lock(&shared.mixer).microphone_enabled()
}

fn capture(source: Source, shared: &Shared) -> anyhow::Result<()> {
    let stream = CaptureStream::open(source)?;
    log::info!("{} stream: {:?}", source.label(), stream.format);
    let downmix = StereoDownmix::new(stream.format);
    let mut resampler = Resampler::new(stream.format.rate, SAMPLE_RATE);
    let mut stereo = Vec::new();
    let mut resampled = Vec::new();
    let mut playout = PlayoutQueue::default();
    while !shared.audio_stopped() && is_wanted(source, shared) {
        thread::sleep(POLL_INTERVAL);
        stream.drain(|packet| {
            stereo.clear();
            if packet.silent {
                stereo.resize(packet.frames * CHANNELS, 0.0);
            } else {
                downmix.convert(packet.data, packet.frames, &mut stereo);
            }
            resampled.clear();
            let offset_frames = resampler.process(&stereo, &mut resampled);
            if !resampled.is_empty() {
                let offset_hns = offset_frames * HNS_PER_SECOND as f64 / stream.format.rate as f64;
                playout.push(packet.timestamp + offset_hns as i64, &resampled, now_hns());
            }
        })?;
        for packet in playout.take_due(now_hns()) {
            let media_time = lock(&shared.clock).media_time_of_capture(packet.timestamp);
            if let Some(media_time) = media_time {
                lock(&shared.mixer).push(source, audio_frame_at(media_time), &packet.samples, now_hns());
            }
        }
    }
    Ok(())
}

struct Packet<'a> {
    data: &'a [u8],
    frames: usize,
    silent: bool,
    /// QPC time (100 ns units) of the first frame: when it was recorded (microphone) or will be heard (loopback).
    timestamp: i64,
}

/// A started shared-mode WASAPI capture stream (loopback of the default render endpoint for system audio).
struct CaptureStream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: StreamFormat,
}

impl CaptureStream {
    fn open(source: Source) -> anyhow::Result<Self> {
        let (flow, stream_flags) = match source {
            Source::System => (eRender, AUDCLNT_STREAMFLAGS_LOOPBACK),
            Source::Microphone => (eCapture, 0),
        };
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.context("MMDeviceEnumerator")?;
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }.context("no default device")?;
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.context("IMMDevice::Activate")?;
        let mix_format = unsafe { client.GetMixFormat() }.context("IAudioClient::GetMixFormat")?;
        let format = unsafe { parse_wave_format(mix_format) };
        let initialized = unsafe {
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, stream_flags, ENDPOINT_BUFFER_HNS, 0, mix_format, None)
        };
        unsafe { CoTaskMemFree(Some(mix_format as *const _)) };
        let format = format?;
        initialized.context("IAudioClient::Initialize")?;
        let capture: IAudioCaptureClient = unsafe { client.GetService() }.context("IAudioCaptureClient")?;
        unsafe { client.Start() }.context("IAudioClient::Start")?;
        Ok(Self { client, capture, format })
    }

    fn drain(&self, mut on_packet: impl FnMut(Packet)) -> anyhow::Result<()> {
        while unsafe { self.capture.GetNextPacketSize() }? > 0 {
            let mut data = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut qpc_position = 0;
            unsafe { self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc_position)) }?;
            if frames == 0 {
                unsafe { self.capture.ReleaseBuffer(0) }?;
                return Ok(());
            }
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
            let timestamp_valid = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 && qpc_position != 0;
            let timestamp = if timestamp_valid {
                qpc_position as i64
            } else {
                now_hns() - frames as i64 * HNS_PER_SECOND / self.format.rate as i64
            };
            let data = if silent {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(data, frames as usize * self.format.block_align) }
            };
            on_packet(Packet { data, frames: frames as usize, silent, timestamp });
            unsafe { self.capture.ReleaseBuffer(frames) }?;
        }
        Ok(())
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        let _ = unsafe { self.client.Stop() };
    }
}

/// Reads a WAVEFORMATEX / WAVEFORMATEXTENSIBLE returned by WASAPI.
unsafe fn parse_wave_format(format: *const WAVEFORMATEX) -> anyhow::Result<StreamFormat> {
    let base = unsafe { std::ptr::read_unaligned(format) };
    let tag = base.wFormatTag as u32;
    let (float, channel_mask) = if tag == WAVE_FORMAT_EXTENSIBLE && base.cbSize >= 22 {
        let extensible = unsafe { std::ptr::read_unaligned(format as *const WAVEFORMATEXTENSIBLE) };
        let sub_format = { extensible.SubFormat };
        ensure!(
            sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT || sub_format == KSDATAFORMAT_SUBTYPE_PCM,
            "unsupported audio sub-format {sub_format:?}"
        );
        (sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, extensible.dwChannelMask)
    } else {
        ensure!(tag == WAVE_FORMAT_PCM || tag == WAVE_FORMAT_IEEE_FLOAT, "unsupported audio format tag {tag}");
        (tag == WAVE_FORMAT_IEEE_FLOAT, 0)
    };
    let kind = SampleKind::from_bits(float, base.wBitsPerSample)?;
    let channels = base.nChannels as usize;
    let block_align = base.nBlockAlign as usize;
    ensure!(
        channels > 0 && block_align >= channels * kind.bytes(),
        "inconsistent audio format {channels} ch / {block_align} B"
    );
    Ok(StreamFormat { rate: base.nSamplesPerSec, channels, kind, block_align, channel_mask })
}
