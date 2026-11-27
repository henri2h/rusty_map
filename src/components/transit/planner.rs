//! Journey planner (public transport): start/destination fields — each searchable, or set by
//! tapping the map — then Transitous itineraries, the selected one detailed leg by leg and
//! drawn on the map.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use freya::{icons::lucide, prelude::*};

use crate::mobile::{Haptic, TOUCH, haptic, tap_padding, text_size, use_screen};
use map_component::util::{tiles::background_delay, viewport::center_to_lonlat};

use crate::components::{
    favorites::Favorites,
    navigation::ui::WalkRoute,
    place::humanize,
    search::favorite_chip,
    search::util::search::{self, SearchResult},
    sheet::{Detent, SheetState},
};

use super::{
    api,
    controller::{END_COLOR, Endpoint, Field, START_COLOR, TransitController, TravelMode},
    journey::{self, Itinerary},
    trip::{self, TripDetail, journey_overlay},
    ui::{
        ACCENT, DIVIDER, LINK, TEXT, TEXT_FAINT, TEXT_MUTED, diff_key, icon, show_on_map,
        status_line,
    },
    when::{When, WhenPicker},
};

/// Same debounce/minimum as the main search box.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);
const MIN_QUERY_LEN: usize = 2;

fn endpoint_key(endpoint: &Option<Endpoint>) -> Option<(String, i64, i64)> {
    endpoint.as_ref().map(|endpoint| {
        (
            endpoint.name.clone(),
            (endpoint.lat * 1e6) as i64,
            (endpoint.lon * 1e6) as i64,
        )
    })
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// The planner's whole drawer content.
#[derive(PartialEq)]
pub struct Planner {}

impl Component for Planner {
    fn render(&self) -> impl IntoElement {
        let mut transit = use_consume::<TransitController>();
        // In the compact layout's bottom sheet, its grab bar (pull down) and back close it.
        let compact = use_screen().compact;
        let from = transit.from.read().clone();
        let to = transit.to.read().clone();
        let active = *transit.active_field.read();
        let picking = *transit.picking.read();
        let shortcuts = favorite_shortcuts(&transit);
        let when_state = transit.when;
        let when = *when_state.read();
        let mode_state = transit.mode;
        let mode = *mode_state.read();
        let trip_open = *transit.trip_open.read();

        let hint = match (active, &from, &to) {
            (Field::From, None, _) | (Field::To, _, None) if !picking => {
                Some("Search, or use ⌖ to pick a point on the map")
            }
            _ => None,
        };

        // The form: hidden while one itinerary's steps take the whole sheet (`trip_open`).
        let form = rect()
            .vertical()
            .width(Size::fill())
            .child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(8.0)
                    .margin((0.0f32, 0.0f32, 10.0f32, 0.0f32))
                    .maybe_child((!compact).then(|| {
                        Button::new()
                            .flat()
                            .maybe(!TOUCH, |button| button.compact())
                            .on_press({
                                let mut transit = transit.clone();
                                move |_: Event<PressEventData>| transit.close_planner()
                            })
                            .child(icon(lucide::arrow_left(), TEXT_MUTED, 18.0))
                    }))
                    .child(
                        label()
                            .text("Directions")
                            .font_size(text_size(17.0))
                            .font_weight(FontWeight::BOLD)
                            .color(TEXT),
                    ),
            )
            .child(
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .cross_align(Alignment::Center)
                    .spacing(6.0)
                    .child(
                        rect()
                            .vertical()
                            .width(Size::flex(1.0))
                            .spacing(6.0)
                            .child(EndpointField {
                                field: Field::From,
                                endpoint: from.clone(),
                                active: active == Field::From,
                                picking: picking && active == Field::From,
                            })
                            .child(EndpointField {
                                field: Field::To,
                                endpoint: to.clone(),
                                active: active == Field::To,
                                picking: picking && active == Field::To,
                            }),
                    )
                    .child(
                        Button::new()
                            .flat()
                            .maybe(!TOUCH, |button| button.compact())
                            .on_press(move |_: Event<PressEventData>| transit.swap_endpoints())
                            .child(icon(lucide::arrow_up_down(), TEXT_MUTED, 18.0)),
                    ),
            )
            .maybe_child(shortcuts)
            .maybe_child(hint.map(|hint| {
                label()
                    .text(hint)
                    .font_size(text_size(12.0))
                    .color(TEXT_FAINT)
                    .margin((8.0f32, 0.0f32, 0.0f32, 4.0f32))
                    .into_element()
            }))
            .child(mode_tabs(mode_state, mode))
            // A walk leaves now: no departure time to pick.
            .maybe_child((mode == TravelMode::Transit).then(|| {
                rect()
                    .margin((10.0f32, 0.0f32, 0.0f32, 4.0f32))
                    .child(WhenPicker {
                        when: when_state,
                        allow_arrive: true,
                        icon: true,
                    })
                    .into_element()
            }));

        rect()
            .vertical()
            .width(Size::fill())
            .height(Size::fill())
            .maybe_child((!trip_open).then(|| form.into_element()))
            .maybe_child(match (from, to, mode) {
                (Some(from), Some(to), TravelMode::Transit) => {
                    Some(Itineraries { from, to, when }.into_element())
                }
                (Some(from), Some(to), TravelMode::Walk) => Some(
                    rect()
                        .vertical()
                        .width(Size::fill())
                        .height(Size::fill())
                        // Clear of the Transit / Walk tabs: no departure-time row between them
                        // here, as there is for transit.
                        .padding((16.0f32, 0.0f32, 0.0f32, 0.0f32))
                        .child(WalkRoute { from, to })
                        .into_element(),
                ),
                _ => None,
            })
    }
}

/// Transit / Walk, side by side: how the journey is planned.
fn mode_tabs(mut state: State<TravelMode>, current: TravelMode) -> Element {
    let tab = |mode: TravelMode, svg: bytes::Bytes, text: &'static str| {
        let selected = mode == current;
        let (background, foreground) = if selected {
            (ACCENT, Color::WHITE)
        } else {
            (Color::from_rgb(244, 246, 248), TEXT_MUTED)
        };
        rect()
            .width(Size::flex(1.0))
            .horizontal()
            .main_align(Alignment::Center)
            .cross_align(Alignment::Center)
            .spacing(6.0)
            .padding((tap_padding(6.0), 8.0))
            .corner_radius(18.0f32)
            .background(background)
            .cursor(CursorIcon::Pointer)
            .on_press(move |_: Event<PressEventData>| {
                if !selected {
                    haptic(Haptic::Toggle);
                    state.set(mode);
                }
            })
            .child(icon(svg, foreground, 15.0))
            .child(
                label()
                    .text(text)
                    .font_size(text_size(13.0))
                    .font_weight(FontWeight::BOLD)
                    .color(foreground),
            )
            .into_element()
    };
    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .spacing(8.0)
        .margin((10.0f32, 0.0f32, 0.0f32, 0.0f32))
        .child(tab(TravelMode::Transit, lucide::train_front(), "Transit"))
        .child(tab(TravelMode::Walk, lucide::footprints(), "Walk"))
        .into_element()
}

/// Saved places as chips: one tap fills the active field (start or destination).
fn favorite_shortcuts(transit: &TransitController) -> Option<Element> {
    let favorites = try_consume_context::<Favorites>()?;
    let list = favorites.list.read().clone();
    if list.is_empty() {
        return None;
    }
    let chips = list.into_iter().map(|favorite| {
        let mut transit = transit.clone();
        let name = favorite.name.clone();
        favorite_chip(&name, favorite.stop, move || {
            transit.stop_picking();
            transit.set_endpoint(Endpoint {
                name: favorite.name.clone(),
                lat: favorite.lat,
                lon: favorite.lon,
            });
        })
    });
    Some(
        ScrollView::new()
            .direction(Direction::horizontal())
            .width(Size::fill())
            .height(Size::auto())
            .show_scrollbar(false)
            .margin((8.0f32, 0.0f32, 0.0f32, 0.0f32))
            .child(rect().horizontal().spacing(6.0).children(chips))
            .into_element(),
    )
}

/// One endpoint input: shows the chosen place's name, searches as the user types, and offers
/// "pick on map". Keyed by its endpoint, so choosing a place resets the text to that place.
#[derive(PartialEq)]
struct EndpointField {
    field: Field,
    endpoint: Option<Endpoint>,
    active: bool,
    /// Waiting for a map tap to fill this field.
    picking: bool,
}

impl Component for EndpointField {
    fn render(&self) -> impl IntoElement {
        let transit = use_consume::<TransitController>();
        let query = use_state({
            let name = self.endpoint.as_ref().map(|e| e.name.clone());
            move || name.unwrap_or_default()
        });
        let results = use_state::<Vec<SearchResult>>(Vec::new);
        let task = use_state::<Option<TaskHandle>>(|| None);
        let field = self.field;

        let on_validate = {
            let mut transit = transit.clone();
            let mut results = results;
            let mut task = task;
            move |validator: InputValidator| {
                let text = validator.text().clone();
                // Typing means searching, not picking on the map.
                transit.stop_picking();
                transit.active_field.set(field);
                if let Some(handle) = task.peek().as_ref() {
                    handle.cancel();
                }
                if text.trim().chars().count() < MIN_QUERY_LEN {
                    results.set(Vec::new());
                    return;
                }
                let bias = Some(center_to_lonlat(*transit.map.center.peek()));
                let platform = transit.map.platform.clone();
                task.set(Some(spawn(async move {
                    background_delay(SEARCH_DEBOUNCE).await;
                    if let Ok(list) = search::search_address(text, bias).await {
                        results.set(list.into_iter().take(5).collect());
                        platform.send(UserEvent::RequestRedraw);
                    }
                })));
            }
        };

        let (dot_svg, dot_color, placeholder) = match field {
            Field::From => (lucide::circle_dot(), START_COLOR, "Start"),
            Field::To => (lucide::map_pin(), END_COLOR, "Destination"),
        };
        let border = if self.active { ACCENT } else { DIVIDER };

        let suggestions: Vec<Element> = results
            .read()
            .iter()
            .map(|result| {
                let mut transit = transit.clone();
                let mut results = results;
                let subtitle = [humanize(&result.category), result.display_name.clone()]
                    .into_iter()
                    .filter(|part| !part.is_empty() && part != result.title())
                    .collect::<Vec<_>>()
                    .join(" · ");
                let endpoint = Endpoint {
                    name: result.title().to_string(),
                    lat: result.lat,
                    lon: result.lon,
                };
                Button::new()
                    .flat()
                    .width(Size::fill())
                    .on_press(move |_: Event<PressEventData>| {
                        transit.stop_picking();
                        transit.active_field.set(field);
                        transit.set_endpoint(endpoint.clone());
                        results.set(Vec::new());
                    })
                    .child(
                        rect()
                            .vertical()
                            .width(Size::fill())
                            .child(
                                label()
                                    .text(result.title().to_string())
                                    .font_size(text_size(13.0))
                                    .color(TEXT)
                                    .max_lines(1)
                                    .text_overflow(TextOverflow::Ellipsis),
                            )
                            .maybe_child((!subtitle.is_empty()).then(|| {
                                label()
                                    .text(subtitle)
                                    .font_size(text_size(11.0))
                                    .color(TEXT_MUTED)
                                    .max_lines(1)
                                    .text_overflow(TextOverflow::Ellipsis)
                                    .into_element()
                            })),
                    )
                    .into_element()
            })
            .collect();

        rect()
            .vertical()
            .width(Size::fill())
            .child(
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .cross_align(Alignment::Center)
                    .padding((0.0f32, 4.0f32, 0.0f32, 10.0f32))
                    .corner_radius(10.0f32)
                    .border(
                        Border::new()
                            .fill(border)
                            .width(if self.active { 2.0 } else { 1.0 })
                            .alignment(BorderAlignment::Inner),
                    )
                    .child(icon(dot_svg, dot_color, 16.0))
                    .child(
                        rect().width(Size::flex(1.0)).child(
                            Input::new(query)
                                .placeholder(placeholder)
                                .flat()
                                .theme_colors(
                                    InputColorsThemePartial::new()
                                        .background(Color::TRANSPARENT)
                                        .focus_background(Color::TRANSPARENT)
                                        .border_fill(Color::TRANSPARENT)
                                        .focus_border_fill(Color::TRANSPARENT),
                                )
                                .width(Size::fill())
                                .on_validate(on_validate),
                        ),
                    )
                    .child({
                        // Toggles map picking for this field; filled while it's on.
                        let picking = self.picking;
                        let mut button = Button::new().maybe(!TOUCH, |button| button.compact());
                        button = if picking {
                            button.filled()
                        } else {
                            button.flat()
                        };
                        button
                            .on_press({
                                let mut transit = transit.clone();
                                move |_: Event<PressEventData>| {
                                    if picking {
                                        transit.stop_picking();
                                    } else {
                                        transit.start_picking(field);
                                    }
                                }
                            })
                            .child(icon(
                                lucide::crosshair(),
                                if picking {
                                    Color::WHITE
                                } else if self.active {
                                    ACCENT
                                } else {
                                    TEXT_FAINT
                                },
                                16.0,
                            ))
                    }),
            )
            .maybe(!suggestions.is_empty(), |r| {
                r.child(
                    rect()
                        .vertical()
                        .width(Size::fill())
                        .margin((4.0f32, 0.0f32, 0.0f32, 0.0f32))
                        .padding(4.0f32)
                        .corner_radius(10.0f32)
                        .background(Color::WHITE)
                        .border(
                            Border::new()
                                .fill(DIVIDER)
                                .width(1.0)
                                .alignment(BorderAlignment::Inner),
                        )
                        .children(suggestions),
                )
            })
    }

    fn render_key(&self) -> DiffKey {
        diff_key((self.field == Field::From, endpoint_key(&self.endpoint)))
    }
}

#[derive(Clone, PartialEq)]
enum PlanLoad {
    Loading,
    Ready(Vec<Itinerary>),
    Failed,
}

/// Itineraries between two set endpoints. Keyed by their positions and `when`, so changing
/// either replans (renaming one, as a map pick's address comes in, doesn't).
#[derive(PartialEq)]
struct Itineraries {
    from: Endpoint,
    to: Endpoint,
    when: When,
}

impl Component for Itineraries {
    fn render(&self) -> impl IntoElement {
        // The sheet's scroll position, for its pull-to-close (see `SheetScroll`).
        let scroll = use_consume::<SheetScroll>();
        // Below full height on a phone, dragging the list moves the sheet instead.
        let compact = use_screen().compact;
        let sheet = use_try_consume::<SheetState>();
        let content_scrolls = !compact || sheet.is_none_or(|sheet| sheet.content_scrolls());
        let transit = use_consume::<TransitController>();
        let state = use_state(|| PlanLoad::Loading);
        let selected = use_state(|| 0usize);

        use_hook({
            let (from, to, when) = (self.from.clone(), self.to.clone(), self.when);
            let mut state = state;
            let mut map = transit.map.clone();
            move || {
                spawn(async move {
                    match journey::plan(
                        (from.lat, from.lon),
                        (to.lat, to.lon),
                        from.name.clone(),
                        to.name.clone(),
                        when,
                    )
                    .await
                    {
                        Ok(mut itineraries) => {
                            trip::sort(&mut itineraries, when);
                            if let Some(first) = itineraries.first() {
                                show_on_map(&mut map, journey_overlay(first));
                            }
                            state.set(PlanLoad::Ready(itineraries));
                        }
                        Err(err) => {
                            log::warn!("[transit] plan failed: {err:?}");
                            state.set(PlanLoad::Failed);
                        }
                    }
                    map.platform.send(UserEvent::RequestRedraw);
                });
            }
        });

        let now = now_unix();
        let current = *selected.read();
        // The chosen itinerary's steps, in place of the list.
        if *transit.trip_open.read()
            && let PlanLoad::Ready(itineraries) = &*state.read()
            && let Some(itinerary) = itineraries.get(current)
        {
            return TripDetail {
                itinerary: itinerary.clone(),
                origin: self.from.name.clone(),
                destination: self.to.name.clone(),
                now,
                content_scrolls,
            }
            .into_element();
        }

        let body: Vec<Element> = match &*state.read() {
            PlanLoad::Loading => (0..3).map(|_| trip::placeholder_card()).collect(),
            PlanLoad::Failed => vec![status_line(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "Couldn't plan this journey",
            )],
            PlanLoad::Ready(itineraries) if itineraries.is_empty() => vec![status_line(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "No public transport route found",
            )],
            PlanLoad::Ready(itineraries) => {
                let tags = trip::tags(itineraries);
                itineraries
                    .iter()
                    .zip(tags)
                    .enumerate()
                    .map(|(index, (itinerary, tag))| {
                        let overlay = journey_overlay(itinerary);
                        let mut selected = selected;
                        let mut transit = transit.clone();
                        let sheet = sheet.filter(|_| compact);
                        trip::itinerary_card(itinerary, tag, index == current, now, move || {
                            selected.set(index);
                            transit.trip_open.set(true);
                            // Opened from the full-height list (phone): down to half height,
                            // so the trip shows on the map above its steps.
                            if let Some(mut sheet) = sheet
                                && sheet.peek_detent() == Detent::Full
                            {
                                sheet.set_detent(Detent::Half);
                            }
                            show_on_map(&mut transit.map, overlay.clone());
                        })
                    })
                    .collect()
            }
        };

        scroll
            .track(ScrollView::new())
            .drag_scrolling(content_scrolls)
            .height(Size::fill())
            .direction(Direction::vertical())
            .margin((12.0f32, 0.0f32, 0.0f32, 0.0f32))
            .child(scroll.marker())
            .children(body)
            .child(
                rect()
                    .cursor(CursorIcon::Pointer)
                    .margin((8.0f32, 0.0f32, 16.0f32, 0.0f32))
                    .on_press(|_: Event<PressEventData>| crate::mobile::open_url(api::SOURCES_URL))
                    .child(
                        label()
                            .text("Routes: Transitous · data sources")
                            .font_size(text_size(11.0))
                            .color(LINK),
                    ),
            )
            .into_element()
    }

    fn render_key(&self) -> DiffKey {
        let position =
            |endpoint: &Endpoint| ((endpoint.lat * 1e6) as i64, (endpoint.lon * 1e6) as i64);
        diff_key((
            "itineraries",
            position(&self.from),
            position(&self.to),
            self.when,
        ))
    }
}
