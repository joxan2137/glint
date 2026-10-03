//! Minimal SVG geometry parser for line icons: path data (M L H V C S Q T A Z, absolute and relative) and the
//! basic shapes Lucide uses. Everything is normalized to absolute move/line/cubic/close segments.

use std::f32::consts::PI;

use glint_core::PointF;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    MoveTo(PointF),
    LineTo(PointF),
    CubicTo(PointF, PointF, PointF),
    Close,
}

/// One SVG element converted to segments.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub segments: Vec<Segment>,
    /// The element is filled (`fill="currentColor"`) rather than only stroked.
    pub filled: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SVG path error at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { bytes: text.as_bytes(), pos: 0 }
    }

    fn skip_separators(&mut self) {
        while let Some(&b) = self.bytes.get(self.pos) {
            if b.is_ascii_whitespace() || b == b',' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn at_end(&mut self) -> bool {
        self.skip_separators();
        self.pos >= self.bytes.len()
    }

    fn peek_is_number(&mut self) -> bool {
        self.skip_separators();
        matches!(self.bytes.get(self.pos), Some(b) if b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.'))
    }

    fn error(&self, message: &'static str) -> ParseError {
        ParseError { offset: self.pos, message }
    }

    fn number(&mut self) -> Result<f32, ParseError> {
        self.skip_separators();
        let start = self.pos;
        let b = self.bytes;
        if matches!(b.get(self.pos), Some(b'-' | b'+')) {
            self.pos += 1;
        }
        let mut digits = 0;
        while matches!(b.get(self.pos), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
            digits += 1;
        }
        if b.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            while matches!(b.get(self.pos), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            self.pos = start;
            return Err(self.error("expected a number"));
        }
        if matches!(b.get(self.pos), Some(b'e' | b'E')) {
            let mark = self.pos;
            self.pos += 1;
            if matches!(b.get(self.pos), Some(b'-' | b'+')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while matches!(b.get(self.pos), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                self.pos = mark;
            }
        }
        std::str::from_utf8(&b[start..self.pos])
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .ok_or_else(|| self.error("malformed number"))
    }

    fn flag(&mut self) -> Result<bool, ParseError> {
        self.skip_separators();
        match self.bytes.get(self.pos) {
            Some(b'0') => {
                self.pos += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.pos += 1;
                Ok(true)
            }
            _ => Err(self.error("expected an arc flag")),
        }
    }

    fn point(&mut self) -> Result<PointF, ParseError> {
        Ok(PointF::new(self.number()?, self.number()?))
    }
}

fn add(a: PointF, b: PointF) -> PointF {
    PointF::new(a.x + b.x, a.y + b.y)
}

fn reflect(control: PointF, about: PointF) -> PointF {
    PointF::new(2.0 * about.x - control.x, 2.0 * about.y - control.y)
}

/// Parses SVG path data into absolute segments.
pub fn parse_path(data: &str) -> Result<Vec<Segment>, ParseError> {
    let mut c = Cursor::new(data);
    let mut out = Vec::new();
    let mut current = PointF::default();
    let mut subpath_start = PointF::default();
    let mut last_cubic_control: Option<PointF> = None;
    let mut last_quad_control: Option<PointF> = None;
    let mut command = 0u8;

    while !c.at_end() {
        let byte = c.bytes[c.pos];
        if byte.is_ascii_alphabetic() {
            command = byte;
            c.pos += 1;
        } else if command == 0 {
            return Err(c.error("path data must start with a command"));
        } else if !c.peek_is_number() {
            return Err(c.error("unexpected character"));
        }
        let relative = command.is_ascii_lowercase();
        let base = if relative { current } else { PointF::default() };
        let mut cubic_control = None;
        let mut quad_control = None;
        match command.to_ascii_uppercase() {
            b'M' => {
                current = add(base, c.point()?);
                subpath_start = current;
                out.push(Segment::MoveTo(current));
                command = if relative { b'l' } else { b'L' };
            }
            b'L' => {
                current = add(base, c.point()?);
                out.push(Segment::LineTo(current));
            }
            b'H' => {
                let x = c.number()?;
                current = PointF::new(if relative { current.x + x } else { x }, current.y);
                out.push(Segment::LineTo(current));
            }
            b'V' => {
                let y = c.number()?;
                current = PointF::new(current.x, if relative { current.y + y } else { y });
                out.push(Segment::LineTo(current));
            }
            b'C' => {
                let c1 = add(base, c.point()?);
                let c2 = add(base, c.point()?);
                let end = add(base, c.point()?);
                out.push(Segment::CubicTo(c1, c2, end));
                cubic_control = Some(c2);
                current = end;
            }
            b'S' => {
                let c1 = last_cubic_control.map_or(current, |p| reflect(p, current));
                let c2 = add(base, c.point()?);
                let end = add(base, c.point()?);
                out.push(Segment::CubicTo(c1, c2, end));
                cubic_control = Some(c2);
                current = end;
            }
            b'Q' => {
                let q = add(base, c.point()?);
                let end = add(base, c.point()?);
                out.push(quad_to_cubic(current, q, end));
                quad_control = Some(q);
                current = end;
            }
            b'T' => {
                let q = last_quad_control.map_or(current, |p| reflect(p, current));
                let end = add(base, c.point()?);
                out.push(quad_to_cubic(current, q, end));
                quad_control = Some(q);
                current = end;
            }
            b'A' => {
                let rx = c.number()?;
                let ry = c.number()?;
                let rotation = c.number()?;
                let large_arc = c.flag()?;
                let sweep = c.flag()?;
                let end = add(base, c.point()?);
                arc_to_cubics(current, rx, ry, rotation, large_arc, sweep, end, &mut out);
                current = end;
            }
            b'Z' => {
                out.push(Segment::Close);
                current = subpath_start;
            }
            _ => return Err(ParseError { offset: c.pos - 1, message: "unknown command" }),
        }
        last_cubic_control = cubic_control;
        last_quad_control = quad_control;
    }
    Ok(out)
}

fn quad_to_cubic(start: PointF, control: PointF, end: PointF) -> Segment {
    let two_thirds = |a: PointF, b: PointF| PointF::new(a.x + (b.x - a.x) * 2.0 / 3.0, a.y + (b.y - a.y) * 2.0 / 3.0);
    Segment::CubicTo(two_thirds(start, control), two_thirds(end, control), end)
}

/// SVG endpoint arc → center parameterization (SVG 1.1 implementation notes F.6.5/F.6.6) → cubic Béziers of at
/// most 90° each.
#[allow(clippy::too_many_arguments)]
pub fn arc_to_cubics(
    start: PointF,
    rx: f32,
    ry: f32,
    rotation_deg: f32,
    large_arc: bool,
    sweep: bool,
    end: PointF,
    out: &mut Vec<Segment>,
) {
    if start == end {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx == 0.0 || ry == 0.0 {
        out.push(Segment::LineTo(end));
        return;
    }
    let phi = rotation_deg.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();
    let dx = (start.x - end.x) / 2.0;
    let dy = (start.y - end.y) / 2.0;
    let x1p = cos_phi * dx + sin_phi * dy;
    let y1p = -sin_phi * dx + cos_phi * dy;

    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let numerator = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let denominator = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coefficient = (numerator / denominator).max(0.0).sqrt();
    if large_arc == sweep {
        coefficient = -coefficient;
    }
    let cxp = coefficient * rx * y1p / ry;
    let cyp = -coefficient * ry * x1p / rx;
    let cx = cos_phi * cxp - sin_phi * cyp + (start.x + end.x) / 2.0;
    let cy = sin_phi * cxp + cos_phi * cyp + (start.y + end.y) / 2.0;

    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 { -a } else { a }
    };
    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;
    let theta1 = angle(1.0, 0.0, ux, uy);
    let mut delta = angle(ux, uy, vx, vy);
    if !sweep && delta > 0.0 {
        delta -= 2.0 * PI;
    } else if sweep && delta < 0.0 {
        delta += 2.0 * PI;
    }

    let pieces = (delta.abs() / (PI / 2.0) - 1e-4).ceil().max(1.0) as usize;
    let step = delta / pieces as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let map = |x: f32, y: f32| PointF::new(cx + cos_phi * rx * x - sin_phi * ry * y, cy + sin_phi * rx * x + cos_phi * ry * y);
    let mut t = theta1;
    for i in 0..pieces {
        let (s1, c1) = t.sin_cos();
        let (s2, c2) = (t + step).sin_cos();
        let p1 = map(c1 - k * s1, s1 + k * c1);
        let p2 = map(c2 + k * s2, s2 - k * c2);
        let p3 = if i + 1 == pieces { end } else { map(c2, s2) };
        out.push(Segment::CubicTo(p1, p2, p3));
        t += step;
    }
}

fn attribute(element: &str, name: &str) -> Option<String> {
    let mut search = 0;
    while let Some(found) = element[search..].find(name) {
        let at = search + found;
        let before_ok = at == 0 || element.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = &element[at + name.len()..];
        let rest_trimmed = rest.trim_start();
        if before_ok && rest_trimmed.starts_with('=') {
            let value = rest_trimmed[1..].trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                let inner = &value[1..];
                return inner.find(quote).map(|close| inner[..close].to_string());
            }
        }
        search = at + name.len();
    }
    None
}

fn number_attribute(element: &str, name: &str) -> Option<f32> {
    attribute(element, name)?.trim().parse().ok()
}

fn ellipse_segments(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<Segment> {
    let mut out = vec![Segment::MoveTo(PointF::new(cx + rx, cy))];
    arc_to_cubics(PointF::new(cx + rx, cy), rx, ry, 0.0, false, true, PointF::new(cx - rx, cy), &mut out);
    arc_to_cubics(PointF::new(cx - rx, cy), rx, ry, 0.0, false, true, PointF::new(cx + rx, cy), &mut out);
    out.push(Segment::Close);
    out
}

fn rect_segments(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Vec<Segment> {
    let rx = rx.min(w / 2.0);
    let ry = ry.min(h / 2.0);
    if rx <= 0.0 || ry <= 0.0 {
        return vec![
            Segment::MoveTo(PointF::new(x, y)),
            Segment::LineTo(PointF::new(x + w, y)),
            Segment::LineTo(PointF::new(x + w, y + h)),
            Segment::LineTo(PointF::new(x, y + h)),
            Segment::Close,
        ];
    }
    let mut out = vec![Segment::MoveTo(PointF::new(x + rx, y))];
    let corner = |from: PointF, to: PointF, out: &mut Vec<Segment>| {
        out.push(Segment::LineTo(from));
        arc_to_cubics(from, rx, ry, 0.0, false, true, to, out);
    };
    corner(PointF::new(x + w - rx, y), PointF::new(x + w, y + ry), &mut out);
    corner(PointF::new(x + w, y + h - ry), PointF::new(x + w - rx, y + h), &mut out);
    corner(PointF::new(x + rx, y + h), PointF::new(x, y + h - ry), &mut out);
    corner(PointF::new(x, y + ry), PointF::new(x + rx, y), &mut out);
    out.push(Segment::Close);
    out
}

fn points_segments(points: &str, close: bool) -> Result<Vec<Segment>, ParseError> {
    let mut c = Cursor::new(points);
    let mut out = Vec::new();
    while !c.at_end() {
        let p = c.point()?;
        out.push(if out.is_empty() { Segment::MoveTo(p) } else { Segment::LineTo(p) });
    }
    if close && !out.is_empty() {
        out.push(Segment::Close);
    }
    Ok(out)
}

/// Parses a sequence of SVG child elements (`<path d=…/>`, `<circle/>`, `<rect/>`, `<line/>`, `<polyline/>`,
/// `<polygon/>`, `<ellipse/>`). Unknown elements are ignored.
pub fn parse_elements(markup: &str) -> Result<Vec<Shape>, ParseError> {
    let mut shapes = Vec::new();
    let mut rest = markup;
    while let Some(open) = rest.find('<') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else { break };
        let element = &after[..close];
        rest = &after[close + 1..];
        let name: String = element.chars().take_while(|ch| ch.is_ascii_alphanumeric()).collect();
        let filled = attribute(element, "fill").is_some_and(|f| f != "none");
        let num = |n: &str| number_attribute(element, n).unwrap_or(0.0);
        let segments = match name.as_str() {
            "path" => parse_path(&attribute(element, "d").unwrap_or_default())?,
            "line" => vec![
                Segment::MoveTo(PointF::new(num("x1"), num("y1"))),
                Segment::LineTo(PointF::new(num("x2"), num("y2"))),
            ],
            "circle" => ellipse_segments(num("cx"), num("cy"), num("r"), num("r")),
            "ellipse" => ellipse_segments(num("cx"), num("cy"), num("rx"), num("ry")),
            "rect" => {
                let rx = number_attribute(element, "rx");
                let ry = number_attribute(element, "ry");
                let (rx, ry) = (rx.or(ry).unwrap_or(0.0), ry.or(rx).unwrap_or(0.0));
                rect_segments(num("x"), num("y"), num("width"), num("height"), rx, ry)
            }
            "polyline" => points_segments(&attribute(element, "points").unwrap_or_default(), false)?,
            "polygon" => points_segments(&attribute(element, "points").unwrap_or_default(), true)?,
            _ => continue,
        };
        if !segments.is_empty() {
            shapes.push(Shape { segments, filled });
        }
    }
    Ok(shapes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> PointF {
        PointF::new(x, y)
    }

    fn near(a: PointF, b: PointF) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    fn end_of(s: &Segment) -> Option<PointF> {
        match *s {
            Segment::MoveTo(e) | Segment::LineTo(e) | Segment::CubicTo(_, _, e) => Some(e),
            Segment::Close => None,
        }
    }

    #[test]
    fn absolute_and_relative_lines() {
        let segs = parse_path("M3 7V5h2l1 1L10 10z").unwrap();
        assert_eq!(
            segs,
            vec![
                Segment::MoveTo(p(3.0, 7.0)),
                Segment::LineTo(p(3.0, 5.0)),
                Segment::LineTo(p(5.0, 5.0)),
                Segment::LineTo(p(6.0, 6.0)),
                Segment::LineTo(p(10.0, 10.0)),
                Segment::Close,
            ]
        );
    }

    #[test]
    fn implicit_lineto_after_move_and_compact_numbers() {
        let segs = parse_path("m2 16 4.5-9 4.5 9").unwrap();
        assert_eq!(segs, vec![Segment::MoveTo(p(2.0, 16.0)), Segment::LineTo(p(6.5, 7.0)), Segment::LineTo(p(11.0, 16.0))]);
        let segs = parse_path("M.5.5-1.42-.587").unwrap();
        assert_eq!(segs, vec![Segment::MoveTo(p(0.5, 0.5)), Segment::LineTo(p(-1.42, -0.587))]);
        let segs = parse_path("M1e1 2E-1").unwrap();
        assert_eq!(segs, vec![Segment::MoveTo(p(10.0, 0.2))]);
    }

    #[test]
    fn close_returns_to_subpath_start_for_relative_moves() {
        let segs = parse_path("M10 10l5 0zl0 5").unwrap();
        assert_eq!(segs[3], Segment::LineTo(p(10.0, 15.0)));
    }

    #[test]
    fn smooth_cubic_reflects_previous_control() {
        let segs = parse_path("M0 0C0 10 10 10 10 0S20 -10 20 0").unwrap();
        assert_eq!(segs[2], Segment::CubicTo(p(10.0, -10.0), p(20.0, -10.0), p(20.0, 0.0)));
        let segs = parse_path("M0 0L5 5S10 10 15 5").unwrap();
        assert_eq!(segs[2], Segment::CubicTo(p(5.0, 5.0), p(10.0, 10.0), p(15.0, 5.0)));
    }

    #[test]
    fn quadratic_elevates_and_smooth_quadratic_reflects() {
        let segs = parse_path("M0 0Q3 3 6 0T12 0").unwrap();
        assert_eq!(segs[1], Segment::CubicTo(p(2.0, 2.0), p(4.0, 2.0), p(6.0, 0.0)));
        match segs[2] {
            Segment::CubicTo(c1, c2, e) => {
                assert!(near(c1, p(8.0, -2.0)));
                assert!(near(c2, p(10.0, -2.0)));
                assert_eq!(e, p(12.0, 0.0));
            }
            _ => panic!("expected cubic"),
        }
    }

    #[test]
    fn arc_quarter_circle_hits_radius() {
        let segs = parse_path("M10 0A10 10 0 0 1 0 10").unwrap();
        assert_eq!(segs.len(), 2);
        let Segment::CubicTo(c1, c2, e) = segs[1] else { panic!() };
        assert_eq!(e, p(0.0, 10.0));
        let mid = |t: f32| {
            let s = p(10.0, 0.0);
            let u = 1.0 - t;
            p(
                u * u * u * s.x + 3.0 * u * u * t * c1.x + 3.0 * u * t * t * c2.x + t * t * t * e.x,
                u * u * u * s.y + 3.0 * u * u * t * c1.y + 3.0 * u * t * t * c2.y + t * t * t * e.y,
            )
        };
        for i in 1..10 {
            let q = mid(i as f32 / 10.0);
            let r = (q.x * q.x + q.y * q.y).sqrt();
            assert!((r - 10.0).abs() < 0.01, "radius {r}");
        }
    }

    #[test]
    fn arc_flags_without_separators_and_large_arc() {
        let segs = parse_path("M5 18a2 2 0 1 0 0-4 2 2 0 0 0 0 4z").unwrap();
        assert!(near(end_of(&segs[segs.len() - 2]).unwrap(), p(5.0, 18.0)));
        let compact = parse_path("M0 0a5 5 0 1010 0").unwrap();
        assert!(near(end_of(compact.last().unwrap()).unwrap(), p(10.0, 0.0)));
        let full = parse_path("M0 0A5 5 0 1 0 10 0").unwrap();
        assert_eq!(full.len(), 3, "half circle splits into two quarter arcs");
    }

    #[test]
    fn arc_radii_scale_up_when_too_small() {
        let segs = parse_path("M0 0A1 1 0 0 1 10 0").unwrap();
        let Segment::CubicTo(c1, ..) = segs[1] else { panic!() };
        assert!(c1.y < -1.0, "radius grows to 5 so the arc bulges well above the chord");
    }

    #[test]
    fn rotated_ellipse_arc_ends_on_target() {
        let segs = parse_path("M1 2A6 3 30 1 1 7 9").unwrap();
        assert!(near(end_of(segs.last().unwrap()).unwrap(), p(7.0, 9.0)));
    }

    #[test]
    fn errors_report_offsets() {
        assert!(parse_path("L1 1").is_ok());
        assert_eq!(parse_path("10 10").unwrap_err().offset, 0);
        assert!(parse_path("M1 1 X").is_err());
        assert!(parse_path("M1").is_err());
    }

    #[test]
    fn elements_parse() {
        let shapes = parse_elements(
            r#"<line x1="10" x2="14" y1="2" y2="2" />
<circle cx="12" cy="14" r="8" />
<rect width="18" height="18" x="3" y="3" rx="2" />
<polyline points="4 17 10 11 4 5" />
<polygon points="6 3 20 12 6 21 6 3" />
<circle cx="7.5" cy="7.5" r=".5" fill="currentColor" />"#,
        )
        .unwrap();
        assert_eq!(shapes.len(), 6);
        assert_eq!(shapes[0].segments, vec![Segment::MoveTo(p(10.0, 2.0)), Segment::LineTo(p(14.0, 2.0))]);
        assert_eq!(shapes[1].segments[0], Segment::MoveTo(p(20.0, 14.0)));
        assert_eq!(shapes[2].segments[0], Segment::MoveTo(p(5.0, 3.0)));
        assert_eq!(shapes[3].segments.len(), 3);
        assert_eq!(*shapes[4].segments.last().unwrap(), Segment::Close);
        assert!(shapes[5].filled && !shapes[1].filled);
    }

    #[test]
    fn every_lucide_icon_parses() {
        for icon in crate::icons::Icon::ALL {
            let shapes = parse_elements(icon.data().data).unwrap_or_else(|e| panic!("{icon:?}: {e}"));
            assert!(!shapes.is_empty(), "{icon:?}");
        }
    }
}
