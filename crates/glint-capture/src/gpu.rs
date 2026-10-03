use std::mem::ManuallyDrop;
use std::sync::{Mutex, PoisonError};

use anyhow::{Context, Result, bail, ensure};
use glint_core::{Image, MonitorInfo, f16};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter1;
use windows::core::Interface;

use crate::dxgi;
use crate::graphics_capture;
use crate::orient::{Rotation, orient};

type AdapterLuid = (u32, i32);

struct WarmDevice {
    luid: AdapterLuid,
    device: ID3D11Device,
}

/// Idle devices that keep the GPU driver initialised between captures; see `warm_up`.
static WARM_DEVICES: Mutex<Vec<WarmDevice>> = Mutex::new(Vec::new());

pub struct Fp16Frame {
    pub width: u32,
    pub height: u32,
    /// RGBA half floats, scRGB.
    pub data: Vec<f16>,
}

pub enum DesktopFrame {
    Fp16(Fp16Frame),
    Bgra(Image),
}

/// The D3D11 device used for one monitor capture: the warm device of the adapter when `warm_up` ran, else a fresh one.
pub struct Gpu {
    device: ManuallyDrop<ID3D11Device>,
    context: ManuallyDrop<ID3D11DeviceContext>,
    warm: bool,
}

impl Gpu {
    pub fn for_adapter(adapter: &IDXGIAdapter1) -> Result<Self> {
        if let Some(device) = warm_device(adapter_luid(adapter)?) {
            // SAFETY: plain getter on a live device.
            let context = unsafe { device.GetImmediateContext() }.context("GetImmediateContext")?;
            return Ok(Self { device: ManuallyDrop::new(device), context: ManuallyDrop::new(context), warm: true });
        }
        let (device, context) = create_device(adapter)?;
        Ok(Self { device: ManuallyDrop::new(device), context: ManuallyDrop::new(context), warm: false })
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    pub fn is_warm(&self) -> bool {
        self.warm
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // SAFETY: both fields are taken exactly once, here, and never used afterwards.
        let (device, context) = unsafe { (ManuallyDrop::take(&mut self.device), ManuallyDrop::take(&mut self.context)) };
        if !self.warm {
            release_in_background(device, context);
        }
    }
}

/// Creates one idle D3D11 device per adapter that owns a display output and keeps it alive until
/// `release_warm_devices`, and loads the Windows.Graphics.Capture classes. Creating the first device on a cold GPU driver costs ~120 ms (NVIDIA), later ones ~25 ms,
/// so a resident idle device is what makes `capture_all` fast. No duplication is kept.
pub fn warm_up() -> Result<usize> {
    let mut devices: Vec<WarmDevice> = Vec::new();
    for output in dxgi::enumerate_outputs()? {
        let luid = adapter_luid(&output.adapter)?;
        if devices.iter().any(|warm| warm.luid == luid) {
            continue;
        }
        let (device, _) = create_device(&output.adapter)?;
        if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
            // SAFETY: plain setter; captures on different monitors share this device's immediate context.
            let _ = unsafe { multithread.SetMultithreadProtected(true) };
        }
        if let Err(error) = graphics_capture::prewarm(&device, output.desc.Monitor) {
            log::debug!("Windows.Graphics.Capture pre-warm skipped: {error:#}");
        }
        devices.push(WarmDevice { luid, device });
    }
    let count = devices.len();
    *WARM_DEVICES.lock().unwrap_or_else(PoisonError::into_inner) = devices;
    Ok(count)
}

pub fn release_warm_devices() {
    WARM_DEVICES.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

fn warm_device(luid: AdapterLuid) -> Option<ID3D11Device> {
    let warm = WARM_DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
    let device = &warm.iter().find(|warm| warm.luid == luid)?.device;
    // SAFETY: plain getter on a live device.
    unsafe { device.GetDeviceRemovedReason() }.is_ok().then(|| device.clone())
}

fn adapter_luid(adapter: &IDXGIAdapter1) -> Result<AdapterLuid> {
    // SAFETY: GetDesc1 only fills the returned struct.
    let desc = unsafe { adapter.GetDesc1() }.context("IDXGIAdapter1::GetDesc1")?;
    Ok((desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart))
}

fn create_device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    // SAFETY: out pointers are valid; the adapter is the one that owns the captured output.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .context("D3D11CreateDevice on the output's adapter")?;
    Ok((device.context("D3D11CreateDevice returned no device")?, context.context("no device context")?))
}

/// Tearing a D3D11 device down costs 20 to 300 ms of driver time, which the hotkey path should not wait for.
/// By the time this is called the duplication, capture session and textures are already released.
fn release_in_background(device: ID3D11Device, context: ID3D11DeviceContext) {
    let released = std::thread::Builder::new().name("glint-gpu-release".into()).spawn(move || {
        drop(context);
        drop(device);
    });
    if let Err(error) = released {
        log::warn!("could not start the GPU release thread, released inline: {error}");
    }
}

/// Queues a GPU copy of `source` into a CPU-readable staging texture of the same size and format.
pub fn stage(gpu: &Gpu, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: GetDesc only fills the passed struct.
    unsafe { source.GetDesc(&mut desc) };
    let staging_desc = D3D11_TEXTURE2D_DESC {
        MipLevels: 1,
        ArraySize: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..desc
    };
    let mut staging = None;
    // SAFETY: the descriptor is fully initialised and the out pointer is valid.
    unsafe { gpu.device().CreateTexture2D(&staging_desc, None, Some(&mut staging)) }.context("create staging texture")?;
    let staging = staging.context("CreateTexture2D returned no texture")?;
    // SAFETY: both textures belong to this device and have identical size and format.
    unsafe { gpu.context().CopyResource(&staging, source) };
    Ok(staging)
}

/// Maps a staged texture and converts it to a desktop-oriented frame matching `monitor.rect`.
pub fn read_staged(
    gpu: &Gpu,
    staging: &ID3D11Texture2D,
    monitor: &MonitorInfo,
    rotation: Rotation,
) -> Result<DesktopFrame> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: GetDesc only fills the passed struct.
    unsafe { staging.GetDesc(&mut desc) };
    let (texture_w, texture_h) = (desc.Width as usize, desc.Height as usize);
    let (desktop_w, desktop_h) = rotation.desktop_size(texture_w, texture_h);
    ensure!(
        (desktop_w as i32, desktop_h as i32) == (monitor.rect.w, monitor.rect.h),
        "captured desktop is {desktop_w}x{desktop_h} but the monitor rect is {}x{}",
        monitor.rect.w,
        monitor.rect.h
    );

    let mapped = Mapped::new(gpu.context(), staging, texture_h)?;
    match desc.Format {
        DXGI_FORMAT_R16G16B16A16_FLOAT => {
            let (pixels, width, height) =
                orient(|y| mapped.row::<[f16; 4]>(y, texture_w), texture_w, texture_h, rotation);
            Ok(DesktopFrame::Fp16(Fp16Frame {
                width: width as u32,
                height: height as u32,
                data: pixels.into_flattened(),
            }))
        }
        DXGI_FORMAT_B8G8R8A8_UNORM => {
            let (pixels, width, height) = orient(|y| mapped.row::<[u8; 4]>(y, texture_w), texture_w, texture_h, rotation);
            let mut data = pixels.into_flattened();
            data.chunks_exact_mut(4).for_each(|pixel| pixel[3] = 255);
            Ok(DesktopFrame::Bgra(Image::from_bgra(width as u32, height as u32, data)))
        }
        other => bail!("unsupported capture format {}", other.0),
    }
}

/// A mapped staging texture that unmaps on drop.
struct Mapped<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
    data: *const u8,
    row_pitch: usize,
    rows: usize,
}

impl<'a> Mapped<'a> {
    fn new(context: &'a ID3D11DeviceContext, texture: &'a ID3D11Texture2D, rows: usize) -> Result<Self> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture has CPU read access; the out pointer is valid.
        unsafe { context.Map(texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.context("map staging texture")?;
        Ok(Self { context, texture, data: mapped.pData as *const u8, row_pitch: mapped.RowPitch as usize, rows })
    }

    fn row<P>(&self, y: usize, width: usize) -> &[P] {
        assert!(y < self.rows && width * size_of::<P>() <= self.row_pitch);
        // SAFETY: the mapping is live for `self`, holds `rows` rows of `row_pitch` bytes, and the assert keeps the slice
        // inside row `y`. Mapped rows start 16-byte aligned, which satisfies every pixel type used here.
        unsafe { std::slice::from_raw_parts(self.data.add(y * self.row_pitch).cast::<P>(), width) }
    }
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        // SAFETY: subresource 0 was mapped in `new` and is unmapped exactly once.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}
