//! Drawer section listing a stop's next departures (see [`super::api`]), refreshed while open.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use freya::{icons::lucide, prelude::*};

use map_component::util::tiles::background_delay;

use crate::components::favorites::StopLine;

use super::{
    api::{self, Departure, StopDepartures},
    controller::TransitController,
    ui::{
        DIVIDER, LATE, LINK, LIVE, TEXT, TEXT_FAINT, TEXT_MUTED, clock_time, icon, line_badge,
        status_line,
    },
    when::{When, WhenPicker},
};

/// Departures are realtime estimates; refetch this often while the drawer shows them.
const REFRESH_EVERY: Duration = Duration::from_secs(30);

/// Departures further out than this show a clock time instead of "in N min".
const RELATIVE_LIMIT_MIN: i64 = 60;

#[derive(Clone, PartialEq)]
enum Load {
    Loading,
    Ready(StopDepartures),
    NoStop,
    Failed,
}

/// Next departures from the stop at (`lat`, `lon`), named `name` on the map — from now, or from
/// a chosen date and time. Keyed by the selected place's id, so selecting another stop starts
/// over (back to "now") instead of showing the previous stop's departures while it loads.
#[derive(PartialEq)]
pub struct Departures {
    pub place_id: u64,
    pub lat: f64,
    pub lon: f64,
    pub name: String,
    /// Only this line's departures (its OSM `ref`, e.g. `"14"`), when set.
    pub line: Option<String>,
}

impl Component for Departures {
    fn render(&self) -> impl IntoElement {
        let when = use_state(When::default);
        DepartureBoard {
            place_id: self.place_id,
            lat: self.lat,
            lon: self.lon,
            name: self.name.clone(),
            line: self.line.clone(),
            when: *when.read(),
            when_state: when,
        }
    }

    fn render_key(&self) -> DiffKey {
        crate::components::place::section_key("departures", self.place_id)
    }
}

/// [`Departures`] for one `when`: keyed by it, so another choice starts a fresh fetch loop.
#[derive(PartialEq)]
struct DepartureBoard {
    place_id: u64,
    lat: f64,
    lon: f64,
    name: String,
    line: Option<String>,
    when: When,
    when_state: State<When>,
}

impl Component for DepartureBoard {
    fn render(&self) -> impl IntoElement {
        let state = use_state(|| Load::Loading);
        let transit = use_consume::<TransitController>();
        let platform = use_hook(|| Platform::get().current_window());

        // Lives as long as this section is shown: the task is tied to this component's scope,
        // so closing the drawer or selecting another place stops the refresh loop.
        use_hook({
            let (lat, lon, name, when) = (self.lat, self.lon, self.name.clone(), self.when);
            // One line's departures are a few among the stop's: ask for more to filter from.
            let count = if self.line.is_some() {
                api::DEPARTURE_COUNT * 4
            } else {
                api::DEPARTURE_COUNT
            };
            let mut state = state;
            move || {
                spawn(async move {
                    loop {
                        let next =
                            match api::departures_near(lat, lon, name.clone(), when, count).await {
                                Ok(Some(found)) => Load::Ready(found),
                                Ok(None) => Load::NoStop,
                                Err(err) => {
                                    log::warn!("[transit] departures failed: {err:?}");
                                    // Keep showing the last good list through a transient failure.
                                    if matches!(*state.peek(), Load::Ready(_)) {
                                        background_delay(REFRESH_EVERY).await;
                                        continue;
                                    }
                                    Load::Failed
                                }
                            };
                        if *state.peek() != next {
                            state.set(next);
                            platform.send(UserEvent::RequestRedraw);
                        }
                        if matches!(*state.peek(), Load::NoStop) {
                            return;
                        }
                        background_delay(REFRESH_EVERY).await;
                    }
                })
            }
        });

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);

        let body: Vec<Element> = match &*state.read() {
            Load::Loading => vec![status_line(
                CircularLoader::new().size(16.0).into_element(),
                "Loading departures…",
            )],
            Load::NoStop => vec![status_line(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "No timetable found for this stop",
            )],
            Load::Failed => vec![status_line(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "Couldn't load departures",
            )],
            Load::Ready(found) if shown(found, &self.line).next().is_none() => vec![status_line(
                icon(lucide::clock(), TEXT_FAINT, 16.0).into_element(),
                "No upcoming departures",
            )],
            Load::Ready(found) => shown(found, &self.line)
                .map(|departure| {
                    let mut transit = transit.clone();
                    let departure_for_trip = departure.clone();
                    let stop = found.stop.name.clone();
                    let position = (self.lat, self.lon);
                    rect()
                        .width(Size::fill())
                        .cursor(CursorIcon::Pointer)
                        .on_press(move |_: Event<PressEventData>| {
                            transit.show_trip(
                                departure_for_trip.clone(),
                                stop.clone(),
                                Some(position),
                            )
                        })
                        .child(departure_row(departure, now))
                        .into_element()
                })
                .collect(),
        };
        // Asked for one line, which the timetable doesn't name: say why every line shows.
        let unmatched_line = match (&*state.read(), &self.line) {
            (Load::Ready(found), Some(line))
                if !found.departures.is_empty() && !line_in_timetable(found, line) =>
            {
                Some(line.clone())
            }
            _ => None,
        };
        let stop_name = match &*state.read() {
            Load::Ready(found) if found.stop.name != self.name => Some(found.stop.name.clone()),
            _ => None,
        };

        rect()
            .vertical()
            .width(Size::fill())
            .padding(12.0f32)
            .corner_radius(10.0f32)
            .border(
                Border::new()
                    .fill(DIVIDER)
                    .width(1.0)
                    .alignment(BorderAlignment::Inner),
            )
            .margin((0.0f32, 0.0f32, 12.0f32, 0.0f32))
            .child(
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .cross_align(Alignment::Center)
                    .spacing(8.0)
                    .margin((0.0f32, 0.0f32, 6.0f32, 0.0f32))
                    .child(icon(lucide::clock(), TEXT_MUTED, 16.0))
                    .child(
                        label()
                            .text(if self.when == When::Now {
                                "Next departures"
                            } else {
                                "Departures"
                            })
                            .width(Size::flex(1.0))
                            .font_size(13.0)
                            .font_weight(FontWeight::BOLD)
                            .color(TEXT),
                    ),
            )
            .child(
                rect()
                    .margin((0.0f32, 0.0f32, 6.0f32, 0.0f32))
                    .child(WhenPicker {
                        when: self.when_state,
                        allow_arrive: false,
                        icon: false,
                    }),
            )
            .maybe_child(unmatched_line.map(|line| {
                label()
                    .text(format!(
                        "The timetable doesn't name line {line} — showing every departure."
                    ))
                    .width(Size::fill())
                    .margin((0.0f32, 0.0f32, 4.0f32, 0.0f32))
                    .font_size(11.0)
                    .color(TEXT_FAINT)
                    .into_element()
            }))
            // Transitous' own name for the stop, when it differs from the map's — confirms which
            // stop was matched.
            .maybe_child(stop_name.map(|name| {
                label()
                    .text(format!("Stop: {name}"))
                    .width(Size::fill())
                    .margin((0.0f32, 0.0f32, 4.0f32, 24.0f32))
                    .font_size(11.0)
                    .color(TEXT_FAINT)
                    .max_lines(1)
                    .text_overflow(TextOverflow::Ellipsis)
                    .into_element()
            }))
            .children(body)
            .child(
                rect()
                    .horizontal()
                    .cursor(CursorIcon::Pointer)
                    .margin((8.0f32, 0.0f32, 0.0f32, 0.0f32))
                    .on_press(|_: Event<PressEventData>| Platform::get().open_url(api::SOURCES_URL))
                    .child(
                        label()
                            .text("Timetables: Transitous · data sources")
                            .font_size(11.0)
                            .color(LINK),
                    ),
            )
    }

    fn render_key(&self) -> DiffKey {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        ("departure-board", self.place_id, self.when).hash(&mut hasher);
        DiffKey::U64(hasher.finish())
    }
}

/// The departures to list: all, or `line`'s, at most [`api::DEPARTURE_COUNT`]. `line` only
/// applies when the timetable names any departure that way: some operators name runs by train
/// number (SNCF's "TER 89131") where OSM has the line ("TER 01"), and then all are listed.
fn shown<'a>(
    found: &'a StopDepartures,
    line: &'a Option<String>,
) -> impl Iterator<Item = &'a Departure> {
    let line = line.as_ref().filter(|line| line_in_timetable(found, line));
    found
        .departures
        .iter()
        .filter(move |departure| line.is_none_or(|line| api::same_line(&departure.line, line)))
        .take(api::DEPARTURE_COUNT as usize)
}

fn line_in_timetable(found: &StopDepartures, line: &str) -> bool {
    found
        .departures
        .iter()
        .any(|departure| api::same_line(&departure.line, line))
}

fn departure_row(departure: &Departure, now: i64) -> impl IntoElement {
    let minutes = (departure.time - now).div_euclid(60);

    let (when, when_color) = if departure.cancelled {
        ("Cancelled".to_string(), LATE)
    } else if minutes <= 0 {
        ("Now".to_string(), LIVE)
    } else if minutes < RELATIVE_LIMIT_MIN {
        (format!("{minutes} min"), TEXT)
    } else {
        (clock_time(departure.time), TEXT)
    };
    let detail = match departure.delay_min {
        _ if departure.cancelled => None,
        Some(delay) if delay > 0 => Some((format!("+{delay} min"), LATE)),
        Some(_) => Some(("on time".to_string(), LIVE)),
        None => None,
    };

    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .cross_align(Alignment::Center)
        .spacing(10.0)
        .padding((6.0f32, 0.0f32))
        .child(line_badge(
            departure.mode,
            &departure.line,
            departure.color,
            departure.text_color,
        ))
        .child(
            label()
                .text(departure.headsign.clone())
                .width(Size::flex(1.0))
                .font_size(13.0)
                .color(if departure.cancelled {
                    TEXT_FAINT
                } else {
                    TEXT
                })
                .max_lines(1)
                .text_overflow(TextOverflow::Ellipsis),
        )
        .child(
            rect()
                .vertical()
                .cross_align(Alignment::End)
                .child(
                    rect()
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(5.0)
                        .maybe_child((departure.realtime && !departure.cancelled).then(|| {
                            rect()
                                .width(Size::px(6.0))
                                .height(Size::px(6.0))
                                .corner_radius(3.0f32)
                                .background(LIVE)
                                .into_element()
                        }))
                        .child(
                            label()
                                .text(when)
                                .font_size(13.0)
                                .font_weight(FontWeight::BOLD)
                                .color(when_color),
                        ),
                )
                .maybe_child(detail.map(|(text, color)| {
                    label()
                        .text(text)
                        .font_size(10.0)
                        .color(color)
                        .into_element()
                })),
        )
}

/// The next departures of one saved line and direction at its stop
/// (`favorites::StopLine`), as a row of times — refreshed while shown. Tapping a time opens
/// that trip.
#[derive(PartialEq)]
pub struct NextTimes {
    pub stop_line: StopLine,
}

/// How many times [`NextTimes`] shows.
const NEXT_TIMES: usize = 4;

impl Component for NextTimes {
    fn render(&self) -> impl IntoElement {
        let state = use_state(|| Load::Loading);
        let transit = use_consume::<TransitController>();
        let platform = use_hook(|| Platform::get().current_window());

        use_hook({
            let saved = self.stop_line.clone();
            let mut state = state;
            move || {
                spawn(async move {
                    loop {
                        let result = api::departures_near(
                            saved.lat,
                            saved.lon,
                            saved.stop.clone(),
                            When::Now,
                            api::DEPARTURE_COUNT * 4,
                        )
                        .await;
                        let next = match result {
                            Ok(Some(found)) => Load::Ready(found),
                            Ok(None) => Load::NoStop,
                            Err(err) => {
                                log::warn!("[transit] next departures failed: {err:?}");
                                if matches!(*state.peek(), Load::Ready(_)) {
                                    background_delay(REFRESH_EVERY).await;
                                    continue;
                                }
                                Load::Failed
                            }
                        };
                        if *state.peek() != next {
                            state.set(next);
                            platform.send(UserEvent::RequestRedraw);
                        }
                        background_delay(REFRESH_EVERY).await;
                    }
                })
            }
        });

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let saved = &self.stop_line;
        let note = |text: &str| {
            label()
                .text(text.to_string())
                .font_size(12.0)
                .color(TEXT_FAINT)
                .into_element()
        };

        let times: Vec<Element> = match &*state.read() {
            Load::Loading => vec![CircularLoader::new().size(14.0).into_element()],
            Load::NoStop => vec![note("No timetable for this stop")],
            Load::Failed => vec![note("Couldn't load departures")],
            Load::Ready(found) => {
                let matching: Vec<&Departure> = found
                    .departures
                    .iter()
                    .filter(|departure| {
                        departure.line.eq_ignore_ascii_case(&saved.line)
                            && departure
                                .headsign
                                .trim()
                                .eq_ignore_ascii_case(saved.headsign.trim())
                    })
                    .take(NEXT_TIMES)
                    .collect();
                if matching.is_empty() {
                    vec![note("No upcoming departures")]
                } else {
                    matching
                        .into_iter()
                        .map(|departure| {
                            let mut transit = transit.clone();
                            let trip = departure.clone();
                            let stop = found.stop.name.clone();
                            let position = (saved.lat, saved.lon);
                            rect()
                                .padding((4.0f32, 10.0f32))
                                .corner_radius(8.0f32)
                                .background(Color::from_rgb(244, 246, 248))
                                .cursor(CursorIcon::Pointer)
                                .on_press(move |_: Event<PressEventData>| {
                                    transit.show_trip(trip.clone(), stop.clone(), Some(position))
                                })
                                .child(time_chip(departure, now))
                                .into_element()
                        })
                        .collect()
                }
            }
        };

        rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(6.0)
            .children(times)
    }
}

/// One departure as a short time ("Now", "4 min", "21:03"), with its live/late status.
fn time_chip(departure: &Departure, now: i64) -> impl IntoElement {
    let minutes = (departure.time - now).div_euclid(60);
    let (text, color) = if departure.cancelled {
        ("Cancelled".to_string(), LATE)
    } else if minutes <= 0 {
        ("Now".to_string(), LIVE)
    } else if minutes < RELATIVE_LIMIT_MIN {
        (format!("{minutes} min"), TEXT)
    } else {
        (clock_time(departure.time), TEXT)
    };
    let late = departure
        .delay_min
        .filter(|delay| *delay > 0 && !departure.cancelled);

    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(4.0)
        .maybe_child((departure.realtime && !departure.cancelled).then(|| {
            rect()
                .width(Size::px(6.0))
                .height(Size::px(6.0))
                .corner_radius(3.0f32)
                .background(if late.is_some() { LATE } else { LIVE })
                .into_element()
        }))
        .child(
            label()
                .text(text)
                .font_size(13.0)
                .font_weight(FontWeight::BOLD)
                .color(color),
        )
        .maybe_child(late.map(|delay| {
            label()
                .text(format!("+{delay}"))
                .font_size(10.0)
                .color(LATE)
                .into_element()
        }))
}
