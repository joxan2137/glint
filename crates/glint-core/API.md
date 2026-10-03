# glint-core public API

`glint-core` is platform-independent. Images use physical pixels and geometry has no DPI conversion.

## Geometry

- `PointI { x, y }`, `RectI { x, y, w, h }`: integer points and rectangles. `RectI` can be built from edges or points and supports bounds, containment, intersection, union, offset, and conversion to `RectF`.
- `PointF { x, y }`, `SizeF { w, h }`, `RectF { x, y, w, h }`: floating-point geometry. Helpers cover distance, edges, center, size, containment, inset, offset, scale, interpolation, and outward rounding.

## Images

- `Image { width, height, data }`: straight-alpha BGRA8 with tightly packed rows. Use `new`, `from_bgra`, `bounds`, `stride`, `pixel`, `is_opaque`, and `crop`.
- `HdrImage { width, height, data, sdr_white_nits, display_peak_nits }`: tightly packed linear scRGB RGBA `f16`; scRGB `1.0` is 80 nits. Use `bounds`, `pixel`, and `crop`.
- `f16` is re-exported from `half`.

## HDR tone mapping

- `ToneMapMode::{Auto, Clip}` selects BT.2390 highlight compression or per-channel clipping.
- `ToneMapParams { mode, exposure_stops }` configures conversion; its default is Auto at zero exposure.
- `HdrStats { peak, max, hdr_fraction, out_of_gamut_fraction }` reports brightness in SDR-white units. `has_hdr_content()` applies the half-code HDR tolerance.
- `tonemap::analyze(&HdrImage)` and `analyze_region(&HdrImage, RectI)` calculate content statistics.
- `tonemap::tonemap(&HdrImage, &ToneMapParams)` analyzes and converts a whole image to opaque BGRA8.
- `tonemap::tonemap_with_stats(...)` converts with caller-provided statistics for a stable preview.
- `tonemap::tonemap_region(...)` crops, analyzes that crop independently, and converts it.

## Encoding

- `encode::encode_png(&Image)` returns PNG bytes with alpha and fast compression.
- `encode::encode_jpeg(&Image, quality)` flattens alpha onto white and returns JPEG bytes.
- `encode::save(&Image, path, ImageFormat)` writes through a same-directory temporary file; JPEG uses quality 90.
- `encode::decode(bytes)` decodes PNG or JPEG bytes to BGRA8.

## Display and capture data

- `HdrInfo` contains SDR white, peak, full-frame peak, and minimum luminance in nits.
- `MonitorInfo` describes a monitor handle, names, physical-pixel bounds/work area, DPI, primary state, and optional HDR metadata. `scale()` returns `dpi / 96`.
- `WindowInfo` describes a visible top-level window and its z-order snapshot.
- `MonitorCapture` groups monitor metadata, its tone-mapped image, optional raw HDR image, and optional HDR statistics.

## Settings

- `Settings` groups `hotkeys`, `after_capture`, `capture`, `hdr`, `record`, `appearance`, and `general`; all settings types support serde and defaults.
- `CaptureMode`: Rectangle, Window, FullScreen, Freeform, Text, or ColorPicker.
- `ThemeMode`: System, Light, or Dark.
- `ImageFormat`: Png or Jpeg.
- `HotkeySettings` enables each global shortcut.
- `AfterCaptureSettings` controls clipboard, thumbnail/editor, auto-save, destination, format, sound, and optional `Outline`.
- `CaptureSettings` stores the last mode, magnifier visibility, and delay.
- `HdrSettings` stores tone-map mode, exposure, and badge visibility.
- `RecordSettings` stores frame rate, audio inputs, cursor inclusion, and destination.
- `AppearanceSettings` stores theme and system-accent use.
- `GeneralSettings` stores launch-at-login behavior.

Frequently used display, geometry, image, settings, and tone-map types are re-exported from the crate root.
