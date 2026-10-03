use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, ensure};
use image::ImageEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};

use crate::image::Image;
use crate::settings::ImageFormat;

/// PNG bytes with alpha (fast compression level; used for the clipboard "PNG" format).
pub fn encode_png(img: &Image) -> anyhow::Result<Vec<u8>> {
    validate_image(img)?;
    let rgba = bgra_to_rgba(img);
    let mut bytes = Vec::new();
    PngEncoder::new_with_quality(&mut bytes, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            &rgba,
            img.width,
            img.height,
            image::ExtendedColorType::Rgba8,
        )
        .context("encode PNG")?;
    Ok(bytes)
}

/// JPEG bytes; transparent pixels are flattened onto white.
pub fn encode_jpeg(img: &Image, quality: u8) -> anyhow::Result<Vec<u8>> {
    validate_image(img)?;
    let rgb = flatten_bgra_onto_white(img);
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, quality)
        .write_image(&rgb, img.width, img.height, image::ExtendedColorType::Rgb8)
        .context("encode JPEG")?;
    Ok(bytes)
}

/// Writes atomically (temp file + rename) in the given format.
pub fn save(img: &Image, path: &Path, format: ImageFormat) -> anyhow::Result<()> {
    let bytes = match format {
        ImageFormat::Png => encode_png(img)?,
        ImageFormat::Jpeg => encode_jpeg(img, 90)?,
    };
    let (temporary_path, mut temporary_file) = create_temporary_file(path)?;
    let result = (|| -> anyhow::Result<()> {
        temporary_file
            .write_all(&bytes)
            .context("write temporary image")?;
        temporary_file.sync_all().context("flush temporary image")?;
        drop(temporary_file);

        match fs::rename(&temporary_path, path) {
            Ok(()) => Ok(()),
            Err(_error) if path.exists() => {
                fs::remove_file(path).context("replace existing image")?;
                fs::rename(&temporary_path, path).context("rename temporary image")
            }
            Err(error) => Err(error).context("rename temporary image"),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

pub fn decode(bytes: &[u8]) -> anyhow::Result<Image> {
    let rgba = image::load_from_memory(bytes)
        .context("decode image")?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut bgra = rgba.into_raw();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Image::from_bgra(width, height, bgra))
}

fn validate_image(img: &Image) -> anyhow::Result<()> {
    let expected = img.width as usize * img.height as usize * 4;
    ensure!(
        img.data.len() == expected,
        "invalid BGRA image buffer length"
    );
    Ok(())
}

fn bgra_to_rgba(img: &Image) -> Vec<u8> {
    let mut rgba = img.data.clone();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    rgba
}

fn flatten_bgra_onto_white(img: &Image) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(img.width as usize * img.height as usize * 3);
    for pixel in img.data.chunks_exact(4) {
        let alpha = pixel[3] as u32;
        for channel in [pixel[2], pixel[1], pixel[0]] {
            let blended = (channel as u32 * alpha + 255 * (255 - alpha) + 127) / 255;
            rgb.push(blended as u8);
        }
    }
    rgb
}

fn create_temporary_file(path: &Path) -> anyhow::Result<(PathBuf, fs::File)> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    let file_name = path.file_name().context("image path has no file name")?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for _ in 0..128 {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = OsString::from(".");
        temporary_name.push(file_name);
        temporary_name.push(format!(".{}.{id}.tmp", std::process::id()));
        let temporary_path = parent.join(temporary_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("create temporary image"),
        }
    }
    anyhow::bail!("could not create a unique temporary image")
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn png_round_trip_preserves_bgra_and_alpha() {
        let image = Image::from_bgra(
            2,
            2,
            vec![1, 2, 3, 4, 20, 40, 60, 80, 255, 128, 0, 127, 9, 8, 7, 255],
        );
        let decoded = decode(&encode_png(&image).unwrap()).unwrap();
        assert_eq!(decoded, image);
    }

    #[test]
    fn jpeg_flattens_transparency_onto_white() {
        let image = Image::from_bgra(16, 16, [0, 0, 0, 0].repeat(16 * 16));
        let decoded = decode(&encode_jpeg(&image, 95).unwrap()).unwrap();
        assert!(decoded.data.chunks_exact(4).all(|pixel| {
            pixel[0] >= 250 && pixel[1] >= 250 && pixel[2] >= 250 && pixel[3] == 255
        }));
    }

    #[test]
    fn save_uses_the_requested_format_and_replaces_existing_file() {
        let image = Image::from_bgra(1, 1, vec![10, 20, 30, 255]);
        let path = std::env::temp_dir().join(format!(
            "glint-core-encode-{}-{}.bin",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, b"old").unwrap();
        save(&image, &path, ImageFormat::Png).unwrap();
        let decoded = decode(&fs::read(&path).unwrap()).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(decoded, image);
    }
}
