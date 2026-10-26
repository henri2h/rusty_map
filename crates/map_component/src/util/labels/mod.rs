//! Screen-space text-label pass: collects label candidates from visible compiled tiles, sorts by
//! style priority, deduplicates tile-boundary splits, resolves collisions, and draws halo+fill
//! text blobs. Runs at the end of every paint frame — bounded by a hard candidate cap and a
//! frame-budget bail-out so labels can only degrade, never stall the frame.
//!
//! Interactivity roadmap hooks: the pass returns the labels it actually placed (screen rects +
//! feature properties) for hit-testing, and takes an optional visibility predicate so a future
//! search box ("bakery") can filter symbols without touching this module.

mod animator;
mod fonts;

use std::time;

use freya::engine::prelude::{
    Canvas, Font, Image, Paint, PaintStyle, Rect, SamplingOptions, TextBlob,
};
use rustc_hash::FxHashSet;

use crate::style::{StyleLayer, StyleSpec, SymbolLayer, TextPlacement};

use super::{
    renderer::{CompiledLabel, CompiledPoi, LabelAnchor, Properties},
    sprites::SpriteSheet,
    viewport::Viewport,
};

pub use animator::LabelAnimator;
use animator::{AnimEntry, Ghost, GhostKind, TextMetrics, fxhash_str, icon_identity};
use fonts::{make_font, strip_unsupported_glyphs, typefaces};

/// Hard cap on labels drawn per frame — backstop for the frame budget.
const MAX_LABELS_PER_FRAME: usize = 200;

/// Reduced cap when the frame is already hot by the time the label pass starts (tiles ran long):
/// the highest-priority labels still draw, the rest wait for a calmer frame.
const MAX_LABELS_HOT_FRAME: usize = 60;

/// Cell size (px) of the cross-tile dedup grid: the same street/city name within one cell is
/// drawn once (street names split at tile boundaries, city labels repeat in adjacent tiles).
const DEDUP_CELL_PX: f32 = 64.0;

/// Max labels placed per style layer per `LAYER_CELL_PX` screen cell. Combined with each layer's
/// sort key this keeps the *important* instances (biggest towns by population) and stops one
/// dense layer (village names around a metropolis) from flooding the screen and starving every
/// layer placed after it. Per cell rather than per screen: a screen-wide count let whichever
/// labels were placed first (the first tiles to load, top-left, since the fetch loop starts
/// there) fill every slot and then keep them through placement persistence, leaving the rest of
/// the map unlabeled.
const MAX_PER_LAYER_CELL: usize = 2;

/// Cell size (px) for `MAX_PER_LAYER_CELL`.
const LAYER_CELL_PX: f32 = 256.0;

/// Collision padding around point labels (place/city names) — enforces breathing room between
/// them instead of packing names edge to edge.
const POINT_PADDING_PX: f32 = 28.0;

/// Collision padding around line-placed (street) labels — smaller, streets tolerate density.
const LINE_PADDING_PX: f32 = 8.0;

const FADE_IN: time::Duration = time::Duration::from_millis(150);
const FADE_OUT: time::Duration = time::Duration::from_millis(220);

/// POI icons only make sense at street-level zooms. Default for `StyleSpec::icon_min_zoom`.
pub const ICON_MIN_ZOOM: f64 = 14.0;

/// On-screen icon size (sprites are 32px sources).
const ICON_SIZE_PX: f32 = 20.0;

/// Icons placed after all text (lowest priority) with their own cap and spacing.
const MAX_ICONS_PER_FRAME: usize = 64;
const ICON_PADDING_PX: f32 = 6.0;

/// Radius of the pin-style backdrop circle drawn behind each icon, exported so
/// `map_component`'s hover ring can size itself to sit just outside it.
pub const ICON_PIN_RADIUS_PX: f32 = ICON_SIZE_PX / 2.0 + 6.0;
const ICON_PIN_FILL: freya::engine::prelude::Color =
    freya::engine::prelude::Color::from_argb(235, 255, 255, 255);
const ICON_PIN_STROKE: freya::engine::prelude::Color =
    freya::engine::prelude::Color::from_argb(90, 66, 62, 82);

/// A label candidate projected to screen space, ready for sorting/collision.
#[derive(Clone, Copy)]
pub struct LabelCandidate<'a> {
    pub label: &'a CompiledLabel,
    /// Screen position of the anchor, in canvas pixels.
    pub screen_x: f32,
    pub screen_y: f32,
    /// Rotation for line-placed text (radians); `0.0` for point labels.
    pub angle: f32,
    /// On-screen length of the polyline available for line-placed text; `f32::MAX` for points.
    pub avail_px: f32,
}

/// A label that was actually drawn this frame — retained for hit-testing (hover) and future
/// interactivity. `rect` is the screen-space AABB used for collision.
pub struct PlacedLabel {
    pub rect: Rect,
    pub text: String,
    /// `usize::MAX` for POI icons (they belong to no style layer).
    pub style_index: usize,
    /// Sprite name when this placement is a POI icon rather than text.
    pub sprite: Option<String>,
    /// See `renderer::RenderableFeature::id`'s doc comment.
    pub feature_id: Option<u64>,
    pub properties: std::sync::Arc<Properties>,
}

/// A POI icon candidate projected to screen space.
#[derive(Clone, Copy)]
pub struct IconCandidate<'a> {
    pub poi: &'a CompiledPoi,
    pub screen_x: f32,
    pub screen_y: f32,
}

/// Optional per-frame symbol visibility predicate (layer + feature properties); the future
/// search filter sets this and requests a redraw.
pub type SymbolFilter = dyn Fn(&SymbolLayer, &Properties) -> bool;

/// A normalized tile-local position inside the tile proper, not its buffer.
pub fn in_tile(x: f32, y: f32) -> bool {
    (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y)
}

/// Collects candidates for one visible tile's labels. Called from the paint loop's tile loop
/// (`Loaded` arm) so no second cache walk/borrow is needed. `zoom` is the continuous zoom —
/// the exact style gate runs here, per-frame.
pub fn collect_candidates<'a>(
    labels: &'a [CompiledLabel],
    style: &StyleSpec,
    zoom: f64,
    tile_screen_x: f32,
    tile_screen_y: f32,
    tile_px: f32,
    viewport_w: f32,
    viewport_h: f32,
    visibility: Option<&SymbolFilter>,
    out: &mut Vec<LabelCandidate<'a>>,
) {
    const CULL_PAD: f32 = 200.0;

    for label in labels {
        let Some(StyleLayer::Symbol(symbol_layer)) = style.layers.get(label.style_index) else {
            continue;
        };
        if zoom < symbol_layer.min_zoom || zoom > symbol_layer.max_zoom {
            continue;
        }
        if let Some(filter) = visibility
            && !filter(symbol_layer, &label.properties)
        {
            continue;
        }

        let mut push = |x: f32, y: f32, angle: f32, avail_px: f32| {
            // Only anchors inside the tile itself: its buffer repeats what lies just past its
            // edge — the neighbour tile's own labels, or at z0 the world wrapped around, whose
            // labels then showed beside the single world tile with no map under them.
            if !in_tile(x, y) {
                return;
            }
            let screen_x = tile_screen_x + x * tile_px;
            let screen_y = tile_screen_y + y * tile_px;
            if screen_x < -CULL_PAD
                || screen_y < -CULL_PAD
                || screen_x > viewport_w + CULL_PAD
                || screen_y > viewport_h + CULL_PAD
            {
                return;
            }
            out.push(LabelCandidate {
                label,
                screen_x,
                screen_y,
                angle,
                avail_px,
            });
        };

        match &label.anchor {
            LabelAnchor::Point { x, y } => push(*x, *y, 0.0, f32::MAX),
            LabelAnchor::Line {
                x,
                y,
                angle,
                len_norm,
                chain,
            } => {
                if !repeat_along_line(chain, *len_norm, tile_px, &mut push) {
                    push(*x, *y, *angle, len_norm * tile_px);
                }
            }
        }
    }
}

/// Target on-screen distance between repeated labels of one street.
const LINE_REPEAT_PX: f32 = 600.0;

/// Repeats a line label along `chain` (normalized tile coords) once the street is long enough on
/// screen to fit at least two copies — at heavy over-zoom the single midpoint anchor of a tile
/// spanning many screens is almost always off screen, so streets lost their names entirely.
/// Anchors sit at whole multiples of a power-of-two fraction of the tile (`step`), so halving the
/// spacing on zoom-in keeps every existing anchor and only adds new ones in between — labels
/// don't slide along the street during a zoom gesture. Returns `false` (caller falls back to
/// the midpoint) when the street is too short on screen for that.
fn repeat_along_line(
    chain: &[(f32, f32)],
    len_norm: f32,
    tile_px: f32,
    push: &mut impl FnMut(f32, f32, f32, f32),
) -> bool {
    if tile_px <= 0.0 || len_norm * tile_px < 2.0 * LINE_REPEAT_PX {
        return false;
    }
    // Largest power-of-two fraction of the tile that's still <= LINE_REPEAT_PX on screen.
    let step = 2f32.powi(-((tile_px / LINE_REPEAT_PX).log2().ceil() as i32));
    let avail_px = step * tile_px;

    let mut next = step;
    let mut walked = 0.0;
    for w in chain.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        let seg = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        if seg <= 0.0 {
            continue;
        }
        let angle = crate::util::renderer::readable_angle((y1 - y0).atan2(x1 - x0));
        while next <= walked + seg && next < len_norm {
            let t = (next - walked) / seg;
            push(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, angle, avail_px);
            next += step;
        }
        walked += seg;
    }
    true
}

/// Sprite lookup for POIs, memoized per distinct kind for one frame (`SpriteSheet::resolve`
/// formats several candidate names per call): the specific kind's own sprite, else its coarser
/// class's (see `CompiledPoi::class`).
struct SpriteResolver<'a> {
    sheet: Option<&'a SpriteSheet>,
    memo: rustc_hash::FxHashMap<(&'a str, Option<&'a str>), Option<(&'a str, Rect)>>,
}

impl<'a> SpriteResolver<'a> {
    fn new(sheet: Option<&'a SpriteSheet>) -> Self {
        Self {
            sheet,
            memo: Default::default(),
        }
    }

    fn resolve(&mut self, poi: &'a CompiledPoi) -> Option<(&'a str, Rect)> {
        let sheet = self.sheet?;
        *self
            .memo
            .entry((poi.kind.as_str(), poi.class.as_deref()))
            .or_insert_with(|| sheet.resolve_poi(&poi.kind, poi.class.as_deref()))
    }
}

/// Gap between a POI icon's pin and the name below it.
const ICON_TEXT_GAP_PX: f32 = 2.0;

/// An icon's drawn rect and its padded collision rect, centered on its anchor.
fn icon_rects(candidate: &IconCandidate<'_>) -> (Rect, Rect) {
    let half = ICON_SIZE_PX / 2.0;
    let rect = Rect::from_ltrb(
        candidate.screen_x - half,
        candidate.screen_y - half,
        candidate.screen_x + half,
        candidate.screen_y + half,
    );
    (rect, rect.with_outset((ICON_PADDING_PX, ICON_PADDING_PX)))
}

/// Draws one placed POI icon (pin backdrop + sprite) with its fade-in, and returns its hit-test
/// entry.
#[allow(clippy::too_many_arguments)]
fn draw_icon(
    canvas: &Canvas,
    sheet: &SpriteSheet,
    candidate: &IconCandidate<'_>,
    sprite_name: &str,
    src: Rect,
    rect: Rect,
    animator: &mut LabelAnimator,
    viewport: &Viewport,
    fade_in_step: f32,
    icon_paint: &mut Paint,
) -> PlacedLabel {
    // Icon fade-in, same animator as text (identity from sprite + world position, both stable
    // across frames/panning).
    let (world_x, world_y) = viewport.screen_to_world(candidate.screen_x, candidate.screen_y);
    let identity = icon_identity(sprite_name, world_x, world_y);
    let entry = animator
        .entries
        .entry(identity)
        .or_insert_with(|| AnimEntry {
            opacity: 0.0,
            placed_this_frame: true,
            ghost: Ghost {
                world_x,
                world_y,
                angle: 0.0,
                kind: GhostKind::Icon {
                    sprite: sprite_name.to_string(),
                },
            },
        });
    entry.opacity = (entry.opacity + fade_in_step).min(1.0);
    entry.placed_this_frame = true;
    entry.ghost.world_x = world_x;
    entry.ghost.world_y = world_y;
    icon_paint.set_alpha_f(entry.opacity);

    draw_pin_circle(
        canvas,
        candidate.screen_x,
        candidate.screen_y,
        entry.opacity,
    );
    draw_image_cropped(
        canvas,
        &sheet.image,
        src,
        Rect::from_xywh(
            rect.left.round(),
            rect.top.round(),
            ICON_SIZE_PX,
            ICON_SIZE_PX,
        ),
        freya::engine::prelude::SamplingOptions::new(
            freya::engine::prelude::FilterMode::Linear,
            freya::engine::prelude::MipmapMode::None,
        ),
        icon_paint,
    );

    PlacedLabel {
        rect,
        text: candidate.poi.name.clone(),
        style_index: usize::MAX,
        sprite: Some(sprite_name.to_string()),
        properties: candidate.poi.properties.clone(),
        feature_id: candidate.poi.feature_id,
    }
}

/// Bound on `LabelAnimator::metrics` — every distinct label seen this session gets an entry;
/// clearing past this just costs one re-measure per label.
const MAX_CACHED_METRICS: usize = 50_000;

/// Reference font size for [`TextMetrics`].
const METRICS_REF_SIZE: f32 = 32.0;

fn measure_text(text: &str, typeface: &freya::engine::prelude::Typeface) -> TextMetrics {
    let font = make_font(typeface.clone(), METRICS_REF_SIZE);
    let stripped = strip_unsupported_glyphs(text, &font);
    let bounds = TextBlob::from_str(&stripped, &font).map(|blob| *blob.bounds());
    TextMetrics {
        stripped: match stripped {
            std::borrow::Cow::Owned(s) => Some(s),
            std::borrow::Cow::Borrowed(_) => None,
        },
        width_per_px: bounds.map_or(0.0, |b| b.width() / METRICS_REF_SIZE),
        height_per_px: bounds.map_or(0.0, |b| b.height() / METRICS_REF_SIZE),
        blob: None,
    }
}

/// Uniform-grid index over placed collision rects, so each candidate checks only the rects near
/// it instead of every label placed so far.
#[derive(Default)]
struct CollisionGrid {
    rects: Vec<Rect>,
    cells: rustc_hash::FxHashMap<(i32, i32), Vec<u32>>,
}

impl CollisionGrid {
    const CELL_PX: f32 = 128.0;

    fn cell_range(rect: Rect) -> impl Iterator<Item = (i32, i32)> {
        let x0 = (rect.left / Self::CELL_PX).floor() as i32;
        let x1 = (rect.right / Self::CELL_PX).floor() as i32;
        let y0 = (rect.top / Self::CELL_PX).floor() as i32;
        let y1 = (rect.bottom / Self::CELL_PX).floor() as i32;
        (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (x, y)))
    }

    fn hits(&self, rect: Rect) -> bool {
        Self::cell_range(rect).any(|cell| {
            self.cells.get(&cell).is_some_and(|indices| {
                indices
                    .iter()
                    .any(|&i| self.rects[i as usize].intersects(rect))
            })
        })
    }

    fn insert(&mut self, rect: Rect) {
        let index = self.rects.len() as u32;
        self.rects.push(rect);
        for cell in Self::cell_range(rect) {
            self.cells.entry(cell).or_default().push(index);
        }
    }
}

/// Stable identity of a label across frames (same name from the same style layer), used for
/// dedup and for placement persistence.
pub fn label_identity(text: &str, style_index: usize) -> u64 {
    fxhash_str(text) ^ (style_index as u64).wrapping_mul(31)
}

/// Maps `value` to an integer that orders exactly like [`f64::total_cmp`], so it can be part of
/// an `Ord` sort key.
fn total_order_key(value: f64) -> i64 {
    let bits = value.to_bits() as i64;
    bits ^ (((bits >> 63) as u64) >> 1) as i64
}

/// Sorts, dedups, collides, and draws this frame's candidates. Returns the labels actually
/// placed (screen rect + properties) for hit-testing. `frame_hot` = the frame already spent its
/// budget on tiles; only the top-priority labels draw. `previously_placed` = identities placed
/// last frame — they win ties so labels don't churn during continuous zoom/pan.
pub fn draw_labels(
    canvas: &Canvas,
    style: &StyleSpec,
    mut candidates: Vec<LabelCandidate<'_>>,
    zoom: f64,
    frame_hot: bool,
    previously_placed: &FxHashSet<u64>,
    animator: &mut LabelAnimator,
    viewport: &Viewport,
    icons: Vec<IconCandidate<'_>>,
    sprites: Option<&SpriteSheet>,
) -> Vec<PlacedLabel> {
    // Fade bookkeeping: opacity advances by real elapsed time (capped so a long pause between
    // frames doesn't teleport a fade).
    let now = time::Instant::now();
    let dt = animator
        .last_frame
        .map(|last| {
            now.duration_since(last)
                .min(time::Duration::from_millis(100))
        })
        .unwrap_or_default();
    animator.last_frame = Some(now);
    let fade_in_step = dt.as_secs_f32() / FADE_IN.as_secs_f32();
    let fade_out_step = dt.as_secs_f32() / FADE_OUT.as_secs_f32();
    for entry in animator.entries.values_mut() {
        entry.placed_this_frame = false;
    }
    // Placement priority, highest first:
    // 1. *Descending* style order — the neutrino style orders symbol layers by rising importance
    //    (streets → suburbs → cities → countries), and later layers render on top in MapLibre.
    //    Ascending order let street names steal collision space from place names as they shifted
    //    during a zoom gesture, blinking e.g. a suburb label in and out.
    // 2. Persistence — a label shown last frame keeps its spot, so equal-priority names don't
    //    flip collision winners every frame (visible as nearby streets cycling).
    // 3. The layer's own sort key (e.g. city population).
    // 4. A stable per-name hash, so ties never fall back to frame-dependent insertion order.
    //
    // Keys computed once per candidate (`sort_by_cached_key`), not per comparison: with ~15-20k
    // candidates a frame, the comparator's hash-set lookups dominated the label pass.
    candidates.sort_by_cached_key(|c| {
        let id = c.label.identity;
        (
            std::cmp::Reverse(c.label.style_index),
            !previously_placed.contains(&id),
            total_order_key(c.label.sort_value),
            id,
        )
    });

    let cap = if frame_hot {
        MAX_LABELS_HOT_FRAME
    } else {
        MAX_LABELS_PER_FRAME
    };

    let (regular, bold) = typefaces();
    let mut fill_paint = Paint::default();
    fill_paint.set_anti_alias(true);
    fill_paint.set_style(PaintStyle::Fill);
    let mut halo_paint = Paint::default();
    halo_paint.set_anti_alias(true);
    // Stroke, not StrokeAndFill: the fill half painted the glyph interiors in (blurred) halo
    // color underneath the text, making all labels look soft.
    halo_paint.set_style(PaintStyle::Stroke);

    let mut placed: Vec<PlacedLabel> = Vec::new();
    // Padded rects used for collision (kept separate from `PlacedLabel::rect`, which stays tight
    // for hover hit-testing).
    let mut collisions = CollisionGrid::default();
    // Unpadded rects of placed text, for the icon pass below.
    let mut text_rects = CollisionGrid::default();
    if animator.metrics.len() > MAX_CACHED_METRICS {
        animator.metrics.clear();
    }
    let mut per_layer_counts: rustc_hash::FxHashMap<(usize, i32, i32), usize> =
        rustc_hash::FxHashMap::default();
    let mut dedup: FxHashSet<(u64, i32, i32)> = FxHashSet::default();
    // Font rebuilt only when (style_index-derived) size/bold changes between consecutive
    // candidates — sorted order groups candidates by layer, so this is per-layer in practice.
    let mut font: Option<(usize, Font)> = None;

    // POI icons by anchor position: a POI's name label and its icon come from the same feature
    // point (bit-identical screen position), and are placed as one symbol — icon on the point,
    // name just below — like MapLibre's icon+text symbols. Placed separately, the icon drew over
    // the centered name, and lost its own collision/cap checks independently, leaving names with
    // no icon. Sprites are only resolved (`sprite_for`) for icons actually considered — there
    // can be thousands of candidates a frame.
    let mut sprite_for = SpriteResolver::new(sprites);
    let icon_at: rustc_hash::FxHashMap<(u32, u32), usize> = icons
        .iter()
        .enumerate()
        .map(|(i, c)| ((c.screen_x.to_bits(), c.screen_y.to_bits()), i))
        .collect();
    let mut icons_placed: FxHashSet<usize> = FxHashSet::default();
    // Icons collide only against other icons (see the icon pass below); POI icons placed with
    // their name here count too.
    let mut icon_collision_rects: Vec<Rect> = Vec::new();
    let mut icon_paint = Paint::default();

    for candidate in &candidates {
        if placed.len() >= cap {
            break;
        }
        let label = candidate.label;
        let Some(StyleLayer::Symbol(symbol_layer)) = style.layers.get(label.style_index) else {
            continue;
        };

        let layer_count = per_layer_counts
            .entry((
                label.style_index,
                (candidate.screen_x / LAYER_CELL_PX).floor() as i32,
                (candidate.screen_y / LAYER_CELL_PX).floor() as i32,
            ))
            .or_insert(0);
        if *layer_count >= MAX_PER_LAYER_CELL {
            continue;
        }

        // Dedup across tile splits: same text + same layer in the same coarse screen cell.
        let text_hash = label.identity;
        let cell = (
            text_hash,
            (candidate.screen_x / DEDUP_CELL_PX) as i32,
            (candidate.screen_y / DEDUP_CELL_PX) as i32,
        );
        if !dedup.insert(cell) {
            continue;
        }

        let size = symbol_layer.text_size.eval(zoom) as f32;
        if size <= 1.0 {
            continue;
        }
        let bold_font = symbol_layer.text_font_bold;
        let metrics = animator
            .metrics
            .entry((text_hash, bold_font))
            .or_insert_with(|| measure_text(&label.text, if bold_font { bold } else { regular }));
        if metrics.width_per_px <= 0.0 {
            continue;
        }
        let text_w = metrics.width_per_px * size;
        let text_h = metrics.height_per_px * size;

        // A POI name with an icon sits just below it (see `icon_at`).
        let icon = match symbol_layer.placement {
            TextPlacement::Point => icon_at
                .get(&(candidate.screen_x.to_bits(), candidate.screen_y.to_bits()))
                .copied()
                .filter(|i| !icons_placed.contains(i))
                .and_then(|i| Some((i, sprite_for.resolve(icons[i].poi)?))),
            TextPlacement::Line => None,
        };
        let (center_x, center_y) = match icon {
            Some(_) => (
                candidate.screen_x,
                candidate.screen_y + ICON_PIN_RADIUS_PX + ICON_TEXT_GAP_PX + text_h / 2.0,
            ),
            None => (candidate.screen_x, candidate.screen_y),
        };

        // Line-placed text must fit the street it labels. Hysteresis: a label already on screen
        // last frame survives a marginally tight fit — without this, the fit ratio drifting
        // across the threshold during continuous zoom makes names blink in and out.
        let fit_factor = if previously_placed.contains(&text_hash) {
            1.05
        } else {
            0.9
        };
        if text_w > candidate.avail_px * fit_factor {
            continue;
        }

        // Conservative screen-space AABB of the (possibly rotated) text, centered on the anchor.
        let (sin, cos) = candidate.angle.sin_cos();
        let half_w = (text_w * cos.abs() + text_h * sin.abs()) / 2.0;
        let half_h = (text_w * sin.abs() + text_h * cos.abs()) / 2.0;
        let rect = Rect::from_ltrb(
            center_x - half_w,
            center_y - half_h,
            center_x + half_w,
            center_y + half_h,
        );
        let padding = match symbol_layer.placement {
            TextPlacement::Point => POINT_PADDING_PX,
            TextPlacement::Line => LINE_PADDING_PX,
        };
        let collision_rect = rect.with_outset((padding, padding));
        if collisions.hits(collision_rect) {
            continue;
        }
        let icon_rects = icon.map(|(i, _)| icon_rects(&icons[i]));
        if let Some((_, icon_collision)) = icon_rects
            && (collisions.hits(icon_collision)
                || icon_collision_rects
                    .iter()
                    .any(|r| r.intersects(icon_collision)))
        {
            continue;
        }

        // Placed: only now shape the text for real (or reuse last frame's, same size).
        let blob = match &metrics.blob {
            Some((bits, blob)) if *bits == size.to_bits() => blob.clone(),
            _ => {
                let font = match &mut font {
                    Some((index, font)) if *index == label.style_index => &*font,
                    slot => {
                        let typeface = if bold_font { bold } else { regular };
                        &slot
                            .insert((label.style_index, make_font(typeface.clone(), size)))
                            .1
                    }
                };
                let text = metrics.stripped.as_deref().unwrap_or(&label.text);
                let Some(blob) = TextBlob::from_str(text, font) else {
                    continue;
                };
                metrics.blob = Some((size.to_bits(), blob.clone()));
                blob
            }
        };
        let bounds = blob.bounds();

        // Fade-in: advance this label's opacity and snapshot its state so it can fade out in
        // place (as a world-anchored ghost) if it loses its spot later.
        let text_color = symbol_layer.text_color.eval(&label.properties, zoom);
        let (world_x, world_y) = viewport.screen_to_world(center_x, center_y);
        let identity = text_hash;
        let entry = animator
            .entries
            .entry(identity)
            .or_insert_with(|| AnimEntry {
                opacity: 0.0,
                placed_this_frame: true,
                ghost: Ghost {
                    world_x,
                    world_y,
                    angle: candidate.angle,
                    kind: GhostKind::Text {
                        text: label.text.clone(),
                        size,
                        bold: symbol_layer.text_font_bold,
                        text_color,
                        halo_color: symbol_layer.halo_color,
                        halo_width: symbol_layer.halo_width,
                    },
                },
            });
        entry.opacity = (entry.opacity + fade_in_step).min(1.0);
        entry.placed_this_frame = true;
        entry.ghost.world_x = world_x;
        entry.ghost.world_y = world_y;
        entry.ghost.angle = candidate.angle;
        if let GhostKind::Text { size: s, .. } = &mut entry.ghost.kind {
            *s = size;
        }
        let opacity = entry.opacity;

        // Draw: halo underneath (pure stroke — StrokeAndFill also painted the glyph *interior*
        // in halo color with blur, fuzzing the text everywhere), then the fill on top.
        fill_paint.set_color(super::renderer::apply_opacity(text_color, opacity));

        canvas.save();
        // Unrotated labels snap to whole pixels — fractional origins smear every glyph edge
        // through anti-aliasing. Rotated street text can't snap (any angle implies fractional
        // coverage anyway).
        let origin = if candidate.angle != 0.0 {
            canvas.translate((center_x, center_y));
            canvas.rotate(candidate.angle.to_degrees(), None);
            (-bounds.center_x(), -bounds.center_y())
        } else {
            canvas.translate((center_x.round(), center_y.round()));
            ((-bounds.center_x()).round(), (-bounds.center_y()).round())
        };

        if symbol_layer.halo_width > 0.0 {
            halo_paint.set_color(super::renderer::apply_opacity(
                symbol_layer.halo_color,
                opacity,
            ));
            halo_paint.set_stroke_width(symbol_layer.halo_width * 2.0);
            canvas.draw_text_blob(&blob, origin, &halo_paint);
        }
        canvas.draw_text_blob(&blob, origin, &fill_paint);
        canvas.restore();

        collisions.insert(collision_rect);
        text_rects.insert(rect);
        *layer_count += 1;
        placed.push(PlacedLabel {
            rect,
            text: label.text.clone(),
            style_index: label.style_index,
            sprite: None,
            properties: label.properties.clone(),
            feature_id: label.feature_id,
        });

        if let (Some((i, (sprite_name, src))), Some((icon_rect, icon_collision)), Some(sheet)) =
            (icon, icon_rects, sprites)
        {
            collisions.insert(icon_collision);
            icon_collision_rects.push(icon_collision);
            icons_placed.insert(i);
            placed.push(draw_icon(
                canvas,
                sheet,
                &icons[i],
                sprite_name,
                src,
                icon_rect,
                animator,
                viewport,
                fade_in_step,
                &mut icon_paint,
            ));
        }
    }

    // Icon pass: POI icons without a placed name (unnamed POIs, or names that lost placement
    // when their icon didn't) place after all text, lowest priority, each blitted from the
    // pre-tinted sprite atlas. Unknown sprite names (a POI kind the sheet has no icon for)
    // simply don't draw.
    // These collide against other icons, and against placed text only where they'd actually
    // cover it (tight text rect, no padding) — drawn on top, they hid names. Yielding to every
    // *padded* text box made them wink out whenever they drifted near a label during panning,
    // looking "repulsed" by labeled areas.
    if let Some(sheet) = sprites {
        for (i, candidate) in icons.iter().enumerate() {
            if icon_collision_rects.len() >= MAX_ICONS_PER_FRAME {
                break;
            }
            if icons_placed.contains(&i) {
                continue;
            }
            let Some((sprite_name, src)) = sprite_for.resolve(candidate.poi) else {
                continue;
            };
            let (rect, collision_rect) = icon_rects(candidate);
            if text_rects.hits(rect)
                || icon_collision_rects
                    .iter()
                    .any(|r| r.intersects(collision_rect))
            {
                continue;
            }
            icon_collision_rects.push(collision_rect);
            placed.push(draw_icon(
                canvas,
                sheet,
                candidate,
                sprite_name,
                src,
                rect,
                animator,
                viewport,
                fade_in_step,
                &mut icon_paint,
            ));
        }
    }

    // Fade-out pass: entries that lost placement this frame decay and draw as ghosts at their
    // world-anchored position (tracking pan/zoom) until fully transparent. Ghosts don't claim
    // collision space — they already lost it.
    animator.entries.retain(|_, entry| {
        if entry.placed_this_frame {
            return true;
        }
        entry.opacity -= fade_out_step;
        if entry.opacity <= 0.0 {
            return false;
        }

        let ghost = &entry.ghost;
        let (screen_x, screen_y) = viewport.world_to_screen(ghost.world_x, ghost.world_y);

        match &ghost.kind {
            GhostKind::Text {
                text,
                size,
                bold: is_bold,
                text_color,
                halo_color,
                halo_width,
            } => {
                let typeface = if *is_bold {
                    bold.clone()
                } else {
                    regular.clone()
                };
                let font = make_font(typeface, *size);
                let stripped = strip_unsupported_glyphs(text, &font);
                let Some(blob) = TextBlob::from_str(&stripped, &font) else {
                    return false;
                };
                let bounds = blob.bounds();

                canvas.save();
                let origin = if ghost.angle != 0.0 {
                    canvas.translate((screen_x, screen_y));
                    canvas.rotate(ghost.angle.to_degrees(), None);
                    (-bounds.center_x(), -bounds.center_y())
                } else {
                    canvas.translate((screen_x.round(), screen_y.round()));
                    ((-bounds.center_x()).round(), (-bounds.center_y()).round())
                };
                if *halo_width > 0.0 {
                    halo_paint
                        .set_color(super::renderer::apply_opacity(*halo_color, entry.opacity));
                    halo_paint.set_stroke_width(halo_width * 2.0);
                    halo_paint.set_mask_filter(None);
                    canvas.draw_text_blob(&blob, origin, &halo_paint);
                }
                fill_paint.set_color(super::renderer::apply_opacity(*text_color, entry.opacity));
                canvas.draw_text_blob(&blob, origin, &fill_paint);
                canvas.restore();
            }
            GhostKind::Icon { sprite } => {
                let Some(sheet) = sprites else {
                    return false;
                };
                let Some(src) = sheet.get(sprite) else {
                    return false;
                };
                let half = ICON_SIZE_PX / 2.0;
                let mut paint = Paint::default();
                paint.set_alpha_f(entry.opacity);
                draw_pin_circle(canvas, screen_x, screen_y, entry.opacity);
                draw_image_cropped(
                    canvas,
                    &sheet.image,
                    src,
                    Rect::from_xywh(
                        (screen_x - half).round(),
                        (screen_y - half).round(),
                        ICON_SIZE_PX,
                        ICON_SIZE_PX,
                    ),
                    freya::engine::prelude::SamplingOptions::new(
                        freya::engine::prelude::FilterMode::Linear,
                        freya::engine::prelude::MipmapMode::None,
                    ),
                    &paint,
                );
            }
        }
        true
    });

    placed
}

/// Draws `image`'s `src` sub-rect (a sprite atlas region) scaled to fill `dst` — a
/// `Canvas::draw_image_rect_with_sampling_options` replacement that doesn't need
/// `SrcRectConstraint`: this `skia-safe` version no longer re-exports it at any reachable path
/// (it's still required by that method's own signature, just no longer public — an upstream
/// oversight, not something we can work around by naming the type). Equivalent visually: clip to
/// `dst`, then transform so `image`'s `src` sub-rect lands exactly on `dst`, and draw the whole
/// (now-clipped) image.
fn draw_image_cropped(
    canvas: &Canvas,
    image: &Image,
    src: Rect,
    dst: Rect,
    sampling: SamplingOptions,
    paint: &Paint,
) {
    let scale_x = dst.width() / src.width();
    let scale_y = dst.height() / src.height();
    canvas.save();
    canvas.clip_rect(dst, None, None);
    canvas.translate((dst.left - src.left * scale_x, dst.top - src.top * scale_y));
    canvas.scale((scale_x, scale_y));
    canvas.draw_image_with_sampling_options(image, (0.0, 0.0), sampling, Some(paint));
    canvas.restore();
}

/// Pin-style backdrop behind a POI icon: filled circle + thin stroke, giving icons a map-pin
/// look instead of a bare sprite floating on the tile.
fn draw_pin_circle(canvas: &Canvas, screen_x: f32, screen_y: f32, opacity: f32) {
    let mut fill = Paint::default();
    fill.set_anti_alias(true);
    fill.set_color(ICON_PIN_FILL);
    fill.set_alpha_f(fill.alpha_f() * opacity);
    canvas.draw_circle((screen_x, screen_y), ICON_PIN_RADIUS_PX, &fill);

    let mut stroke = Paint::default();
    stroke.set_anti_alias(true);
    stroke.set_style(PaintStyle::Stroke);
    stroke.set_stroke_width(1.0);
    stroke.set_color(ICON_PIN_STROKE);
    stroke.set_alpha_f(stroke.alpha_f() * opacity);
    canvas.draw_circle((screen_x, screen_y), ICON_PIN_RADIUS_PX, &stroke);
}
