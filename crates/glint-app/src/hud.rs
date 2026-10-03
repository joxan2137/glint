//! Recording HUD (DESIGN §10): a glass pill at the top center of the recorded monitor with a pulsing red dot, the
//! elapsed time, an audio level meter, pause/resume, stop, discard and a microphone toggle; plus the click-through
//! red border just outside the recorded region. Both windows are excluded from capture.

use std::time::Duration;

use glint_ui::widgets::{IconButton, Response, Toolbar, ToolbarAction, ToolbarItem};
use glint_ui::{Animated, Ctx, Event, Gfx, Icon, Painter, PointF, RectF, RectI, SizeF, TextStyle, View, Weight};

use crate::pipeline::format_clock;

pub const TOP_MARGIN: f32 = 12.0;
const SIDE_MARGIN: f32 = 32.0;
const BOTTOM_MARGIN: f32 = 48.0;
const CLOCK: SizeF = SizeF::new(76.0, 32.0);
const METER: SizeF = SizeF::new(26.0, 32.0);
/// Gap between the recorded region and the outside of the border window, in DIP.
pub const BORDER_OUTSET: f32 = 4.0;
const BORDER_WIDTH: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudPhase {
    Starting,
    Recording,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudReadout {
    pub phase: HudPhase,
    pub elapsed: Duration,
    /// Recent audio peak, 0..1 linear amplitude.
    pub level: f32,
}

impl Default for HudReadout {
    fn default() -> Self {
        Self { phase: HudPhase::Starting, elapsed: Duration::ZERO, level: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudCommand {
    TogglePause,
    Stop,
    Discard,
    Microphone(bool),
}

/// Maps a linear peak to 0..1 on a −48…0 dBFS scale.
pub fn meter_fraction(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    ((20.0 * level.log10() + 48.0) / 48.0).clamp(0.0, 1.0)
}

pub struct HudView {
    toolbar: Toolbar,
    readout: HudReadout,
    audio: bool,
    level: Animated<f32>,
}

impl HudView {
    /// `audio`: the recording has an audio track (the mic can only be toggled then).
    pub fn new(audio: bool, microphone: bool) -> Self {
        let mut items = vec![ToolbarItem::custom("clock", CLOCK)];
        if audio {
            items.push(ToolbarItem::custom("meter", METER));
        }
        items.extend([
            ToolbarItem::separator(),
            ToolbarItem::button("pause", IconButton::new(Icon::PauseFill).tooltip("Pause", None)),
            ToolbarItem::button(
                "stop",
                IconButton::new(Icon::StopFill).tooltip("Stop recording", Some("Win+Shift+R")),
            ),
            ToolbarItem::button("discard", IconButton::new(Icon::Trash).tooltip("Discard", None)),
            ToolbarItem::separator(),
        ]);
        let mut mic = IconButton::new(if microphone { Icon::Mic } else { Icon::MicOff })
            .tooltip("Microphone", None)
            .with_selected(microphone && audio);
        if !audio {
            mic = mic.disabled();
        }
        items.push(ToolbarItem::button("mic", mic));
        Self { toolbar: Toolbar::new(items), readout: HudReadout::default(), audio, level: Animated::snappy(0.0) }
    }

    pub fn window_size(&self, gfx: &Gfx) -> SizeF {
        let bar = self.toolbar.preferred_size(gfx);
        SizeF::new(bar.w + 2.0 * SIDE_MARGIN, bar.h + TOP_MARGIN + BOTTOM_MARGIN)
    }

    pub fn update(&mut self, cx: &mut Ctx, readout: HudReadout) {
        if readout.phase != self.readout.phase
            && let Some(pause) = self.toolbar.button_mut("pause")
        {
            let paused = readout.phase == HudPhase::Paused;
            pause.icon = if paused { Icon::PlayFill } else { Icon::PauseFill };
            pause.tooltip = Some(if paused { "Resume" } else { "Pause" }.to_string());
        }
        self.level.set(meter_fraction(readout.level));
        self.readout = readout;
        cx.request_paint();
    }

    /// Sets a readout without animation (previews).
    pub fn with_readout(mut self, readout: HudReadout) -> Self {
        self.level.snap(meter_fraction(readout.level));
        if let Some(pause) = self.toolbar.button_mut("pause") {
            pause.icon = if readout.phase == HudPhase::Paused { Icon::PlayFill } else { Icon::PauseFill };
        }
        self.readout = readout;
        self
    }

    fn paint_clock(&self, p: &mut Painter, slot: RectF, time: f64) {
        let theme = p.theme().clone();
        let dot = PointF::new(slot.x + 13.0, slot.center().y);
        let color = match self.readout.phase {
            HudPhase::Recording => {
                let pulse = 0.5 + 0.5 * (time * std::f64::consts::TAU / 1.6).cos() as f32;
                theme.destructive.with_alpha(0.5 + 0.5 * pulse)
            }
            HudPhase::Starting => theme.destructive.with_alpha(0.45),
            HudPhase::Paused => theme.text_tertiary,
        };
        if self.readout.phase == HudPhase::Recording {
            p.fill_circle(dot, 8.0, theme.destructive.with_alpha(0.14 * color.a));
        }
        p.fill_circle(dot, 5.0, color);
        let style = TextStyle::body().weight(Weight::Semibold).tabular();
        let ink = if self.readout.phase == HudPhase::Paused { theme.text_secondary } else { theme.text };
        p.text(&format_clock(self.readout.elapsed), &style, ink, RectF::new(slot.x + 27.0, slot.y, slot.w - 27.0, slot.h));
    }

    fn paint_meter(&self, p: &mut Painter, slot: RectF, time: f64) {
        let theme = p.theme().clone();
        let level = self.level.get();
        let profile = [0.55, 1.0, 0.75, 0.4];
        let bar_w = 3.0;
        let gap = 2.5;
        let total = profile.len() as f32 * bar_w + (profile.len() - 1) as f32 * gap;
        let max_h = 16.0;
        for (i, weight) in profile.iter().enumerate() {
            let wobble = 0.85 + 0.15 * ((time * 9.0 + i as f64 * 1.7).sin() as f32);
            let active = self.readout.phase == HudPhase::Recording;
            let h = if active { (3.0 + (max_h - 3.0) * level * weight * wobble).min(max_h) } else { 3.0 };
            let x = slot.center().x - total / 2.0 + i as f32 * (bar_w + gap);
            let bar = p.snap_rect(RectF::new(x, slot.center().y - h / 2.0, bar_w, h));
            let ink = if active && level > 0.02 { theme.text } else { theme.text_tertiary };
            p.fill_round_rect(bar, bar_w / 2.0, ink);
        }
    }
}

impl View for HudView {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        match self.toolbar.event(cx, event) {
            Response::Action(ToolbarAction::Clicked(id)) => {
                match id {
                    "pause" => cx.post(HudCommand::TogglePause),
                    "stop" => cx.post(HudCommand::Stop),
                    "discard" => cx.post(HudCommand::Discard),
                    "mic" if self.audio => {
                        if let Some(mic) = self.toolbar.button_mut("mic") {
                            let on = !mic.is_selected();
                            mic.set_selected(on);
                            mic.icon = if on { Icon::Mic } else { Icon::MicOff };
                            cx.post(HudCommand::Microphone(on));
                        }
                    }
                    _ => {}
                }
                true
            }
            response => response.consumed(),
        }
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.toolbar.layout_centered(cx.gfx(), cx.size().w / 2.0, TOP_MARGIN);
        self.toolbar.paint(p, None);
        let time = cx.time();
        if let Some(slot) = self.toolbar.item("clock").map(|i| i.rect()) {
            self.paint_clock(p, slot, time);
        }
        if let Some(slot) = self.toolbar.item("meter").map(|i| i.rect()) {
            self.paint_meter(p, slot, time);
        }
        if self.readout.phase != HudPhase::Paused {
            cx.animate();
        }
    }

    /// Only the pill takes clicks; its shadow and tooltip margins pass them to the windows below.
    fn interactive_region(&self) -> Option<Vec<RectF>> {
        Some(vec![self.toolbar.rect()])
    }
}

/// The border window's rect: the region (virtual-desktop px) grown by `BORDER_OUTSET` DIP, kept on its monitor.
pub fn border_rect_px(region: RectI, monitor: RectI, scale: f32) -> RectI {
    let outset = (BORDER_OUTSET * scale).ceil() as i32;
    let grown = RectI::from_ltrb(region.x - outset, region.y - outset, region.right() + outset, region.bottom() + outset);
    grown.intersect(&monitor).unwrap_or(monitor)
}

/// 2 DIP red rounded frame along the window edge; the window is click-through.
pub struct BorderView;

impl View for BorderView {
    fn paint(&mut self, _cx: &mut Ctx, p: &mut Painter) {
        let theme = p.theme().clone();
        let frame = p.bounds().inset(BORDER_WIDTH / 2.0);
        p.stroke_round_rect(frame, 6.0, theme.destructive, BORDER_WIDTH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_uses_a_decibel_scale() {
        assert_eq!(meter_fraction(0.0), 0.0);
        assert_eq!(meter_fraction(1.0), 1.0);
        assert!((meter_fraction(0.0631) - 0.5).abs() < 0.01, "-24 dBFS is half way");
        assert_eq!(meter_fraction(0.001), 0.0);
    }

    #[test]
    fn border_wraps_the_region_and_stays_on_the_monitor() {
        let monitor = RectI::new(0, 0, 1920, 1080);
        assert_eq!(border_rect_px(RectI::new(100, 100, 800, 600), monitor, 1.5), RectI::new(94, 94, 812, 612));
        assert_eq!(border_rect_px(monitor, monitor, 1.0), monitor);
        assert_eq!(border_rect_px(RectI::new(0, 500, 300, 200), monitor, 1.0), RectI::new(0, 496, 304, 208));
    }
}
