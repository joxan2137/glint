use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use glint_core::settings::HdrSettings;
use glint_core::{Image, MonitorInfo};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIOutput, IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::gpu::Gpu;
use crate::orient::Rotation;
use crate::pipeline::{Grabbed, StageTimings, begin, finish};

const FORMATS: [DXGI_FORMAT; 2] = [DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM];
/// Duplication only delivers a desktop image once something is presented, so a static desktop yields none;
/// the caller then moves on to the next capture method.
const PRESENT_BUDGET: Duration = Duration::from_millis(30);

/// One-shot desktop duplication of `monitor`; the duplication and every texture are released before returning.
pub fn grab(
    monitor: &MonitorInfo,
    output: &IDXGIOutput,
    gpu: &Gpu,
    hdr: &HdrSettings,
    on_sdr: &dyn Fn(&Image),
    clock: &mut StageTimings,
) -> Result<Grabbed> {
    let started = Instant::now();
    let output = output.cast::<IDXGIOutput5>().context("IDXGIOutput5 is not available")?;
    // SAFETY: the device outlives the duplication, which is dropped before this function returns.
    let duplication =
        unsafe { output.DuplicateOutput1(gpu.device(), 0, &FORMATS) }.context("IDXGIOutput5::DuplicateOutput1")?;
    // SAFETY: GetDesc only fills the returned struct.
    let rotation = Rotation::from(unsafe { duplication.GetDesc() }.Rotation);
    clock.setup += started.elapsed();

    let started = Instant::now();
    let acquired = acquire_presented_frame(&duplication);
    clock.acquire += started.elapsed();
    let frame = acquired?;
    let desktop = frame.resource.cast::<ID3D11Texture2D>().context("desktop resource is not a texture")?;
    let in_flight = begin(gpu, &desktop, monitor, rotation, hdr, clock)?;
    drop(frame);
    drop(duplication);
    finish(in_flight, gpu, monitor, hdr, on_sdr, clock)
}

struct AcquiredFrame<'a> {
    duplication: &'a IDXGIOutputDuplication,
    resource: IDXGIResource,
}

impl Drop for AcquiredFrame<'_> {
    fn drop(&mut self) {
        // SAFETY: the frame was acquired from this duplication and is released exactly once.
        let _ = unsafe { self.duplication.ReleaseFrame() };
    }
}

/// Waits for a frame with `LastPresentTime != 0`. Earlier frames of a fresh duplication only carry the cursor and
/// their desktop texture is still empty.
fn acquire_presented_frame(duplication: &IDXGIOutputDuplication) -> Result<AcquiredFrame<'_>> {
    let started = Instant::now();
    loop {
        let remaining = PRESENT_BUDGET.saturating_sub(started.elapsed());
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        // SAFETY: out pointers are valid for the call.
        let acquired =
            unsafe { duplication.AcquireNextFrame(remaining.as_millis().max(1) as u32, &mut info, &mut resource) };
        match acquired {
            Ok(()) => {
                let resource = resource.context("AcquireNextFrame returned no resource")?;
                let frame = AcquiredFrame { duplication, resource };
                if info.LastPresentTime != 0 {
                    return Ok(frame);
                }
            }
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(error) => return Err(error).context("IDXGIOutputDuplication::AcquireNextFrame"),
        }
        ensure!(
            started.elapsed() < PRESENT_BUDGET,
            "no frame with a desktop image within {} ms (static desktop)",
            PRESENT_BUDGET.as_millis()
        );
    }
}
