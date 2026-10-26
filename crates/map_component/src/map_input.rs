use std::{cell::RefCell, rc::Rc, sync::Arc, time};

use freya::prelude::*;
use torin::geometry::CursorPoint;

use crate::style::StyleSpec;

use super::map_paint::FRAME_BUDGET;
use super::util::renderer::Properties;
use super::util::{
    cache::{TileCache, TileState, ZoomDepCache, ensure_view},
    labels::PlacedLabel,
    poi_lookup,
    renderer::TransitLine,
    viewport::{Viewport, world_to_lonlat},
};

/// All the hook state `render()` creates that the input handlers below need to close over.
/// A plain struct of clones, not a hook itself — built fresh each `render()` call from state
/// that already exists, so it carries no lifecycle of its own.
/// A POI icon/label the user tapped, reported through [`crate::MapComponent::on_place_tapped`].
/// The map only identifies *what* was tapped; looking up details about it is the host's job.
#[derive(Clone)]
pub struct TappedFeature {
    pub title: String,
    pub properties: Properties,
    /// `(osm_type, osm_id)` (`N`/`W`/`R`) decoded from the tile's own feature id, when the tile
    /// source sets one — see [`super::util::poi_lookup`].
    pub osm_identity: Option<(String, u64)>,
    pub lon: f64,
    pub lat: f64,
    /// The POI's detail tags from the tile's `poi_details` layer (rusty_map's own tiles only),
    /// so the host can show them without fetching the OSM element.
    pub details: Option<Properties>,
}

/// A tap on empty map, with where it landed.
#[derive(Clone, Copy)]
pub struct MapTap {
    pub lon: f64,
    pub lat: f64,
    /// A right-click: reported wherever it lands, labels and lines included, without
    /// hit-testing — the app's "what is here" gesture.
    pub secondary: bool,
}

/// A rail/transit line the user tapped, reported through
/// [`crate::MapComponent::on_line_tapped`].
#[derive(Clone)]
pub struct TappedLine {
    pub properties: Properties,
    /// The OSM way behind the tapped segment, when the tile carries feature ids.
    pub osm_way_id: Option<u64>,
    pub lon: f64,
    pub lat: f64,
    /// The tapped stretch as `(lon, lat)` points, to highlight it.
    pub path: Vec<(f64, f64)>,
    /// Ground distance (meters) the tap tolerance covers at the current zoom — how far from
    /// (`lat`, `lon`) the tapped line can be.
    pub hit_radius_m: f64,
}

/// How far (screen pixels) from a line a tap still selects it.
const LINE_HIT_RADIUS_PX: f32 = 10.0;

pub struct MapInputState {
    pub center: State<CursorPoint>,
    pub zoom: State<f32>,
    pub initial_center: State<Option<CursorPoint>>,
    pub initial_touch: State<CursorPoint>,
    pub viewport_size: State<torin::geometry::Area>,
    pub tile_cache: TileCache,
    pub zoom_dep_cache: ZoomDepCache,
    pub style: Rc<RefCell<Arc<StyleSpec>>>,
    pub platform: PlatformWindow,
    pub last_pan_update: Rc<RefCell<time::Instant>>,
    pub hovered_screen: Rc<RefCell<Option<CursorPoint>>>,
    pub placed_labels: Rc<RefCell<Vec<PlacedLabel>>>,
    /// Read-only here: taps report through [`Self::on_place_tapped`]/[`Self::on_map_tapped`]
    /// instead of writing this directly — this field only exists so the background enrichment
    /// continuation can check "is my tap still the active selection" (`still_current` below)
    /// before firing a second `on_place_tapped` call with the enriched result.
    /// Fired (possibly twice — once immediately, again once background enrichment lands) with
    /// the tapped POI's place data. `None` if the caller doesn't care about taps (unlikely, but
    /// keeps this struct constructible without one).
    pub on_place_tapped: Option<EventHandler<TappedFeature>>,
    /// Fired when a tap lands on empty map (no icon/label under it) — dismisses whatever's
    /// currently selected.
    pub on_map_tapped: Option<EventHandler<MapTap>>,
    /// Host markers (see `overlay::MarkerSet`), hit-tested before labels.
    pub markers: State<Option<Rc<crate::overlay::MarkerSet>>>,
    pub on_marker_tapped: Option<EventHandler<u64>>,
    /// Search result pins (see `MapController::pins`), hit-tested after markers.
    pub pins: State<Option<Rc<crate::overlay::MarkerSet>>>,
    pub on_pin_tapped: Option<EventHandler<u64>>,
    /// Fired when a tap hits a rail/transit line (only while the style keeps their geometry,
    /// see `StyleSpec::pick_transit_lines`).
    pub on_line_tapped: Option<EventHandler<TappedLine>>,
    /// Host layers (see `overlay::MapLayer`), whose icons are hit-tested after markers.
    pub layers: State<Vec<Rc<crate::overlay::MapLayer>>>,
    pub on_item_tapped: Option<EventHandler<(u64, u64)>>,
    /// Fired with the pointer's position while it moves over the map (not while dragging).
    pub on_hover: Option<EventHandler<Option<MapHover>>>,
}

/// Where the pointer is over the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapHover {
    pub lon: f64,
    pub lat: f64,
    /// Pointer position in map-local pixels (e.g. to place a tooltip).
    pub x: f32,
    pub y: f32,
}

pub fn build_on_mouse_move(s: &MapInputState) -> impl FnMut(Event<MouseEventData>) + 'static {
    let tile_cache = s.tile_cache.clone();
    let zoom_dep_cache = s.zoom_dep_cache.clone();
    let style = s.style.clone();
    let platform = s.platform.clone();
    let last_pan_update = s.last_pan_update.clone();
    let hovered_screen = s.hovered_screen.clone();
    let placed_labels = s.placed_labels.clone();
    let mut center = s.center;
    let initial_center = s.initial_center;
    let initial_touch = s.initial_touch;
    let zoom = s.zoom;
    let viewport_size = s.viewport_size;
    let on_hover = s.on_hover.clone();

    move |event: Event<MouseEventData>| {
        // Element-local, like `PlacedLabel::rect` (canvas space) — window coordinates only
        // matched while the map sat at the window's top-left corner.
        let pos = event.element_location;
        if initial_center.read().is_none() {
            // Not dragging: track hover for the icon highlight. Throttled like panning —
            // same reasoning, raw pointer-move messages arrive far faster than paint can
            // keep up with, and each change needs its own repaint to show the highlight.
            let mut last = last_pan_update.borrow_mut();
            if last.elapsed() < FRAME_BUDGET {
                return;
            }
            *last = time::Instant::now();
            drop(last);

            if let Some(handler) = &on_hover {
                let viewport = Viewport::new(*center.peek(), *zoom.peek(), style.borrow().max_real_zoom);
                let (world_x, world_y) = viewport.screen_to_world(pos.x as f32, pos.y as f32);
                let (lon, lat) = world_to_lonlat(world_x, world_y);
                handler.call(Some(MapHover { lon, lat, x: pos.x as f32, y: pos.y as f32 }));
            }

            let is_over_icon = placed_labels.borrow().iter().rev().any(|p| {
                p.sprite.is_some()
                    && pos.x as f32 >= p.rect.left
                    && pos.x as f32 <= p.rect.right
                    && pos.y as f32 >= p.rect.top
                    && pos.y as f32 <= p.rect.bottom
            });
            let new_hover = is_over_icon.then_some(pos);
            if *hovered_screen.borrow() != new_hover {
                *hovered_screen.borrow_mut() = new_hover;
                platform.send(UserEvent::RequestRedraw);
            }
            return;
        }
        if let Some(initial) = *initial_center.read() {
            // Cap pan updates to the paint budget (~60fps): raw OS pointer-move messages
            // arrive far faster than frames can be painted, and each `center.set()` +
            // `RequestRedraw` queues a synchronous paint — without this cap a fast drag
            // grows a paint backlog that leaves the app looking frozen while it drains.
            // Dropping an intermediate move loses nothing: the next kept event re-derives
            // `center` from the absolute pointer position, not accumulated deltas.
            {
                let mut last = last_pan_update.borrow_mut();
                if last.elapsed() < FRAME_BUDGET {
                    return;
                }
                *last = time::Instant::now();
            }

            let zoom_val = *zoom.peek();
            let delta = pos - *initial_touch.read();
            let center_delta = Viewport::new(initial, zoom_val, style.borrow().max_real_zoom)
                .pixels_to_center_delta(delta.x, delta.y);
            center.set(CursorPoint::new(
                initial.x - center_delta.x,
                initial.y - center_delta.y,
            ));

            ensure_view(
                *center.peek(),
                zoom_val,
                viewport_size.peek().size,
                &tile_cache,
                &zoom_dep_cache,
                &style.borrow(),
                &platform,
            );

            // The paint closure reads `center` via `.peek()` (non-reactive, deliberately,
            // so it isn't itself a reactive dependency), so `center.set()` above doesn't
            // by itself schedule a repaint. Panning within already-cached tiles (no new
            // fetch, so no `RequestRedraw` from `fetch_tile_if_new`'s completion callback)
            // would otherwise silently update `center` with nothing telling freya to
            // actually repaint.
            platform.send(UserEvent::RequestRedraw);
        }
    }
}

pub fn build_on_pointer_down(s: &MapInputState) -> impl FnMut(Event<PointerEventData>) + 'static {
    let mut initial_touch = s.initial_touch;
    let mut initial_center = s.initial_center;
    let center = s.center;
    move |event: Event<PointerEventData>| {
        initial_touch.set(event.element_location());
        initial_center.set(Some(*center.peek()));
    }
}

pub fn build_on_global_pointer_up(
    s: &MapInputState,
    gesture: Rc<RefCell<TouchGesture>>,
) -> impl FnMut(Event<PointerEventData>) + 'static {
    let placed_labels = s.placed_labels.clone();
    let mut initial_center = s.initial_center;
    let initial_touch = s.initial_touch;
    let mut center = s.center;
    let zoom = s.zoom;
    let style = s.style.clone();
    let platform = s.platform.clone();
    let on_place_tapped = s.on_place_tapped.clone();
    let on_map_tapped = s.on_map_tapped.clone();
    let on_line_tapped = s.on_line_tapped.clone();
    let markers = s.markers;
    let on_marker_tapped = s.on_marker_tapped.clone();
    let pins = s.pins;
    let on_pin_tapped = s.on_pin_tapped.clone();
    let layers = s.layers;
    let on_item_tapped = s.on_item_tapped.clone();
    let tile_cache = s.tile_cache.clone();
    let viewport_size = s.viewport_size;
    move |event: Event<PointerEventData>| {
        // Global: fires for releases anywhere in the window. Only a press that started on the
        // map (`on_pointer_down` set `initial_center`) is a map gesture — a click on an overlay
        // (search panel, place drawer) near the last map press was otherwise taken as a map tap.
        let Some(initial) = *initial_center.peek() else {
            return;
        };
        initial_center.set(None);
        // A pinch: its fingers' releases are neither taps (opening whatever was under one)
        // nor drag ends (jumping the map by the distance between the fingers).
        if matches!(*event, PointerEventData::Touch(_)) && gesture.borrow().was_multi_touch() {
            return;
        }

        // A global event's `element_location` isn't relative to the map: rebase the window
        // position onto the map's area so it matches `initial_touch` (element-local). They only
        // agreed while the map sat at the window's top-left corner.
        let origin = viewport_size.peek().origin;
        let global = event.global_location();
        let pos = CursorPoint::new(global.x - origin.x as f64, global.y - origin.y as f64);
        let start = *initial_touch.peek();
        let moved = ((pos.x - start.x).powi(2) + (pos.y - start.y).powi(2)).sqrt();
        if moved >= 4.0 {
            // Drag release: apply the final pointer position — the pan throttle in
            // `on_mouse_move` may have dropped the last few moves, leaving the map short of
            // where it was let go.
            let center_delta = Viewport::new(initial, *zoom.peek(), style.borrow().max_real_zoom)
                .pixels_to_center_delta(pos.x - start.x, pos.y - start.y);
            center.set(CursorPoint::new(
                initial.x - center_delta.x,
                initial.y - center_delta.y,
            ));
            platform.send(UserEvent::RequestRedraw);
            return;
        }

        if event.button() == Some(MouseButton::Right) {
            if let Some(handler) = &on_map_tapped {
                let viewport =
                    Viewport::new(*center.peek(), *zoom.peek(), style.borrow().max_real_zoom);
                let (world_x, world_y) = viewport.screen_to_world(pos.x as f32, pos.y as f32);
                let (lon, lat) = world_to_lonlat(world_x, world_y);
                handler.call(MapTap {
                    lon,
                    lat,
                    secondary: true,
                });
            }
            return;
        }

        // Markers sit on top of everything, so they're hit first.
        for (handler, set) in [(&on_marker_tapped, markers), (&on_pin_tapped, pins)] {
            let (Some(handler), Some(set)) = (handler, set.peek().clone()) else {
                continue;
            };
            let viewport =
                Viewport::new(*center.peek(), *zoom.peek(), style.borrow().max_real_zoom);
            if let Some(id) =
                crate::overlay::marker_at(&set, &viewport, *zoom.peek(), pos.x as f32, pos.y as f32)
            {
                handler.call(id);
                return;
            }
        }
        if let Some(handler) = &on_item_tapped {
            let viewport =
                Viewport::new(*center.peek(), *zoom.peek(), style.borrow().max_real_zoom);
            let hit =
                crate::overlay::item_at(&layers.peek(), &viewport, pos.x as f32, pos.y as f32);
            if let Some(hit) = hit {
                handler.call(hit);
                return;
            }
        }

        // Tap (press without drag): hit-test the labels/icons placed last frame and report the
        // topmost hit.
        {
            let placed = placed_labels.borrow();
            // Reverse order: icons/last-drawn items sit visually on top.
            let hit = placed.iter().rev().find(|p| {
                pos.x as f32 >= p.rect.left
                    && pos.x as f32 <= p.rect.right
                    && pos.y as f32 >= p.rect.top
                    && pos.y as f32 <= p.rect.bottom
            });
            match hit {
                Some(hit) => {
                    let viewport =
                        Viewport::new(*center.peek(), *zoom.peek(), style.borrow().max_real_zoom);
                    let hit_x = (hit.rect.left + hit.rect.right) / 2.0;
                    let hit_y = (hit.rect.top + hit.rect.bottom) / 2.0;
                    let (world_x, world_y) = viewport.screen_to_world(hit_x, hit_y);
                    let (lon, lat) = world_to_lonlat(world_x, world_y);
                    let osm_identity = hit.feature_id.and_then(poi_lookup::decode_feature_id);
                    let details = osm_identity.as_ref().and_then(|identity| {
                        find_poi_details(
                            &tile_cache,
                            &viewport,
                            viewport_size.peek().size,
                            identity,
                        )
                    });
                    if let Some(handler) = &on_place_tapped {
                        handler.call(TappedFeature {
                            title: hit.text.clone(),
                            properties: (*hit.properties).clone(),
                            // The tile already downloaded for rendering may carry the feature's
                            // own OSM id (see `renderer::RenderableFeature::id`) — no network
                            // round trip needed to resolve it.
                            osm_identity,
                            lon,
                            lat,
                            details,
                        });
                    }
                }
                None => {
                    let style = style.borrow().clone();
                    let viewport = Viewport::new(*center.peek(), *zoom.peek(), style.max_real_zoom);
                    let (world_x, world_y) = viewport.screen_to_world(pos.x as f32, pos.y as f32);
                    let (lon, lat) = world_to_lonlat(world_x, world_y);

                    if style.pick_transit_lines
                        && let Some(handler) = &on_line_tapped
                        && let Some(hit) = hit_transit_line(
                            &tile_cache,
                            &viewport,
                            *zoom.peek(),
                            viewport_size.peek().size,
                            (pos.x as f32, pos.y as f32),
                        )
                    {
                        // Web Mercator ground resolution at this latitude and zoom.
                        let meters_per_px = 40_075_016.686 * lat.to_radians().cos()
                            / (super::util::renderer::TILE_SIZE as f64
                                * 2f64.powf(*zoom.peek() as f64));
                        handler.call(TappedLine {
                            properties: hit.properties,
                            osm_way_id: hit.feature_id.and_then(poi_lookup::decode_line_way_id),
                            lon,
                            lat,
                            path: hit.path,
                            hit_radius_m: (LINE_HIT_RADIUS_PX as f64 * meters_per_px).max(8.0),
                        });
                        return;
                    }

                    if let Some(handler) = &on_map_tapped {
                        handler.call(MapTap {
                            lon,
                            lat,
                            secondary: false,
                        });
                    }
                }
            }
        }
    }
}

/// Fingers on the map, for [`build_on_touch`].
#[derive(Default)]
pub struct TouchGesture {
    /// Element-local position of each finger down, by id.
    fingers: Vec<(u64, CursorPoint)>,
    /// While two fingers are down: the view and fingers when the pinch started.
    pinch: Option<Pinch>,
    /// A pinch ended with a finger still down: that finger's next move re-anchors the pan
    /// rather than jumping the map by the distance between the fingers.
    reanchor: bool,
    /// When the gesture's first finger went down.
    started_at: Option<time::Instant>,
    /// Last time two or more fingers were down.
    multi_at: Option<time::Instant>,
}

impl TouchGesture {
    /// Whether the current (or just finished) gesture had two fingers down at some point: its
    /// release is then neither a tap nor a drag. Timestamps rather than a flag cleared on
    /// release, as Freya dispatches `TouchEnd`'s derived `GlobalPointerUp` in no set order
    /// relative to `TouchEnd` itself.
    pub fn was_multi_touch(&self) -> bool {
        matches!((self.started_at, self.multi_at), (Some(start), Some(multi)) if multi >= start)
    }
}

struct Pinch {
    center: CursorPoint,
    zoom: f32,
    distance: f64,
    midpoint: CursorPoint,
}

fn distance(a: CursorPoint, b: CursorPoint) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

fn midpoint(a: CursorPoint, b: CursorPoint) -> CursorPoint {
    CursorPoint::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
}

/// Touch gestures: one finger pans, two pinch-zoom around their midpoint (and pan with it).
/// Touch never produces `MouseMove`, so [`build_on_mouse_move`]'s drag never runs on a touch
/// screen; a one-finger drag pans here instead, from the same `initial_touch`/`initial_center`
/// anchor `on_pointer_down` sets, which `on_global_pointer_up` then finishes as usual.
pub fn build_on_touch(
    s: &MapInputState,
    gesture: Rc<RefCell<TouchGesture>>,
) -> impl FnMut(Event<TouchEventData>) + 'static {
    let tile_cache = s.tile_cache.clone();
    let zoom_dep_cache = s.zoom_dep_cache.clone();
    let style = s.style.clone();
    let platform = s.platform.clone();
    let last_pan_update = s.last_pan_update.clone();
    let mut center = s.center;
    let mut zoom = s.zoom;
    let mut initial_center = s.initial_center;
    let mut initial_touch = s.initial_touch;
    let viewport_size = s.viewport_size;

    move |event: Event<TouchEventData>| {
        let pos = event.element_location;
        let id = event.finger_id;
        let mut gesture = gesture.borrow_mut();

        match event.phase {
            TouchPhase::Started => {
                gesture.fingers.retain(|(finger, _)| *finger != id);
                if gesture.fingers.is_empty() {
                    gesture.started_at = Some(time::Instant::now());
                }
                gesture.fingers.push((id, pos));
                if gesture.fingers.len() >= 2 {
                    gesture.multi_at = Some(time::Instant::now());
                }
                if let [(_, a), (_, b)] = gesture.fingers[..] {
                    gesture.pinch = Some(Pinch {
                        center: *center.peek(),
                        zoom: *zoom.peek(),
                        distance: distance(a, b).max(1.0),
                        midpoint: midpoint(a, b),
                    });
                    // Not a pan nor a tap any more: `on_global_pointer_up` leaves it alone.
                    initial_center.set(None);
                }
                return;
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if gesture.fingers.len() >= 2 {
                    gesture.multi_at = Some(time::Instant::now());
                }
                gesture.fingers.retain(|(finger, _)| *finger != id);
                if gesture.pinch.take().is_some() && !gesture.fingers.is_empty() {
                    gesture.reanchor = true;
                }
                if gesture.fingers.is_empty() {
                    gesture.reanchor = false;
                }
                return;
            }
            TouchPhase::Moved => {}
        }

        if let Some((_, at)) = gesture.fingers.iter_mut().find(|(finger, _)| *finger == id) {
            *at = pos;
        }

        if gesture.reanchor && gesture.fingers.len() == 1 {
            gesture.reanchor = false;
            initial_touch.set(pos);
            initial_center.set(Some(*center.peek()));
            return;
        }

        // Same cap as mouse panning (see `build_on_mouse_move`): each kept move re-derives the
        // view from the gesture's start, so dropping one loses nothing.
        {
            let mut last = last_pan_update.borrow_mut();
            if last.elapsed() < FRAME_BUDGET {
                return;
            }
            *last = time::Instant::now();
        }

        let max_real_zoom = style.borrow().max_real_zoom;
        let (new_center, new_zoom) = match (&gesture.pinch, &gesture.fingers[..]) {
            (Some(pinch), [(_, a), (_, b)]) => {
                // `on_pointer_down` fires for the second finger too, possibly after
                // `TouchPhase::Started` cleared this: a pinch must not end as a drag release.
                if initial_center.peek().is_some() {
                    initial_center.set(None);
                }
                let new_zoom = (pinch.zoom + (distance(*a, *b) / pinch.distance).log2() as f32)
                    .clamp(
                        style.borrow().min_zoom,
                        super::util::viewport::max_zoom_ui(max_real_zoom),
                    );
                // Zoom around where the pinch started, then follow the midpoint's travel.
                let zoomed = Viewport::zoom_around(
                    pinch.center,
                    pinch.zoom,
                    new_zoom,
                    pinch.midpoint,
                    max_real_zoom,
                );
                let mid = midpoint(*a, *b);
                let pan = Viewport::new(zoomed, new_zoom, max_real_zoom)
                    .pixels_to_center_delta(mid.x - pinch.midpoint.x, mid.y - pinch.midpoint.y);
                (
                    CursorPoint::new(zoomed.x - pan.x, zoomed.y - pan.y),
                    new_zoom,
                )
            }
            (None, [_]) => {
                let Some(initial) = *initial_center.peek() else {
                    return;
                };
                let zoom_val = *zoom.peek();
                let delta = pos - *initial_touch.peek();
                let pan = Viewport::new(initial, zoom_val, max_real_zoom)
                    .pixels_to_center_delta(delta.x, delta.y);
                (
                    CursorPoint::new(initial.x - pan.x, initial.y - pan.y),
                    zoom_val,
                )
            }
            _ => return,
        };
        drop(gesture);

        center.set(new_center);
        if *zoom.peek() != new_zoom {
            zoom.set(new_zoom);
        }
        ensure_view(
            new_center,
            new_zoom,
            viewport_size.peek().size,
            &tile_cache,
            &zoom_dep_cache,
            &style.borrow(),
            &platform,
        );
        // The paint closure only `peek`s the view (see `build_on_mouse_move`).
        platform.send(UserEvent::RequestRedraw);
    }
}

pub fn build_on_wheel(s: &MapInputState) -> impl FnMut(Event<WheelEventData>) + 'static {
    let tile_cache = s.tile_cache.clone();
    let zoom_dep_cache = s.zoom_dep_cache.clone();
    let style = s.style.clone();
    let platform = s.platform.clone();
    let mut center = s.center;
    let mut zoom = s.zoom;
    let viewport_size = s.viewport_size;

    move |e: Event<WheelEventData>| {
        let max_real_zoom = style.borrow().max_real_zoom;
        let old_zoom = *zoom.peek();
        let new_zoom = (old_zoom + e.delta_y as f32 * 0.02).clamp(
            style.borrow().min_zoom,
            super::util::viewport::max_zoom_ui(max_real_zoom),
        );

        // Compute into a local before `set`: `peek`'s read guard lives to the end of
        // the statement, and `set` inside the same expression would try to take the
        // write borrow while it's still held (runtime panic, not a compile error).
        let new_center = Viewport::zoom_around(
            *center.peek(),
            old_zoom,
            new_zoom,
            e.element_location,
            max_real_zoom,
        );
        center.set(new_center);
        zoom.set(new_zoom);

        ensure_view(
            *center.peek(),
            new_zoom,
            viewport_size.peek().size,
            &tile_cache,
            &zoom_dep_cache,
            &style.borrow(),
            &platform,
        );

        // Same as panning: the paint closure only `peek`s `zoom`/`center`. (The canvas used to be
        // re-keyed by zoom to force this instead, which rebuilt the element on every wheel tick —
        // dropping its pointer/hover state, so a pan started right after zooming didn't register.)
        platform.send(UserEvent::RequestRedraw);
    }
}

struct LineHit {
    properties: Properties,
    feature_id: Option<u64>,
    /// The hit polyline as `(lon, lat)`.
    path: Vec<(f64, f64)>,
}

/// Loaded, visible tiles with their screen origin.
fn visible_tiles<'a>(
    cache: &'a lru::LruCache<super::util::tiles::TileKey, TileState>,
    viewport: &Viewport,
    viewport_size: torin::geometry::Size2D,
) -> Vec<(&'a super::util::renderer::CompiledTile, (f32, f32))> {
    let (tile_x_min, tile_x_max, tile_y_min, tile_y_max) = viewport.tile_range(viewport_size);
    let mut tiles = Vec::new();
    for tile_y in tile_y_min..tile_y_max {
        for tile_x in tile_x_min..tile_x_max {
            if let Some(key) = viewport.tile_key(tile_x, tile_y)
                && let Some(TileState::Loaded(tile)) = cache.peek(&key)
            {
                tiles.push((tile.as_ref(), viewport.screen_pos(tile_x, tile_y)));
            }
        }
    }
    tiles
}

/// The rail/transit line closest to screen point `pos`, within [`LINE_HIT_RADIUS_PX`], among
/// the loaded visible tiles' `transit_lines`. A line that names its routes (`transit_lines`
/// layer) wins over a bare track drawn under it.
fn hit_transit_line(
    tile_cache: &TileCache,
    viewport: &Viewport,
    zoom: f32,
    viewport_size: torin::geometry::Size2D,
    pos: (f32, f32),
) -> Option<LineHit> {
    let cache = tile_cache.borrow();
    let margin = LINE_HIT_RADIUS_PX / viewport.tile_px;
    // Rank (no routes, bus, distance) — lower is better: a line naming its routes, then rail over
    // the bus lines running in the streets around it — then the line, its hit polyline, and
    // the tile's screen origin.
    type Candidate<'a> = (
        (bool, bool, f32),
        &'a TransitLine,
        &'a Vec<(f32, f32)>,
        (f32, f32),
    );
    let mut best: Option<Candidate> = None;
    for (tile, origin) in visible_tiles(&cache, viewport, viewport_size) {
        // The tap in this tile's normalized coordinates.
        let local = (
            (pos.0 - origin.0) / viewport.tile_px,
            (pos.1 - origin.1) / viewport.tile_px,
        );
        if !(-margin..=1.0 + margin).contains(&local.0)
            || !(-margin..=1.0 + margin).contains(&local.1)
        {
            continue;
        }
        for line in &tile.transit_lines {
            if is_bus(&line.properties) && (zoom as f64) < crate::style::transit::BUS_LINES_MIN_ZOOM
            {
                continue;
            }
            for polyline in &line.coords {
                for segment in polyline.windows(2) {
                    let distance =
                        point_segment_distance(local, segment[0], segment[1]) * viewport.tile_px;
                    let rank = (!line.has_routes, is_bus(&line.properties), distance);
                    if distance <= LINE_HIT_RADIUS_PX
                        && best.is_none_or(|(best_rank, ..)| rank < best_rank)
                    {
                        best = Some((rank, line, polyline, origin));
                    }
                }
            }
        }
    }
    best.map(|(_, line, polyline, origin)| LineHit {
        properties: (*line.properties).clone(),
        feature_id: line.feature_id,
        path: polyline
            .iter()
            .map(|(x, y)| {
                let (world_x, world_y) = viewport.screen_to_world(
                    origin.0 + x * viewport.tile_px,
                    origin.1 + y * viewport.tile_px,
                );
                world_to_lonlat(world_x, world_y)
            })
            .collect(),
    })
}

/// `true` for a `transit_lines` stretch whose main line is a bus.
fn is_bus(properties: &Properties) -> bool {
    matches!(
        properties.get("mode"),
        Some(mvt_reader::feature::Value::String(mode)) if mode == "bus" || mode == "trolleybus"
    )
}

/// The `poi_details` entry for the OSM element `(type, id)` in any loaded visible tile.
fn find_poi_details(
    tile_cache: &TileCache,
    viewport: &Viewport,
    viewport_size: torin::geometry::Size2D,
    (osm_type, osm_id): &(String, u64),
) -> Option<Properties> {
    let kind = osm_type.chars().next()?;
    let cache = tile_cache.borrow();
    visible_tiles(&cache, viewport, viewport_size)
        .into_iter()
        .find_map(|(tile, _)| tile.poi_details.get(&(kind, *osm_id)))
        .map(|properties| (**properties).clone())
}

fn point_segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_sq = dx * dx + dy * dy;
    let t = if length_sq > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}
