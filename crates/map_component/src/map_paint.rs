use std::{cell::RefCell, rc::Rc, sync::Arc, time};

use freya::{
    engine::prelude::{FilterMode, Image, MipmapMode, Paint, PaintStyle, Rect, SamplingOptions},
    prelude::*,
};
use torin::geometry::{CursorPoint, Size2D};

use crate::style::{StyleLayer, StyleSpec};

use super::util::{
    cache::{
        TileCache, TileState, ZoomDepCache, cell_children, cell_level, cell_lookup,
        find_loaded_ancestor, visible_cells, zoom_dep_bucket, zoom_dep_lookup,
    },
    labels::{self, IconCandidate, LabelAnimator, PlacedLabel},
    renderer::CompiledTile,
    sprites::SpriteSheet,
    tiles::TileKey,
    viewport::Viewport,
};

/// Target frame time. The label pass draws fewer labels once a frame has already spent this much
/// (see `labels::draw_labels`'s `frame_hot`); doubles as the pan-update throttle interval (see
/// `last_pan_update` in `map_input`).
pub const FRAME_BUDGET: time::Duration = time::Duration::from_millis(16);

/// Draws one full frame: background, then every visible tile — compiled bitmap plus its baked
/// line overlay, over-zoom cells, or ancestor placeholder, whichever the tile's state allows —
/// then labels. Read-only over the caches (aside from LRU recency bookkeeping); anything that
/// *spawns* work must run from input handlers instead (see `cache::ensure_tile_bakes`).
pub fn paint_map(
    context: &mut FillRenderContext,
    tile_cache: &TileCache,
    zoom_dep_cache: &ZoomDepCache,
    style: &Arc<StyleSpec>,
    center: CursorPoint,
    zoom_val: f32,
    placed_labels: &RefCell<Vec<PlacedLabel>>,
    label_animator: &RefCell<LabelAnimator>,
    sprite_sheet: Option<Rc<SpriteSheet>>,
    platform: &PlatformWindow,
    hovered_screen: Option<CursorPoint>,
    // World position (see `Viewport::screen_to_world`) of a search-result pin to draw, when the
    // selected place isn't an already-visible POI icon — see `MapController::marker_world`.
    marker_world: Option<(f64, f64)>,
    route_overlay: Option<&crate::overlay::RouteOverlay>,
) {
    let time_start = time::Instant::now();
    let zoom = zoom_val as f64;
    let viewport = Viewport::new(center, zoom_val, style.max_real_zoom);

    for layer in &style.layers {
        if let StyleLayer::Background(bg) = layer {
            let mut paint = Paint::default();
            paint.set_color(bg.color);
            context.canvas.draw_rect(
                Rect::from_wh(context.size.width, context.size.height),
                &paint,
            );
        }
    }

    let (tile_x_min, tile_x_max, tile_y_min, tile_y_max) =
        viewport.tile_range(Size2D::new(context.size.width, context.size.height));
    let image_paint = Paint::default();

    // `get` (not `peek`) so tiles currently on screen count as recently used and aren't the first
    // evicted once the cache is full.
    let mut cache = tile_cache.borrow_mut();

    let mut tiles_time = time::Duration::ZERO;
    let mut tiles_drawn = 0usize;

    // Loaded tiles kept alive past the tile loop so their labels can feed the label pass (the
    // `Rc` clone is cheap; the borrow of `cache` ends with the loop). Tuple: (tile, screen_x,
    // screen_y, on-screen tile size) — the size is per-entry because ancestor tiles standing in
    // for still-loading ones are drawn `2^levels` larger than the current zoom's tiles.
    let mut label_tiles: Vec<(Rc<CompiledTile>, f32, f32, f32)> = Vec::new();

    // Ancestors contributing labels this frame — several pending tiles share one ancestor, and
    // its labels must be pushed only once.
    let mut label_ancestors_seen: rustc_hash::FxHashSet<super::util::tiles::TileKey> =
        rustc_hash::FxHashSet::default();

    for tile_y in tile_y_min..tile_y_max {
        for tile_x in tile_x_min..tile_x_max {
            let Some(key) = viewport.tile_key(tile_x, tile_y) else {
                continue;
            };
            let (screen_x, screen_y) = viewport.screen_pos(tile_x, tile_y);
            let time_start_tile = time::Instant::now();

            context.canvas.save();
            context.canvas.translate((screen_x, screen_y));

            let mut path_tag = "none";
            // Per-phase timing so a slow tile can be attributed: the main draw (blit/replay) vs
            // the per-frame `zoom_dependent` overlay, plus the picture's op count when the
            // expensive live-vector path runs (correlates replay cost with tile density).
            let mut zoom_dep_time = time::Duration::ZERO;
            let mut zoom_dep_count = 0usize;
            let mut zoom_dep_tag = "none";
            let mut cell_stats: Option<CellStats> = None;

            match cache.get(&key) {
                Some(TileState::Loaded(tile)) => {
                    // Own the `Rc` (cheap) instead of holding `tile` borrowed from `cache` — the
                    // heavy-overzoom branch below needs `&mut cache` again (for
                    // `find_loaded_ancestor`'s fallback lookup), which would otherwise conflict
                    // with this same borrow still being alive for the `zoom_dependent` loop later
                    // in this arm.
                    let tile = tile.clone();
                    if !tile.labels.is_empty() || !tile.pois.is_empty() {
                        label_tiles.push((tile.clone(), screen_x, screen_y, viewport.tile_px));
                    }
                    if let Some(level) = cell_level(viewport.tile_px) {
                        // Over-zoom: drawn from baked cells, base and lines together — see
                        // `cache::cell_level`.
                        path_tag = "cells";
                        cell_stats = Some(draw_cells(
                            context,
                            &tile,
                            key,
                            level,
                            zoom_dep_bucket(zoom_val),
                            (screen_x, screen_y),
                            viewport.tile_px,
                            &image_paint,
                            zoom_dep_cache,
                        ));
                    } else {
                        // Common case: many tiles on screen, moderate zoom — blitting the
                        // pre-rasterized bitmap (scaled to the tile's current on-screen size via
                        // this destination rect) is far cheaper than re-rasterizing every style
                        // layer's paths every frame. Linear sampling keeps continuous zoom's
                        // fractional scaling smooth rather than blocky.
                        path_tag = "bitmap-blit";
                        blit_tile_image(context, &tile.image, viewport.tile_px, &image_paint);

                        // Lines (and the rare zoom-dependent-color layer) aren't in `tile.image`:
                        // they're baked per zoom bucket by `cache::ensure_tile_bakes`.
                        let zoom_dep_start = time::Instant::now();
                        zoom_dep_count = tile.zoom_dependent.len();
                        zoom_dep_tag = draw_zoom_dependent(
                            context,
                            &tile,
                            key,
                            zoom,
                            zoom_dep_cache,
                            viewport.tile_px,
                        );
                        zoom_dep_time = zoom_dep_start.elapsed();
                    }
                }
                // Not loaded yet (or failed) — fall back to the nearest already-loaded ancestor
                // tile, cropped to this tile's quadrant and stretched to fill, so zooming in shows
                // a blurry-but-correct preview instead of a blank flash while the real tile
                // fetches. No `zoom_dependent` overlay here: the ancestor's per-feature paths
                // don't correspond to this tile's exact geometry, so it's skipped until the real
                // tile replaces this stand-in.
                _ => {
                    if let Some((ancestor, levels)) =
                        find_loaded_ancestor(&mut cache, viewport.zoom_z, key.1, key.2)
                    {
                        path_tag = "unloaded-ancestor";
                        draw_ancestor_fallback(
                            context,
                            &ancestor.image,
                            key.1,
                            key.2,
                            levels,
                            viewport.tile_px,
                            &image_paint,
                        );

                        // Borrow the ancestor's *labels* too: without this, crossing an integer
                        // zoom level makes every label vanish for the moment all tiles are
                        // pending, then reappear once compiled — visible flicker on zoom-in.
                        // The ancestor's places are the same places; the label pass's dedup and
                        // collision absorb any overlap with already-loaded neighbors' labels.
                        let ancestor_key =
                            (viewport.zoom_z - levels, key.1 >> levels, key.2 >> levels);
                        if !ancestor.labels.is_empty() && label_ancestors_seen.insert(ancestor_key)
                        {
                            // The ancestor spans 2^levels current tiles; anchor its top-left at
                            // the screen position of the first tile it covers.
                            let (anc_screen_x, anc_screen_y) = viewport.screen_pos(
                                ((key.1 >> levels) as i64) << levels,
                                ((key.2 >> levels) as i64) << levels,
                            );
                            label_tiles.push((
                                ancestor,
                                anc_screen_x,
                                anc_screen_y,
                                viewport.tile_px * (1 << levels) as f32,
                            ));
                        }
                    }
                }
            }

            context.canvas.restore();

            let tile_time = time_start_tile.elapsed();
            tiles_time += tile_time;
            tiles_drawn += 1;
            let dt_tile = tile_time.as_millis();
            if dt_tile > 1 && log::log_enabled!(log::Level::Debug) {
                // `draw` is the main path (blit / picture replay / ancestor); `zoom-dep` is the
                // per-frame overlay — a slow "bitmap-blit" tile with high zoom-dep time means the
                // overlay paths, not the blit, are the cost.
                let draw_ms = (tile_time - zoom_dep_time).as_millis();
                let zoom_dep_ms = zoom_dep_time.as_millis();
                let detail = match cell_stats {
                    Some(c) => format!(
                        "{} cells: exact {}, stale {}, ancestor {}, missing {}",
                        c.exact + c.stale + c.ancestor + c.missing,
                        c.exact,
                        c.stale,
                        c.ancestor,
                        c.missing
                    ),
                    None => format!(
                        "draw {draw_ms} ms, zoom-dep {zoom_dep_tag} {zoom_dep_ms} ms / {zoom_dep_count} layers"
                    ),
                };
                log::debug!(
                    "  [tile {tile_x},{tile_y},{zoom:.3}] {path_tag}: {dt_tile} ms ({detail})"
                );
            }
        }
    }

    // Trip/journey overlay: over the tiles, under the labels so stop and street names stay
    // readable.
    if let Some(overlay) = route_overlay {
        crate::overlay::draw_overlay(context.canvas, overlay, &viewport);
    }

    // Label pass: screen-space text over the finished tiles. `drop(cache)` first — the pass
    // doesn't need the tile cache, and holding the borrow would be a needless hazard.
    drop(cache);
    let labels_start = time::Instant::now();
    let frame_hot = time_start.elapsed() >= FRAME_BUDGET;
    let mut candidates = Vec::new();

    for (tile, tile_screen_x, tile_screen_y, tile_px) in &label_tiles {
        labels::collect_candidates(
            &tile.labels,
            style,
            zoom,
            *tile_screen_x,
            *tile_screen_y,
            *tile_px,
            context.size.width,
            context.size.height,
            None,
            &mut candidates,
        );
    }

    // POI icon candidates: same projection as labels, only at street-level zooms.
    let mut icon_candidates: Vec<IconCandidate> = Vec::new();
    if zoom >= style.icon_min_zoom && sprite_sheet.is_some() {
        for (tile, tile_screen_x, tile_screen_y, tile_px) in &label_tiles {
            for poi in &tile.pois {
                let screen_x = tile_screen_x + poi.x * tile_px;
                let screen_y = tile_screen_y + poi.y * tile_px;
                if screen_x < -32.0
                    || screen_y < -32.0
                    || screen_x > context.size.width + 32.0
                    || screen_y > context.size.height + 32.0
                {
                    continue;
                }
                icon_candidates.push(IconCandidate {
                    poi,
                    screen_x,
                    screen_y,
                });
            }
        }
    }

    let candidate_count = candidates.len();
    // Identities placed last frame: they win ties this frame so the visible set stays stable
    // through continuous zoom/pan instead of churning.
    let previously_placed: rustc_hash::FxHashSet<u64> = placed_labels
        .borrow()
        .iter()
        .map(|placed| labels::label_identity(&placed.text, placed.style_index))
        .collect();
    let mut animator = label_animator.borrow_mut();
    let placed = labels::draw_labels(
        context.canvas,
        style,
        candidates,
        zoom,
        frame_hot,
        &previously_placed,
        &mut animator,
        &viewport,
        icon_candidates,
        sprite_sheet.as_deref(),
    );
    // Fades in flight need further frames — keep repainting until they settle (opacities clamp,
    // so this always terminates).
    if animator.is_animating() {
        platform.send(UserEvent::RequestRedraw);
    }
    drop(animator);

    // Hover highlight: a ring just outside the pin whichever icon the cursor was last found over
    // (computed by `on_mouse_move` against the *previous* frame's `placed_labels` — one frame
    // stale, same as the tap hit-test, and imperceptible since the pointer moves far slower than
    // paint runs). Stroked, not filled, so it sits around the opaque pin circle rather than
    // covering it.
    if let Some(pos) = hovered_screen
        && let Some(hit) = placed.iter().rev().find(|p| {
            p.sprite.is_some()
                && pos.x as f32 >= p.rect.left
                && pos.x as f32 <= p.rect.right
                && pos.y as f32 >= p.rect.top
                && pos.y as f32 <= p.rect.bottom
        })
    {
        let mut halo_paint = Paint::default();
        halo_paint.set_style(PaintStyle::Stroke);
        halo_paint.set_stroke_width(3.0);
        halo_paint.set_color(Color::from_argb(180, 66, 62, 82));
        halo_paint.set_anti_alias(true);
        let center = (hit.rect.center_x(), hit.rect.center_y());
        context
            .canvas
            .draw_circle(center, labels::ICON_PIN_RADIUS_PX + 3.0, &halo_paint);
    }

    // Search-result pin: a dropped marker for a geocoded place that has no icon already drawn on
    // the map (see `MapController::marker_world`). Recomputed from world coordinates every
    // frame, same as label ghosts, so it tracks pan/zoom without needing its own state.
    if let Some((world_x, world_y)) = marker_world {
        let (screen_x, screen_y) = viewport.world_to_screen(world_x, world_y);
        draw_search_pin(context, screen_x, screen_y);
    }

    let labels_ms = labels_start.elapsed().as_millis();
    let placed_count = placed.len();
    *placed_labels.borrow_mut() = placed;

    let dt = time_start.elapsed().as_millis();
    if dt > 1 && log::log_enabled!(log::Level::Debug) {
        // `other` = frame time not spent inside per-tile drawing: background rect, tile-range
        // math, cache borrows, and any canvas overhead outside the per-tile save/restore blocks.
        let other_ms = dt
            .saturating_sub(tiles_time.as_millis())
            .saturating_sub(labels_ms);
        log::debug!(
            "Rendering done, {dt} ms ({tiles_drawn} tiles {} ms, \
             labels {placed_count}/{candidate_count} {labels_ms} ms, other {other_ms} ms)",
            tiles_time.as_millis()
        );
    }
}

/// Teardrop-style pin for a dropped search-result marker — deliberately distinct from the round
/// POI pin (`labels::ICON_PIN_*`) so a geocoded address never looks like an already-cataloged
/// business.
fn draw_search_pin(context: &mut FillRenderContext, screen_x: f32, screen_y: f32) {
    const RADIUS: f32 = 9.0;

    let mut fill = Paint::default();
    fill.set_anti_alias(true);
    fill.set_color(Color::from_rgb(217, 48, 37));
    context
        .canvas
        .draw_circle((screen_x, screen_y - RADIUS), RADIUS, &fill);

    let mut point = freya::engine::prelude::PathBuilder::new();
    point.move_to((screen_x - RADIUS * 0.62, screen_y - RADIUS * 0.55));
    point.line_to((screen_x + RADIUS * 0.62, screen_y - RADIUS * 0.55));
    point.line_to((screen_x, screen_y + RADIUS * 0.7));
    point.close();
    context.canvas.draw_path(&point.detach(), &fill);

    let mut hole = Paint::default();
    hole.set_anti_alias(true);
    hole.set_color(Color::WHITE);
    context
        .canvas
        .draw_circle((screen_x, screen_y - RADIUS), RADIUS * 0.4, &hole);
}

/// Draws `tile`'s `zoom_dependent` layers by reading `zoom_dep_cache` — never spawns work itself
/// (that's `cache::ensure_tile_bakes`'s job, called from input handlers and on tile
/// load). Blits the current zoom bucket's bake when ready, otherwise the nearest ready bucket's
/// stretched (line widths off by a pixel or two until the exact bake lands). Never strokes the
/// paths live: that cost 30–110ms *per tile* on dense tiles, stalling whole frames for every
/// bucket a zoom gesture entered before its bake finished. With no bake at all yet (a tile that
/// just loaded), the overlay is skipped for that frame — its bake was already kicked off on load
/// and requests a redraw when done. Returns a short tag for the per-tile timing log.
fn draw_zoom_dependent(
    context: &mut FillRenderContext,
    tile: &CompiledTile,
    key: TileKey,
    zoom: f64,
    zoom_dep_cache: &ZoomDepCache,
    tile_px: f32,
) -> &'static str {
    if tile.zoom_dependent.is_empty() {
        return "none";
    }

    let Some((image, stale)) = zoom_dep_lookup(zoom_dep_cache, key, zoom_dep_bucket(zoom as f32))
    else {
        return "missing";
    };

    blit_tile_image(context, &image, tile_px, &Paint::default());
    if stale { "stale" } else { "exact" }
}

/// How one over-zoomed tile's visible cells were drawn this frame, for the timing log.
#[derive(Default, Clone, Copy)]
struct CellStats {
    exact: usize,
    stale: usize,
    ancestor: usize,
    missing: usize,
}

/// Draws an over-zoomed tile's visible cells (canvas already translated to the tile's screen
/// origin). Each cell uses the best ready bake `cache::cell_lookup` finds — exact, another zoom
/// bucket's, or a coarser ancestor cell cropped to it. With none at all, the tile's compiled
/// bitmap stands in (blurry, no lines), overlaid with any finer child cells already baked; the
/// real bake was requested by the input handler that changed the view and redraws when done.
#[allow(clippy::too_many_arguments)]
fn draw_cells(
    context: &mut FillRenderContext,
    tile: &CompiledTile,
    key: TileKey,
    level: u32,
    bucket: i32,
    (screen_x, screen_y): (f32, f32),
    tile_px: f32,
    paint: &Paint,
    zoom_dep_cache: &ZoomDepCache,
) -> CellStats {
    let mut stats = CellStats::default();
    let cell = tile_px / (1u32 << level) as f32;
    let viewport_size = Size2D::new(context.size.width, context.size.height);
    for (cx, cy) in visible_cells(screen_x, screen_y, tile_px, level, viewport_size) {
        let dest = Rect::from_xywh(cx as f32 * cell, cy as f32 * cell, cell, cell);
        context.canvas.save();
        context.canvas.clip_rect(dest, None, None);
        match cell_lookup(zoom_dep_cache, key, bucket, level, cx, cy) {
            Some(hit) => {
                if hit.level < level {
                    stats.ancestor += 1;
                } else if hit.stale {
                    stats.stale += 1;
                } else {
                    stats.exact += 1;
                }
                // The found cell's own on-screen rect; for an ancestor, larger than `dest` and
                // cropped by the clip.
                let hit_size = cell * (1u32 << (level - hit.level)) as f32;
                draw_image_at(
                    context,
                    &hit.image,
                    Rect::from_xywh(
                        hit.cx as f32 * hit_size,
                        hit.cy as f32 * hit_size,
                        hit_size,
                        hit_size,
                    ),
                    paint,
                );
            }
            None => {
                stats.missing += 1;
                blit_tile_image(context, &tile.image, tile_px, paint);
                let child = cell / 2.0;
                for (image, x, y) in cell_children(zoom_dep_cache, key, bucket, level, cx, cy) {
                    draw_image_at(
                        context,
                        &image,
                        Rect::from_xywh(x as f32 * child, y as f32 * child, child, child),
                        paint,
                    );
                }
            }
        }
        context.canvas.restore();
    }
    stats
}

/// Draws `image` stretched into `dest`, with linear sampling.
fn draw_image_at(context: &mut FillRenderContext, image: &Image, dest: Rect, paint: &Paint) {
    context.canvas.draw_image_rect_with_sampling_options(
        image,
        None,
        dest,
        SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
        paint,
    );
}

/// Blits a tile bitmap stretched to the tile's current on-screen size, with linear sampling so
/// continuous zoom's fractional scaling stays smooth rather than blocky.
fn blit_tile_image(context: &mut FillRenderContext, image: &Image, tile_px: f32, paint: &Paint) {
    context.canvas.draw_image_rect_with_sampling_options(
        image,
        None,
        Rect::from_wh(tile_px, tile_px),
        SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
        paint,
    );
}

/// Draws `image` (an ancestor tile `levels` zoom levels coarser than the tile actually being
/// painted) cropped to the quadrant `(tile_x, tile_y)` occupies within that ancestor, stretched to
/// fill a `tile_px`-sized square — the same "zoomed-in blurry parent" placeholder every slippy map
/// viewer shows while a finer tile is still loading. Achieved by drawing the *whole* ancestor
/// image scaled up by `2^levels` (so one quadrant fills the destination) and clipping to the
/// destination rect, rather than via `draw_image_rect`'s `src` cropping parameter — the vendored
/// freya's `freya_engine::prelude` doesn't currently re-export `SrcRectConstraint`.
fn draw_ancestor_fallback(
    context: &mut FillRenderContext,
    image: &Image,
    tile_x: u32,
    tile_y: u32,
    levels: u32,
    tile_px: f32,
    paint: &Paint,
) {
    let scale = (1u32 << levels) as f32;
    let mask = (1u32 << levels) - 1;
    let full_size = tile_px * scale;
    let offset_x = (tile_x & mask) as f32 * tile_px;
    let offset_y = (tile_y & mask) as f32 * tile_px;

    context.canvas.save();
    context
        .canvas
        .clip_rect(Rect::from_wh(tile_px, tile_px), None, None);
    context.canvas.draw_image_rect_with_sampling_options(
        image,
        None,
        Rect::from_xywh(-offset_x, -offset_y, full_size, full_size),
        SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
        paint,
    );
    context.canvas.restore();
}
