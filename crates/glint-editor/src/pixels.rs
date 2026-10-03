//! CPU pixel work: block pixelation of the whole base image for redaction (blocks stay put while a redaction moves),
//! the redaction strengths, and box downscaling for images beyond the GPU's bitmap size limit.

use glint_core::{Image, RectF};

/// Pixelation block side: 1/40 of the shorter image side, at least 8 px.
pub fn block_size(width: u32, height: u32) -> u32 {
    (width.min(height) / 40).max(8)
}

/// Gaussian σ for blur redaction: strong enough that body text is unreadable at any capture size.
pub fn blur_sigma(width: u32, height: u32) -> f32 {
    (width.min(height) as f32 / 70.0).max(8.0)
}

/// Visits every `block`×`block` cell (clipped at the edges) with its column, row, pixel bounds and average
/// (alpha-weighted color, mean alpha).
fn for_each_block(image: &Image, block: u32, mut visit: impl FnMut(usize, usize, (usize, usize, usize, usize), [u8; 4])) {
    let block = block.max(1) as usize;
    let (w, h) = (image.width as usize, image.height as usize);
    if image.data.len() < w * h * 4 {
        return;
    }
    for (row, by) in (0..h).step_by(block).enumerate() {
        for (col, bx) in (0..w).step_by(block).enumerate() {
            let (x1, y1) = ((bx + block).min(w), (by + block).min(h));
            let mut sum = [0u64; 4];
            for y in by..y1 {
                for px in image.data[(y * w + bx) * 4..(y * w + x1) * 4].chunks_exact(4) {
                    let a = px[3] as u64;
                    sum[0] += px[0] as u64 * a;
                    sum[1] += px[1] as u64 * a;
                    sum[2] += px[2] as u64 * a;
                    sum[3] += a;
                }
            }
            let count = ((x1 - bx) * (y1 - by)) as u64;
            let alpha = sum[3];
            let channel = |s: u64| (s + alpha / 2).checked_div(alpha).unwrap_or(0) as u8;
            let mean_alpha = ((alpha + count / 2) / count.max(1)) as u8;
            visit(col, row, (bx, by, x1, y1), [channel(sum[0]), channel(sum[1]), channel(sum[2]), mean_alpha]);
        }
    }
}

/// Every `block`×`block` cell replaced by its average (alpha-weighted color, mean alpha).
pub fn pixelate(image: &Image, block: u32) -> Image {
    let w = image.width as usize;
    let mut out = Image::new(image.width, image.height);
    for_each_block(image, block, |_, _, (bx, by, x1, y1), average| {
        for y in by..y1 {
            for px in out.data[(y * w + bx) * 4..(y * w + x1) * 4].chunks_exact_mut(4) {
                px.copy_from_slice(&average);
            }
        }
    });
    out
}

/// One pixel per `factor`×`factor` cell (box filter); edge cells are partial.
pub fn downscale(image: &Image, factor: u32) -> Image {
    let factor = factor.max(1);
    let (w, h) = (image.width.div_ceil(factor), image.height.div_ceil(factor));
    let mut out = Image::new(w, h);
    for_each_block(image, factor, |col, row, _, average| {
        let i = (row * w as usize + col) * 4;
        out.data[i..i + 4].copy_from_slice(&average);
    });
    out
}

/// The whole-number reduction that brings both sides within `max_side`.
pub fn reduction(width: u32, height: u32, max_side: u32) -> u32 {
    width.max(height).div_ceil(max_side.max(1)).max(1)
}

/// `rect` grown outward to whole pixelation blocks, so redactions never show half blocks at their edges.
pub fn block_aligned(rect: RectF, block: u32, width: u32, height: u32) -> RectF {
    let b = block.max(1) as f32;
    let l = (rect.x / b).floor() * b;
    let t = (rect.y / b).floor() * b;
    let r = ((rect.right() / b).ceil() * b).min(width as f32);
    let bottom = ((rect.bottom() / b).ceil() * b).min(height as f32);
    RectF::from_ltrb(l.max(0.0), t.max(0.0), r, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_size_follows_the_short_side_with_a_floor() {
        assert_eq!(block_size(1920, 1080), 27);
        assert_eq!(block_size(200, 100), 8);
        assert!(blur_sigma(3840, 2160) > blur_sigma(1280, 720));
        assert_eq!(blur_sigma(300, 200), 8.0);
    }

    #[test]
    fn pixelate_averages_each_block() {
        let mut image = Image::new(4, 2);
        for x in 0..4u32 {
            for y in 0..2u32 {
                let i = ((y * 4 + x) * 4) as usize;
                let v = if x < 2 { if (x + y) % 2 == 0 { 0 } else { 200 } } else { 50 };
                image.data[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let out = pixelate(&image, 2);
        assert_eq!(out.pixel(0, 0), [100, 100, 100, 255]);
        assert_eq!(out.pixel(1, 1), [100, 100, 100, 255]);
        assert_eq!(out.pixel(3, 0), [50, 50, 50, 255]);
    }

    #[test]
    fn partial_edge_blocks_and_transparency() {
        let mut image = Image::new(3, 1);
        image.data.copy_from_slice(&[10, 20, 30, 255, 0, 0, 0, 0, 90, 90, 90, 255]);
        let out = pixelate(&image, 2);
        assert_eq!(out.pixel(0, 0), [10, 20, 30, 128], "transparent pixels do not darken the color");
        assert_eq!(out.pixel(2, 0), [90, 90, 90, 255]);
    }

    #[test]
    fn downscale_averages_cells_and_rounds_sizes_up() {
        let mut image = Image::new(5, 3);
        for px in image.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[100, 50, 200, 255]);
        }
        image.data[0..4].copy_from_slice(&[0, 0, 0, 255]);
        let small = downscale(&image, 2);
        assert_eq!((small.width, small.height), (3, 2));
        assert_eq!(small.pixel(0, 0), [75, 38, 150, 255]);
        assert_eq!(small.pixel(2, 1), [100, 50, 200, 255]);
        assert_eq!(reduction(40_000, 1_000, 16_384), 3);
        assert_eq!(reduction(1920, 1080, 16_384), 1);
        assert_eq!(reduction(0, 0, 16_384), 1);
    }

    #[test]
    fn redactions_align_to_blocks() {
        let r = block_aligned(RectF::new(13.0, 7.0, 20.0, 10.0), 8, 100, 20);
        assert_eq!(r, RectF::new(8.0, 0.0, 32.0, 20.0));
    }
}
