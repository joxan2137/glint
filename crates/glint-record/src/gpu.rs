use anyhow::{Context, anyhow, bail};
use glint_core::RectI;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC,
    D3D11_COMPARISON_NEVER, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SAMPLER_DESC, D3D11_SDK_VERSION, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11PixelShader, ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView,
    ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::core::{Interface, PCSTR, s};

use crate::tonecurve::ToneCurve;

const SHADER_SOURCE: &str = include_str!("convert.hlsl");

/// D3D11 device shared by Windows.Graphics.Capture, the convert pass and the Media Foundation encoder.
pub(crate) struct Gpu {
    pub device: ID3D11Device,
    context: ID3D11DeviceContext,
    multithread: ID3D11Multithread,
}

impl Gpu {
    /// Creates the device on the adapter that drives `monitor`, so captured frames never cross adapters.
    pub fn for_monitor(monitor: HMONITOR) -> anyhow::Result<Self> {
        match adapter_for_monitor(monitor) {
            Some(adapter) => Self::create(Some(&adapter)).or_else(|error| {
                log::warn!("device on the monitor's adapter failed ({error:#}); using the default adapter");
                Self::create(None)
            }),
            None => Self::create(None),
        }
    }

    fn create(adapter: Option<&IDXGIAdapter>) -> anyhow::Result<Self> {
        let with_video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        let (device, context) = create_device(adapter, with_video)
            .or_else(|_| create_device(adapter, D3D11_CREATE_DEVICE_BGRA_SUPPORT))
            .context("D3D11CreateDevice")?;
        let multithread: ID3D11Multithread = context.cast().context("ID3D11Multithread")?;
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
        Ok(Self { device, context, multithread })
    }

    pub fn winrt_device(&self) -> anyhow::Result<IDirect3DDevice> {
        let dxgi_device: IDXGIDevice = self.device.cast()?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }?;
        Ok(inspectable.cast()?)
    }

    /// Runs `f` with the immediate context locked against Media Foundation's worker threads.
    pub fn with_context<R>(&self, f: impl FnOnce(&ID3D11DeviceContext) -> R) -> R {
        unsafe { self.multithread.Enter() };
        let result = f(&self.context);
        unsafe { self.multithread.Leave() };
        result
    }

    pub fn texture(&self, desc: &D3D11_TEXTURE2D_DESC) -> anyhow::Result<ID3D11Texture2D> {
        let mut texture = None;
        unsafe { self.device.CreateTexture2D(desc, None, Some(&mut texture)) }.context("CreateTexture2D")?;
        texture.ok_or_else(|| anyhow!("CreateTexture2D returned nothing"))
    }

    pub fn render_target(&self, width: u32, height: u32) -> anyhow::Result<RenderTarget> {
        let texture = self.texture(&D3D11_TEXTURE2D_DESC {
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            ..bgra_desc(width, height)
        })?;
        let mut view = None;
        unsafe { self.device.CreateRenderTargetView(&texture, None, Some(&mut view)) }
            .context("CreateRenderTargetView")?;
        let view = view.ok_or_else(|| anyhow!("CreateRenderTargetView returned nothing"))?;
        Ok(RenderTarget { texture, view, width, height })
    }
}

fn create_device(
    adapter: Option<&IDXGIAdapter>,
    flags: D3D11_CREATE_DEVICE_FLAG,
) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
    let driver = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            driver,
            HMODULE::default(),
            flags,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }?;
    match (device, context) {
        (Some(device), Some(context)) => Ok((device, context)),
        _ => Err(windows::core::Error::empty()),
    }
}

fn adapter_for_monitor(monitor: HMONITOR) -> Option<IDXGIAdapter> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    (0..)
        .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
        .find(|adapter: &IDXGIAdapter1| {
            (0..)
                .map_while(|index| unsafe { adapter.EnumOutputs(index) }.ok())
                .any(|output| unsafe { output.GetDesc() }.is_ok_and(|desc| desc.Monitor == monitor))
        })
        .and_then(|adapter| adapter.cast().ok())
}

fn bgra_desc(width: u32, height: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    }
}

/// A BGRA8 texture the convert pass renders into.
pub(crate) struct RenderTarget {
    pub texture: ID3D11Texture2D,
    view: ID3D11RenderTargetView,
    pub width: u32,
    pub height: u32,
}

/// Copies BGRA8 textures of one size back to the CPU.
pub(crate) struct Readback {
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl Readback {
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> anyhow::Result<Self> {
        let staging = gpu.texture(&D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..bgra_desc(width, height)
        })?;
        Ok(Self { staging, width, height })
    }

    /// Tightly packed BGRA rows of `texture`.
    pub fn read(&self, gpu: &Gpu, texture: &ID3D11Texture2D) -> anyhow::Result<Vec<u8>> {
        let row_bytes = self.width as usize * 4;
        let mut pixels = vec![0u8; row_bytes * self.height as usize];
        gpu.with_context(|context| -> anyhow::Result<()> {
            unsafe {
                context.CopyResource(&self.staging, texture);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).context("Map staging texture")?;
                for (row, target) in pixels.chunks_exact_mut(row_bytes).enumerate() {
                    let source = (mapped.pData as *const u8).add(row * mapped.RowPitch as usize);
                    target.copy_from_slice(std::slice::from_raw_parts(source, row_bytes));
                }
                context.Unmap(&self.staging, 0);
            }
            Ok(())
        })?;
        Ok(pixels)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ShaderParams {
    region_origin: [i32; 2],
    source_max: [i32; 2],
    source_step: [f32; 2],
    texel_size: [f32; 2],
    mode: u32,
    exposure_scale: f32,
    sdr_white_nits: f32,
    eetf_source_pq: f32,
    eetf_max_lum: f32,
    eetf_knee: f32,
    resample: u32,
    unused: f32,
}

/// The per-frame GPU pass: crop the region, tone map scRGB (or copy BGRA8), scale to the encoded size.
pub(crate) struct Converter {
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
    region: RectI,
    curve: ToneCurve,
    /// For capture textures that cannot be bound as shader resources.
    shader_copy: Option<ID3D11Texture2D>,
}

impl Converter {
    pub fn new(gpu: &Gpu, region: RectI, curve: ToneCurve) -> anyhow::Result<Self> {
        let vertex_code = compile(s!("vs_main"), s!("vs_4_0"))?;
        let pixel_code = compile(s!("ps_main"), s!("ps_4_0"))?;
        let device = &gpu.device;
        let mut vertex_shader = None;
        let mut pixel_shader = None;
        let mut sampler = None;
        let mut constants = None;
        unsafe {
            device.CreateVertexShader(&vertex_code, None, Some(&mut vertex_shader)).context("CreateVertexShader")?;
            device.CreatePixelShader(&pixel_code, None, Some(&mut pixel_shader)).context("CreatePixelShader")?;
            let sampler_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&sampler_desc, Some(&mut sampler)).context("CreateSamplerState")?;
            let constants_desc = D3D11_BUFFER_DESC {
                ByteWidth: size_of::<ShaderParams>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            device.CreateBuffer(&constants_desc, None, Some(&mut constants)).context("CreateBuffer")?;
        }
        let missing = || anyhow!("D3D11 returned no object");
        Ok(Self {
            vertex_shader: vertex_shader.ok_or_else(missing)?,
            pixel_shader: pixel_shader.ok_or_else(missing)?,
            sampler: sampler.ok_or_else(missing)?,
            constants: constants.ok_or_else(missing)?,
            region,
            curve,
            shader_copy: None,
        })
    }

    /// Renders the region of `source` (whose valid content is `content_width` x `content_height`) into `target`.
    pub fn convert(
        &mut self,
        gpu: &Gpu,
        source: &ID3D11Texture2D,
        content_width: u32,
        content_height: u32,
        target: &RenderTarget,
    ) -> anyhow::Result<()> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        let readable = self.shader_readable(gpu, source, &desc)?;
        let mut view: Option<ID3D11ShaderResourceView> = None;
        unsafe { gpu.device.CreateShaderResourceView(&readable, None, Some(&mut view)) }
            .context("CreateShaderResourceView on the captured frame")?;
        let params = self.params(&desc, content_width, content_height, target);
        gpu.with_context(|context| unsafe {
            if readable != *source {
                context.CopyResource(&readable, source);
            }
            context.UpdateSubresource(&self.constants, 0, None, &params as *const ShaderParams as *const _, 0, 0);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.IASetInputLayout(None);
            context.VSSetShader(&self.vertex_shader, None);
            context.PSSetShader(&self.pixel_shader, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            context.PSSetShaderResources(0, Some(&[view]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            context.OMSetRenderTargets(Some(&[Some(target.view.clone())]), None);
            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: target.width as f32,
                Height: target.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            context.Draw(3, 0);
            context.PSSetShaderResources(0, Some(&[None]));
            context.OMSetRenderTargets(None, None);
        });
        Ok(())
    }

    fn shader_readable(
        &mut self,
        gpu: &Gpu,
        source: &ID3D11Texture2D,
        desc: &D3D11_TEXTURE2D_DESC,
    ) -> anyhow::Result<ID3D11Texture2D> {
        if desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0 && desc.SampleDesc.Count == 1 {
            return Ok(source.clone());
        }
        let copy_desc = D3D11_TEXTURE2D_DESC {
            MipLevels: 1,
            ArraySize: 1,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            ..*desc
        };
        let reusable = self.shader_copy.as_ref().filter(|copy| {
            let mut existing = D3D11_TEXTURE2D_DESC::default();
            unsafe { copy.GetDesc(&mut existing) };
            existing.Width == desc.Width && existing.Height == desc.Height && existing.Format == desc.Format
        });
        if let Some(copy) = reusable {
            return Ok(copy.clone());
        }
        let copy = gpu.texture(&copy_desc)?;
        self.shader_copy = Some(copy.clone());
        Ok(copy)
    }

    fn params(
        &self,
        source: &D3D11_TEXTURE2D_DESC,
        content_width: u32,
        content_height: u32,
        target: &RenderTarget,
    ) -> ShaderParams {
        let visible_width = content_width.min(source.Width).max(1);
        let visible_height = content_height.min(source.Height).max(1);
        let step = [self.region.w as f32 / target.width as f32, self.region.h as f32 / target.height as f32];
        ShaderParams {
            region_origin: [self.region.x, self.region.y],
            source_max: [visible_width as i32 - 1, visible_height as i32 - 1],
            source_step: step,
            texel_size: [1.0 / source.Width as f32, 1.0 / source.Height as f32],
            mode: self.curve.mode as u32,
            exposure_scale: self.curve.exposure_scale,
            sdr_white_nits: self.curve.sdr_white_nits,
            eetf_source_pq: self.curve.source_pq,
            eetf_max_lum: self.curve.max_lum,
            eetf_knee: self.curve.knee,
            resample: u32::from(step != [1.0, 1.0]),
            unused: 0.0,
        }
    }
}

fn compile(entry_point: PCSTR, target: PCSTR) -> anyhow::Result<Vec<u8>> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let compiled = unsafe {
        D3DCompile(
            SHADER_SOURCE.as_ptr() as *const _,
            SHADER_SOURCE.len(),
            s!("convert.hlsl"),
            None,
            None,
            entry_point,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = compiled {
        let log = errors.map(|blob| String::from_utf8_lossy(blob_bytes(&blob)).into_owned()).unwrap_or_default();
        bail!("D3DCompile failed ({error}): {log}");
    }
    let code = code.ok_or_else(|| anyhow!("D3DCompile produced no code"))?;
    Ok(blob_bytes(&code).to_vec())
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) }
}
