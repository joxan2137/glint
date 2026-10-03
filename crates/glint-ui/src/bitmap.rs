//! GPU bitmaps backed by a CPU `Image` (re-uploaded transparently after device loss), blurred glass backdrops
//! and readback.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Result, ensure};
use glint_core::Image;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COMPOSITE_MODE_SOURCE_OVER};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1Border, CLSID_D2D1ColorMatrix, CLSID_D2D1GaussianBlur, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BORDER_EDGE_MODE_CLAMP,
    D2D1_BORDER_PROP_EDGE_MODE_X, D2D1_BORDER_PROP_EDGE_MODE_Y, D2D1_COLORMATRIX_PROP_CLAMP_OUTPUT,
    D2D1_COLORMATRIX_PROP_COLOR_MATRIX, D2D1_GAUSSIANBLUR_OPTIMIZATION_QUALITY, D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION,
    D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_MAP_OPTIONS_READ,
    D2D1_PROPERTY_TYPE, D2D1_PROPERTY_TYPE_BOOL, D2D1_PROPERTY_TYPE_ENUM, D2D1_PROPERTY_TYPE_FLOAT,
    D2D1_PROPERTY_TYPE_MATRIX_5X4, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Effect, ID2D1Image,
};
use windows::core::Interface;

use crate::gfx::Gfx;
use crate::theme::Theme;

enum Source {
    Image(Rc<Image>),
    Blurred { image: Rc<Image>, sigma_px: f32, saturation: f32 },
}

/// An image on the GPU. Cheap to clone via `Rc`; the GPU copy is created on first use and recreated after a
/// device loss.
pub struct Bitmap {
    width: u32,
    height: u32,
    source: Source,
    gpu: RefCell<Option<(u64, ID2D1Bitmap1)>>,
    derived: RefCell<Vec<(u32, u32, Rc<Bitmap>)>>,
}

impl Bitmap {
    /// Wraps a straight-alpha BGRA image; premultiplied on upload.
    pub fn new(image: Image) -> Rc<Bitmap> {
        Self::from_shared(Rc::new(image))
    }

    pub fn from_shared(image: Rc<Image>) -> Rc<Bitmap> {
        Rc::new(Bitmap {
            width: image.width,
            height: image.height,
            source: Source::Image(image),
            gpu: RefCell::new(None),
            derived: RefCell::new(Vec::new()),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The CPU image for plain bitmaps; None for derived (blurred) bitmaps.
    pub fn image(&self) -> Option<&Image> {
        match &self.source {
            Source::Image(image) => Some(image),
            Source::Blurred { .. } => None,
        }
    }

    /// Gaussian-blurred (σ in bitmap pixels) and saturated derivative, computed once on the GPU and cached.
    /// Edges are clamped so the blur never fades to transparent at the image border.
    pub fn blurred(&self, sigma_px: f32, saturation: f32) -> Rc<Bitmap> {
        let key = (sigma_px.to_bits(), saturation.to_bits());
        if let Some((.., bitmap)) = self.derived.borrow().iter().find(|(s, t, _)| (*s, *t) == key) {
            return bitmap.clone();
        }
        let image = match &self.source {
            Source::Image(image) | Source::Blurred { image, .. } => image.clone(),
        };
        let bitmap = Rc::new(Bitmap {
            width: self.width,
            height: self.height,
            source: Source::Blurred { image, sigma_px, saturation },
            gpu: RefCell::new(None),
            derived: RefCell::new(Vec::new()),
        });
        self.derived.borrow_mut().push((key.0, key.1, bitmap.clone()));
        bitmap
    }

    /// The glass backdrop for this image per DESIGN §5 (σ 24 DIP, saturation 1.5). `px_per_dip` is how many bitmap
    /// pixels one DIP covers where the bitmap is shown (the window scale when drawn 1:1 with physical pixels).
    pub fn glass_backdrop(&self, theme: &Theme, px_per_dip: f32) -> Rc<Bitmap> {
        self.blurred(theme.backdrop_blur * px_per_dip, theme.backdrop_saturation)
    }

    /// The device bitmap, uploading or recomputing it if missing or stale.
    pub fn d2d(&self, gfx: &Gfx) -> Result<ID2D1Bitmap1> {
        if let Some((generation, bitmap)) = &*self.gpu.borrow()
            && *generation == gfx.generation()
        {
            return Ok(bitmap.clone());
        }
        let bitmap = match &self.source {
            Source::Image(image) => upload(gfx, image)?,
            Source::Blurred { image, sigma_px, saturation } => {
                let source = upload(gfx, image)?;
                blur_saturate(gfx, &source, self.width, self.height, *sigma_px, *saturation)?
            }
        };
        *self.gpu.borrow_mut() = Some((gfx.generation(), bitmap.clone()));
        Ok(bitmap)
    }

    /// Reads the GPU contents back as a straight-alpha image.
    pub fn read_back(&self, gfx: &Gfx) -> Result<Image> {
        read_back(gfx, &self.d2d(gfx)?, self.width, self.height)
    }
}

fn premultiplied(image: &Image) -> Option<Vec<u8>> {
    if image.is_opaque() {
        return None;
    }
    let mut data = image.data.clone();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        for c in &mut px[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    Some(data)
}

fn upload(gfx: &Gfx, image: &Image) -> Result<ID2D1Bitmap1> {
    ensure!(image.width > 0 && image.height > 0, "cannot upload an empty image");
    let max = gfx.max_bitmap_size();
    ensure!(image.width <= max && image.height <= max, "image {}x{} exceeds the GPU limit {max}", image.width, image.height);
    let converted = premultiplied(image);
    let data = converted.as_deref().unwrap_or(&image.data);
    gfx.create_bitmap(image.width, image.height, Some(data), D2D1_BITMAP_OPTIONS_NONE, 96.0)
}

pub(crate) fn set_property(effect: &ID2D1Effect, index: u32, kind: D2D1_PROPERTY_TYPE, bytes: &[u8]) -> Result<()> {
    // SAFETY: `bytes` matches the size the property type expects.
    unsafe { effect.SetValue(index, kind, bytes)? };
    Ok(())
}

pub(crate) fn set_float(effect: &ID2D1Effect, index: u32, value: f32) -> Result<()> {
    set_property(effect, index, D2D1_PROPERTY_TYPE_FLOAT, &value.to_le_bytes())
}

pub(crate) fn set_enum(effect: &ID2D1Effect, index: u32, value: u32) -> Result<()> {
    set_property(effect, index, D2D1_PROPERTY_TYPE_ENUM, &value.to_le_bytes())
}

fn saturation_matrix(s: f32) -> [f32; 20] {
    let (lr, lg, lb) = (0.2126 * (1.0 - s), 0.7152 * (1.0 - s), 0.0722 * (1.0 - s));
    [
        lr + s, lr, lr, 0.0, //
        lg, lg + s, lg, 0.0, //
        lb, lb, lb + s, 0.0, //
        0.0, 0.0, 0.0, 1.0, //
        0.0, 0.0, 0.0, 0.0,
    ]
}

/// Renders `draw` into a new drawable bitmap using the auxiliary device context.
pub(crate) fn render_to_bitmap(
    gfx: &Gfx,
    width: u32,
    height: u32,
    draw: impl FnOnce(&ID2D1DeviceContext) -> Result<()>,
) -> Result<ID2D1Bitmap1> {
    let target = gfx.create_bitmap(width, height, None, D2D1_BITMAP_OPTIONS_TARGET, 96.0)?;
    let context = gfx.devices().aux_context;
    // SAFETY: the aux context is used only here, between matching BeginDraw/EndDraw calls.
    unsafe {
        context.SetTarget(&target);
        context.SetDpi(96.0, 96.0);
        context.BeginDraw();
        context.Clear(None);
        let drawn = draw(&context);
        let ended = context.EndDraw(None, None);
        context.SetTarget(None::<&ID2D1Image>);
        drawn?;
        ended?;
    }
    Ok(target)
}

fn blur_saturate(gfx: &Gfx, source: &ID2D1Bitmap1, width: u32, height: u32, sigma: f32, saturation: f32) -> Result<ID2D1Bitmap1> {
    render_to_bitmap(gfx, width, height, |context| {
        // SAFETY: effects are created on and drawn by the same device context.
        unsafe {
            let border = context.CreateEffect(&CLSID_D2D1Border)?;
            border.SetInput(0, &source.cast::<ID2D1Image>()?, true);
            set_enum(&border, D2D1_BORDER_PROP_EDGE_MODE_X.0 as u32, D2D1_BORDER_EDGE_MODE_CLAMP.0 as u32)?;
            set_enum(&border, D2D1_BORDER_PROP_EDGE_MODE_Y.0 as u32, D2D1_BORDER_EDGE_MODE_CLAMP.0 as u32)?;
            let blur = context.CreateEffect(&CLSID_D2D1GaussianBlur)?;
            blur.SetInput(0, &border.GetOutput()?, true);
            set_float(&blur, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, sigma.max(0.0))?;
            set_enum(&blur, D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION.0 as u32, D2D1_GAUSSIANBLUR_OPTIMIZATION_QUALITY.0 as u32)?;
            let matrix = context.CreateEffect(&CLSID_D2D1ColorMatrix)?;
            matrix.SetInput(0, &blur.GetOutput()?, true);
            let values = saturation_matrix(saturation);
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            set_property(&matrix, D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32, D2D1_PROPERTY_TYPE_MATRIX_5X4, &bytes)?;
            set_property(&matrix, D2D1_COLORMATRIX_PROP_CLAMP_OUTPUT.0 as u32, D2D1_PROPERTY_TYPE_BOOL, &1u32.to_le_bytes())?;
            let bounds = D2D_RECT_F { left: 0.0, top: 0.0, right: width as f32, bottom: height as f32 };
            context.DrawImage(
                &matrix.GetOutput()?,
                None,
                Some(&bounds),
                windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE(D2D1_INTERPOLATION_MODE_LINEAR.0),
                D2D1_COMPOSITE_MODE_SOURCE_OVER,
            );
        }
        Ok(())
    })
}

/// Copies a bitmap to CPU memory and un-premultiplies it.
pub(crate) fn read_back(gfx: &Gfx, bitmap: &ID2D1Bitmap1, width: u32, height: u32) -> Result<Image> {
    let staging = gfx.create_bitmap(width, height, None, D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW, 96.0)?;
    let mut image = Image::new(width, height);
    // SAFETY: staging and source have identical sizes; the mapped pointer is valid for height * pitch bytes until
    // Unmap.
    unsafe {
        staging.CopyFromBitmap(None, bitmap, None)?;
        let mapped = staging.Map(D2D1_MAP_OPTIONS_READ)?;
        let row_bytes = width as usize * 4;
        for y in 0..height as usize {
            let src = std::slice::from_raw_parts(mapped.bits.add(y * mapped.pitch as usize), row_bytes);
            image.data[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(src);
        }
        staging.Unmap()?;
    }
    unpremultiply(&mut image.data);
    Ok(image)
}

pub(crate) fn unpremultiply(data: &mut [u8]) {
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 {
            px[..3].fill(0);
        } else if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_round_trip_is_close() {
        let mut image = Image::new(2, 1);
        image.data.copy_from_slice(&[200, 100, 50, 128, 10, 20, 30, 255]);
        let mut pre = premultiplied(&image).unwrap();
        assert_eq!(&pre[4..], &[10, 20, 30, 255]);
        unpremultiply(&mut pre);
        for (a, b) in pre.iter().zip(&image.data) {
            assert!((*a as i32 - *b as i32).abs() <= 2);
        }
    }

    #[test]
    fn saturation_identity_and_grey() {
        let identity = saturation_matrix(1.0);
        assert_eq!(identity[0], 1.0);
        assert_eq!(identity[1], 0.0);
        let grey = saturation_matrix(0.0);
        let r = grey[0] + grey[4] + grey[8];
        assert!((r - 1.0).abs() < 1e-6, "rows of a greyscale matrix sum to 1 per output channel");
    }
}
