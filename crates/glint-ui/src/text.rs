//! DirectWrite text: styles per DESIGN §5, cached formats and layouts, optical vertical centering.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Result;
use glint_core::{PointF, RectF, SizeF};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout, IDWriteTypography,
};
use windows::core::{BOOL, HSTRING, w};

const TEXT_FAMILY: &str = "Segoe UI Variable Text";
const DISPLAY_FAMILY: &str = "Segoe UI Variable Display";
const FALLBACK_FAMILY: &str = "Segoe UI";
const UNCONSTRAINED: f32 = 1.0e5;
const LAYOUT_CACHE_LIMIT: usize = 2048;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Weight {
    Light,
    #[default]
    Regular,
    Medium,
    Semibold,
    Bold,
}

impl Weight {
    fn dwrite(self) -> DWRITE_FONT_WEIGHT {
        DWRITE_FONT_WEIGHT(match self {
            Weight::Light => 300,
            Weight::Regular => 400,
            Weight::Medium => 500,
            Weight::Semibold => 600,
            Weight::Bold => 700,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    #[default]
    Leading,
    Center,
    Trailing,
}

/// Font size in DIP, weight and layout options. Family follows DESIGN §5 (Variable Text below 20 DIP, Variable
/// Display from 20 DIP, Segoe UI fallback).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub weight: Weight,
    /// OpenType `tnum`: every digit has the same advance so changing numbers do not jitter.
    pub tabular: bool,
    pub align: TextAlign,
    /// Wrap at the available width; otherwise one line trimmed with an ellipsis.
    pub wrap: bool,
    /// Fixed line height in DIP for wrapped text.
    pub line_height: Option<f32>,
}

impl TextStyle {
    pub const fn new(size: f32) -> Self {
        Self { size, weight: Weight::Regular, tabular: false, align: TextAlign::Leading, wrap: false, line_height: None }
    }

    /// 11 DIP.
    pub const fn caption() -> Self {
        Self::new(11.0)
    }
    /// 13 DIP: body text and control labels.
    pub const fn body() -> Self {
        Self::new(13.0)
    }
    /// 13 DIP semibold.
    pub const fn emphasized() -> Self {
        Self::new(13.0).weight(Weight::Semibold)
    }
    /// 15 DIP semibold.
    pub const fn title() -> Self {
        Self::new(15.0).weight(Weight::Semibold)
    }
    /// 22 DIP semibold.
    pub const fn large_title() -> Self {
        Self::new(22.0).weight(Weight::Semibold)
    }
    /// 64 DIP semibold tabular figures.
    pub const fn countdown() -> Self {
        Self::new(64.0).weight(Weight::Semibold).tabular()
    }

    pub const fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
    pub const fn weight(mut self, weight: Weight) -> Self {
        self.weight = weight;
        self
    }
    pub const fn tabular(mut self) -> Self {
        self.tabular = true;
        self
    }
    pub const fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }
    pub const fn centered(self) -> Self {
        self.align(TextAlign::Center)
    }
    pub const fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }
    pub const fn line_height(mut self, height: f32) -> Self {
        self.line_height = Some(height);
        self
    }

    fn is_display(&self) -> bool {
        self.size >= 20.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FormatKey {
    display: bool,
    size_bits: u32,
    weight: Weight,
    align: TextAlign,
    wrap: bool,
    line_height_bits: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LayoutKey {
    text: String,
    format: FormatKey,
    tabular: bool,
    max_width_bits: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FontMetrics {
    /// All values in DIP for the style's size.
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: f32,
    pub x_height: f32,
}

/// A shaped, cached DirectWrite layout.
pub struct TextLayout {
    pub(crate) layout: IDWriteTextLayout,
    /// Ink-independent width of the text (without trailing whitespace).
    pub width: f32,
    pub height: f32,
    /// Distance from the layout top to the first baseline.
    pub baseline: f32,
    pub line_count: u32,
    /// Offset of the first glyph from the layout's left edge (non-zero for centered/trailing alignment).
    pub left: f32,
    pub metrics: FontMetrics,
    pub trimmed: bool,
}

impl TextLayout {
    pub fn raw(&self) -> &IDWriteTextLayout {
        &self.layout
    }

    pub fn size(&self) -> SizeF {
        SizeF::new(self.width, self.height)
    }

    /// Caret rectangle (relative to the layout origin) before UTF-16 index `position`.
    pub fn caret_rect(&self, position: u32) -> RectF {
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut hit = DWRITE_HIT_TEST_METRICS::default();
        // SAFETY: out-pointers are valid locals.
        let ok = unsafe { self.layout.HitTestTextPosition(position, false, &mut x, &mut y, &mut hit) }.is_ok();
        if !ok {
            return RectF::new(0.0, 0.0, 1.0, self.height);
        }
        RectF::new(x, hit.top, 1.0, hit.height)
    }

    /// UTF-16 index of the caret position nearest to `point` (relative to the layout origin).
    pub fn hit_test(&self, point: PointF) -> u32 {
        let mut trailing = BOOL::default();
        let mut inside = BOOL::default();
        let mut hit = DWRITE_HIT_TEST_METRICS::default();
        // SAFETY: out-pointers are valid locals.
        let ok = unsafe { self.layout.HitTestPoint(point.x, point.y, &mut trailing, &mut inside, &mut hit) }.is_ok();
        if !ok {
            return 0;
        }
        hit.textPosition + if trailing.as_bool() { hit.length } else { 0 }
    }
}

pub(crate) struct TextSystem {
    dwrite: IDWriteFactory,
    collection: IDWriteFontCollection,
    text_family: HSTRING,
    display_family: HSTRING,
    tabular: IDWriteTypography,
    formats: RefCell<HashMap<FormatKey, IDWriteTextFormat>>,
    layouts: RefCell<HashMap<LayoutKey, Rc<TextLayout>>>,
    font_metrics: RefCell<HashMap<(bool, Weight), FontMetrics>>,
}

fn family_exists(collection: &IDWriteFontCollection, name: &str) -> bool {
    let mut index = 0u32;
    let mut exists = BOOL::default();
    // SAFETY: out-pointers are valid locals.
    let ok = unsafe { collection.FindFamilyName(&HSTRING::from(name), &mut index, &mut exists) }.is_ok();
    ok && exists.as_bool()
}

impl TextSystem {
    pub(crate) fn new(dwrite: &IDWriteFactory) -> Result<Self> {
        let mut collection = None;
        // SAFETY: out-pointer is a valid Option.
        unsafe { dwrite.GetSystemFontCollection(&mut collection, false)? };
        let collection = collection.ok_or_else(|| anyhow::anyhow!("no system font collection"))?;
        let pick = |preferred: &str| {
            if family_exists(&collection, preferred) { HSTRING::from(preferred) } else { HSTRING::from(FALLBACK_FAMILY) }
        };
        let text_family = pick(TEXT_FAMILY);
        let display_family = pick(DISPLAY_FAMILY);
        // SAFETY: plain factory calls.
        let tabular = unsafe {
            let typography = dwrite.CreateTypography()?;
            typography.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 })?;
            typography
        };
        Ok(Self {
            dwrite: dwrite.clone(),
            collection,
            text_family,
            display_family,
            tabular,
            formats: RefCell::default(),
            layouts: RefCell::default(),
            font_metrics: RefCell::default(),
        })
    }

    pub(crate) fn family_names(&self) -> (String, String) {
        (self.text_family.to_string(), self.display_family.to_string())
    }

    fn format_key(style: &TextStyle) -> FormatKey {
        FormatKey {
            display: style.is_display(),
            size_bits: style.size.to_bits(),
            weight: style.weight,
            align: style.align,
            wrap: style.wrap,
            line_height_bits: style.line_height.unwrap_or(0.0).to_bits(),
        }
    }

    fn format(&self, style: &TextStyle) -> Result<IDWriteTextFormat> {
        let key = Self::format_key(style);
        if let Some(format) = self.formats.borrow().get(&key) {
            return Ok(format.clone());
        }
        let family = if key.display { &self.display_family } else { &self.text_family };
        // SAFETY: plain DirectWrite factory/format calls with valid strings.
        let format = unsafe {
            let format = self.dwrite.CreateTextFormat(
                family,
                &self.collection,
                style.weight.dwrite(),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                style.size,
                w!("en-us"),
            )?;
            format.SetTextAlignment(match style.align {
                TextAlign::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
                TextAlign::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                TextAlign::Trailing => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            if style.wrap {
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            } else {
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                let sign = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
                let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                format.SetTrimming(&trimming, &sign)?;
            }
            if let Some(line_height) = style.line_height {
                let metrics = self.font_metrics(style);
                let baseline = (line_height + metrics.ascent - metrics.descent) / 2.0;
                format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line_height, baseline)?;
            }
            format
        };
        self.formats.borrow_mut().insert(key, format.clone());
        Ok(format)
    }

    /// Ascent/descent/cap height for the style's family, weight and size.
    pub(crate) fn font_metrics(&self, style: &TextStyle) -> FontMetrics {
        let key = (style.is_display(), style.weight);
        let unit = self.font_metrics.borrow().get(&key).copied();
        let unit = unit.unwrap_or_else(|| {
            let measured = self.measure_font(key.0, style.weight).unwrap_or(FontMetrics {
                ascent: 0.93,
                descent: 0.25,
                cap_height: 0.7,
                x_height: 0.5,
            });
            self.font_metrics.borrow_mut().insert(key, measured);
            measured
        });
        FontMetrics {
            ascent: unit.ascent * style.size,
            descent: unit.descent * style.size,
            cap_height: unit.cap_height * style.size,
            x_height: unit.x_height * style.size,
        }
    }

    fn measure_font(&self, display: bool, weight: Weight) -> Result<FontMetrics> {
        let family = if display { &self.display_family } else { &self.text_family };
        let mut index = 0u32;
        let mut exists = BOOL::default();
        let mut metrics = DWRITE_FONT_METRICS::default();
        // SAFETY: out-pointers are valid locals; the family index comes from FindFamilyName.
        unsafe {
            self.collection.FindFamilyName(family, &mut index, &mut exists)?;
            let font = self.collection.GetFontFamily(index)?.GetFirstMatchingFont(
                weight.dwrite(),
                DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
            )?;
            font.GetMetrics(&mut metrics);
        }
        let em = metrics.designUnitsPerEm.max(1) as f32;
        Ok(FontMetrics {
            ascent: metrics.ascent as f32 / em,
            descent: metrics.descent as f32 / em,
            cap_height: metrics.capHeight as f32 / em,
            x_height: metrics.xHeight as f32 / em,
        })
    }

    /// Shaped layout for `text`; `max_width` None = unconstrained single measure.
    pub(crate) fn layout(&self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Result<Rc<TextLayout>> {
        let max_width = max_width.unwrap_or(UNCONSTRAINED).max(0.0);
        let key = LayoutKey {
            text: text.to_string(),
            format: Self::format_key(style),
            tabular: style.tabular,
            max_width_bits: max_width.to_bits(),
        };
        if let Some(layout) = self.layouts.borrow().get(&key) {
            return Ok(layout.clone());
        }
        let format = self.format(style)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain DirectWrite calls; out-pointers are valid locals.
        let (layout, text_metrics, first_line) = unsafe {
            let layout = self.dwrite.CreateTextLayout(&wide, &format, max_width, UNCONSTRAINED)?;
            if style.tabular {
                layout.SetTypography(&self.tabular, DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 })?;
            }
            let mut text_metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut text_metrics)?;
            let mut lines = [DWRITE_LINE_METRICS::default(); 1];
            let mut count = 0u32;
            let _ = layout.GetLineMetrics(Some(&mut lines), &mut count);
            (layout, text_metrics, lines[0])
        };
        let result = Rc::new(TextLayout {
            layout,
            width: text_metrics.width,
            height: text_metrics.height,
            baseline: first_line.baseline,
            line_count: text_metrics.lineCount,
            left: text_metrics.left,
            metrics: self.font_metrics(style),
            trimmed: first_line.isTrimmed.as_bool(),
        });
        let mut layouts = self.layouts.borrow_mut();
        if layouts.len() >= LAYOUT_CACHE_LIMIT {
            layouts.clear();
        }
        layouts.insert(key, result.clone());
        Ok(result)
    }
}
