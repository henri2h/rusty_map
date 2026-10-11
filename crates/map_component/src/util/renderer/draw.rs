use freya::engine::prelude::{Canvas, Color, Paint, Path, PathBuilder};
use rustc_hash::FxHashMap;
use skia_safe::{PaintCap, PaintJoin, PathEffect};

use crate::style::{ColorExpr, FillLayer, LineCap, LineJoin, NumExpr, StyleLayer};

use super::compile::ZoomDependentEntry;
use super::{Properties, RenderableFeature};

/// Draws one already-merged path for `layer`, evaluating its zoom gate, color, and width at the
/// continuous `zoom` value — the single place per-layer-kind drawing logic lives for paths drawn
/// fresh each frame (the paint loop's `zoom_dependent` entries). `stroke_scale` converts
/// screen-pixel style widths into the canvas's current normalized tile-local space (the canvas is
/// already scaled into `0..1` tile space when this is called). A future layer kind (e.g. `Symbol`
/// text) adds its arm here plus `compile_tile`'s match and the style parser — nothing else.
pub fn draw_layer_path(
    canvas: &Canvas,
    layer: &StyleLayer,
    path: &Path,
    zoom: f64,
    stroke_scale: f32,
    fill_paint: &mut Paint,
    stroke_paint: &mut Paint,
) {
    match layer {
        StyleLayer::Background(_) => {}
        // Labels are drawn by the paint loop's dedicated screen-space label pass, not here.
        StyleLayer::Symbol(_) => {}
        StyleLayer::Fill(fill_layer) => {
            if zoom < fill_layer.min_zoom || zoom > fill_layer.max_zoom {
                return;
            }
            let color = apply_opacity(
                fill_layer.color.eval(&Properties::default(), zoom),
                fill_layer.opacity.eval(zoom) as f32,
            );
            // `fill-translate` (e.g. the "building-top" pseudo-3D overlay, offset a couple
            // pixels up-left of the base building fill): shift the canvas by the same amount a
            // style pixel width converts to here (`stroke_scale`, screen px → this canvas's
            // current normalized space), draw, then undo it — `path` itself stays untranslated
            // so other layers sharing it (there are none today, but the contract holds) aren't
            // affected.
            if let Some((dx, dy)) = &fill_layer.translate {
                let dx = dx.eval(zoom) as f32 * stroke_scale;
                let dy = dy.eval(zoom) as f32 * stroke_scale;
                canvas.save();
                canvas.translate((dx, dy));
                draw_fill_path(
                    canvas,
                    path,
                    color,
                    fill_layer.outline_color,
                    fill_paint,
                    stroke_paint,
                );
                canvas.restore();
            } else {
                draw_fill_path(
                    canvas,
                    path,
                    color,
                    fill_layer.outline_color,
                    fill_paint,
                    stroke_paint,
                );
            }
        }
        StyleLayer::Line(line_layer) => {
            if zoom < line_layer.min_zoom || zoom > line_layer.max_zoom {
                return;
            }
            let width_px = line_layer.width.eval(zoom) as f32;
            stroke_paint.set_stroke_cap(match line_layer.cap {
                LineCap::Butt => PaintCap::Butt,
                LineCap::Round => PaintCap::Round,
                LineCap::Square => PaintCap::Square,
            });
            stroke_paint.set_stroke_join(match line_layer.join {
                LineJoin::Miter => PaintJoin::Miter,
                LineJoin::Round => PaintJoin::Round,
                LineJoin::Bevel => PaintJoin::Bevel,
            });
            if let Some(dasharray) = &line_layer.dasharray {
                // MapLibre dasharray units are multiples of the line width; scale into the same
                // normalized tile-local space the path lives in (`stroke_scale` converts style
                // pixels to that space, same as the width just below).
                let intervals: Vec<f32> = dasharray
                    .iter()
                    .map(|interval| interval * width_px * stroke_scale)
                    .collect();
                stroke_paint.set_path_effect(PathEffect::dash(&intervals, 0.0));
            }
            draw_line_path(
                canvas,
                path,
                apply_opacity(
                    line_layer.color.eval(&Properties::default(), zoom),
                    line_layer.opacity.eval(zoom) as f32,
                ),
                width_px * stroke_scale,
                stroke_paint,
            );
            stroke_paint.set_path_effect(None);
            stroke_paint.set_stroke_cap(PaintCap::Butt);
            stroke_paint.set_stroke_join(PaintJoin::Miter);
        }
    }
}

fn draw_fill_path(
    canvas: &Canvas,
    path: &Path,
    color: Color,
    outline_color: Option<Color>,
    fill_paint: &mut Paint,
    stroke_paint: &mut Paint,
) {
    fill_paint.set_color(color);
    canvas.draw_path(path, fill_paint);

    if let Some(outline) = outline_color {
        stroke_paint.set_color(outline);
        // Hairline: always one device pixel, like MapLibre's fill outline. A width in tile units
        // (the recorded picture's space) scaled with over-zoom instead — ~22px at z19.5.
        stroke_paint.set_stroke_width(0.0);
        canvas.draw_path(path, stroke_paint);
    }
}

fn draw_line_path(
    canvas: &Canvas,
    path: &Path,
    color: Color,
    width: f32,
    stroke_paint: &mut Paint,
) {
    stroke_paint.set_stroke_width(width);
    stroke_paint.set_color(color);
    canvas.draw_path(path, stroke_paint);
}

pub fn record_fill_layer<'a>(
    canvas: &Canvas,
    matched: impl Iterator<Item = (&'a Path, &'a RenderableFeature)>,
    fill_layer: &FillLayer,
    zoom: f64,
    fill_paint: &mut Paint,
    stroke_paint: &mut Paint,
    zoom_dependent: &mut Vec<ZoomDependentEntry>,
    style_index: usize,
) {
    // Anything whose paint depends on continuous zoom beyond just color — `fill-opacity` (e.g.
    // "building-top"'s 0-at-z13-to-1-at-z16 fade-in) or `fill-translate` — also can't be baked:
    // baking freezes it at the compile-time `zoom_z` bucket forever. `translate` doubly so — a
    // baked, unshifted "building-top" would draw exactly on top of the baked "building" fill
    // below it with no offset, and land in the wrong relative *order* besides (this layer's own
    // `zoom_dependent` entry, if pushed, is drawn after every baked layer, alongside "building"'s
    // — which restores their correct relative order too, since compile_tile visits layers in the
    // style's own sequence).
    if matches!(
        fill_layer.color,
        ColorExpr::ZoomInterp(_) | ColorExpr::ZoomStep { .. }
    ) || matches!(
        fill_layer.opacity,
        NumExpr::ZoomInterp { .. } | NumExpr::ZoomStep { .. }
    ) || fill_layer.translate.is_some()
    {
        push_zoom_dependent(matched, zoom_dependent, style_index);
        return;
    }

    let opacity = fill_layer.opacity.eval(zoom) as f32;
    let groups = batch_paths_by_color(matched.map(|(path, feature)| {
        (
            path,
            apply_opacity(fill_layer.color.eval(&feature.properties, zoom), opacity),
        )
    }));
    for (color, path) in groups {
        draw_fill_path(
            canvas,
            &path,
            color,
            fill_layer.outline_color,
            fill_paint,
            stroke_paint,
        );
    }
}

/// Lines are never baked into the compile-time bitmap — always pushed to `zoom_dependent` and
/// stroked fresh every frame at the continuous viewport zoom (see `draw_layer_path`'s `Line` arm).
/// Baking them, like fills, would mean panning/zooming shows them through the tile's rasterized
/// bitmap: fine for large fills (blur only shows at their edges), but roads are *all* edge —
/// stretching that bitmap past its native resolution (any digital over-zoom past the source's
/// `max_real_zoom`, the common case once panning around a bit) turned every road into a blurry,
/// bleeding smear while buildings right next to them still looked fine. Confirmed directly: a
/// screenshot at z18.5 showed crisp building fills next to mush where the road should be.
/// Redrawing lines live costs roughly the merged path's stroke length, not the tile's pixel area,
/// so it stays cheap regardless of how far a tile is digitally over-zoomed.
///
/// Tried baking lines (unless color/opacity was `ZoomInterp`/`ZoomStep`, evaluating width once at
/// compile-time `zoom_z` like every other baked layer's sizing) to cut the always-live cost this
/// doc comment describes — real style check showed 0 of 72 line layers use zoom-dependent color,
/// so nearly everything should have baked. Caused a visible UI regression in practice, reverted.
/// Root cause not diagnosed — worth another look before retrying, rather than re-guessing.
pub fn record_line_layer<'a>(
    matched: impl Iterator<Item = (&'a Path, &'a RenderableFeature)>,
    zoom_dependent: &mut Vec<ZoomDependentEntry>,
    style_index: usize,
) {
    push_zoom_dependent(matched, zoom_dependent, style_index);
}

fn push_zoom_dependent<'a>(
    matched: impl Iterator<Item = (&'a Path, &'a RenderableFeature)>,
    zoom_dependent: &mut Vec<ZoomDependentEntry>,
    style_index: usize,
) {
    let mut builder = PathBuilder::new();
    let mut any = false;
    for (path, _) in matched {
        builder.add_path(path, None);
        any = true;
    }
    if any {
        zoom_dependent.push(ZoomDependentEntry {
            style_index,
            path: builder.detach(),
        });
    }
}

/// Applies a style layer's `opacity` (e.g. `fill-opacity`) by scaling the resolved color's alpha —
/// Skia has no separate opacity knob on `Paint` beyond the color's own alpha channel.
pub fn apply_opacity(color: Color, opacity: f32) -> Color {
    if opacity >= 1.0 {
        return color;
    }
    Color::from_argb(
        (color.a() as f32 * opacity.clamp(0.0, 1.0)).round() as u8,
        color.r(),
        color.g(),
        color.b(),
    )
}

/// A hashable stand-in for [`Color`] (which doesn't implement `Hash`), used to batch features by
/// their resolved color.
fn color_key(color: Color) -> u32 {
    u32::from_be_bytes([color.a(), color.r(), color.g(), color.b()])
}

/// Combines same-colored paths into one path per color, so a style layer with many features that
/// happen to resolve to the same color (the common case — most features in a layer share a
/// style, only a few use per-feature `Match` expressions) needs only a handful of `draw_path`
/// calls instead of one per feature. Skia's path scan-conversion (anti-aliased fill/stroke
/// rasterization) cost scales with the number of `draw_path` calls, not just total geometry.
fn batch_paths_by_color<'a>(
    paths_and_colors: impl Iterator<Item = (&'a Path, Color)>,
) -> Vec<(Color, Path)> {
    let mut batches: FxHashMap<u32, (Color, PathBuilder)> = FxHashMap::default();

    for (path, color) in paths_and_colors {
        batches
            .entry(color_key(color))
            .or_insert_with(|| (color, PathBuilder::new()))
            .1
            .add_path(path, None);
    }

    batches
        .into_values()
        .map(|(color, mut builder)| (color, builder.detach()))
        .collect()
}
