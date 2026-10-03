//! Offscreen rendering checks (GPU or WARP; no windows).

use std::rc::Rc;

use glint_ui::widgets::{Badge, Toolbar, ToolbarItem, IconButton};
use glint_ui::{
    Bitmap, Color, Gfx, Icon, Image, OffscreenSpec, PointF, RectF, SizeF, TextStyle, Theme, render_offscreen,
};

fn gfx() -> Rc<Gfx> {
    Gfx::new().expect("graphics devices")
}

fn alpha(image: &Image, x: u32, y: u32) -> u8 {
    image.pixel(x, y)[3]
}

#[test]
fn readback_is_straight_alpha_bgra() {
    let gfx = gfx();
    let spec = OffscreenSpec::new(SizeF::new(4.0, 4.0), 1.0, Theme::dark());
    let image = render_offscreen(&gfx, &spec, |_, p| p.fill_rect(p.bounds(), Color::rgba(1.0, 0.0, 0.0, 0.5))).unwrap();
    let [b, g, r, a] = image.pixel(1, 1);
    assert_eq!((b, g), (0, 0));
    assert!(r >= 254, "red stays full intensity after un-premultiplying: {r}");
    assert!((127..=128).contains(&a));
}

#[test]
fn hairline_is_exactly_one_physical_pixel_at_every_scale() {
    let gfx = gfx();
    for scale in [1.0f32, 1.5, 2.0] {
        let spec = OffscreenSpec::new(SizeF::new(40.0, 40.0), scale, Theme::dark());
        let image = render_offscreen(&gfx, &spec, |_, p| {
            let r = p.snap_rect(RectF::new(10.0, 10.0, 20.0, 20.0));
            p.hairline_round_rect(r, 0.0, Color::WHITE, true);
        })
        .unwrap();
        let x = (20.0 * scale) as u32;
        let column: Vec<u8> = (0..image.height).map(|y| alpha(&image, x, y)).collect();
        let lit: Vec<usize> = column.iter().enumerate().filter(|(_, a)| **a > 0).map(|(y, _)| y).collect();
        assert_eq!(lit.len(), 2, "scale {scale}: one top and one bottom pixel, got rows {lit:?}");
        assert!(lit.iter().all(|&y| column[y] == 255), "scale {scale}: hairline pixels are fully covered");
    }
}

#[test]
fn every_icon_draws_inside_its_box() {
    let gfx = gfx();
    let spec = OffscreenSpec::new(SizeF::new(24.0, 24.0), 2.0, Theme::dark());
    for icon in Icon::ALL {
        let image = render_offscreen(&gfx, &spec, |_, p| p.icon(*icon, PointF::new(12.0, 12.0), 18.0, Color::WHITE)).unwrap();
        let covered = image.data.chunks_exact(4).filter(|px| px[3] > 0).count();
        assert!(covered > 20, "{icon:?} drew {covered} pixels");
        let border_lit = (0..48).any(|i| alpha(&image, i, 0) > 0 || alpha(&image, 0, i) > 0);
        assert!(!border_lit, "{icon:?} bleeds outside the 18 DIP box");
    }
}

#[test]
fn tabular_figures_have_equal_advances() {
    let gfx = gfx();
    let style = TextStyle::body().tabular();
    let ones = gfx.measure_text("1111", &style).w;
    let eights = gfx.measure_text("8888", &style).w;
    assert!(ones > 10.0);
    assert!((ones - eights).abs() < 0.01, "{ones} vs {eights}");
    let proportional = TextStyle::body();
    assert!(gfx.measure_text("1111", &proportional).w < gfx.measure_text("8888", &proportional).w);
}

#[test]
fn text_and_widgets_render_without_errors() {
    let gfx = gfx();
    for theme in [Theme::dark(), Theme::light()] {
        let spec = OffscreenSpec::new(SizeF::new(300.0, 80.0), 1.5, theme);
        let image = render_offscreen(&gfx, &spec, |_, p| {
            let mut toolbar = Toolbar::new(vec![
                ToolbarItem::button("a", IconButton::new(Icon::Pen)),
                ToolbarItem::separator(),
                ToolbarItem::button("b", IconButton::new(Icon::Crop).label("Crop")),
            ]);
            toolbar.layout_at(p.gfx(), PointF::new(8.0, 8.0));
            toolbar.paint(p, None);
            Badge::new("1280 × 720").paint_centered(p, PointF::new(240.0, 30.0));
        })
        .unwrap();
        assert!(image.data.chunks_exact(4).filter(|px| px[3] > 128).count() > 5000);
    }
}

#[test]
fn glass_backdrop_blur_keeps_edges_opaque_and_smooths_steps() {
    let gfx = gfx();
    let mut image = Image::new(64, 32);
    for y in 0..32 {
        for x in 0..64 {
            let v = if x < 32 { 0 } else { 255 };
            let i = ((y * 64 + x) * 4) as usize;
            image.data[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let bitmap = Bitmap::new(image);
    let blurred = bitmap.blurred(4.0, 1.0).read_back(&gfx).unwrap();
    assert_eq!(blurred.pixel(0, 0)[3], 255, "clamped edges stay opaque");
    assert!(blurred.pixel(0, 16)[0] < 8 && blurred.pixel(63, 16)[0] > 247);
    let middle = blurred.pixel(32, 16)[0];
    assert!((96..=160).contains(&middle), "step is blurred: {middle}");
}

#[test]
fn large_shadows_have_no_nine_slice_seams() {
    let gfx = gfx();
    let spec = OffscreenSpec::new(SizeF::new(400.0, 200.0), 2.0, Theme::dark());
    let shadow = glint_ui::Shadow::new(0.0, 40.0, Color::rgba(0.0, 0.0, 0.0, 0.8));
    let image = render_offscreen(&gfx, &spec, |_, p| p.shadow(RectF::new(80.0, 60.0, 240.0, 80.0), 14.0, &shadow)).unwrap();
    let y = 50 * 2;
    let row: Vec<i32> = (0..image.width).map(|x| alpha(&image, x, y) as i32).collect();
    let center = row[(200 * 2) as usize];
    assert!(center > 40, "shadow visible above the shape: {center}");
    for x in (160 * 2)..(240 * 2) {
        assert!((row[x as usize] - center).abs() <= 1, "edge band is uniform along the straight side at x {x}");
    }
    for pair in row.windows(2) {
        assert!((pair[1] - pair[0]).abs() <= 3, "no seam between slices: {pair:?}");
    }
}
