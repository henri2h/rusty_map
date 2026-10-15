//! Next departures for a public-transport stop, from [Transitous](https://transitous.org) — a
//! community-run MOTIS 2 instance fed with, for France, the GTFS/GTFS-RT feeds published on
//! transport.data.gouv.fr. Two calls: `reverse-geocode` (type `STOP`) to find the stop behind a
//! tapped POI or search result, then `stoptimes` for its departures.
//!
//! Usage policy (<https://transitous.org/api/>): open-source, non-commercial, light use; every
//! request identifies the app and a contact via its User-Agent ([`USER_AGENT`]), and the UI links
//! [`SOURCES_URL`].

use serde::Deserialize;

use map_component::util::tiles::{FetchError, background_runtime, client};

const API: &str = "https://api.transitous.org/api";

/// Transitous asks for app name, version and a contact in every request's User-Agent.
pub const USER_AGENT: &str = concat!("rusty_map/", env!("CARGO_PKG_VERSION"), " (map@carnot.cc)");

/// Transitous' data sources and licenses — must be linked wherever its data is shown.
pub const SOURCES_URL: &str = "https://transitous.org/sources/";

/// A stop whose reverse-geocoded position is further than this from the tapped point is a
/// different stop, not the same one with slightly different coordinates.
const MAX_STOP_DISTANCE_M: f64 = 300.0;

/// How many departures to ask for.
const DEPARTURE_COUNT: u32 = 12;

#[derive(Clone, PartialEq)]
pub struct Stop {
    pub id: String,
    pub name: String,
}

#[derive(Clone, PartialEq)]
pub struct Departure {
    /// Line name as shown on vehicles (`"42"`, `"T3b"`, `"RER A"`).
    pub line: String,
    /// GTFS `route_color`/`route_text_color`, as `0xRRGGBB`.
    pub color: Option<u32>,
    pub text_color: Option<u32>,
    pub mode: Mode,
    pub headsign: String,
    /// Expected departure (realtime when available), Unix seconds.
    pub time: i64,
    /// Expected minus scheduled, in whole minutes; only set for realtime departures.
    pub delay_min: Option<i64>,
    pub realtime: bool,
    pub cancelled: bool,
    /// Transitous trip id, to fetch the whole trip (see `journey::fetch_trip`).
    pub trip_id: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Walk,
    Bus,
    Tram,
    Subway,
    Rail,
    Ferry,
    Other,
}

impl Mode {
    pub(super) fn from_motis(mode: &str) -> Self {
        match mode {
            "BUS" | "COACH" => Mode::Bus,
            "TRAM" | "CABLE_CAR" | "FUNICULAR" => Mode::Tram,
            "SUBWAY" | "METRO" => Mode::Subway,
            "RAIL" | "HIGHSPEED_RAIL" | "LONG_DISTANCE" | "NIGHT_RAIL" | "REGIONAL_FAST_RAIL"
            | "REGIONAL_RAIL" | "SUBURBAN" => Mode::Rail,
            "FERRY" => Mode::Ferry,
            "WALK" => Mode::Walk,
            _ => Mode::Other,
        }
    }
}

/// The departures shown for a stop: the stop as Transitous names it, plus its next departures.
#[derive(Clone, PartialEq)]
pub struct StopDepartures {
    pub stop: Stop,
    pub departures: Vec<Departure>,
}

/// Finds the stop at (`lat`, `lon`) — preferring one named like `name` among the nearby
/// candidates — and fetches its next departures. `Ok(None)`: Transitous knows no stop there.
pub async fn departures_near(
    lat: f64,
    lon: f64,
    name: String,
) -> Result<Option<StopDepartures>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let result = async {
            let Some(stop) = find_stop(lat, lon, &name).await? else {
                return Ok(None);
            };
            let departures = fetch_departures(&stop.id).await?;
            Ok(Some(StopDepartures { stop, departures }))
        }
        .await;
        let _ = tx.send(result);
    });
    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

pub(super) async fn get_json<T: for<'de> Deserialize<'de>>(
    path: &str,
    query: &[(&str, String)],
) -> Result<T, FetchError> {
    let url = format!("{API}{path}");
    log::trace!("[http] GET {url} {query:?}");
    let response = client()
        .get(&url)
        .query(query)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(FetchError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    let text = response.text().await.map_err(FetchError::Network)?;
    serde_json::from_str(&text).map_err(|err| {
        log::warn!("[transit] unexpected {path} response: {err}");
        FetchError::InvalidData
    })
}

#[derive(Deserialize)]
struct GeocodeMatch {
    id: String,
    name: String,
    lat: f64,
    lon: f64,
}

async fn find_stop(lat: f64, lon: f64, name: &str) -> Result<Option<Stop>, FetchError> {
    let matches: Vec<GeocodeMatch> = get_json(
        "/v1/reverse-geocode",
        &[
            ("place", format!("{lat},{lon}")),
            ("type", "STOP".to_string()),
        ],
    )
    .await?;
    Ok(pick_stop(matches, lat, lon, name))
}

/// Nearby candidates come back nearest first; a same-named one wins over a nearer one with
/// another name (a tapped POI's position is approximate, and bus stops cluster at junctions).
fn pick_stop(matches: Vec<GeocodeMatch>, lat: f64, lon: f64, name: &str) -> Option<Stop> {
    let wanted = normalize_name(name);
    let near: Vec<GeocodeMatch> = matches
        .into_iter()
        .filter(|m| distance_m(lat, lon, m.lat, m.lon) <= MAX_STOP_DISTANCE_M)
        .collect();
    let index = near
        .iter()
        .position(|m| !wanted.is_empty() && normalize_name(&m.name) == wanted)
        .or_else(|| {
            near.iter().position(|m| {
                let candidate = normalize_name(&m.name);
                !wanted.is_empty() && (candidate.contains(&wanted) || wanted.contains(&candidate))
            })
        })
        .unwrap_or(0);
    near.into_iter().nth(index).map(|m| Stop {
        id: m.id,
        name: m.name,
    })
}

/// Lowercase, accents and punctuation dropped: `"Châtelet - Les Halles"` → `"chatelet les halles"`.
fn normalize_name(name: &str) -> String {
    let folded: String = name
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Great-circle distance, good enough at stop scale.
fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * a.sqrt().asin()
}

#[derive(Deserialize)]
struct StopTimesResponse {
    #[serde(rename = "stopTimes", default)]
    stop_times: Vec<StopTime>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopTime {
    place: StopTimePlace,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    real_time: bool,
    #[serde(default)]
    headsign: String,
    #[serde(default)]
    route_short_name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    route_color: Option<String>,
    #[serde(default)]
    route_text_color: Option<String>,
    trip_to: Option<TripEnd>,
    #[serde(default)]
    trip_id: String,
}

#[derive(Deserialize)]
struct TripEnd {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StopTimePlace {
    departure: Option<String>,
    scheduled_departure: Option<String>,
    #[serde(default)]
    cancelled: bool,
    #[serde(default)]
    pickup_type: Option<String>,
}

async fn fetch_departures(stop_id: &str) -> Result<Vec<Departure>, FetchError> {
    let response: StopTimesResponse = get_json(
        "/v6/stoptimes",
        &[
            ("stopId", stop_id.to_string()),
            ("n", DEPARTURE_COUNT.to_string()),
        ],
    )
    .await?;
    Ok(response
        .stop_times
        .into_iter()
        .filter_map(to_departure)
        .collect())
}

fn to_departure(stop_time: StopTime) -> Option<Departure> {
    // A trip ending here is listed with no boarding allowed — not a departure.
    if stop_time.place.pickup_type.as_deref() == Some("NOT_ALLOWED") {
        return None;
    }
    let time = parse_utc(stop_time.place.departure.as_deref()?)?;
    let scheduled = stop_time
        .place
        .scheduled_departure
        .as_deref()
        .and_then(parse_utc);
    let delay_min = scheduled
        .filter(|_| stop_time.real_time)
        .map(|scheduled| (time - scheduled).div_euclid(60));
    // Île-de-France RER trips carry a 4-letter mission code as headsign ("NEMO", "ROPO"),
    // meaningless to riders — show the terminus instead.
    let headsign = match stop_time.trip_to {
        Some(to) if is_mission_code(&stop_time.headsign) || stop_time.headsign.is_empty() => {
            to.name
        }
        _ => stop_time.headsign,
    };
    let line = if stop_time.route_short_name.is_empty() {
        stop_time.display_name
    } else {
        stop_time.route_short_name
    };
    Some(Departure {
        line,
        color: stop_time.route_color.as_deref().and_then(parse_hex_color),
        text_color: stop_time
            .route_text_color
            .as_deref()
            .and_then(parse_hex_color),
        mode: Mode::from_motis(&stop_time.mode),
        headsign,
        time,
        delay_min,
        realtime: stop_time.real_time,
        cancelled: stop_time.place.cancelled,
        trip_id: stop_time.trip_id,
    })
}

pub(super) fn is_mission_code(headsign: &str) -> bool {
    (2..=5).contains(&headsign.len()) && headsign.chars().all(|c| c.is_ascii_uppercase())
}

pub(super) fn parse_hex_color(hex: &str) -> Option<u32> {
    let hex = hex.trim_start_matches('#');
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok())?
}

/// `2026-10-11T22:01:00Z` (optionally with fractional seconds) → Unix seconds.
pub(super) fn parse_utc(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let mut time = time.split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second = time.next()?.parse::<f64>().ok()? as i64;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(super) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utc_timestamps() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2026-10-11T22:01:00Z"), Some(1_791_756_060));
        assert_eq!(parse_utc("2026-10-11T22:01:00.500Z"), Some(1_791_756_060));
        assert_eq!(parse_utc("2026-10-11T22:01:00+02:00"), None);
    }

    #[test]
    fn normalizes_stop_names() {
        assert_eq!(
            normalize_name("Châtelet - Les Halles"),
            "chatelet les halles"
        );
        assert_eq!(normalize_name("  Gare de l'Est "), "gare de l est");
    }

    fn candidate(name: &str, lat: f64, lon: f64) -> GeocodeMatch {
        GeocodeMatch {
            id: name.to_string(),
            name: name.to_string(),
            lat,
            lon,
        }
    }

    #[test]
    fn prefers_same_named_stop_over_nearest() {
        let picked = pick_stop(
            vec![
                candidate("Monttessuy", 48.8590, 2.2945),
                candidate("Tour Eiffel", 48.8597, 2.2943),
                candidate("Far away", 48.90, 2.35),
            ],
            48.8590,
            2.2945,
            "Tour Eiffel",
        );
        assert_eq!(picked.map(|s| s.name).as_deref(), Some("Tour Eiffel"));
    }

    #[test]
    fn ignores_stops_out_of_range() {
        let picked = pick_stop(vec![candidate("Far", 48.90, 2.35)], 48.8590, 2.2945, "Far");
        assert!(picked.is_none());
    }

    #[test]
    fn terminus_arrivals_are_not_departures() {
        let stop_time = |pickup: &str| StopTime {
            place: StopTimePlace {
                departure: Some("2026-10-11T22:05:00Z".into()),
                scheduled_departure: Some("2026-10-11T22:03:00Z".into()),
                cancelled: false,
                pickup_type: Some(pickup.into()),
            },
            mode: "BUS".into(),
            real_time: true,
            headsign: "X".into(),
            route_short_name: "42".into(),
            display_name: String::new(),
            route_color: Some("ffbe00".into()),
            route_text_color: None,
            trip_id: "trip".into(),
            trip_to: Some(TripEnd {
                name: "Gare Saint-Lazare".into(),
            }),
        };
        assert!(to_departure(stop_time("NOT_ALLOWED")).is_none());
        let departure = to_departure(stop_time("NORMAL")).unwrap();
        assert_eq!(departure.delay_min, Some(2));
        assert_eq!(departure.color, Some(0xffbe00));
        // A real headsign is kept; a mission code gives way to the terminus.
        assert_eq!(departure.headsign, "X");
        let mut coded = stop_time("NORMAL");
        coded.headsign = "NEMO".into();
        assert_eq!(to_departure(coded).unwrap().headsign, "Gare Saint-Lazare");
    }
}
