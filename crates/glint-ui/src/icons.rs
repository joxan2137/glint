//! Lucide line icons parsed once into Direct2D path geometry on the 24-unit grid.

use std::rc::Rc;

use anyhow::Result;
use glint_core::PointF;
use icondata_core::IconData;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED,
    D2D1_FIGURE_END_OPEN, D2D1_FILL_MODE_ALTERNATE, D2D1_FILL_MODE_WINDING,
};
use windows::Win32::Graphics::Direct2D::{ID2D1Factory1, ID2D1PathGeometry};
use windows::core::Interface;
use windows_numerics::Vector2;

use crate::svg::{self, Segment, Shape};

/// Grid size of the source icons.
pub const ICON_GRID: f32 = 24.0;
/// Stroke width on the 24 grid (DESIGN §5).
pub const ICON_STROKE: f32 = 1.75;

macro_rules! icons {
    ($($name:ident => $lucide:ident;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($name,)*
        }

        impl Icon {
            pub const ALL: &'static [Icon] = &[$(Icon::$name,)*];

            pub fn data(self) -> &'static IconData {
                match self {
                    $(Icon::$name => icondata_lu::$lucide,)*
                }
            }

            pub fn name(self) -> &'static str {
                match self {
                    $(Icon::$name => stringify!($name),)*
                }
            }

        }
    };
}

icons! {
    SquareDashed => LuSquareDashed;
    AppWindow => LuAppWindow;
    Monitor => LuMonitor;
    Lasso => LuLasso;
    ScanText => LuScanText;
    Pipette => LuPipette;
    Timer => LuTimer;
    Camera => LuCamera;
    Video => LuVideo;
    X => LuX;
    Pen => LuPen;
    Pencil => LuPencil;
    Highlighter => LuHighlighter;
    Eraser => LuEraser;
    Square => LuSquare;
    Circle => LuCircle;
    Line => LuSlash;
    Minus => LuMinus;
    Plus => LuPlus;
    ArrowUpRight => LuArrowUpRight;
    Arrow => LuMoveUpRight;
    Type => LuType;
    TextSize => LuALargeSmall;
    Crop => LuCrop;
    Undo => LuUndo2;
    Redo => LuRedo2;
    Copy => LuCopy;
    Download => LuDownload;
    Save => LuSave;
    Share => LuShare;
    Settings => LuSettings;
    MousePointer => LuMousePointer2;
    Grid => LuGrid3x3;
    Redact => LuGrid2x2;
    Blur => LuDroplets;
    PaintBucket => LuPaintBucket;
    ZoomIn => LuZoomIn;
    ZoomOut => LuZoomOut;
    Hand => LuHand;
    Move => LuMove;
    Pause => LuPause;
    Play => LuPlay;
    Stop => LuSquare;
    PauseFill => LuPause;
    PlayFill => LuPlay;
    StopFill => LuSquare;
    RecordFill => LuCircle;
    Record => LuCircleDot;
    Mic => LuMic;
    MicOff => LuMicOff;
    Volume => LuVolume2;
    VolumeX => LuVolumeX;
    Check => LuCheck;
    ChevronDown => LuChevronDown;
    ChevronUp => LuChevronUp;
    ChevronLeft => LuChevronLeft;
    ChevronRight => LuChevronRight;
    Folder => LuFolder;
    Image => LuImage;
    Sun => LuSun;
    Moon => LuMoon;
    Trash => LuTrash2;
    Maximize => LuMaximize;
    Expand => LuMaximize2;
    More => LuEllipsis;
    ExternalLink => LuExternalLink;
    Scissors => LuScissors;
    Keyboard => LuKeyboard;
    Info => LuInfo;
    Clipboard => LuClipboard;
    Search => LuSearch;
}

impl Icon {
    /// Filled glyph variants fill their closed shapes as well as stroking them, which keeps the rounded corners.
    pub fn is_solid(self) -> bool {
        matches!(self, Icon::PauseFill | Icon::PlayFill | Icon::StopFill | Icon::RecordFill)
    }
}

pub(crate) struct IconGeometry {
    pub stroke: ID2D1PathGeometry,
    pub fill: Option<ID2D1PathGeometry>,
}

fn point(p: PointF) -> Vector2 {
    Vector2 { X: p.x, Y: p.y }
}

/// Builds one path geometry from figures of absolute segments.
pub(crate) fn build_geometry<'a>(
    factory: &ID2D1Factory1,
    figures: impl IntoIterator<Item = &'a [Segment]>,
    filled: bool,
    even_odd: bool,
) -> Result<ID2D1PathGeometry> {
    // SAFETY: plain geometry sink calls on freshly created objects.
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.SetFillMode(if even_odd { D2D1_FILL_MODE_ALTERNATE } else { D2D1_FILL_MODE_WINDING });
        let begin = if filled { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW };
        for segments in figures {
            let mut open = false;
            let mut start = PointF::default();
            let mut current = PointF::default();
            for segment in segments {
                match *segment {
                    Segment::MoveTo(p) => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        }
                        sink.BeginFigure(point(p), begin);
                        open = true;
                        start = p;
                        current = p;
                    }
                    Segment::LineTo(p) => {
                        if !open {
                            sink.BeginFigure(point(current), begin);
                            open = true;
                        }
                        sink.AddLine(point(p));
                        current = p;
                    }
                    Segment::CubicTo(c1, c2, p) => {
                        if !open {
                            sink.BeginFigure(point(current), begin);
                            open = true;
                        }
                        sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: point(c1), point2: point(c2), point3: point(p) });
                        current = p;
                    }
                    Segment::Close => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                            open = false;
                        }
                        current = start;
                    }
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
        }
        sink.Close()?;
        Ok(geometry.cast()?)
    }
}

pub(crate) fn icon_geometry(factory: &ID2D1Factory1, icon: Icon) -> Result<Rc<IconGeometry>> {
    let shapes = svg::parse_elements(icon.data().data)?;
    let stroke = build_geometry(factory, shapes.iter().map(|s| s.segments.as_slice()), false, false)?;
    let filled: Vec<&Shape> = shapes.iter().filter(|s| s.filled || icon.is_solid()).collect();
    let fill = if filled.is_empty() {
        None
    } else {
        Some(build_geometry(factory, filled.iter().map(|s| s.segments.as_slice()), true, false)?)
    };
    Ok(Rc::new(IconGeometry { stroke, fill }))
}
