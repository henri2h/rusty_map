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
