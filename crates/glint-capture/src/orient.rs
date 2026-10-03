use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
};

/// How the display scans the desktop out, as reported by `DXGI_OUTDUPL_DESC::Rotation`.
/// The duplicated texture is in scan-out orientation; `orient` turns it back into desktop orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    Identity,
    Rotate90,
    Rotate180,
    Rotate270,
}

impl From<DXGI_MODE_ROTATION> for Rotation {
    fn from(value: DXGI_MODE_ROTATION) -> Self {
        match value {
            DXGI_MODE_ROTATION_ROTATE90 => Rotation::Rotate90,
            DXGI_MODE_ROTATION_ROTATE180 => Rotation::Rotate180,
            DXGI_MODE_ROTATION_ROTATE270 => Rotation::Rotate270,
            _ => Rotation::Identity,
        }
    }
}

impl Rotation {
    pub fn desktop_size(self, texture_width: usize, texture_height: usize) -> (usize, usize) {
        match self {
            Rotation::Rotate90 | Rotation::Rotate270 => (texture_height, texture_width),
            Rotation::Identity | Rotation::Rotate180 => (texture_width, texture_height),
        }
    }
}

/// Reads a `texture_width` x `texture_height` texture row by row and returns the desktop-oriented pixels
/// with their size. `Rotate90` means the desktop is the texture rotated 90 degrees clockwise.
pub fn orient<'a, P: Copy + Default + 'a>(
    row: impl Fn(usize) -> &'a [P],
    texture_width: usize,
    texture_height: usize,
    rotation: Rotation,
) -> (Vec<P>, usize, usize) {
    let (width, height) = rotation.desktop_size(texture_width, texture_height);
    if rotation == Rotation::Identity {
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..texture_height {
            pixels.extend_from_slice(&row(y)[..texture_width]);
        }
        return (pixels, width, height);
    }
    let destination = |x: usize, y: usize| match rotation {
        Rotation::Rotate90 => (texture_height - 1 - y, x),
        Rotation::Rotate180 => (texture_width - 1 - x, texture_height - 1 - y),
        _ => (y, texture_width - 1 - x),
    };
    let mut pixels = vec![P::default(); width * height];
    for y in 0..texture_height {
        for (x, &pixel) in row(y)[..texture_width].iter().enumerate() {
            let (dx, dy) = destination(x, y);
            pixels[dy * width + dx] = pixel;
        }
    }
    (pixels, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXTURE: [u8; 6] = *b"abcdef";

    fn oriented(rotation: Rotation) -> (String, usize, usize) {
        let (pixels, w, h) = orient(|y| &TEXTURE[y * 3..y * 3 + 3], 3, 2, rotation);
        (String::from_utf8(pixels).unwrap(), w, h)
    }

    #[test]
    fn identity_keeps_rows() {
        assert_eq!(oriented(Rotation::Identity), ("abcdef".into(), 3, 2));
    }

    #[test]
    fn rotate90_is_clockwise() {
        assert_eq!(oriented(Rotation::Rotate90), ("daebfc".into(), 2, 3));
    }

    #[test]
    fn rotate180_reverses() {
        assert_eq!(oriented(Rotation::Rotate180), ("fedcba".into(), 3, 2));
    }

    #[test]
    fn rotate270_is_counter_clockwise() {
        assert_eq!(oriented(Rotation::Rotate270), ("cfbead".into(), 2, 3));
    }

    #[test]
    fn dxgi_values_map() {
        assert_eq!(Rotation::from(DXGI_MODE_ROTATION_ROTATE90), Rotation::Rotate90);
        assert_eq!(Rotation::from(DXGI_MODE_ROTATION(0)), Rotation::Identity);
    }
}
