//! A small parser for the common subset of OSM's [`opening_hours`
//! syntax](https://wiki.openstreetmap.org/wiki/Key:opening_hours): semicolon-separated rules of
//! `<day selector> <time ranges>` (e.g. `"Mo-Fr 09:00-18:00; Sa 10:00-13:00; Su off"`). Not the
//! full grammar — no holidays, no comments, no month/date selectors, no nested exceptions — a
//! rule outside this subset is shown as its own raw text under an "Hours" heading rather than
//! parsed wrong.

/// One parsed rule, ready for a two-column day/hours row.
pub struct OpeningRule {
    pub days: String,
    pub hours: String,
}

const DAY_NAMES: &[(&str, &str)] = &[
    ("Mo", "Mon"),
    ("Tu", "Tue"),
    ("We", "Wed"),
    ("Th", "Thu"),
    ("Fr", "Fri"),
    ("Sa", "Sat"),
    ("Su", "Sun"),
];

fn day_name(abbr: &str) -> Option<&'static str> {
    if abbr == "PH" {
        return Some("Holidays");
    }
    DAY_NAMES.iter().find(|(a, _)| *a == abbr).map(|(_, n)| *n)
}

/// `true` for a time-range list such as `09:00-12:00,14:00-18:00`.
fn is_time_ranges(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && text.contains(':')
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ':' | '-' | ',' | '+' | ' '))
}

/// `09:00-12:00,14:00-18:00` → `09:00–12:00, 14:00–18:00`.
fn format_time_ranges(text: &str) -> String {
    text.split(',')
        .map(|range| range.trim().replace('-', "–"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_hours(text: &str) -> String {
    if text.eq_ignore_ascii_case("off") || text.eq_ignore_ascii_case("closed") {
        "Closed".to_string()
    } else if is_time_ranges(text) {
        format_time_ranges(text)
    } else {
        text.to_string()
    }
}

/// Parses one day token — a range (`"Mo-Fr"`) or a single day (`"Sa"`, from a comma list like
/// `"Sa,Su"`) — into its expanded label. `None` if it isn't a recognized day token at all.
fn parse_day_token(token: &str) -> Option<String> {
    if let Some((from, to)) = token.split_once('-') {
        Some(format!("{}–{}", day_name(from)?, day_name(to)?))
    } else {
        day_name(token).map(str::to_string)
    }
}

fn parse_day_selector(selector: &str) -> Option<String> {
    let parts: Option<Vec<String>> = selector.split(',').map(parse_day_token).collect();
    parts.map(|days| days.join(", "))
}

/// Parses one `;`-separated rule. Recognized shapes: `<days> <times>`, `<days> off`/`closed`,
/// `24/7`, or bare `<times>` (applies every day, e.g. a single-rule `"09:00-18:00"` with no day
/// selector at all). `PH` (public holidays) counts as a day. Anything else — the rule doesn't start with a recognized day token and isn't a plain
/// time range — is kept as `days: "Hours"`, `hours: <the rule's own text>` so it's still shown.
fn parse_rule(rule: &str) -> Option<OpeningRule> {
    let rule = rule.trim();
    if rule.is_empty() {
        return None;
    }

    let (first, rest) = match rule.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (rule, ""),
    };

    if rule == "24/7" {
        return Some(OpeningRule {
            days: "Every day".to_string(),
            hours: "Open 24 hours".to_string(),
        });
    }

    if let Some(days) = parse_day_selector(first) {
        let hours = format_hours(rest);
        return Some(OpeningRule { days, hours });
    }

    if is_time_ranges(rule) {
        return Some(OpeningRule {
            days: "Every day".to_string(),
            hours: format_time_ranges(rule),
        });
    }

    Some(OpeningRule {
        days: "Hours".to_string(),
        hours: rule.to_string(),
    })
}

/// Splits an `opening_hours` value on `;` and parses each rule, for a day/hours table — see the
/// module doc comment for what's actually understood.
pub fn parse_opening_hours(raw: &str) -> Vec<OpeningRule> {
    raw.split(';').filter_map(parse_rule).collect()
}

// ─── Open now? ────────────────────────────────────────────────────────────────

/// How long before closing a place counts as closing soon.
pub const CLOSING_SOON_MIN: u32 = 60;

const DAY_MINUTES: u32 = 24 * 60;
const WEEK_MINUTES: u32 = 7 * DAY_MINUTES;

/// Whether a place is open at a given time, from its `opening_hours`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenStatus {
    /// Open around the clock.
    AlwaysOpen,
    /// Open; closes at `closes` (minutes since Monday 00:00, may be in a later day).
    Open { closes: u32 },
    /// Open, closing within [`CLOSING_SOON_MIN`].
    ClosingSoon { closes: u32 },
    /// Closed; opens next at `opens` (same scale), if it ever does within a week.
    Closed { opens: Option<u32> },
}

/// Days a selector covers (`Mo-Fr`, `Sa,Su`, `Fr-Mo` wrapping round), Monday = 0. `None` for
/// anything else, `PH` included: holidays can't be told from the date alone.
fn day_set(selector: &str) -> Option<[bool; 7]> {
    let index = |abbr: &str| DAY_NAMES.iter().position(|(a, _)| *a == abbr);
    let mut days = [false; 7];
    for token in selector.split(',') {
        match token.split_once('-') {
            Some((from, to)) => {
                let (from, to) = (index(from)?, index(to)?);
                let mut day = from;
                loop {
                    days[day] = true;
                    if day == to {
                        break;
                    }
                    day = (day + 1) % 7;
                }
            }
            None => days[index(token)?] = true,
        }
    }
    Some(days)
}

/// `09:00-12:00,14:00-18:00` as minute ranges within the day; an end at or before its start
/// (`22:00-02:00`) runs past midnight. `None` for anything else (`+`, `sunset`, …).
fn minute_ranges(text: &str) -> Option<Vec<(u32, u32)>> {
    let minute = |time: &str| {
        let (hours, minutes) = time.trim().split_once(':')?;
        let (hours, minutes): (u32, u32) = (hours.parse().ok()?, minutes.parse().ok()?);
        (hours <= 24 && minutes < 60).then_some(hours * 60 + minutes)
    };
    text.split(',')
        .map(|range| {
            let (from, to) = range.split_once('-')?;
            let (from, to) = (minute(from)?, minute(to)?);
            Some((from, if to <= from { to + DAY_MINUTES } else { to }))
        })
        .collect()
}

/// The week's opening ranges, per day (Monday = 0). Later rules replace earlier ones for the
/// days they name, as in OSM. `None` when a rule isn't understood (better no status than a
/// wrong one); holiday-only rules are skipped.
fn week_schedule(raw: &str) -> Option<[Vec<(u32, u32)>; 7]> {
    let mut week: [Vec<(u32, u32)>; 7] = Default::default();
    for rule in raw
        .split(';')
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
    {
        if rule == "24/7" {
            week = std::array::from_fn(|_| vec![(0, DAY_MINUTES)]);
            continue;
        }
        let (days, hours) = match rule.split_once(char::is_whitespace) {
            Some((first, rest)) if !first.contains(':') => (first, rest.trim()),
            _ => ("Mo-Su", rule),
        };
        if days == "PH" {
            continue;
        }
        let days = day_set(days)?;
        let ranges = if hours.eq_ignore_ascii_case("off") || hours.eq_ignore_ascii_case("closed") {
            Vec::new()
        } else {
            minute_ranges(hours)?
        };
        for (day, named) in days.iter().enumerate() {
            if *named {
                week[day] = ranges.clone();
            }
        }
    }
    Some(week)
}

/// The status of `raw` at `now`: minutes since Monday 00:00 (local time). `None` when the value
/// isn't understood.
pub fn open_status(raw: &str, now: u32) -> Option<OpenStatus> {
    let week = week_schedule(raw)?;
    if week.iter().all(|day| day.as_slice() == [(0, DAY_MINUTES)]) {
        return Some(OpenStatus::AlwaysOpen);
    }
    // Every range on one axis, in minutes since this Monday 00:00, repeated a week either way:
    // last Sunday night's can run into this Monday, and the next opening can be next week.
    let ranges: Vec<(i64, i64)> = week
        .iter()
        .enumerate()
        .flat_map(|(day, ranges)| {
            let offset = (day as u32 * DAY_MINUTES) as i64;
            ranges
                .iter()
                .map(move |(from, to)| (offset + *from as i64, offset + *to as i64))
        })
        .flat_map(|(from, to)| {
            let week = WEEK_MINUTES as i64;
            [
                (from - week, to - week),
                (from, to),
                (from + week, to + week),
            ]
        })
        .collect();
    let now_at = now as i64;

    // Open: the latest close among back-to-back ranges (`Mo 18:00-24:00; Tu 00:00-02:00`).
    if ranges
        .iter()
        .any(|(from, to)| (*from..*to).contains(&now_at))
    {
        let mut closes = now_at;
        while let Some(to) = ranges
            .iter()
            .filter(|(from, to)| *from <= closes && *to > closes)
            .map(|(_, to)| *to)
            .max()
        {
            closes = to;
            if closes >= now_at + WEEK_MINUTES as i64 {
                return Some(OpenStatus::AlwaysOpen);
            }
        }
        let closes = closes as u32;
        return Some(if closes - now <= CLOSING_SOON_MIN {
            OpenStatus::ClosingSoon { closes }
        } else {
            OpenStatus::Open { closes }
        });
    }
    let opens = ranges
        .iter()
        .map(|(from, _)| *from)
        .filter(|from| *from > now_at && *from <= now_at + WEEK_MINUTES as i64)
        .min()
        .map(|from| from as u32);
    Some(OpenStatus::Closed { opens })
}

/// `"18:00"`, with the day in front when it isn't `now`'s (`"Mon 09:00"`, `"tomorrow 09:00"`).
pub fn when_text(at: u32, now: u32) -> String {
    let clock = format!("{:02}:{:02}", at % DAY_MINUTES / 60, at % 60);
    match at / DAY_MINUTES - now / DAY_MINUTES {
        0 => clock,
        1 => format!("tomorrow {clock}"),
        _ => format!("{} {clock}", DAY_NAMES[(at / DAY_MINUTES % 7) as usize].1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_weekday_and_weekend() {
        let rules = parse_opening_hours("Mo-Fr 09:00-18:00; Sa 10:00-13:00; Su off");
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].days, "Mon–Fri");
        assert_eq!(rules[0].hours, "09:00–18:00");
        assert_eq!(rules[1].days, "Sat");
        assert_eq!(rules[2].days, "Sun");
        assert_eq!(rules[2].hours, "Closed");
    }

    #[test]
    fn comma_day_list() {
        let rules = parse_opening_hours("Sa,Su 10:00-16:00");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].days, "Sat, Sun");
    }

    #[test]
    fn bare_time_range_has_no_day_selector() {
        let rules = parse_opening_hours("09:30-23:45");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].days, "Every day");
        assert_eq!(rules[0].hours, "09:30–23:45");
    }

    #[test]
    fn split_shift_and_holidays() {
        let rules = parse_opening_hours("Mo-Fr 09:00-12:00,14:00-18:00; PH off");
        assert_eq!(rules[0].hours, "09:00–12:00, 14:00–18:00");
        assert_eq!(rules[1].days, "Holidays");
        assert_eq!(rules[1].hours, "Closed");
    }

    #[test]
    fn always_open() {
        let rules = parse_opening_hours("24/7");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].days, "Every day");
        assert_eq!(rules[0].hours, "Open 24 hours");
    }

    #[test]
    fn unsupported_rule_falls_back_to_raw_text() {
        let rules = parse_opening_hours("Jun 21-Sep 02: 09:00-00:45");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].days, "Hours");
        assert_eq!(rules[0].hours, "Jun 21-Sep 02: 09:00-00:45");
    }

    #[test]
    fn empty_clauses_are_skipped() {
        let rules = parse_opening_hours("Mo-Fr 09:00-18:00; ; Sa 10:00-13:00");
        assert_eq!(rules.len(), 2);
    }

    /// Minutes since Monday 00:00.
    fn at(day: u32, hours: u32, minutes: u32) -> u32 {
        day * DAY_MINUTES + hours * 60 + minutes
    }

    #[test]
    fn open_closing_soon_and_closed() {
        let raw = "Mo-Fr 09:00-18:00; Sa 10:00-13:00; Su off";
        assert_eq!(
            open_status(raw, at(0, 12, 0)),
            Some(OpenStatus::Open {
                closes: at(0, 18, 0)
            })
        );
        assert_eq!(
            open_status(raw, at(0, 17, 30)),
            Some(OpenStatus::ClosingSoon {
                closes: at(0, 18, 0)
            })
        );
        assert_eq!(
            open_status(raw, at(0, 19, 0)),
            Some(OpenStatus::Closed {
                opens: Some(at(1, 9, 0))
            })
        );
        // Saturday after closing: Sunday off, so Monday.
        assert_eq!(
            open_status(raw, at(5, 14, 0)),
            Some(OpenStatus::Closed {
                opens: Some(at(7, 9, 0))
            })
        );
    }

    #[test]
    fn past_midnight_and_week_wrap() {
        let raw = "Fr-Su 20:00-02:00";
        // Saturday 00:30 is still Friday night's opening.
        assert_eq!(
            open_status(raw, at(5, 0, 30)),
            Some(OpenStatus::Open {
                closes: at(5, 2, 0)
            })
        );
        // Monday 01:30 is still Sunday night's, closing within the hour.
        assert_eq!(
            open_status(raw, at(0, 1, 30)),
            Some(OpenStatus::ClosingSoon {
                closes: at(0, 2, 0)
            })
        );
    }

    #[test]
    fn later_rule_overrides_and_always_open() {
        assert_eq!(
            open_status("Mo-Su 09:00-18:00; Su off", at(6, 12, 0)),
            Some(OpenStatus::Closed {
                opens: Some(at(7, 9, 0))
            })
        );
        assert_eq!(
            open_status("24/7", at(3, 3, 0)),
            Some(OpenStatus::AlwaysOpen)
        );
        assert_eq!(open_status("Jun 21-Sep 02: 09:00-00:45", 0), None);
    }

    #[test]
    fn when_text_names_other_days() {
        let now = at(0, 19, 0);
        assert_eq!(when_text(at(0, 21, 0), now), "21:00");
        assert_eq!(when_text(at(1, 9, 0), now), "tomorrow 09:00");
        assert_eq!(when_text(at(3, 9, 0), now), "Thu 09:00");
    }
}
