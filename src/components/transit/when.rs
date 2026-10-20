//! When a journey or a departure board is for: now, leaving at a chosen date and time, or
//! (journeys only) arriving by one. Times are entered in French local time, like every clock
//! time the transit views show (see `ui::clock_time`).

use std::time::{SystemTime, UNIX_EPOCH};

use crate::mobile::{TOUCH, tap_padding, text_size};
use freya::{icons::lucide, prelude::*};

use super::api::days_from_civil;
use super::ui::{
    ACCENT, DIVIDER, LATE, TEXT, TEXT_FAINT, TEXT_MUTED, central_european_offset, clock_time, icon,
};

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
        let when = self.when;
        let current = *when.read();
        // The date or time picker unfolded, if any.
        let mut open = use_state(Panel::default);

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
            let shown = *open.read();
            let toggle = move |panel: Panel| {
                move |_: Event<PressEventData>| {
                    let next = if *open.peek() == panel {
                        Panel::None
                    } else {
                        panel
                    };
                    open.set(next);
                }
            };
            column = column.child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(8.0)
                    .padding((0.0f32, 0.0f32, 0.0f32, indent))
                    .child(field_button(
                        lucide::calendar(),
                        day_label(time, now),
                        shown == Panel::Date,
                        toggle(Panel::Date),
                    ))
                    .child(field_button(
                        lucide::clock(),
                        clock_time(time),
                        shown == Panel::Time,
                        toggle(Panel::Time),
                    )),
            );
            // Picking only changes `when` once a day, or an hour and minute, is chosen: each
            // change is a new timetable request.
            match shown {
                Panel::Date => {
                    column = column.child(
                        rect()
                            .padding((0.0f32, 0.0f32, 0.0f32, indent))
                            .child(Calendar { when, open }),
                    )
                }
                Panel::Time => {
                    column = column.child(
                        rect()
                            .padding((0.0f32, 0.0f32, 0.0f32, indent))
                            .child(TimePicker { when, open }),
                    )
                }
                Panel::None => {}
            }
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

/// Which picker is unfolded under the date and time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Panel {
    #[default]
    None,
    Date,
    Time,
}

/// A rounded button showing a date or time, highlighted while its picker is open.
fn field_button(
    svg: Bytes,
    text: String,
    active: bool,
    on_press: impl FnMut(Event<PressEventData>) + 'static,
) -> Element {
    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(6.0)
        .padding((tap_padding(5.0), 10.0))
        .corner_radius(8.0f32)
        .border(
            Border::new()
                .fill(if active { ACCENT } else { DIVIDER })
                .width(1.0)
                .alignment(BorderAlignment::Inner),
        )
        .cursor(CursorIcon::Pointer)
        .on_press(on_press)
        .child(icon(svg, if active { ACCENT } else { TEXT_MUTED }, 14.0))
        .child(label().text(text).font_size(text_size(13.0)).color(TEXT))
        .into_element()
}

/// One cell of a picker grid: a day of the month, an hour or a minute.
fn grid_cell(
    text: String,
    selected: bool,
    muted: bool,
    on_press: impl FnMut(Event<PressEventData>) + 'static,
) -> Element {
    rect()
        .width(Size::flex(1.0))
        .padding((tap_padding(5.0), 0.0))
        .corner_radius(8.0f32)
        .center()
        .background(if selected { ACCENT } else { Color::TRANSPARENT })
        .cursor(CursorIcon::Pointer)
        .on_press(on_press)
        .child(
            label()
                .text(text)
                .font_size(text_size(12.0))
                .color(match (selected, muted) {
                    (true, _) => Color::WHITE,
                    (false, true) => TEXT_FAINT,
                    (false, false) => TEXT,
                }),
        )
        .into_element()
}

/// A row of `columns` equal cells, padded with blanks.
fn grid_row(mut cells: Vec<Element>, columns: usize) -> Element {
    while cells.len() < columns {
        cells.push(rect().width(Size::flex(1.0)).into_element());
    }
    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .spacing(2.0)
        .children(cells)
        .into_element()
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Days in `month` (1-12) of `year`.
fn month_length(year: i64, month: i64) -> i64 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    days_from_civil(next_year, next_month, 1) - days_from_civil(year, month, 1)
}

/// A month of days, Monday first; a tap moves `when` to that day at the same time of day. Paging
/// between months is local: nothing is fetched until a day is picked.
#[derive(PartialEq)]
struct Calendar {
    when: State<When>,
    open: State<Panel>,
}

impl Component for Calendar {
    fn render(&self) -> impl IntoElement {
        let mut when = self.when;
        let mut open = self.open;
        let time = when.peek().time().unwrap_or_else(next_round_time);
        let selected_day = local_day(time);
        let today = local_day(now_unix());
        let mut month = use_state(|| {
            let (year, month, _) = civil_from_days(selected_day);
            (year, month)
        });
        let (year, month_number) = *month.read();

        let page = move |delta: i64| {
            move |_: Event<PressEventData>| {
                let (year, month_number) = *month.peek();
                let index = year * 12 + (month_number - 1) + delta;
                month.set((index.div_euclid(12), index.rem_euclid(12) + 1));
            }
        };
        let header = rect()
            .horizontal()
            .content(Content::flex())
            .width(Size::fill())
            .cross_align(Alignment::Center)
            .child(
                Button::new()
                    .flat()
                    .maybe(!TOUCH, |button| button.compact())
                    .on_press(page(-1))
                    .child(icon(lucide::chevron_left(), TEXT_MUTED, 16.0)),
            )
            .child(
                label()
                    .text(format!(
                        "{} {year}",
                        MONTH_NAMES[(month_number - 1) as usize]
                    ))
                    .width(Size::flex(1.0))
                    .text_align(TextAlign::Center)
                    .font_size(text_size(13.0))
                    .font_weight(FontWeight::BOLD)
                    .color(TEXT),
            )
            .child(
                Button::new()
                    .flat()
                    .maybe(!TOUCH, |button| button.compact())
                    .on_press(page(1))
                    .child(icon(lucide::chevron_right(), TEXT_MUTED, 16.0)),
            );

        let weekdays = grid_row(
            ["M", "T", "W", "T", "F", "S", "S"]
                .into_iter()
                .map(|day| {
                    label()
                        .text(day)
                        .width(Size::flex(1.0))
                        .text_align(TextAlign::Center)
                        .font_size(text_size(11.0))
                        .color(TEXT_FAINT)
                        .into_element()
                })
                .collect(),
            7,
        );

        let first = days_from_civil(year, month_number, 1);
        // 1970-01-01 (day 0) was a Thursday: Monday-based index 3.
        let lead = (first + 3).rem_euclid(7);
        let mut cells: Vec<Element> = (0..lead)
            .map(|_| rect().width(Size::flex(1.0)).into_element())
            .collect();
        for date in 1..=month_length(year, month_number) {
            let day = first + date - 1;
            cells.push(grid_cell(
                date.to_string(),
                day == selected_day,
                day < today,
                move |_: Event<PressEventData>| {
                    let current = *when.peek();
                    if let Some(time) = current.time() {
                        let minutes =
                            (time + central_european_offset(time)).rem_euclid(86_400) / 60;
                        let updated = local_time(day, minutes / 60, minutes % 60);
                        if updated != time {
                            when.set(current.at(updated));
                        }
                    }
                    open.set(Panel::None);
                },
            ));
        }
        let mut rows = vec![header.into_element(), weekdays];
        let mut cells = cells.into_iter().peekable();
        while cells.peek().is_some() {
            rows.push(grid_row(cells.by_ref().take(7).collect(), 7));
        }

        rect()
            .vertical()
            .width(Size::fill())
            .max_width(Size::px(320.0))
            .spacing(2.0)
            .padding(6.0f32)
            .corner_radius(10.0f32)
            .border(
                Border::new()
                    .fill(DIVIDER)
                    .width(1.0)
                    .alignment(BorderAlignment::Inner),
            )
            .children(rows)
    }
}

/// Hours, then minutes in 5-minute steps; picking the minute sets `when` (same day) and closes.
/// A time can be typed too.
#[derive(PartialEq)]
struct TimePicker {
    when: State<When>,
    open: State<Panel>,
}

impl Component for TimePicker {
    fn render(&self) -> impl IntoElement {
        let mut when = self.when;
        let mut open = self.open;
        let time = when.peek().time().unwrap_or_else(next_round_time);
        let local = (time + central_european_offset(time)).rem_euclid(86_400) / 60;
        let mut hour = use_state(|| local / 60);
        let chosen_hour = *hour.read();

        let hours: Vec<Element> = (0..24)
            .map(|h| {
                grid_cell(
                    format!("{h:02}"),
                    h == chosen_hour,
                    false,
                    move |_: Event<PressEventData>| hour.set(h),
                )
            })
            .collect();
        let minutes: Vec<Element> = (0..60)
            .step_by(5)
            .map(|m| {
                grid_cell(
                    format!(":{m:02}"),
                    chosen_hour == local / 60 && m == local % 60,
                    false,
                    move |_: Event<PressEventData>| {
                        let current = *when.peek();
                        if let Some(time) = current.time() {
                            let updated = local_time(local_day(time), *hour.peek(), m);
                            if updated != time {
                                when.set(current.at(updated));
                            }
                        }
                        open.set(Panel::None);
                    },
                )
            })
            .collect();

        let caption = |text: &str| {
            label()
                .text(text.to_string())
                .font_size(text_size(11.0))
                .color(TEXT_FAINT)
                .into_element()
        };
        let mut rows = vec![caption("Hour")];
        let mut hours = hours.into_iter().peekable();
        while hours.peek().is_some() {
            rows.push(grid_row(hours.by_ref().take(8).collect(), 8));
        }
        rows.push(caption("Minute"));
        let mut minutes = minutes.into_iter().peekable();
        while minutes.peek().is_some() {
            rows.push(grid_row(minutes.by_ref().take(6).collect(), 6));
        }
        rows.push(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(8.0)
                .margin((4.0f32, 0.0f32, 0.0f32, 0.0f32))
                .child(caption("Or type"))
                .child(TimeField { when })
                .into_element(),
        );

        rect()
            .vertical()
            .width(Size::fill())
            .max_width(Size::px(320.0))
            .spacing(2.0)
            .padding(6.0f32)
            .corner_radius(10.0f32)
            .border(
                Border::new()
                    .fill(DIVIDER)
                    .width(1.0)
                    .alignment(BorderAlignment::Inner),
            )
            .children(rows)
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

    // A fresh text whenever the time changes from elsewhere (the pickers).
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
    fn month_lengths() {
        assert_eq!(month_length(2026, 2), 28);
        assert_eq!(month_length(2028, 2), 29);
        assert_eq!(month_length(2026, 12), 31);
        assert_eq!(month_length(2026, 4), 30);
    }

    #[test]
    fn labels_days() {
        let now = local_time(api::days_from_civil(2026, 10, 19), 12, 0);
        assert_eq!(day_label(now, now), "Today");
        assert_eq!(day_label(now + 86_400, now), "Tomorrow");
        assert_eq!(day_label(now + 3 * 86_400, now), "Thu 22 Oct");
    }
}
