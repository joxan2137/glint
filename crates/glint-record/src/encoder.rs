use std::mem::ManuallyDrop;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, anyhow};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncMPVGOPSize, ICodecAPI, IMF2DBuffer, IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult,
    IMFAttributes, IMFByteStream, IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaType, IMFSample, IMFSinkWriter,
    MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE,
    MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SINK_WRITER_D3D_MANAGER,
    MF_TRANSCODE_CONTAINERTYPE, MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateAttributes, MFCreateDXGISurfaceBuffer,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFCreateTrackedSample,
    MFMediaType_Audio, MFMediaType_Video, MFNominalRange_16_235, MFTranscodeContainerType_MPEG4, MFVideoFormat_H264,
    MFVideoFormat_RGB32, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoTransFunc_sRGB,
    MFVideoTransferMatrix_BT709, eAVEncH264VProfile_High,
};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
use windows::core::{GUID, HSTRING, IUnknown, Interface, Ref, implement};

use crate::audio::{AAC_BYTES_PER_SECOND, CHANNELS, SAMPLE_RATE};

/// AAC-LC, level 2.
const AAC_PROFILE_LEVEL: u32 = 0x29;
const PCM_BYTES_PER_SAMPLE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VideoInput {
    /// D3D11 textures through the DXGI device manager; color conversion and encoding stay on the GPU.
    Gpu,
    /// BGRA in system memory.
    Cpu,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EncoderSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub audio: bool,
}

/// Media Foundation sink writer producing an H.264 (+ AAC) MP4.
pub(crate) struct Encoder {
    writer: IMFSinkWriter,
    video_stream: u32,
    audio_stream: Option<u32>,
    input: VideoInput,
}

impl Encoder {
    pub fn create(path: &Path, settings: &EncoderSettings, gpu: Option<&IMFDXGIDeviceManager>) -> anyhow::Result<Self> {
        let attributes = attributes()?;
        unsafe {
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            if let Some(manager) = gpu {
                attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, manager)?;
            }
        }
        let url = HSTRING::from(path.as_os_str());
        let writer = unsafe { MFCreateSinkWriterFromURL(&url, None::<&IMFByteStream>, &attributes) }
            .with_context(|| format!("cannot create {}", path.display()))?;
        let video_stream = unsafe { writer.AddStream(&h264_type(settings)?) }.context("H.264 output stream")?;
        unsafe { writer.SetInputMediaType(video_stream, &bgra_type(settings)?, None::<&IMFAttributes>) }
            .context("BGRA input for the H.264 encoder")?;
        set_keyframe_interval(&writer, video_stream, settings.fps * 2);
        let audio_stream = if settings.audio {
            let stream = unsafe { writer.AddStream(&aac_type()?) }.context("AAC output stream")?;
            unsafe { writer.SetInputMediaType(stream, &pcm_type()?, None::<&IMFAttributes>) }
                .context("PCM input for the AAC encoder")?;
            Some(stream)
        } else {
            None
        };
        unsafe { writer.BeginWriting() }.context("IMFSinkWriter::BeginWriting")?;
        let input = if gpu.is_some() { VideoInput::Gpu } else { VideoInput::Cpu };
        Ok(Self { writer, video_stream, audio_stream, input })
    }

    pub fn input(&self) -> VideoInput {
        self.input
    }

    /// Queues one video frame. With a tracker, the frame's texture counts as in use until the encoder lets go of it.
    pub fn write_video(
        &self,
        buffer: &IMFMediaBuffer,
        time: i64,
        duration: i64,
        tracker: Option<&SampleTracker>,
    ) -> anyhow::Result<()> {
        let sample: IMFSample = match tracker {
            Some(tracker) => unsafe {
                let tracked = MFCreateTrackedSample()?;
                tracked.SetAllocator(&tracker.callback, None::<&IUnknown>)?;
                tracker.in_flight.fetch_add(1, Ordering::AcqRel);
                tracked.cast()?
            },
            None => unsafe { MFCreateSample() }?,
        };
        unsafe {
            sample.AddBuffer(buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            self.writer.WriteSample(self.video_stream, &sample)
        }
        .context("writing a video frame")
    }

    /// Queues interleaved stereo 16-bit PCM.
    pub fn write_audio(&self, pcm: &[i16], time: i64, duration: i64) -> anyhow::Result<()> {
        let Some(stream) = self.audio_stream else {
            return Ok(());
        };
        let bytes: Vec<u8> = pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        let buffer = memory_buffer(&bytes)?;
        unsafe {
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            self.writer.WriteSample(stream, &sample)
        }
        .context("writing audio")
    }

    /// Flushes the encoders and writes the MP4 index.
    pub fn finish(self) -> anyhow::Result<()> {
        unsafe { self.writer.Finalize() }.context("IMFSinkWriter::Finalize")
    }
}

/// Counts how many queued samples still reference one texture.
pub(crate) struct SampleTracker {
    callback: IMFAsyncCallback,
    in_flight: Arc<AtomicUsize>,
}

impl SampleTracker {
    pub fn new() -> Self {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let callback = SampleReleased { in_flight: in_flight.clone() }.into();
        Self { callback, in_flight }
    }

    pub fn is_idle(&self) -> bool {
        self.in_flight.load(Ordering::Acquire) == 0
    }
}

#[implement(IMFAsyncCallback)]
struct SampleReleased {
    in_flight: Arc<AtomicUsize>,
}

impl IMFAsyncCallback_Impl for SampleReleased_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, _result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        Ok(())
    }
}

/// Wraps a D3D11 texture as a Media Foundation buffer.
pub(crate) fn surface_buffer(texture: &ID3D11Texture2D) -> anyhow::Result<IMFMediaBuffer> {
    unsafe {
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
        let length = buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?;
        buffer.SetCurrentLength(length)?;
        Ok(buffer)
    }
}

pub(crate) fn memory_buffer(bytes: &[u8]) -> anyhow::Result<IMFMediaBuffer> {
    let length = u32::try_from(bytes.len()).context("media buffer too large")?;
    unsafe {
        let buffer = MFCreateMemoryBuffer(length)?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(length)?;
        Ok(buffer)
    }
}

fn attributes() -> anyhow::Result<IMFAttributes> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, 4) }?;
    attributes.ok_or_else(|| anyhow!("MFCreateAttributes returned nothing"))
}

fn set_ratio(media_type: &IMFMediaType, key: &GUID, numerator: u32, denominator: u32) -> windows::core::Result<()> {
    unsafe { media_type.SetUINT64(key, (numerator as u64) << 32 | denominator as u64) }
}

fn video_type(settings: &EncoderSettings, subtype: &GUID) -> windows::core::Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    }
    set_ratio(&media_type, &MF_MT_FRAME_SIZE, settings.width, settings.height)?;
    set_ratio(&media_type, &MF_MT_FRAME_RATE, settings.fps, 1)?;
    set_ratio(&media_type, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)?;
    Ok(media_type)
}

fn h264_type(settings: &EncoderSettings) -> windows::core::Result<IMFMediaType> {
    let media_type = video_type(settings, &MFVideoFormat_H264)?;
    unsafe {
        media_type.SetUINT32(&MF_MT_AVG_BITRATE, settings.bitrate)?;
        media_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        media_type.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        media_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        media_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_sRGB.0 as u32)?;
        media_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    }
    Ok(media_type)
}

fn bgra_type(settings: &EncoderSettings) -> windows::core::Result<IMFMediaType> {
    let media_type = video_type(settings, &MFVideoFormat_RGB32)?;
    unsafe { media_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, settings.width * 4) }?;
    Ok(media_type)
}

fn audio_type(subtype: &GUID) -> windows::core::Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, PCM_BYTES_PER_SAMPLE * 8)?;
        media_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        media_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
    }
    Ok(media_type)
}

fn aac_type() -> windows::core::Result<IMFMediaType> {
    let media_type = audio_type(&MFAudioFormat_AAC)?;
    unsafe {
        media_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND)?;
        media_type.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, AAC_PROFILE_LEVEL)?;
    }
    Ok(media_type)
}

fn pcm_type() -> windows::core::Result<IMFMediaType> {
    let media_type = audio_type(&MFAudioFormat_PCM)?;
    let block_align = CHANNELS as u32 * PCM_BYTES_PER_SAMPLE;
    unsafe {
        media_type.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)?;
        media_type.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, block_align * SAMPLE_RATE)?;
    }
    Ok(media_type)
}

/// Best effort: encoders that do not expose ICodecAPI keep their default GOP.
fn set_keyframe_interval(writer: &IMFSinkWriter, stream: u32, frames: u32) {
    let mut codec = std::ptr::null_mut();
    if unsafe { writer.GetServiceForStream(stream, &GUID::zeroed(), &ICodecAPI::IID, &mut codec) }.is_err()
        || codec.is_null()
    {
        log::debug!("encoder exposes no ICodecAPI; default keyframe interval");
        return;
    }
    let codec = unsafe { ICodecAPI::from_raw(codec) };
    let value = VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: frames },
            }),
        },
    };
    if let Err(error) = unsafe { codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &value) } {
        log::debug!("keyframe interval not set: {error}");
    }
}
