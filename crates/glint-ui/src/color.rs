use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

/// sRGB color with straight alpha, all channels in 0..=1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Color = Color::rgba(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Color = Color::rgba(1.0, 1.0, 1.0, 1.0);

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self::rgba8(r, g, b, 1.0)
    }

    pub const fn rgba8(r: u8, g: u8, b: u8, a: f32) -> Self {
        Self { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
    }

    /// `#RRGGBB` or `#RRGGBBAA`, leading `#` optional.
    pub fn hex(text: &str) -> Option<Self> {
        let digits = text.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok();
        match digits.len() {
            6 => Some(Self::rgb8(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Self::rgba8(byte(0)?, byte(2)?, byte(4)?, byte(6)? as f32 / 255.0)),
            _ => None,
        }
    }

    /// `#RRGGBB` (alpha dropped).
    pub fn to_hex(&self) -> String {
        let [r, g, b, _] = self.to_rgba8();
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    pub fn from_bgra8(bgra: [u8; 4]) -> Self {
        Self::rgba8(bgra[2], bgra[1], bgra[0], bgra[3] as f32 / 255.0)
    }

    pub fn to_rgba8(&self) -> [u8; 4] {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [q(self.r), q(self.g), q(self.b), q(self.a)]
    }

    pub fn with_alpha(&self, a: f32) -> Self {
        Self { a, ..*self }
    }

    pub fn multiply_alpha(&self, factor: f32) -> Self {
        Self { a: self.a * factor, ..*self }
    }

    /// Interpolates in premultiplied space so fading to transparent never darkens or tints.
    pub fn lerp(&self, other: &Color, t: f32) -> Color {
        let a = self.a + (other.a - self.a) * t;
        if a <= f32::EPSILON {
            return Color::rgba(other.r, other.g, other.b, 0.0);
        }
        let mix = |x: f32, y: f32| (x * self.a + (y * other.a - x * self.a) * t) / a;
        Color::rgba(mix(self.r, other.r), mix(self.g, other.g), mix(self.b, other.b), a)
    }

    /// Source-over composite of `self` on top of `below`.
    pub fn over(&self, below: &Color) -> Color {
        let a = self.a + below.a * (1.0 - self.a);
        if a <= f32::EPSILON {
            return Color::TRANSPARENT;
        }
        let mix = |top: f32, bottom: f32| (top * self.a + bottom * below.a * (1.0 - self.a)) / a;
        Color::rgba(mix(self.r, below.r), mix(self.g, below.g), mix(self.b, below.b), a)
    }

    /// Relative luminance (sRGB coefficients on linearized channels).
    pub fn luminance(&self) -> f32 {
        let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    pub fn to_d2d(&self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Color, b: Color) -> bool {
        [a.r - b.r, a.g - b.g, a.b - b.b, a.a - b.a].iter().all(|d| d.abs() < 1e-5)
    }

    #[test]
    fn hex_round_trip() {
        let c = Color::hex("#0A84FF").unwrap();
        assert_eq!(c.to_rgba8(), [10, 132, 255, 255]);
        assert_eq!(c.to_hex(), "#0A84FF");
        assert_eq!(Color::hex("ff3b3080").unwrap().to_rgba8(), [255, 59, 48, 128]);
        assert!(Color::hex("#12345").is_none());
        assert!(Color::hex("#GG0000").is_none());
    }

    #[test]
    fn lerp_endpoints_and_midpoint() {
        let a = Color::rgba(1.0, 0.0, 0.0, 1.0);
        let b = Color::rgba(0.0, 0.0, 1.0, 1.0);
        assert!(close(a.lerp(&b, 0.0), a));
        assert!(close(a.lerp(&b, 1.0), b));
        assert!(close(a.lerp(&b, 0.5), Color::rgba(0.5, 0.0, 0.5, 1.0)));
    }

    #[test]
    fn lerp_to_transparent_keeps_hue() {
        let white = Color::WHITE;
        let clear_black = Color::rgba(0.0, 0.0, 0.0, 0.0);
        let mid = white.lerp(&clear_black, 0.5);
        assert!(close(mid, Color::rgba(1.0, 1.0, 1.0, 0.5)));
    }

    #[test]
    fn over_composites() {
        let half_white = Color::rgba(1.0, 1.0, 1.0, 0.5);
        let out = half_white.over(&Color::BLACK);
        assert!(close(out, Color::rgba(0.5, 0.5, 0.5, 1.0)));
        assert!(close(Color::TRANSPARENT.over(&Color::BLACK), Color::BLACK));
    }

    #[test]
    fn luminance_extremes() {
        assert!(Color::BLACK.luminance().abs() < 1e-6);
        assert!((Color::WHITE.luminance() - 1.0).abs() < 1e-4);
    }
}
