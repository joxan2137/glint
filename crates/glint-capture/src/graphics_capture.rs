use std::ffi::c_void;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use glint_core::MonitorInfo;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface};

use crate::gpu::{DesktopFrame, Gpu, read_staged, stage};
use crate::orient::Rotation;

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(300);
const FRAME_POOL_BUFFERS: i32 = 2;

/// One frame of `monitor` through Windows.Graphics.Capture; session and frame pool are closed before returning.
/// The first frame arrives even when the desktop is static. FP16 for HDR monitors, BGRA8 otherwise.
pub fn grab(monitor: &MonitorInfo, gpu: &Gpu) -> Result<DesktopFrame> {
    let started = Instant::now();
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
    let staging = first_frame_staged(&pool, &item, gpu, monitor);
    let _ = pool.Close();
    let staged = started.elapsed();
    let frame = read_staged(gpu, &staging?, monitor, Rotation::Identity)?;
    log::debug!("{}: WGC frame staged {staged:?}, read back {:?}", monitor.device_name, started.elapsed());
    Ok(frame)
}

/// Loads the WinRT capture classes (first use costs 0.3 to 2 s) by creating and dropping a capture item and a 1x1
/// frame pool. No session is created, so no capture border appears.
pub fn prewarm(device: &ID3D11Device, monitor: HMONITOR) -> Result<()> {
    ensure!(GraphicsCaptureSession::IsSupported().unwrap_or(false), "Windows.Graphics.Capture is not supported");
    let _item = capture_item(monitor)?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &direct3d_device(device)?,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        SizeInt32 { Width: 1, Height: 1 },
    )
    .context("create frame pool")?;
    let _ = pool.Close();
    Ok(())
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

fn first_frame_staged(
    pool: &Direct3D11CaptureFramePool,
    item: &GraphicsCaptureItem,
    gpu: &Gpu,
    monitor: &MonitorInfo,
) -> Result<ID3D11Texture2D> {
    let (arrived, frames) = mpsc::channel();
    let handler = TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |_, _| {
        let _ = arrived.send(());
        Ok(())
    });
    let token = pool.FrameArrived(&handler).context("subscribe to FrameArrived")?;
    let staged = (|| {
        let _session = Session::start(pool, item, monitor)?;
        frames
            .recv_timeout(FIRST_FRAME_TIMEOUT)
            .map_err(|_| anyhow!("no frame within {} ms", FIRST_FRAME_TIMEOUT.as_millis()))?;
        let frame = pool.TryGetNextFrame().context("TryGetNextFrame")?;
        let staged = frame_texture(&frame).and_then(|texture| stage(gpu, &texture));
        let _ = frame.Close();
        staged
    })();
    let _ = pool.RemoveFrameArrived(token);
    staged
}

/// A started capture session that is closed on drop, which also removes the capture border.
struct Session(GraphicsCaptureSession);

impl Session {
    fn start(pool: &Direct3D11CaptureFramePool, item: &GraphicsCaptureItem, monitor: &MonitorInfo) -> Result<Self> {
        let session = Session(pool.CreateCaptureSession(item).context("CreateCaptureSession")?);
        session.0.SetIsCursorCaptureEnabled(false).context("SetIsCursorCaptureEnabled(false)")?;
        match session.0.SetIsBorderRequired(false) {
            Ok(()) => log::info!("{}: WGC SetIsBorderRequired(false) accepted", monitor.device_name),
            Err(error) => {
                log::info!("{}: WGC SetIsBorderRequired(false) refused ({error}); border stays", monitor.device_name);
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

fn frame_texture(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D> {
    let access = frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>().context("IDirect3DDxgiInterfaceAccess")?;
    // SAFETY: the surface wraps a D3D11 texture on the device the pool was created with.
    unsafe { access.GetInterface::<ID3D11Texture2D>() }.context("frame surface is not a D3D11 texture")
}
