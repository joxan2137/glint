//! Glint's render kit: Direct2D/DirectWrite/DirectComposition drawing, a single-threaded event loop with
//! compositor-paced frames, spring animation, Lucide icons, DESIGN §5 themes and Apple-style widgets.
//! See `API.md` for the overview.

pub mod anim;
pub mod app;
pub mod bitmap;
pub mod color;
pub mod cursor;
pub mod event;
pub mod gfx;
pub mod icons;
pub mod offscreen;
pub mod painter;
pub mod svg;
pub mod text;
pub mod theme;
pub mod view;
pub mod widgets;
pub mod win;
mod window;

pub use anim::{Animatable, Animated, CubicBezier, Motion, Spring, Tween};
pub use app::{App, AppProxy, TimerId, WindowId, run};
pub use bitmap::Bitmap;
pub use color::Color;
pub use cursor::Cursor;
pub use event::{Buttons, Event, HitArea, Key, KeyEvent, Modifiers, MouseButton, PointerEvent, PointerKind, WheelEvent};
pub use gfx::Gfx;
pub use icons::Icon;
pub use offscreen::{OffscreenSpec, render as render_offscreen, render_view as render_view_offscreen};
pub use painter::{Backdrop, Brush, Interpolation, LineCap, LineJoin, Painter, Path, PathBuilder, StrokeStyle};
pub use text::{TextAlign, TextLayout, TextStyle, Weight};
pub use theme::{Shadow, Theme, ThemeKind};
pub use view::{Ctx, View};
pub use win::enable_per_monitor_dpi_awareness;
pub use window::{WindowKind, WindowSpec};

pub use glint_core::{Image, PointF, PointI, RectF, RectI, SizeF, ThemeMode};
pub use windows_numerics::Matrix3x2;
