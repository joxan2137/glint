//! Per-UI-thread graphics state: D3D11/D2D/DirectWrite/WIC/DirectComposition plus device-lost recovery.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use anyhow::{Context, Result};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS, D2D1_BITMAP_PROPERTIES1, D2D1_DEBUG_LEVEL_NONE, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1, ID2D1GradientStopCollection1,
    ID2D1SolidColorBrush, ID2D1StrokeStyle1,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice3, IDCompositionDesktopDevice};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, IDXGIAdapter, IDXGIDevice, IDXGIFactory2,
};
use windows::Win32::Foundation::D2DERR_RECREATE_TARGET;
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::{HRESULT, Interface};

use crate::icons::{Icon, IconGeometry};
use crate::painter::{ShadowKey, StrokeKey};
use crate::text::TextSystem;

/// Device-dependent objects. Cloning only bumps COM reference counts.
#[derive(Clone)]
pub(crate) struct Devices {
    pub d3d: ID3D11Device,
    pub dxgi_device: IDXGIDevice,
    pub dxgi_factory: IDXGIFactory2,
    pub d2d_device: ID2D1Device,
    pub context: ID2D1DeviceContext,
    pub aux_context: ID2D1DeviceContext,
    pub dcomp: Option<IDCompositionDesktopDevice>,
    pub max_bitmap_size: u32,
    pub software: bool,
}

#[derive(Default)]
pub(crate) struct DeviceCache {
    pub solid_brush: Option<ID2D1SolidColorBrush>,
    pub gradients: HashMap<Vec<u32>, ID2D1GradientStopCollection1>,
    pub shadows: HashMap<ShadowKey, ID2D1Bitmap1>,
}

/// Graphics devices and caches for one UI thread. Create once and share with `Rc`.
pub struct Gfx {
    this: Weak<Gfx>,
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    wic: OnceCell<IWICImagingFactory>,
    devices: RefCell<Devices>,
    generation: Cell<u64>,
    pub(crate) text: TextSystem,
    pub(crate) icons: RefCell<HashMap<Icon, Rc<IconGeometry>>>,
    pub(crate) strokes: RefCell<HashMap<StrokeKey, ID2D1StrokeStyle1>>,
    pub(crate) cache: RefCell<DeviceCache>,
}

fn create_d3d_device(driver: D3D_DRIVER_TYPE) -> windows::core::Result<ID3D11Device> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
    let mut device = None;
    // SAFETY: out-pointer is a valid Option<ID3D11Device>; feature level slice outlives the call.
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| windows::core::Error::from_hresult(HRESULT(-2147467259)))
}

fn create_devices(factory: &ID2D1Factory1, force_software: bool) -> Result<Devices> {
    let (d3d, software) = match (force_software, create_d3d_device(D3D_DRIVER_TYPE_HARDWARE)) {
        (false, Ok(device)) => (device, false),
        _ => (create_d3d_device(D3D_DRIVER_TYPE_WARP).context("creating WARP D3D11 device")?, true),
    };
    let dxgi_device: IDXGIDevice = d3d.cast()?;
    raise_gpu_priority(&dxgi_device);
    // SAFETY: plain COM calls on live interfaces.
    unsafe {
        let dxgi_factory: IDXGIFactory2 = dxgi_device.GetAdapter()?.GetParent()?;
        let d2d_device = factory.CreateDevice(&dxgi_device)?;
        let context = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        let aux_context = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        for ctx in [&context, &aux_context] {
            ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        let max_bitmap_size = context.GetMaximumBitmapSize();
        Ok(Devices { d3d, dxgi_device, dxgi_factory, d2d_device, context, aux_context, dcomp: None, max_bitmap_size, software })
    }
}

/// Lets our small UI frames overtake a busy game's GPU queue: the highest `SetGPUThreadPriority` the process may use
/// (positive values need the increase-base-priority privilege, so this often stays at 0 without elevation).
fn raise_gpu_priority(device: &IDXGIDevice) {
    // SAFETY: plain COM call on a live device.
    match (1..=7).rev().find(|&priority| unsafe { device.SetGPUThreadPriority(priority) }.is_ok()) {
        Some(priority) => log::info!("GPU thread priority {priority}"),
        None => log::info!("GPU thread priority unchanged (raising it is not permitted for this process)"),
    }
}

pub(crate) fn is_device_lost(code: HRESULT) -> bool {
    code == D2DERR_RECREATE_TARGET || code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET
}

impl Gfx {
    pub fn new() -> Result<Rc<Gfx>> {
        Self::with_options(false)
    }

    /// Uses the WARP software rasterizer even when a GPU is present.
    pub fn new_software() -> Result<Rc<Gfx>> {
        Self::with_options(true)
    }

    fn with_options(force_software: bool) -> Result<Rc<Gfx>> {
        // SAFETY: COM initialization for this thread; S_FALSE / RPC_E_CHANGED_MODE are fine to ignore.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let options = D2D1_FACTORY_OPTIONS { debugLevel: D2D1_DEBUG_LEVEL_NONE };
        // SAFETY: options pointer is valid for the call.
        let factory: ID2D1Factory1 = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&options))? };
        // SAFETY: factory creation has no preconditions.
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let devices = create_devices(&factory, force_software)?;
        let text = TextSystem::new(&dwrite)?;
        Ok(Rc::new_cyclic(|this| Gfx {
            this: this.clone(),
            factory,
            dwrite,
            wic: OnceCell::new(),
            devices: RefCell::new(devices),
            generation: Cell::new(1),
            text,
            icons: RefCell::default(),
            strokes: RefCell::default(),
            cache: RefCell::default(),
        }))
    }

    /// The `Rc` this `Gfx` lives in, for APIs that take `&Rc<Gfx>` when only `&Gfx` is at hand. None only if the
    /// `Rc` was unwrapped.
    pub fn shared(&self) -> Option<Rc<Gfx>> {
        self.this.upgrade()
    }

    pub fn factory(&self) -> &ID2D1Factory1 {
        &self.factory
    }

    pub fn dwrite(&self) -> &IDWriteFactory {
        &self.dwrite
    }

    pub fn wic(&self) -> Result<IWICImagingFactory> {
        if let Some(wic) = self.wic.get() {
            return Ok(wic.clone());
        }
        // SAFETY: COM was initialized in `new`.
        let wic: IWICImagingFactory = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        Ok(self.wic.get_or_init(|| wic).clone())
    }

    /// The shared D2D device context every window renders with.
    pub fn context(&self) -> ID2D1DeviceContext {
        self.devices.borrow().context.clone()
    }

    pub fn d3d_device(&self) -> ID3D11Device {
        self.devices.borrow().d3d.clone()
    }

    /// Increments every time devices are rebuilt after a loss; GPU resources tagged with an older value are stale.
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    pub fn max_bitmap_size(&self) -> u32 {
        self.devices.borrow().max_bitmap_size
    }

    pub fn is_software(&self) -> bool {
        self.devices.borrow().software
    }

    pub(crate) fn devices(&self) -> Devices {
        self.devices.borrow().clone()
    }

    pub(crate) fn dcomp(&self) -> Result<IDCompositionDesktopDevice> {
        if let Some(dcomp) = self.devices.borrow().dcomp.clone() {
            return Ok(dcomp);
        }
        let dxgi_device = self.devices.borrow().dxgi_device.clone();
        // SAFETY: a valid DXGI device is passed as the rendering device.
        let dcomp: IDCompositionDesktopDevice = unsafe { DCompositionCreateDevice3(&dxgi_device)? };
        self.devices.borrow_mut().dcomp = Some(dcomp.clone());
        Ok(dcomp)
    }

    /// True when the D3D device was removed or reset.
    pub fn device_lost(&self) -> bool {
        // SAFETY: simple query on a live device.
        unsafe { self.devices.borrow().d3d.GetDeviceRemovedReason().is_err() }
    }

    /// Rebuilds all devices and drops device-dependent caches. Windows and bitmaps notice the new generation and
    /// recreate their GPU objects lazily.
    pub fn recover(&self) -> Result<()> {
        let software = self.devices.borrow().software;
        let devices = create_devices(&self.factory, software)?;
        *self.devices.borrow_mut() = devices;
        *self.cache.borrow_mut() = DeviceCache::default();
        self.generation.set(self.generation.get() + 1);
        log::warn!("graphics device lost; rebuilt devices (generation {})", self.generation.get());
        Ok(())
    }

    /// Recovers when `error` means the device is gone; returns true if it did.
    pub fn recover_if_lost(&self, error: &windows::core::Error) -> Result<bool> {
        if is_device_lost(error.code()) || self.device_lost() {
            self.recover()?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn solid_brush(&self, color: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        let mut cache = self.cache.borrow_mut();
        if let Some(brush) = &cache.solid_brush {
            // SAFETY: color pointer valid for the call.
            unsafe { brush.SetColor(&color) };
            return Ok(brush.clone());
        }
        let context = self.devices.borrow().context.clone();
        // SAFETY: color pointer valid for the call.
        let brush = unsafe { context.CreateSolidColorBrush(&color, None)? };
        cache.solid_brush = Some(brush.clone());
        Ok(brush)
    }

    pub(crate) fn create_bitmap(
        &self,
        width: u32,
        height: u32,
        data: Option<&[u8]>,
        options: D2D1_BITMAP_OPTIONS,
        dpi: f32,
    ) -> Result<ID2D1Bitmap1> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: dpi,
            dpiY: dpi,
            bitmapOptions: options,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let size = windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U { width, height };
        let context = self.devices.borrow().context.clone();
        // SAFETY: when present, `data` holds height rows of width * 4 bytes; props lives across the call.
        unsafe {
            context
                .CreateBitmap(size, data.map(|d| d.as_ptr().cast()), width * 4, &props)
                .with_context(|| format!("creating {width}x{height} bitmap"))
        }
    }
}
