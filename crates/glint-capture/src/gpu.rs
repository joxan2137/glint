use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use anyhow::{Context, Result, anyhow};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter1, IDXGIDevice};
use windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL;
use windows::core::Interface;

use crate::dxgi;
use crate::graphics_capture;
use crate::orient::{Rotation, orient};
use crate::threads;
use crate::tonemap_gpu::ToneMapper;

type AdapterLuid = (u32, i32);

const HIGHEST_GPU_PRIORITY: i32 = 7;

/// State shared by every capture that uses the same device: the immediate context is single-threaded, so command
/// recording is serialised, and the tone map shaders are built once per device.
#[derive(Default)]
struct Shared {
    command_lock: Mutex<()>,
    tone_mapper: OnceLock<Result<ToneMapper, String>>,
}

impl Shared {
    fn tone_mapper(&self, device: &ID3D11Device) -> Result<&ToneMapper> {
        self.tone_mapper
            .get_or_init(|| ToneMapper::new(device).map_err(|error| format!("{error:#}")))
            .as_ref()
            .map_err(|error| anyhow!("GPU tone mapper unavailable: {error}"))
    }
}

struct WarmDevice {
    luid: AdapterLuid,
    device: ID3D11Device,
    shared: Arc<Shared>,
}

/// Idle devices that keep the GPU driver initialised between captures; see `warm_up`.
static WARM_DEVICES: Mutex<Vec<WarmDevice>> = Mutex::new(Vec::new());
static PRIORITY_LOGGED: AtomicBool = AtomicBool::new(false);

/// The D3D11 device used for one monitor capture: the warm device of the adapter when `warm_up` ran, else a fresh one.
pub struct Gpu {
    device: ManuallyDrop<ID3D11Device>,
    context: ManuallyDrop<ID3D11DeviceContext>,
    warm: bool,
    shared: Arc<Shared>,
}

impl Gpu {
    pub fn for_adapter(adapter: &IDXGIAdapter1) -> Result<Self> {
        if let Some((device, shared)) = warm_device(adapter_luid(adapter)?) {
            // SAFETY: plain getter on a live device.
            let context = unsafe { device.GetImmediateContext() }.context("GetImmediateContext")?;
            return Ok(Self::new(device, context, true, shared));
        }
        let (device, context) = create_device(adapter)?;
        Ok(Self::new(device, context, false, Arc::default()))
    }

    fn new(device: ID3D11Device, context: ID3D11DeviceContext, warm: bool, shared: Arc<Shared>) -> Self {
        Self { device: ManuallyDrop::new(device), context: ManuallyDrop::new(context), warm, shared }
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

    /// Hold this while recording a command sequence on the (possibly shared) immediate context.
    pub fn commands(&self) -> MutexGuard<'_, ()> {
        self.shared.command_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn tone_mapper(&self) -> Result<&ToneMapper> {
        self.shared.tone_mapper(&self.device)
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
/// `release_warm_devices`. It also builds everything the first capture would otherwise pay for: the tone map shaders
/// (compiled once per process) and the Windows.Graphics.Capture classes and session path.
/// Creating the first device on a cold GPU driver costs ~120 ms (NVIDIA), later ones ~25 ms.
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
        let shared = Arc::new(Shared::default());
        if let Err(error) = shared.tone_mapper(&device) {
            log::warn!("GPU tone mapping unavailable, the CPU tone map will be used: {error:#}");
        }
        if let Err(error) = graphics_capture::prewarm(&device, output.desc.Monitor) {
            log::debug!("Windows.Graphics.Capture pre-warm skipped: {error:#}");
        }
        devices.push(WarmDevice { luid, device, shared });
    }
    let count = devices.len();
    *WARM_DEVICES.lock().unwrap_or_else(PoisonError::into_inner) = devices;
    Ok(count)
}

pub fn release_warm_devices() {
    WARM_DEVICES.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

fn warm_device(luid: AdapterLuid) -> Option<(ID3D11Device, Arc<Shared>)> {
    let warm = WARM_DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
    let found = warm.iter().find(|warm| warm.luid == luid)?;
    // SAFETY: plain getter on a live device.
    unsafe { found.device.GetDeviceRemovedReason() }.is_ok().then(|| (found.device.clone(), found.shared.clone()))
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
    let device = device.context("D3D11CreateDevice returned no device")?;
    raise_gpu_priority(&device);
    Ok((device, context.context("no device context")?))
}

/// Queues this device's GPU work ahead of other processes (a game) where the driver and privileges allow it.
fn raise_gpu_priority(device: &ID3D11Device) {
    let Ok(dxgi_device) = device.cast::<IDXGIDevice>() else {
        return;
    };
    let granted = (1..=HIGHEST_GPU_PRIORITY).rev().find(|priority| {
        // SAFETY: plain setter on a live device.
        unsafe { dxgi_device.SetGPUThreadPriority(*priority) }.is_ok()
    });
    let first_time = !PRIORITY_LOGGED.swap(true, Ordering::Relaxed);
    let message = match granted {
        Some(priority) => format!("GPU thread priority raised to {priority}"),
        None => "SetGPUThreadPriority above 0 refused; GPU thread priority stays 0".to_string(),
    };
    if first_time {
        log::info!("{message}");
    } else {
        log::debug!("{message}");
    }
}

/// Tearing a D3D11 device down costs 20 to 300 ms of driver time, which the hotkey path should not wait for.
/// By the time this is called the duplication, capture session and textures are already released.
fn release_in_background(device: ID3D11Device, context: ID3D11DeviceContext) {
    let released = std::thread::Builder::new().name("glint-gpu-release".into()).spawn(move || {
        threads::set_current_priority(THREAD_PRIORITY_BELOW_NORMAL);
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

/// A mapped staging resource that unmaps on drop.
pub struct Mapped<'a> {
    context: &'a ID3D11DeviceContext,
    resource: ID3D11Resource,
    data: *const u8,
    row_pitch: usize,
    rows: usize,
}

impl<'a> Mapped<'a> {
    /// Blocks until the GPU has finished every copy queued into `resource`.
    pub fn new<R: Interface>(context: &'a ID3D11DeviceContext, resource: &R, rows: usize) -> Result<Self> {
        let resource = resource.cast::<ID3D11Resource>().context("not a D3D11 resource")?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging resource has CPU read access; the out pointer is valid.
        unsafe { context.Map(&resource, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.context("map staging resource")?;
        Ok(Self { context, resource, data: mapped.pData as *const u8, row_pitch: mapped.RowPitch as usize, rows })
    }

    pub fn row<P>(&self, y: usize, width: usize) -> &[P] {
        assert!(y < self.rows && width * size_of::<P>() <= self.row_pitch);
        // SAFETY: the mapping is live for `self`, holds `rows` rows of `row_pitch` bytes, and the assert keeps the slice
        // inside row `y`. Mapped rows start 16-byte aligned, which satisfies every pixel type used here.
        unsafe { std::slice::from_raw_parts(self.data.add(y * self.row_pitch).cast::<P>(), width) }
    }

    /// Copies the first `len` bytes of a mapped buffer.
    pub fn bytes(&self, len: usize) -> &[u8] {
        // SAFETY: the caller maps a buffer of at least `len` bytes; the mapping is live for `self`.
        unsafe { std::slice::from_raw_parts(self.data, len) }
    }

    /// Pixels in desktop orientation; one memcpy when the rows are tightly packed and need no rotation.
    pub fn pixels<P: Copy + Default>(&self, width: usize, height: usize, rotation: Rotation) -> (Vec<P>, usize, usize) {
        if rotation == Rotation::Identity && self.row_pitch == width * size_of::<P>() && height <= self.rows {
            // SAFETY: tightly packed rows make the mapping one contiguous run of width * height pixels.
            let all = unsafe { std::slice::from_raw_parts(self.data.cast::<P>(), width * height) };
            return (all.to_vec(), width, height);
        }
        orient(|y| self.row::<P>(y, width), width, height, rotation)
    }
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        // SAFETY: subresource 0 was mapped in `new` and is unmapped exactly once.
        unsafe { self.context.Unmap(&self.resource, 0) };
    }
}

#[cfg(test)]
impl Gpu {
    /// Default hardware adapter, WARP when there is none; for headless tests.
    pub fn standalone() -> Result<Self> {
        use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
        use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            let mut device = None;
            let mut context = None;
            // SAFETY: out pointers are valid.
            let created = unsafe {
                D3D11CreateDevice(
                    None::<&IDXGIAdapter>,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            if created.is_ok() && let (Some(device), Some(context)) = (device, context) {
                return Ok(Self::new(device, context, true, Arc::default()));
            }
        }
        Err(anyhow!("no D3D11 device available"))
    }
}
