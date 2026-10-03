use half::f16;

use crate::geom::RectI;

/// 8-bit image. Pixels are BGRA, straight (non-premultiplied) alpha, stride = width * 4.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize * 4] }
    }

    pub fn from_bgra(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_eq!(data.len(), width as usize * height as usize * 4);
        Self { width, height, data }
    }

    pub fn bounds(&self) -> RectI {
        RectI::new(0, 0, self.width as i32, self.height as i32)
    }

    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }

    /// BGRA of one pixel.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    pub fn is_opaque(&self) -> bool {
        self.data.chunks_exact(4).all(|p| p[3] == 255)
    }

    /// Copies `rect` (clamped to the image bounds) into a new image.
    pub fn crop(&self, rect: RectI) -> Image {
        let Some(r) = rect.intersect(&self.bounds()) else {
            return Image::new(0, 0);
        };
        let mut out = Image::new(r.w as u32, r.h as u32);
        let row_bytes = r.w as usize * 4;
        for row in 0..r.h as usize {
            let src = ((r.y as usize + row) * self.width as usize + r.x as usize) * 4;
            out.data[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(&self.data[src..src + row_bytes]);
        }
        out
    }
}

/// Raw HDR capture: RGBA half floats, linear scRGB (BT.709 primaries, 1.0 = 80 nits).
/// Values may exceed 1.0 (brighter than 80 nits) and may be negative (outside BT.709 gamut).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HdrImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f16>,
    /// Windows "SDR content brightness" of the source monitor, in nits. SDR white sits at sdr_white_nits / 80 in scRGB.
    pub sdr_white_nits: f32,
    /// Peak luminance the source monitor reports, in nits.
    pub display_peak_nits: f32,
}

impl HdrImage {
    pub fn bounds(&self) -> RectI {
        RectI::new(0, 0, self.width as i32, self.height as i32)
    }

    /// RGBA of one pixel as f32.
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i].to_f32(), self.data[i + 1].to_f32(), self.data[i + 2].to_f32(), self.data[i + 3].to_f32()]
    }

    pub fn crop(&self, rect: RectI) -> HdrImage {
        let mut out = HdrImage {
            width: 0,
            height: 0,
            data: Vec::new(),
            sdr_white_nits: self.sdr_white_nits,
            display_peak_nits: self.display_peak_nits,
        };
        let Some(r) = rect.intersect(&self.bounds()) else {
            return out;
        };
        out.width = r.w as u32;
        out.height = r.h as u32;
        out.data.reserve(r.w as usize * r.h as usize * 4);
        for row in 0..r.h as usize {
            let src = ((r.y as usize + row) * self.width as usize + r.x as usize) * 4;
            out.data.extend_from_slice(&self.data[src..src + r.w as usize * 4]);
        }
        out
    }
}
