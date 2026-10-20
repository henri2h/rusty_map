//! Left-side search panel: a small floating pill (search input + Photon results dropdown) that
//! grows into a full-height drawer once a place is selected — either a POI tap or a picked
//! search result (both go through [`PlaceController`], see `place::SelectedPlace`) — showing that
//! place's details below the same search box.
//!
//! A sibling of `map_component::MapComponent`, not a child of it — both read/write the same
//! shared [`MapController`] instead of one owning the other's state. Rendered by `HomePage` as an
//! absolute overlay atop the map. A real [`Component`] (not a bare function) specifically so
//! typing/results/selection reads (`query`/`results`/`selected_place`) subscribe *this*
//! component's own reactive scope, not `HomePage`'s — otherwise every keystroke would re-render
//! `HomePage`'s whole child tree, `MapComponent` included, for no reason.
//!
//! Search debounce/geocoding ported (trimmed) from `shom_client`'s `app.rs` search panel:
//! debounce-via-cancel-and-resleep on every keystroke, minimum query length, location-biased
//! results.

use std::time::Duration;

use bytes::Bytes;
use freya::{icons::lucide, prelude::*};
use torin::{content::Content, position::Position, prelude::Direction};

use map_component::{
    Insets, MapController, MapMarker, MarkerGlyph, MarkerSet,
    util::{
        sprites::SpriteSheet,
        tiles::background_delay,
        viewport::{Viewport, center_to_lonlat, lonlat_to_center, max_zoom_ui, world_to_lonlat},
    },
};

use crate::{
    components::{
        charging::ChargerLive,
        favorites::{Favorite, Favorites},
        panoramax::StreetPhotos,
        place::{PlaceController, SelectedPlace, humanize},
        poi_icon::poi_icon,
        transit::{
            Departures, Endpoint, LineView, Planner, TrackView, TransitController, TransitView,
            TripView, central_european_offset,
        },
    },
    mobile::{
        Haptic, Pull, SHEET_PERCENT, Screen, SheetScroll, TOUCH, haptic, is_compact, on_pull_end,
        on_pull_move, on_pull_start, on_sheet_pull_start, share, tap_padding, text_size,
        use_screen, use_sheet_scroll,
    },
};

use super::{
    opening_hours::{OpenStatus, open_status, parse_opening_hours, when_text},
    search_controller::{SearchController, SearchStatus},
    util::search::{self, SearchResult},
};

/// Queries shorter than this just clear results instead of firing a request — Photon returns
/// mostly noise for single characters.
const MIN_QUERY_LEN: usize = 2;

/// Wait this long after the last keystroke before actually searching (debounce-via-cancel):
/// typing fires far more events than we want network requests for.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);

/// Zoom a picked search result is shown at (street level), capped by the style's max zoom.
const RESULT_ZOOM: f32 = 16.0;

const PANEL_WIDTH: f32 = 360.0;
const RESULTS_MAX_HEIGHT: f32 = 320.0;
const ICON_SIZE: f32 = 18.0;

/// A kind of place the category chips search the visible map for.
struct Category {
    label: &'static str,
    /// The POI kind whose icon the chip shows (see [`poi_icon`]).
    kind: &'static str,
    /// OSM `key.value` tags it covers (Photon `include` categories); a bare key covers all its
    /// values.
    tags: &'static [&'static str],
    /// What people type for it (lowercase, unaccented, French and English), searched as the
    /// category rather than as a name: few bakeries are *named* "boulangerie".
    words: &'static [&'static str],
}

const CATEGORIES: &[Category] = &[
    Category {
        label: "Restaurants",
        kind: "restaurant",
        tags: &["amenity.restaurant", "amenity.fast_food"],
        words: &["restaurant", "restaurants", "resto", "restos", "fast food"],
    },
    Category {
        label: "Cafés",
        kind: "cafe",
        tags: &["amenity.cafe"],
        words: &["cafe", "cafes", "coffee"],
    },
    Category {
        label: "Bakeries",
        kind: "bakery",
        tags: &["shop.bakery", "shop.pastry"],
        words: &[
            "boulangerie",
            "boulangeries",
            "patisserie",
            "bakery",
            "bakeries",
            "pain",
        ],
    },
    Category {
        label: "Groceries",
        kind: "supermarket",
        tags: &["shop.supermarket", "shop.convenience", "shop.greengrocer"],
        words: &[
            "supermarche",
            "supermarches",
            "epicerie",
            "superette",
            "courses",
            "grocery",
            "groceries",
            "supermarket",
        ],
    },
    Category {
        label: "Bars",
        kind: "bar",
        tags: &["amenity.bar", "amenity.pub"],
        words: &["bar", "bars", "pub", "pubs"],
    },
    Category {
        label: "Pharmacies",
        kind: "pharmacy",
        tags: &["amenity.pharmacy"],
        words: &["pharmacie", "pharmacies", "pharmacy"],
    },
    Category {
        label: "Health",
        kind: "doctors",
        tags: &[
            "amenity.doctors",
            "amenity.dentist",
            "amenity.clinic",
            "amenity.hospital",
            "healthcare",
        ],
        words: &[
            "medecin",
            "medecins",
            "docteur",
            "doctor",
            "doctors",
            "dentiste",
            "dentist",
            "sante",
            "health",
            "kine",
            "kinesitherapeute",
            "infirmier",
            "infirmiere",
            "hopital",
            "hospital",
            "clinique",
        ],
    },
    Category {
        label: "Fuel",
        kind: "fuel",
        tags: &["amenity.fuel"],
        words: &[
            "essence",
            "carburant",
            "station service",
            "fuel",
            "gas station",
        ],
    },
    Category {
        label: "Charging",
        kind: "charging_station",
        tags: &["amenity.charging_station"],
        words: &["recharge", "borne", "bornes", "charging", "charger"],
    },
    Category {
        label: "Parking",
        kind: "parking",
        tags: &["amenity.parking"],
        words: &["parking", "parkings"],
    },
    Category {
        label: "Hotels",
        kind: "hotel",
        tags: &["tourism.hotel", "tourism.hostel", "tourism.guest_house"],
        words: &["hotel", "hotels", "hebergement", "auberge"],
    },
    Category {
        label: "ATMs",
        kind: "atm",
        tags: &["amenity.atm"],
        words: &["distributeur", "dab", "atm", "atms", "cash"],
    },
];

/// The category `query` names (`"Boulangerie"`, `"médecin"`), if any.
fn category_for(query: &str) -> Option<&'static Category> {
    let wanted = fold(query);
    CATEGORIES.iter().find(|category| {
        fold(category.label) == wanted || category.words.contains(&wanted.as_str())
    })
}

/// Lowercase, accents dropped, spaces collapsed: `" Médecin "` → `"medecin"`.
fn fold(text: &str) -> String {
    let folded: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

const TEXT: Color = Color::from_rgb(32, 33, 36);
const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const TEXT_FAINT: Color = Color::from_rgb(140, 144, 150);
const ACCENT: Color = Color::from_rgb(26, 115, 232);
const DIVIDER: Color = Color::from_rgb(232, 234, 237);
const SURFACE_ALT: Color = Color::from_rgb(244, 246, 248);
const STAR: Color = Color::from_rgb(242, 153, 0);
const OPEN: Color = Color::from_rgb(24, 128, 56);
const CLOSING_SOON: Color = Color::from_rgb(227, 116, 0);
const CLOSED: Color = Color::from_rgb(197, 34, 31);

#[derive(PartialEq)]
pub struct SearchPanel {
    map: MapController,
    places: PlaceController,
    search: SearchController,
    transit: TransitController,
}

impl SearchPanel {
    pub fn new(
        map: MapController,
        places: PlaceController,
        search: SearchController,
        transit: TransitController,
    ) -> Self {
        Self {
            map,
            places,
            search,
            transit,
        }
    }
}

impl Component for SearchPanel {
    fn render(&self) -> impl IntoElement {
        let (map, places, search, transit) = (&self.map, &self.places, &self.search, &self.transit);
        let favorites = use_consume::<Favorites>();
        // Favorites show only while the search box has focus — when the user starts a search.
        let input_id = use_a11y();
        let input_focus = use_focus(input_id);
        let screen = use_screen();
        let mut sheet_expanded = use_state(|| false);
        let pull = use_state(Pull::default);
        // The sheet's scrolled content, so a pull anywhere on it doesn't fight its scrolling.
        let sheet_scroll = use_sheet_scroll();
        use_provide_context(move || sheet_scroll);
        // Each time the drawer opens, the sheet starts at half height.
        use_side_effect({
            let (selected, view, planner_open) =
                (places.selected_place, transit.view, transit.planner_open);
            move || {
                let open =
                    selected.read().is_some() || view.read().is_some() || *planner_open.read();
                if !open && *sheet_expanded.peek() {
                    sheet_expanded.set(false);
                }
            }
        });

        // The listed results, pinned on the map (tapping one picks it, see `HomePage`).
        use_side_effect({
            let (results, mut map) = (search.results, map.clone());
            move || {
                let markers: Vec<MapMarker> = results
                    .read()
                    .iter()
                    .enumerate()
                    .map(|(index, result)| {
                        MapMarker::from_lonlat(index as u64, result.lon, result.lat, PIN)
                    })
                    .collect();
                map.set_pins((!markers.is_empty()).then_some(MarkerSet {
                    markers,
                    selected: None,
                    glyph: MarkerGlyph::Dot,
                }));
            }
        });

        let on_validate = build_on_search_input(map, places, search);
        let has_query = !search.query.read().is_empty();
        let status = *search.status.read();
        let results = search.results.read().clone();
        let place = places.selected_place.read().clone();
        let sprites = map.sprites.read().clone();
        let view = transit.view.read().clone();
        let planner_open = *transit.planner_open.read();
        let drawer_open = place.is_some() || view.is_some() || planner_open;
        // Compact layout: a search in progress takes the bottom sheet too, the results under
        // the box.
        let searching = screen.compact
            && !drawer_open
            && (input_focus().is_focused() || has_query || status != SearchStatus::Idle);
        let sheet = screen.compact && (drawer_open || searching);
        let dropdown = (!drawer_open && status != SearchStatus::Idle).then(|| {
            results_dropdown(
                status,
                results,
                searching,
                sheet_scroll,
                map,
                places,
                search,
            )
        });
        let rounded = !drawer_open && dropdown.is_none();

        let shadow = if drawer_open {
            // Right-edge shadow, falling into the map — the drawer spans the full height flush
            // against the left window edge.
            (
                2.0f32,
                0.0f32,
                12.0f32,
                0.0f32,
                Color::from_argb(60, 0, 0, 0),
            )
        } else {
            // Bottom/ambient shadow for the small floating pill.
            (
                0.0f32,
                2.0f32,
                10.0f32,
                0.0f32,
                Color::from_argb(70, 0, 0, 0),
            )
        };

        let mut input = Input::new(search.query)
            .placeholder("Search places or addresses…")
            .flat()
            // The pill/drawer around it is the visible box: the input's own background and
            // (focus) border would otherwise show as a second box inset by the pill's padding.
            .theme_colors(
                InputColorsThemePartial::new()
                    .background(Color::TRANSPARENT)
                    .focus_background(Color::TRANSPARENT)
                    .border_fill(Color::TRANSPARENT)
                    .focus_border_fill(Color::TRANSPARENT),
            )
            .width(Size::fill())
            .leading(if searching {
                // The search sheet's visible way out (as are back and pulling the sheet down).
                let mut search = search.clone();
                rect()
                    .cursor(CursorIcon::Pointer)
                    .child(
                        icon(lucide::arrow_left(), TEXT_MUTED)
                            // The input claims the pointer otherwise (see the ✕ below).
                            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
                            .on_press(move |e: Event<PressEventData>| {
                                e.stop_propagation();
                                e.prevent_default();
                                haptic(Haptic::Press);
                                search.reset(String::new());
                                input_id.request_unfocus();
                            }),
                    )
                    .into_element()
            } else {
                icon(lucide::search(), TEXT_FAINT).into_element()
            })
            .a11y_id(input_id)
            .on_validate(on_validate)
            // Enter: every match on the visible map, not just the few best anywhere.
            .on_submit({
                let map = map.clone();
                let mut places = places.clone();
                let mut search = search.clone();
                move |text: String| {
                    places.clear();
                    search_query_in_view(text, &map, &mut search);
                }
            });
        if !has_query {
            let mut transit = transit.clone();
            input = input.trailing(
                rect().cursor(CursorIcon::Pointer).child(
                    icon(lucide::route(), ACCENT)
                        .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
                        .on_press(move |e: Event<PressEventData>| {
                            e.stop_propagation();
                            e.prevent_default();
                            haptic(Haptic::Press);
                            transit.open_planner(None);
                        }),
                ),
            );
        } else {
            let mut places = places.clone();
            let mut search = search.clone();
            let mut transit = transit.clone();
            input = input.trailing(
                rect().cursor(CursorIcon::Pointer).child(
                    icon(lucide::x(), TEXT_MUTED)
                        // The input claims the pointer otherwise: the press would just move the
                        // text cursor instead of reaching this handler.
                        .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
                        .on_press(move |e: Event<PressEventData>| {
                            e.stop_propagation();
                            e.prevent_default();
                            search.reset(String::new());
                            places.clear();
                            transit.close_view();
                        }),
                ),
            );
        }

        let content: Vec<Element> = if planner_open {
            vec![Planner {}.into_element()]
        } else {
            let mut content = vec![input.into_element()];
            if !drawer_open && input_focus().is_focused() {
                // Typing narrows them down to the matching names.
                let query = search.query.read().trim().to_lowercase();
                let saved: Vec<Favorite> = favorites
                    .list
                    .read()
                    .iter()
                    .filter(|favorite| favorite.name.to_lowercase().contains(&query))
                    .cloned()
                    .collect();
                if !saved.is_empty() {
                    content.push(favorite_chips(saved, map, places, search));
                }
                let categories: Vec<&'static Category> = CATEGORIES
                    .iter()
                    .filter(|category| {
                        let query = fold(&query);
                        fold(category.label).contains(&query)
                            || category.words.iter().any(|word| word.starts_with(&query))
                    })
                    .collect();
                if !categories.is_empty() {
                    content.push(category_chips(categories, sprites.as_deref(), map, search));
                }
            }
            content.extend(dropdown);
            if let Some(view) = view {
                content.push(transit_view(view, transit.clone(), sheet_scroll));
            } else if let Some(place) = place {
                content.push(
                    place_details(place, transit.clone(), favorites, sheet_scroll).into_element(),
                );
            }
            content
        };

        let on_back = {
            let mut places = places.clone();
            let mut search = search.clone();
            let mut transit = transit.clone();
            move |e: Event<KeyboardEventData>| {
                if e.key != Key::Named(NamedKey::BrowserBack) {
                    return;
                }
                // Back closes the photo viewer first.
                if crate::components::panoramax::viewer_handles_back() {
                    return;
                }
                // The pick banner's own handler stops picking.
                if *transit.picking.peek() && *transit.planner_open.peek() {
                    return;
                }
                if transit.view.peek().is_some() {
                    transit.close_view();
                } else if *transit.planner_open.peek() {
                    transit.close_planner();
                } else {
                    search.reset(String::new());
                    places.clear();
                    input_id.request_unfocus();
                }
            }
        };

        let panel = if sheet {
            bottom_sheet(screen, *sheet_expanded.read(), pull.read().offset)
        } else if screen.compact {
            rect()
                .position(Position::new_absolute().top(0.0f32).left(0.0f32))
                // The window's width less the margins on both sides.
                .width(Size::func_data(|c| Some(c.root - 24.0), &"pill"))
                .margin((12.0 + screen.insets.top, 12.0, 12.0, 12.0))
                .padding(8.0f32)
        } else {
            rect()
                .position(Position::new_absolute().top(0.0f32).left(0.0f32))
                .width(Size::px(PANEL_WIDTH))
                .maybe(drawer_open, |r| r.height(Size::percent(100.)))
                .maybe(!drawer_open, |r| {
                    r.margin((12.0 + screen.insets.top, 12.0, 12.0, 12.0))
                })
                .padding(if drawer_open {
                    (
                        16.0 + screen.insets.top,
                        16.0,
                        16.0 + screen.insets.bottom,
                        16.0,
                    )
                } else {
                    (8.0, 8.0, 8.0, 8.0)
                })
        };
        // What pulling the sheet down (or tapping its bar) does: end the search; else shrink a
        // full-height sheet; else close what it shows, as back does.
        let close_sheet = {
            let mut places = places.clone();
            let mut search = search.clone();
            let mut transit = transit.clone();
            move || {
                if searching {
                    search.reset(String::new());
                    input_id.request_unfocus();
                } else if *sheet_expanded.peek() {
                    sheet_expanded.set(false);
                } else if transit.view.peek().is_some() {
                    transit.close_view();
                } else if *transit.planner_open.peek() {
                    transit.close_planner();
                } else {
                    search.reset(String::new());
                    places.clear();
                }
            }
        };
        let handle = if searching {
            sheet_handle(pull, close_sheet.clone())
        } else if sheet {
            sheet_handle(pull, move || sheet_expanded.toggle())
        } else {
            // Keeps the children's positions, and so the input and its focus, when the pill
            // turns into the sheet.
            rect().into_element()
        };

        panel
            .background(Color::from_argb(250, 255, 255, 255))
            .maybe(!sheet, |r| {
                r.corner_radius(match (drawer_open, rounded) {
                    (true, _) => 0.0f32,
                    (false, true) => 24.0f32,
                    (false, false) => 12.0f32,
                })
                .shadow(shadow)
            })
            // Without this, the map canvas's own paint callback (rebuilt fresh on every
            // zoom-triggered `.key()` change) can land after this element in the base layer's
            // paint order and draw over it — seen as the panel flickering while zooming.
            // Level 2, not `Overlay`: Freya paints one layer's nodes in no set order, and the
            // bottom sheet must cover the map controls (`Overlay`, ~2047 + depth — above level
            // 1). Under the favorites page (3) and the layers modal (4).
            .layer(Layer::OverlayLevel(2))
            .vertical()
            .on_global_key_down(on_back)
            // The whole sheet pulls down, not only its grab bar (see `on_sheet_pull_start`).
            .maybe(sheet, |r| {
                r.on_pointer_down(on_sheet_pull_start(pull, sheet_scroll))
            })
            .on_global_pointer_move(on_pull_move(pull))
            .on_global_pointer_up(on_pull_end(pull, close_sheet))
            .child(handle)
            .children(content)
    }
}

/// Compact layout's drawer and search: a sheet along the bottom edge, leaving the top of the map
/// visible, or `full` height; `pulled` down by that much (see [`Pull`]).
fn bottom_sheet(screen: Screen, full: bool, pulled: f32) -> Rect {
    let top = screen.insets.top;
    rect()
        .position(Position::new_absolute().bottom(-pulled).left(0.0f32))
        .width(Size::window_percent(100.))
        .height(if full {
            Size::func_data(move |c| Some(c.root - top), &(top as u32))
        } else {
            Size::window_percent(SHEET_PERCENT)
        })
        .padding((0.0, 16.0, 16.0 + screen.insets.bottom, 16.0))
        .corner_radius((16.0f32, 16.0f32, 0.0f32, 0.0f32))
        .shadow((
            0.0f32,
            -2.0f32,
            12.0f32,
            0.0f32,
            Color::from_argb(60, 0, 0, 0),
        ))
}

/// The grab bar atop the bottom sheet: pulling it down closes the sheet (see `close_sheet`); a
/// tap toggles a place's sheet between half and full height, or closes the search. Pulls start
/// here only, so scrolling the sheet's content never drags it.
fn sheet_handle(pull: State<Pull>, mut on_press: impl FnMut() + 'static) -> Element {
    rect()
        .width(Size::fill())
        .padding((tap_padding(10.0), 0.0))
        .center()
        .on_pointer_down(on_pull_start(pull))
        .on_press(move |_: Event<PressEventData>| {
            // The release of a pull, not a tap.
            if pull.peek().moved {
                return;
            }
            haptic(Haptic::Press);
            on_press();
        })
        .child(
            rect()
                .width(Size::px(36.0))
                .height(Size::px(4.0))
                .corner_radius(2.0f32)
                .background(DIVIDER),
        )
        .into_element()
}

/// A transit view (tapped track, a line, or a trip) under a "Back" row returning to what was there.
fn transit_view(view: TransitView, transit: TransitController, scroll: SheetScroll) -> Element {
    let mut close = transit.clone();
    let content = match view {
        TransitView::Line { id, route, way_id } => LineView { id, route, way_id }.into_element(),
        TransitView::Track {
            id,
            way_id,
            lat,
            lon,
            radius_m,
            kind,
            from_tile,
        } => TrackView {
            id,
            way_id,
            lat,
            lon,
            radius_m,
            kind,
            from_tile,
        }
        .into_element(),
        TransitView::Trip {
            id,
            departure,
            from_stop,
            stop_position,
        } => TripView {
            id,
            departure,
            from_stop,
            stop_position,
        }
        .into_element(),
    };
    rect()
        .vertical()
        .width(Size::fill())
        .height(Size::fill())
        .margin((8.0f32, 0.0f32, 0.0f32, 0.0f32))
        .child(
            Button::new()
                .flat()
                .maybe(!TOUCH, |button| button.compact())
                .on_press(move |_: Event<PressEventData>| close.close_view())
                .child(
                    rect()
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(6.0)
                        .child(icon(lucide::arrow_left(), TEXT_MUTED))
                        .child(
                            label()
                                .text("Back")
                                .font_size(text_size(13.0))
                                .color(TEXT_MUTED),
                        ),
                ),
        )
        .child(
            scroll
                .track(ScrollView::new())
                .height(Size::fill())
                .direction(Direction::vertical())
                .margin((8.0f32, 0.0f32, 0.0f32, 0.0f32))
                .child(scroll.marker())
                .child(content),
        )
        .into_element()
}

/// A lucide icon at the standard row size.
fn icon(svg: Bytes, color: Color) -> SvgViewer {
    SvgViewer::new(svg)
        .width(Size::px(ICON_SIZE))
        .height(Size::px(ICON_SIZE))
        .color(color)
}

fn divider() -> impl IntoElement {
    rect()
        .width(Size::fill())
        .height(Size::px(1.0))
        .background(DIVIDER)
}

/// The pill's dropdown: the result rows, or a one-line status when there's nothing to list yet.
/// Older results stay visible while a refined query is still debouncing/in flight, rather than
/// flashing to "Searching…" on every keystroke. `fill`: down to the bottom of the search sheet
/// (compact layout) rather than a capped dropdown.
fn results_dropdown(
    status: SearchStatus,
    results: Vec<SearchResult>,
    fill: bool,
    scroll: SheetScroll,
    map: &MapController,
    places: &PlaceController,
    search: &SearchController,
) -> Element {
    let sprites = map.sprites.read().clone();
    // Distances from the user when located, else from the middle of the visible map.
    let origin = map
        .user_location
        .read()
        .as_ref()
        .map(|location| {
            let (lon, lat) = world_to_lonlat(location.point.0, location.point.1);
            (lat, lon)
        })
        .unwrap_or_else(|| {
            let (_, (lon, lat)) = visible_area(map);
            (lat, lon)
        });
    let body = if !results.is_empty() {
        scroll
            .track(ScrollView::new())
            // `ScrollView` defaults to `height: fill()`, which resolves to nothing inside this
            // panel's content-sized pill height — shrink to the rows' own height instead,
            // capped by `max_height`.
            .height(if fill { Size::fill() } else { Size::auto() })
            .maybe(!fill, |r| r.max_height(Size::px(RESULTS_MAX_HEIGHT)))
            .direction(Direction::vertical())
            .child(scroll.marker())
            .children(results.into_iter().map(|result| {
                result_row(result, origin, sprites.as_deref(), map, places, search).into_element()
            }))
            .into_element()
    } else {
        let (svg, text) = match status {
            SearchStatus::Searching => (lucide::loader_circle(), "Searching…"),
            SearchStatus::Failed => (
                lucide::circle_alert(),
                "Search failed — check your connection",
            ),
            SearchStatus::Done | SearchStatus::Idle => (lucide::search(), "No results"),
        };
        rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(10.0)
            .padding((12.0f32, 8.0f32))
            .child(icon(svg, TEXT_FAINT))
            .child(
                label()
                    .text(text)
                    .font_size(text_size(13.0))
                    .color(TEXT_MUTED),
            )
            .into_element()
    };

    rect()
        .vertical()
        .width(Size::fill())
        .maybe(fill, |r| r.height(Size::fill()))
        .padding((4.0f32, 0.0f32, 0.0f32, 0.0f32))
        .child(divider())
        .child(body)
        .into_element()
}

fn result_row(
    result: SearchResult,
    (origin_lat, origin_lon): (f64, f64),
    sprites: Option<&SpriteSheet>,
    map: &MapController,
    places: &PlaceController,
    search: &SearchController,
) -> impl IntoElement {
    let title = result.title().to_string();
    let subtitle = [humanize(&result.category), result.display_name.clone()]
        .into_iter()
        .filter(|part| !part.is_empty() && *part != title)
        .collect::<Vec<_>>()
        .join(" · ");
    let distance = search::format_distance(search::distance_m(
        origin_lat, origin_lon, result.lat, result.lon,
    ));
    let icon = poi_icon(
        sprites,
        &result.category,
        Some(&result.osm_key),
        ICON_SIZE,
        TEXT_MUTED,
    );

    let map = map.clone();
    let mut places = places.clone();
    let mut search = search.clone();

    Button::new()
        .flat()
        .width(Size::fill())
        .on_press(move |_: Event<PressEventData>| {
            pick_result(&result, &map, &mut places, &mut search)
        })
        .child(
            rect()
                .horizontal()
                .content(Content::flex())
                .width(Size::fill())
                .cross_align(Alignment::Center)
                .spacing(12.0)
                .padding((tap_padding(6.0), 4.0))
                .child(icon)
                .child(
                    rect()
                        .vertical()
                        .width(Size::flex(1.0))
                        .child(
                            label()
                                .text(title)
                                .font_size(text_size(14.0))
                                .color(TEXT)
                                .max_lines(1)
                                .text_overflow(TextOverflow::Ellipsis),
                        )
                        .maybe(!subtitle.is_empty(), |r| {
                            r.child(
                                label()
                                    .text(subtitle)
                                    .font_size(text_size(12.0))
                                    .color(TEXT_MUTED)
                                    .max_lines(1)
                                    .text_overflow(TextOverflow::Ellipsis),
                            )
                        }),
                )
                .child(
                    label()
                        .text(distance)
                        .font_size(text_size(12.0))
                        .color(TEXT_FAINT),
                ),
        )
}

/// Shows a search result: the map centred on it, its details in the drawer.
pub fn pick_result(
    result: &SearchResult,
    map: &MapController,
    places: &mut PlaceController,
    search: &mut SearchController,
) {
    fly_to(map, result.lat, result.lon);
    search.reset(result.title().to_string());
    places.select_search_result(result, map.platform.clone());
}

/// Colour of the search result pins on the map.
const PIN: Color = Color::from_rgb(234, 67, 53);

/// Centres the map on (`lat`, `lon`) at street level (capped by the style's max zoom), in the
/// part of the map the drawer leaves visible — the callers all open the drawer.
pub fn fly_to(map: &MapController, lat: f64, lon: f64) {
    let (mut center, mut zoom) = (map.center, map.zoom);
    let max_real_zoom = map.style.borrow().max_real_zoom;
    let target_zoom = RESULT_ZOOM.min(max_zoom_ui(max_real_zoom));
    // `lonlat_to_center` alone would put the target at the viewport's top-left corner (that's
    // what `center` actually anchors — see `Viewport`'s own doc comment), not its visual
    // middle. Shift by the free area's middle in pixels, converted through the same
    // `pixels_to_center_delta` panning uses, so the target lands where the user is looking.
    let area = map.viewport_size.peek().size;
    // The drawer covers the left side; in the compact layout, the bottom sheet the lower part.
    let (free_x, free_y) = if is_compact(area.width) {
        (
            area.width / 2.0,
            area.height * (1.0 - SHEET_PERCENT / 100.0) / 2.0,
        )
    } else {
        let free_width = (area.width - PANEL_WIDTH).max(0.0);
        (area.width - free_width / 2.0, area.height / 2.0)
    };
    let half_offset = Viewport::new(CursorPoint::zero(), target_zoom, max_real_zoom)
        .pixels_to_center_delta(free_x as f64, free_y as f64);
    let raw_center = lonlat_to_center(lon, lat);
    center.set(CursorPoint::new(
        raw_center.x - half_offset.x,
        raw_center.y - half_offset.y,
    ));
    zoom.set(target_zoom);
    // `zoom` may end up unchanged (e.g. picking another result while already at the cap), so
    // the canvas's `.key()` alone can't be relied on to force a repaint.
    map.platform.send(UserEvent::RequestRedraw);
}

/// Saved places under the idle search pill: one tap shows the place.
fn favorite_chips(
    favorites: Vec<Favorite>,
    map: &MapController,
    places: &PlaceController,
    search: &SearchController,
) -> Element {
    let place_chips = favorites.into_iter().map(|favorite| {
        let map = map.clone();
        let mut places = places.clone();
        let mut search = search.clone();
        let name = favorite.name.clone();
        favorite_chip(&name, favorite.stop, move || {
            fly_to(&map, favorite.lat, favorite.lon);
            search.reset(favorite.name.clone());
            places.select_favorite(&favorite, map.platform.clone());
        })
    });
    ScrollView::new()
        .direction(Direction::horizontal())
        .width(Size::fill())
        .height(Size::auto())
        .show_scrollbar(false)
        .child(
            rect()
                .horizontal()
                .spacing(6.0)
                .padding((6.0f32, 4.0f32, 2.0f32, 4.0f32))
                .children(place_chips),
        )
        .into_element()
}

/// Kinds of place to look for on the visible map, under the idle search pill: one tap lists them.
fn category_chips(
    categories: Vec<&'static Category>,
    sprites: Option<&SpriteSheet>,
    map: &MapController,
    search: &SearchController,
) -> Element {
    let chips = categories.into_iter().map(|category| {
        let map = map.clone();
        let mut search = search.clone();
        chip(
            poi_icon(sprites, category.kind, None, 14.0, TEXT_MUTED),
            category.label,
            move || search_category(category, &map, &mut search),
        )
    });
    ScrollView::new()
        .direction(Direction::horizontal())
        .width(Size::fill())
        .height(Size::auto())
        .show_scrollbar(false)
        .child(
            rect()
                .horizontal()
                .spacing(6.0)
                .padding((6.0f32, 4.0f32, 2.0f32, 4.0f32))
                .children(chips),
        )
        .into_element()
}

/// Lists the `category`'s places on the visible map in the results dropdown, nearest to its
/// centre first, under the category's name in the search box.
fn search_category(
    category: &'static Category,
    map: &MapController,
    search: &mut SearchController,
) {
    search_visible(
        category.label.to_string(),
        map,
        search,
        move |bounds, center| search::search_category(category.tags, bounds, center),
    );
}

/// Lists every match for `query` on the visible map (Enter in the search box), nearest first.
fn search_query_in_view(query: String, map: &MapController, search: &mut SearchController) {
    if query.trim().chars().count() < MIN_QUERY_LEN {
        return;
    }
    if let Some(category) = category_for(&query) {
        search_visible(query, map, search, move |bounds, center| {
            search::search_category(category.tags, bounds, center)
        });
        return;
    }
    let text = query.trim().to_string();
    search_visible(query, map, search, move |bounds, center| {
        search::search_in_area(text, bounds, center)
    });
}

/// `((min_lon, min_lat), (max_lon, max_lat))`.
type LonLatBounds = ((f64, f64), (f64, f64));

/// The part of the map not covered by the panel, as `(bounds, centre)` lon/lat (see
/// `MapController::visible_lonlat_bounds`).
fn visible_area(map: &MapController) -> (LonLatBounds, (f64, f64)) {
    let area = map.viewport_size.peek().size;
    let insets = if is_compact(area.width) {
        Insets {
            bottom: area.height * SHEET_PERCENT / 100.0,
            ..Insets::default()
        }
    } else {
        Insets {
            left: PANEL_WIDTH + 12.0,
            ..Insets::default()
        }
    };
    let bounds @ ((min_lon, min_lat), (max_lon, max_lat)) = map.visible_lonlat_bounds(insets);
    (
        bounds,
        ((min_lon + max_lon) / 2.0, (min_lat + max_lat) / 2.0),
    )
}

/// Shows `query` in the search box and `fetch`'s places on the visible map in the results
/// dropdown.
fn search_visible<Fut>(
    query: String,
    map: &MapController,
    search: &mut SearchController,
    fetch: impl FnOnce(((f64, f64), (f64, f64)), (f64, f64)) -> Fut,
) where
    Fut: std::future::Future<
            Output = Result<Vec<SearchResult>, map_component::util::tiles::FetchError>,
        > + 'static,
{
    search.reset(query);
    search.status.set(SearchStatus::Searching);
    let (bounds, center) = visible_area(map);
    let request = fetch(bounds, center);
    let (mut results, mut status) = (search.results, search.status);
    let platform = map.platform.clone();
    // `spawn_in_window`, not `spawn`: a chip's press runs in the scope of the chip row's
    // `ScrollView`, unmounted as soon as the input loses focus — which cancelled the request
    // and left "Searching…" forever. `search.task` still lets a new search cancel it.
    let handle = spawn_in_window(async move {
        match request.await {
            Ok(list) => {
                results.set(list);
                status.set(SearchStatus::Done);
            }
            Err(err) => {
                log::warn!("[search] area request failed: {err:?}");
                results.set(Vec::new());
                status.set(SearchStatus::Failed);
            }
        }
        platform.send(UserEvent::RequestRedraw);
    });
    search.task.set(Some(handle));
}

/// A saved place as a small rounded button: star (or a train for stops) plus its name.
pub fn favorite_chip(name: &str, stop: bool, on_press: impl FnMut() + 'static) -> Element {
    let icon = SvgViewer::new(if stop {
        lucide::train_front()
    } else {
        lucide::star()
    })
    .width(Size::px(13.0))
    .height(Size::px(13.0))
    .color(STAR);
    chip(icon.into_element(), name, on_press)
}

/// A small rounded button: an icon plus a name.
fn chip(icon: Element, name: &str, mut on_press: impl FnMut() + 'static) -> Element {
    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(5.0)
        .padding((tap_padding(4.0), 10.0))
        .corner_radius(14.0f32)
        .background(SURFACE_ALT)
        .cursor(CursorIcon::Pointer)
        .on_press(move |_: Event<PressEventData>| {
            haptic(Haptic::Press);
            on_press();
        })
        .child(icon)
        .child(
            label()
                .text(name.to_string())
                .font_size(text_size(12.0))
                .color(TEXT)
                .max_lines(1),
        )
        .into_element()
}

fn build_on_search_input(
    map: &MapController,
    places: &PlaceController,
    search: &SearchController,
) -> impl FnMut(InputValidator) + 'static {
    let mut search = search.clone();
    let mut places = places.clone();
    let center = map.center;
    let platform = map.platform.clone();
    let map = map.clone();

    move |validator: InputValidator| {
        let text = validator.text().clone();
        search.cancel();

        // Editing the box always drops whatever place was shown (a POI tap, or an earlier
        // result — both set this same text) — typing means the user is searching again, not
        // looking at that place anymore.
        places.clear();

        if text.trim().chars().count() < MIN_QUERY_LEN {
            search.results.set(Vec::new());
            search.status.set(SearchStatus::Idle);
            return;
        }

        if *search.status.peek() != SearchStatus::Searching {
            search.status.set(SearchStatus::Searching);
        }
        let bias = Some(center_to_lonlat(*center.peek()));
        // A category's name ("boulangerie") lists that kind of place on the visible map.
        let category = category_for(&text).map(|category| (category, visible_area(&map)));
        let mut results = search.results;
        let mut status = search.status;
        let platform = platform.clone();
        let handle = spawn(async move {
            background_delay(SEARCH_DEBOUNCE).await;
            // IGN's geocoder (France) answers in a fraction of a second, Photon in several: show
            // IGN's results meanwhile, then Photon's (OSM, full details) once in.
            let mut quick = false;
            let found = match category {
                Some((category, (bounds, center))) => {
                    search::search_category(category.tags, bounds, center).await
                }
                None => {
                    let mut photon = std::pin::pin!(search::search_address(text.clone(), bias));
                    let first = tokio::select! {
                        biased;
                        found = &mut photon => Some(found),
                        ign = search::search_ign(text, bias) => {
                            if let Ok(list) = ign
                                && !list.is_empty()
                            {
                                results.set(list);
                                quick = true;
                                platform.send(UserEvent::RequestRedraw);
                            }
                            None
                        }
                    };
                    match first {
                        Some(found) => found,
                        None => photon.await,
                    }
                }
            };
            // Photon failed or found nothing: IGN's results stay.
            if quick && !matches!(&found, Ok(list) if !list.is_empty()) {
                if let Err(err) = &found {
                    log::warn!("[search] Photon failed, keeping IGN results: {err:?}");
                }
                status.set(SearchStatus::Done);
                platform.send(UserEvent::RequestRedraw);
                return;
            }
            match found {
                Ok(list) => {
                    results.set(list);
                    status.set(SearchStatus::Done);
                }
                Err(err) => {
                    log::warn!("[search] request failed: {err:?}");
                    results.set(Vec::new());
                    status.set(SearchStatus::Failed);
                }
            }
            platform.send(UserEvent::RequestRedraw);
        });
        search.task.set(Some(handle));
    }
}

/// Drawer body below the search box. The search box above already shows the place's name and
/// its own ✕ closes the drawer, so there's no second title/close header here. One continuous
/// `ScrollView` for everything (not several small independent ones), so content past the
/// window's bottom edge is actually reachable instead of silently clipped.
fn place_details(
    place: SelectedPlace,
    transit: TransitController,
    mut favorites: Favorites,
    scroll: SheetScroll,
) -> impl IntoElement {
    let favorite = Favorite::from_place(&place);
    let saved = favorite
        .as_ref()
        .is_some_and(|favorite| favorites.contains(&favorite.name, favorite.lat, favorite.lon));
    let SelectedPlace {
        id,
        title,
        subtitle,
        mut details,
        mut raw_tags,
        links,
        loading,
        image,
        transit_stop,
        coords,
        charger,
        ..
    } = place;
    // The street-level photo mappers picked, shown first by the Panoramax strip.
    let panoramax_tag = raw_tags
        .iter()
        .position(|(key, _)| key == "panoramax")
        .map(|index| raw_tags.remove(index).1);

    // The Wikipedia intro is a paragraph, not a label/value row — shown as its own block.
    let wikipedia = details
        .iter()
        .position(|(key, _)| key == "Wikipedia")
        .map(|index| details.remove(index).1);
    // Where-and-when first (the facts most POI lookups are after), provenance metadata last.
    // `sort_by_key` is stable, so everything else keeps its curated order.
    details.sort_by_key(|(key, _)| match key.as_str() {
        "Address" => 0,
        "Opening hours" => 1,
        "Coordinates" => 3,
        "SIRET" | "Source" | "Source date" | "Last checked" | "Last edited" => 4,
        _ => 2,
    });
    let is_empty = details.is_empty() && links.is_empty() && wikipedia.is_none();

    let mut body: Vec<Element> = Vec::new();
    if let Some(image) = image {
        body.push(
            rect()
                .width(Size::fill())
                .height(Size::px(180.0))
                .corner_radius(12.0f32)
                .overflow(Overflow::Clip)
                .margin((0.0f32, 0.0f32, 12.0f32, 0.0f32))
                .child(
                    ImageViewer::new(image)
                        .width(Size::fill())
                        .height(Size::fill())
                        .aspect_ratio(AspectRatio::Max)
                        .image_cover(ImageCover::Center),
                )
                .into_element(),
        );
    }
    if !subtitle.is_empty() {
        body.push(
            rect()
                .padding((4.0f32, 10.0f32))
                .corner_radius(12.0f32)
                .background(SURFACE_ALT)
                .margin((0.0f32, 0.0f32, 12.0f32, 0.0f32))
                .child(
                    label()
                        .text(subtitle)
                        .font_size(text_size(12.0))
                        .color(TEXT_MUTED),
                )
                .into_element(),
        );
    }
    if let Some((lat, lon)) = coords {
        let mut transit = transit.clone();
        let name = title.clone();
        let directions = Button::new()
            .filled()
            .width(Size::flex(1.0))
            .on_press(move |_: Event<PressEventData>| {
                transit.open_planner(Some(Endpoint {
                    name: name.clone(),
                    lat,
                    lon,
                }))
            })
            .child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(6.0)
                    .child(icon(lucide::route(), Color::WHITE))
                    .child(
                        label()
                            .text("Directions")
                            .font_size(text_size(13.0))
                            .color(Color::WHITE),
                    ),
            );
        // Save / unsave this place.
        let star = favorite.map(|favorite| {
            Button::new()
                .flat()
                .on_press(move |_: Event<PressEventData>| favorites.toggle(favorite.clone()))
                .child(
                    rect()
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(6.0)
                        .child(icon(
                            if saved {
                                lucide::star_off()
                            } else {
                                lucide::star()
                            },
                            STAR,
                        ))
                        .child(
                            label()
                                .text(if saved { "Saved" } else { "Save" })
                                .font_size(text_size(13.0))
                                .color(TEXT),
                        ),
                )
                .into_element()
        });
        body.push(
            rect()
                .horizontal()
                .content(Content::flex())
                .width(Size::fill())
                .cross_align(Alignment::Center)
                .spacing(8.0)
                .child(directions)
                .maybe_child(star)
                .into_element(),
        );
        body.push(rect().height(Size::px(8.0)).into_element());
    }
    if !links.is_empty() {
        // Two per row, each half the width: a wrapping row would shrink a chip's label onto two
        // lines instead of moving the chip down (text always "fits" by wrapping).
        let chips: Vec<_> = links
            .into_iter()
            .map(|(label, _, url)| (label, url))
            .collect();
        for pair in chips.chunks(2) {
            body.push(
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .spacing(8.0)
                    .margin((0.0f32, 0.0f32, 8.0f32, 0.0f32))
                    .children(
                        pair.iter()
                            .cloned()
                            .map(|(label, url)| link_chip(label, url, &title).into_element()),
                    )
                    .maybe(pair.len() == 1, |r| r.child(rect().width(Size::flex(1.0))))
                    .into_element(),
            );
        }
    }
    if let Some(station_id) = charger {
        body.push(
            ChargerLive {
                place_id: id,
                station_id,
            }
            .into_element(),
        );
    }
    if let Some((lat, lon)) = transit_stop {
        // OSM numbers a bus station's platforms (`local_ref=4`, or `ref=Quai 4`).
        let platform = details
            .iter()
            .find(|(key, _)| key == "Platform")
            .or_else(|| {
                raw_tags
                    .iter()
                    .find(|(key, _)| key == "ref" || key == "local_ref")
            })
            .map(|(_, value)| value.clone());
        body.push(
            Departures {
                place_id: id,
                lat,
                lon,
                name: title,
                line: None,
                platform,
            }
            .into_element(),
        );
    }
    if loading {
        body.push(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(10.0)
                .padding((8.0f32, 0.0f32))
                .child(CircularLoader::new().size(18.0))
                .child(
                    label()
                        .text("Loading details…")
                        .font_size(text_size(12.0))
                        .color(TEXT_FAINT),
                )
                .into_element(),
        );
    } else if is_empty {
        body.push(
            label()
                .text("No further details for this place.")
                .font_size(text_size(12.0))
                .color(TEXT_FAINT)
                .margin((8.0f32, 0.0f32))
                .into_element(),
        );
    }
    for (index, (key, value)) in details.into_iter().enumerate() {
        if index > 0 {
            body.push(divider().into_element());
        }
        body.push(detail_row(key, value));
    }
    // After the rows: address/hours/phone are what most lookups are after, and a multi-paragraph
    // intro above them would push them below the fold.
    if let Some(intro) = wikipedia {
        body.push(wikipedia_block(intro).into_element());
    }
    if let Some((lat, lon)) = coords {
        body.push(
            StreetPhotos {
                place_id: id,
                lat,
                lon,
                picture: panoramax_tag,
            }
            .into_element(),
        );
    }
    if !raw_tags.is_empty() {
        body.push(
            RawTags {
                place_id: id,
                tags: raw_tags,
            }
            .into_element(),
        );
    }

    scroll
        .track(ScrollView::new())
        .height(Size::fill())
        .direction(Direction::vertical())
        .margin((16.0f32, 0.0f32, 0.0f32, 0.0f32))
        .child(scroll.marker())
        .children(body)
}

/// Icon for a detail/link row label, when there's a fitting one.
fn detail_icon(label: &str) -> Option<Bytes> {
    Some(match label {
        "Address" => lucide::map_pin(),
        "Coordinates" => lucide::navigation(),
        "Opening hours" => lucide::clock(),
        "Phone" => lucide::phone(),
        "Email" => lucide::mail(),
        "Website" => lucide::globe(),
        "Wheelchair access" => lucide::accessibility(),
        "Fee" | "Fare" => lucide::coins(),
        "Cuisine" => lucide::utensils(),
        "Amenity" | "Shop" | "Craft" => lucide::store(),
        "Office" => lucide::briefcase(),
        "Healthcare" | "Speciality" => lucide::stethoscope(),
        "Tourism" | "Leisure" => lucide::camera(),
        "Historic" => lucide::landmark(),
        "Artwork" | "Artist" => lucide::palette(),
        "Building" | "Land use" => lucide::building(),
        "Floor" => lucide::layers(),
        "Smoking" => lucide::cigarette(),
        "Collection times" => lucide::mailbox(),
        "Platform" | "Lines" | "Network" => lucide::bus(),
        "Shelter" | "Bench" | "Tactile paving" => lucide::umbrella(),
        "SIRET" | "Company record" => lucide::file_text(),
        "Last checked" => lucide::badge_check(),
        "Payment" => lucide::credit_card(),
        "Fuels" => lucide::fuel(),
        "Sockets" => lucide::plug_zap(),
        "Recycling" => lucide::recycle(),
        "Capacity" => lucide::users(),
        "Access" | "Self service" => lucide::door_open(),
        "Religion" | "Denomination" => lucide::church(),
        "Delivery" | "Drive-through" => lucide::truck(),
        "Wheelchair toilets" => lucide::accessibility(),
        "Air conditioning" => lucide::snowflake(),
        "Facebook" | "Instagram" | "TikTok" | "YouTube" | "LinkedIn" => lucide::at_sign(),
        "Brand" => lucide::tag(),
        "Operator" => lucide::building_2(),
        "Internet access" => lucide::wifi(),
        "Wikipedia" => lucide::book_open(),
        "OpenStreetMap" => lucide::map(),
        "Wikivoyage" => lucide::compass(),
        "Heritage" | "Mérimée reference" => lucide::landmark(),
        "Since" => lucide::calendar(),
        "Last edited" => lucide::pen_line(),
        "Population" => lucide::users(),
        "Elevation" => lucide::mountain(),
        "Height" => lucide::ruler(),
        "Source" | "Source date" => lucide::info(),
        _ => return None,
    })
}

/// Icon column + content column, shared by every detail row so values line up.
fn icon_row(key: &str, content: impl IntoElement) -> impl IntoElement {
    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .spacing(14.0)
        .padding((10.0f32, 0.0f32))
        .child(
            rect()
                .width(Size::px(ICON_SIZE))
                .height(Size::px(ICON_SIZE))
                .maybe_child(detail_icon(key).map(|svg| icon(svg, TEXT_MUTED).into_element())),
        )
        .child(rect().vertical().width(Size::flex(1.0)).child(content))
}

fn detail_row(key: String, value: String) -> Element {
    if key == "Opening hours" {
        return opening_hours_row(value).into_element();
    }
    if key == "Coordinates" {
        return CoordinatesRow { value }.into_element();
    }
    if key == "Collection times" {
        return hours_table(&key, value, false).into_element();
    }
    // A tap calls (the phone app, on Android) or writes an email.
    let link = match key.as_str() {
        "Phone" => Some("tel:"),
        "Email" => Some("mailto:"),
        _ => None,
    };
    if let Some(scheme) = link {
        return contact_row(key, value, scheme);
    }

    icon_row(
        &key,
        rect()
            .vertical()
            .width(Size::fill())
            .child(
                label()
                    .text(value)
                    .font_size(text_size(14.0))
                    .color(TEXT)
                    .width(Size::fill()),
            )
            .child(
                label()
                    .text(key.clone())
                    .font_size(text_size(11.0))
                    .color(TEXT_FAINT),
            ),
    )
    .into_element()
}

/// Phone numbers or email addresses (`;`-separated in OSM), each opening `scheme`'s app.
fn contact_row(key: String, value: String, scheme: &'static str) -> Element {
    let entries: Vec<String> = value
        .split(';')
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    icon_row(
        &key,
        rect()
            .vertical()
            .width(Size::fill())
            .children(entries.into_iter().map(|entry| {
                // `tel:` takes no spaces or dots: `+33 4 42 00 00 00` → `+33442000000`.
                let target = if scheme == "tel:" {
                    entry
                        .chars()
                        .filter(|c| c.is_ascii_digit() || *c == '+')
                        .collect::<String>()
                } else {
                    entry.clone()
                };
                label()
                    .text(entry)
                    .font_size(text_size(14.0))
                    .color(ACCENT)
                    .cursor(CursorIcon::Pointer)
                    .padding((tap_padding(0.0), 0.0))
                    .on_press(move |_: Event<PressEventData>| {
                        haptic(Haptic::Press);
                        crate::mobile::open_url(format!("{scheme}{target}"));
                    })
                    .into_element()
            }))
            .child(
                label()
                    .text(key.clone())
                    .font_size(text_size(11.0))
                    .color(TEXT_FAINT),
            ),
    )
    .into_element()
}

/// GPS coordinates; a click copies them to the clipboard.
#[derive(PartialEq)]
struct CoordinatesRow {
    value: String,
}

impl Component for CoordinatesRow {
    fn render(&self) -> impl IntoElement {
        let mut copied = use_state(|| false);
        let value = self.value.clone();
        let hint = if *copied.read() {
            "Coordinates · copied"
        } else {
            "Coordinates · click to copy"
        };

        icon_row(
            "Coordinates",
            rect()
                .vertical()
                .width(Size::fill())
                .cursor(CursorIcon::Pointer)
                .on_press(
                    move |_: Event<PressEventData>| match Clipboard::set(value.clone()) {
                        Ok(()) => copied.set(true),
                        Err(err) => log::warn!("[place] copy failed: {err:?}"),
                    },
                )
                .child(
                    label()
                        .text(self.value.clone())
                        .font_size(text_size(14.0))
                        .color(TEXT)
                        .width(Size::fill()),
                )
                .child(
                    label()
                        .text(hint)
                        .font_size(text_size(11.0))
                        .color(TEXT_FAINT),
                ),
        )
    }
}

/// Renders a parsed `opening_hours` value (see `opening_hours::parse_opening_hours`) as a small
/// day/hours table — one row per rule, day label left, hours right.
fn opening_hours_row(value: String) -> impl IntoElement {
    hours_table("Opening hours", value, true)
}

/// A day/hours table for `opening_hours`-syntax `value` under `key`; `open_now`: with the
/// open/closed-now line above it.
fn hours_table(key: &str, value: String, open_now: bool) -> impl IntoElement {
    let rules = parse_opening_hours(&value);

    icon_row(
        key,
        rect()
            .vertical()
            .width(Size::fill())
            .spacing(2.0)
            .maybe_child(open_now.then(|| OpenNow { raw: value }.into_element()))
            .children(rules.into_iter().map(|rule| {
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .child(
                        label()
                            .text(rule.days)
                            .width(Size::px(96.0))
                            .font_size(text_size(13.0))
                            .color(TEXT_MUTED),
                    )
                    .child(
                        label()
                            .text(rule.hours)
                            .width(Size::flex(1.0))
                            .font_size(text_size(13.0))
                            .color(TEXT),
                    )
                    .into_element()
            }))
            .child(
                label()
                    .text(key.to_string())
                    .font_size(text_size(11.0))
                    .color(TEXT_FAINT),
            ),
    )
}

/// Open, closing soon or closed right now (French local time, as transit times), from the raw
/// `opening_hours`; nothing when it isn't understood. Rechecked every half minute while shown.
#[derive(PartialEq)]
struct OpenNow {
    raw: String,
}

/// Minutes since Monday 00:00, French local time (see `transit::ui::clock_time`).
fn local_week_minute() -> u32 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let local = unix + central_european_offset(unix);
    // 1970-01-01 was a Thursday: day 0 is Monday-based index 3.
    let weekday = (local.div_euclid(86_400) + 3).rem_euclid(7);
    (weekday * 24 * 60 + local.rem_euclid(86_400) / 60) as u32
}

impl Component for OpenNow {
    fn render(&self) -> impl IntoElement {
        let mut now = use_state(local_week_minute);
        // Tied to this component's scope: stops with the place's details.
        use_hook(move || {
            spawn(async move {
                loop {
                    background_delay(Duration::from_secs(30)).await;
                    let minute = local_week_minute();
                    if *now.peek() != minute {
                        now.set(minute);
                    }
                }
            });
        });
        let now = *now.read();

        let Some(status) = open_status(&self.raw, now) else {
            return rect().into_element();
        };
        let (state, detail, color) = match status {
            OpenStatus::AlwaysOpen => ("Open 24 hours", None, OPEN),
            OpenStatus::Open { closes } => (
                "Open",
                Some(format!("closes {}", when_text(closes, now))),
                OPEN,
            ),
            OpenStatus::ClosingSoon { closes } => (
                "Closing soon",
                Some(format!("closes {}", when_text(closes, now))),
                CLOSING_SOON,
            ),
            OpenStatus::Closed { opens } => (
                "Closed",
                opens.map(|opens| format!("opens {}", when_text(opens, now))),
                CLOSED,
            ),
        };

        rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(6.0)
            .margin((0.0f32, 0.0f32, 4.0f32, 0.0f32))
            .child(
                label()
                    .text(state)
                    .font_size(text_size(13.0))
                    .font_weight(FontWeight::BOLD)
                    .color(color),
            )
            .maybe_child(detail.map(|detail| {
                label()
                    .text(format!("· {detail}"))
                    .font_size(text_size(13.0))
                    .color(TEXT_MUTED)
                    .into_element()
            }))
            .into_element()
    }
}

fn wikipedia_block(intro: String) -> impl IntoElement {
    rect()
        .vertical()
        .width(Size::fill())
        .spacing(6.0)
        .padding(12.0f32)
        .corner_radius(10.0f32)
        .background(SURFACE_ALT)
        .margin((8.0f32, 0.0f32, 8.0f32, 0.0f32))
        .child(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(8.0)
                .child(icon(lucide::book_open(), TEXT_MUTED))
                .child(
                    label()
                        .text("From Wikipedia")
                        .font_size(text_size(11.0))
                        .color(TEXT_FAINT),
                )
                // The intro's licence, which reusing Wikipedia text requires showing.
                .child(
                    label()
                        .text("CC BY-SA 4.0")
                        .font_size(text_size(11.0))
                        .color(TEXT_FAINT)
                        .cursor(CursorIcon::Pointer)
                        .on_press(|_: Event<PressEventData>| {
                            crate::mobile::open_url(
                                "https://creativecommons.org/licenses/by-sa/4.0/",
                            )
                        }),
                ),
        )
        .child(
            label()
                .text(intro)
                .font_size(text_size(13.0))
                .color(TEXT)
                .width(Size::fill()),
        )
}

/// A compact action button — website/Wikipedia/… — that opens `url` in the system browser. The
/// OpenStreetMap link shares the place instead (`title` and that link): the system share sheet
/// on Android, copying the link elsewhere.
fn link_chip(link_label: String, url: String, title: &str) -> impl IntoElement {
    let sharing = link_label == "OpenStreetMap";
    let (svg, link_label) = match (sharing, TOUCH) {
        (true, true) => (lucide::share_2(), "Share".to_string()),
        (true, false) => (lucide::copy(), "Copy link".to_string()),
        (false, _) => (
            detail_icon(&link_label).unwrap_or_else(lucide::external_link),
            link_label,
        ),
    };
    let title = title.to_string();

    Button::new()
        .outline()
        .maybe(!TOUCH, |button| button.compact())
        .width(Size::flex(1.0))
        .on_press(move |_: Event<PressEventData>| {
            if sharing {
                haptic(Haptic::Press);
                share(&title, &url);
            } else {
                crate::mobile::open_url(url.clone());
            }
        })
        .child(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(6.0)
                .child(icon(svg, ACCENT))
                .child(
                    label()
                        .text(link_label)
                        .font_size(text_size(13.0))
                        .color(ACCENT)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                ),
        )
}

/// The "More tags" section: every OSM/MVT tag not mapped to a curated row, collapsed by default
/// since it's mostly raw data. Keyed by the place's id so it starts collapsed again for each new
/// selection instead of inheriting the previous place's toggle.
#[derive(PartialEq)]
struct RawTags {
    place_id: u64,
    tags: Vec<(String, String)>,
}

impl Component for RawTags {
    fn render(&self) -> impl IntoElement {
        let mut open = use_state(|| false);
        let is_open = *open.read();
        let header = format!("{} more tag{}", self.tags.len(), plural(self.tags.len()));

        rect()
            .vertical()
            .width(Size::fill())
            .margin((8.0f32, 0.0f32, 16.0f32, 0.0f32))
            .child(divider())
            .child(
                Button::new()
                    .flat()
                    .width(Size::fill())
                    .on_press(move |_: Event<PressEventData>| open.toggle())
                    .child(
                        rect()
                            .horizontal()
                            .content(Content::flex())
                            .width(Size::fill())
                            .cross_align(Alignment::Center)
                            .spacing(8.0)
                            .child(icon(lucide::hash(), TEXT_FAINT))
                            .child(
                                label()
                                    .text(header)
                                    .width(Size::flex(1.0))
                                    .font_size(text_size(12.0))
                                    .color(TEXT_MUTED),
                            )
                            .child(
                                rect()
                                    .rotate(if is_open { 180.0 } else { 0.0 })
                                    .child(icon(lucide::chevron_down(), TEXT_FAINT)),
                            ),
                    ),
            )
            .maybe(is_open, |r| {
                r.children(self.tags.iter().map(|(key, value)| {
                    rect()
                        .horizontal()
                        .content(Content::flex())
                        .width(Size::fill())
                        .spacing(8.0)
                        .padding((4.0f32, 0.0f32))
                        .child(
                            label()
                                .text(key.clone())
                                .width(Size::px(130.0))
                                .font_size(text_size(12.0))
                                .color(TEXT_FAINT),
                        )
                        .child(
                            label()
                                .text(value.clone())
                                .width(Size::flex(1.0))
                                .font_size(text_size(12.0))
                                .color(TEXT),
                        )
                        .into_element()
                }))
            })
    }

    fn render_key(&self) -> DiffKey {
        crate::components::place::section_key("raw-tags", self.place_id)
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_words_name_categories() {
        assert_eq!(category_for(" Boulangerie ").unwrap().label, "Bakeries");
        assert_eq!(category_for("médecin").unwrap().label, "Health");
        assert_eq!(category_for("cafés").unwrap().label, "Cafés");
        assert!(category_for("Boulangerie Paul").is_none());
    }
}
