use std::time::{Duration, Instant};

use freya::{icons::lucide, prelude::*};
use map_component::{
    Insets, MapComponent, MapController, MapTap, TappedFeature, TappedLine, UserLocation,
    util::viewport::lonlat_to_world,
};

use super::{favorites::FavoritesPage, settings::SettingsPage};

use crate::{
    location::{self, Reading},
    mobile::{Haptic, Screen, haptic, is_back, use_provide_screen, use_screen},
};

use crate::components::{
    charging::{ChargerLayer, Chargers},
    favorites::Favorites,
    layers::{LayersButton, LayersModal, MapLayer},
    map_view::use_remembered_view,
    navigation::{
        NavigationController,
        ui::{NavigationBanner, NavigationBar, RecenterChip},
    },
    place::PlaceController,
    search::{SearchController, SearchPanel, pick_result},
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
        let navigation = NavigationController::new(controller.clone());
        use_provide_context({
            let navigation = navigation.clone();
            move || navigation
        });
        // Only start and end re-render the page, not every fix while guided.
        let navigating = use_memo({
            let navigation = navigation.clone();
            move || navigation.is_active()
        });
        let navigating = *navigating.read();
        let favorites = Favorites::new();
        use_provide_context(move || favorites);
        let chargers = Chargers::new();
        let chargers_on = *chargers.enabled.read();
        use_remembered_view(&controller);
        use_provide_screen(controller.viewport_size);
        let layers_open = use_state(|| false);
        let settings_open = use_state(|| false);
        let layers = vec![
            MapLayer {
                name: "Transport",
                icon: lucide::train_front(),
                color: Color::from_rgb(30, 91, 216),
                enabled: controller.transit,
            },
            MapLayer {
                name: "Chargers",
                icon: lucide::zap(),
                color: Color::from_rgb(24, 128, 56),
                enabled: chargers.enabled,
            },
        ];
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
                                    haptic(Haptic::Select);
                                    transit.set_endpoint(Endpoint {
                                        name: tap.title.clone(),
                                        lat: tap.lat,
                                        lon: tap.lon,
                                    });
                                }
                                return;
                            }
                            haptic(Haptic::Select);
                            transit.close_view();
                            search.reset(tap.title.clone());
                            places.select_tap(tap, platform.clone());
                        }
                    })
                    // A click on empty map closes whatever the drawer shows, or, when it shows
                    // nothing, drops a pin there with its coordinates and address. A right-click
                    // always drops the pin.
                    .on_marker_tapped({
                        let mut places = places.clone();
                        let mut search = search.clone();
                        let mut transit = transit.clone();
                        move |id: u64| {
                            let Some(charger) = chargers.by_marker(id) else {
                                return;
                            };
                            if *transit.planner_open.peek() {
                                if *transit.picking.peek() {
                                    haptic(Haptic::Select);
                                    transit.set_endpoint(Endpoint {
                                        name: charger.name.clone(),
                                        lat: charger.lat,
                                        lon: charger.lon,
                                    });
                                }
                                return;
                            }
                            haptic(Haptic::Select);
                            transit.close_view();
                            search.reset(charger.name.clone());
                            places.select_charger(&charger);
                        }
                    })
                    // A search result pin: shown like picking its row.
                    .on_pin_tapped({
                        let mut places = places.clone();
                        let mut search = search.clone();
                        let mut transit = transit.clone();
                        let map = controller.clone();
                        move |index: u64| {
                            let Some(result) = search.results.peek().get(index as usize).cloned()
                            else {
                                return;
                            };
                            if *transit.planner_open.peek() {
                                return;
                            }
                            haptic(Haptic::Select);
                            transit.close_view();
                            pick_result(&result, &map, &mut places, &mut search);
                        }
                    })
                    .on_map_tapped({
                        let mut places = places.clone();
                        let mut search = search.clone();
                        let mut transit = transit.clone();
                        let platform = controller.platform.clone();
                        move |tap: MapTap| {
                            if *transit.planner_open.peek() {
                                if *transit.picking.peek() {
                                    haptic(Haptic::Select);
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
                                haptic(Haptic::Select);
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
                                    haptic(Haptic::Select);
                                    transit.set_endpoint(map_point(line.lat, line.lon));
                                }
                                return;
                            }
                            haptic(Haptic::Select);
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
            .child(LayersButton {
                layers: layers.clone(),
                open: layers_open,
                top: 12.,
            })
            // While guiding, the walk follows the location itself (and offers to re-centre).
            .maybe_child((location::AVAILABLE && !navigating).then(|| {
                LocationButton {
                    map: controller.clone(),
                }
                .into_element()
            }))
            .maybe_child(chargers_on.then(|| {
                ChargerLayer {
                    chargers,
                    map: controller.clone(),
                    places: places.clone(),
                }
                .into_element()
            }))
            .child(FavoritesButton {
                favorites,
                places: places.clone(),
                search: search.clone(),
                transit: transit.clone(),
            })
            .child(PickBanner {
                transit: transit.clone(),
            })
            // Hidden while guiding: the banner and bottom bar take its place.
            .maybe_child((!navigating).then(|| {
                SearchPanel::new(controller.clone(), places.clone(), search.clone(), transit)
                    .into_element()
            }))
            .child(NavigationBanner {})
            .child(NavigationBar {})
            .child(RecenterChip {})
            .child(FavoritesPage {
                map: controller,
                places,
                search,
            })
            .child(SettingsButton {
                open: settings_open,
            })
            .child(SettingsPage {
                open: settings_open,
            })
            .child(LayersModal {
                layers,
                open: layers_open,
                top: 12.,
            })
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
        // Hooks before the early return: their count must not change between renders.
        let screen = use_screen();
        let picking = *self.transit.picking.read() && *self.transit.planner_open.read();
        let field = *self.transit.active_field.read();
        if !picking {
            return rect().into_element();
        }
        let text = match (field, cfg!(target_os = "android")) {
            (Field::From, false) => "Click the map to choose the start",
            (Field::To, false) => "Click the map to choose the destination",
            (Field::From, true) => "Tap the map to choose the start",
            (Field::To, true) => "Tap the map to choose the destination",
        };
        // Centered over the part of the map the drawer doesn't cover; above the bottom sheet in
        // the compact layout.
        let left = if screen.compact {
            12.
        } else {
            PICK_BANNER_LEFT
        };
        let mut cancel = self.transit.clone();
        let mut escape = self.transit.clone();

        rect()
            .position(
                Position::new_absolute()
                    .top(14. + screen.insets.top)
                    .left(left),
            )
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
                if is_back(&e.key) {
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

/// Current zoom level, bottom-left, as discreet as the attribution. Its own component so that reading `zoom` subscribes only this
/// label: read from `HomePage::render`, every wheel tick would re-render `HomePage` and diff its
/// whole tree, `MapComponent` included.
#[derive(PartialEq)]
struct ZoomBadge {
    zoom: State<f32>,
}

impl Component for ZoomBadge {
    fn render(&self) -> impl IntoElement {
        let zoom = *self.zoom.read();
        let screen = use_screen();
        rect()
            .position(Position::new_absolute().bottom(0.).left(0.))
            .layer(Layer::Overlay)
            // Above the Android navigation bar.
            .padding((2.0, 8.0, 2.0 + screen.insets.bottom, 8.0))
            .corner_radius((0.0f32, 6.0f32, 0.0f32, 0.0f32))
            .background(Color::from_argb(200, 255, 255, 255))
            .child(
                label()
                    .text(format!("z {zoom:.1}"))
                    .font_size(11.0)
                    .color(Color::from_rgb(60, 60, 60)),
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
                .on_press(move |_: Event<PressEventData>| crate::mobile::open_url(url))
        };

        let screen = use_screen();
        rect()
            .position(Position::new_absolute().bottom(0.).right(0.))
            .layer(Layer::Overlay)
            .horizontal()
            .spacing(8.0)
            // Above the Android navigation bar.
            .padding((2.0, 8.0, 2.0 + screen.insets.bottom, 8.0))
            .corner_radius((6.0f32, 0.0f32, 0.0f32, 0.0f32))
            .background(Color::from_argb(200, 255, 255, 255))
            .child(link("© OpenMapTiles", "https://openmaptiles.org/"))
            .child(link(
                "© OpenStreetMap contributors",
                "https://www.openstreetmap.org/copyright",
            ))
    }
}

/// Map control opening the settings page (about, data attribution), under Favorites.
#[derive(PartialEq)]
struct SettingsButton {
    open: State<bool>,
}

impl Component for SettingsButton {
    fn render(&self) -> impl IntoElement {
        let screen = use_screen();
        let mut open = self.open;
        rect()
            .position(
                Position::new_absolute()
                    .top(92. + screen.controls_top())
                    .right(12.),
            )
            .layer(Layer::Overlay)
            .padding((6.0f32, 12.0f32))
            .corner_radius(16.0f32)
            .background(Color::from_argb(235, 255, 255, 255))
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
            .on_press(move |_: Event<PressEventData>| {
                haptic(Haptic::Press);
                open.set(true);
            })
            .child(
                SvgViewer::new(lucide::settings())
                    .width(Size::px(15.))
                    .height(Size::px(15.))
                    .color(Color::from_rgb(60, 60, 60)),
            )
            .child(
                label()
                    .text("Settings")
                    .font_size(13.0)
                    .color(Color::from_rgb(60, 60, 60)),
            )
    }
}

/// Map control opening the favorites page.
#[derive(PartialEq)]
struct FavoritesButton {
    favorites: Favorites,
    places: PlaceController,
    search: SearchController,
    transit: TransitController,
}

impl Component for FavoritesButton {
    fn render(&self) -> impl IntoElement {
        let mut page_open = self.favorites.page_open;
        let mut places = self.places.clone();
        let mut search = self.search.clone();
        let mut transit = self.transit.clone();
        let screen = use_screen();
        rect()
            .position(
                Position::new_absolute()
                    .top(52. + screen.controls_top())
                    .right(12.),
            )
            .layer(Layer::Overlay)
            .padding((6.0f32, 12.0f32))
            .corner_radius(16.0f32)
            .background(Color::from_argb(235, 255, 255, 255))
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
            // Closes the drawer first: a transit view open in it would keep the page hidden
            // (see `FavoritesPage`), and the page replaces whatever was being looked at.
            .on_press(move |_: Event<PressEventData>| {
                haptic(Haptic::Press);
                transit.close_view();
                transit.close_planner();
                search.reset(String::new());
                places.clear();
                page_open.set(true);
            })
            .child(
                SvgViewer::new(lucide::star())
                    .width(Size::px(15.))
                    .height(Size::px(15.))
                    .color(Color::from_rgb(242, 153, 0)),
            )
            .child(
                label()
                    .text("Favorites")
                    .font_size(13.0)
                    .color(Color::from_rgb(60, 60, 60)),
            )
    }
}

/// What the location control does: nothing yet, keep the map centred on the device, or only
/// show the device's dot (after the user panned away while following).
#[derive(Clone, Copy, PartialEq)]
enum LocationMode {
    Off,
    Follow,
    Show,
}

/// A second tap this soon after the first makes a double tap.
const DOUBLE_TAP: Duration = Duration::from_millis(350);
/// Zoom a double tap goes to (or this much closer, when already past it).
const FOLLOW_ZOOM: f32 = 16.0;
const FOLLOW_ZOOM_STEP: f32 = 2.0;

/// Map control showing the device's position, bottom-right. A tap centres the map on it and
/// follows it; a double tap also zooms in. Panning the map stops following but keeps the dot.
/// Location stays on once started (`MainActivity` pauses it while the app is in the background).
#[derive(PartialEq)]
struct LocationButton {
    map: MapController,
}

impl Component for LocationButton {
    fn render(&self) -> impl IntoElement {
        let screen = use_screen();
        let mut mode = use_state(|| LocationMode::Off);
        // Where the last recentre left the map; `None` until the first one since following
        // started.
        let mut followed_to = use_state(|| None::<CursorPoint>);
        // A double tap before the first fix: zoom in once it arrives.
        let mut zoom_on_fix = use_state(|| false);
        let mut last_tap = use_state(|| None::<Instant>);
        let reading = use_location_reading(self.map.clone(), mode, followed_to, zoom_on_fix);
        let current = *mode.read();

        let (svg, color) = match current {
            LocationMode::Off => (lucide::locate(), Color::from_rgb(60, 60, 60)),
            LocationMode::Follow => (lucide::locate_fixed(), Color::from_rgb(26, 115, 232)),
            LocationMode::Show => (lucide::locate(), Color::from_rgb(26, 115, 232)),
        };
        let mut map = self.map.clone();

        rect()
            .position(
                Position::new_absolute()
                    .bottom(32. + screen.insets.bottom)
                    .right(12.),
            )
            .layer(Layer::Overlay)
            .width(Size::px(44.))
            .height(Size::px(44.))
            .center()
            .corner_radius(22.0f32)
            .background(Color::from_argb(245, 255, 255, 255))
            .shadow((
                0.0f32,
                1.0f32,
                4.0f32,
                0.0f32,
                Color::from_argb(60, 0, 0, 0),
            ))
            .cursor(CursorIcon::Pointer)
            .on_press(move |_: Event<PressEventData>| {
                haptic(Haptic::Toggle);
                let now = Instant::now();
                let double = last_tap
                    .peek()
                    .is_some_and(|at| now.duration_since(at) < DOUBLE_TAP);
                // A third fast tap starts over rather than making another double.
                last_tap.set((!double).then_some(now));

                if current == LocationMode::Off {
                    location::set_tracking(true);
                }
                mode.set(LocationMode::Follow);
                match *reading.peek() {
                    Reading::Fix(fix) => {
                        let zoom = double.then(|| closer_zoom(&map));
                        follow(&mut map, fix, screen, zoom);
                        followed_to.set(Some(*map.center.peek()));
                    }
                    // Centres on the first fix (see `use_location_reading`).
                    _ => zoom_on_fix.set(double),
                }
            })
            .child(
                SvgViewer::new(svg)
                    .width(Size::px(20.))
                    .height(Size::px(20.))
                    .color(color),
            )
    }
}

/// Mirrors each reading onto the map: moves the dot, and while following recentres on it —
/// unless the map moved since the last recentre, meaning the user panned away, which drops
/// following to only showing.
fn use_location_reading(
    mut map: MapController,
    mut mode: State<LocationMode>,
    mut followed_to: State<Option<CursorPoint>>,
    mut zoom_on_fix: State<bool>,
) -> State<Reading> {
    let reading = location::use_reading();
    let screen = use_consume::<Memo<Screen>>();
    use_side_effect(move || match *reading.read() {
        Reading::Fix(fix) => {
            map.set_user_location(Some(UserLocation::from_lonlat(
                fix.lon,
                fix.lat,
                fix.accuracy_m,
                fix.bearing,
            )));
            if *mode.peek() != LocationMode::Follow {
                return;
            }
            let panned = followed_to
                .peek()
                .is_some_and(|at| *map.center.peek() != at);
            if panned {
                mode.set(LocationMode::Show);
                return;
            }
            let zoom = (*zoom_on_fix.peek()).then(|| closer_zoom(&map));
            if zoom.is_some() {
                zoom_on_fix.set(false);
            }
            follow(&mut map, fix, *screen.peek(), zoom);
            followed_to.set(Some(*map.center.peek()));
        }
        Reading::Unavailable => {
            log::warn!("location unavailable: permission refused or location turned off");
            mode.set(LocationMode::Off);
            followed_to.set(None);
            zoom_on_fix.set(false);
            map.set_user_location(None);
        }
        Reading::None => {
            followed_to.set(None);
            map.set_user_location(None);
        }
    });
    reading
}

/// Where a double tap zooms to: street level from further out, else a bit closer.
fn closer_zoom(map: &MapController) -> f32 {
    let zoom = *map.zoom.peek();
    if zoom < FOLLOW_ZOOM {
        FOLLOW_ZOOM
    } else {
        zoom + FOLLOW_ZOOM_STEP
    }
}

/// Centres the map on `fix`, below the status bar, at `zoom` (capped by the style), else the
/// current one.
fn follow(map: &mut MapController, fix: location::Fix, screen: Screen, zoom: Option<f32>) {
    let zoom = zoom.unwrap_or(*map.zoom.peek());
    let point = lonlat_to_world(fix.lon, fix.lat);
    map.fit_world_bounds(
        (point, point),
        Insets {
            top: screen.insets.top,
            bottom: screen.insets.bottom,
            ..Insets::default()
        },
        zoom,
    );
}
