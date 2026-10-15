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
