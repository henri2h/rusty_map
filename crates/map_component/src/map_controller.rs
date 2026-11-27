use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use freya::prelude::*;
use torin::geometry::{Area, CursorPoint};

use crate::{
    overlay::{MapLayer, MarkerSet, RouteOverlay, UserLocation},
    style::{self, StyleSpec},
    util::{
        renderer::TILE_SIZE,
        sprites::SpriteSheet,
        tiles::background_delay,
        viewport::{
            Viewport, lonlat_to_center, max_zoom_ui, nearest_copy, world_to_lonlat, wrap_center,
        },
    },
};

/// How long a camera glide (see [`MapController::glide_to`]) lasts.
const GLIDE: Duration = Duration::from_millis(380);
/// Step between a glide's frames.
const GLIDE_FRAME: Duration = Duration::from_millis(16);
/// How far past the free area (screen pixels) a box may reach and still count as shown, see
/// [`MapController::frame_world_bounds`].
const FRAMED_TOLERANCE_PX: f32 = 32.0;
/// How many zoom levels further out than a fit the view may be and still count as framing a box
/// (one level: the box at least half the size the fit would draw it).
const FRAMED_ZOOM_SLACK: f32 = 1.0;

/// Cubic ease in and out over `t` in `0..=1`.
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// A world-coordinate box (see `viewport::lonlat_to_world`), `((min_x, min_y), (max_x, max_y))`.
pub type WorldBounds = ((f64, f64), (f64, f64));

/// Screen-pixel margins kept clear when fitting the view, e.g. for a panel covering the map's
/// left side.
#[derive(Clone, Copy, Default)]
pub struct Insets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

/// Map-rendering state, shared between [`crate::MapComponent`] (which reads/writes it while
/// drawing the raw map and handling pan/zoom/tap) and whatever host UI needs to recenter the map
/// or drop a pin.
#[derive(Clone)]
pub struct MapController {
    pub center: State<CursorPoint>,
    pub zoom: State<f32>,
    pub viewport_size: State<Area>,

    /// Zoom-independent world position (see `Viewport::screen_to_world`) of a dropped pin drawn
    /// each frame by the paint loop; set by the host app (e.g. for a geocoded search result).
    pub marker_world: State<Option<(f64, f64)>>,

    /// Public-transport overlay on/off (see `style::transit::with_transit`). The map re-styles
    /// itself when this changes.
    pub transit: State<bool>,

    /// Trip/journey drawn over the map (see [`RouteOverlay`]); `None` draws nothing.
    pub route_overlay: State<Option<Rc<RouteOverlay>>>,
    /// Tappable point markers drawn over the map (see `overlay::MarkerSet`).
    pub markers: State<Option<Rc<MarkerSet>>>,
    /// Search results drawn over the map, under [`Self::markers`] (see
    /// `MapComponent::on_pin_tapped`).
    pub pins: State<Option<Rc<MarkerSet>>>,
    /// The device's position (see [`UserLocation`]); `None` draws nothing.
    pub user_location: State<Option<UserLocation>>,
    /// Host-drawn layers (see [`MapLayer`]), above the route overlay and below markers.
    pub layers: State<Vec<Rc<MapLayer>>>,

    /// Mouse cursor over the map, e.g. a crosshair while the host waits for the user to pick a
    /// point. `None` keeps the default.
    pub cursor: State<Option<CursorIcon>>,

    /// The style's POI icon sheet once loaded, so host UI can show the same icon the map draws
    /// for a kind of place (see [`SpriteSheet::icon_png`]).
    pub sprites: State<Option<Rc<SpriteSheet>>>,

    pub style: Rc<RefCell<Arc<StyleSpec>>>,
    pub platform: PlatformWindow,
    /// The camera glide under way, cancelled by the next one (see [`Self::glide_to`]).
    glide: Rc<RefCell<Option<TaskHandle>>>,
    /// What the host's UI covers of the map (see [`Self::set_covered`]).
    covered: Rc<Cell<Insets>>,
}

impl PartialEq for MapController {
    fn eq(&self, other: &Self) -> bool {
        self.center == other.center
            && self.zoom == other.zoom
            && self.viewport_size == other.viewport_size
            && self.marker_world == other.marker_world
            && self.transit == other.transit
            && self.route_overlay == other.route_overlay
            && self.markers == other.markers
            && self.pins == other.pins
            && self.user_location == other.user_location
            && self.layers == other.layers
            && self.cursor == other.cursor
            && self.sprites == other.sprites
            && Rc::ptr_eq(&self.style, &other.style)
            && Rc::ptr_eq(&self.glide, &other.glide)
            && Rc::ptr_eq(&self.covered, &other.covered)
    }
}

impl MapController {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let center = use_state(CursorPoint::zero);
        let zoom = use_state(|| {
            (style::default::DEFAULT_MIN_ZOOM + max_zoom_ui(style::default::DEFAULT_MAX_REAL_ZOOM))
                / 2.0
        });
        let viewport_size = use_state(Area::default);
        let marker_world = use_state::<Option<(f64, f64)>>(|| None);
        let transit = use_state(|| false);
        let route_overlay = use_state::<Option<Rc<RouteOverlay>>>(|| None);
        let markers = use_state::<Option<Rc<MarkerSet>>>(|| None);
        let pins = use_state::<Option<Rc<MarkerSet>>>(|| None);
        let cursor = use_state::<Option<CursorIcon>>(|| None);
        let user_location = use_state::<Option<UserLocation>>(|| None);
        let layers = use_state::<Vec<Rc<MapLayer>>>(Vec::new);
        let sprites = use_state::<Option<Rc<SpriteSheet>>>(|| None);
        let style = use_hook(|| Rc::new(RefCell::new(Arc::new(style::default::default_style()))));
        let platform = use_hook(|| Platform::get().current_window());
        let glide = use_hook(|| Rc::new(RefCell::new(None)));
        let covered = use_hook(|| Rc::new(Cell::new(Insets::default())));

        Self {
            center,
            zoom,
            viewport_size,
            marker_world,
            transit,
            route_overlay,
            markers,
            pins,
            user_location,
            layers,
            cursor,
            sprites,
            style,
            platform,
            glide,
            covered,
        }
    }
}

impl MapController {
    /// Replaces the drawn overlay and repaints.
    pub fn set_route_overlay(&mut self, overlay: Option<RouteOverlay>) {
        self.route_overlay.set(overlay.map(Rc::new));
        self.platform.send(UserEvent::RequestRedraw);
    }

    /// Moves the device position dot and repaints.
    pub fn set_user_location(&mut self, location: Option<UserLocation>) {
        if *self.user_location.peek() != location {
            self.user_location.set(location);
            self.platform.send(UserEvent::RequestRedraw);
        }
    }

    /// Replaces the drawn markers and repaints.
    pub fn set_markers(&mut self, markers: Option<MarkerSet>) {
        self.markers.set(markers.map(Rc::new));
        self.platform.send(UserEvent::RequestRedraw);
    }

    /// Replaces the drawn search result pins and repaints, unless unchanged.
    pub fn set_pins(&mut self, pins: Option<MarkerSet>) {
        if self.pins.peek().as_deref() != pins.as_ref() {
            self.pins.set(pins.map(Rc::new));
            self.platform.send(UserEvent::RequestRedraw);
        }
    }

    /// Replaces the layer with `layer.id`, or appends it on top, and repaints.
    pub fn set_layer(&mut self, layer: MapLayer) {
        let layer = Rc::new(layer);
        let mut layers = self.layers.write();
        match layers.iter_mut().find(|existing| existing.id == layer.id) {
            Some(existing) => *existing = layer,
            None => layers.push(layer),
        }
        drop(layers);
        self.platform.send(UserEvent::RequestRedraw);
    }

    /// Removes the layer with `id` and repaints.
    pub fn remove_layer(&mut self, id: u64) {
        self.layers.write().retain(|layer| layer.id != id);
        self.platform.send(UserEvent::RequestRedraw);
    }

    /// Removes every layer and repaints.
    pub fn clear_layers(&mut self) {
        self.layers.set(Vec::new());
        self.platform.send(UserEvent::RequestRedraw);
    }

    /// Records what the host's UI covers of the map (a drawer down one side, a bottom sheet), so
    /// whatever frames something on the map can keep it in the part left visible. Not reactive:
    /// read when framing, not drawn from.
    pub fn set_covered(&self, covered: Insets) {
        self.covered.set(covered);
    }

    /// What [`Self::set_covered`] last recorded.
    pub fn covered(&self) -> Insets {
        self.covered.get()
    }

    /// Moves and zooms the view so the world-coordinate box `bounds` fills the part of the map
    /// not covered by `insets`, zooming no closer than `max_zoom`.
    pub fn fit_world_bounds(&mut self, bounds: WorldBounds, insets: Insets, max_zoom: f32) {
        let (center, zoom) = self.fit_target(bounds, insets, max_zoom);
        self.center.set(center);
        self.zoom.set(zoom);
        self.platform.send(UserEvent::RequestRedraw);
    }

    /// [`Self::fit_world_bounds`], gliding there (see [`Self::glide_to`]).
    pub fn glide_to_world_bounds(&mut self, bounds: WorldBounds, insets: Insets, max_zoom: f32) {
        let (center, zoom) = self.fit_target(bounds, insets, max_zoom);
        self.glide_to(center, zoom);
    }

    /// Brings `bounds` into view like [`Self::glide_to_world_bounds`], but only when the view
    /// doesn't already frame it: all of it within the free area (give or take
    /// `FRAMED_TOLERANCE_PX`) at about the zoom a fit would pick. A new route over the same
    /// place — another of a journey's itineraries — leaves the map alone instead of shifting it.
    pub fn frame_world_bounds(&mut self, bounds: WorldBounds, insets: Insets, max_zoom: f32) {
        let (_, fit_zoom) = self.fit_target(bounds, insets, max_zoom);
        let framed = *self.zoom.peek() >= fit_zoom - FRAMED_ZOOM_SLACK
            && self.shows_world_bounds(bounds, insets, FRAMED_TOLERANCE_PX);
        if !framed {
            self.glide_to_world_bounds(bounds, insets, max_zoom);
        }
    }

    /// Whether all of `bounds` is on screen outside `insets`, `tolerance` pixels past it allowed.
    pub fn shows_world_bounds(
        &self,
        ((min_x, min_y), (max_x, max_y)): WorldBounds,
        insets: Insets,
        tolerance: f32,
    ) -> bool {
        let max_real_zoom = self.style.borrow().max_real_zoom;
        let viewport = Viewport::new(*self.center.peek(), *self.zoom.peek(), max_real_zoom);
        // Together, onto one copy of the sideways-repeating world.
        let corners = viewport.shape_to_screen(&[(min_x, min_y), (max_x, max_y)]);
        let ((left, top), (right, bottom)) = (corners[0], corners[1]);
        let area = self.viewport_size.peek().size;
        left >= insets.left - tolerance
            && top >= insets.top - tolerance
            && right <= area.width - insets.right + tolerance
            && bottom <= area.height - insets.bottom + tolerance
    }

    /// The view [`Self::fit_world_bounds`] moves to: `(center, zoom)`.
    fn fit_target(
        &self,
        ((min_x, min_y), (max_x, max_y)): WorldBounds,
        insets: Insets,
        max_zoom: f32,
    ) -> (CursorPoint, f32) {
        let style = self.style.borrow().clone();
        let area = self.viewport_size.peek().size;
        let available_w = (area.width - insets.left - insets.right).max(64.0) as f64;
        let available_h = (area.height - insets.top - insets.bottom).max(64.0) as f64;
        let span_w = (max_x - min_x).max(1e-9);
        let span_h = (max_y - min_y).max(1e-9);
        let scale = (available_w / span_w).min(available_h / span_h) / TILE_SIZE as f64;
        let zoom = (scale.log2() as f32)
            .min(max_zoom)
            .clamp(style.min_zoom, max_zoom_ui(style.max_real_zoom));

        // `lonlat_to_center` anchors a point at the screen's top-left corner; shift it to the
        // middle of the free area, the same way panning converts pixels to `center` deltas.
        let (lon, lat) = world_to_lonlat((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
        let raw = lonlat_to_center(lon, lat);
        let offset = Viewport::new(CursorPoint::zero(), zoom, style.max_real_zoom)
            .pixels_to_center_delta(
                insets.left as f64 + available_w / 2.0,
                insets.top as f64 + available_h / 2.0,
            );
        (CursorPoint::new(raw.x - offset.x, raw.y - offset.y), zoom)
    }

    /// Moves the view to `(center, zoom)` over [`GLIDE`], easing in and out, rather than jumping
    /// there. Zoom changes evenly (it's a scale exponent) while the point in the middle of the
    /// screen travels straight to its new place. Replaces a glide under way; stops as soon as
    /// something else moves the map (a drag, the wheel) — the user's hand wins.
    pub fn glide_to(&mut self, center: CursorPoint, zoom: f32) {
        if let Some(previous) = self.glide.borrow_mut().take() {
            previous.cancel();
        }
        let (mut center_state, mut zoom_state) = (self.center, self.zoom);
        let (from_center, from_zoom) = (*center_state.peek(), *zoom_state.peek());
        if from_center == center && from_zoom == zoom {
            return;
        }
        let max_real_zoom = self.style.borrow().max_real_zoom;
        let size = self.viewport_size.peek().size;
        // `center` anchors the top-left corner: the screen's middle, from it, at `zoom`.
        let half = move |zoom: f32| {
            Viewport::new(CursorPoint::zero(), zoom, max_real_zoom)
                .pixels_to_center_delta(size.width as f64 / 2.0, size.height as f64 / 2.0)
                .to_vector()
        };
        let from_middle = from_center + half(from_zoom);
        // The short way round the sideways-repeating world.
        let to_middle = nearest_copy(from_middle, center + half(zoom));
        let platform = self.platform.clone();

        // In the window's scope: a glide started from a list row's press handler would otherwise
        // stop when that row re-renders away.
        let task = spawn_in_window(async move {
            let start = Instant::now();
            let mut shown = (from_center, from_zoom);
            loop {
                background_delay(GLIDE_FRAME).await;
                if (*center_state.peek(), *zoom_state.peek()) != shown {
                    return;
                }
                let t = (start.elapsed().as_secs_f32() / GLIDE.as_secs_f32()).min(1.0);
                let done = t >= 1.0;
                // The last frame lands exactly on the target, not on interpolation round-off.
                let frame = if done {
                    (center, zoom)
                } else {
                    let eased = ease_in_out(t);
                    let frame_zoom = from_zoom + (zoom - from_zoom) * eased;
                    let middle = from_middle + (to_middle - from_middle) * eased as f64;
                    (wrap_center(middle - half(frame_zoom)), frame_zoom)
                };
                center_state.set(frame.0);
                zoom_state.set(frame.1);
                shown = frame;
                platform.send(UserEvent::RequestRedraw);
                if done {
                    return;
                }
            }
        });
        *self.glide.borrow_mut() = Some(task);
    }

    /// The lon/lat box the map currently shows (less `insets`, e.g. a panel covering its
    /// side), as `((min_lon, min_lat), (max_lon, max_lat))`.
    pub fn visible_lonlat_bounds(&self, insets: Insets) -> ((f64, f64), (f64, f64)) {
        let max_real_zoom = self.style.borrow().max_real_zoom;
        let area = self.viewport_size.peek().size;
        let viewport = Viewport::new(*self.center.peek(), *self.zoom.peek(), max_real_zoom);
        let (left, top) = viewport.screen_to_world(insets.left, insets.top);
        let (right, bottom) = viewport.screen_to_world(
            (area.width - insets.right).max(insets.left + 1.0),
            (area.height - insets.bottom).max(insets.top + 1.0),
        );
        // World y grows southwards. Longitudes from the left edge's, not each edge wrapped on
        // its own: a view across the antimeridian would come out with its sides swapped. Clamped,
        // as a box can't cross it.
        let ((min_lon, min_lat), (_, max_lat)) =
            (world_to_lonlat(left, bottom), world_to_lonlat(right, top));
        let max_lon = (min_lon + (right - left) * 360.0).min(180.0);
        ((min_lon, min_lat), (max_lon, max_lat))
    }
}
