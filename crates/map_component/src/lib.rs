//! Self-contained vector-tile map: tile fetch/decode, style parsing, label placement and the
//! pan/zoom/tap [`MapComponent`]. Knows nothing about search or place details — a POI tap is
//! reported as a [`TappedFeature`] and the host app decides what to do with it.

mod map_controller;
mod map_input;
mod map_paint;
mod map_style_loader;
pub mod style;
pub mod util;

pub use map_controller::MapController;
pub use map_input::TappedFeature;

use std::{cell::RefCell, rc::Rc, sync::Arc};

use freya::prelude::*;
use torin::{node::Node, size::Size};

use util::{
    cache::{ensure_view, new_tile_cache, new_zoom_dep_cache},
    labels::LabelAnimator,
    sprites::{self, SpriteSheet},
};

use map_input::{
    MapInputState, build_on_global_pointer_up, build_on_mouse_move, build_on_pointer_down,
    build_on_wheel,
};
use map_paint::paint_map;
use map_style_loader::load_style;

pub struct MapComponent {
    layout: LayoutData,
    controller: MapController,
    /// Fired when a tap hits a POI icon/label — see `map_input::MapInputState`'s doc comment.
    /// Not compared by [`PartialEq`] below: this component doesn't own "what selecting a place
    /// means" (writing `selected_place`, mirroring a search box, …), the caller does, so a fresh
    /// closure identity each render must not force a re-render of this component — only
    /// `controller`'s actual state matters for that.
    on_place_tapped: Option<EventHandler<TappedFeature>>,
    /// Fired when a tap lands on empty map. Same not-compared reasoning as `on_place_tapped`.
    on_map_tapped: Option<EventHandler<()>>,
}

impl PartialEq for MapComponent {
    fn eq(&self, other: &Self) -> bool {
        self.layout == other.layout && self.controller == other.controller
    }
}

impl MapComponent {
    pub fn new(controller: MapController) -> Self {
        Self {
            layout: Node {
                width: Size::fill(),
                height: Size::fill(),
                ..Default::default()
            }
            .into(),
            controller,
            on_place_tapped: None,
            on_map_tapped: None,
        }
    }

    pub fn on_place_tapped(mut self, handler: impl FnMut(TappedFeature) + 'static) -> Self {
        self.on_place_tapped = Some(EventHandler::new(handler));
        self
    }

    pub fn on_map_tapped(mut self, handler: impl FnMut(()) + 'static) -> Self {
        self.on_map_tapped = Some(EventHandler::new(handler));
        self
    }
}

impl LayoutExt for MapComponent {
    fn get_layout(&mut self) -> &mut LayoutData {
        &mut self.layout
    }
}

impl ContainerSizeExt for MapComponent {}

impl Component for MapComponent {
    fn render(&self) -> impl IntoElement {
        // map controller's state
        let center = self.controller.center;
        let zoom = self.controller.zoom;
        let mut viewport_size = self.controller.viewport_size;
        let marker_world_state = self.controller.marker_world;
        let style = self.controller.style.clone();
        let platform = self.controller.platform.clone();

        // Internal states

        let initial_center = use_state::<Option<CursorPoint>>(|| None);
        let initial_touch = use_state(CursorPoint::zero);

        // caches
        let tile_cache = use_hook(new_tile_cache);
        let zoom_dep_cache = use_hook(new_zoom_dep_cache);

        let placed_labels = use_hook(|| Rc::new(RefCell::new(Vec::new())));

        let label_animator: Rc<RefCell<LabelAnimator>> =
            use_hook(|| Rc::new(RefCell::new(LabelAnimator::default())));

        let hovered_screen = use_hook(|| Rc::new(RefCell::new(None)));

        // POI icon sheet
        let sprite_sheet: Rc<RefCell<Option<Rc<SpriteSheet>>>> =
            use_hook(|| Rc::new(RefCell::new(None)));

        let last_pan_update = use_hook(|| Rc::new(RefCell::new(std::time::Instant::now())));

        // initial fetch
        // fetch style
        use_hook(|| {
            let style_slot = style.clone();
            let tile_cache = tile_cache.clone();
            let zoom_dep_cache = zoom_dep_cache.clone();
            let platform = platform.clone();
            let sheet_slot = sprite_sheet.clone();
            spawn(async move {
                let loaded = Arc::new(load_style().await);
                *style_slot.borrow_mut() = loaded.clone();
                // Everything compiled or baked so far used the default style.
                tile_cache.reset();
                zoom_dep_cache.clear();

                // Cleared tiles won't refetch themselves — nothing but an input event normally
                // triggers `ensure_view`, so kick it off now for whatever's on screen.
                ensure_view(
                    *center.peek(),
                    *zoom.peek(),
                    viewport_size.peek().size,
                    &tile_cache,
                    &zoom_dep_cache,
                    &loaded,
                    &platform,
                );

                platform.send(UserEvent::RequestRedraw);

                // Sprite sheet URL comes from the style too — fetch it now that we know it,
                // rather than a hardcoded guess that may not match this style at all.
                let sprite_url = loaded
                    .sprite_url
                    .clone()
                    .unwrap_or_else(|| style::default::DEFAULT_SPRITE_URL.to_string());
                match sprites::load_sprites(&sprite_url).await {
                    Ok(sheet) => {
                        log::info!("[sprites] loaded {} icons", sheet.index.len());
                        *sheet_slot.borrow_mut() = Some(Rc::new(sheet));
                        platform.send(UserEvent::RequestRedraw);
                    }
                    Err(err) => log::warn!("[sprites] load failed, no icons: {err:?}"),
                }
            });
        });

        let input_state = MapInputState {
            center,
            zoom,
            initial_center,
            initial_touch,
            viewport_size,
            tile_cache: tile_cache.clone(),
            zoom_dep_cache: zoom_dep_cache.clone(),
            style: style.clone(),
            platform: platform.clone(),
            last_pan_update: last_pan_update.clone(),
            hovered_screen: hovered_screen.clone(),
            placed_labels: placed_labels.clone(),
            on_place_tapped: self.on_place_tapped.clone(),
            on_map_tapped: self.on_map_tapped.clone(),
        };

        // On events
        let on_mouse_move = build_on_mouse_move(&input_state);
        let on_pointer_down = build_on_pointer_down(&input_state);
        let on_global_pointer_up = build_on_global_pointer_up(&input_state);
        let on_wheel = build_on_wheel(&input_state);

        rect()
            .width(self.layout.layout.width.clone())
            .height(self.layout.layout.height.clone())
            .child({
                let canvas = {
                    let tile_cache = tile_cache.clone();
                    let zoom_dep_cache = zoom_dep_cache.clone();
                    let style = style.clone();
                    let platform = platform.clone();

                    rect().background(RenderCallback::new(move |context| {
                        let marker_world = *marker_world_state.peek();

                        paint_map(
                            context,
                            &tile_cache,
                            &zoom_dep_cache,
                            &style.borrow().clone(),
                            *center.peek(),
                            *zoom.peek(),
                            &placed_labels,
                            &label_animator,
                            sprite_sheet.borrow().clone(),
                            &platform,
                            *hovered_screen.borrow(),
                            marker_world,
                        );
                    }))
                };

                canvas
                    .width(Size::fill())
                    .height(Size::fill())
                    // handlers
                    .on_sized({
                        let tile_cache = tile_cache.clone();
                        let zoom_dep_cache = zoom_dep_cache.clone();
                        let style = style.clone();
                        let platform = platform.clone();
                        move |e: Event<SizedEventData>| {
                            viewport_size.set(e.area);

                            ensure_view(
                                *center.peek(),
                                *zoom.peek(),
                                e.area.size,
                                &tile_cache,
                                &zoom_dep_cache,
                                &style.borrow(),
                                &platform,
                            );
                        }
                    })
                    .on_mouse_move(on_mouse_move)
                    .on_pointer_down(on_pointer_down)
                    .on_global_pointer_up(on_global_pointer_up)
                    .on_wheel(on_wheel)
            })
    }
}
