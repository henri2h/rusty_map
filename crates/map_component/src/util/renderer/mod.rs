mod compile;
mod draw;
mod labels;

use std::{rc::Rc, sync::Arc};

use freya::engine::prelude::{Path, PathBuilder, PathFillType};
use geo_types::{Coord, Geometry};
use mvt_reader::feature::{Feature, Value};
use rustc_hash::FxHashMap;

use super::tiles::DecodedTile;
use crate::style::{StyleLayer, StyleSpec, TextPlacement};

pub use compile::{
    CompiledLabel, CompiledPoi, CompiledTile, LabelAnchor, TILE_RASTER_SIZE, ZoomDependentEntry,
    compile_tile, rasterize_cell, rasterize_picture, rasterize_zoom_dependent,
};
pub use draw::{apply_opacity, draw_layer_path};
pub use labels::readable_angle;

/// On-screen pixel size of one tile at its own (integer) `zoom_z` — the scale a tile is compiled
/// for. Screen-space style widths (e.g. `line-width`) are baked into normalized tile-local units
/// by dividing by this, matching the `0..1` path space `normalize()` builds paths in.
///
/// 512, like MapLibre: vector styles' zoom-dependent sizes (line widths, text sizes, layer zoom
/// ranges) are authored for 512px tiles. At 256 a given zoom showed twice the ground per pixel
/// that the style assumes, so every road was drawn at the next zoom level's width — cities
/// filled solid with roads around z11-12.
pub const TILE_SIZE: f32 = 512.0;

/// A feature's properties, keyed for fast lookup. [`style::Filter`]/[`style::ColorExpr`] hash
/// this by key — `std::HashMap`'s default `SipHash` is needlessly slow for that (it's designed
/// to resist hash-flooding from untrusted input, not something our own decoded tile data needs);
/// `FxHashMap` is the same faster hasher `rustc` itself uses internally for exactly this kind of
/// hot, non-adversarial lookup.
///
/// [`style::Filter`]: crate::style::Filter
/// [`style::ColorExpr`]: crate::style::ColorExpr
pub type Properties = FxHashMap<String, Value>;

/// A tile's features, grouped by source layer name and pre-built into Skia [`Path`]s in
/// tile-local *normalized* space (`0.0..1.0`, dividing every coordinate by the layer's `extent`).
/// Building these once (off the UI thread, right after MVT decoding) instead of per paint call
/// means panning/zooming never re-walks raw feature geometry.
pub struct RenderableTile {
    pub source_layers: std::collections::HashMap<String, Vec<RenderableFeature>>,
}

pub struct RenderableFeature {
    pub geometry: FeatureGeometry,
    /// Shared (not cloned) by every label/POI/placed label made from this feature: per-frame
    /// clones of these maps measured ~13% of the main thread, and dropping thousands of them
    /// with each evicted tile more.
    pub properties: Arc<Properties>,
    /// The MVT feature's own protocol-level id, when the tile source sets one — Planetiler-based
    /// sources (OpenMapTiles schema, e.g. the live `openmaptiles.data.gouv.fr`/OpenFreeMap
    /// sources) pack the OSM element id into it (see `map::poi_lookup`'s doc comment), letting a
    /// tapped POI resolve its own `osm_type`/`osm_id` straight from the tile already downloaded
    /// for rendering — no extra network round trip. `None` for schemas that don't set it
    /// (`shortbread_v1`, the offline fallback style's source).
    pub id: Option<u64>,
    /// Raw normalized polyline coordinates, kept **only** for `*_labels` source layers (a Skia
    /// `Path` can't give its points back) — needed to compute a street label's midpoint/tangent.
    /// `None` everywhere else so the fill/line hot path pays nothing.
    pub line_coords: Option<Rc<Vec<Vec<(f32, f32)>>>>,
}

pub enum FeatureGeometry {
    /// A polygon (or multi-polygon)'s rings, combined into one path (even-odd fill handles
    /// holes).
    Fill(Path),
    /// A line string (or multi-line-string), combined into one path.
    Line(Path),
    /// A point (or multi-point) feature, normalized `(x, y)` tile-local coordinates.
    Points(Vec<(f32, f32)>),
}

/// `style` decides which source layers keep raw line coordinates (`RenderableFeature::line_coords`):
/// only those a line-placed symbol layer reads (street names — `street_labels` in shortbread,
/// `transportation_name`/`waterway` in OpenMapTiles), for label anchor math.
pub fn build_renderable_tile(decoded: DecodedTile, style: &StyleSpec) -> RenderableTile {
    let line_label_sources: rustc_hash::FxHashSet<&str> = style
        .layers
        .iter()
        .filter_map(|layer| match layer {
            StyleLayer::Symbol(symbol) if symbol.placement == TextPlacement::Line => {
                Some(symbol.source_layer.as_str())
            }
            _ => None,
        })
        .collect();
    let source_layers = decoded
        .into_iter()
        .map(|(layer, features)| {
            let keep_line_coords = line_label_sources.contains(layer.name.as_str());
            (
                layer.name,
                build_layer_features(layer.extent, features, keep_line_coords),
            )
        })
        .collect();

    RenderableTile { source_layers }
}

fn build_layer_features(
    extent: u32,
    features: Vec<Feature>,
    keep_line_coords: bool,
) -> Vec<RenderableFeature> {
    let extent = extent.max(1) as f32;

    features
        .into_iter()
        .flat_map(|feature| {
            let properties: Arc<Properties> =
                Arc::new(feature.properties.into_iter().flatten().collect());
            let line_coords = keep_line_coords
                .then(|| extract_line_coords(&feature.geometry, extent))
                .filter(|lines| !lines.is_empty())
                .map(Rc::new);
            let id = feature.id;
            build_feature_geometries(&feature.geometry, extent)
                .into_iter()
                .map(move |geometry| RenderableFeature {
                    geometry,
                    properties: properties.clone(),
                    line_coords: line_coords.clone(),
                    id,
                })
        })
        .collect()
}

fn extract_line_coords(geometry: &Geometry<f32>, extent: f32) -> Vec<Vec<(f32, f32)>> {
    match geometry {
        Geometry::LineString(line) => vec![
            line.coords()
                .map(|coord| normalize(coord, extent))
                .collect(),
        ],
        Geometry::MultiLineString(multi) => multi
            .0
            .iter()
            .map(|line| {
                line.coords()
                    .map(|coord| normalize(coord, extent))
                    .collect()
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn build_feature_geometries(geometry: &Geometry<f32>, extent: f32) -> Vec<FeatureGeometry> {
    match geometry {
        Geometry::Point(point) => vec![FeatureGeometry::Points(vec![normalize(&point.0, extent)])],
        Geometry::MultiPoint(multi) => vec![FeatureGeometry::Points(
            multi
                .0
                .iter()
                .map(|point| normalize(&point.0, extent))
                .collect(),
        )],
        Geometry::LineString(line) => {
            let mut builder = PathBuilder::new();
            add_ring(&mut builder, line.coords(), extent, false);
            vec![FeatureGeometry::Line(builder.detach())]
        }
        Geometry::MultiLineString(multi) => {
            let mut builder = PathBuilder::new();
            for line in &multi.0 {
                add_ring(&mut builder, line.coords(), extent, false);
            }
            vec![FeatureGeometry::Line(builder.detach())]
        }
        Geometry::Polygon(polygon) => {
            let mut builder = PathBuilder::new_with_fill_type(PathFillType::EvenOdd);
            add_ring(&mut builder, polygon.exterior().coords(), extent, true);
            for interior in polygon.interiors() {
                add_ring(&mut builder, interior.coords(), extent, true);
            }
            vec![FeatureGeometry::Fill(builder.detach())]
        }
        Geometry::MultiPolygon(multi) => {
            let mut builder = PathBuilder::new_with_fill_type(PathFillType::EvenOdd);
            for polygon in &multi.0 {
                add_ring(&mut builder, polygon.exterior().coords(), extent, true);
                for interior in polygon.interiors() {
                    add_ring(&mut builder, interior.coords(), extent, true);
                }
            }
            vec![FeatureGeometry::Fill(builder.detach())]
        }
        Geometry::GeometryCollection(collection) => collection
            .0
            .iter()
            .flat_map(|geometry| build_feature_geometries(geometry, extent))
            .collect(),
        // MVT tiles never actually produce these; nothing to draw.
        Geometry::Line(_) | Geometry::Rect(_) | Geometry::Triangle(_) => vec![],
    }
}

fn add_ring<'a>(
    builder: &mut PathBuilder,
    coords: impl Iterator<Item = &'a Coord<f32>>,
    extent: f32,
    close: bool,
) {
    let mut coords = coords.map(|coord| normalize(coord, extent));

    let Some(first) = coords.next() else {
        return;
    };

    builder.move_to(first);
    for point in coords {
        builder.line_to(point);
    }
    if close {
        builder.close();
    }
}

fn normalize(coord: &Coord<f32>, extent: f32) -> (f32, f32) {
    (coord.x / extent, coord.y / extent)
}
