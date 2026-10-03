use anyhow::{Result, ensure};
use glint_core::Image;
use windows::{
    Win32::{
        Foundation::COLORREF,
        Graphics::Gdi::*,
        UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    },
    core::w,
};

fn test_image() -> Result<Image> {
    unsafe {
        let dc = CreateCompatibleDC(None);
        ensure!(!dc.is_invalid(), "Could not create offscreen DC");
        let mut bits = std::ptr::null_mut();
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 720,
                biHeight: -100,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) => bitmap,
            Err(error) => {
                let _ = DeleteDC(dc);
                return Err(error.into());
            }
        };
        let previous_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        let font = CreateFontW(
            -40,
            0,
            0,
            0,
            700,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            w!("Arial"),
        );
        let previous_font = SelectObject(dc, HGDIOBJ(font.0));
        let bytes = std::slice::from_raw_parts_mut(bits.cast::<u8>(), 720 * 100 * 4);
        bytes.fill(255);
        SetTextColor(dc, COLORREF(0));
        SetBkColor(dc, COLORREF(0xffffff));
        let text: Vec<u16> = "Glint OCR test 12345".encode_utf16().collect();
        let drawn = TextOutW(dc, 20, 24, &text).as_bool();
        let _ = GdiFlush();
        let mut pixels = bytes.to_vec();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        SelectObject(dc, previous_font);
        SelectObject(dc, previous_bitmap);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        ensure!(drawn, "Could not draw OCR self-test text");
        Ok(Image::from_bgra(720, 100, pixels))
    }
}
fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let arg = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Usage: ocr <png> | --self-test"))?;
    let self_test = arg == "--self-test";
    let image = if self_test {
        test_image()?
    } else {
        glint_core::encode::decode(&std::fs::read(arg)?)?
    };
    let result = glint_sys::ocr::ocr(&image)?;
    println!("{}", result.text);
    if self_test {
        ensure!(
            result.text.contains("12345"),
            "OCR self-test did not recognize 12345"
        );
    }
    Ok(())
}
