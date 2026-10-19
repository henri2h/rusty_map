//! Whole trips and journey planning from Transitous: `trip` (every stop of one vehicle's run,
//! opened from a departure row) and `plan` (public-transport itineraries between two points).
//! Both answer with the same leg shape, parsed here into [`Leg`]s.

use serde::Deserialize;

use map_component::util::tiles::{FetchError, background_runtime};

use super::{
    api::{Mode, get_json, is_mission_code, parse_hex_color, parse_utc},
    when::When,
};

/// A stop (or the journey's start/end) along a leg.
#[derive(Clone, PartialEq)]
pub struct LegPlace {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// Expected time at this place (departure, else arrival), Unix seconds.
    pub time: Option<i64>,
    pub cancelled: bool,
}

#[derive(Clone, PartialEq)]
pub struct Leg {
    pub mode: Mode,
    /// Line name (`"42"`, `"RER A"`); empty for walking.
    pub line: String,
    pub long_name: String,
    pub color: Option<u32>,
    pub text_color: Option<u32>,
    pub headsign: String,
    pub agency: String,
    pub from: LegPlace,
    pub to: LegPlace,
    pub intermediate: Vec<LegPlace>,
    pub start: i64,
    pub end: i64,
    /// Expected minus scheduled start, whole minutes; realtime legs only.
    pub delay_min: Option<i64>,
    pub realtime: bool,
    pub wheelchair_accessible: Option<bool>,
    /// `(lon, lat)` path.
    pub geometry: Vec<(f64, f64)>,
    /// Meters, for walking legs.
    pub distance_m: Option<f64>,
}

impl Leg {
    pub fn is_walk(&self) -> bool {
        self.mode == Mode::Walk
    }
}

#[derive(Clone, PartialEq)]
pub struct Itinerary {
    pub start: i64,
    pub end: i64,
    pub transfers: u32,
    pub legs: Vec<Leg>,
}

impl Itinerary {
    pub fn duration_s(&self) -> i64 {
        self.end - self.start
    }

    pub fn walk_s(&self) -> i64 {
        self.legs
            .iter()
            .filter(|leg| leg.is_walk())
            .map(|leg| leg.end - leg.start)
            .sum()
    }
}

/// Runs `future` on the background runtime (reqwest needs Tokio; freya's executor isn't).
pub(crate) async fn on_background<T: Send + 'static>(
    future: impl Future<Output = Result<T, FetchError>> + Send + 'static,
) -> Result<T, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let _ = tx.send(future.await);
    });
    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

/// Every stop of the trip `trip_id` (from a departure), with expected times and its path.
pub async fn fetch_trip(trip_id: String) -> Result<Leg, FetchError> {
    on_background(async move {
        let response: ItineraryJson = get_json(
            "/v6/trip",
            &[("tripId", trip_id), ("detailedLegs", "true".into())],
        )
        .await?;
        response
            .legs
            .into_iter()
            .find_map(|leg| to_leg(leg).filter(|leg| !leg.is_walk()))
            .ok_or(FetchError::InvalidData)
    })
    .await
}

/// Public-transport itineraries from `from` to `to` (`(lat, lon)`), leaving or arriving as
/// `when` says. The journey's own ends are labeled `from_name`/`to_name`.
pub async fn plan(
    from: (f64, f64),
    to: (f64, f64),
    from_name: String,
    to_name: String,
    when: When,
) -> Result<Vec<Itinerary>, FetchError> {
    on_background(async move {
        let mut query = vec![
            ("fromPlace", format!("{},{}", from.0, from.1)),
            ("toPlace", format!("{},{}", to.0, to.1)),
            ("detailedLegs", "true".into()),
        ];
        query.extend(when.query());
        let response: PlanJson = get_json("/v6/plan", &query).await?;
        let mut itineraries: Vec<Itinerary> = response
            .itineraries
            .into_iter()
            .filter_map(to_itinerary)
            .collect();
        for itinerary in &mut itineraries {
            for leg in &mut itinerary.legs {
                for place in [&mut leg.from, &mut leg.to] {
                    match place.name.as_str() {
                        START_LABEL => place.name = from_name.clone(),
                        END_LABEL => place.name = to_name.clone(),
                        _ => {}
                    }
                }
            }
        }
        Ok(itineraries)
    })
    .await
}

#[derive(Deserialize)]
struct PlanJson {
    #[serde(default)]
    itineraries: Vec<ItineraryJson>,
}

#[derive(Deserialize)]
struct ItineraryJson {
    #[serde(rename = "startTime", default)]
    start_time: Option<String>,
    #[serde(rename = "endTime", default)]
    end_time: Option<String>,
    #[serde(default)]
    transfers: u32,
    #[serde(default)]
    legs: Vec<LegJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegJson {
    #[serde(default)]
    mode: String,
    from: PlaceJson,
    to: PlaceJson,
    start_time: String,
    end_time: String,
    scheduled_start_time: Option<String>,
    #[serde(default)]
    real_time: bool,
    #[serde(default)]
    headsign: String,
    #[serde(default)]
    route_short_name: String,
    #[serde(default)]
    route_long_name: String,
    #[serde(default)]
    display_name: String,
    route_color: Option<String>,
    route_text_color: Option<String>,
    #[serde(default)]
    agency_name: String,
    trip_to: Option<NameJson>,
    #[serde(default)]
    intermediate_stops: Vec<PlaceJson>,
    leg_geometry: Option<PolylineJson>,
    distance: Option<f64>,
    wheelchair_accessible: Option<String>,
}

#[derive(Deserialize)]
struct NameJson {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceJson {
    #[serde(default)]
    name: String,
    lat: f64,
    lon: f64,
    departure: Option<String>,
    arrival: Option<String>,
    #[serde(default)]
    cancelled: bool,
}

#[derive(Deserialize)]
struct PolylineJson {
    points: String,
    precision: u32,
}

fn to_itinerary(json: ItineraryJson) -> Option<Itinerary> {
    let legs: Vec<Leg> = json.legs.into_iter().filter_map(to_leg).collect();
    let start = json
        .start_time
        .as_deref()
        .and_then(parse_utc)
        .or_else(|| legs.first().map(|leg| leg.start))?;
    let end = json
        .end_time
        .as_deref()
        .and_then(parse_utc)
        .or_else(|| legs.last().map(|leg| leg.end))?;
    Some(Itinerary {
        start,
        end,
        transfers: json.transfers,
        legs,
    })
}

/// Placeholder names MOTIS gives a journey's own start and end.
const START_LABEL: &str = "START";
const END_LABEL: &str = "END";

fn to_place(json: PlaceJson) -> LegPlace {
    let time = json
        .departure
        .as_deref()
        .or(json.arrival.as_deref())
        .and_then(parse_utc);
    LegPlace {
        name: json.name,
        lat: json.lat,
        lon: json.lon,
        time,
        cancelled: json.cancelled,
    }
}

fn to_leg(json: LegJson) -> Option<Leg> {
    let start = parse_utc(&json.start_time)?;
    let end = parse_utc(&json.end_time)?;
    let delay_min = json
        .scheduled_start_time
        .as_deref()
        .and_then(parse_utc)
        .filter(|_| json.real_time)
        .map(|scheduled| (start - scheduled).div_euclid(60));
    let headsign = match json.trip_to {
        Some(to) if json.headsign.is_empty() || is_mission_code(&json.headsign) => to.name,
        _ => json.headsign,
    };
    let line = if json.route_short_name.is_empty() {
        json.display_name
    } else {
        json.route_short_name
    };
    let geometry = json
        .leg_geometry
        .map(|polyline| decode_polyline(&polyline.points, polyline.precision))
        .unwrap_or_default();
    Some(Leg {
        mode: Mode::from_motis(&json.mode),
        line,
        long_name: json.route_long_name,
        color: json.route_color.as_deref().and_then(parse_hex_color),
        text_color: json.route_text_color.as_deref().and_then(parse_hex_color),
        headsign,
        agency: json.agency_name,
        from: to_place(json.from),
        to: to_place(json.to),
        intermediate: json.intermediate_stops.into_iter().map(to_place).collect(),
        start,
        end,
        delay_min,
        realtime: json.real_time,
        wheelchair_accessible: json
            .wheelchair_accessible
            .as_deref()
            .map(|value| value == "ACCESSIBLE"),
        geometry,
        distance_m: json.distance,
    })
}

/// Google encoded polyline → `(lon, lat)` points; `precision` is the number of decimals.
pub fn decode_polyline(encoded: &str, precision: u32) -> Vec<(f64, f64)> {
    let factor = 10f64.powi(precision as i32);
    let bytes = encoded.as_bytes();
    let mut index = 0;
    let (mut lat, mut lon) = (0i64, 0i64);
    let mut points = Vec::new();
    let next = |index: &mut usize| -> Option<i64> {
        let (mut result, mut shift) = (0i64, 0u32);
        loop {
            let byte = (*bytes.get(*index)? as i64) - 63;
            *index += 1;
            result |= (byte & 0x1f) << shift;
            shift += 5;
            if byte < 0x20 {
                break;
            }
        }
        Some(if result & 1 == 1 {
            !(result >> 1)
        } else {
            result >> 1
        })
    };
    while index < bytes.len() {
        let (Some(d_lat), Some(d_lon)) = (next(&mut index), next(&mut index)) else {
            break;
        };
        lat += d_lat;
        lon += d_lon;
        points.push((lon as f64 / factor, lat as f64 / factor));
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_google_polyline() {
        // The reference example from Google's polyline algorithm documentation.
        let points = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@", 5);
        let expected = [(-120.2, 38.5), (-120.95, 40.7), (-126.453, 43.252)];
        assert_eq!(points.len(), expected.len());
        for ((lon, lat), (want_lon, want_lat)) in points.iter().zip(expected) {
            assert!((lon - want_lon).abs() < 1e-9 && (lat - want_lat).abs() < 1e-9);
        }
    }
}
