//! Drawer views opened from transit interactions: the lines running on a tapped track
//! ([`TrackView`]) and every stop of one vehicle's run ([`TripView`]). Each fetches on mount and
//! draws itself on the map.

use freya::{icons::lucide, prelude::*};
use map_component::{OverlayPath, OverlayStop, RouteOverlay};

use crate::components::place::{humanize, section_key};

use super::{
    api::Departure,
    controller::TransitController,
    journey::{self, Leg},
    osm_lines::{self, RouteInfo, TrackInfo},
    ui::{
        ACCENT, LATE, LIVE, TEXT, TEXT_FAINT, TEXT_MUTED, clock_time, divider, icon, line_badge,
        rgb_or, section_title, show_on_map, status_line,
    },
};

#[derive(Clone, PartialEq)]
enum Load<T> {
    Loading,
    Ready(T),
    Failed,
}

fn loading_line(text: &str) -> Element {
    status_line(CircularLoader::new().size(16.0).into_element(), text)
}

fn failed_line(text: &str) -> Element {
    status_line(
        icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
        text,
    )
}

// ─── Track ─────────────────────────────────────────────────────────────────────

/// Lines using the tapped track, from its OSM route relations.
#[derive(PartialEq)]
pub struct TrackView {
    pub id: u64,
    pub way_id: Option<u64>,
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    pub kind: String,
}

impl Component for TrackView {
    fn render(&self) -> impl IntoElement {
        let transit = use_consume::<TransitController>();
        let state = use_state(|| Load::Loading);
        // The line drawn in full on the map, when the user picked one.
        let shown_route = use_state::<Option<u64>>(|| None);

        use_hook({
            let (way_id, lat, lon, radius_m) = (self.way_id, self.lat, self.lon, self.radius_m);
            let kind = self.kind.clone();
            let mut state = state;
            let mut map = transit.map.clone();
            move || {
                spawn(async move {
                    match osm_lines::track_near(lat, lon, radius_m, kind, way_id).await {
                        Ok(info) => {
                            let color = info
                                .routes
                                .first()
                                .and_then(|route| route.color)
                                .map_or(ACCENT, |c| rgb_or(Some(c), ACCENT));
                            map.set_route_overlay(Some(RouteOverlay {
                                paths: vec![OverlayPath::from_lonlat(
                                    &info.geometry,
                                    color,
                                    7.0,
                                    false,
                                )],
                                stops: Vec::new(),
                            }));
                            state.set(Load::Ready(info));
                        }
                        Err(err) => {
                            log::warn!("[transit] track lookup failed: {err:?}");
                            state.set(Load::Failed);
                        }
                    }
                    map.platform.send(UserEvent::RequestRedraw);
                });
            }
        });

        let kind_title = match self.kind.as_str() {
            "subway" => "Metro track".to_string(),
            "tram" => "Tram track".to_string(),
            "light_rail" => "Light rail track".to_string(),
            "rail" | "" => "Railway track".to_string(),
            other => format!("{} track", humanize(other)),
        };

        let mut body: Vec<Element> = Vec::new();
        match &*state.read() {
            Load::Loading => body.push(loading_line("Looking up lines…")),
            Load::Failed => body.push(failed_line("No line information for this track")),
            Load::Ready(info) => {
                body.push(track_header(info, &kind_title));
                if info.routes.is_empty() {
                    body.push(failed_line(
                        "No public transport line is mapped on this track",
                    ));
                } else {
                    body.push(section_title(lucide::route(), "Lines on this track"));
                    for (index, route) in info.routes.iter().enumerate() {
                        if index > 0 {
                            body.push(divider());
                        }
                        body.push(route_row(route, *shown_route.read(), &transit, shown_route));
                    }
                    body.push(
                        label()
                            .text("Tap a line to show its whole route. Data © OpenStreetMap contributors")
                            .font_size(11.0)
                            .color(TEXT_FAINT)
                            .margin((10.0f32, 0.0f32))
                            .into_element(),
                    );
                }
            }
        }

        rect().vertical().width(Size::fill()).children(body)
    }

    fn render_key(&self) -> DiffKey {
        section_key("track", self.id)
    }
}

fn track_header(info: &TrackInfo, kind_title: &str) -> Element {
    let title = info.name.clone().unwrap_or_else(|| kind_title.to_string());
    rect()
        .vertical()
        .width(Size::fill())
        .margin((0.0f32, 0.0f32, 4.0f32, 0.0f32))
        .child(
            label()
                .text(title)
                .font_size(18.0)
                .font_weight(FontWeight::BOLD)
                .color(TEXT)
                .width(Size::fill()),
        )
        .child(
            label()
                .text(match &info.operator {
                    Some(operator) => format!("{kind_title} · {operator}"),
                    None => kind_title.to_string(),
                })
                .font_size(12.0)
                .color(TEXT_MUTED),
        )
        .into_element()
}

fn route_row(
    route: &RouteInfo,
    shown: Option<u64>,
    transit: &TransitController,
    mut shown_route: State<Option<u64>>,
) -> Element {
    let details = [route.network.clone(), route.operator.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let ends = match (&route.from, &route.to) {
        (Some(from), Some(to)) => Some(format!("{from} ↔ {to}")),
        _ => None,
    };
    let selected = shown == Some(route.relation_id);
    let relation_id = route.relation_id;
    let color = rgb_or(route.color, ACCENT);
    let map = transit.map.clone();

    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .spacing(10.0)
        .padding((8.0f32, 4.0f32))
        .corner_radius(8.0f32)
        .maybe(selected, |r| r.background(Color::from_rgb(241, 244, 250)))
        .cursor(CursorIcon::Pointer)
        .on_press(move |_: Event<PressEventData>| {
            shown_route.set(Some(relation_id));
            let mut map = map.clone();
            spawn(async move {
                match osm_lines::route_shape(relation_id).await {
                    Ok(shape) => show_on_map(
                        &mut map,
                        RouteOverlay {
                            paths: shape
                                .ways
                                .iter()
                                .map(|way| OverlayPath::from_lonlat(way, color, 5.0, false))
                                .collect(),
                            stops: shape
                                .stops
                                .iter()
                                .map(|(_, lon, lat)| {
                                    OverlayStop::from_lonlat(*lon, *lat, color, false)
                                })
                                .collect(),
                        },
                    ),
                    Err(err) => log::warn!("[transit] route shape failed: {err:?}"),
                }
            });
        })
        .child(line_badge(route.mode, &route.line, route.color, None))
        .child(
            rect()
                .vertical()
                .width(Size::flex(1.0))
                .child(
                    label()
                        .text(route.name.clone())
                        .font_size(13.0)
                        .color(TEXT)
                        .width(Size::fill()),
                )
                .maybe_child(ends.map(|ends| {
                    label()
                        .text(ends)
                        .font_size(11.0)
                        .color(TEXT_MUTED)
                        .width(Size::fill())
                        .into_element()
                }))
                .maybe_child((!details.is_empty()).then(|| {
                    label()
                        .text(details)
                        .font_size(11.0)
                        .color(TEXT_FAINT)
                        .width(Size::fill())
                        .into_element()
                })),
        )
        .into_element()
}

// ─── Trip ──────────────────────────────────────────────────────────────────────

/// Every stop of one vehicle's run, opened from a departure row.
#[derive(PartialEq)]
pub struct TripView {
    pub id: u64,
    pub departure: Departure,
    pub from_stop: String,
}

impl Component for TripView {
    fn render(&self) -> impl IntoElement {
        let transit = use_consume::<TransitController>();
        let state = use_state(|| Load::<Leg>::Loading);

        use_hook({
            let trip_id = self.departure.trip_id.clone();
            let from_stop = self.from_stop.clone();
            let mut state = state;
            let mut map = transit.map.clone();
            move || {
                spawn(async move {
                    match journey::fetch_trip(trip_id).await {
                        Ok(leg) => {
                            show_on_map(&mut map, trip_overlay(&leg, &from_stop));
                            state.set(Load::Ready(leg));
                        }
                        Err(err) => {
                            log::warn!("[transit] trip failed: {err:?}");
                            state.set(Load::Failed);
                        }
                    }
                    map.platform.send(UserEvent::RequestRedraw);
                });
            }
        });

        let departure = &self.departure;
        let header = rect().vertical().width(Size::fill()).spacing(4.0).child(
            rect()
                .horizontal()
                .content(Content::flex())
                .width(Size::fill())
                .cross_align(Alignment::Center)
                .spacing(10.0)
                .child(line_badge(
                    departure.mode,
                    &departure.line,
                    departure.color,
                    departure.text_color,
                ))
                .child(
                    label()
                        .text(format!("→ {}", departure.headsign))
                        .width(Size::flex(1.0))
                        .font_size(16.0)
                        .font_weight(FontWeight::BOLD)
                        .color(TEXT),
                ),
        );

        let body: Vec<Element> = match &*state.read() {
            Load::Loading => vec![loading_line("Loading trip…")],
            Load::Failed => vec![failed_line("Couldn't load this trip")],
            Load::Ready(leg) => trip_body(leg, &self.from_stop),
        };

        rect()
            .vertical()
            .width(Size::fill())
            .child(header)
            .children(body)
    }

    fn render_key(&self) -> DiffKey {
        section_key("trip", self.id)
    }
}

fn trip_overlay(leg: &Leg, from_stop: &str) -> RouteOverlay {
    let color = rgb_or(leg.color, ACCENT);
    RouteOverlay {
        paths: vec![OverlayPath::from_lonlat(&leg.geometry, color, 5.0, false)],
        stops: all_stops(leg)
            .map(|stop| OverlayStop::from_lonlat(stop.lon, stop.lat, color, stop.name == from_stop))
            .collect(),
    }
}

fn all_stops(leg: &Leg) -> impl Iterator<Item = &journey::LegPlace> {
    std::iter::once(&leg.from)
        .chain(leg.intermediate.iter())
        .chain(std::iter::once(&leg.to))
}

fn trip_body(leg: &Leg, from_stop: &str) -> Vec<Element> {
    let mut facts = Vec::new();
    if !leg.long_name.is_empty() {
        facts.push(leg.long_name.clone());
    }
    if !leg.agency.is_empty() {
        facts.push(leg.agency.clone());
    }
    match leg.wheelchair_accessible {
        Some(true) => facts.push("Wheelchair accessible".into()),
        Some(false) => facts.push("Not wheelchair accessible".into()),
        None => {}
    }

    let mut body = vec![
        label()
            .text(facts.join(" · "))
            .font_size(12.0)
            .color(TEXT_MUTED)
            .width(Size::fill())
            .margin((4.0f32, 0.0f32, 0.0f32, 0.0f32))
            .into_element(),
    ];
    if leg.realtime {
        let (text, color) = match leg.delay_min {
            Some(delay) if delay > 0 => (format!("Live · {delay} min late"), LATE),
            _ => ("Live · on time".to_string(), LIVE),
        };
        body.push(
            label()
                .text(text)
                .font_size(12.0)
                .color(color)
                .into_element(),
        );
    }
    body.push(section_title(lucide::route(), "Stops"));

    let color = rgb_or(leg.color, ACCENT);
    let stops: Vec<&journey::LegPlace> = all_stops(leg).collect();
    let current = stops.iter().position(|stop| stop.name == from_stop);
    let count = stops.len();
    for (index, stop) in stops.into_iter().enumerate() {
        let passed = current.is_some_and(|current| index < current);
        let here = Some(index) == current;
        body.push(stop_row(
            stop,
            color,
            index == 0,
            index + 1 == count,
            passed,
            here,
        ));
    }
    body
}

/// One stop on a vertical timeline: a colored rail with a dot, the name, and the time.
fn stop_row(
    stop: &journey::LegPlace,
    color: Color,
    first: bool,
    last: bool,
    passed: bool,
    here: bool,
) -> Element {
    let rail_color = if passed {
        Color::from_rgb(200, 203, 208)
    } else {
        color
    };
    let text_color = if passed { TEXT_FAINT } else { TEXT };
    let dot = if here || first || last { 12.0 } else { 8.0 };

    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .spacing(10.0)
        .height(Size::px(28.0))
        .cross_align(Alignment::Center)
        .child(
            // Rail segment (hidden above the first stop and below the last) with the dot over it.
            rect()
                .width(Size::px(16.0))
                .height(Size::fill())
                .main_align(Alignment::Center)
                .cross_align(Alignment::Center)
                .child(
                    rect()
                        .width(Size::px(4.0))
                        .height(Size::fill())
                        .background(rail_color)
                        .margin((
                            if first { 14.0f32 } else { 0.0f32 },
                            0.0f32,
                            if last { 14.0f32 } else { 0.0f32 },
                            0.0f32,
                        )),
                )
                .child(
                    rect()
                        .position(
                            Position::new_absolute()
                                .top(14.0 - dot / 2.0)
                                .left(8.0 - dot / 2.0),
                        )
                        .width(Size::px(dot))
                        .height(Size::px(dot))
                        .corner_radius(dot / 2.0)
                        .background(Color::WHITE)
                        .border(
                            Border::new()
                                .fill(rail_color)
                                .width(if here { 3.0 } else { 2.0 })
                                .alignment(BorderAlignment::Inner),
                        ),
                ),
        )
        .child(
            label()
                .text(stop.name.clone())
                .width(Size::flex(1.0))
                .font_size(13.0)
                .font_weight(if here {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                })
                .color(text_color)
                .max_lines(1)
                .text_overflow(TextOverflow::Ellipsis),
        )
        .child(
            label()
                .text(if stop.cancelled {
                    "Cancelled".to_string()
                } else {
                    stop.time.map(clock_time).unwrap_or_default()
                })
                .font_size(12.0)
                .color(if stop.cancelled { LATE } else { TEXT_MUTED }),
        )
        .into_element()
}
