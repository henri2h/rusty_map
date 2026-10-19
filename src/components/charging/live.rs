//! Drawer section for a tapped fast charger: each charge point's live state and price, from
//! QualiCharge's per-station endpoint, refreshed while shown.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use freya::{icons::lucide, prelude::*};
use map_component::util::tiles::background_delay;

use super::{
    BUSY, DOWN, FREE, UNKNOWN,
    api::{self, ChargePoint, PointState, StationDetail},
};
use crate::components::{place::section_key, transit::days_from_civil};

const REFRESH_EVERY: Duration = Duration::from_secs(30);

const TEXT: Color = Color::from_rgb(32, 33, 36);
const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const TEXT_FAINT: Color = Color::from_rgb(140, 144, 150);
const DIVIDER: Color = Color::from_rgb(232, 234, 237);
const LINK: Color = Color::from_rgb(26, 115, 232);

#[derive(Clone, PartialEq)]
enum Load {
    Loading,
    Ready(StationDetail),
    Unknown,
    Failed,
}

/// Live charge points of station `station_id` (`id_station_itinerance`). Keyed by the selected
/// place, so another station starts over.
#[derive(PartialEq)]
pub struct ChargerLive {
    pub place_id: u64,
    pub station_id: String,
}

impl Component for ChargerLive {
    fn render(&self) -> impl IntoElement {
        let state = use_state(|| Load::Loading);
        let platform = use_hook(|| Platform::get().current_window());

        use_hook({
            let id = self.station_id.clone();
            let mut state = state;
            move || {
                spawn(async move {
                    loop {
                        let next = match api::fetch_station(id.clone()).await {
                            Ok(Some(detail)) => Load::Ready(detail),
                            Ok(None) => Load::Unknown,
                            Err(err) => {
                                log::warn!("[chargers] station {id} failed: {err:?}");
                                // Keep the last good state through a transient failure.
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
                        if matches!(*state.peek(), Load::Unknown) {
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
        let mut body: Vec<Element> = Vec::new();
        match &*state.read() {
            Load::Loading => body.push(status(
                CircularLoader::new().size(16.0).into_element(),
                "Loading live availability…",
            )),
            Load::Unknown => body.push(status(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "No live details for this station",
            )),
            Load::Failed => body.push(status(
                icon(lucide::circle_alert(), TEXT_FAINT, 16.0).into_element(),
                "Couldn't load live availability",
            )),
            Load::Ready(detail) => {
                let free = detail
                    .points
                    .iter()
                    .filter(|point| point.state == PointState::Free)
                    .count();
                body.push(
                    label()
                        .text(format!("{free} of {} free", detail.points.len()))
                        .font_size(15.0)
                        .font_weight(FontWeight::BOLD)
                        .color(if free > 0 { FREE } else { BUSY })
                        .margin((0.0f32, 0.0f32, 4.0f32, 0.0f32))
                        .into_element(),
                );
                for (index, point) in detail.points.iter().enumerate() {
                    if index > 0 {
                        body.push(
                            rect()
                                .width(Size::fill())
                                .height(Size::px(1.0))
                                .background(DIVIDER)
                                .into_element(),
                        );
                    }
                    body.push(point_row(point, now));
                }
                // Address, opening hours, operator phone.
                for value in [&detail.address, &detail.hours, &detail.phone]
                    .into_iter()
                    .flatten()
                {
                    body.push(
                        label()
                            .text(value.clone())
                            .font_size(12.0)
                            .color(TEXT_MUTED)
                            .width(Size::fill())
                            .margin((4.0f32, 0.0f32, 0.0f32, 0.0f32))
                            .into_element(),
                    );
                }
            }
        }

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
                    .cross_align(Alignment::Center)
                    .spacing(8.0)
                    .margin((0.0f32, 0.0f32, 6.0f32, 0.0f32))
                    .child(icon(lucide::zap(), TEXT_MUTED, 16.0))
                    .child(
                        label()
                            .text("Live availability")
                            .font_size(13.0)
                            .font_weight(FontWeight::BOLD)
                            .color(TEXT),
                    ),
            )
            .children(body)
            .child(
                rect()
                    .cursor(CursorIcon::Pointer)
                    .margin((8.0f32, 0.0f32, 0.0f32, 0.0f32))
                    .on_press(|_: Event<PressEventData>| Platform::get().open_url(api::SOURCE_URL))
                    .child(
                        label()
                            .text("Live data: QualiCharge")
                            .font_size(11.0)
                            .color(LINK),
                    ),
            )
    }

    fn render_key(&self) -> DiffKey {
        section_key("charger", self.place_id)
    }
}

/// One charge point: power and plugs, price, and its state with how old that is.
fn point_row(point: &ChargePoint, now: i64) -> Element {
    let (state, color) = match point.state {
        PointState::Free => ("Free", FREE),
        PointState::Occupied => ("Occupied", BUSY),
        PointState::Reserved => ("Reserved", BUSY),
        PointState::OutOfService => ("Out of service", DOWN),
        PointState::Unknown => ("Unknown", UNKNOWN),
    };
    let power = point
        .kw
        .map(|kw| format!("{kw:.0} kW"))
        .unwrap_or_else(|| "? kW".to_string());
    let plugs = if point.plugs.is_empty() {
        String::new()
    } else {
        format!(" · {}", point.plugs.join(", "))
    };
    let age = point
        .reported_at
        .as_deref()
        .and_then(parse_timestamp)
        .map(|at| age_text(now - at));

    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .cross_align(Alignment::Center)
        .spacing(10.0)
        .padding((7.0f32, 0.0f32))
        .child(
            rect()
                .vertical()
                .width(Size::flex(1.0))
                .child(
                    label()
                        .text(format!("{power}{plugs}"))
                        .font_size(13.0)
                        .color(TEXT),
                )
                .maybe_child(point.price.clone().map(|price| {
                    label()
                        .text(price)
                        .font_size(11.0)
                        .color(TEXT_MUTED)
                        .into_element()
                })),
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
                        .child(
                            rect()
                                .width(Size::px(8.0))
                                .height(Size::px(8.0))
                                .corner_radius(4.0f32)
                                .background(color),
                        )
                        .child(
                            label()
                                .text(state)
                                .font_size(13.0)
                                .font_weight(FontWeight::BOLD)
                                .color(color),
                        ),
                )
                .maybe_child(age.map(|age| {
                    label()
                        .text(age)
                        .font_size(10.0)
                        .color(TEXT_FAINT)
                        .into_element()
                })),
        )
        .into_element()
}

/// `"just now"`, `"5 min ago"`, `"3 h ago"`, `"12 days ago"`: how old a reported state is,
/// since operators report on changes and a stale state is worth knowing.
fn age_text(seconds: i64) -> String {
    let minutes = seconds.max(0) / 60;
    if minutes < 1 {
        "just now".to_string()
    } else if minutes < 60 {
        format!("{minutes} min ago")
    } else if minutes < 48 * 60 {
        format!("{} h ago", minutes / 60)
    } else {
        format!("{} days ago", minutes / (24 * 60))
    }
}

/// `2026-09-24 09:18:45+00:00`, `2026-10-19T18:32:40.249247+00:00` or `…Z` → Unix seconds.
fn parse_timestamp(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = text.split_at_checked(10)?;
    let mut parts = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    let rest = rest.strip_prefix(['T', ' '])?;
    let (clock, offset) = match rest.find(['+', '-', 'Z']) {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second = clock
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as i64;
    let offset_s = match offset.split_at_checked(1) {
        Some((sign @ ("+" | "-"), hm)) => {
            let mut hm = hm.split(':');
            let h: i64 = hm.next()?.parse().ok()?;
            let m: i64 = hm.next().and_then(|m| m.parse().ok()).unwrap_or(0);
            let total = h * 3_600 + m * 60;
            if sign == "+" { total } else { -total }
        }
        _ => 0,
    };
    Some(
        days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second - offset_s,
    )
}

fn status(leading: Element, text: &str) -> Element {
    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(8.0)
        .padding((6.0f32, 0.0f32))
        .child(leading)
        .child(
            label()
                .text(text.to_string())
                .font_size(12.0)
                .color(TEXT_MUTED),
        )
        .into_element()
}

fn icon(svg: Bytes, color: Color, size: f32) -> SvgViewer {
    SvgViewer::new(svg)
        .width(Size::px(size))
        .height(Size::px(size))
        .color(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_report_times() {
        let utc = parse_timestamp("2026-09-24 09:18:45+00:00").unwrap();
        assert_eq!(parse_timestamp("2026-09-24T11:18:45.5+02:00"), Some(utc));
        assert_eq!(parse_timestamp("2026-09-24T09:18:45Z"), Some(utc));
        assert_eq!(utc % 86_400, 9 * 3_600 + 18 * 60 + 45);
    }

    #[test]
    fn ages() {
        assert_eq!(age_text(20), "just now");
        assert_eq!(age_text(5 * 60), "5 min ago");
        assert_eq!(age_text(3 * 3_600), "3 h ago");
        assert_eq!(age_text(12 * 86_400), "12 days ago");
    }
}
