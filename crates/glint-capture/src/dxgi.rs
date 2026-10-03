use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_OUTPUT_DESC, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput6,
};
use windows::core::Interface;

pub struct DxgiOutput {
    pub adapter: IDXGIAdapter1,
    pub output: IDXGIOutput,
    pub desc: DXGI_OUTPUT_DESC,
}

#[derive(Clone, Copy, Debug)]
pub struct OutputColor {
    pub hdr_active: bool,
    pub min_nits: f32,
    pub max_nits: f32,
    pub max_full_frame_nits: f32,
}

pub fn enumerate_outputs() -> Result<Vec<DxgiOutput>> {
    // SAFETY: plain COM factory creation and enumeration; every interface is owned and released by the windows crate.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.context("CreateDXGIFactory1")?;
    let mut outputs = Vec::new();
    for adapter_index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(adapter_index) }) else {
            break;
        };
        for output_index in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else {
                break;
            };
            if let Ok(desc) = unsafe { output.GetDesc() } {
                outputs.push(DxgiOutput { adapter: adapter.clone(), output, desc });
            }
        }
    }
    Ok(outputs)
}

pub fn find_output(monitor_handle: isize) -> Result<DxgiOutput> {
    let outputs = enumerate_outputs()?;
    let Some(found) = outputs.into_iter().find(|o| o.desc.Monitor.0 as isize == monitor_handle) else {
        bail!("no DXGI output matches monitor handle {monitor_handle:#x}");
    };
    Ok(found)
}

pub fn output_color(output: &IDXGIOutput) -> Option<OutputColor> {
    let output6 = output.cast::<IDXGIOutput6>().ok()?;
    // SAFETY: GetDesc1 only writes the returned struct.
    let desc = unsafe { output6.GetDesc1() }.ok()?;
    Some(OutputColor {
        hdr_active: desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
        min_nits: desc.MinLuminance,
        max_nits: desc.MaxLuminance,
        max_full_frame_nits: desc.MaxFullFrameLuminance,
    })
}
