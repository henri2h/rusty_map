//! Journey planner (public transport): start/destination fields — each searchable, or set by
//! tapping the map — then Transitous itineraries, the selected one detailed leg by leg and
//! drawn on the map.

use std::{
    hash::{Hash, Hasher},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use freya::{icons::lucide, prelude::*};

use crate::mobile::{Haptic, SheetScroll, TOUCH, haptic, tap_padding, text_size, use_screen};
use map_component::{
    OverlayPath, OverlayStop, RouteOverlay,
    util::{tiles::background_delay, viewport::center_to_lonlat},
};

use crate::components::{
    favorites::Favorites,
    navigation::ui::WalkRoute,
    place::humanize,
    search::favorite_chip,
    search::util::search::{self, SearchResult},
};

use super::{
    api::{self, Mode},
    controller::{Endpoint, Field, TransitController, TravelMode},
    journey::{self, Itinerary, Leg},
    ui::{
        ACCENT, DIVIDER, LATE, LINK, LIVE, TEXT, TEXT_FAINT, TEXT_MUTED, clock_time, divider,
        duration_text, icon, line_badge, rgb_or, show_on_map, status_line,
    },
    when::{When, WhenPicker},
};

const START_COLOR: Color = Color::from_rgb(24, 128, 56);
const END_COLOR: Color = Color::from_rgb(217, 48, 37);
const WALK_COLOR: Color = Color::from_rgb(95, 99, 104);

/// Same debounce/minimum as the main search box.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);
const MIN_QUERY_LEN: usize = 2;

fn hash_key(parts: impl Hash) -> DiffKey {
    let mut hasher = std::hash::DefaultHasher::new();
    parts.hash(&mut hasher);
    DiffKey::U64(hasher.finish())
}

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

        let hint = match (active, &from, &to) {
            (Field::From, None, _) | (Field::To, _, None) if !picking => {
                Some("Search, or use ⌖ to pick a point on the map")
            }
            _ => None,
        };

        rect()
            .vertical()
            .width(Size::fill())
            .height(Size::fill())
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
            }))
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
        hash_key((self.field == Field::From, endpoint_key(&self.endpoint)))
    }
}

#[derive(Clone, PartialEq)]
enum PlanLoad {
    Loading,
    Ready(Vec<Itinerary>),
    Failed,
}

/// Itineraries between two set endpoints. Keyed by both, so changing either replans.
#[derive(PartialEq)]
struct Itineraries {
    from: Endpoint,
    to: Endpoint,
    when: When,
}

impl Component for Itineraries {
    fn render(&self) -> impl IntoElement {
        // The sheet's scroll position, for its pull-to-close (see `mobile::SheetScroll`).
        let scroll = use_consume::<SheetScroll>();
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
                        Ok(itineraries) => {
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
        let body: Vec<Element> = match &*state.read() {
            PlanLoad::Loading => vec![status_line(
                CircularLoader::new().size(16.0).into_element(),
                "Finding routes…",
            )],
            PlanLoad::Failed => vec![status_line(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "Couldn't plan this journey",
            )],
            PlanLoad::Ready(itineraries) if itineraries.is_empty() => vec![status_line(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "No public transport route found",
            )],
            PlanLoad::Ready(itineraries) => {
                let current = *selected.read();
                itineraries
                    .iter()
                    .enumerate()
                    .map(|(index, itinerary)| {
                        itinerary_card(
                            itinerary,
                            index == current,
                            now,
                            index,
                            selected,
                            transit.clone(),
                        )
                    })
                    .collect()
            }
        };

        scroll
            .track(ScrollView::new())
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
    }

    fn render_key(&self) -> DiffKey {
        hash_key((
            "itineraries",
            endpoint_key(&Some(self.from.clone())),
            endpoint_key(&Some(self.to.clone())),
            self.when,
        ))
    }
}

/// The journey drawn on the map: vehicles in their line colour, walks dotted, stops as dots.
fn journey_overlay(itinerary: &Itinerary) -> RouteOverlay {
    let mut overlay = RouteOverlay::default();
    for leg in &itinerary.legs {
        let color = if leg.is_walk() {
            WALK_COLOR
        } else {
            rgb_or(leg.color, ACCENT)
        };
        let path = if leg.geometry.len() >= 2 {
            leg.geometry.clone()
        } else {
            vec![(leg.from.lon, leg.from.lat), (leg.to.lon, leg.to.lat)]
        };
        overlay.paths.push(OverlayPath::from_lonlat(
            &path,
            color,
            if leg.is_walk() { 4.0 } else { 6.0 },
            leg.is_walk(),
        ));
        if !leg.is_walk() {
            for stop in [&leg.from, &leg.to] {
                overlay
                    .stops
                    .push(OverlayStop::from_lonlat(stop.lon, stop.lat, color, false));
            }
        }
    }
    if let (Some(first), Some(last)) = (itinerary.legs.first(), itinerary.legs.last()) {
        overlay.stops.push(OverlayStop::from_lonlat(
            first.from.lon,
            first.from.lat,
            START_COLOR,
            true,
        ));
        overlay.stops.push(OverlayStop::from_lonlat(
            last.to.lon,
            last.to.lat,
            END_COLOR,
            true,
        ));
    }
    overlay
}

fn itinerary_card(
    itinerary: &Itinerary,
    selected: bool,
    now: i64,
    index: usize,
    mut selection: State<usize>,
    transit: TransitController,
) -> Element {
    let first_vehicle = itinerary.legs.iter().find(|leg| !leg.is_walk());
    let leaves_in = (itinerary.start - now).div_euclid(60);
    let leave_text = if leaves_in <= 0 {
        "Leave now".to_string()
    } else {
        format!("Leave in {}", duration_text(leaves_in * 60))
    };
    let mut facts = vec![leave_text];
    if itinerary.transfers > 0 {
        facts.push(format!(
            "{} transfer{}",
            itinerary.transfers,
            if itinerary.transfers == 1 { "" } else { "s" }
        ));
    }
    let walk = itinerary.walk_s();
    if walk >= 60 {
        facts.push(format!("walk {}", duration_text(walk)));
    }
    if let Some(leg) = first_vehicle
        && !leg.from.name.is_empty()
    {
        facts.push(format!("from {}", leg.from.name));
    }

    let chips: Vec<Element> = itinerary
        .legs
        .iter()
        .filter(|leg| !leg.is_walk() || leg.end - leg.start >= 60)
        .enumerate()
        .flat_map(|(position, leg)| {
            let separator = (position > 0)
                .then(|| icon(lucide::chevron_right(), TEXT_FAINT, 12.0).into_element());
            let chip = if leg.is_walk() {
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(2.0)
                    .child(icon(lucide::footprints(), TEXT_MUTED, 14.0))
                    .child(
                        label()
                            .text(format!("{}", (leg.end - leg.start + 30) / 60))
                            .font_size(text_size(11.0))
                            .color(TEXT_MUTED),
                    )
                    .into_element()
            } else {
                line_badge(leg.mode, &leg.line, leg.color, leg.text_color)
            };
            separator.into_iter().chain(std::iter::once(chip))
        })
        .collect();

    let itinerary_for_map = itinerary.clone();
    let mut map = transit.map.clone();

    rect()
        .vertical()
        .width(Size::fill())
        .padding(12.0f32)
        .margin((0.0f32, 0.0f32, 8.0f32, 0.0f32))
        .corner_radius(10.0f32)
        .background(if selected {
            Color::from_rgb(246, 249, 255)
        } else {
            Color::WHITE
        })
        .border(
            Border::new()
                .fill(if selected { ACCENT } else { DIVIDER })
                .width(if selected { 1.5 } else { 1.0 })
                .alignment(BorderAlignment::Inner),
        )
        .cursor(CursorIcon::Pointer)
        .on_press(move |_: Event<PressEventData>| {
            selection.set(index);
            show_on_map(&mut map, journey_overlay(&itinerary_for_map));
        })
        .child(
            rect()
                .horizontal()
                .content(Content::flex())
                .width(Size::fill())
                .cross_align(Alignment::Center)
                .child(
                    label()
                        .text(format!(
                            "{} → {}",
                            clock_time(itinerary.start),
                            clock_time(itinerary.end)
                        ))
                        .width(Size::flex(1.0))
                        .font_size(text_size(15.0))
                        .font_weight(FontWeight::BOLD)
                        .color(TEXT),
                )
                .child(
                    label()
                        .text(duration_text(itinerary.duration_s()))
                        .font_size(text_size(15.0))
                        .font_weight(FontWeight::BOLD)
                        .color(TEXT),
                ),
        )
        .child(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(4.0)
                .margin((8.0f32, 0.0f32, 6.0f32, 0.0f32))
                .overflow(Overflow::Clip)
                .children(chips),
        )
        .child(
            label()
                .text(facts.join(" · "))
                .font_size(text_size(11.0))
                .color(TEXT_MUTED)
                .width(Size::fill()),
        )
        .maybe(selected, |r| {
            r.child(
                rect()
                    .vertical()
                    .width(Size::fill())
                    .margin((10.0f32, 0.0f32, 0.0f32, 0.0f32))
                    .child(divider())
                    .children(itinerary.legs.iter().map(leg_detail)),
            )
        })
        .into_element()
}

/// One leg of the selected itinerary.
fn leg_detail(leg: &Leg) -> Element {
    if leg.is_walk() {
        let distance = leg
            .distance_m
            .filter(|meters| *meters >= 1.0)
            .map(|meters| format!(" ({} m)", meters.round() as i64))
            .unwrap_or_default();
        return rect()
            .horizontal()
            .content(Content::flex())
            .width(Size::fill())
            .cross_align(Alignment::Center)
            .spacing(10.0)
            .padding((8.0f32, 0.0f32))
            .child(
                rect()
                    .width(Size::px(56.0))
                    .main_align(Alignment::Center)
                    .cross_align(Alignment::Center)
                    .child(icon(lucide::footprints(), TEXT_MUTED, 16.0)),
            )
            .child(
                label()
                    .text(format!(
                        "Walk {}{distance} to {}",
                        duration_text(leg.end - leg.start),
                        leg.to.name
                    ))
                    .width(Size::flex(1.0))
                    .font_size(text_size(12.0))
                    .color(TEXT_MUTED),
            )
            .into_element();
    }

    let stops = leg.intermediate.len() + 1;
    let live = match (leg.realtime, leg.delay_min) {
        (true, Some(delay)) if delay > 0 => Some((format!("{delay} min late"), LATE)),
        (true, _) => Some(("on time".to_string(), LIVE)),
        _ => None,
    };
    let mode_word = match leg.mode {
        Mode::Bus => "bus",
        Mode::Tram => "tram",
        Mode::Subway => "metro",
        Mode::Rail => "train",
        Mode::Ferry => "boat",
        Mode::Walk | Mode::Other => "vehicle",
    };

    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .spacing(10.0)
        .padding((8.0f32, 0.0f32))
        .child(line_badge(leg.mode, &leg.line, leg.color, leg.text_color))
        .child(
            rect()
                .vertical()
                .width(Size::flex(1.0))
                .spacing(2.0)
                .child(
                    label()
                        .text(format!("→ {}", leg.headsign))
                        .font_size(text_size(13.0))
                        .font_weight(FontWeight::BOLD)
                        .color(TEXT)
                        .width(Size::fill()),
                )
                .child(
                    label()
                        .text(format!(
                            "{}  Board at {}",
                            clock_time(leg.start),
                            leg.from.name
                        ))
                        .font_size(text_size(12.0))
                        .color(TEXT)
                        .width(Size::fill()),
                )
                .child(
                    label()
                        .text(format!(
                            "{stops} stop{} · {} by {mode_word}",
                            if stops == 1 { "" } else { "s" },
                            duration_text(leg.end - leg.start)
                        ))
                        .font_size(text_size(11.0))
                        .color(TEXT_FAINT),
                )
                .child(
                    label()
                        .text(format!(
                            "{}  Get off at {}",
                            clock_time(leg.end),
                            leg.to.name
                        ))
                        .font_size(text_size(12.0))
                        .color(TEXT)
                        .width(Size::fill()),
                )
                .maybe_child(live.map(|(text, color)| {
                    label()
                        .text(text)
                        .font_size(text_size(11.0))
                        .color(color)
                        .into_element()
                })),
        )
        .into_element()
}
