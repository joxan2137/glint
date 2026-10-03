//! DESIGN.md section 4 as three D3D11 compute shaders queued back to back without a CPU round trip: a statistics pass
//! (histogram, maximum, HDR and out-of-gamut counts), a curve pass (99.9th percentile peak and the BT.2390 m -> m'
//! table) and the tone map itself. The tables mirror `glint-core::tonemap` exactly.

use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use glint_core::HdrStats;
use windows::Win32::Graphics::Direct3D::Fxc::{
    D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_IEEE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile,
};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, ID3DInclude};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_UNORDERED_ACCESS, D3D11_BUFFER_DESC,
    D3D11_CPU_ACCESS_READ, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, D3D11_SUBRESOURCE_DATA, D3D11_USAGE,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_IMMUTABLE, D3D11_USAGE_STAGING, ID3D11Buffer, ID3D11ComputeShader, ID3D11Device,
    ID3D11ShaderResourceView, ID3D11UnorderedAccessView,
};
use windows::core::{PCSTR, s};

use crate::gpu::Gpu;

pub const HISTOGRAM_BINS: usize = 2048;
pub const STATS_WORDS: usize = HISTOGRAM_BINS + 3;
pub const CURVE_LUT_SIZE: usize = 4096;
pub const HDR_THRESHOLD: f32 = 1.0 + 0.5 / 255.0;
const AUX_WORDS: usize = 1 + CURVE_LUT_SIZE;
const MIDPOINT_COUNT: usize = 255;
const MANTISSA_TABLE_SIZE: usize = 2049;
const LOG_MIN: f32 = -16.0;
const LOG_MAX: f32 = 8.0;
const GROUP_SIZE: u32 = 16;
const HLSL: &str = include_str!("tonemap.hlsl");

struct Bytecode {
    stats: Vec<u8>,
    curve: Vec<u8>,
    tonemap: Vec<u8>,
}

static BYTECODE: OnceLock<Result<Bytecode, String>> = OnceLock::new();

/// Shader objects and constant tables for one device.
pub struct ToneMapper {
    stats_shader: ID3D11ComputeShader,
    curve_shader: ID3D11ComputeShader,
    tonemap_shader: ID3D11ComputeShader,
    tables: ID3D11ShaderResourceView,
}

/// Per-capture constants; layout must match `Params` in tonemap.hlsl.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Params {
    pub width: u32,
    pub height: u32,
    pub percentile_rank: u32,
    pub auto_mode: u32,
    pub scale: f32,
    pub normalization: f32,
    pub hdr_threshold: f32,
    pub exposure: f32,
    pub sdr_white_nits: f32,
    pub lut_step: f32,
    pub unused: [f32; 2],
}

/// CPU-readable copies of the statistics counters and the packed BGRA8 tone map output; each is ready once the GPU
/// has executed the queued passes.
pub struct Queued {
    pub stats_readback: ID3D11Buffer,
    pub output_readback: ID3D11Buffer,
}

impl ToneMapper {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        let bytecode = bytecode()?;
        let shader = |code: &[u8], name: &str| -> Result<ID3D11ComputeShader> {
            let mut shader = None;
            // SAFETY: `code` is a valid compiled shader; the out pointer is valid.
            unsafe { device.CreateComputeShader(code, None, Some(&mut shader)) }
                .with_context(|| format!("create {name} shader"))?;
            shader.with_context(|| format!("no {name} shader"))
        };
        let tables = static_tables();
        let table_buffer = create_buffer(
            device,
            size_of_val(&tables[..]),
            D3D11_USAGE_IMMUTABLE,
            D3D11_BIND_SHADER_RESOURCE.0 as u32,
            Some(as_bytes(&tables)),
        )?;
        Ok(Self {
            stats_shader: shader(&bytecode.stats, "statistics")?,
            curve_shader: shader(&bytecode.curve, "curve")?,
            tonemap_shader: shader(&bytecode.tonemap, "tone map")?,
            tables: shader_view(device, &table_buffer)?,
        })
    }

    /// Queues statistics, curve (Auto only), tone map and the readback copies; the caller flushes.
    /// `source` is the FP16 desktop copy, `params.auto_mode` selects the highlight compression.
    pub fn queue(&self, gpu: &Gpu, source: &ID3D11ShaderResourceView, params: &Params) -> Result<Queued> {
        let device = gpu.device();
        let context = gpu.context();
        let output_bytes = params.width as usize * params.height as usize * 4;

        // Recycled GPU memory is not guaranteed to be zero, so the counters and the curve flag start from zeros.
        let stats =
            create_buffer(device, STATS_WORDS * 4, D3D11_USAGE_DEFAULT, uav_flags(), Some(&[0u8; STATS_WORDS * 4]))?;
        let aux_bytes = vec![0u8; AUX_WORDS * 4];
        let aux = create_buffer(
            device,
            AUX_WORDS * 4,
            D3D11_USAGE_DEFAULT,
            uav_flags() | D3D11_BIND_SHADER_RESOURCE.0 as u32,
            Some(&aux_bytes),
        )?;
        let output = create_buffer(device, output_bytes, D3D11_USAGE_DEFAULT, uav_flags(), None)?;
        let stats_readback = create_readback(device, STATS_WORDS * 4)?;
        let output_readback = create_readback(device, output_bytes)?;

        let constants = [Some(create_constants(device, params)?)];
        let stats_view = unordered_view(device, &stats)?;
        let aux_view = unordered_view(device, &aux)?;
        let output_view = unordered_view(device, &output)?;
        let aux_resource = shader_view(device, &aux)?;
        let (groups_x, groups_y) = (params.width.div_ceil(GROUP_SIZE), params.height.div_ceil(GROUP_SIZE));
        let tables = self.tables.clone();

        // SAFETY: every view and buffer was created on this device and outlives the queued commands; each pass unbinds
        // its resources so the shared immediate context keeps no reference to this capture.
        unsafe {
            context.CSSetConstantBuffers(0, Some(&constants));

            context.CSSetShader(&self.stats_shader, None);
            context.CSSetShaderResources(0, Some(&[Some(source.clone()), Some(tables.clone())]));
            context.CSSetUnorderedAccessViews(0, 1, Some([Some(stats_view.clone())].as_ptr()), None);
            context.Dispatch(groups_x, groups_y, 1);
            unbind(gpu);

            if params.auto_mode != 0 {
                context.CSSetShader(&self.curve_shader, None);
                let views = [Some(stats_view), None, Some(aux_view)];
                context.CSSetUnorderedAccessViews(0, 3, Some(views.as_ptr()), None);
                context.Dispatch(1, 1, 1);
                unbind(gpu);
            }

            context.CSSetShader(&self.tonemap_shader, None);
            context.CSSetShaderResources(0, Some(&[Some(source.clone()), Some(tables), Some(aux_resource)]));
            let views = [None, Some(output_view)];
            context.CSSetUnorderedAccessViews(0, 2, Some(views.as_ptr()), None);
            context.Dispatch(groups_x, groups_y, 1);
            unbind(gpu);

            context.CopyResource(&stats_readback, &stats);
            context.CopyResource(&output_readback, &output);
        }
        Ok(Queued { stats_readback, output_readback })
    }
}

/// Unbinds everything the passes use.
unsafe fn unbind(gpu: &Gpu) {
    let context = gpu.context();
    // SAFETY: binding null views is always valid.
    unsafe {
        context.CSSetShaderResources(0, Some(&[None, None, None]));
        context.CSSetUnorderedAccessViews(0, 3, Some([None, None, None].as_ptr()), None);
    }
}

fn uav_flags() -> u32 {
    D3D11_BIND_UNORDERED_ACCESS.0 as u32
}

fn create_buffer(
    device: &ID3D11Device,
    byte_width: usize,
    usage: D3D11_USAGE,
    bind_flags: u32,
    initial: Option<&[u8]>,
) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: byte_width as u32,
        Usage: usage,
        BindFlags: bind_flags,
        CPUAccessFlags: 0,
        MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
        StructureByteStride: 4,
    };
    build_buffer(device, &desc, initial)
}

fn create_readback(device: &ID3D11Device, byte_width: usize) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: byte_width as u32,
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    build_buffer(device, &desc, None)
}

fn build_buffer(device: &ID3D11Device, desc: &D3D11_BUFFER_DESC, initial: Option<&[u8]>) -> Result<ID3D11Buffer> {
    let data = initial.map(|bytes| D3D11_SUBRESOURCE_DATA { pSysMem: bytes.as_ptr().cast(), ..Default::default() });
    let mut buffer = None;
    // SAFETY: `desc` and the optional initial data describe the same live byte range; the out pointer is valid.
    unsafe { device.CreateBuffer(desc, data.as_ref().map(std::ptr::from_ref), Some(&mut buffer)) }
        .context("create buffer")?;
    buffer.context("CreateBuffer returned no buffer")
}

fn create_constants(device: &ID3D11Device, params: &Params) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: size_of::<Params>() as u32,
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        ..Default::default()
    };
    let data = D3D11_SUBRESOURCE_DATA { pSysMem: std::ptr::from_ref(params).cast(), ..Default::default() };
    let mut buffer = None;
    // SAFETY: `params` is a live repr(C) value of exactly ByteWidth bytes.
    unsafe { device.CreateBuffer(&desc, Some(&data), Some(&mut buffer)) }.context("create constant buffer")?;
    buffer.context("CreateBuffer returned no buffer")
}

fn shader_view(device: &ID3D11Device, buffer: &ID3D11Buffer) -> Result<ID3D11ShaderResourceView> {
    let mut view = None;
    // SAFETY: a null description views the whole structured buffer.
    unsafe { device.CreateShaderResourceView(buffer, None, Some(&mut view)) }.context("create shader resource view")?;
    view.context("no shader resource view")
}

fn unordered_view(device: &ID3D11Device, buffer: &ID3D11Buffer) -> Result<ID3D11UnorderedAccessView> {
    let mut view = None;
    // SAFETY: a null description views the whole structured buffer.
    unsafe { device.CreateUnorderedAccessView(buffer, None, Some(&mut view)) }.context("create unordered access view")?;
    view.context("no unordered access view")
}

fn as_bytes(values: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and any byte pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast(), size_of_val(values)) }
}

fn bytecode() -> Result<&'static Bytecode> {
    BYTECODE
        .get_or_init(|| compile().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map_err(|error| anyhow!("tone map shader compile failed: {error}"))
}

fn compile() -> Result<Bytecode> {
    Ok(Bytecode {
        stats: compile_entry(s!("stats_main"))?,
        curve: compile_entry(s!("curve_main"))?,
        tonemap: compile_entry(s!("tonemap_main"))?,
    })
}

fn compile_entry(entry: PCSTR) -> Result<Vec<u8>> {
    let flags = D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_IEEE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3;
    let mut code = None;
    let mut errors = None;
    // SAFETY: the source slice outlives the call; out pointers are valid.
    let compiled = unsafe {
        D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            s!("tonemap.hlsl"),
            None,
            None::<&ID3DInclude>,
            entry,
            s!("cs_5_0"),
            flags,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = compiled {
        let message = errors.map(|blob| blob_bytes(&blob)).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        bail!("{error}: {}", message.unwrap_or_default());
    }
    Ok(blob_bytes(&code.context("D3DCompile returned no bytecode")?))
}

fn blob_bytes(blob: &ID3DBlob) -> Vec<u8> {
    // SAFETY: the blob owns GetBufferSize bytes at GetBufferPointer for as long as it lives.
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()) }.to_vec()
}

/// sRGB midpoints followed by the mantissa log2 table, as laid out in `tables` of tonemap.hlsl.
fn static_tables() -> Vec<f32> {
    let mut tables = srgb_midpoints();
    tables.extend(mantissa_log2());
    tables
}

fn srgb_midpoints() -> Vec<f32> {
    (0..MIDPOINT_COUNT).map(|code| srgb_eotf((code as f32 + 0.5) / 255.0)).collect()
}

fn srgb_eotf(encoded: f32) -> f32 {
    if encoded <= 0.04045 { encoded / 12.92 } else { ((encoded + 0.055) / 1.055).powf(2.4) }
}

fn mantissa_log2() -> Vec<f32> {
    (0..MANTISSA_TABLE_SIZE).map(|index| (1.0 + index as f32 / 2048.0).log2()).collect()
}

pub fn sdr_normalization(sdr_white_nits: f32) -> f32 {
    if sdr_white_nits.is_finite() && sdr_white_nits > 0.0 { 80.0 / sdr_white_nits } else { 1.0 }
}

/// Step of the m -> m' table over log2(m), exactly as glint-core computes it.
pub fn lut_step() -> f32 {
    (LOG_MAX - LOG_MIN) / (CURVE_LUT_SIZE - 1) as f32
}

/// Rank of the 99.9th percentile pixel, as in `glint-core::tonemap::analyze`.
pub fn percentile_rank(pixel_count: u64) -> u32 {
    (pixel_count * 999).div_ceil(1000) as u32
}

/// Reduces the GPU counters the same way `glint-core::tonemap::analyze` does.
pub fn stats_from_counters(words: &[u32], pixel_count: u64) -> HdrStats {
    if pixel_count == 0 || words.len() < STATS_WORDS {
        return HdrStats::default();
    }
    let maximum = f32::from_bits(words[HISTOGRAM_BINS + 2]);
    let rank = u64::from(percentile_rank(pixel_count));
    let mut cumulative = 0u64;
    let mut percentile_bin = 0;
    for (index, count) in words[..HISTOGRAM_BINS].iter().enumerate() {
        cumulative += u64::from(*count);
        if cumulative >= rank {
            percentile_bin = index;
            break;
        }
    }
    let peak = if maximum <= 0.0 {
        0.0
    } else {
        let bin_width = (LOG_MAX - LOG_MIN) / HISTOGRAM_BINS as f32;
        2.0_f32.powf(LOG_MIN + (percentile_bin as f32 + 0.5) * bin_width)
    };
    let count = pixel_count as f32;
    HdrStats {
        peak,
        max: maximum,
        hdr_fraction: words[HISTOGRAM_BINS] as f32 / count,
        out_of_gamut_fraction: words[HISTOGRAM_BINS + 1] as f32 / count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_have_the_layout_the_shader_expects() {
        let tables = static_tables();
        assert_eq!(tables.len(), MIDPOINT_COUNT + MANTISSA_TABLE_SIZE);
        assert!(tables[..MIDPOINT_COUNT].windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(tables[MIDPOINT_COUNT], 0.0);
        assert_eq!(tables[MIDPOINT_COUNT + MANTISSA_TABLE_SIZE - 1], 1.0);
    }

    #[test]
    fn counters_reduce_to_the_999th_percentile_bin() {
        let mut words = vec![0u32; STATS_WORDS];
        words[1024] = 9_990;
        words[1500] = 10;
        words[HISTOGRAM_BINS] = 10;
        words[HISTOGRAM_BINS + 2] = 4.0_f32.to_bits();
        let stats = stats_from_counters(&words, 10_000);
        let bin_width = (LOG_MAX - LOG_MIN) / HISTOGRAM_BINS as f32;
        assert!((stats.peak.log2() - (LOG_MIN + 1024.5 * bin_width)).abs() < 1e-4);
        assert_eq!(stats.max, 4.0);
        assert!((stats.hdr_fraction - 0.001).abs() < 1e-9);
    }

    #[test]
    fn percentile_rank_rounds_up() {
        assert_eq!(percentile_rank(1000), 999);
        assert_eq!(percentile_rank(1001), 1000);
    }
}
