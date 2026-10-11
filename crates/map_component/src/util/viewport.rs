//! Screen-pixel ↔ tile-coordinate conversion and the zoom model: which integer zoom level's real
//! tiles are shown, how big they are on screen, and how pan/zoom gestures map back onto the
//! stable `center` state. Pure math — no caches, no I/O, no freya component types.

use torin::geometry::{CursorPoint, Size2D};

use super::{renderer::TILE_SIZE, tiles::TileKey};

/// How many zoom levels up `find_loaded_ancestor` will search for a stand-in tile. Bounded so a
/// missing tile deep in unexplored territory doesn't fall all the way back to a near-blank,
/// heavily-cropped world tile — past a handful of levels the crop is so tiny/blurry it's no more
/// useful than just showing nothing until the real tile arrives.
pub const MAX_FALLBACK_LEVELS: u32 = 6;

/// User-facing zoom ceiling for a source whose deepest real zoom is `max_real_zoom` (the style's
/// `StyleSpec::max_real_zoom`, itself derived from the vector source's `maxzoom`). Past
/// `max_real_zoom`, `zoom_z` (see `Viewport`) stays pinned there while `zoom` keeps climbing, so
/// `tile_px` keeps growing past its normal bounded range — the same deepest real tile just gets
/// drawn bigger. Bounded by `MAX_FALLBACK_LEVELS`: a tile this far over the real max would ask
/// `find_loaded_ancestor` to look up more levels than it searches, which would show blank instead
/// of a magnified deepest-real-zoom tile.
pub fn max_zoom_ui(max_real_zoom: u32) -> f32 {
    max_real_zoom as f32 + MAX_FALLBACK_LEVELS as f32
}

/// Real slippy-map tile covering Paris, at `ORIGIN_ZOOM`, used as the anchor for `center`'s
/// (0, 0) — the grid itself still isn't real lat/lon-referenced, but anchoring it here means
/// panning around the initial view requests real tiles around Paris instead of an arbitrary spot
/// on the globe.
const ORIGIN_TILE_X: f64 = 261.0;
const ORIGIN_TILE_Y: f64 = 178.0;
const ORIGIN_ZOOM: f64 = 9.0;

/// Normalized "world" tile coordinates (`0..1`, standard Web Mercator tile fractions at z0) for a
/// lon/lat pair — the same coordinate space [`Viewport::screen_to_world`]/`world_to_screen`
/// operate in, so a geocoded search result's marker can be anchored and redrawn across pan/zoom
/// the same way fading label ghosts already are.
pub fn lonlat_to_world(lon: f64, lat: f64) -> (f64, f64) {
    let lat_rad = lat.to_radians();
    let x = (lon + 180.0) / 360.0;
    let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0;
    (x, y)
}

/// Inverse of [`lonlat_to_world`].
pub fn world_to_lonlat(world_x: f64, world_y: f64) -> (f64, f64) {
    let lon = world_x * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * world_y))
        .sinh()
        .atan()
        .to_degrees();
    (lon, lat)
}

/// The `center` (zoom-independent pan position — see [`Viewport`]) that puts `(lon, lat)` at the
/// map's origin, used to recenter on a geocoded search result.
pub fn lonlat_to_center(lon: f64, lat: f64) -> CursorPoint {
    let (world_x, world_y) = lonlat_to_world(lon, lat);
    let scale = 2f64.powf(ORIGIN_ZOOM);
    CursorPoint::new(
        world_x * scale - ORIGIN_TILE_X,
        world_y * scale - ORIGIN_TILE_Y,
    )
}

/// Inverse of [`lonlat_to_center`] — the current view's rough center as lon/lat, used to bias
/// search results toward what's currently on screen.
pub fn center_to_lonlat(center: CursorPoint) -> (f64, f64) {
    let scale = 2f64.powf(ORIGIN_ZOOM);
    world_to_lonlat(
        (ORIGIN_TILE_X + center.x) / scale,
        (ORIGIN_TILE_Y + center.y) / scale,
    )
}

/// Everything needed to convert between screen pixels and real tile coordinates for one frame or
/// event, derived fresh from `center` (a *zoom-independent* pan position — an offset from the
/// origin tile, in `ORIGIN_ZOOM` tile units) and the current continuous `zoom`.
///
/// Deliberately not built by incrementally rescaling a pixel offset as zoom changes: doing that
/// previously needed a separate, *lossy* integer bit-shift to relocate the origin tile to each
/// new zoom level (`ORIGIN_TILE_X >> (9 - z)`, silently dropping a fractional tile each time),
/// which caused a real position jump exactly when the integer zoom level changed (small in tile
/// units, but tiles are huge at low zoom, so the dropped fraction was a very real distance).
/// Recomputing everything from the stable `center` with one exact float multiply each time has
/// nothing to accumulate error into.
pub struct Viewport {
    /// The integer zoom level whose real tiles are currently fetched/drawn.
    pub zoom_z: u32,
    /// On-screen pixel size of one real tile at `zoom_z`. Bounded to ~362..724px *below*
    /// `MAX_REAL_ZOOM` (not `TILE_SIZE * 2^(zoom - MIN_ZOOM)` spanning the whole zoom range: an
    /// unbounded scale would shrink on-screen tiles to a handful of pixels at low zoom — small
    /// enough that many of them land on the very same real tile, redrawing its content over and
    /// over, tiled edge to edge). Past `MAX_REAL_ZOOM` this grows unbounded instead, since `zoom_z`
    /// is pinned there while `zoom` keeps climbing — deliberate, this is what digitally magnifies
    /// the deepest real tile for over-zoom instead of requesting tiles the server doesn't have.
    pub tile_px: f32,
    /// Exact scale from `ORIGIN_ZOOM` tile units to `zoom_z` tile units (`2^(zoom_z - ORIGIN_ZOOM)`).
    origin_scale: f64,
    /// Real, possibly-fractional tile-space coordinate of `center`.
    center_real_x: f64,
    center_real_y: f64,
}

impl Viewport {
    pub fn new(center: CursorPoint, zoom: f32, max_real_zoom: u32) -> Self {
        let zoom_z = zoom.round().min(max_real_zoom as f32);
        let tile_px = TILE_SIZE * 2f32.powf(zoom - zoom_z);
        let origin_scale = 2f64.powi(zoom_z as i32 - ORIGIN_ZOOM as i32);

        Viewport {
            zoom_z: zoom_z as u32,
            tile_px,
            origin_scale,
            center_real_x: (ORIGIN_TILE_X + center.x) * origin_scale,
            center_real_y: (ORIGIN_TILE_Y + center.y) * origin_scale,
        }
    }

    pub fn tile_range(&self, viewport_size: Size2D) -> (i64, i64, i64, i64) {
        let tiles_wide = viewport_size.width as f64 / self.tile_px as f64;
        let tiles_high = viewport_size.height as f64 / self.tile_px as f64;

        (
            self.center_real_x.floor() as i64,
            (self.center_real_x + tiles_wide).ceil() as i64,
            self.center_real_y.floor() as i64,
            (self.center_real_y + tiles_high).ceil() as i64,
        )
    }

    /// `None` when the coordinate falls outside the real tile grid at `zoom_z` (negative, or past
    /// `2^zoom_z - 1`) — those positions have no tile to fetch or draw, and clamping them instead
    /// used to draw/fetch the edge tile several times over at the world's border.
    pub fn tile_key(&self, tile_x: i64, tile_y: i64) -> Option<TileKey> {
        let max = 1i64 << self.zoom_z;
        if tile_x < 0 || tile_y < 0 || tile_x >= max || tile_y >= max {
            return None;
        }
        Some((self.zoom_z, tile_x as u32, tile_y as u32))
    }

    pub fn screen_pos(&self, tile_x: i64, tile_y: i64) -> (f32, f32) {
        (
            (tile_x as f64 - self.center_real_x) as f32 * self.tile_px,
            (tile_y as f64 - self.center_real_y) as f32 * self.tile_px,
        )
    }

    /// Screen pixels → zoom-independent world coordinates (tile units at z0). Stable across any
    /// zoom/pan change — used to anchor fading-out label ghosts to the map, not the screen.
    pub fn screen_to_world(&self, screen_x: f32, screen_y: f32) -> (f64, f64) {
        let scale = 2f64.powi(self.zoom_z as i32);
        (
            (self.center_real_x + screen_x as f64 / self.tile_px as f64) / scale,
            (self.center_real_y + screen_y as f64 / self.tile_px as f64) / scale,
        )
    }

    /// Inverse of [`Self::screen_to_world`] for the current frame's viewport.
    pub fn world_to_screen(&self, world_x: f64, world_y: f64) -> (f32, f32) {
        let scale = 2f64.powi(self.zoom_z as i32);
        (
            ((world_x * scale - self.center_real_x) * self.tile_px as f64) as f32,
            ((world_y * scale - self.center_real_y) * self.tile_px as f64) as f32,
        )
    }

    /// Converts a screen-pixel delta into a `center` delta (still in `ORIGIN_ZOOM` tile units).
    pub fn pixels_to_center_delta(&self, dx: f64, dy: f64) -> CursorPoint {
        let scale = self.tile_px as f64 * self.origin_scale;
        CursorPoint::new(dx / scale, dy / scale)
    }

    /// The new `center` that keeps the map point currently under `mouse` (element-local pixels)
    /// fixed on screen while zoom changes from `old_zoom` to `new_zoom` — the standard
    /// zoom-toward-the-cursor anchor.
    pub fn zoom_around(
        center: CursorPoint,
        old_zoom: f32,
        new_zoom: f32,
        mouse: CursorPoint,
        max_real_zoom: u32,
    ) -> CursorPoint {
        let old_viewport = Viewport::new(center, old_zoom, max_real_zoom);

        // Real tile-space point (at the *old* zoom level) currently under the mouse.
        let mouse_real_x = old_viewport.center_real_x + mouse.x / old_viewport.tile_px as f64;
        let mouse_real_y = old_viewport.center_real_y + mouse.y / old_viewport.tile_px as f64;

        // Its stable, zoom-independent `center`-space equivalent (never rescaled or truncated —
        // this is what makes the anchor exact across any zoom change).
        let mouse_center_x = mouse_real_x / old_viewport.origin_scale - ORIGIN_TILE_X;
        let mouse_center_y = mouse_real_y / old_viewport.origin_scale - ORIGIN_TILE_Y;

        // Re-derive at the new zoom level, then solve for the `center` that keeps that same point
        // under the mouse.
        let new_viewport_at_origin = Viewport::new(CursorPoint::zero(), new_zoom, max_real_zoom);
        let mouse_real_x_new =
            (mouse_center_x + ORIGIN_TILE_X) * new_viewport_at_origin.origin_scale;
        let mouse_real_y_new =
            (mouse_center_y + ORIGIN_TILE_Y) * new_viewport_at_origin.origin_scale;

        let new_center_real_x = mouse_real_x_new - mouse.x / new_viewport_at_origin.tile_px as f64;
        let new_center_real_y = mouse_real_y_new - mouse.y / new_viewport_at_origin.tile_px as f64;

        CursorPoint::new(
            new_center_real_x / new_viewport_at_origin.origin_scale - ORIGIN_TILE_X,
            new_center_real_y / new_viewport_at_origin.origin_scale - ORIGIN_TILE_Y,
        )
    }
}
