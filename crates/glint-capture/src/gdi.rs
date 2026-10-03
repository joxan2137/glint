use std::mem::size_of;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use glint_core::{Image, RectI};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, HBITMAP, HDC, HGDIOBJ, ROP_CODE, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::System::Threading::THREAD_PRIORITY_ABOVE_NORMAL;

use crate::threads;

/// A BitBlt normally takes 20 to 50 ms; it was seen to block for two minutes once, so it runs on a helper thread that is
/// abandoned (it finishes or dies on its own) when the answer takes longer than this.
const BLIT_TIMEOUT: Duration = Duration::from_millis(2000);

/// SDR-only BitBlt of `rect` (virtual-desktop physical pixels) from the screen DC, layered windows included.
pub fn grab(rect: RectI) -> Result<Image> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("glint-gdi-blit".into())
        .spawn(move || {
            threads::set_current_priority(THREAD_PRIORITY_ABOVE_NORMAL);
            let _ = sender.send(grab_blocking(rect));
        })
        .context("start the GDI capture thread")?;
    receiver
        .recv_timeout(BLIT_TIMEOUT)
        .map_err(|_| anyhow!("GDI BitBlt did not finish within {} ms", BLIT_TIMEOUT.as_millis()))?
}

fn grab_blocking(rect: RectI) -> Result<Image> {
    ensure!(!rect.is_empty(), "cannot capture an empty rect");
    let screen = ScreenDc::acquire()?;
    let memory = MemoryDc::compatible_with(&screen)?;
    let dib = Dib::create(&screen, rect.w, rect.h)?;
    // SAFETY: both DCs and the bitmap are valid; the previous bitmap is restored before the DIB is deleted.
    let previous = unsafe { SelectObject(memory.0, HGDIOBJ(dib.bitmap.0)) };
    let blit = unsafe {
        BitBlt(memory.0, 0, 0, rect.w, rect.h, Some(screen.0), rect.x, rect.y, ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0))
    };
    unsafe { SelectObject(memory.0, previous) };
    blit.context("BitBlt from the screen DC")?;
    Ok(dib.read_opaque_image(rect.w as u32, rect.h as u32))
}

struct ScreenDc(HDC);

impl ScreenDc {
    fn acquire() -> Result<Self> {
        // SAFETY: GetDC(NULL) returns the screen DC, released in Drop.
        let dc = unsafe { GetDC(None) };
        ensure!(!dc.is_invalid(), "GetDC(NULL) failed");
        Ok(Self(dc))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: the DC came from GetDC(NULL) and is released once.
        unsafe { ReleaseDC(None, self.0) };
    }
}

struct MemoryDc(HDC);

impl MemoryDc {
    fn compatible_with(screen: &ScreenDc) -> Result<Self> {
        // SAFETY: the screen DC is valid; the memory DC is deleted in Drop.
        let dc = unsafe { CreateCompatibleDC(Some(screen.0)) };
        ensure!(!dc.is_invalid(), "CreateCompatibleDC failed");
        Ok(Self(dc))
    }
}

impl Drop for MemoryDc {
    fn drop(&mut self) {
        // SAFETY: the DC was created by CreateCompatibleDC and is deleted once.
        let _ = unsafe { DeleteDC(self.0) };
    }
}

/// A top-down 32-bit DIB section.
struct Dib {
    bitmap: HBITMAP,
    bits: *const u8,
}

impl Dib {
    fn create(screen: &ScreenDc, width: i32, height: i32) -> Result<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        // SAFETY: `info` describes a plain 32-bit RGB DIB and `bits` is a valid out pointer.
        let bitmap = unsafe { CreateDIBSection(Some(screen.0), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
            .context("CreateDIBSection")?;
        Ok(Self { bitmap, bits: bits as *const u8 })
    }

    fn read_opaque_image(&self, width: u32, height: u32) -> Image {
        let len = width as usize * height as usize * 4;
        // SAFETY: the DIB section owns width * height * 4 bytes (32 bpp, no row padding) for as long as `self` lives.
        let mut data = unsafe { std::slice::from_raw_parts(self.bits, len) }.to_vec();
        data.chunks_exact_mut(4).for_each(|pixel| pixel[3] = 255);
        Image::from_bgra(width, height, data)
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: the bitmap was created by CreateDIBSection and is no longer selected into any DC.
        let _ = unsafe { DeleteObject(HGDIOBJ(self.bitmap.0)) };
    }
}
