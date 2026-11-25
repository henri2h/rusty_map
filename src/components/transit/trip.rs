//! The journey planner's results: a summary card per itinerary for the list, and the chosen
//! one's step-by-step timeline (see [`TripDetail`]).
//!
//! The timeline is a grid of rows — time | rail | text — where the rail runs down the line's
//! colour while riding and dotted while walking, with a node at every place the trip stops at:
//! its start, each stop it boards or leaves a vehicle at, its end. Each row is a `Content::fit`
//! row so the rail column stretches to whatever height the text beside it takes.

use std::{cmp::Reverse, collections::HashSet};

use freya::{icons::lucide, prelude::*};

use crate::mobile::{TOUCH, text_size};

use super::{
    api::Mode,
    controller::{END_COLOR, START_COLOR, TransitController},
    journey::{Itinerary, Leg},
    ui::{
        ACCENT, DIVIDER, LATE, LINK, LIVE, TEXT, TEXT_FAINT, TEXT_MUTED, clock_time, duration_text,
        icon, line_badge, rgb_or,
    },
    when::When,
};

/// Width of the timeline's time column.
const TIME_WIDTH: f32 = 44.0;
/// Width of the column the rail runs down.
const RAIL_COLUMN: f32 = 24.0;
const RAIL_WIDTH: f32 = 4.0;
/// From a node row's top to the middle of its first line of text, where its dot sits.
const NODE_DOT_Y: f32 = 10.0;
/// Light grey: walking rails, placeholders.
const QUIET: Color = Color::from_rgb(200, 203, 208);
const PLACEHOLDER: Color = Color::from_rgb(241, 243, 244);
const SELECTED_BACKGROUND: Color = Color::from_rgb(246, 249, 255);

// ─── Ordering and tags ────────────────────────────────────────────────────────

/// The order the list shows: earliest arrival first (then fewer transfers, less walking, later
/// departure); for an "arrive by" journey, latest departure first. Transitous' own order is by
/// departure, which could put an hour-long trip above one twenty minutes faster.
pub fn sort(itineraries: &mut [Itinerary], when: When) {
    match when {
        When::Arrive(_) => itineraries.sort_by_key(|itinerary| {
            (
                Reverse(itinerary.start),
                itinerary.duration_s(),
                itinerary.transfers,
            )
        }),
        When::Now | When::Depart(_) => itineraries.sort_by_key(|itinerary| {
            (
                itinerary.end,
                itinerary.transfers,
                itinerary.walk_s(),
                Reverse(itinerary.start),
            )
        }),
    }
}

/// What an itinerary is compared on for a tag: lower is better.
type Measure = fn(&Itinerary) -> i64;

/// A label for each itinerary that alone is best at something: the fastest, the one with the
/// fewest transfers, the one with the least walking — one label at most per itinerary, and
/// none for a lone result.
pub fn tags(itineraries: &[Itinerary]) -> Vec<Option<&'static str>> {
    let mut tags = vec![None; itineraries.len()];
    if itineraries.len() < 2 {
        return tags;
    }
    let criteria: [(&'static str, Measure); 3] = [
        ("Fastest", |itinerary| itinerary.duration_s() / 60),
        ("Fewest transfers", |itinerary| itinerary.transfers as i64),
        ("Least walking", |itinerary| itinerary.walk_s() / 60),
    ];
    for (label, key) in criteria {
        let Some(best) = itineraries.iter().map(key).min() else {
            continue;
        };
        let mut winners = itineraries
            .iter()
            .enumerate()
            .filter(|(_, itinerary)| key(itinerary) == best)
            .map(|(index, _)| index);
        if let (Some(winner), None) = (winners.next(), winners.next())
            && tags[winner].is_none()
        {
            tags[winner] = Some(label);
        }
    }
    tags
}

// ─── Summary card ─────────────────────────────────────────────────────────────

/// One itinerary in the results list: times and duration, its legs as badges, where the first
/// vehicle leaves from, and the cost in transfers and walking. A press opens its steps.
pub fn itinerary_card(
    itinerary: &Itinerary,
    tag: Option<&'static str>,
    selected: bool,
    now: i64,
    mut on_press: impl FnMut() + 'static,
) -> Element {
    let first_ride = itinerary.legs.iter().find(|leg| !leg.is_walk());
    let departure: Element = match first_ride {
        Some(ride) => rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(6.0)
            .width(Size::fill())
            .child(
                label()
                    .text(format!(
                        "{} from {}",
                        clock_time(ride.start),
                        ride.from.name
                    ))
                    .font_size(text_size(12.0))
                    .color(TEXT)
                    .max_lines(1)
                    .text_overflow(TextOverflow::Ellipsis),
            )
            .maybe_child(live_status(ride).map(|(text, color)| {
                label()
                    .text(text)
                    .font_size(text_size(12.0))
                    .color(color)
                    .into_element()
            }))
            .into_element(),
        None => label()
            .text("Walk the whole way")
            .font_size(text_size(12.0))
            .color(TEXT)
            .into_element(),
    };

    rect()
        .vertical()
        .width(Size::fill())
        .spacing(6.0)
        .padding(12.0f32)
        .margin((0.0f32, 0.0f32, 8.0f32, 0.0f32))
        .corner_radius(12.0f32)
        .background(if selected {
            SELECTED_BACKGROUND
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
        .on_press(move |_: Event<PressEventData>| on_press())
        .child(
            rect()
                .horizontal()
                .content(Content::flex())
                .width(Size::fill())
                .cross_align(Alignment::Center)
                .spacing(8.0)
                .child(
                    label()
                        .text(format!(
                            "{} – {}",
                            clock_time(itinerary.start),
                            clock_time(itinerary.end)
                        ))
                        .font_size(text_size(15.0))
                        .font_weight(FontWeight::BOLD)
                        .color(TEXT),
                )
                .maybe_child(tag.map(tag_pill))
                .child(rect().width(Size::flex(1.0)))
                .child(
                    label()
                        .text(duration_text(itinerary.duration_s()))
                        .font_size(text_size(15.0))
                        .font_weight(FontWeight::BOLD)
                        .color(TEXT),
                )
                .child(icon(lucide::chevron_right(), TEXT_FAINT, 16.0)),
        )
        .child(leg_strip(itinerary))
        .child(departure)
        .child(
            label()
                .text(costs(itinerary, now).join(" · "))
                .font_size(text_size(11.0))
                .color(TEXT_MUTED)
                .width(Size::fill()),
        )
        .into_element()
}

/// Stand-in for a card while the itineraries load, about as tall, so the list doesn't jump
/// when they arrive.
pub fn placeholder_card() -> Element {
    let bar = |width: f32, height: f32| {
        rect()
            .width(Size::px(width))
            .height(Size::px(height))
            .corner_radius(4.0f32)
            .background(PLACEHOLDER)
            .into_element()
    };
    rect()
        .vertical()
        .width(Size::fill())
        .spacing(10.0)
        .padding(12.0f32)
        .margin((0.0f32, 0.0f32, 8.0f32, 0.0f32))
        .corner_radius(12.0f32)
        .border(
            Border::new()
                .fill(DIVIDER)
                .width(1.0)
                .alignment(BorderAlignment::Inner),
        )
        .child(bar(120.0, 16.0))
        .child(bar(200.0, 20.0))
        .child(bar(170.0, 12.0))
        .child(bar(140.0, 10.0))
        .into_element()
}

fn tag_pill(text: &'static str) -> Element {
    rect()
        .padding((2.0f32, 8.0f32))
        .corner_radius(10.0f32)
        .background(Color::from_rgb(230, 244, 234))
        .child(
            label()
                .text(text)
                .font_size(text_size(11.0))
                .font_weight(FontWeight::BOLD)
                .color(LIVE),
        )
        .into_element()
}

/// `"Leave in 4 min"`, transfers, walking: what the trip asks of you.
fn costs(itinerary: &Itinerary, now: i64) -> Vec<String> {
    let leaves_in = (itinerary.start - now).div_euclid(60);
    let mut costs = vec![if leaves_in <= 0 {
        "Leave now".to_string()
    } else {
        format!("Leave in {}", duration_text(leaves_in * 60))
    }];
    costs.push(match itinerary.transfers {
        0 => "Direct".to_string(),
        1 => "1 transfer".to_string(),
        transfers => format!("{transfers} transfers"),
    });
    let walk = itinerary.walk_s();
    if walk >= 60 {
        costs.push(format!("{} walk", duration_text(walk)));
    }
    costs
}

/// The legs at a glance — line badges, walks as a footprint and their minutes — wrapping onto
/// another line rather than being cut off.
pub fn leg_strip(itinerary: &Itinerary) -> Element {
    // One height for every chip, so wrapped lines and mixed chips line up.
    let chip_height = if TOUCH { 26.0 } else { 22.0 };
    let cell = |child: Element| {
        rect()
            .height(Size::px(chip_height))
            .main_align(Alignment::Center)
            .child(child)
            .into_element()
    };
    let chips: Vec<Element> = itinerary
        .legs
        .iter()
        // A walk under a minute (across a platform) isn't worth a chip.
        .filter(|leg| !leg.is_walk() || leg.end - leg.start >= 60)
        .enumerate()
        .flat_map(|(position, leg)| {
            let separator = (position > 0)
                .then(|| cell(icon(lucide::chevron_right(), TEXT_FAINT, 12.0).into_element()));
            let chip = if leg.is_walk() {
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(1.0)
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
            separator.into_iter().chain(std::iter::once(cell(chip)))
        })
        .collect();
    rect()
        .horizontal()
        .content(Content::wrap_spacing(4.0))
        .width(Size::fill())
        .spacing(4.0)
        .children(chips)
        .into_element()
}

/// `"on time"` / `"+3 min"` for a ride with live data, `"Cancelled"` for a cancelled departure.
fn live_status(ride: &Leg) -> Option<(String, Color)> {
    if ride.from.cancelled {
        return Some(("Cancelled".to_string(), LATE));
    }
    match (ride.realtime, ride.delay_min) {
        (true, Some(delay)) if delay > 0 => Some((format!("+{delay} min"), LATE)),
        (true, _) => Some(("on time".to_string(), LIVE)),
        _ => None,
    }
}

// ─── Step-by-step timeline ────────────────────────────────────────────────────

/// The chosen itinerary's steps, under a header that leads back to the list.
#[derive(PartialEq)]
pub struct TripDetail {
    pub itinerary: Itinerary,
    /// The journey's ends as the planner names them now (a map pick's address comes in late).
    pub origin: String,
    pub destination: String,
    pub now: i64,
    /// Whether the steps drag-scroll (see `sheet::SheetState::content_scrolls`).
    pub content_scrolls: bool,
}

impl Component for TripDetail {
    fn render(&self) -> impl IntoElement {
        let scroll = use_consume::<SheetScroll>();
        let mut transit = use_consume::<TransitController>();
        // Rides showing every stop they pass, by leg index.
        let expanded = use_state(HashSet::<usize>::new);
        let itinerary = &self.itinerary;

        let header = rect()
            .vertical()
            .width(Size::fill())
            .spacing(8.0)
            .padding((0.0f32, 0.0f32, 12.0f32, 0.0f32))
            .child(
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .spacing(8.0)
                    .child(
                        Button::new()
                            .flat()
                            .maybe(!TOUCH, |button| button.compact())
                            .on_press(move |_: Event<PressEventData>| transit.close_trip())
                            .child(icon(lucide::arrow_left(), TEXT_MUTED, 18.0)),
                    )
                    .child(
                        rect()
                            .vertical()
                            .width(Size::flex(1.0))
                            .child(
                                label()
                                    .text(format!(
                                        "{} – {}",
                                        clock_time(itinerary.start),
                                        clock_time(itinerary.end)
                                    ))
                                    .font_size(text_size(17.0))
                                    .font_weight(FontWeight::BOLD)
                                    .color(TEXT),
                            )
                            .child(
                                label()
                                    .text(costs(itinerary, self.now).join(" · "))
                                    .font_size(text_size(12.0))
                                    .color(TEXT_MUTED),
                            )
                            // Under the times, aligned with them past the back button.
                            .child(
                                rect()
                                    .margin((8.0f32, 0.0f32, 0.0f32, 0.0f32))
                                    .child(leg_strip(itinerary)),
                            ),
                    )
                    .child(
                        label()
                            .text(duration_text(itinerary.duration_s()))
                            .font_size(text_size(17.0))
                            .font_weight(FontWeight::BOLD)
                            .color(TEXT),
                    ),
            );

        scroll
            .track(ScrollView::new())
            .drag_scrolling(self.content_scrolls)
            .height(Size::fill())
            .direction(Direction::vertical())
            .child(scroll.marker())
            .child(header)
            .child(
                rect()
                    .height(Size::px(1.0))
                    .width(Size::fill())
                    .background(DIVIDER)
                    .margin((0.0f32, 0.0f32, 14.0f32, 0.0f32)),
            )
            .children(timeline(
                itinerary,
                &self.origin,
                &self.destination,
                expanded,
            ))
            .child(rect().height(Size::px(16.0)))
    }

    fn render_key(&self) -> DiffKey {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        ("trip", self.itinerary.start, self.itinerary.end).hash(&mut hasher);
        self.itinerary.legs.len().hash(&mut hasher);
        DiffKey::U64(hasher.finish())
    }
}

/// What the rail looks like along a stretch of the trip.
#[derive(Clone, Copy, PartialEq)]
enum Rail {
    /// Nothing: above the start, below the end.
    None,
    Walk,
    Ride(Color),
}

impl Rail {
    fn of(leg: &Leg) -> Self {
        if leg.is_walk() {
            Rail::Walk
        } else {
            Rail::Ride(rgb_or(leg.color, ACCENT))
        }
    }
}

/// How a node's dot looks.
#[derive(Clone, Copy)]
enum Dot {
    Start,
    End,
    /// A stop, ringed in the colour of the line boarded or left there.
    Stop(Color),
}

fn timeline(
    itinerary: &Itinerary,
    origin: &str,
    destination: &str,
    expanded: State<HashSet<usize>>,
) -> Vec<Element> {
    let legs = &itinerary.legs;
    let Some(first) = legs.first() else {
        return Vec::new();
    };
    let mut rows = vec![node_row(
        NodeTime::new(first.start, live_status_if_ride(first)),
        origin,
        Rail::None,
        Rail::of(first),
        Dot::Start,
    )];
    for (index, leg) in legs.iter().enumerate() {
        if leg.is_walk() {
            rows.push(walk_row(leg));
        } else {
            rows.extend(ride_rows(index, leg, expanded));
        }
        match legs.get(index + 1) {
            // Where this leg ends and the next starts: boarding (its departure time and live
            // status) if the next one rides, else getting off (this one's arrival).
            Some(next) => {
                let time = if next.is_walk() {
                    NodeTime::new(leg.end, None)
                } else {
                    NodeTime::new(next.start, live_status(next))
                };
                let ride_color = [next, leg].into_iter().find_map(|leg| match Rail::of(leg) {
                    Rail::Ride(color) => Some(color),
                    _ => None,
                });
                rows.push(node_row(
                    time,
                    if next.from.name.is_empty() {
                        &leg.to.name
                    } else {
                        &next.from.name
                    },
                    Rail::of(leg),
                    Rail::of(next),
                    Dot::Stop(ride_color.unwrap_or(QUIET)),
                ));
            }
            None => rows.push(node_row(
                NodeTime::new(leg.end, None),
                destination,
                Rail::of(leg),
                Rail::None,
                Dot::End,
            )),
        }
    }
    rows
}

fn live_status_if_ride(leg: &Leg) -> Option<(String, Color)> {
    (!leg.is_walk()).then(|| live_status(leg)).flatten()
}

/// A node's time column: the clock time, and under it the live status of what leaves there.
struct NodeTime {
    time: i64,
    status: Option<(String, Color)>,
}

impl NodeTime {
    fn new(time: i64, status: Option<(String, Color)>) -> Self {
        Self { time, status }
    }
}

/// A row of the timeline: `time` right-aligned in its column, `rail` stretched to the row's
/// height, `content` taking the rest.
fn timeline_row(time: Option<Element>, rail: Element, content: Element) -> Element {
    rect()
        .horizontal()
        // Lets the rail column fill the row's height (`Size::fill_minimum`).
        .content(Content::fit())
        .width(Size::fill())
        .child(
            rect()
                .width(Size::px(TIME_WIDTH))
                .vertical()
                .cross_align(Alignment::End)
                .maybe_child(time),
        )
        .child(rail)
        .child(rect().width(Size::fill()).child(content))
        .into_element()
}

/// A rail segment `height` tall, in the middle of the rail column's width.
fn rail_segment(rail: Rail, height: Size) -> Element {
    let segment = rect().width(Size::px(RAIL_WIDTH)).height(height);
    match rail {
        Rail::None => segment.into_element(),
        Rail::Ride(color) => segment
            .corner_radius(RAIL_WIDTH / 2.0)
            .background(color)
            .into_element(),
        // Dotted: more dots than any walk row is tall, the rest clipped.
        Rail::Walk => segment
            .vertical()
            .overflow(Overflow::Clip)
            .spacing(4.0)
            .padding((2.0f32, 0.0f32))
            .children((0..24).map(|_| {
                rect()
                    .width(Size::px(RAIL_WIDTH))
                    .height(Size::px(RAIL_WIDTH))
                    .corner_radius(RAIL_WIDTH / 2.0)
                    .background(QUIET)
                    .into_element()
            }))
            .into_element(),
    }
}

/// The rail column of a row, stretched to the row's height: `segments` stacked top to bottom,
/// then `dot` (absolutely placed) over them.
///
/// Everything in it is absolutely positioned, so it adds nothing to the row's measured height:
/// a `fill` rail measured with the row would claim all the height there is.
fn rail_stack(segments: Vec<Element>, dot: Option<Element>) -> Element {
    rect()
        .width(Size::px(RAIL_COLUMN))
        .height(Size::fill_minimum())
        .child(
            rect()
                .position(Position::new_absolute().top(0.0).left(0.0))
                .width(Size::fill())
                .height(Size::fill())
                .vertical()
                .cross_align(Alignment::Center)
                .children(segments),
        )
        .maybe_child(dot)
        .into_element()
}

/// The rail column of a row, one style top to bottom.
fn rail_column(rail: Rail) -> Element {
    rail_stack(vec![rail_segment(rail, Size::fill())], None)
}

/// A round marker on the rail, centred `center_y` below the row's top.
fn rail_dot(center_y: f32, size: f32, fill: Color, ring: Color, ring_width: f32) -> Element {
    rect()
        .position(
            Position::new_absolute()
                .top(center_y - size / 2.0)
                .left((RAIL_COLUMN - size) / 2.0),
        )
        .width(Size::px(size))
        .height(Size::px(size))
        .corner_radius(size / 2.0)
        .background(fill)
        .border(
            Border::new()
                .fill(ring)
                .width(ring_width)
                .alignment(BorderAlignment::Inner),
        )
        .into_element()
}

/// A place the trip stops at: its dot over the rail (`above` leading in, `below` leading out),
/// the time, and the place's name.
fn node_row(time: NodeTime, name: &str, above: Rail, below: Rail, dot: Dot) -> Element {
    let size = match dot {
        Dot::Start | Dot::End => 16.0,
        Dot::Stop(_) => 14.0,
    };
    let (fill, ring) = match dot {
        Dot::Start => (Color::WHITE, START_COLOR),
        Dot::End => (END_COLOR, END_COLOR),
        Dot::Stop(color) => (Color::WHITE, color),
    };
    let rail = rail_stack(
        vec![
            rail_segment(above, Size::px(NODE_DOT_Y)),
            rail_segment(below, Size::fill()),
        ],
        Some(rail_dot(NODE_DOT_Y, size, fill, ring, 3.0)),
    );

    let clock = rect()
        .vertical()
        .cross_align(Alignment::End)
        .padding((1.0f32, 0.0f32, 0.0f32, 0.0f32))
        .child(
            label()
                .text(clock_time(time.time))
                .font_size(text_size(13.0))
                .font_weight(FontWeight::BOLD)
                .color(TEXT),
        )
        .maybe_child(time.status.map(|(text, color)| {
            label()
                .text(text)
                .font_size(text_size(11.0))
                .color(color)
                .into_element()
        }))
        .into_element();

    timeline_row(
        Some(clock),
        rail,
        label()
            .text(name.to_string())
            .width(Size::fill())
            .padding((0.0f32, 0.0f32, 14.0f32, 8.0f32))
            .font_size(text_size(14.0))
            .font_weight(FontWeight::BOLD)
            .color(TEXT)
            .into_element(),
    )
}

/// A walk between two places: dotted rail, how long and how far.
fn walk_row(leg: &Leg) -> Element {
    let distance = leg
        .distance_m
        .filter(|meters| *meters >= 1.0)
        .map(|meters| {
            if meters < 1_000.0 {
                format!(" · {} m", (meters / 10.0).round() as i64 * 10)
            } else {
                format!(" · {:.1} km", meters / 1_000.0)
            }
        })
        .unwrap_or_default();
    timeline_row(
        None,
        rail_column(Rail::Walk),
        rect()
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(6.0)
            .padding((4.0f32, 0.0f32, 18.0f32, 8.0f32))
            .child(icon(lucide::footprints(), TEXT_MUTED, 15.0))
            .child(
                label()
                    .text(format!(
                        "Walk {}{distance}",
                        duration_text((leg.end - leg.start).max(60))
                    ))
                    .font_size(text_size(12.0))
                    .color(TEXT_MUTED),
            )
            .into_element(),
    )
}

/// A ride: its line and direction, who runs it, and its stops — folded to a count, unfolded
/// to every stop with its time.
fn ride_rows(index: usize, leg: &Leg, mut expanded: State<HashSet<usize>>) -> Vec<Element> {
    let color = rgb_or(leg.color, ACCENT);
    let open = expanded.read().contains(&index);
    let stops = leg.intermediate.len() + 1;
    let mode_word = match leg.mode {
        Mode::Bus => "bus",
        Mode::Tram => "tram",
        Mode::Subway => "metro",
        Mode::Rail => "train",
        Mode::Ferry => "boat",
        Mode::Walk | Mode::Other => "vehicle",
    };
    let operator = [leg.long_name.as_str(), leg.agency.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty() && *part != leg.line)
        .collect::<Vec<_>>()
        .join(" · ");

    let toggle = rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(4.0)
        .padding((tap_slack(), 0.0f32))
        .cursor(CursorIcon::Pointer)
        .on_press(move |_: Event<PressEventData>| {
            let mut set = expanded.write();
            if !set.remove(&index) {
                set.insert(index);
            }
        })
        .child(icon(
            if open {
                lucide::chevron_up()
            } else {
                lucide::chevron_down()
            },
            LINK,
            14.0,
        ))
        .child(
            label()
                .text(format!(
                    "{stops} stop{} · {} by {mode_word}",
                    if stops == 1 { "" } else { "s" },
                    duration_text(leg.end - leg.start)
                ))
                .font_size(text_size(12.0))
                .color(LINK),
        );

    let summary = rect()
        .vertical()
        .width(Size::fill())
        .spacing(4.0)
        .padding((2.0f32, 0.0f32, 12.0f32, 8.0f32))
        .child(
            rect()
                .horizontal()
                .content(Content::flex())
                .width(Size::fill())
                .cross_align(Alignment::Center)
                .spacing(8.0)
                .child(line_badge(leg.mode, &leg.line, leg.color, leg.text_color))
                .child(
                    label()
                        .text(format!("→ {}", leg.headsign))
                        .width(Size::flex(1.0))
                        .font_size(text_size(13.0))
                        .font_weight(FontWeight::BOLD)
                        .color(TEXT)
                        .max_lines(2)
                        .text_overflow(TextOverflow::Ellipsis),
                )
                .maybe_child(
                    (leg.wheelchair_accessible == Some(true))
                        .then(|| icon(lucide::accessibility(), TEXT_MUTED, 15.0).into_element()),
                ),
        )
        .maybe_child((!operator.is_empty()).then(|| {
            label()
                .text(operator)
                .font_size(text_size(11.0))
                .color(TEXT_FAINT)
                .max_lines(1)
                .text_overflow(TextOverflow::Ellipsis)
                .into_element()
        }))
        .child(toggle)
        .into_element();

    let mut rows = vec![timeline_row(None, rail_column(Rail::Ride(color)), summary)];
    if open {
        rows.extend(
            leg.intermediate
                .iter()
                .map(|stop| passed_stop_row(stop, color)),
        );
        // Leaves a gap above the stop it gets off at.
        rows.push(timeline_row(
            None,
            rail_column(Rail::Ride(color)),
            rect().height(Size::px(8.0)).into_element(),
        ));
    }
    rows
}

/// A stop a ride passes without the trip getting off: a small dot, its name and time.
fn passed_stop_row(stop: &super::journey::LegPlace, color: Color) -> Element {
    let rail = rail_stack(
        vec![rail_segment(Rail::Ride(color), Size::fill())],
        Some(rail_dot(11.0, 8.0, Color::WHITE, color, 2.0)),
    );
    let time = if stop.cancelled {
        label()
            .text("Cancelled")
            .font_size(text_size(11.0))
            .color(LATE)
    } else {
        label()
            .text(stop.time.map(clock_time).unwrap_or_default())
            .font_size(text_size(11.0))
            .color(TEXT_FAINT)
    };
    timeline_row(
        Some(
            rect()
                .padding((4.0f32, 0.0f32, 0.0f32, 0.0f32))
                .child(time)
                .into_element(),
        ),
        rail,
        label()
            .text(stop.name.clone())
            .width(Size::fill())
            .padding((3.0f32, 0.0f32, 6.0f32, 8.0f32))
            .font_size(text_size(12.0))
            .color(TEXT_MUTED)
            .max_lines(1)
            .text_overflow(TextOverflow::Ellipsis)
            .into_element(),
    )
}

/// Extra vertical padding around a tap target on touch screens.
fn tap_slack() -> f32 {
    if TOUCH { 6.0 } else { 2.0 }
}
