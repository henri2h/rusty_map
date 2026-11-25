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
    #[serde(default, deserialize_with = "ign_name")]
    name: IgnNameJson,
}

#[derive(Deserialize, Default)]
struct IgnNameJson {
    #[serde(default)]
    nom_1_droite: String,
    #[serde(default)]
    nom_1_gauche: String,
}

/// A step's road names: normally an object, but IGN also sends them as a string holding a
/// fragment of that object's JSON, cut off at either end (seen for long walks) — which
/// failed the whole route. Names are dug out of such a fragment when it has them; none
/// otherwise (the step still counts, unnamed).
fn ign_name<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<IgnNameJson, D::Error> {
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(fragment) => {
            let field = |key: &str| {
                let start = fragment.find(&format!("{key}\": \""))? + key.len() + 4;
                let end = fragment[start..].find('"')?;
                Some(fragment[start..start + end].to_string())
            };
            IgnNameJson {
                nom_1_droite: field("nom_1_droite").unwrap_or_default(),
                nom_1_gauche: field("nom_1_gauche").unwrap_or_default(),
            }
        }
        value => serde_json::from_value(value).unwrap_or_default(),
    })
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
