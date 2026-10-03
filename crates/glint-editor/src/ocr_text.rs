//! Recognized text over the canvas: word boxes in image pixels, reading-order selection and copy text.

use glint_core::{PointF, RectF};

use crate::math::{Vec2, intersects, outset};

#[derive(Clone, Debug, PartialEq)]
pub struct WordBox {
    pub text: String,
    pub rect: RectF,
    pub line: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecognizedText {
    /// Reading order: lines top to bottom, words left to right.
    pub words: Vec<WordBox>,
    pub line_count: usize,
}

impl RecognizedText {
    /// `lines` of (text, rect) words; `offset` moves export coordinates back into image pixels.
    pub fn new(lines: Vec<Vec<(String, RectF)>>, offset: PointF) -> Self {
        let line_count = lines.iter().filter(|l| !l.is_empty()).count();
        let words = lines
            .into_iter()
            .filter(|l| !l.is_empty())
            .enumerate()
            .flat_map(|(line, words)| {
                words.into_iter().map(move |(text, rect)| WordBox { text, rect: rect.offset(offset.x, offset.y), line })
            })
            .collect();
        Self { words, line_count }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// The word under `p`, or the nearest one within `reach` image pixels.
    pub fn word_at(&self, p: PointF, reach: f32) -> Option<usize> {
        if let Some(i) = self.words.iter().position(|w| w.rect.contains(p)) {
            return Some(i);
        }
        self.words
            .iter()
            .enumerate()
            .map(|(i, w)| (i, distance_to_rect(p, w.rect)))
            .filter(|(_, d)| *d <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Words touched by a marquee (image pixels).
    pub fn words_in(&self, area: RectF) -> Option<(usize, usize)> {
        let hits: Vec<usize> =
            self.words.iter().enumerate().filter(|(_, w)| intersects(outset(w.rect, 1.0), area)).map(|(i, _)| i).collect();
        Some((*hits.first()?, *hits.last()?))
    }

    /// Text of words `first..=last` in reading order: spaces inside a line, newlines between lines.
    pub fn text_of(&self, first: usize, last: usize) -> String {
        let (a, b) = (first.min(last), first.max(last).min(self.words.len().saturating_sub(1)));
        let mut out = String::new();
        let mut line = None;
        for w in self.words.get(a..=b).unwrap_or_default() {
            match line {
                Some(l) if l == w.line => out.push(' '),
                Some(_) => out.push('\n'),
                None => {}
            }
            out.push_str(&w.text);
            line = Some(w.line);
        }
        out
    }

    pub fn all_text(&self) -> String {
        if self.words.is_empty() { String::new() } else { self.text_of(0, self.words.len() - 1) }
    }

    pub fn lines_in(&self, first: usize, last: usize) -> usize {
        let (a, b) = (first.min(last), first.max(last));
        match (self.words.get(a), self.words.get(b.min(self.words.len().saturating_sub(1)))) {
            (Some(x), Some(y)) => y.line - x.line + 1,
            _ => 0,
        }
    }
}

fn distance_to_rect(p: PointF, r: RectF) -> f32 {
    let dx = (r.x - p.x).max(0.0).max(p.x - r.right());
    let dy = (r.y - p.y).max(0.0).max(p.y - r.bottom());
    PointF::new(dx, dy).length()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RecognizedText {
        let w = |t: &str, x: f32, y: f32| (t.to_string(), RectF::new(x, y, 40.0, 12.0));
        RecognizedText::new(
            vec![
                vec![w("Hello", 0.0, 0.0), w("world", 50.0, 0.0)],
                vec![],
                vec![w("second", 0.0, 20.0), w("line", 50.0, 20.0), w("here", 100.0, 20.0)],
            ],
            PointF::new(10.0, 100.0),
        )
    }

    #[test]
    fn reading_order_text() {
        let t = sample();
        assert_eq!(t.line_count, 2);
        assert_eq!(t.all_text(), "Hello world\nsecond line here");
        assert_eq!(t.text_of(3, 1), "world\nsecond line");
        assert_eq!(t.lines_in(1, 3), 2);
        assert_eq!(t.lines_in(2, 4), 1);
    }

    #[test]
    fn hit_testing_uses_image_coordinates() {
        let t = sample();
        assert_eq!(t.word_at(PointF::new(65.0, 105.0), 4.0), Some(1));
        assert_eq!(t.word_at(PointF::new(65.0, 114.0), 4.0), Some(1), "nearest within reach");
        assert_eq!(t.word_at(PointF::new(500.0, 500.0), 4.0), None);
        assert_eq!(t.words_in(RectF::new(55.0, 105.0, 60.0, 20.0)), Some((1, 4)));
        assert_eq!(t.words_in(RectF::new(900.0, 0.0, 5.0, 5.0)), None);
    }
}
