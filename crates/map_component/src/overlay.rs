//! Host-provided vector overlay drawn over the map every frame: a trip's or journey's path and
//! its stops, set through `MapController::route_overlay`. World coordinates (see
//! `viewport::lonlat_to_world`), so it follows pan/zoom without any per-view state.

use freya::{
    engine::prelude::{Color as SkColor, Paint, PaintStyle, PathBuilder},
    prelude::Color,
};
use skia_safe::{PaintCap, PaintJoin, PathEffect};

use crate::util::viewport::{Viewport, lonlat_to_world};

#[derive(Clone, PartialEq, Default)]
pub struct RouteOverlay {
    pub paths: Vec<OverlayPath>,
    pub stops: Vec<OverlayStop>,
}

#[derive(Clone, PartialEq)]
pub struct OverlayPath {
    /// World coordinates.
    pub points: Vec<(f64, f64)>,
    pub color: Color,
    /// Screen pixels.
    pub width: f32,
    /// Dotted, e.g. a walking leg.
    pub dashed: bool,
}

#[derive(Clone, PartialEq)]
pub struct OverlayStop {
    /// World coordinates.
    pub point: (f64, f64),
    pub color: Color,
    /// Larger ring, for a journey's start/end or a transfer.
    pub major: bool,
}

/// The device's position, drawn as a dot inside its accuracy circle, pointing towards `bearing`
/// when moving. Set through `MapController::user_location`.
#[derive(Clone, Copy, PartialEq)]
pub struct UserLocation {
    /// World coordinates.
    pub point: (f64, f64),
    /// Latitude in degrees, for the meters-per-pixel scale of the accuracy circle.
    pub lat: f64,
    /// Radius (meters) the true position lies within, at 68% confidence.
    pub accuracy_m: Option<f32>,
    /// Heading in degrees clockwise from north, when moving.
    pub bearing: Option<f32>,
}

impl UserLocation {
    pub fn from_lonlat(lon: f64, lat: f64, accuracy_m: Option<f32>, bearing: Option<f32>) -> Self {
        Self {
            point: lonlat_to_world(lon, lat),
            lat,
            accuracy_m,
            bearing,
        }
    }
}

/// Earth's equatorial circumference in meters: one world unit across at the equator.
const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.686;
const LOCATION_BLUE: SkColor = SkColor::from_rgb(26, 115, 232);

pub(crate) fn draw_user_location(
    canvas: &freya::engine::prelude::Canvas,
    location: &UserLocation,
    viewport: &Viewport,
) {
    let center = viewport.world_to_screen(location.point.0, location.point.1);
    let mut paint = Paint::default();
    paint.set_anti_alias(true);

    if let Some(accuracy) = location.accuracy_m {
        let meters_per_px =
            EARTH_CIRCUMFERENCE_M * location.lat.to_radians().cos() / viewport.world_px();
        let radius = (accuracy as f64 / meters_per_px) as f32;
        // Only when it reaches past the dot itself.
        if radius > 10.0 {
            paint.set_color(SkColor::from_argb(40, 26, 115, 232));
            canvas.draw_circle(center, radius, &paint);
            paint.set_style(PaintStyle::Stroke);
            paint.set_stroke_width(1.0);
            paint.set_color(SkColor::from_argb(110, 26, 115, 232));
            canvas.draw_circle(center, radius, &paint);
            paint.set_style(PaintStyle::Fill);
        }
    }

    if let Some(bearing) = location.bearing {
        // A wedge ahead of the dot, in the direction of travel.
        let (sin, cos) = bearing.to_radians().sin_cos();
        let at = |forward: f32, side: f32| {
            (
                center.0 + forward * sin + side * cos,
                center.1 - forward * cos + side * sin,
            )
        };
        let mut wedge = PathBuilder::new();
        wedge.move_to(at(18.0, 0.0));
        wedge.line_to(at(6.0, -7.0));
        wedge.line_to(at(6.0, 7.0));
        wedge.close();
        paint.set_color(LOCATION_BLUE);
        canvas.draw_path(&wedge.detach(), &paint);
    }

    paint.set_color(SkColor::from_argb(60, 0, 0, 0));
    canvas.draw_circle((center.0, center.1 + 1.0), 9.5, &paint);
    paint.set_color(SkColor::WHITE);
    canvas.draw_circle(center, 9.0, &paint);
    paint.set_color(LOCATION_BLUE);
    canvas.draw_circle(center, 6.5, &paint);
}

impl OverlayPath {
    pub fn from_lonlat(points: &[(f64, f64)], color: Color, width: f32, dashed: bool) -> Self {
        Self {
            points: points
                .iter()
                .map(|&(lon, lat)| lonlat_to_world(lon, lat))
                .collect(),
            color,
            width,
            dashed,
        }
    }
}

impl OverlayStop {
    pub fn from_lonlat(lon: f64, lat: f64, color: Color, major: bool) -> Self {
        Self {
            point: lonlat_to_world(lon, lat),
            color,
            major,
        }
    }
}

impl RouteOverlay {
    /// World-coordinate bounding box `((min_x, min_y), (max_x, max_y))` of everything drawn.
    pub fn world_bounds(&self) -> Option<((f64, f64), (f64, f64))> {
        let points = self
            .paths
            .iter()
            .flat_map(|path| path.points.iter().copied())
            .chain(self.stops.iter().map(|stop| stop.point));
        points.fold(None, |bounds, (x, y)| {
            Some(match bounds {
                None => ((x, y), (x, y)),
                Some(((min_x, min_y), (max_x, max_y))) => {
                    ((min_x.min(x), min_y.min(y)), (max_x.max(x), max_y.max(y)))
                }
            })
        })
    }
}

pub(crate) fn draw_overlay(
    canvas: &freya::engine::prelude::Canvas,
    overlay: &RouteOverlay,
    viewport: &Viewport,
) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_cap(PaintCap::Round);
    paint.set_stroke_join(PaintJoin::Round);

    for path in &overlay.paths {
        let mut points = path
            .points
            .iter()
            .map(|&(x, y)| viewport.world_to_screen(x, y));
        let Some(first) = points.next() else {
            continue;
        };
        let mut builder = PathBuilder::new();
        builder.move_to(first);
        for point in points {
            builder.line_to(point);
        }
        let skia_path = builder.detach();

        if path.dashed {
            paint.set_path_effect(PathEffect::dash(&[0.1, path.width * 2.2], 0.0));
            paint.set_stroke_width(path.width);
            paint.set_color(SkColor::from(path.color));
            canvas.draw_path(&skia_path, &paint);
            paint.set_path_effect(None);
        } else {
            paint.set_stroke_width(path.width + 4.0);
            paint.set_color(SkColor::from_argb(235, 255, 255, 255));
            canvas.draw_path(&skia_path, &paint);
            paint.set_stroke_width(path.width);
            paint.set_color(SkColor::from(path.color));
            canvas.draw_path(&skia_path, &paint);
        }
    }

    let mut fill = Paint::default();
    fill.set_anti_alias(true);
    for stop in &overlay.stops {
        let center = viewport.world_to_screen(stop.point.0, stop.point.1);
        let radius = if stop.major { 6.5 } else { 3.5 };
        fill.set_color(SkColor::WHITE);
        canvas.draw_circle(center, radius + 2.0, &fill);
        fill.set_color(SkColor::from(stop.color));
        canvas.draw_circle(center, radius, &fill);
        if stop.major {
            fill.set_color(SkColor::WHITE);
            canvas.draw_circle(center, radius * 0.45, &fill);
        }
    }
}

/// Host-provided point markers drawn over everything (labels included) and tappable through
/// `MapComponent::on_marker_tapped`, e.g. EV chargers coloured by live availability. Set through
/// `MapController::markers`.
#[derive(Clone, PartialEq, Default)]
pub struct MarkerSet {
    pub markers: Vec<MapMarker>,
    /// The marker shown as selected (larger), by [`MapMarker::id`].
    pub selected: Option<u64>,
    /// What up-close markers carry inside.
    pub glyph: MarkerGlyph,
}

/// The symbol inside a marker badge once zoomed in (see [`MARKER_GLYPH_ZOOM`]).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum MarkerGlyph {
    /// A lightning bolt: EV chargers.
    #[default]
    Bolt,
    /// A white dot: search results.
    Dot,
}

#[derive(Clone, PartialEq)]
pub struct MapMarker {
    /// Reported back on tap.
    pub id: u64,
    /// World coordinates.
    pub point: (f64, f64),
    pub color: Color,
}

impl MapMarker {
    pub fn from_lonlat(id: u64, lon: f64, lat: f64, color: Color) -> Self {
        Self {
            id,
            point: lonlat_to_world(lon, lat),
            color,
        }
    }
}

/// Marker radius in screen pixels at `zoom`: dots when zoomed out, badges with a bolt up close.
fn marker_radius(zoom: f32, selected: bool) -> f32 {
    let radius = if zoom < 8.0 {
        3.0
    } else if zoom < 11.0 {
        5.0
    } else {
        9.0
    };
    if selected { radius + 3.0 } else { radius }
}

/// Zoom from which markers carry a bolt glyph.
const MARKER_GLYPH_ZOOM: f32 = 11.0;

/// The id of the marker under screen point (`x`, `y`), if any — topmost (last drawn) first.
pub fn marker_at(set: &MarkerSet, viewport: &Viewport, zoom: f32, x: f32, y: f32) -> Option<u64> {
    set.markers.iter().rev().find_map(|marker| {
        let (cx, cy) = viewport.world_to_screen(marker.point.0, marker.point.1);
        // A few pixels of slack: the small dots are hard to hit otherwise.
        let reach = marker_radius(zoom, set.selected == Some(marker.id)) + 4.0;
        ((cx - x).powi(2) + (cy - y).powi(2) <= reach * reach).then_some(marker.id)
    })
}

pub(crate) fn draw_markers(
    canvas: &freya::engine::prelude::Canvas,
    set: &MarkerSet,
    viewport: &Viewport,
    zoom: f32,
    size: (f32, f32),
) {
    let mut fill = Paint::default();
    fill.set_anti_alias(true);
    let selected = set
        .selected
        .and_then(|id| set.markers.iter().find(|marker| marker.id == id));
    // The selected marker last, so it sits on top.
    let ordered = set
        .markers
        .iter()
        .filter(|marker| Some(marker.id) != set.selected)
        .chain(selected);
    for marker in ordered {
        let is_selected = set.selected == Some(marker.id);
        let radius = marker_radius(zoom, is_selected);
        let center = viewport.world_to_screen(marker.point.0, marker.point.1);
        if center.0 < -radius
            || center.1 < -radius
            || center.0 > size.0 + radius
            || center.1 > size.1 + radius
        {
            continue;
        }
        fill.set_color(SkColor::WHITE);
        canvas.draw_circle(center, radius + if radius > 4.0 { 2.0 } else { 1.0 }, &fill);
        fill.set_color(SkColor::from(marker.color));
        canvas.draw_circle(center, radius, &fill);
        if zoom >= MARKER_GLYPH_ZOOM && set.glyph == MarkerGlyph::Dot {
            fill.set_color(SkColor::WHITE);
            canvas.draw_circle(center, radius * 0.38, &fill);
        } else if zoom >= MARKER_GLYPH_ZOOM {
            // A lightning bolt, in a unit box scaled to the badge.
            let s = radius * 0.62;
            let (cx, cy) = center;
            let mut bolt = PathBuilder::new();
            bolt.move_to((cx + 0.15 * s, cy - 1.0 * s));
            bolt.line_to((cx - 0.55 * s, cy + 0.12 * s));
            bolt.line_to((cx - 0.02 * s, cy + 0.12 * s));
            bolt.line_to((cx - 0.15 * s, cy + 1.0 * s));
            bolt.line_to((cx + 0.55 * s, cy - 0.12 * s));
            bolt.line_to((cx + 0.02 * s, cy - 0.12 * s));
            bolt.close();
            fill.set_color(SkColor::WHITE);
            canvas.draw_path(&bolt.detach(), &fill);
        }
    }
}

/// One host-drawn shape, in world coordinates (see `viewport::lonlat_to_world`). Grouped into a
/// [`MapLayer`] and set through `MapController::set_layer`.
#[derive(Clone, PartialEq)]
pub enum MapItem {
    Polyline {
        points: Vec<(f64, f64)>,
        color: Color,
        /// Screen pixels.
        width: f32,
        dashed: bool,
    },
    /// Closed area, e.g. an isochrone.
    Polygon {
        points: Vec<(f64, f64)>,
        fill: Option<Color>,
        /// Color and screen-pixel width.
        stroke: Option<(Color, f32)>,
    },
    /// Tappable symbol, reported through `MapComponent::on_item_tapped` by `id`.
    Icon {
        id: u64,
        point: (f64, f64),
        glyph: IconGlyph,
        /// Degrees clockwise from north.
        rotation_deg: f32,
        color: Color,
        size_px: f32,
    },
    /// Text centered on `point`, with a white halo.
    Label {
        point: (f64, f64),
        text: String,
        color: Color,
        size_px: f32,
    },
}

/// The symbol of a [`MapItem::Icon`], pointing north before rotation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconGlyph {
    /// Top view of a sailing boat hull.
    Boat,
    Dot,
    Arrow,
}

fn lonlat_points(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    points
        .iter()
        .map(|&(lon, lat)| lonlat_to_world(lon, lat))
        .collect()
}

impl MapItem {
    pub fn polyline_lonlat(points: &[(f64, f64)], color: Color, width: f32, dashed: bool) -> Self {
        Self::Polyline {
            points: lonlat_points(points),
            color,
            width,
            dashed,
        }
    }

    pub fn polygon_lonlat(
        points: &[(f64, f64)],
        fill: Option<Color>,
        stroke: Option<(Color, f32)>,
    ) -> Self {
        Self::Polygon {
            points: lonlat_points(points),
            fill,
            stroke,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn icon_lonlat(
        id: u64,
        lon: f64,
        lat: f64,
        glyph: IconGlyph,
        rotation_deg: f32,
        color: Color,
        size_px: f32,
    ) -> Self {
        Self::Icon {
            id,
            point: lonlat_to_world(lon, lat),
            glyph,
            rotation_deg,
            color,
            size_px,
        }
    }

    pub fn label_lonlat(
        lon: f64,
        lat: f64,
        text: impl Into<String>,
        color: Color,
        size_px: f32,
    ) -> Self {
        Self::Label {
            point: lonlat_to_world(lon, lat),
            text: text.into(),
            color,
            size_px,
        }
    }
}

/// A group of [`MapItem`]s the host replaces as a whole, drawn in order above the route overlay
/// and below markers.
#[derive(Clone, PartialEq)]
pub struct MapLayer {
    pub id: u64,
    pub items: Vec<MapItem>,
    pub visible: bool,
}

impl MapLayer {
    pub fn new(id: u64, items: Vec<MapItem>) -> Self {
        Self {
            id,
            items,
            visible: true,
        }
    }
}

/// Whether the screen box around `points`, grown by `pad`, overlaps the canvas.
fn on_screen(points: &[(f32, f32)], pad: f32, size: (f32, f32)) -> bool {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in points {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    max_x >= -pad && max_y >= -pad && min_x <= size.0 + pad && min_y <= size.1 + pad
}

fn label_font(size: f32) -> freya::engine::prelude::Font {
    use freya::engine::prelude::{Font, FontMgr, FontStyle, Typeface};
    static TYPEFACE: std::sync::OnceLock<Typeface> = std::sync::OnceLock::new();
    let typeface = TYPEFACE.get_or_init(|| {
        let mgr = FontMgr::new();
        mgr.match_family_style("Noto Sans", FontStyle::bold())
            .or_else(|| mgr.legacy_make_typeface(None, FontStyle::bold()))
            .expect("system font manager always yields some typeface")
    });
    Font::from_typeface(typeface.clone(), size)
}

fn glyph_path(glyph: IconGlyph, s: f32) -> freya::engine::prelude::Path {
    let mut path = PathBuilder::new();
    match glyph {
        IconGlyph::Boat => {
            path.move_to((0.0, -0.5 * s));
            path.quad_to((0.25 * s, -0.22 * s), (0.21 * s, 0.15 * s));
            path.line_to((0.16 * s, 0.5 * s));
            path.line_to((-0.16 * s, 0.5 * s));
            path.line_to((-0.21 * s, 0.15 * s));
            path.quad_to((-0.25 * s, -0.22 * s), (0.0, -0.5 * s));
        }
        IconGlyph::Arrow => {
            path.move_to((0.0, -0.5 * s));
            path.line_to((0.38 * s, 0.4 * s));
            path.line_to((0.0, 0.2 * s));
            path.line_to((-0.38 * s, 0.4 * s));
        }
        IconGlyph::Dot => {
            path.add_circle((0.0, 0.0), 0.3 * s, None);
        }
    }
    path.close();
    path.detach()
}

fn draw_icon(
    canvas: &freya::engine::prelude::Canvas,
    center: (f32, f32),
    glyph: IconGlyph,
    rotation_deg: f32,
    color: Color,
    s: f32,
) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    canvas.save();
    canvas.translate(center);
    canvas.rotate(rotation_deg, None);

    let shape = glyph_path(glyph, s);
    let ring = if glyph == IconGlyph::Dot {
        0.1 * s
    } else {
        (0.07 * s).max(1.5)
    };
    paint.set_color(SkColor::from_argb(60, 0, 0, 0));
    canvas.draw_path(&shape.with_offset((0.0, 1.0)), &paint);
    paint.set_color(SkColor::from(color));
    canvas.draw_path(&shape, &paint);
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_join(PaintJoin::Round);
    paint.set_stroke_width(ring);
    paint.set_color(SkColor::WHITE);
    canvas.draw_path(&shape, &paint);

    if glyph == IconGlyph::Boat {
        // Mainsail let out to starboard from the mast.
        let mut sail = PathBuilder::new();
        sail.move_to((0.0, -0.18 * s));
        sail.quad_to((0.16 * s, 0.08 * s), (0.07 * s, 0.38 * s));
        sail.line_to((0.0, -0.18 * s));
        sail.close();
        let sail = sail.detach();
        paint.set_style(PaintStyle::Fill);
        paint.set_color(SkColor::from_argb(230, 255, 255, 255));
        canvas.draw_path(&sail, &paint);
    }
    canvas.restore();
}

/// Draws the visible `layers` in order, skipping items fully off the `size` canvas.
pub(crate) fn draw_layers(
    canvas: &freya::engine::prelude::Canvas,
    layers: &[std::rc::Rc<MapLayer>],
    viewport: &Viewport,
    size: (f32, f32),
) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_stroke_cap(PaintCap::Round);
    paint.set_stroke_join(PaintJoin::Round);

    let shape = |points: &[(f32, f32)], close: bool| {
        let mut builder = PathBuilder::new();
        builder.move_to(points[0]);
        for &point in &points[1..] {
            builder.line_to(point);
        }
        if close {
            builder.close();
        }
        builder.detach()
    };
    let to_screen = |points: &[(f64, f64)]| -> Vec<(f32, f32)> {
        points
            .iter()
            .map(|&(x, y)| viewport.world_to_screen(x, y))
            .collect()
    };

    for item in layers
        .iter()
        .filter(|layer| layer.visible)
        .flat_map(|layer| &layer.items)
    {
        match item {
            MapItem::Polyline {
                points,
                color,
                width,
                dashed,
            } => {
                let points = to_screen(points);
                if points.len() < 2 || !on_screen(&points, *width, size) {
                    continue;
                }
                paint.set_style(PaintStyle::Stroke);
                paint.set_stroke_width(*width);
                paint.set_color(SkColor::from(*color));
                if *dashed {
                    paint.set_stroke_cap(PaintCap::Butt);
                    paint.set_path_effect(PathEffect::dash(&[width * 3.0, width * 2.0], 0.0));
                }
                canvas.draw_path(&shape(&points, false), &paint);
                paint.set_path_effect(None);
                paint.set_stroke_cap(PaintCap::Round);
            }
            MapItem::Polygon {
                points,
                fill,
                stroke,
            } => {
                let points = to_screen(points);
                let pad = stroke.map_or(0.0, |(_, width)| width);
                if points.len() < 3 || !on_screen(&points, pad, size) {
                    continue;
                }
                let path = shape(&points, true);
                if let Some(fill) = fill {
                    paint.set_style(PaintStyle::Fill);
                    paint.set_color(SkColor::from(*fill));
                    canvas.draw_path(&path, &paint);
                }
                if let Some((color, width)) = stroke {
                    paint.set_style(PaintStyle::Stroke);
                    paint.set_stroke_width(*width);
                    paint.set_color(SkColor::from(*color));
                    canvas.draw_path(&path, &paint);
                }
            }
            MapItem::Icon {
                point,
                glyph,
                rotation_deg,
                color,
                size_px,
                ..
            } => {
                let center = viewport.world_to_screen(point.0, point.1);
                if on_screen(&[center], *size_px, size) {
                    draw_icon(canvas, center, *glyph, *rotation_deg, *color, *size_px);
                }
            }
            MapItem::Label {
                point,
                text,
                color,
                size_px,
            } => {
                let (x, y) = viewport.world_to_screen(point.0, point.1);
                let reach = size_px * text.chars().count() as f32;
                if !on_screen(&[(x, y)], reach, size) {
                    continue;
                }
                let font = label_font(*size_px);
                let Some(blob) = freya::engine::prelude::TextBlob::from_str(text, &font) else {
                    continue;
                };
                let (advance, _) = font.measure_str(text, None);
                let origin = (x - advance / 2.0, y + size_px * 0.35);
                paint.set_style(PaintStyle::Stroke);
                paint.set_stroke_width((size_px * 0.2).max(2.0));
                paint.set_color(SkColor::from_argb(230, 255, 255, 255));
                canvas.draw_text_blob(&blob, origin, &paint);
                paint.set_style(PaintStyle::Fill);
                paint.set_color(SkColor::from(*color));
                canvas.draw_text_blob(&blob, origin, &paint);
            }
        }
    }
}

/// The `(layer id, item id)` of the [`MapItem::Icon`] under screen point (`x`, `y`), if any,
/// topmost (last drawn) first.
pub fn item_at(
    layers: &[std::rc::Rc<MapLayer>],
    viewport: &Viewport,
    x: f32,
    y: f32,
) -> Option<(u64, u64)> {
    layers
        .iter()
        .rev()
        .filter(|layer| layer.visible)
        .find_map(|layer| {
            layer.items.iter().rev().find_map(|item| {
                let MapItem::Icon {
                    id, point, size_px, ..
                } = item
                else {
                    return None;
                };
                let (cx, cy) = viewport.world_to_screen(point.0, point.1);
                let reach = size_px / 2.0 + 4.0;
                ((cx - x).powi(2) + (cy - y).powi(2) <= reach * reach).then_some((layer.id, *id))
            })
        })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use torin::geometry::CursorPoint;

    use super::*;

    fn icon(id: u64, point: (f64, f64)) -> MapItem {
        MapItem::Icon {
            id,
            point,
            glyph: IconGlyph::Boat,
            rotation_deg: 0.0,
            color: Color::BLACK,
            size_px: 20.0,
        }
    }

    #[test]
    fn item_at_hits_topmost_icon() {
        let viewport = Viewport::new(CursorPoint::zero(), 12.0, 14);
        let point = viewport.screen_to_world(100.0, 100.0);
        let far = viewport.screen_to_world(400.0, 400.0);
        let layers = vec![
            Rc::new(MapLayer::new(1, vec![icon(10, point)])),
            Rc::new(MapLayer::new(2, vec![icon(20, point), icon(21, far)])),
        ];
        assert_eq!(item_at(&layers, &viewport, 102.0, 98.0), Some((2, 20)));
        assert_eq!(item_at(&layers, &viewport, 401.0, 400.0), Some((2, 21)));
        assert_eq!(item_at(&layers, &viewport, 250.0, 250.0), None);

        let mut hidden = MapLayer::new(2, vec![icon(20, point)]);
        hidden.visible = false;
        let layers = vec![layers[0].clone(), Rc::new(hidden)];
        assert_eq!(item_at(&layers, &viewport, 100.0, 100.0), Some((1, 10)));
    }
}
