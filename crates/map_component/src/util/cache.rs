//! Tile-state caching and the background work that keeps it filled: the LRU of compiled tiles,
//! the per-zoom-bucket bakes (line overlays at normal zoom, whole cells at over-zoom), and the
//! "ensure X is fetched/baked" entry points input handlers call. Owns no rendering — the paint loop only
//! reads these caches.

use std::{
    cell::{Cell, Ref, RefCell, RefMut},
    num::NonZeroUsize,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
};

use freya::{engine::prelude::Image, prelude::*};
use lru::LruCache;
use torin::geometry::{CursorPoint, Size2D};

use crate::style::StyleSpec;

use super::{
    renderer::{self, CompiledTile, ZoomDependentEntry},
    tiles::{
        TileKey, fetch_tile, rasterize_cell_in_background, rasterize_zoom_dependent_in_background,
    },
    viewport::{MAX_FALLBACK_LEVELS, Viewport},
};

/// Cap on the number of decoded tiles kept in memory, so panning across a large area doesn't
/// grow memory/CPU unbounded. Each entry holds a rasterized `TILE_RASTER_SIZE`² bitmap
/// (`renderer::compile_tile`) rather than lightweight vector paths — at 768² RGBA that's ~2.3MiB
/// per tile, so worst-case cache memory is ~150MiB — well above the ~25 tiles visible at once on
/// a large screen (tiles are 362-724px), plus some pan buffer. (Heavy over-zoom doesn't need a bigger bitmap
/// here — it's drawn from separately baked cells, see `cell_level`.)
const TILE_CACHE_CAPACITY: usize = 64;

/// How long a failed tile (network error, timeout, HTTP error) stays failed before the next view
/// change that needs it fetches it again — without this, one failure blanked a tile for the
/// rest of the session.
const FAILED_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone)]
pub enum TileState {
    Pending,
    Loaded(Rc<CompiledTile>),
    /// When it failed, see `FAILED_RETRY_AFTER`.
    Failed(std::time::Instant),
}

pub struct TileCacheData {
    entries: RefCell<LruCache<TileKey, TileState>>,
    /// Bumped by [`Self::reset`]; a fetch started before a reset drops its result instead of
    /// storing a tile compiled against the previous style.
    generation: Cell<u64>,
}

impl TileCacheData {
    pub fn borrow(&self) -> Ref<'_, LruCache<TileKey, TileState>> {
        self.entries.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<'_, LruCache<TileKey, TileState>> {
        self.entries.borrow_mut()
    }

    /// Forgets every tile, including fetches still in flight (see `generation`) — for a style
    /// change, which invalidates every compiled tile.
    pub fn reset(&self) {
        self.entries.borrow_mut().clear();
        self.generation.set(self.generation.get() + 1);
    }
}

pub type TileCache = Rc<TileCacheData>;

pub fn new_tile_cache() -> TileCache {
    Rc::new(TileCacheData {
        entries: RefCell::new(LruCache::new(
            NonZeroUsize::new(TILE_CACHE_CAPACITY).unwrap(),
        )),
        generation: Cell::new(0),
    })
}

/// How finely `ensure_tile_bakes`'s caches bucket continuous zoom. Coarser than exact
/// zoom (so a real gesture's continuous drift lands on the *same* bucket across many consecutive
/// frames — real reuse, not just while sitting still — cutting background-bake churn during a
/// fast gesture) but much finer than `CompiledTile`'s own compile-time `zoom_z` bucket, which
/// spans a *whole* integer zoom level (that coarseness is exactly what made simply baking lines
/// into the main bitmap look wrong — see `renderer::record_line_layer`'s doc comment on the
/// regression that caused). Checked against the real style's steepest line-width stop: worst-case
/// drift within one bucket is under 1px.
pub const ZOOM_DEP_BUCKET_STEP: f32 = 0.25;

/// Which `ZOOM_DEP_BUCKET_STEP`-wide bucket `zoom` falls in.
pub fn zoom_dep_bucket(zoom: f32) -> i32 {
    (zoom / ZOOM_DEP_BUCKET_STEP).round() as i32
}

/// A zoom-bucket key: `(tile, which `ZOOM_DEP_BUCKET_STEP` bucket this bake covers)`.
pub type ZoomDepKey = (TileKey, i32);

/// An over-zoom cell key: `(tile, zoom bucket, level, cell x, cell y)` — the tile split into a
/// `2^level`×`2^level` grid, see `cell_level`.
pub type CellKey = (TileKey, i32, u32, u32, u32);

pub enum ZoomDepState {
    Pending,
    Ready(Image),
}

/// Bitmaps re-baked from a loaded tile's `zoom_dependent` layers at a specific zoom bucket (tiles
/// at most ~724px on screen before switching to cells, so each is at most ~2MiB) — see
/// `ensure_tile_bakes`. Every visible tile (~25 on a large screen) gets an entry per bucket it's
/// been painted at, and a zoom gesture touches several buckets in sequence.
const ZOOM_DEP_CACHE_CAPACITY: usize = 160;

/// Over-zoom cells kept (up to `CELL_MAX_PX`² ≈ 4MiB each). A screen shows ~20 at most, so this
/// holds a few zoom buckets' worth plus some pan margin.
const CELL_CACHE_CAPACITY: usize = 96;

/// Largest on-screen side of an over-zoom cell. Bigger cells mean fewer bakes, but each one costs
/// more to rasterize and upload, and more of it is wasted off screen.
pub const CELL_MAX_PX: f32 = 1024.0;

pub struct ZoomDepCacheData {
    entries: RefCell<LruCache<ZoomDepKey, ZoomDepState>>,
    /// Over-zoom bakes: base layers *and* line overlay of one cell, see `cell_level`.
    cells: RefCell<LruCache<CellKey, ZoomDepState>>,
    /// The bucket the view is currently at, updated by `ensure_tile_bakes`. Shared
    /// with the background bake jobs so one that's still queued when a zoom gesture has moved on
    /// can skip rasterizing — each over-zoomed bake is millions of pixels, and finishing obsolete
    /// ones kept every worker thread saturated throughout a gesture.
    wanted_bucket: Arc<AtomicI32>,
    /// Bumped by [`Self::clear`]; a bake started before then is dropped when it finishes.
    generation: Cell<u64>,
}

impl ZoomDepCacheData {
    /// Drops every bake — for a style change, after which they show the old style.
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
        self.cells.borrow_mut().clear();
        self.generation.set(self.generation.get() + 1);
    }
}

pub type ZoomDepCache = Rc<ZoomDepCacheData>;

pub fn new_zoom_dep_cache() -> ZoomDepCache {
    Rc::new(ZoomDepCacheData {
        entries: RefCell::new(LruCache::new(
            NonZeroUsize::new(ZOOM_DEP_CACHE_CAPACITY).unwrap(),
        )),
        cells: RefCell::new(LruCache::new(
            NonZeroUsize::new(CELL_CACHE_CAPACITY).unwrap(),
        )),
        wanted_bucket: Arc::new(AtomicI32::new(i32::MIN)),
        generation: Cell::new(0),
    })
}

/// A queued bake further than this many buckets from `wanted_bucket` when a worker picks it up is
/// dropped unrasterized. Matches the ±1 neighborhood `bake_tile` requests line overlays for.
const ZOOM_DEP_OBSOLETE_BUCKETS: i32 = 1;

/// Over-zoom: once a tile is drawn larger than its compiled `TILE_RASTER_SIZE` bitmap, it's drawn
/// from cells instead — the tile split into a `2^level`×`2^level` grid, each cell at most
/// `CELL_MAX_PX` on screen and baked (base picture plus line overlay, at the current zoom bucket)
/// only while visible. Bake cost and memory then scale with the screen, not the tile: at z19.5 a
/// z14 tile spans ~11,000px, which no single bitmap can cover sharply. `None` = not over-zoomed,
/// draw the compiled bitmap plus a whole-tile line bake as usual.
pub fn cell_level(tile_px: f32) -> Option<u32> {
    (tile_px > renderer::TILE_RASTER_SIZE as f32)
        .then(|| (tile_px / CELL_MAX_PX).log2().ceil().max(0.0) as u32)
}

/// Cells of a `2^level` grid over a tile drawn at `(screen_x, screen_y)` with side `tile_px`
/// that intersect the `viewport_size` screen.
pub fn visible_cells(
    screen_x: f32,
    screen_y: f32,
    tile_px: f32,
    level: u32,
    viewport_size: Size2D,
) -> impl Iterator<Item = (u32, u32)> {
    let n = 1u32 << level;
    let cell = tile_px / n as f32;
    let range = |origin: f32, extent: f32| {
        let first = ((-origin / cell).floor().max(0.0) as u32).min(n);
        let last = (((extent - origin) / cell).ceil().max(0.0) as u32).min(n);
        first..last
    };
    let ys = range(screen_y, viewport_size.height);
    range(screen_x, viewport_size.width).flat_map(move |cx| ys.clone().map(move |cy| (cx, cy)))
}

/// Everything a view change needs, from an input handler (never the paint closure — `spawn()`
/// panics inside a `RenderCallback`): fetch any visible tile not loaded yet, and bake the loaded
/// ones for the current zoom bucket.
#[allow(clippy::too_many_arguments)]
pub fn ensure_view(
    center: CursorPoint,
    zoom: f32,
    viewport_size: Size2D,
    tile_cache: &TileCache,
    zoom_dep_cache: &ZoomDepCache,
    style: &Arc<StyleSpec>,
    platform: &PlatformWindow,
) {
    ensure_tiles_fetched(
        center,
        zoom,
        viewport_size,
        tile_cache,
        zoom_dep_cache,
        style,
        platform,
    );
    ensure_tile_bakes(
        center,
        zoom,
        viewport_size,
        tile_cache,
        zoom_dep_cache,
        style,
        platform,
    );
}

/// Kicks off background bakes for every visible loaded tile at the current zoom bucket: the line
/// overlay (current bucket and its neighbors) for normally-sized tiles, or the visible cells for
/// over-zoomed ones (see `cell_level`). Must be called from an input handler, not the paint
/// closure — `spawn()` panics inside a `RenderCallback`.
#[allow(clippy::too_many_arguments)]
fn ensure_tile_bakes(
    center: CursorPoint,
    zoom: f32,
    viewport_size: Size2D,
    tile_cache: &TileCache,
    zoom_dep_cache: &ZoomDepCache,
    style: &Arc<StyleSpec>,
    platform: &PlatformWindow,
) {
    let viewport = Viewport::new(center, zoom, style.max_real_zoom);
    let (tile_x_min, tile_x_max, tile_y_min, tile_y_max) = viewport.tile_range(viewport_size);
    zoom_dep_cache
        .wanted_bucket
        .store(zoom_dep_bucket(zoom), Ordering::Relaxed);

    let cache = tile_cache.borrow();
    for tile_y in tile_y_min..tile_y_max {
        for tile_x in tile_x_min..tile_x_max {
            let Some(key) = viewport.tile_key(tile_x, tile_y) else {
                continue;
            };
            let Some(TileState::Loaded(tile)) = cache.peek(&key) else {
                continue;
            };
            bake_tile(
                key,
                tile,
                &viewport,
                zoom,
                viewport_size,
                style,
                zoom_dep_cache,
                platform,
            );
        }
    }
}

/// One tile's share of `ensure_tile_bakes`. Also called when a tile finishes loading, which
/// otherwise wouldn't get a bake until the next input event.
#[allow(clippy::too_many_arguments)]
fn bake_tile(
    key: TileKey,
    tile: &CompiledTile,
    viewport: &Viewport,
    zoom: f32,
    viewport_size: Size2D,
    style: &Arc<StyleSpec>,
    zoom_dep_cache: &ZoomDepCache,
    platform: &PlatformWindow,
) {
    let bucket = zoom_dep_bucket(zoom);
    let Some(level) = cell_level(viewport.tile_px) else {
        if tile.zoom_dependent.is_empty() {
            return;
        }
        for bucket in [bucket, bucket - 1, bucket + 1] {
            ensure_zoom_dep_baked_one(
                key,
                bucket,
                &tile.zoom_dependent,
                style,
                (bucket as f32 * ZOOM_DEP_BUCKET_STEP) as f64,
                viewport.tile_px.round().max(1.0) as i32,
                zoom_dep_cache,
                platform,
            );
        }
        return;
    };

    // Sized for the bucket's own zoom, not the exact current one, so the bitmap for a given key
    // is always the same size no matter which zoom inside the bucket requested it.
    let bucket_zoom = bucket as f32 * ZOOM_DEP_BUCKET_STEP;
    let n = 1u32 << level;
    let bucket_tile_px = renderer::TILE_SIZE * 2f32.powf(bucket_zoom - key.0 as f32);
    let size = (bucket_tile_px / n as f32)
        .ceil()
        .clamp(1.0, 2.0 * CELL_MAX_PX) as i32;
    let (screen_x, screen_y) = viewport.screen_pos(key.1 as i64, key.2 as i64);
    for (cx, cy) in visible_cells(screen_x, screen_y, viewport.tile_px, level, viewport_size) {
        ensure_cell_baked_one(
            (key, bucket, level, cx, cy),
            tile,
            style,
            bucket_zoom as f64,
            size,
            zoom_dep_cache,
            platform,
        );
    }
}

/// `ensure_zoom_dep_baked_one`'s counterpart for an over-zoom cell.
fn ensure_cell_baked_one(
    cache_key: CellKey,
    tile: &CompiledTile,
    style: &Arc<StyleSpec>,
    zoom: f64,
    size: i32,
    zoom_dep_cache: &ZoomDepCache,
    platform: &PlatformWindow,
) {
    if zoom_dep_cache.cells.borrow_mut().contains(&cache_key) {
        return;
    }
    zoom_dep_cache
        .cells
        .borrow_mut()
        .put(cache_key, ZoomDepState::Pending);

    let (_, bucket, level, cx, cy) = cache_key;
    let picture = tile.picture.clone();
    let zoom_dependent = tile.zoom_dependent.clone();
    let style = style.clone();
    let zoom_dep_cache = zoom_dep_cache.clone();
    let platform = platform.clone();
    let is_current = obsolescence_check(&zoom_dep_cache, bucket);
    let generation = zoom_dep_cache.generation.get();

    spawn(async move {
        let image = rasterize_cell_in_background(
            picture,
            zoom_dependent,
            style,
            zoom,
            (level, cx, cy),
            size,
            is_current,
        )
        .await;
        if zoom_dep_cache.generation.get() != generation {
            return;
        }
        let mut cells = zoom_dep_cache.cells.borrow_mut();
        match image {
            Some(image) => {
                cells.put(cache_key, ZoomDepState::Ready(image));
                platform.send(UserEvent::RequestRedraw);
            }
            None => {
                cells.pop(&cache_key);
            }
        }
    });
}

/// Whether a bake for `bucket` is still worth doing once a worker picks it up — see
/// `ZoomDepCacheData::wanted_bucket`.
fn obsolescence_check(
    zoom_dep_cache: &ZoomDepCache,
    bucket: i32,
) -> impl FnOnce() -> bool + Send + 'static {
    let wanted_bucket = zoom_dep_cache.wanted_bucket.clone();
    move || {
        let wanted = wanted_bucket.load(Ordering::Relaxed);
        // `i32::MIN` = no input event has set it yet (initial load): everything is current.
        wanted == i32::MIN || (wanted - bucket).abs() <= ZOOM_DEP_OBSOLETE_BUCKETS
    }
}

/// Marks `(key, bucket)` as pending and spawns its background bake, unless one's already
/// loading/settled.
#[allow(clippy::too_many_arguments)]
fn ensure_zoom_dep_baked_one(
    key: TileKey,
    bucket: i32,
    zoom_dependent: &[ZoomDependentEntry],
    style: &Arc<StyleSpec>,
    zoom: f64,
    size: i32,
    zoom_dep_cache: &ZoomDepCache,
    platform: &PlatformWindow,
) {
    let cache_key = (key, bucket);
    if zoom_dep_cache.entries.borrow_mut().contains(&cache_key) {
        return;
    }
    zoom_dep_cache
        .entries
        .borrow_mut()
        .put(cache_key, ZoomDepState::Pending);

    // Cloned only past the dedup check above — cheap anyway (each `Path` clone just bumps a
    // shared refcount), but this runs per visible tile × 3 buckets on every input event.
    let zoom_dependent = zoom_dependent.to_vec();
    let style = style.clone();
    let zoom_dep_cache = zoom_dep_cache.clone();
    let platform = platform.clone();

    let is_current = obsolescence_check(&zoom_dep_cache, bucket);
    let generation = zoom_dep_cache.generation.get();

    spawn(async move {
        let image =
            rasterize_zoom_dependent_in_background(zoom_dependent, style, zoom, size, is_current)
                .await;
        if zoom_dep_cache.generation.get() != generation {
            return;
        }
        let mut entries = zoom_dep_cache.entries.borrow_mut();
        match image {
            Some(image) => {
                entries.put(cache_key, ZoomDepState::Ready(image));
                platform.send(UserEvent::RequestRedraw);
            }
            // Skipped as obsolete: forget the `Pending` marker so returning to this bucket later
            // bakes it for real.
            None => {
                entries.pop(&cache_key);
            }
        }
    });
}

/// Nearest-bucket fallback for the paint loop: how far (in buckets, either side) a ready bake may
/// be from the current one and still be stretched in as a stand-in. A tile key covers about one
/// zoom level (4 buckets) plus over-zoom, so this reaches any bake the same tile already has
/// without stretching across wildly different line widths.
pub const ZOOM_DEP_MAX_STALE_BUCKETS: i32 = 8;

/// The ready bake for `(key, bucket)`, or else the nearest ready one within
/// `ZOOM_DEP_MAX_STALE_BUCKETS` (second value `true` = stale stand-in). Only the exact hit is
/// promoted in the LRU — stale lookups `peek` so they don't keep obsolete buckets alive.
pub fn zoom_dep_lookup(
    zoom_dep_cache: &ZoomDepCache,
    key: TileKey,
    bucket: i32,
) -> Option<(Image, bool)> {
    let mut cache = zoom_dep_cache.entries.borrow_mut();
    if let Some(ZoomDepState::Ready(image)) = cache.get(&(key, bucket)) {
        return Some((image.clone(), false));
    }
    (1..=ZOOM_DEP_MAX_STALE_BUCKETS).find_map(|distance| {
        [bucket - distance, bucket + distance]
            .into_iter()
            .find_map(|b| match cache.peek(&(key, b)) {
                Some(ZoomDepState::Ready(image)) => Some((image.clone(), true)),
                _ => None,
            })
    })
}

/// A ready cell bitmap found by `cell_lookup`. `level`/`cx`/`cy` are the found cell's own — an
/// ancestor (coarser level) when the requested one isn't baked yet, which the caller crops.
pub struct CellHit {
    pub image: Image,
    pub level: u32,
    pub cx: u32,
    pub cy: u32,
    pub stale: bool,
}

/// Best ready stand-in for cell `(level, cx, cy)` of `key` at `bucket`: the exact cell, else the
/// same cell at the nearest ready bucket within `ZOOM_DEP_MAX_STALE_BUCKETS`, else the cell that
/// contains it one level coarser (and so on up). Only the exact hit is promoted in the LRU.
pub fn cell_lookup(
    zoom_dep_cache: &ZoomDepCache,
    key: TileKey,
    bucket: i32,
    level: u32,
    cx: u32,
    cy: u32,
) -> Option<CellHit> {
    let mut cells = zoom_dep_cache.cells.borrow_mut();
    if let Some(ZoomDepState::Ready(image)) = cells.get(&(key, bucket, level, cx, cy)) {
        return Some(CellHit {
            image: image.clone(),
            level,
            cx,
            cy,
            stale: false,
        });
    }
    for up in 0..=level {
        let (l, x, y) = (level - up, cx >> up, cy >> up);
        for distance in 0..=ZOOM_DEP_MAX_STALE_BUCKETS {
            for b in [bucket - distance, bucket + distance] {
                if let Some(ZoomDepState::Ready(image)) = cells.peek(&(key, b, l, x, y)) {
                    return Some(CellHit {
                        image: image.clone(),
                        level: l,
                        cx: x,
                        cy: y,
                        stale: true,
                    });
                }
            }
        }
    }
    None
}

/// Ready bitmaps of the four cells one level finer than `(level, cx, cy)` (nearest bucket each),
/// as `(image, child x, child y)` — partial cover for a cell with no exact/ancestor bake, typically
/// right after zooming out across a level boundary.
pub fn cell_children(
    zoom_dep_cache: &ZoomDepCache,
    key: TileKey,
    bucket: i32,
    level: u32,
    cx: u32,
    cy: u32,
) -> Vec<(Image, u32, u32)> {
    let cells = zoom_dep_cache.cells.borrow();
    let mut found = Vec::new();
    for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let (x, y) = (cx * 2 + x, cy * 2 + y);
        let hit = (0..=ZOOM_DEP_MAX_STALE_BUCKETS).find_map(|distance| {
            [bucket - distance, bucket + distance]
                .into_iter()
                .find_map(|b| match cells.peek(&(key, b, level + 1, x, y)) {
                    Some(ZoomDepState::Ready(image)) => Some(image.clone()),
                    _ => None,
                })
        });
        if let Some(image) = hit {
            found.push((image, x, y));
        }
    }
    found
}

#[allow(clippy::too_many_arguments)]
fn ensure_tiles_fetched(
    center: CursorPoint,
    zoom: f32,
    viewport_size: Size2D,
    tile_cache: &TileCache,
    zoom_dep_cache: &ZoomDepCache,
    style: &Arc<StyleSpec>,
    platform: &PlatformWindow,
) {
    let viewport = Viewport::new(center, zoom, style.max_real_zoom);
    let (tile_x_min, tile_x_max, tile_y_min, tile_y_max) = viewport.tile_range(viewport_size);

    for tile_y in tile_y_min..tile_y_max {
        for tile_x in tile_x_min..tile_x_max {
            if let Some(key) = viewport.tile_key(tile_x, tile_y) {
                fetch_tile_if_new(
                    key,
                    center,
                    zoom,
                    viewport_size,
                    tile_cache,
                    zoom_dep_cache,
                    style,
                    platform,
                );
            }
        }
    }
}

/// Marks `key` as pending and spawns its fetch, unless it's already loading/settled.
/// `center`/`zoom`/`viewport_size` = the view at request time: once the tile loads, it's baked
/// for that view straight away (see `bake_tile`), so it doesn't sit un-baked until the next input
/// event.
#[allow(clippy::too_many_arguments)]
fn fetch_tile_if_new(
    key: TileKey,
    center: CursorPoint,
    zoom: f32,
    viewport_size: Size2D,
    tile_cache: &TileCache,
    zoom_dep_cache: &ZoomDepCache,
    style: &Arc<StyleSpec>,
    platform: &PlatformWindow,
) {
    match tile_cache.borrow_mut().peek(&key) {
        Some(TileState::Failed(at)) if at.elapsed() >= FAILED_RETRY_AFTER => {}
        Some(_) => return,
        None => {}
    }
    store_tile(tile_cache, key, TileState::Pending);
    let generation = tile_cache.generation.get();

    let tile_cache = tile_cache.clone();
    let zoom_dep_cache = zoom_dep_cache.clone();
    let style = style.clone();
    let platform = platform.clone();

    spawn(async move {
        let fetched = fetch_tile(key, style.clone()).await;
        if tile_cache.generation.get() != generation {
            // Compiled against a style that has since been replaced.
            return;
        }
        let state = match fetched {
            Ok(compiled) => {
                bake_tile(
                    key,
                    &compiled,
                    &Viewport::new(center, zoom, style.max_real_zoom),
                    zoom,
                    viewport_size,
                    &style,
                    &zoom_dep_cache,
                    &platform,
                );
                TileState::Loaded(Rc::new(compiled))
            }
            Err(_) => TileState::Failed(std::time::Instant::now()),
        };

        store_tile(&tile_cache, key, state);
        platform.send(UserEvent::RequestRedraw);
    });
}

/// Inserts `state` for `key`. A loaded tile this evicts (or replaces) is freed on a background
/// thread when nothing else still holds it: dropping one frees thousands of small allocations
/// (per-feature paths, labels), measured ~15% of main-thread time while zooming churns the cache.
fn store_tile(tile_cache: &TileCache, key: TileKey, state: TileState) {
    let evicted = tile_cache.borrow_mut().push(key, state);
    if let Some((_, TileState::Loaded(tile))) = evicted
        && let Ok(tile) = Rc::try_unwrap(tile)
    {
        super::tiles::background_runtime().spawn_blocking(move || drop(tile));
    }
}

/// Looks for the nearest already-loaded ancestor of `(zoom_z, tile_x, tile_y)` in the real slippy
/// tile pyramid — tile `(z, x, y)`'s area is exactly covered by tile `(z - n, x >> n, y >> n)` at
/// any coarser level `n` — so a tile that's still loading can borrow its ancestor's already-cached
/// bitmap (cropped/stretched to this tile's quadrant) as a placeholder instead of a blank tile.
/// Returns the ancestor's `CompiledTile` plus how many levels up it was found.
pub fn find_loaded_ancestor(
    cache: &mut LruCache<TileKey, TileState>,
    zoom_z: u32,
    tile_x: u32,
    tile_y: u32,
) -> Option<(Rc<CompiledTile>, u32)> {
    for levels in 1..=zoom_z.min(MAX_FALLBACK_LEVELS) {
        let key = (zoom_z - levels, tile_x >> levels, tile_y >> levels);
        if let Some(TileState::Loaded(tile)) = cache.get(&key) {
            return Some((tile.clone(), levels));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_level_kicks_in_past_the_compiled_bitmap() {
        assert_eq!(cell_level(renderer::TILE_RASTER_SIZE as f32), None);
        assert_eq!(cell_level(800.0), Some(0));
        assert_eq!(cell_level(CELL_MAX_PX), Some(0));
        assert_eq!(cell_level(CELL_MAX_PX + 1.0), Some(1));
        // z19.5 over a z14 tile: 512 · 2^5.5 ≈ 23170px → 32×32 cells of ~724px.
        assert_eq!(cell_level(23170.0), Some(5));
    }

    #[test]
    fn visible_cells_cover_exactly_the_screen() {
        // 16×16 grid of 724px cells, tile origin 5000px left of the screen and 300px above it.
        let cells: Vec<_> =
            visible_cells(-5000.0, -300.0, 11584.0, 4, Size2D::new(1920.0, 1080.0)).collect();
        let xs: std::collections::BTreeSet<_> = cells.iter().map(|c| c.0).collect();
        let ys: std::collections::BTreeSet<_> = cells.iter().map(|c| c.1).collect();
        assert_eq!(xs.into_iter().collect::<Vec<_>>(), vec![6, 7, 8, 9]);
        assert_eq!(ys.into_iter().collect::<Vec<_>>(), vec![0, 1]);
        // Tile entirely off screen: nothing.
        assert_eq!(
            visible_cells(3000.0, 0.0, 11584.0, 4, Size2D::new(1920.0, 1080.0)).count(),
            0
        );
    }
}
