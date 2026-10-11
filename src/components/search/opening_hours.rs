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
}
