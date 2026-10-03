use anyhow::{Result, ensure};
use glint_core::Image;
use std::{path::PathBuf, thread, time::Duration};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HWND},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW,
                SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
    },
    core::w,
};

struct Clipboard;
impl Clipboard {
    fn open(owner: HWND) -> Result<Self> {
        for attempt in 0..10 {
            match unsafe { OpenClipboard(Some(owner)) } {
                Ok(()) => return Ok(Self),
                Err(error) if attempt == 9 => return Err(error.into()),
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
        unreachable!()
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn put_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    ensure!(format != 0, "Could not register clipboard format");
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let destination = GlobalLock(memory);
        if destination.is_null() {
            let error = windows::core::Error::from_thread();
            let _ = GlobalFree(Some(memory));
            return Err(error.into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination.cast(), bytes.len());
        let _ = GlobalUnlock(memory);
        if let Err(error) = SetClipboardData(format, Some(HANDLE(memory.0))) {
            let _ = GlobalFree(Some(memory));
            return Err(error.into());
        }
    }
    Ok(())
}

pub fn dibv5_bytes(img: &Image) -> Vec<u8> {
    let mut bytes = vec![0u8; 124];
    let fields = [
        (0, 124u32),
        (4, img.width),
        (8, img.height),
        (16, 3),
        (20, img.data.len() as u32),
        (40, 0x00ff0000),
        (44, 0x0000ff00),
        (48, 0x000000ff),
        (52, 0xff000000),
        (56, 0x73524742),
        (108, 4),
    ];
    for (offset, value) in fields {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
    if img.width > 0 {
        for row in img.data.chunks_exact(img.stride()).rev() {
            bytes.extend_from_slice(row);
        }
    }
    bytes
}

pub fn copy_image(owner: HWND, img: &Image) -> Result<()> {
    let dib = dibv5_bytes(img);
    let png = glint_core::encode::encode_png(img)?;
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    let _clipboard = Clipboard::open(owner)?;
    unsafe {
        EmptyClipboard()?;
    }
    put_bytes(17, &dib)?;
    put_bytes(png_format, &png)
}

pub fn copy_text(owner: HWND, text: &str) -> Result<()> {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    let _clipboard = Clipboard::open(owner)?;
    unsafe {
        EmptyClipboard()?;
    }
    put_bytes(13, &bytes)
}

pub fn copy_files(owner: HWND, paths: &[PathBuf]) -> Result<()> {
    ensure!(!paths.is_empty(), "No files to copy");
    let mut bytes = vec![0u8; 20];
    bytes[..4].copy_from_slice(&20u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for path in paths {
        bytes.extend(
            crate::util::wide(std::path::absolute(path)?)
                .into_iter()
                .flat_map(u16::to_le_bytes),
        );
    }
    bytes.extend_from_slice(&[0, 0]);
    let effect_format = unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) };
    let _clipboard = Clipboard::open(owner)?;
    unsafe {
        EmptyClipboard()?;
    }
    put_bytes(15, &bytes)?;
    put_bytes(effect_format, &1u32.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dib_header_and_straight_bgra_bottom_up() {
        let image = Image::from_bgra(1, 2, vec![10, 20, 30, 64, 40, 50, 60, 128]);
        let dib = dibv5_bytes(&image);
        let word = |at| u32::from_le_bytes(dib[at..at + 4].try_into().unwrap());
        assert_eq!(
            (word(0), word(4), word(8), word(16), word(20)),
            (124, 1, 2, 3, 8)
        );
        assert_eq!(&dib[12..16], &[1, 0, 32, 0]);
        assert_eq!(
            (word(40), word(44), word(48), word(52)),
            (0xff0000, 0xff00, 0xff, 0xff000000)
        );
        assert_eq!(&dib[124..], &[40, 50, 60, 128, 10, 20, 30, 64]);
    }
}
