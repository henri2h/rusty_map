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
    MapController,
    util::{
        tiles::background_delay,
        viewport::{Viewport, center_to_lonlat, lonlat_to_center, max_zoom_ui},
    },
};

use crate::components::{
    place::{PlaceController, SelectedPlace, humanize},
    transit::{Departures, Endpoint, Planner, TrackView, TransitController, TransitView, TripView},
};

use super::{
    opening_hours::parse_opening_hours,
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

const TEXT: Color = Color::from_rgb(32, 33, 36);
const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const TEXT_FAINT: Color = Color::from_rgb(140, 144, 150);
const ACCENT: Color = Color::from_rgb(26, 115, 232);
const DIVIDER: Color = Color::from_rgb(232, 234, 237);
const SURFACE_ALT: Color = Color::from_rgb(244, 246, 248);

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

        let on_validate = build_on_search_input(map, places, search);
        let has_query = !search.query.read().is_empty();
        let status = *search.status.read();
        let results = search.results.read().clone();
        let place = places.selected_place.read().clone();
        let view = transit.view.read().clone();
        let planner_open = *transit.planner_open.read();
        let drawer_open = place.is_some() || view.is_some() || planner_open;
        let dropdown = (!drawer_open && status != SearchStatus::Idle)
            .then(|| results_dropdown(status, results, map, places, search));
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
            .leading(icon(lucide::search(), TEXT_FAINT))
            .on_validate(on_validate);
        if !has_query {
            let mut transit = transit.clone();
            input = input.trailing(
                rect().cursor(CursorIcon::Pointer).child(
                    icon(lucide::route(), ACCENT)
                        .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
                        .on_press(move |e: Event<PressEventData>| {
                            e.stop_propagation();
                            e.prevent_default();
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
            content.extend(dropdown);
            if let Some(view) = view {
                content.push(transit_view(view, transit.clone()));
            } else if let Some(place) = place {
                content.push(place_details(place, transit.clone()).into_element());
            }
            content
        };

        rect()
            .position(Position::new_absolute().top(0.0f32).left(0.0f32))
            .width(Size::px(PANEL_WIDTH))
            .maybe(drawer_open, |r| r.height(Size::percent(100.)))
            .background(Color::from_argb(250, 255, 255, 255))
            .corner_radius(match (drawer_open, rounded) {
                (true, _) => 0.0f32,
                (false, true) => 24.0f32,
                (false, false) => 12.0f32,
            })
            .shadow(shadow)
            // Without this, the map canvas's own paint callback (rebuilt fresh on every
            // zoom-triggered `.key()` change) can land after this element in the base layer's
            // paint order and draw over it — seen as the panel flickering while zooming.
            .layer(Layer::Overlay)
            .maybe(!drawer_open, |r| r.margin(12.0f32))
            .padding(if drawer_open { 16.0f32 } else { 8.0f32 })
            .vertical()
            .children(content)
    }
}

/// A transit view (tapped track, or a trip) under a "Back" row returning to what was there.
fn transit_view(view: TransitView, transit: TransitController) -> Element {
    let mut close = transit.clone();
    let content = match view {
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
        } => TripView {
            id,
            departure,
            from_stop,
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
                .compact()
                .on_press(move |_: Event<PressEventData>| close.close_view())
                .child(
                    rect()
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(6.0)
                        .child(icon(lucide::arrow_left(), TEXT_MUTED))
                        .child(label().text("Back").font_size(13.0).color(TEXT_MUTED)),
                ),
        )
        .child(
            ScrollView::new()
                .height(Size::fill())
                .direction(Direction::vertical())
                .margin((8.0f32, 0.0f32, 0.0f32, 0.0f32))
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
/// flashing to "Searching…" on every keystroke.
fn results_dropdown(
    status: SearchStatus,
    results: Vec<SearchResult>,
    map: &MapController,
    places: &PlaceController,
    search: &SearchController,
) -> Element {
    let body = if !results.is_empty() {
        ScrollView::new()
            // `ScrollView` defaults to `height: fill()`, which resolves to nothing inside this
            // panel's content-sized pill height — shrink to the rows' own height instead,
            // capped by `max_height`.
            .height(Size::auto())
            .max_height(Size::px(RESULTS_MAX_HEIGHT))
            .direction(Direction::vertical())
            .children(
                results
                    .into_iter()
                    .map(|result| result_row(result, map, places, search).into_element()),
            )
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
            .child(label().text(text).font_size(13.0).color(TEXT_MUTED))
            .into_element()
    };

    rect()
        .vertical()
        .width(Size::fill())
        .padding((4.0f32, 0.0f32, 0.0f32, 0.0f32))
        .child(divider())
        .child(body)
        .into_element()
}

fn result_row(
    result: SearchResult,
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

    let mut center = map.center;
    let mut zoom = map.zoom;
    let viewport_size = map.viewport_size;
    let max_real_zoom = map.style.borrow().max_real_zoom;
    let platform = map.platform.clone();
    let mut places = places.clone();
    let mut search = search.clone();

    Button::new()
        .flat()
        .width(Size::fill())
        .on_press(move |_: Event<PressEventData>| {
            let target_zoom = RESULT_ZOOM.min(max_zoom_ui(max_real_zoom));
            // `lonlat_to_center` alone would put the target at the viewport's top-left corner
            // (that's what `center` actually anchors — see `Viewport`'s own doc comment), not
            // its visual middle. Shift by half the viewport in pixels, converted through the
            // same `pixels_to_center_delta` panning uses, so a search result actually lands
            // where the user is looking.
            let area = viewport_size.peek().size;
            let half_offset = Viewport::new(CursorPoint::zero(), target_zoom, max_real_zoom)
                .pixels_to_center_delta(area.width as f64 / 2.0, area.height as f64 / 2.0);
            let raw_center = lonlat_to_center(result.lon, result.lat);
            center.set(CursorPoint::new(
                raw_center.x - half_offset.x,
                raw_center.y - half_offset.y,
            ));
            zoom.set(target_zoom);

            search.reset(result.title().to_string());
            places.select_search_result(&result, platform.clone());
            // `zoom` may end up unchanged (e.g. picking another result while already at the
            // cap), so the canvas's `.key()` alone can't be relied on to force a repaint.
            platform.send(UserEvent::RequestRedraw);
        })
        .child(
            rect()
                .horizontal()
                .content(Content::flex())
                .width(Size::fill())
                .cross_align(Alignment::Center)
                .spacing(12.0)
                .padding((6.0f32, 4.0f32))
                .child(icon(lucide::map_pin(), TEXT_FAINT))
                .child(
                    rect()
                        .vertical()
                        .width(Size::flex(1.0))
                        .child(
                            label()
                                .text(title)
                                .font_size(14.0)
                                .color(TEXT)
                                .max_lines(1)
                                .text_overflow(TextOverflow::Ellipsis),
                        )
                        .maybe(!subtitle.is_empty(), |r| {
                            r.child(
                                label()
                                    .text(subtitle)
                                    .font_size(12.0)
                                    .color(TEXT_MUTED)
                                    .max_lines(1)
                                    .text_overflow(TextOverflow::Ellipsis),
                            )
                        }),
                ),
        )
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
        let mut results = search.results;
        let mut status = search.status;
        let platform = platform.clone();
        let handle = spawn(async move {
            background_delay(SEARCH_DEBOUNCE).await;
            match search::search_address(text, bias).await {
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
fn place_details(place: SelectedPlace, transit: TransitController) -> impl IntoElement {
    let SelectedPlace {
        id,
        title,
        subtitle,
        mut details,
        raw_tags,
        links,
        loading,
        image,
        transit_stop,
        coords,
        ..
    } = place;

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
        "Source" | "Source date" | "Last edited" => 4,
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
                .child(label().text(subtitle).font_size(12.0).color(TEXT_MUTED))
                .into_element(),
        );
    }
    if let Some((lat, lon)) = coords {
        let mut transit = transit.clone();
        let name = title.clone();
        body.push(
            Button::new()
                .filled()
                .width(Size::fill())
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
                                .font_size(13.0)
                                .color(Color::WHITE),
                        ),
                )
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
                            .map(|(label, url)| link_chip(label, url).into_element()),
                    )
                    .maybe(pair.len() == 1, |r| r.child(rect().width(Size::flex(1.0))))
                    .into_element(),
            );
        }
    }
    if let Some((lat, lon)) = transit_stop {
        body.push(
            Departures {
                place_id: id,
                lat,
                lon,
                name: title,
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
                        .font_size(12.0)
                        .color(TEXT_FAINT),
                )
                .into_element(),
        );
    } else if is_empty {
        body.push(
            label()
                .text("No further details for this place.")
                .font_size(12.0)
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
    if !raw_tags.is_empty() {
        body.push(
            RawTags {
                place_id: id,
                tags: raw_tags,
            }
            .into_element(),
        );
    }

    ScrollView::new()
        .height(Size::fill())
        .direction(Direction::vertical())
        .margin((16.0f32, 0.0f32, 0.0f32, 0.0f32))
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
        "Amenity" | "Shop" => lucide::store(),
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

    icon_row(
        &key,
        rect()
            .vertical()
            .width(Size::fill())
            .child(
                label()
                    .text(value)
                    .font_size(14.0)
                    .color(TEXT)
                    .width(Size::fill()),
            )
            .child(label().text(key.clone()).font_size(11.0).color(TEXT_FAINT)),
    )
    .into_element()
}

/// Renders a parsed `opening_hours` value (see `opening_hours::parse_opening_hours`) as a small
/// day/hours table — one row per rule, day label left, hours right.
fn opening_hours_row(value: String) -> impl IntoElement {
    let rules = parse_opening_hours(&value);

    icon_row(
        "Opening hours",
        rect()
            .vertical()
            .width(Size::fill())
            .spacing(2.0)
            .children(rules.into_iter().map(|rule| {
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .child(
                        label()
                            .text(rule.days)
                            .width(Size::px(96.0))
                            .font_size(13.0)
                            .color(TEXT_MUTED),
                    )
                    .child(
                        label()
                            .text(rule.hours)
                            .width(Size::flex(1.0))
                            .font_size(13.0)
                            .color(TEXT),
                    )
                    .into_element()
            }))
            .child(
                label()
                    .text("Opening hours")
                    .font_size(11.0)
                    .color(TEXT_FAINT),
            ),
    )
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
                        .font_size(11.0)
                        .color(TEXT_FAINT),
                )
                // The intro's licence, which reusing Wikipedia text requires showing.
                .child(
                    label()
                        .text("CC BY-SA 4.0")
                        .font_size(11.0)
                        .color(TEXT_FAINT)
                        .cursor(CursorIcon::Pointer)
                        .on_press(|_: Event<PressEventData>| {
                            Platform::get()
                                .open_url("https://creativecommons.org/licenses/by-sa/4.0/")
                        }),
                ),
        )
        .child(
            label()
                .text(intro)
                .font_size(13.0)
                .color(TEXT)
                .width(Size::fill()),
        )
}

/// A compact action button — website/Wikipedia/OSM/… — that opens `url` in the system browser.
fn link_chip(link_label: String, url: String) -> impl IntoElement {
    let svg = detail_icon(&link_label).unwrap_or_else(lucide::external_link);

    Button::new()
        .outline()
        .compact()
        .width(Size::flex(1.0))
        .on_press(move |_: Event<PressEventData>| Platform::get().open_url(url.clone()))
        .child(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(6.0)
                .child(icon(svg, ACCENT))
                .child(
                    label()
                        .text(link_label)
                        .font_size(13.0)
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
                                    .font_size(12.0)
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
                                .font_size(12.0)
                                .color(TEXT_FAINT),
                        )
                        .child(
                            label()
                                .text(value.clone())
                                .width(Size::flex(1.0))
                                .font_size(12.0)
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
