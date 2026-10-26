use std::{cell::RefCell, rc::Rc, sync::Arc};

use freya::prelude::*;
use torin::geometry::{Area, CursorPoint};

use crate::{
    overlay::{MapLayer, MarkerSet, RouteOverlay, UserLocation},
    style::{self, StyleSpec},
    util::{
        renderer::TILE_SIZE,
        sprites::SpriteSheet,
        viewport::{Viewport, lonlat_to_center, max_zoom_ui, world_to_lonlat},
    },
};

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

    /// Moves and zooms the view so the world-coordinate box `bounds` fills the part of the map
    /// not covered by `insets`, zooming no closer than `max_zoom`.
    pub fn fit_world_bounds(
        &mut self,
        ((min_x, min_y), (max_x, max_y)): ((f64, f64), (f64, f64)),
        insets: Insets,
        max_zoom: f32,
    ) {
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
        self.center
            .set(CursorPoint::new(raw.x - offset.x, raw.y - offset.y));
        self.zoom.set(zoom);
        self.platform.send(UserEvent::RequestRedraw);
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
        // World y grows southwards.
        (world_to_lonlat(left, bottom), world_to_lonlat(right, top))
    }
}
