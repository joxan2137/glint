use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use glint_core::settings::HdrSettings;
use glint_core::{Image, MonitorInfo};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface};

use crate::gpu::Gpu;
use crate::orient::Rotation;
use crate::pipeline::{Grabbed, StageTimings, begin, finish};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(300);
const FRAME_POOL_BUFFERS: i32 = 2;

static BORDER_LOGGED: AtomicBool = AtomicBool::new(false);

/// One frame of `monitor` through Windows.Graphics.Capture; session and frame pool are closed before returning.
/// The first frame arrives even when the desktop is static. FP16 for HDR monitors, BGRA8 otherwise.
pub fn grab(
    monitor: &MonitorInfo,
    gpu: &Gpu,
    hdr: &HdrSettings,
    on_sdr: &dyn Fn(&Image),
    clock: &mut StageTimings,
) -> Result<Grabbed> {
    ensure!(GraphicsCaptureSession::IsSupported().unwrap_or(false), "Windows.Graphics.Capture is not supported");
    let item = monitor_item(monitor)?;
    let device = direct3d_device(gpu.device())?;
    let format = if monitor.hdr.is_some() {
        DirectXPixelFormat::R16G16B16A16Float
    } else {
        DirectXPixelFormat::B8G8R8A8UIntNormalized
    };
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&device, format, FRAME_POOL_BUFFERS, item.Size()?)
        .context("create frame pool")?;
    let captured = first_frame(&pool, &item, &monitor.device_name, false, |texture| {
        begin(gpu, texture, monitor, Rotation::Identity, hdr, clock)
    });
    let _ = pool.Close();
    let (in_flight, waited) = captured?;
    clock.setup += waited.setup;
    clock.acquire += waited.acquire;
    finish(in_flight, gpu, monitor, hdr, on_sdr, clock)
}

/// Loads the WinRT capture classes (first use costs 0.3 to 2 s) and runs one complete capture session, so the first
/// real capture does not. Skipped when the capture border cannot be switched off, because the session would flash it.
pub fn prewarm(device: &ID3D11Device, monitor: HMONITOR) -> Result<()> {
    ensure!(GraphicsCaptureSession::IsSupported().unwrap_or(false), "Windows.Graphics.Capture is not supported");
    let item = capture_item(monitor)?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &direct3d_device(device)?,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        FRAME_POOL_BUFFERS,
        item.Size()?,
    )
    .context("create frame pool")?;
    let warmed = first_frame(&pool, &item, "pre-warm", true, |_| Ok(()));
    let _ = pool.Close();
    warmed.map(|_| ())
}

fn capture_item(monitor: HMONITOR) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .context("IGraphicsCaptureItemInterop")?;
    // SAFETY: the handle comes from EnumDisplayMonitors / DXGI in this session.
    unsafe { interop.CreateForMonitor(monitor) }.context("CreateForMonitor")
}

fn monitor_item(monitor: &MonitorInfo) -> Result<GraphicsCaptureItem> {
    let item = capture_item(HMONITOR(monitor.handle as *mut c_void))?;
    let size = item.Size()?;
    ensure!(
        (size.Width, size.Height) == (monitor.rect.w, monitor.rect.h),
        "capture item is {}x{} but the monitor rect is {}x{}",
        size.Width,
        size.Height,
        monitor.rect.w,
        monitor.rect.h
    );
    Ok(item)
}

fn direct3d_device(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi_device = device.cast::<IDXGIDevice>().context("ID3D11Device is not an IDXGIDevice")?;
    // SAFETY: the DXGI device is valid; the returned WinRT wrapper keeps its own reference.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }
        .context("CreateDirect3D11DeviceFromDXGIDevice")?;
    inspectable.cast().context("IDirect3DDevice")
}

struct FrameWait {
    setup: Duration,
    acquire: Duration,
}

/// Runs a session until the first frame arrives and hands its texture to `on_frame` while the frame is still alive.
fn first_frame<T>(
    pool: &Direct3D11CaptureFramePool,
    item: &GraphicsCaptureItem,
    name: &str,
    borderless_required: bool,
    on_frame: impl FnOnce(&ID3D11Texture2D) -> Result<T>,
) -> Result<(T, FrameWait)> {
    let (arrived, frames) = mpsc::channel();
    let handler = TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |_, _| {
        let _ = arrived.send(());
        Ok(())
    });
    let token = pool.FrameArrived(&handler).context("subscribe to FrameArrived")?;
    let started = Instant::now();
    let captured = (|| {
        let _session = Session::start(pool, item, name, borderless_required)?;
        let setup = started.elapsed();
        frames
            .recv_timeout(FIRST_FRAME_TIMEOUT)
            .map_err(|_| anyhow!("no frame within {} ms", FIRST_FRAME_TIMEOUT.as_millis()))?;
        let frame = pool.TryGetNextFrame().context("TryGetNextFrame")?;
        let acquire = started.elapsed() - setup;
        let result = frame_texture(&frame).and_then(|texture| on_frame(&texture));
        let _ = frame.Close();
        result.map(|value| (value, FrameWait { setup, acquire }))
    })();
    let _ = pool.RemoveFrameArrived(token);
    captured
}

/// A started capture session that is closed on drop, which also removes the capture border.
struct Session(GraphicsCaptureSession);

impl Session {
    fn start(
        pool: &Direct3D11CaptureFramePool,
        item: &GraphicsCaptureItem,
        name: &str,
        borderless_required: bool,
    ) -> Result<Self> {
        let session = Session(pool.CreateCaptureSession(item).context("CreateCaptureSession")?);
        session.0.SetIsCursorCaptureEnabled(false).context("SetIsCursorCaptureEnabled(false)")?;
        match session.0.SetIsBorderRequired(false) {
            Ok(()) => log_border(name, "accepted".into()),
            Err(error) => {
                log_border(name, format!("refused ({error}); the capture border stays"));
                ensure!(!borderless_required, "the capture border cannot be disabled");
            }
        }
        session.0.StartCapture().context("StartCapture")?;
        Ok(session)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}

fn log_border(name: &str, outcome: String) {
    let message = format!("{name}: WGC SetIsBorderRequired(false) {outcome}");
    if BORDER_LOGGED.swap(true, Ordering::Relaxed) {
        log::debug!("{message}");
    } else {
        log::info!("{message}");
    }
}

fn frame_texture(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D> {
    let access = frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>().context("IDirect3DDxgiInterfaceAccess")?;
    // SAFETY: the surface wraps a D3D11 texture on the device the pool was created with.
    unsafe { access.GetInterface::<ID3D11Texture2D>() }.context("frame surface is not a D3D11 texture")
}
