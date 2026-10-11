use std::{cell::RefCell, rc::Rc, sync::Arc, time};

use freya::prelude::*;
use torin::geometry::CursorPoint;

use crate::style::StyleSpec;

use super::map_paint::FRAME_BUDGET;
use super::util::renderer::Properties;
use super::util::{
    cache::{TileCache, ZoomDepCache, ensure_view},
    labels::PlacedLabel,
    poi_lookup,
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
}

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
    pub on_map_tapped: Option<EventHandler<()>>,
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
    move |event: Event<PointerEventData>| {
        // Global: fires for releases anywhere in the window. Only a press that started on the
        // map (`on_pointer_down` set `initial_center`) is a map gesture — a click on an overlay
        // (search panel, place drawer) near the last map press was otherwise taken as a map tap.
        let Some(initial) = *initial_center.peek() else {
            return;
        };
        initial_center.set(None);

        let pos = event.element_location();
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
                    if let Some(handler) = &on_place_tapped {
                        handler.call(TappedFeature {
                            title: hit.text.clone(),
                            properties: (*hit.properties).clone(),
                            // The tile already downloaded for rendering may carry the feature's
                            // own OSM id (see `renderer::RenderableFeature::id`) — no network
                            // round trip needed to resolve it.
                            osm_identity: hit.feature_id.and_then(poi_lookup::decode_feature_id),
                            lon,
                            lat,
                        });
                    }
                }
                // Tapping empty map area dismisses whatever popup/pin/search dropdown was
                // showing.
                None => {
                    if let Some(handler) = &on_map_tapped {
                        handler.call(());
                    }
                }
            }
        }
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
