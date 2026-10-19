//! Walking routes. First from IGN's Géoplateforme router (`bdtopo-valhalla`, pedestrian, as
//! IGN's own Cartes app asks it): real turn-by-turn instructions on BD TOPO, France only. Else
//! (abroad, or IGN failing) from Transitous' `plan`: with only `directModes=WALK` (and no transit
//! modes, so it doesn't search connections), its `direct` answer is the walk alone — one leg
//! whose steps carry street names and their own paths but no turns (see `maneuvers`, which
//! derives them from those paths).

use serde::Deserialize;

use map_component::util::tiles::FetchError;

use super::{
    geo::cumulative,
    maneuvers::{GivenStep, Maneuver, RawStep, assemble, build, kind_from_instruction},
    streets::pretty_street,
};
use crate::components::transit::{decode_polyline, get_json, on_background};

/// The longest walk asked for: three hours.
const MAX_WALK_S: u32 = 3 * 3600;

const IGN_API: &str = "https://data.geopf.fr/navigation/itineraire";
const IGN_USER_AGENT: &str = concat!("rusty_map/", env!("CARGO_PKG_VERSION"), " (map@carnot.cc)");
/// Credited under IGN routes.
pub const IGN_SOURCE_URL: &str =
    "https://geoservices.ign.fr/documentation/services/services-geoplateforme/itineraire";

/// Which router found a route, for its credit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteSource {
    Ign,
    Transitous,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WalkRoute {
    /// `(lon, lat)` path.
    pub points: Vec<(f64, f64)>,
    /// Metres along the path at each point.
    pub along: Vec<f64>,
    pub distance_m: f64,
    pub duration_s: f64,
    pub maneuvers: Vec<Maneuver>,
    pub source: RouteSource,
}

impl WalkRoute {
    /// Walking pace the router assumed, metres per second.
    pub fn pace(&self) -> f64 {
        if self.duration_s > 0.0 {
            self.distance_m / self.duration_s
        } else {
            1.3
        }
    }

    /// Metres along the path at `maneuver`.
    pub fn along_at(&self, maneuver: &Maneuver) -> f64 {
        self.along[maneuver.at.min(self.along.len() - 1)]
    }
}

/// The walk from `from` to `to`, both `(lat, lon)`: IGN's, else Transitous'.
pub async fn fetch_walk_route(from: (f64, f64), to: (f64, f64)) -> Result<WalkRoute, FetchError> {
    match fetch_ign(from, to).await {
        Ok(route) => Ok(route),
        Err(err) => {
            log::info!("[navigation] IGN route unavailable ({err:?}), asking Transitous");
            fetch_transitous(from, to).await
        }
    }
}

async fn fetch_ign(from: (f64, f64), to: (f64, f64)) -> Result<WalkRoute, FetchError> {
    on_background(async move {
        let response = map_component::util::tiles::client()
            .get(IGN_API)
            .query(&[
                ("resource", "bdtopo-valhalla".to_string()),
                ("profile", "pedestrian".to_string()),
                // Walking: shortest, as IGN's own app does for pedestrians.
                ("optimization", "shortest".to_string()),
                ("start", format!("{},{}", from.1, from.0)),
                ("end", format!("{},{}", to.1, to.0)),
                ("geometryFormat", "geojson".to_string()),
                ("getSteps", "true".to_string()),
                ("crs", "EPSG:4326".to_string()),
                ("distanceUnit", "meter".to_string()),
                ("timeUnit", "second".to_string()),
            ])
            .header(reqwest::header::USER_AGENT, IGN_USER_AGENT)
            .send()
            .await
            .map_err(FetchError::Network)?;
        if !response.status().is_success() {
            return Err(FetchError::Http(response.status().as_u16()));
        }
        let text = response.text().await.map_err(FetchError::Network)?;
        let json: IgnRouteJson = serde_json::from_str(&text).map_err(|err| {
            log::warn!("[navigation] unexpected IGN response: {err}");
            FetchError::InvalidData
        })?;
        parse_ign(json).ok_or(FetchError::InvalidData)
    })
    .await
}

#[derive(Deserialize)]
struct IgnRouteJson {
    distance: f64,
    duration: f64,
    #[serde(default)]
    portions: Vec<IgnPortionJson>,
}

#[derive(Deserialize)]
struct IgnPortionJson {
    #[serde(default)]
    steps: Vec<IgnStepJson>,
}

#[derive(Deserialize)]
struct IgnStepJson {
    instruction: IgnInstructionJson,
    geometry: IgnGeometryJson,
    #[serde(default)]
    attributes: IgnAttributesJson,
}

#[derive(Deserialize)]
struct IgnInstructionJson {
    #[serde(rename = "type")]
    kind: String,
    modifier: Option<String>,
    exit: Option<u32>,
}

#[derive(Deserialize)]
struct IgnGeometryJson {
    coordinates: Vec<(f64, f64)>,
}

#[derive(Deserialize, Default)]
struct IgnAttributesJson {
    #[serde(default)]
    name: IgnNameJson,
}

#[derive(Deserialize, Default)]
struct IgnNameJson {
    #[serde(default)]
    nom_1_droite: String,
    #[serde(default)]
    nom_1_gauche: String,
}

fn parse_ign(json: IgnRouteJson) -> Option<WalkRoute> {
    let steps: Vec<GivenStep> = json
        .portions
        .into_iter()
        .flat_map(|portion| portion.steps)
        .map(|step| {
            let name = &step.attributes.name;
            // The right-hand name, as IGN's app shows; the left one when only it is set.
            let raw = if name.nom_1_droite.is_empty() {
                &name.nom_1_gauche
            } else {
                &name.nom_1_droite
            };
            GivenStep {
                kind: kind_from_instruction(
                    &step.instruction.kind,
                    step.instruction.modifier.as_deref(),
                ),
                street: pretty_street(raw),
                exit: step.instruction.exit,
                points: step.geometry.coordinates,
            }
        })
        .collect();
    let (points, maneuvers) = assemble(&steps);
    if points.len() < 2 {
        return None;
    }
    let along = cumulative(&points);
    let distance_m = if json.distance > 0.0 {
        json.distance
    } else {
        *along.last()?
    };
    Some(WalkRoute {
        points,
        along,
        distance_m,
        duration_s: json.duration,
        maneuvers,
        source: RouteSource::Ign,
    })
}

async fn fetch_transitous(from: (f64, f64), to: (f64, f64)) -> Result<WalkRoute, FetchError> {
    on_background(async move {
        let query = [
            ("fromPlace", format!("{},{}", from.0, from.1)),
            ("toPlace", format!("{},{}", to.0, to.1)),
            ("directModes", "WALK".to_string()),
            ("transitModes", String::new()),
            ("maxDirectTime", MAX_WALK_S.to_string()),
            ("detailedLegs", "true".to_string()),
        ];
        let response: PlanJson = get_json("/v6/plan", &query).await?;
        parse(response).ok_or(FetchError::InvalidData)
    })
    .await
}

#[derive(Deserialize)]
struct PlanJson {
    #[serde(default)]
    direct: Vec<ItineraryJson>,
}

#[derive(Deserialize)]
struct ItineraryJson {
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    legs: Vec<LegJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegJson {
    #[serde(default)]
    distance: f64,
    leg_geometry: Option<PolylineJson>,
    #[serde(default)]
    steps: Vec<StepJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StepJson {
    #[serde(default)]
    relative_direction: String,
    #[serde(default)]
    street_name: String,
    polyline: Option<PolylineJson>,
}

#[derive(Deserialize)]
struct PolylineJson {
    points: String,
    precision: u32,
}

fn parse(response: PlanJson) -> Option<WalkRoute> {
    let itinerary = response.direct.into_iter().next()?;
    let mut steps = Vec::new();
    let mut fallback = Vec::new();
    let mut distance_m = 0.0;
    for leg in itinerary.legs {
        distance_m += leg.distance;
        if let Some(geometry) = &leg.leg_geometry {
            fallback.extend(decode_polyline(&geometry.points, geometry.precision));
        }
        steps.extend(leg.steps.into_iter().filter_map(|step| {
            let polyline = step.polyline?;
            Some(RawStep {
                direction: step.relative_direction,
                street: step.street_name,
                points: decode_polyline(&polyline.points, polyline.precision),
            })
        }));
    }
    // Without steps, the leg's whole path as one unnamed step: no turns, but still a route.
    if steps.iter().all(|step| step.points.len() < 2) {
        steps = vec![RawStep {
            direction: "CONTINUE".into(),
            street: String::new(),
            points: fallback,
        }];
    }
    let (points, maneuvers) = build(&steps);
    if points.len() < 2 {
        return None;
    }
    let along = cumulative(&points);
    if distance_m <= 0.0 {
        distance_m = *along.last()?;
    }
    Some(WalkRoute {
        points,
        along,
        distance_m,
        duration_s: itinerary.duration,
        maneuvers,
        source: RouteSource::Transitous,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::navigation::maneuvers::ManeuverKind;

    /// A real answer, Eiffel Tower → Louvre (2026-10-19), trimmed to `direct`.
    const FIXTURE: &str = include_str!("testdata/walk_plan.json");
    /// IGN's `bdtopo-valhalla` pedestrian answer for the same walk (2026-10-19).
    const IGN_FIXTURE: &str = include_str!("testdata/ign_walk.json");

    #[test]
    fn parses_a_real_ign_walk() {
        let route = parse_ign(serde_json::from_str(IGN_FIXTURE).unwrap()).unwrap();
        assert_eq!(route.source, RouteSource::Ign);
        assert!((route.distance_m - 3681.9).abs() < 1.0);
        let length = *route.along.last().unwrap();
        assert!(
            (length - route.distance_m).abs() / route.distance_m < 0.05,
            "{length}"
        );
        let kinds: Vec<_> = route.maneuvers.iter().map(|m| m.kind).collect();
        assert_eq!(kinds.first(), Some(&ManeuverKind::Depart));
        assert_eq!(kinds.last(), Some(&ManeuverKind::Arrive));
        // IGN's own turns come through, named the way streets are signed.
        let named: Vec<&str> = route
            .maneuvers
            .iter()
            .filter_map(|m| m.street.as_deref())
            .collect();
        assert!(named.contains(&"Boulevard Saint-Germain"), "{named:?}");
        assert!(
            named.contains(&"Quai Valery Giscard d'Estaing"),
            "{named:?}"
        );
        let germain = route
            .maneuvers
            .iter()
            .find(|m| m.street.as_deref() == Some("Boulevard Saint-Germain"))
            .unwrap();
        assert_eq!(germain.kind, ManeuverKind::Right);
        let total: f64 = route.maneuvers.iter().map(|m| m.distance_m).sum();
        assert!((total - length).abs() < 1.0);
    }

    #[test]
    fn parses_a_real_walk() {
        let route = parse(serde_json::from_str(FIXTURE).unwrap()).unwrap();
        assert!((route.distance_m - 3779.0).abs() < 1.0);
        assert!((route.duration_s - 3129.0).abs() < 1.0);
        // The path's own length agrees with the router's distance.
        let length = *route.along.last().unwrap();
        assert!(
            (length - route.distance_m).abs() / route.distance_m < 0.05,
            "{length}"
        );
        // 112 raw steps become a readable handful, framed by Depart and Arrive.
        let count = route.maneuvers.len();
        assert!((3..=40).contains(&count), "{count} maneuvers");
        assert_eq!(route.maneuvers[0].kind, ManeuverKind::Depart);
        assert_eq!(route.maneuvers[count - 1].kind, ManeuverKind::Arrive);
        assert!(
            route
                .maneuvers
                .iter()
                .any(|m| m.kind == ManeuverKind::Stairs)
        );
        let total: f64 = route.maneuvers.iter().map(|m| m.distance_m).sum();
        assert!((total - length).abs() < 1.0);
    }
}
