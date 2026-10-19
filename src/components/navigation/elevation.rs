//! Elevation along a walk, from the IGN Géoplateforme altimetry service (`elevationLine`):
//! heights sampled along a line, RGE ALTI in France, a world model elsewhere. Transitous'
//! router has no elevation data (its steps report 0 m up and down even up Montmartre).

use serde::Deserialize;

use map_component::util::tiles::{FetchError, client};

use super::geo::{cumulative, distance_m, point_at};
use crate::components::transit::on_background;

const API: &str = "https://data.geopf.fr/altimetrie/1.0/calcul/alti/rest/elevationLine.json";
const USER_AGENT: &str = concat!("rusty_map/", env!("CARGO_PKG_VERSION"), " (map@carnot.cc)");
/// Credited wherever this data is shown.
pub const SOURCE_URL: &str =
    "https://geoservices.ign.fr/documentation/services/services-geoplateforme/altimetrie";

/// Most points of the route sent in one request (the IGN Cartes app's own limit); a longer path
/// is resampled evenly down to this.
const MAX_LINE_POINTS: usize = 1500;
/// Heights asked for along it (as the IGN Cartes app does).
const SAMPLES: usize = 200;
/// The service's "no data" height.
const NO_DATA: f64 = -99_999.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// `(metres along the route, metres above sea level)`.
    pub samples: Vec<(f64, f64)>,
    pub climb_m: f64,
    pub descent_m: f64,
    pub min_m: f64,
    pub max_m: f64,
}

impl Profile {
    /// The height `along` metres into the walk, between the samples either side.
    pub fn height_at(&self, along: f64) -> f64 {
        let index = self.samples.partition_point(|(at, _)| *at < along);
        match (
            index.checked_sub(1).map(|i| self.samples[i]),
            self.samples.get(index).copied(),
        ) {
            (Some((a, za)), Some((b, zb))) if b > a => za + (zb - za) * (along - a) / (b - a),
            (_, Some((_, z))) | (Some((_, z)), None) => z,
            (None, None) => 0.0,
        }
    }
}

/// The profile of `points` (`(lon, lat)`, the route's path).
pub async fn fetch_profile(points: Vec<(f64, f64)>) -> Result<Profile, FetchError> {
    on_background(async move {
        let along = cumulative(&points);
        let length = *along.last().ok_or(FetchError::InvalidData)?;
        // The path itself, so the heights follow its streets; resampled evenly when too long.
        let line: Vec<(f64, f64)> = if points.len() <= MAX_LINE_POINTS {
            points.clone()
        } else {
            (0..MAX_LINE_POINTS)
                .map(|i| {
                    point_at(
                        &points,
                        &along,
                        length * i as f64 / (MAX_LINE_POINTS - 1) as f64,
                    )
                })
                .collect()
        };
        let join = |pick: fn(&(f64, f64)) -> f64| {
            line.iter()
                .map(|point| format!("{:.6}", pick(point)))
                .collect::<Vec<_>>()
                .join("|")
        };
        // POST with a JSON body, as the IGN Cartes app does: no URL length limit.
        let body = serde_json::json!({
            "lon": join(|point| point.0),
            "lat": join(|point| point.1),
            "resource": "ign_rge_alti_wld",
            "delimiter": "|",
            "indent": "false",
            "measures": "false",
            "zonly": "false",
            "profile_mode": "simple",
            "sampling": SAMPLES,
        });
        let response = client()
            .post(API)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(FetchError::Network)?;
        if !response.status().is_success() {
            return Err(FetchError::Http(response.status().as_u16()));
        }
        let text = response.text().await.map_err(FetchError::Network)?;
        let json: LineJson = serde_json::from_str(&text).map_err(|err| {
            log::warn!("[elevation] unexpected response: {err}");
            FetchError::InvalidData
        })?;
        parse(json, length).ok_or(FetchError::InvalidData)
    })
    .await
}

#[derive(Deserialize)]
struct LineJson {
    elevations: Vec<PointJson>,
    height_differences: Option<DifferencesJson>,
}

#[derive(Deserialize)]
struct PointJson {
    lon: f64,
    lat: f64,
    z: f64,
}

#[derive(Deserialize)]
struct DifferencesJson {
    positive: f64,
    negative: f64,
}

/// Samples placed along the route: their own spacing, scaled to the route's `length` (they
/// follow the simplified line sent, a little shorter than the route itself).
fn parse(json: LineJson, length: f64) -> Option<Profile> {
    let points: Vec<(f64, f64, f64)> = json
        .elevations
        .iter()
        .map(|point| (point.lon, point.lat, point.z))
        .collect();
    let mut sampled = vec![0.0];
    for pair in points.windows(2) {
        let step = distance_m((pair[0].0, pair[0].1), (pair[1].0, pair[1].1));
        sampled.push(sampled.last().unwrap() + step);
    }
    let scale = match sampled.last() {
        Some(total) if *total > 0.0 => length / total,
        _ => 1.0,
    };
    let samples: Vec<(f64, f64)> = points
        .iter()
        .zip(&sampled)
        .filter(|((_, _, z), _)| *z > NO_DATA + 1.0)
        .map(|((_, _, z), at)| (at * scale, *z))
        .collect();
    if samples.len() < 2 {
        return None;
    }
    let (climb_m, descent_m) = match json.height_differences {
        Some(differences) => (differences.positive, differences.negative),
        None => samples.windows(2).fold((0.0, 0.0), |(up, down), pair| {
            let change = pair[1].1 - pair[0].1;
            (up + change.max(0.0), down + (-change).max(0.0))
        }),
    };
    let (min_m, max_m) = samples
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (_, z)| {
            (lo.min(*z), hi.max(*z))
        });
    Some(Profile {
        samples,
        climb_m,
        descent_m,
        min_m,
        max_m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IGN's answer for Pigalle → Sacré-Cœur, 8 samples (2026-10-19).
    const FIXTURE: &str = r#"{"elevations": [{"lon": 2.3376, "lat": 48.8822, "z": 63.14, "acc": "x"}, {"lon": 2.338334929285643, "lat": 48.882904307232074, "z": 68.43, "acc": "x"}, {"lon": 2.339069858571286, "lat": 48.88360861446415, "z": 75.14, "acc": "x"}, {"lon": 2.3398047878569286, "lat": 48.88431292169623, "z": 81.71, "acc": "x"}, {"lon": 2.3406096265600813, "lat": 48.88493263820393, "z": 87.36, "acc": "x"}, {"lon": 2.3414397510400544, "lat": 48.88552175880262, "z": 106.88, "acc": "x"}, {"lon": 2.342269875520027, "lat": 48.88611087940131, "z": 124.29, "acc": "x"}, {"lon": 2.3431, "lat": 48.8867, "z": 119.0, "acc": "x"}], "height_differences": {"positive": 61.15, "negative": 5.29}}"#;

    #[test]
    fn parses_a_real_profile() {
        let profile = parse(serde_json::from_str(FIXTURE).unwrap(), 700.0).unwrap();
        assert_eq!(profile.samples.len(), 8);
        assert_eq!(profile.samples[0].0, 0.0);
        assert!(
            (profile.samples[7].0 - 700.0).abs() < 1e-6,
            "scaled to the route"
        );
        assert_eq!((profile.climb_m, profile.descent_m), (61.15, 5.29));
        assert_eq!((profile.min_m, profile.max_m), (63.14, 124.29));
        let middle = profile.height_at(350.0);
        assert!((81.0..=88.0).contains(&middle), "{middle}");
    }

    #[test]
    fn no_data_points_are_dropped() {
        let json = r#"{"elevations": [{"lon": 2.0, "lat": 48.0, "z": -99999}, {"lon": 2.001, "lat": 48.0, "z": 50}, {"lon": 2.002, "lat": 48.0, "z": 52}], "height_differences": null}"#;
        let profile = parse(serde_json::from_str(json).unwrap(), 150.0).unwrap();
        assert_eq!(profile.samples.len(), 2);
        assert_eq!((profile.climb_m, profile.descent_m), (2.0, 0.0));
    }
}
