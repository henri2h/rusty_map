//! On screen: the Walk tab's route preview with its Start button ([`WalkRoute`]), and, while a
//! walk is guided, the instruction banner, the bottom bar and the re-centre chip
//! ([`NavigationOverlay`]).

use std::{
    hash::{Hash, Hasher},
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use freya::{icons::lucide, prelude::*};
use map_component::{OverlayPath, RouteOverlay};
use torin::{content::Content, position::Position, prelude::Direction};

use crate::{
    components::transit::{
        Endpoint, SOURCES_URL, clock_time, duration_text, show_on_map, status_line,
    },
    location::{self, Reading},
    mobile::{Haptic, SheetScroll, TOUCH, haptic, is_back, tap_padding, text_size, use_screen},
};

use super::{
    NavigationController, ROUTE_COLOR,
    maneuvers::{ManeuverKind, distance_text, instruction_text, maneuver_icon},
    profile_view::ElevationProfile,
    route::{IGN_SOURCE_URL, RouteSource, WalkRoute as Route, fetch_walk_route},
};

const TEXT: Color = Color::from_rgb(32, 33, 36);
const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const TEXT_FAINT: Color = Color::from_rgb(140, 144, 150);
const DIVIDER: Color = Color::from_rgb(232, 234, 237);
const BANNER: Color = Color::from_rgb(24, 128, 56);
const BANNER_OFF: Color = Color::from_rgb(95, 99, 104);
const LINK: Color = Color::from_rgb(26, 115, 232);

fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

fn icon(svg: bytes::Bytes, color: Color, size: f32) -> SvgViewer {
    SvgViewer::new(svg)
        .width(Size::px(size))
        .height(Size::px(size))
        .color(color)
}

// ─── Route preview (Directions, Walk tab) ────────────────────────────────────

#[derive(Clone, PartialEq)]
enum Load {
    Loading,
    Ready(Route),
    Failed,
}

/// The walk from `from` to `to`: on the map, its time and length, its instructions, Start.
#[derive(PartialEq)]
pub struct WalkRoute {
    pub from: Endpoint,
    pub to: Endpoint,
}

impl Component for WalkRoute {
    fn render(&self) -> impl IntoElement {
        let scroll = use_consume::<SheetScroll>();
        let mut nav = use_consume::<NavigationController>();
        let state = use_state(|| Load::Loading);

        use_hook({
            let (from, to) = (self.from.clone(), self.to.clone());
            let mut state = state;
            let mut map = nav.map.clone();
            move || {
                spawn(async move {
                    match fetch_walk_route((from.lat, from.lon), (to.lat, to.lon)).await {
                        Ok(route) => {
                            show_on_map(&mut map, preview_overlay(&route));
                            state.set(Load::Ready(route));
                        }
                        Err(err) => {
                            log::warn!("[navigation] walk route failed: {err:?}");
                            state.set(Load::Failed);
                        }
                    }
                    map.platform.send(UserEvent::RequestRedraw);
                });
            }
        });

        let body: Vec<Element> = match &*state.read() {
            Load::Loading => vec![status_line(
                CircularLoader::new().size(16.0).into_element(),
                "Finding a walking route…",
            )],
            Load::Failed => vec![status_line(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "Couldn't find a walking route",
            )],
            Load::Ready(route) => {
                let mut body = vec![summary(route), {
                    let route = route.clone();
                    let destination = self.to.clone();
                    Button::new()
                        .filled()
                        .width(Size::fill())
                        .on_press(move |_: Event<PressEventData>| {
                            nav.start(route.clone(), destination.clone())
                        })
                        .child(
                            rect()
                                .horizontal()
                                .cross_align(Alignment::Center)
                                .spacing(8.0)
                                .padding((tap_padding(4.0), 0.0))
                                .child(icon(lucide::navigation(), Color::WHITE, 16.0))
                                .child(
                                    label()
                                        .text("Start")
                                        .font_size(text_size(14.0))
                                        .font_weight(FontWeight::BOLD)
                                        .color(Color::WHITE),
                                ),
                        )
                        .into_element()
                }];
                body.push(
                    ElevationProfile {
                        points: Rc::new(route.points.clone()),
                        length_m: route.distance_m,
                    }
                    .into_element(),
                );
                body.extend(route.maneuvers.iter().map(|maneuver| {
                    step_row(
                        maneuver.kind,
                        &instruction_text(maneuver, &self.to.name),
                        maneuver.distance_m,
                    )
                }));
                body
            }
        };

        scroll
            .track(ScrollView::new())
            .height(Size::fill())
            .direction(Direction::vertical())
            .margin((12.0f32, 0.0f32, 0.0f32, 0.0f32))
            .child(scroll.marker())
            .children(body)
            .child({
                // The router that answered, credited.
                let (text, url) = match &*state.read() {
                    Load::Ready(route) if route.source == RouteSource::Ign => {
                        ("Route: IGN Géoplateforme · elevation: IGN", IGN_SOURCE_URL)
                    }
                    _ => (
                        "Route: Transitous · data sources · elevation: IGN",
                        SOURCES_URL,
                    ),
                };
                rect()
                    .cursor(CursorIcon::Pointer)
                    .margin((8.0f32, 0.0f32, 16.0f32, 0.0f32))
                    .on_press(move |_: Event<PressEventData>| Platform::get().open_url(url))
                    .child(label().text(text).font_size(text_size(11.0)).color(LINK))
            })
    }

    fn render_key(&self) -> DiffKey {
        let mut hasher = std::hash::DefaultHasher::new();
        for endpoint in [&self.from, &self.to] {
            endpoint.name.hash(&mut hasher);
            ((endpoint.lat * 1e6) as i64).hash(&mut hasher);
            ((endpoint.lon * 1e6) as i64).hash(&mut hasher);
        }
        "walk".hash(&mut hasher);
        DiffKey::U64(hasher.finish())
    }
}

fn preview_overlay(route: &Route) -> RouteOverlay {
    RouteOverlay {
        paths: vec![OverlayPath::from_lonlat(
            &route.points,
            ROUTE_COLOR,
            5.0,
            false,
        )],
        stops: Vec::new(),
    }
}

/// "38 min · 3.8 km · arrive 14:32".
fn summary(route: &Route) -> Element {
    let arrive = now_s() as i64 + route.duration_s as i64;
    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(8.0)
        .margin((0.0f32, 0.0f32, 10.0f32, 0.0f32))
        .child(icon(lucide::footprints(), TEXT_MUTED, 18.0))
        .child(
            label()
                .text(duration_text(route.duration_s as i64))
                .font_size(text_size(16.0))
                .font_weight(FontWeight::BOLD)
                .color(TEXT),
        )
        .child(
            label()
                .text(format!(
                    "· {} · arrive {}",
                    distance_text(route.distance_m),
                    clock_time(arrive)
                ))
                .font_size(text_size(13.0))
                .color(TEXT_MUTED),
        )
        .into_element()
}

fn step_row(kind: ManeuverKind, text: &str, distance_m: f64) -> Element {
    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .cross_align(Alignment::Center)
        .spacing(12.0)
        .padding((tap_padding(8.0), 0.0))
        .border(
            Border::new()
                .fill(DIVIDER)
                .width(1.0)
                .alignment(BorderAlignment::Inner),
        )
        .child(icon(maneuver_icon(kind), TEXT_MUTED, 18.0))
        .child(
            label()
                .text(text.to_string())
                .width(Size::flex(1.0))
                .font_size(text_size(13.0))
                .color(TEXT),
        )
        .maybe_child((distance_m >= 1.0).then(|| {
            label()
                .text(distance_text(distance_m))
                .font_size(text_size(12.0))
                .color(TEXT_FAINT)
                .into_element()
        }))
        .into_element()
}

// ─── While guiding ───────────────────────────────────────────────────────────

// Three siblings rendered straight into `HomePage` (banner, bottom bar, re-centre chip), not
// one component wrapping them: a wrapping element would sit in the page's flow under the
// full-height map, taking the absolutely positioned parts off-screen.

/// Follows the location readings while a walk is guided (see `NavigationController::on_fix`),
/// and shows the next instruction across the top. Nothing while no walk is guided.
#[derive(PartialEq)]
pub struct NavigationBanner {}

impl Component for NavigationBanner {
    fn render(&self) -> impl IntoElement {
        // Hooks before the early return: their count must not change between renders.
        let screen = use_screen();
        let nav = use_consume::<NavigationController>();
        let reading = location::use_reading();
        use_side_effect({
            let mut nav = nav.clone();
            move || {
                if let Reading::Fix(fix) = *reading.read() {
                    let insets = screen.insets;
                    nav.on_fix(fix, now_s(), insets);
                }
            }
        });

        let session = nav.session.read().clone();
        let Some(walk) = session else {
            return rect().into_element();
        };

        // The next instruction, and how far it is.
        let (kind, text, distance) = if walk.arrived {
            (
                ManeuverKind::Arrive,
                format!("You've arrived at {}", walk.destination.name),
                None,
            )
        } else {
            let index = walk.update.map_or(0, |update| update.maneuver);
            let maneuver = &walk.route.maneuvers[index.min(walk.route.maneuvers.len() - 1)];
            let distance = walk
                .update
                .map_or(maneuver.distance_m, |update| update.to_next_m);
            (
                maneuver.kind,
                instruction_text(maneuver, &walk.destination.name),
                Some(distance),
            )
        };
        let off_route = walk
            .update
            .is_some_and(|update| update.status == super::session::Status::OffRoute);
        let (subtitle, background) = if walk.rerouting {
            (Some("Rerouting…"), BANNER_OFF)
        } else if off_route {
            (Some("Off route"), BANNER_OFF)
        } else if walk.update.is_none() && !walk.arrived {
            (Some("Waiting for your location…"), BANNER)
        } else {
            (None, BANNER)
        };

        let banner = rect()
            .position(Position::new_absolute().top(0.).left(0.))
            .width(Size::window_percent(100.))
            .layer(Layer::OverlayLevel(2))
            .padding((12.0 + screen.insets.top, 16.0, 14.0, 16.0))
            .corner_radius((0.0f32, 0.0f32, 16.0f32, 16.0f32))
            .background(background)
            .shadow((
                0.0f32,
                2.0f32,
                10.0f32,
                0.0f32,
                Color::from_argb(70, 0, 0, 0),
            ))
            .horizontal()
            .content(Content::flex())
            .cross_align(Alignment::Center)
            .spacing(14.0)
            .child(icon(maneuver_icon(kind), Color::WHITE, 36.0))
            .child(
                rect()
                    .vertical()
                    .width(Size::flex(1.0))
                    .spacing(2.0)
                    .maybe_child(distance.map(|metres| {
                        label()
                            .text(distance_text(metres))
                            .font_size(text_size(22.0))
                            .font_weight(FontWeight::BOLD)
                            .color(Color::WHITE)
                            .into_element()
                    }))
                    .child(
                        label()
                            .text(text)
                            .font_size(text_size(16.0))
                            .color(Color::WHITE)
                            .max_lines(2)
                            .text_overflow(TextOverflow::Ellipsis),
                    )
                    .maybe_child(subtitle.map(|subtitle| {
                        label()
                            .text(subtitle)
                            .font_size(text_size(12.0))
                            .color(Color::from_argb(220, 255, 255, 255))
                            .into_element()
                    })),
            );

        banner.into_element()
    }
}

/// Time and distance left, and End (Done on arrival); back ends the walk too.
#[derive(PartialEq)]
pub struct NavigationBar {}

impl Component for NavigationBar {
    fn render(&self) -> impl IntoElement {
        let screen = use_screen();
        let nav = use_consume::<NavigationController>();
        let session = nav.session.read().clone();
        let Some(walk) = session else {
            return rect().into_element();
        };
        let remaining_m = walk
            .update
            .map_or(walk.route.distance_m, |update| update.remaining_m);
        let remaining_s = walk
            .update
            .map_or(walk.route.duration_s, |update| update.remaining_s);
        let status = if walk.arrived {
            "Arrived".to_string()
        } else {
            format!(
                "{} · {} · arrive {}",
                duration_text(remaining_s as i64),
                distance_text(remaining_m),
                clock_time(now_s() as i64 + remaining_s as i64)
            )
        };
        let mut end = nav.clone();
        let mut back = nav.clone();
        let bottom = rect()
            .position(Position::new_absolute().bottom(0.).left(0.))
            .width(Size::window_percent(100.))
            .layer(Layer::OverlayLevel(2))
            .padding((14.0, 16.0, 14.0 + screen.insets.bottom, 16.0))
            .corner_radius((16.0f32, 16.0f32, 0.0f32, 0.0f32))
            .background(Color::WHITE)
            .shadow((
                0.0f32,
                -2.0f32,
                12.0f32,
                0.0f32,
                Color::from_argb(60, 0, 0, 0),
            ))
            .horizontal()
            .content(Content::flex())
            .cross_align(Alignment::Center)
            .spacing(12.0)
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if is_back(&e.key) {
                    back.stop();
                }
            })
            .child(
                label()
                    .text(status)
                    .width(Size::flex(1.0))
                    .font_size(text_size(15.0))
                    .font_weight(FontWeight::BOLD)
                    .color(TEXT),
            )
            .child(
                Button::new()
                    .filled()
                    .maybe(!TOUCH, |button| button.compact())
                    .on_press(move |_: Event<PressEventData>| {
                        haptic(Haptic::Press);
                        end.stop();
                    })
                    .child(
                        label()
                            .text(if walk.arrived { "Done" } else { "End" })
                            .font_size(text_size(14.0))
                            .font_weight(FontWeight::BOLD)
                            .color(Color::WHITE),
                    ),
            );

        bottom.into_element()
    }
}

/// Panned away while guided: a way back to following.
#[derive(PartialEq)]
pub struct RecenterChip {}

impl Component for RecenterChip {
    fn render(&self) -> impl IntoElement {
        let screen = use_screen();
        let mut nav = use_consume::<NavigationController>();
        let show = nav
            .session
            .read()
            .as_ref()
            .is_some_and(|walk| !walk.following && !walk.arrived);
        if !show {
            return rect().into_element();
        }
        rect()
            .position(
                Position::new_absolute()
                    .bottom(BOTTOM_BAR_CLEARANCE + screen.insets.bottom)
                    .left(12.),
            )
            .layer(Layer::OverlayLevel(2))
            .padding((tap_padding(8.0), 14.0))
            .corner_radius(20.0f32)
            .background(Color::WHITE)
            .shadow((
                0.0f32,
                1.0f32,
                6.0f32,
                0.0f32,
                Color::from_argb(70, 0, 0, 0),
            ))
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(6.0)
            .on_press(move |_: Event<PressEventData>| {
                haptic(Haptic::Press);
                nav.recenter();
            })
            .child(icon(lucide::locate_fixed(), LINK, 16.0))
            .child(
                label()
                    .text("Re-centre")
                    .font_size(text_size(13.0))
                    .color(LINK),
            )
            .into_element()
    }
}

/// The re-centre chip sits this far above the window's bottom edge, over the bottom bar.
const BOTTOM_BAR_CLEARANCE: f32 = 96.0;
