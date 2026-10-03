use anyhow::{Context, Result, ensure};
use glint_core::{Image, RectF};
use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::HSTRING,
};

#[derive(Clone, Debug)]
pub struct OcrWord {
    pub text: String,
    pub rect: RectF,
}
#[derive(Clone, Debug)]
pub struct OcrLine {
    pub text: String,
    pub words: Vec<OcrWord>,
}
#[derive(Clone, Debug)]
pub struct OcrResult {
    pub text: String,
    pub lines: Vec<OcrLine>,
    pub language: String,
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

fn engine() -> windows::core::Result<OcrEngine> {
    OcrEngine::TryCreateFromUserProfileLanguages().or_else(|_| {
        OcrEngine::TryCreateFromLanguage(&Language::CreateLanguage(&HSTRING::from("en-US"))?)
    })
}

pub fn ocr_available() -> bool {
    match Apartment::new() {
        Ok(_apartment) => engine().is_ok(),
        Err(error)
            if error
                .downcast_ref::<windows::core::Error>()
                .is_some_and(|error| {
                    error.code() == windows::Win32::Foundation::RPC_E_CHANGED_MODE
                }) =>
        {
            engine().is_ok()
        }
        Err(_) => false,
    }
}

fn dimensions(width: u32, height: u32, limit: u32) -> (u32, u32) {
    let desired: f64 = if height < 60 { 2.0 } else { 1.0 };
    let scale = desired.min(limit as f64 / width.max(height) as f64);
    (
        ((width as f64 * scale).round() as u32).clamp(1, limit),
        ((height as f64 * scale).round() as u32).clamp(1, limit),
    )
}

fn scaled_premultiplied(img: &Image, width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let source_x = ((x as f64 + 0.5) * img.width as f64 / width as f64 - 0.5)
                .clamp(0.0, (img.width - 1) as f64);
            let source_y = ((y as f64 + 0.5) * img.height as f64 / height as f64 - 0.5)
                .clamp(0.0, (img.height - 1) as f64);
            let x0 = source_x.floor() as u32;
            let y0 = source_y.floor() as u32;
            let fx = source_x.fract();
            let fy = source_y.fract();
            let pixels = [
                img.pixel(x0, y0),
                img.pixel((x0 + 1).min(img.width - 1), y0),
                img.pixel(x0, (y0 + 1).min(img.height - 1)),
                img.pixel((x0 + 1).min(img.width - 1), (y0 + 1).min(img.height - 1)),
            ];
            let weights = [
                (1.0 - fx) * (1.0 - fy),
                fx * (1.0 - fy),
                (1.0 - fx) * fy,
                fx * fy,
            ];
            let offset = (y as usize * width as usize + x as usize) * 4;
            for channel in 0..4 {
                let value: f64 = pixels
                    .iter()
                    .zip(weights)
                    .map(|(p, w)| {
                        let alpha = if channel == 3 {
                            1.0
                        } else {
                            p[3] as f64 / 255.0
                        };
                        p[channel] as f64 * alpha * w
                    })
                    .sum();
                bytes[offset + channel] = value.round() as u8;
            }
        }
    }
    bytes
}

/// Blocking WinRT OCR. Call on a worker thread; this initializes an MTA apartment.
pub fn ocr(img: &Image) -> Result<OcrResult> {
    ensure!(img.width > 0 && img.height > 0, "OCR image is empty");
    ensure!(
        img.data.len() == img.width as usize * img.height as usize * 4,
        "Invalid BGRA image length"
    );
    let _apartment = Apartment::new().context("OCR must run on a worker thread")?;
    let engine = engine().context("No OCR language is installed (including en-US)")?;
    let (width, height) = dimensions(img.width, img.height, OcrEngine::MaxImageDimension()?);
    let writer = DataWriter::new()?;
    writer.WriteBytes(&scaled_premultiplied(img, width, height))?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &writer.DetachBuffer()?,
        BitmapPixelFormat::Bgra8,
        width as i32,
        height as i32,
        BitmapAlphaMode::Premultiplied,
    )?;
    let recognized = engine.RecognizeAsync(&bitmap)?.join()?;
    let sx = img.width as f32 / width as f32;
    let sy = img.height as f32 / height as f32;
    let mut lines = Vec::new();
    for line in recognized.Lines()? {
        let mut words = Vec::new();
        for word in line.Words()? {
            let rect = word.BoundingRect()?;
            words.push(OcrWord {
                text: word.Text()?.to_string(),
                rect: RectF::new(rect.X * sx, rect.Y * sy, rect.Width * sx, rect.Height * sy),
            });
        }
        lines.push(OcrLine {
            text: line.Text()?.to_string(),
            words,
        });
    }
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(OcrResult {
        text,
        lines,
        language: engine.RecognizerLanguage()?.LanguageTag()?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scales_small_and_large_images() {
        assert_eq!(dimensions(200, 40, 2600), (400, 80));
        assert_eq!(dimensions(5200, 2600, 2600), (2600, 1300));
        assert_eq!(dimensions(5200, 40, 2600), (2600, 20));
        assert_eq!(
            scaled_premultiplied(&Image::from_bgra(1, 1, vec![100, 200, 50, 128]), 1, 1),
            [50, 100, 25, 128]
        );
    }
}
