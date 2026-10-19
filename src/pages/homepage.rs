use freya::{icons::lucide, prelude::*};
use map_component::{MapComponent, MapController, MapTap, TappedFeature, TappedLine};

use crate::components::{
    place::PlaceController,
    search::{SearchController, SearchPanel},
    transit::{Endpoint, Field, TransitController},
};

#[derive(PartialEq)]
pub struct HomePage {}

impl Component for HomePage {
    fn render(&self) -> impl IntoElement {
        let controller = MapController::new();
        let places = PlaceController::new();
        let search = SearchController::new();
        let transit = TransitController::new(controller.clone());
        // Drawer sections (a departure row, a place's Directions button) reach it from deep in
        // the tree.
        use_provide_context({
            let transit = transit.clone();
            move || transit
        });

        // Mirror the selected place's pin onto the map.
        use_side_effect({
            let mut marker = controller.marker_world;
            let platform = controller.platform.clone();
            let selected = places.selected_place;
            move || {
                let world = selected.read().as_ref().and_then(|p| p.marker_world);
                if *marker.peek() != world {
                    marker.set(world);
                    platform.send(UserEvent::RequestRedraw);
                }
            }
        });

        rect()
            .child(
                MapComponent::new(controller.clone())
                    .width(Size::percent(100.))
                    .height(Size::percent(100.))
                    // While planning a journey, taps choose its start/destination instead of
                    // selecting things.
                    .on_place_tapped({
                        let mut places = places.clone();
                        let mut search = search.clone();
                        let mut transit = transit.clone();
                        let platform = controller.platform.clone();
                        move |tap: TappedFeature| {
                            if *transit.planner_open.peek() {
                                if *transit.picking.peek() {
                                    transit.set_endpoint(Endpoint {
                                        name: tap.title.clone(),
                                        lat: tap.lat,
                                        lon: tap.lon,
                                    });
                                }
                                return;
                            }
                            transit.close_view();
                            search.reset(tap.title.clone());
                            places.select_tap(tap, platform.clone());
                        }
                    })
                    // A click on empty map closes whatever the drawer shows, or, when it shows
                    // nothing, drops a pin there with its coordinates and address. A right-click
                    // always drops the pin.
                    .on_map_tapped({
                        let mut places = places.clone();
                        let mut search = search.clone();
                        let mut transit = transit.clone();
                        let platform = controller.platform.clone();
                        move |tap: MapTap| {
                            if *transit.planner_open.peek() {
                                if *transit.picking.peek() {
                                    transit.set_endpoint(map_point(tap.lat, tap.lon));
                                }
                                return;
                            }
                            let showing = places.selected_place.peek().is_some()
                                || transit.view.peek().is_some()
                                || !search.results.peek().is_empty();
                            transit.close_view();
                            search.reset(String::new());
                            if tap.secondary || !showing {
                                places.select_point(
                                    tap.lat,
                                    tap.lon,
                                    search.query,
                                    platform.clone(),
                                );
                            } else {
                                places.clear();
                            }
                        }
                    })
                    .on_line_tapped({
                        let mut places = places.clone();
                        let mut search = search.clone();
                        let mut transit = transit.clone();
                        move |line: TappedLine| {
                            if *transit.planner_open.peek() {
                                if *transit.picking.peek() {
                                    transit.set_endpoint(map_point(line.lat, line.lon));
                                }
                                return;
                            }
                            search.reset(String::new());
                            places.clear();
                            transit.show_track(&line);
                        }
                    }),
            )
            .child(ZoomBadge {
                zoom: controller.zoom,
            })
            .child(Attribution {})
            .child(TransitToggle {
                transit: controller.transit,
            })
            .child(PickBanner {
                transit: transit.clone(),
            })
            .child(SearchPanel::new(controller, places, search, transit))
    }
}

/// A journey endpoint set by tapping the map.
fn map_point(lat: f64, lon: f64) -> Endpoint {
    Endpoint {
        name: format!("Point on map ({lat:.4}, {lon:.4})"),
        lat,
        lon,
    }
}

/// Shown over the map while waiting for a tap to choose a journey's start or destination:
/// says so, and offers a way out (also Escape).
#[derive(PartialEq)]
struct PickBanner {
    transit: TransitController,
}

impl Component for PickBanner {
    fn render(&self) -> impl IntoElement {
        let picking = *self.transit.picking.read() && *self.transit.planner_open.read();
        let field = *self.transit.active_field.read();
        if !picking {
            return rect().into_element();
        }
        let text = match field {
            Field::From => "Click the map to choose the start",
            Field::To => "Click the map to choose the destination",
        };
        let mut cancel = self.transit.clone();
        let mut escape = self.transit.clone();

        rect()
            // Centered over the part of the map the drawer doesn't cover.
            .position(Position::new_absolute().top(14.).left(PICK_BANNER_LEFT))
            .layer(Layer::Overlay)
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(10.0)
            .padding((6.0f32, 6.0f32, 6.0f32, 14.0f32))
            .corner_radius(20.0f32)
            .background(Color::from_rgb(32, 33, 36))
            .shadow((
                0.0f32,
                2.0f32,
                8.0f32,
                0.0f32,
                Color::from_argb(80, 0, 0, 0),
            ))
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if e.key == Key::Named(NamedKey::Escape) {
                    escape.stop_picking();
                }
            })
            .child(
                SvgViewer::new(lucide::crosshair())
                    .width(Size::px(16.))
                    .height(Size::px(16.))
                    .color(Color::WHITE),
            )
            .child(label().text(text).font_size(13.0).color(Color::WHITE))
            .child(
                rect()
                    .padding((4.0f32, 10.0f32))
                    .corner_radius(14.0f32)
                    .background(Color::from_argb(60, 255, 255, 255))
                    .cursor(CursorIcon::Pointer)
                    .on_press(move |_: Event<PressEventData>| cancel.stop_picking())
                    .child(label().text("Cancel").font_size(12.0).color(Color::WHITE)),
            )
            .into_element()
    }
}

/// Left edge of the picking banner: past the drawer (360 px) plus a margin.
const PICK_BANNER_LEFT: f32 = 384.0;

/// Current zoom level, top-right. Its own component so that reading `zoom` subscribes only this
/// label: read from `HomePage::render`, every wheel tick would re-render `HomePage` and diff its
/// whole tree, `MapComponent` included.
#[derive(PartialEq)]
struct ZoomBadge {
    zoom: State<f32>,
}

impl Component for ZoomBadge {
    fn render(&self) -> impl IntoElement {
        let zoom = *self.zoom.read();
        rect()
            .padding((4.0f32, 10.0f32))
            .background(Color::from_argb(220, 255, 255, 255))
            .corner_radius(12.0f32)
            .shadow((
                0.0f32,
                1.0f32,
                4.0f32,
                0.0f32,
                Color::from_argb(50, 0, 0, 0),
            ))
            .position(Position::new_absolute().top(12.).right(12.))
            .layer(Layer::Overlay)
            .child(
                label()
                    .text(format!("Zoom {zoom:.1}"))
                    .color(Color::from_rgb(60, 60, 60))
                    .font_size(12.0),
            )
    }
}

/// Credits for the map data, bottom-right, as the OpenStreetMap (ODbL) and OpenMapTiles licences
/// require. Each name links to its copyright page.
#[derive(PartialEq)]
struct Attribution {}

impl Component for Attribution {
    fn render(&self) -> impl IntoElement {
        let link = |text: &'static str, url: &'static str| {
            label()
                .text(text)
                .font_size(11.0)
                .color(Color::from_rgb(60, 60, 60))
                .cursor(CursorIcon::Pointer)
                .on_press(move |_: Event<PressEventData>| Platform::get().open_url(url))
        };

        rect()
            .position(Position::new_absolute().bottom(0.).right(0.))
            .layer(Layer::Overlay)
            .horizontal()
            .spacing(8.0)
            .padding((2.0f32, 8.0f32))
            .corner_radius((6.0f32, 0.0f32, 0.0f32, 0.0f32))
            .background(Color::from_argb(200, 255, 255, 255))
            .child(link("© OpenMapTiles", "https://openmaptiles.org/"))
            .child(link(
                "© OpenStreetMap contributors",
                "https://www.openstreetmap.org/copyright",
            ))
    }
}

/// Map control switching the public-transport overlay (see `map_component::style::transit`).
#[derive(PartialEq)]
struct TransitToggle {
    transit: State<bool>,
}

impl Component for TransitToggle {
    fn render(&self) -> impl IntoElement {
        let mut transit = self.transit;
        let on = *transit.read();
        let (background, foreground) = if on {
            (Color::from_rgb(30, 91, 216), Color::WHITE)
        } else {
            (
                Color::from_argb(235, 255, 255, 255),
                Color::from_rgb(60, 60, 60),
            )
        };

        rect()
            .position(Position::new_absolute().top(44.).right(12.))
            .layer(Layer::Overlay)
            .padding((6.0f32, 12.0f32))
            .corner_radius(16.0f32)
            .background(background)
            .shadow((
                0.0f32,
                1.0f32,
                4.0f32,
                0.0f32,
                Color::from_argb(60, 0, 0, 0),
            ))
            .cursor(CursorIcon::Pointer)
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(6.0)
            .on_press(move |_: Event<PressEventData>| transit.toggle())
            .child(
                SvgViewer::new(lucide::train_front())
                    .width(Size::px(16.))
                    .height(Size::px(16.))
                    .color(foreground),
            )
            .child(label().text("Transport").font_size(13.0).color(foreground))
    }
}
