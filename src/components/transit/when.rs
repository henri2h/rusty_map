//! When a journey or a departure board is for: now, leaving at a chosen date and time, or
//! (journeys only) arriving by one. Times are entered in French local time, like every clock
//! time the transit views show (see `ui::clock_time`).

use std::time::{SystemTime, UNIX_EPOCH};

use crate::mobile::{TOUCH, text_size};
use freya::{icons::lucide, prelude::*};

use super::ui::{
    ACCENT, DIVIDER, LATE, TEXT, TEXT_MUTED, central_european_offset, clock_time, icon,
};

/// Step of the − / + time buttons.
const STEP_S: i64 = 15 * 60;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum When {
    #[default]
    Now,
    /// Leaving at this Unix time.
    Depart(i64),
    /// Arriving by this Unix time.
    Arrive(i64),
}

impl When {
    pub fn time(self) -> Option<i64> {
        match self {
            When::Now => None,
            When::Depart(time) | When::Arrive(time) => Some(time),
        }
    }

    /// The same kind of choice at another time.
    fn at(self, time: i64) -> Self {
        match self {
            When::Arrive(_) => When::Arrive(time),
            _ => When::Depart(time),
        }
    }

    /// Transitous' (MOTIS) `time`/`arriveBy` query parameters; none for [`When::Now`].
    pub fn query(self) -> Vec<(&'static str, String)> {
        match self {
            When::Now => Vec::new(),
            When::Depart(time) => vec![("time", iso_utc(time))],
            When::Arrive(time) => vec![("time", iso_utc(time)), ("arriveBy", "true".into())],
        }
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// `2026-10-20T06:30:00Z`.
fn iso_utc(unix: i64) -> String {
    let (year, month, day) = civil_from_days(unix.div_euclid(86_400));
    let seconds = unix.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60
    )
}

/// Inverse of `api::days_from_civil` (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Days since 1970-01-01 of `unix`'s local (French) date.
fn local_day(unix: i64) -> i64 {
    (unix + central_european_offset(unix)).div_euclid(86_400)
}

/// The Unix time of `hour:minute`, French local time, on local day `day`.
fn local_time(day: i64, hour: i64, minute: i64) -> i64 {
    let local = day * 86_400 + hour * 3_600 + minute * 60;
    // The offset in force at that moment; off by an hour only inside the DST switch itself.
    local - central_european_offset(local - 3_600)
}

/// `"Today"`, `"Tomorrow"`, or `"Mon 20 Oct"`.
fn day_label(unix: i64, now: i64) -> String {
    let day = local_day(unix);
    match day - local_day(now) {
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        -1 => "Yesterday".to_string(),
        _ => {
            const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let (year, month, date) = civil_from_days(day);
            let weekday = WEEKDAYS[(day + 4).rem_euclid(7) as usize];
            let month = MONTHS[(month - 1) as usize];
            if year == civil_from_days(local_day(now)).0 {
                format!("{weekday} {date} {month}")
            } else {
                format!("{weekday} {date} {month} {year}")
            }
        }
    }
}

/// `"8:30"`, `"08h30"`, `"0830"`, `"8"` → `(8, 30)`.
fn parse_clock(text: &str) -> Option<(i64, i64)> {
    let digits: Vec<&str> = text
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .collect();
    let (hour, minute) = match digits.as_slice() {
        [hour, minute] => (hour.parse().ok()?, minute.parse().ok()?),
        [both] if both.len() > 2 => {
            let (hour, minute) = both.split_at(both.len() - 2);
            (hour.parse().ok()?, minute.parse().ok()?)
        }
        [hour] => (hour.parse().ok()?, 0),
        _ => return None,
    };
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// Now, rounded up to the next 5 minutes: the starting point when leaving "Now".
fn next_round_time() -> i64 {
    let now = now_unix();
    now + (300 - now.rem_euclid(300)) % 300
}

/// "Now / Depart at / Arrive by" choice, plus the date and time when not now.
#[derive(PartialEq)]
pub struct WhenPicker {
    pub when: State<When>,
    /// Offer "Arrive by" (journeys; a departure board only leaves).
    pub allow_arrive: bool,
    /// A clock icon before the choices, where no section title already shows one.
    pub icon: bool,
}

impl Component for WhenPicker {
    fn render(&self) -> impl IntoElement {
        let mut when = self.when;
        let current = *when.read();

        let chip = move |text: &'static str, selected: bool, next: When| {
            let mut when = when;
            rect()
                .padding((4.0f32, 10.0f32))
                .corner_radius(12.0f32)
                .background(if selected { ACCENT } else { Color::TRANSPARENT })
                .border(
                    Border::new()
                        .fill(if selected { ACCENT } else { DIVIDER })
                        .width(1.0)
                        .alignment(BorderAlignment::Inner),
                )
                .cursor(CursorIcon::Pointer)
                .on_press(move |_: Event<PressEventData>| {
                    if *when.peek() != next {
                        when.set(next);
                    }
                })
                .child(
                    label()
                        .text(text)
                        .font_size(text_size(12.0))
                        .color(if selected { Color::WHITE } else { TEXT_MUTED }),
                )
                .into_element()
        };
        let time = current.time().unwrap_or_else(next_round_time);
        let mut chips = vec![
            chip("Now", current == When::Now, When::Now),
            chip(
                "Depart at",
                matches!(current, When::Depart(_)),
                When::Depart(time),
            ),
        ];
        if self.allow_arrive {
            chips.push(chip(
                "Arrive by",
                matches!(current, When::Arrive(_)),
                When::Arrive(time),
            ));
        }

        let mut column = rect().vertical().width(Size::fill()).spacing(6.0).child(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(6.0)
                .maybe_child(
                    self.icon
                        .then(|| icon(lucide::clock(), TEXT_MUTED, 15.0).into_element()),
                )
                .children(chips),
        );

        if let Some(time) = current.time() {
            let now = now_unix();
            // Lines the date up with the first choice, past the clock icon.
            let indent = if self.icon { 21.0f32 } else { 4.0 };
            let step = move |delta: i64, svg: Bytes| {
                Button::new()
                    .flat()
                    .maybe(!TOUCH, |button| button.compact())
                    .on_press(move |_: Event<PressEventData>| {
                        let current = *when.peek();
                        if let Some(time) = current.time() {
                            when.set(current.at(time + delta));
                        }
                    })
                    .child(icon(svg, TEXT_MUTED, 16.0))
                    .into_element()
            };
            column = column.child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(2.0)
                    .padding((0.0f32, 0.0f32, 0.0f32, indent - 4.0))
                    .child(step(-86_400, lucide::chevron_left()))
                    .child(
                        label()
                            .text(day_label(time, now))
                            .width(Size::px(92.0))
                            .text_align(TextAlign::Center)
                            .font_size(text_size(13.0))
                            .color(TEXT),
                    )
                    .child(step(86_400, lucide::chevron_right()))
                    .child(rect().width(Size::px(12.0)))
                    .child(step(-STEP_S, lucide::minus()))
                    .child(TimeField { when })
                    .child(step(STEP_S, lucide::plus())),
            );
            if time < now - 60 {
                column = column.child(
                    label()
                        .text("This time is in the past")
                        .font_size(text_size(11.0))
                        .color(LATE)
                        .margin((0.0f32, 0.0f32, 0.0f32, indent)),
                );
            }
        }
        column
    }
}

/// The time, editable: typing `8:30` + Enter sets it (on the same day).
#[derive(PartialEq)]
struct TimeField {
    when: State<When>,
}

impl Component for TimeField {
    fn render(&self) -> impl IntoElement {
        let mut when = self.when;
        let time = when.read().time().unwrap_or_default();
        let text = use_state(|| clock_time(time));

        rect()
            .width(Size::px(64.0))
            .child(
                Input::new(text)
                    .width(Size::fill())
                    .on_submit(move |typed: String| {
                        let current = *when.peek();
                        let Some(time) = current.time() else {
                            return;
                        };
                        let mut text = text;
                        match parse_clock(&typed) {
                            Some((hour, minute)) => {
                                let updated = local_time(local_day(time), hour, minute);
                                when.set(current.at(updated));
                                text.set(clock_time(updated));
                            }
                            None => text.set(clock_time(time)),
                        }
                    }),
            )
    }

    // A fresh text whenever the time changes from elsewhere (− / + / day buttons).
    fn render_key(&self) -> DiffKey {
        DiffKey::U64(self.when.peek().time().unwrap_or_default() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::transit::api;

    #[test]
    fn formats_utc_for_motis() {
        let unix = api::days_from_civil(2026, 10, 20) * 86_400 + 6 * 3_600 + 30 * 60;
        assert_eq!(iso_utc(unix), "2026-10-20T06:30:00Z");
        assert_eq!(
            When::Arrive(unix).query()[1],
            ("arriveBy", "true".to_string())
        );
        assert!(When::Now.query().is_empty());
    }

    #[test]
    fn civil_round_trip() {
        for days in [-1, 0, 59, 10_957, 20_746, 20_747, 30_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(api::days_from_civil(year, month, day), days);
        }
    }

    #[test]
    fn local_time_is_french() {
        // 2026-07-01 08:30 CEST = 06:30 UTC; 2026-12-01 08:30 CET = 07:30 UTC.
        let summer = local_time(api::days_from_civil(2026, 7, 1), 8, 30);
        assert_eq!(iso_utc(summer), "2026-07-01T06:30:00Z");
        let winter = local_time(api::days_from_civil(2026, 12, 1), 8, 30);
        assert_eq!(iso_utc(winter), "2026-12-01T07:30:00Z");
        assert_eq!(local_day(summer), api::days_from_civil(2026, 7, 1));
    }

    #[test]
    fn parses_typed_times() {
        assert_eq!(parse_clock("8:30"), Some((8, 30)));
        assert_eq!(parse_clock("08h05"), Some((8, 5)));
        assert_eq!(parse_clock("1745"), Some((17, 45)));
        assert_eq!(parse_clock("9"), Some((9, 0)));
        assert_eq!(parse_clock("25:00"), None);
        assert_eq!(parse_clock("soon"), None);
    }

    #[test]
    fn labels_days() {
        let now = local_time(api::days_from_civil(2026, 10, 19), 12, 0);
        assert_eq!(day_label(now, now), "Today");
        assert_eq!(day_label(now + 86_400, now), "Tomorrow");
        assert_eq!(day_label(now + 3 * 86_400, now), "Thu 22 Oct");
    }
}
