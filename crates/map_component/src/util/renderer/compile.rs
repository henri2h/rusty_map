use freya::engine::prelude::{Image, Paint, PaintStyle, Path, Rect, raster_n32_premul};
use skia_safe::{Picture, PictureRecorder};

use crate::style::StyleSpec;
use crate::style::{Filter, StyleLayer};

use super::draw::{draw_layer_path, record_fill_layer, record_line_layer};
use super::labels::extract_labels;
use super::{FeatureGeometry, Properties, RenderableTile};

/// Pixel size tiles are rasterized at when compiled. Set above the on-screen range `tile_px` ever
/// takes within one `zoom_z` band (`TILE_SIZE * 2^(zoom - zoom_z)`, ~0.7x-1.4x `TILE_SIZE`, i.e.
/// ~362-724px — see `Viewport`) so the bitmap is always downscaled rather than upscaled, which
/// looks better under linear sampling.
///
/// Once `tile_px` exceeds this (heavy digital over-zoom past `MAX_REAL_ZOOM`), the paint loop
/// draws over-zoom cells baked from `CompiledTile::picture` instead of stretching this bitmap (see
/// `cache::cell_level`) — so this only has to comfortably cover the normal `tile_px` range.
pub const TILE_RASTER_SIZE: i32 = 768;

/// Side of [`CompiledTile::lines`]: about a tile's usual on-screen size (~1MiB each).
pub const LINES_FALLBACK_SIZE: i32 = 512;

// ─── Style compilation ────────────────────────────────────────────────────────
//
// Filtering/coloring/drawing a tile's features against a style is real work, so it's done once
// per `(tile, style, zoom_z)` triple here — off the UI thread, right alongside decoding — rather
// than every single paint frame. The bulk of it (any layer whose color doesn't need to visibly
// cross-fade within a zoom_z band) is rasterized straight into a fixed-size CPU bitmap
// (`raster_n32_premul`, no GPU context needed — safe to build inside `spawn_blocking`): the paint
// loop then just blits that bitmap (`Canvas::draw_image_rect`) once per tile per frame, instead of
// re-rasterizing every path's fill/stroke on every single frame. A prior version of this recorded
// into a Skia `Picture` and replayed it via `draw_picture` — that only saved re-issuing draw calls,
// not the actual rasterization cost, which dominates once many tiles are on screen at once.
//
// Widths/zoom-gating are evaluated once here using the tile's own integer `zoom_z` rather than
// the continuous zoom value paint sees each frame. This is intentionally coarser than
// re-evaluating every frame: style-defined width/color zoom stops now only update when `zoom_z`
// changes (which already triggers a fresh tile fetch/compile — a different real tile, different
// `z`). The continuous fractional zoom within a `zoom_z` band still smoothly scales tile content
// (the cached bitmap is stretched to the current `tile_px` via `draw_image_rect`'s destination
// rect) — only the *style's own* stepped interpolation loses its sub-zoom_z smoothing, which is a
// minor, deliberate trade for cutting per-frame rasterization from ~200 paths/tile to one blit.
pub struct CompiledTile {
    pub image: Image,
    /// The same drawing recorded as a `Picture` instead of rasterized — used by the paint loop
    /// when a tile is stretched past `image`'s own resolution (heavy digital over-zoom), since
    /// replaying exact vector geometry stays sharp where stretching the bitmap would blur.
    pub picture: Picture,
    /// Layers whose color is `ZoomInterp`/`ZoomStep` — rare in practice (checked against the real
    /// style: none currently use zoom-dependent *color*, only a few zoom-dependent *widths*, which
    /// get baked above like any other layer) — kept out of the baked bitmap and redrawn as vector
    /// paths each frame so their color can track the continuous zoom value exactly. All lines
    /// land here unconditionally too, regardless of color — see `record_line_layer`'s doc comment.
    pub zoom_dependent: Vec<ZoomDependentEntry>,
    /// `zoom_dependent` rasterized once at the tile's own zoom, at [`LINES_FALLBACK_SIZE`]: drawn
    /// while the exact per-bucket bake isn't ready (a tile that just loaded or whose bakes were
    /// evicted), so roads never vanish mid-zoom — only their width is off for a few frames.
    /// `None` when there are no such layers.
    pub lines: Option<Image>,
    /// POI icon markers extracted from the shortbread `pois` source layer (the style itself has
    /// no icon layers — icons are this app's own addition), drawn/hit-tested by the label pass.
    pub pois: Vec<CompiledPoi>,
    /// Rail/transit line features with their raw coordinates, for tap hit-testing — only filled
    /// when the style asks for it (`StyleSpec::pick_transit_lines`).
    pub transit_lines: Vec<TransitLine>,
    /// `poi_details` features (rusty_map's own tiles only), by `(N|W|R, osm id)` — the same
    /// identity a `poi` feature's id decodes to.
    pub poi_details: rustc_hash::FxHashMap<(char, u64), std::sync::Arc<Properties>>,
    /// Text labels extracted from this tile's symbol layers — text already evaluated and
    /// transformed, anchors in normalized tile coords. Drawn per-frame in screen space by the
    /// paint loop's label pass (never baked into the bitmap, so text size stays constant on
    /// screen and collision works across tiles).
    pub labels: Vec<CompiledLabel>,
}

/// A filtered-and-merged path for one style layer whose *color* still needs a per-frame lookup.
/// `style_index` indexes back into `StyleSpec::layers` to find that layer's `ColorExpr`.
#[derive(Clone)]
pub struct ZoomDependentEntry {
    pub style_index: usize,
    pub path: Path,
    /// For a layer whose color depends on the feature (`ColorExpr::is_data_driven`): one of the
    /// features merged into `path`, all of which share that color. `None` otherwise.
    pub properties: Option<std::sync::Arc<Properties>>,
}

/// One ready-to-place text label: everything zoom-independent resolved at compile time.
pub struct CompiledLabel {
    /// `text-field` evaluated against the feature's properties, `text-transform` applied.
    pub text: String,
    /// Index of the owning `StyleLayer::Symbol` in `StyleSpec::layers`.
    pub style_index: usize,
    /// [`crate::util::labels::label_identity`] of `(text, style_index)`, computed once here —
    /// the per-frame placement sort compares it for every candidate pair, and hashing the text
    /// there measured ~10% of main-thread CPU.
    pub identity: u64,
    pub anchor: LabelAnchor,
    /// Resolved `symbol-sort-key` value (lower draws first / wins collisions); `f64::MAX` when
    /// the layer has none, so unkeyed labels sort after keyed ones within a layer.
    pub sort_value: f64,
    /// The feature's decoded MVT properties, kept for the interactive-symbol roadmap
    /// (hover/hit-testing, search filtering by e.g. `kind`).
    pub properties: std::sync::Arc<Properties>,
    /// See `RenderableFeature::id`'s doc comment. Always `None` for a line-placed label — those
    /// merge several source features (street segments) into one label, so no single feature id
    /// applies.
    pub feature_id: Option<u64>,
}

/// One POI icon marker: normalized tile-local position plus the raw kind (e.g. `bakery`) it
/// wants an icon for. Sprite sheets name icons differently per style (`icon-bakery`,
/// `bakery_11`, …), so the actual sprite name is resolved against the loaded sheet at paint
/// time — see [`crate::util::sprites::SpriteSheet::resolve_poi`], which falls back to a close
/// kind's sprite, then the class's, then a generic dot.
pub struct CompiledPoi {
    /// Raw POI kind straight off the feature's properties, e.g. `bakery`.
    pub kind: String,
    /// The feature's coarser OpenMapTiles `class` (e.g. `shop` for `kind` `clothes`), when it
    /// differs from `kind` — tried second when resolving a sprite: osm-bright names its icons
    /// after `class` (`{class}_11`), so most specific kinds have no sprite of their own.
    pub class: Option<String>,
    pub x: f32,
    pub y: f32,
    /// The feature's `name` (often empty) — future hover tooltip.
    pub name: String,
    pub properties: std::sync::Arc<Properties>,
    /// See `RenderableFeature::id`'s doc comment.
    pub feature_id: Option<u64>,
}

/// One rail/transit line feature a tap can select.
pub struct TransitLine {
    /// Polylines in normalized tile coordinates.
    pub coords: Vec<Vec<(f32, f32)>>,
    pub properties: std::sync::Arc<Properties>,
    /// See `RenderableFeature::id`'s doc comment.
    pub feature_id: Option<u64>,
    /// From the `transit_lines` layer, so `properties` already names the lines using it
    /// (`route_<n>_*`); otherwise a bare `transportation` track.
    pub has_routes: bool,
}

fn extract_transit_lines(tile: &RenderableTile) -> Vec<TransitLine> {
    let tracks = tile
        .source_layers
        .get(super::TRANSIT_SOURCE_LAYER)
        .into_iter()
        .flatten()
        .filter(|feature| super::is_transit_line(&feature.properties))
        .map(|feature| (feature, false));
    let routes = tile
        .source_layers
        .get(super::ROUTE_LINES_LAYER)
        .into_iter()
        .flatten()
        .map(|feature| (feature, true));
    tracks
        .chain(routes)
        .filter_map(|(feature, has_routes)| {
            Some(TransitLine {
                // Owned copy: compiled tiles cross threads, the source `Rc` can't.
                coords: feature.line_coords.as_deref()?.clone(),
                properties: feature.properties.clone(),
                feature_id: feature.id,
                has_routes,
            })
        })
        .collect()
}

fn extract_poi_details(
    tile: &RenderableTile,
) -> rustc_hash::FxHashMap<(char, u64), std::sync::Arc<Properties>> {
    use mvt_reader::feature::Value;
    let Some(features) = tile.source_layers.get(super::POI_DETAILS_LAYER) else {
        return Default::default();
    };
    features
        .iter()
        .filter_map(|feature| {
            let kind = match feature.properties.get("osm_type") {
                Some(Value::String(kind)) => match kind.as_str() {
                    "node" => 'N',
                    "way" => 'W',
                    "relation" => 'R',
                    _ => return None,
                },
                _ => return None,
            };
            let id = match feature.properties.get("osm_id")? {
                Value::Int(id) | Value::SInt(id) => u64::try_from(*id).ok()?,
                Value::UInt(id) => *id,
                _ => return None,
            };
            Some(((kind, id), feature.properties.clone()))
        })
        .collect()
}

pub enum LabelAnchor {
    /// Normalized `0..1` tile-local position.
    Point { x: f32, y: f32 },
    /// Straight-text line placement: polyline midpoint, tangent angle at that point (radians,
    /// already flipped into the readable range so text is never upside-down — valid in screen
    /// space too, since tile scaling is uniform), and the polyline's total length in normalized
    /// units (screen length = `len_norm * tile_px`, used for the does-the-text-fit check).
    /// `chain` is that polyline itself (normalized tile coords): at heavy over-zoom one tile
    /// spans many screens, so the paint loop repeats the label along it instead of using only
    /// the midpoint, which is usually off screen there.
    Line {
        x: f32,
        y: f32,
        angle: f32,
        len_norm: f32,
        chain: Vec<(f32, f32)>,
    },
}

pub fn compile_tile(tile: &RenderableTile, style: &StyleSpec, zoom_z: u32) -> CompiledTile {
    let zoom = zoom_z as f64;
    let record_start = std::time::Instant::now();

    let mut recorder = PictureRecorder::new();
    let bounds = Rect::from_ltrb(0.0, 0.0, 1.0, 1.0);
    let canvas = recorder.begin_recording(bounds, false);

    let mut fill_paint = Paint::default();
    fill_paint.set_anti_alias(true);
    fill_paint.set_style(PaintStyle::Fill);
    let mut stroke_paint = Paint::default();
    stroke_paint.set_anti_alias(true);
    stroke_paint.set_style(PaintStyle::Stroke);

    let mut zoom_dependent = Vec::new();
    let mut labels = Vec::new();

    for (style_index, layer) in style.layers.iter().enumerate() {
        match layer {
            StyleLayer::Background(_) => {}
            StyleLayer::Symbol(symbol_layer) => {
                extract_labels(
                    tile,
                    symbol_layer,
                    style_index,
                    zoom,
                    style.max_real_zoom,
                    &mut labels,
                );
            }
            StyleLayer::Fill(fill_layer) => {
                if zoom < fill_layer.min_zoom || zoom > fill_layer.max_zoom {
                    continue;
                }
                let Some(features) = tile.source_layers.get(&fill_layer.source_layer) else {
                    continue;
                };
                let matched = features.iter().filter_map(|feature| {
                    let FeatureGeometry::Fill(path) = &feature.geometry else {
                        return None;
                    };
                    fill_layer
                        .filter
                        .as_ref()
                        .is_none_or(|f| f.eval(&feature.properties))
                        .then_some((path, feature))
                });
                record_fill_layer(
                    canvas,
                    matched,
                    fill_layer,
                    zoom,
                    &mut fill_paint,
                    &mut stroke_paint,
                    &mut zoom_dependent,
                    style_index,
                );
            }
            StyleLayer::Line(line_layer) => {
                if zoom < line_layer.min_zoom || zoom > line_layer.max_zoom {
                    continue;
                }
                let Some(features) = tile.source_layers.get(&line_layer.source_layer) else {
                    continue;
                };
                let matched = features.iter().filter_map(|feature| {
                    let FeatureGeometry::Line(path) = &feature.geometry else {
                        return None;
                    };
                    line_layer
                        .filter
                        .as_ref()
                        .is_none_or(|f| f.eval(&feature.properties))
                        .then_some((path, feature))
                });
                record_line_layer(matched, line_layer, zoom, &mut zoom_dependent, style_index);
            }
        }
    }

    let picture = recorder
        .finish_recording_as_picture(Some(&bounds))
        .expect("a PictureRecorder that began recording always yields a picture");
    let record_ms = record_start.elapsed().as_millis();

    // Rasterize the same recorded drawing into a fixed-size bitmap — the paint loop's fast path
    // for the common case (many tiles on screen, moderate zoom); `picture` itself is used directly
    // (baked into over-zoom cells, see `rasterize_cell`) when a tile is stretched past this
    // resolution.
    let raster_start = std::time::Instant::now();
    let image = rasterize_picture(&picture, TILE_RASTER_SIZE);
    let raster_ms = raster_start.elapsed().as_millis();
    if record_ms + raster_ms > 1 {
        log::debug!("  [compile-detail] filter+record {record_ms} ms, rasterize {raster_ms} ms");
    }

    let lines = (!zoom_dependent.is_empty())
        .then(|| rasterize_zoom_dependent(&zoom_dependent, style, zoom, LINES_FALLBACK_SIZE));

    CompiledTile {
        image,
        picture,
        zoom_dependent,
        lines,
        labels,
        pois: extract_pois(tile, style.poi_filter.as_ref()),
        transit_lines: extract_transit_lines(tile),
        poi_details: extract_poi_details(tile),
    }
}

/// Property keys checked (in order) to derive a POI's raw kind. `class`/`subclass` are
/// OpenMapTiles' schema (the `poi` layer, e.g. osm-bright); the rest are shortbread's `pois`
/// layer schema. Different styles use different vector schemas, so both are checked.
const POI_KIND_KEYS: &[&str] = &[
    "subclass", "class", "kind", "shop", "amenity", "tourism", "leisure", "craft",
];

/// Kinds of unnamed POI not worth an icon: a city tile has hundreds of garden pools, gates,
/// bins and bollards.
/// (Unnamed parking stays: people look for those.) The tile build drops them too.
const UNNAMED_SKIPPED: &[&str] = &[
    "swimming_pool",
    "gate",
    "lift_gate",
    "waste_basket",
    "bollard",
];

/// Source layer names carrying POI point features, checked in order: `poi` is OpenMapTiles'
/// schema, `pois` is shortbread's.
const POI_LAYER_NAMES: &[&str] = &["poi", "pois"];

fn extract_pois(tile: &RenderableTile, filter: Option<&Filter>) -> Vec<CompiledPoi> {
    let Some(features) = POI_LAYER_NAMES
        .iter()
        .filter_map(|name| tile.source_layers.get(*name))
        .find(|features| !features.is_empty())
    else {
        return Vec::new();
    };

    let mut pois = Vec::new();
    for feature in features {
        let FeatureGeometry::Points(points) = &feature.geometry else {
            continue;
        };
        if filter.is_some_and(|filter| !filter.eval(&feature.properties)) {
            continue;
        }
        let Some(kind) = POI_KIND_KEYS.iter().find_map(|key| {
            feature.properties.get(*key).and_then(|value| match value {
                mvt_reader::feature::Value::String(s) if !s.is_empty() => Some(s.clone()),
                _ => None,
            })
        }) else {
            continue;
        };

        let name = feature
            .properties
            .get("name")
            .and_then(|value| match value {
                mvt_reader::feature::Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if name.is_empty() && UNNAMED_SKIPPED.contains(&kind.as_str()) {
            continue;
        }

        let class = match feature.properties.get("class") {
            Some(mvt_reader::feature::Value::String(class))
                if !class.is_empty() && *class != kind =>
            {
                Some(class.clone())
            }
            _ => None,
        };

        for &(x, y) in points {
            pois.push(CompiledPoi {
                kind: kind.clone(),
                class: class.clone(),
                x,
                y,
                name: name.clone(),
                properties: feature.properties.clone(),
                feature_id: feature.id,
            });
        }
    }
    pois
}

/// Rasterizes `picture` (recorded in normalized `0..1` tile-local space) into a `size`×`size`
/// bitmap — `compile_tile`'s own `TILE_RASTER_SIZE` bake. Over-zoom draws from `rasterize_cell`
/// instead.
pub fn rasterize_picture(picture: &Picture, size: i32) -> Image {
    let mut surface =
        raster_n32_premul((size, size)).expect("size is always a positive constant/power-of-two");
    let canvas = surface.canvas();
    canvas.scale((size as f32, size as f32));
    canvas.draw_picture(picture, None, None);
    surface.image_snapshot()
}

/// Rasterizes one over-zoom cell — the `(cx, cy)` square of a tile split into a
/// `2^level`×`2^level` grid (see `cache::cell_level`) — at `size`×`size`: the tile's base
/// `picture` plus its `zoom_dependent` layers at `zoom`, in one bitmap. Everything outside the
/// cell falls off the raster's edges and is clipped there.
pub fn rasterize_cell(
    picture: &Picture,
    zoom_dependent: &[ZoomDependentEntry],
    style: &StyleSpec,
    zoom: f64,
    (level, cx, cy): (u32, u32, u32),
    size: i32,
) -> Image {
    let mut surface =
        raster_n32_premul((size, size)).expect("size is always a positive, bounded cell size");
    let canvas = surface.canvas();
    let n = (1u32 << level) as f32;
    let tile_px = size as f32 * n;
    canvas.scale((tile_px, tile_px));
    canvas.translate((-(cx as f32) / n, -(cy as f32) / n));
    canvas.draw_picture(picture, None, None);

    let mut fill_paint = Paint::default();
    fill_paint.set_anti_alias(true);
    fill_paint.set_style(PaintStyle::Fill);
    let mut stroke_paint = Paint::default();
    stroke_paint.set_anti_alias(true);
    stroke_paint.set_style(PaintStyle::Stroke);
    let stroke_scale = 1.0 / tile_px;
    for entry in zoom_dependent {
        draw_layer_path(
            canvas,
            &style.layers[entry.style_index],
            &entry.path,
            entry.properties.as_deref(),
            zoom,
            stroke_scale,
            &mut fill_paint,
            &mut stroke_paint,
        );
    }

    surface.image_snapshot()
}

/// Rasterizes a tile's `zoom_dependent` layers (the ones whose color/opacity — or, for lines,
/// always, see `record_line_layer`'s doc comment — can't be baked into `compile_tile`'s own
/// bitmap) into a `size`×`size` bitmap at `zoom`, off the render thread — see
/// `tiles::rasterize_zoom_dependent_in_background`/`cache::ensure_tile_bakes`, which call this
/// from a background task kicked off by input handlers.
pub fn rasterize_zoom_dependent(
    zoom_dependent: &[ZoomDependentEntry],
    style: &StyleSpec,
    zoom: f64,
    size: i32,
) -> Image {
    let mut surface =
        raster_n32_premul((size, size)).expect("size is always a positive, finite tile pixel size");
    let canvas = surface.canvas();
    canvas.scale((size as f32, size as f32));

    let mut fill_paint = Paint::default();
    fill_paint.set_anti_alias(true);
    fill_paint.set_style(PaintStyle::Fill);
    let mut stroke_paint = Paint::default();
    stroke_paint.set_anti_alias(true);
    stroke_paint.set_style(PaintStyle::Stroke);
    let stroke_scale = 1.0 / size as f32;

    for entry in zoom_dependent {
        draw_layer_path(
            canvas,
            &style.layers[entry.style_index],
            &entry.path,
            entry.properties.as_deref(),
            zoom,
            stroke_scale,
            &mut fill_paint,
            &mut stroke_paint,
        );
    }

    surface.image_snapshot()
}

#[cfg(test)]
mod tests {
    use freya::engine::prelude::Color;

    use super::*;

    /// Picture covering only the tile's top-right quadrant in red, in normalized tile space.
    fn top_right_red() -> Picture {
        let mut recorder = PictureRecorder::new();
        let bounds = Rect::from_ltrb(0.0, 0.0, 1.0, 1.0);
        let canvas = recorder.begin_recording(bounds, false);
        let mut paint = Paint::default();
        paint.set_color(Color::RED);
        canvas.draw_rect(Rect::from_ltrb(0.5, 0.0, 1.0, 0.5), &paint);
        recorder.finish_recording_as_picture(Some(&bounds)).unwrap()
    }

    fn center_pixel(image: &Image) -> Color {
        let size = image.width();
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        let info = image.image_info();
        assert!(image.read_pixels(
            info,
            &mut pixels,
            (size * 4) as usize,
            (0, 0),
            skia_safe::image::CachingHint::Allow,
        ));
        let i = ((size / 2 * size + size / 2) * 4) as usize;
        // N32 on little-endian is BGRA.
        Color::from_argb(pixels[i + 3], pixels[i + 2], pixels[i + 1], pixels[i])
    }

    #[test]
    fn rasterize_cell_maps_its_own_square_of_the_tile() {
        let picture = top_right_red();
        let style = crate::style::default::default_style();
        let cell = |level, cx, cy| {
            center_pixel(&rasterize_cell(
                &picture,
                &[],
                &style,
                14.0,
                (level, cx, cy),
                32,
            ))
        };

        // Level 1 (2×2 grid): only cell (1, 0) is the red quadrant.
        assert_eq!(cell(1, 1, 0), Color::RED);
        assert_eq!(cell(1, 0, 0).a(), 0);
        assert_eq!(cell(1, 1, 1).a(), 0);
        assert_eq!(cell(1, 0, 1).a(), 0);
        // Level 2 (4×4 grid): cells 2..4 × 0..2 are inside it, the rest outside.
        assert_eq!(cell(2, 3, 1), Color::RED);
        assert_eq!(cell(2, 2, 0), Color::RED);
        assert_eq!(cell(2, 1, 1).a(), 0);
        assert_eq!(cell(2, 3, 2).a(), 0);
    }
}
