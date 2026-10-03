use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use windows::Foundation::TimeSpan;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind,
    GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface;

use crate::gpu::Gpu;

const BUFFER_COUNT: i32 = 2;
const ACCESS_REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
/// `AsyncStatus::Started`; the enum lives in windows-future, which is not a direct dependency.
const ASYNC_STARTED: i32 = 0;

pub(crate) struct CaptureOptions {
    pub monitor: HMONITOR,
    pub fp16: bool,
    pub include_cursor: bool,
    pub frame_interval_hns: i64,
}

/// A Windows.Graphics.Capture session on one monitor, polled from the video thread.
pub(crate) struct ScreenCapture {
    _item: GraphicsCaptureItem,
    device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pixel_format: DirectXPixelFormat,
    pool_size: SizeInt32,
}

/// A captured desktop frame; its texture is valid until the frame is dropped.
pub(crate) struct CapturedFrame {
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    frame: Direct3D11CaptureFrame,
}

impl Drop for CapturedFrame {
    fn drop(&mut self) {
        let _ = self.frame.Close();
    }
}

impl ScreenCapture {
    pub fn start(gpu: &Gpu, options: &CaptureOptions) -> anyhow::Result<Self> {
        ensure!(
            GraphicsCaptureSession::IsSupported().unwrap_or(false),
            "Windows.Graphics.Capture is not supported on this system"
        );
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(options.monitor) }.context("GraphicsCaptureItem for the monitor")?;
        let device = gpu.winrt_device()?;
        let pixel_format = if options.fp16 {
            DirectXPixelFormat::R16G16B16A16Float
        } else {
            DirectXPixelFormat::B8G8R8A8UIntNormalized
        };
        let pool_size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&device, pixel_format, BUFFER_COUNT, pool_size)
            .context("Direct3D11CaptureFramePool")?;
        let session = pool.CreateCaptureSession(&item).context("GraphicsCaptureSession")?;
        session.SetIsCursorCaptureEnabled(options.include_cursor)?;
        request_borderless_access();
        if let Err(error) = session.SetIsBorderRequired(false) {
            log::debug!("capture border stays on: {error}");
        }
        if let Err(error) = session.SetMinUpdateInterval(TimeSpan { Duration: options.frame_interval_hns }) {
            log::debug!("MinUpdateInterval unavailable: {error}");
        }
        session.StartCapture().context("StartCapture")?;
        Ok(Self { _item: item, device, pool, session, pixel_format, pool_size })
    }

    /// The newest frame since the last call, or None when the screen has not changed.
    pub fn latest_frame(&mut self) -> anyhow::Result<Option<CapturedFrame>> {
        let mut newest: Option<Direct3D11CaptureFrame> = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            if let Some(older) = newest.replace(frame) {
                let _ = older.Close();
            }
        }
        let Some(frame) = newest else {
            return Ok(None);
        };
        let size = frame.ContentSize()?;
        if size != self.pool_size && size.Width > 0 && size.Height > 0 {
            log::info!("captured monitor resized to {}x{}", size.Width, size.Height);
            self.pool.Recreate(&self.device, self.pixel_format, BUFFER_COUNT, size)?;
            self.pool_size = size;
        }
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
        Ok(Some(CapturedFrame { texture, width: size.Width.max(1) as u32, height: size.Height.max(1) as u32, frame }))
    }

    pub fn wait_for_frame(&mut self, timeout: Duration, cancelled: impl Fn() -> bool) -> anyhow::Result<CapturedFrame> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.latest_frame()? {
                return Ok(frame);
            }
            if cancelled() {
                bail!("recording stopped before the first frame arrived");
            }
            if Instant::now() > deadline {
                bail!("no frame from Windows.Graphics.Capture within {timeout:?}");
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// Lets `SetIsBorderRequired(false)` take effect where the OS allows it (no prompt for unpackaged apps).
fn request_borderless_access() {
    let Ok(request) = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless) else {
        return;
    };
    let deadline = Instant::now() + ACCESS_REQUEST_TIMEOUT;
    while request.Status().is_ok_and(|status| status.0 == ASYNC_STARTED) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    match request.GetResults() {
        Ok(status) => log::debug!("borderless capture access: {}", status.0),
        Err(error) => log::debug!("borderless capture access unavailable: {error}"),
    }
}
