//! Offscreen rendering of views or painting closures to straight-alpha `Image`s, identical to on-screen output.

use std::rc::Rc;

use anyhow::{Result, ensure};
use glint_core::{Image, SizeF};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, ID2D1Image,
};

use crate::anim;
use crate::bitmap::read_back;
use crate::color::Color;
use crate::gfx::Gfx;
use crate::painter::Painter;
use crate::theme::Theme;
use crate::view::{Ctx, View};

/// Pixel size, scale, theme and frame time of an offscreen render.
#[derive(Clone, Debug)]
pub struct OffscreenSpec {
    pub width_px: u32,
    pub height_px: u32,
    /// Physical pixels per DIP.
    pub scale: f32,
    pub theme: Theme,
    /// Animation clock for the render (deterministic stepping).
    pub time: f64,
    /// Cleared to this first; transparent by default.
    pub background: Color,
}

impl OffscreenSpec {
    /// A target `size` DIP large at `scale`.
    pub fn new(size: SizeF, scale: f32, theme: Theme) -> Self {
        Self {
            width_px: (size.w * scale).round().max(1.0) as u32,
            height_px: (size.h * scale).round().max(1.0) as u32,
            scale,
            theme,
            time: 1.0e6,
            background: Color::TRANSPARENT,
        }
    }

    /// A target of exact pixel dimensions (e.g. full-resolution export at scale 1).
    pub fn pixels(width_px: u32, height_px: u32, scale: f32, theme: Theme) -> Self {
        Self { width_px, height_px, scale, theme, time: 1.0e6, background: Color::TRANSPARENT }
    }

    pub fn time(mut self, time: f64) -> Self {
        self.time = time;
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.background = color;
        self
    }

    pub fn size_dip(&self) -> SizeF {
        SizeF::new(self.width_px as f32 / self.scale, self.height_px as f32 / self.scale)
    }
}

/// Renders `paint` into an offscreen bitmap and reads it back. Uses its own device context, so it is safe to call
/// from event handlers and even while a window is painting.
pub fn render(gfx: &Rc<Gfx>, spec: &OffscreenSpec, paint: impl FnOnce(&mut Ctx, &mut Painter)) -> Result<Image> {
    let max = gfx.max_bitmap_size();
    ensure!(spec.width_px > 0 && spec.height_px > 0, "empty offscreen target");
    ensure!(spec.width_px <= max && spec.height_px <= max, "{}x{} exceeds the GPU limit {max}", spec.width_px, spec.height_px);
    let dpi = 96.0 * spec.scale;
    let target = gfx.create_bitmap(spec.width_px, spec.height_px, None, D2D1_BITMAP_OPTIONS_TARGET, dpi)?;
    let devices = gfx.devices();
    // SAFETY: a fresh context used only within this function.
    let context = unsafe { devices.d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };
    let mut cx = Ctx::offscreen(gfx.clone(), spec.size_dip(), spec.scale, spec.theme.clone(), spec.time);
    let result = anim::with_clock(spec.time, || {
        // SAFETY: balanced BeginDraw/EndDraw on our own context and target.
        unsafe {
            context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            context.SetTarget(&target);
            context.SetDpi(dpi, dpi);
            context.BeginDraw();
            context.Clear(Some(&spec.background.to_d2d()));
            {
                let mut painter = Painter::new(gfx, context.clone(), spec.scale, &spec.theme, spec.size_dip());
                paint(&mut cx, &mut painter);
            }
            let end = context.EndDraw(None, None);
            context.SetTarget(None::<&ID2D1Image>);
            end
        }
    });
    anim::take_motion_pending();
    result?;
    read_back(gfx, &target, spec.width_px, spec.height_px)
}

/// Renders a view exactly as its window would at `spec`'s size, scale, theme and time.
pub fn render_view(gfx: &Rc<Gfx>, view: &mut dyn View, spec: &OffscreenSpec) -> Result<Image> {
    render(gfx, spec, |cx, p| view.paint(cx, p))
}
