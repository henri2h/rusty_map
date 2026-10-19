//! What runs on a tapped rail/metro/tram track: the OSM way under the tap, and the
//! public-transport route relations (`type=route`) that include it — line number, name, colour,
//! network, operator. The vector tiles carry no line names or colours themselves.
//!
//! The way is found with Overpass (the French instance, `overpass.openstreetmap.fr`) around the
//! tapped point: the tile's own feature id can't be trusted for that, because the tiles merge
//! adjacent tunnel ways of different lines into one feature that keeps only one member's id.
//! When Overpass is unavailable, that id is used through the OSM API as a best effort.

use serde::Deserialize;
use serde_json::Value;

use map_component::util::tiles::{FetchError, background_runtime, client};

use super::api::{Mode, USER_AGENT, parse_hex_color};

const OSM_API: &str = "https://api.openstreetmap.org/api/0.6";
const OVERPASS: &str = "https://overpass.openstreetmap.fr/api/interpreter";

/// `route=*` values that are public transport.
const TRANSIT_ROUTES: &[&str] = &[
    "subway",
    "tram",
    "light_rail",
    "train",
    "railway",
    "monorail",
    "funicular",
    "bus",
    "trolleybus",
    "ferry",
];

#[derive(Clone, PartialEq)]
pub struct TrackInfo {
    /// The way's own name (`"Métro 13"`, `"Ligne de Paris-Nord à Lille"`), if any.
    pub name: Option<String>,
    /// OSM `railway=*` value (`subway`, `tram`, `rail`).
    pub railway: Option<String>,
    pub operator: Option<String>,
    /// The way's `(lon, lat)` path, to highlight it.
    pub geometry: Vec<(f64, f64)>,
    pub routes: Vec<RouteInfo>,
}

#[derive(Clone, PartialEq)]
pub struct RouteInfo {
    pub relation_id: u64,
    pub mode: Mode,
    /// Line number (`"13"`, `"T3a"`, `"RER B"`).
    pub line: String,
    pub name: String,
    pub color: Option<u32>,
    pub network: Option<String>,
    pub operator: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// A whole route relation's track and stops, to draw it on the map.
#[derive(Clone, PartialEq)]
pub struct RouteShape {
    /// `(lon, lat)` paths, one per member way.
    pub ways: Vec<Vec<(f64, f64)>>,
    /// `(name, lon, lat)` of stop positions/platforms with a name.
    pub stops: Vec<(String, f64, f64)>,
}

async fn get(url: String) -> Result<Value, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let result = async {
            log::trace!("[http] GET {url}");
            let response = client()
                .get(&url)
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                .send()
                .await
                .map_err(FetchError::Network)?;
            let status = response.status();
            if !status.is_success() {
                return Err(FetchError::Http(status.as_u16()));
            }
            let text = response.text().await.map_err(FetchError::Network)?;
            serde_json::from_str::<Value>(&text).map_err(|_| FetchError::InvalidData)
        }
        .await;
        let _ = tx.send(result);
    });
    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

#[derive(Deserialize, Clone, Copy)]
struct LatLon {
    lat: f64,
    lon: f64,
}

#[derive(Deserialize)]
struct Element {
    #[serde(rename = "type")]
    kind: String,
    id: u64,
    #[serde(default)]
    tags: serde_json::Map<String, Value>,
    lat: Option<f64>,
    lon: Option<f64>,
    #[serde(default)]
    nodes: Vec<u64>,
    #[serde(default)]
    members: Vec<Member>,
    /// Overpass `out geom` way geometry.
    #[serde(default)]
    geometry: Vec<LatLon>,
}

#[derive(Deserialize)]
struct Member {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "ref")]
    id: u64,
    #[serde(default)]
    role: String,
}

impl Element {
    fn tag(&self, key: &str) -> Option<String> {
        self.tags
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }
}

fn elements(value: Value) -> Vec<Element> {
    value
        .get("elements")
        .cloned()
        .and_then(|elements| serde_json::from_value(elements).ok())
        .unwrap_or_default()
}

/// The track within `radius_m` of the tapped point (of the tile's `kind`: `subway`, `tram`,
/// `rail`, …) and the lines running on it. `way_hint` (the tile feature's id) is the fallback
/// when Overpass can't answer.
pub async fn track_near(
    lat: f64,
    lon: f64,
    radius_m: f64,
    kind: String,
    way_hint: Option<u64>,
) -> Result<TrackInfo, FetchError> {
    match overpass_track(lat, lon, radius_m, &kind).await {
        Ok(Some(info)) => return Ok(info),
        Ok(None) => log::debug!("[transit] no {kind} way within {radius_m:.0} m"),
        Err(err) => log::warn!("[transit] overpass failed, using the tile's way id: {err:?}"),
    }
    match way_hint {
        Some(way_id) => track_info(way_id).await,
        None => Err(FetchError::NoData),
    }
}

/// `railway=*` values matching a tile `transportation` subclass.
fn railway_pattern(kind: &str) -> &'static str {
    match kind {
        "subway" => "subway",
        "tram" => "tram",
        "light_rail" => "light_rail",
        "rail" | "narrow_gauge" | "preserved" => "rail|narrow_gauge|preserved",
        "funicular" => "funicular",
        "monorail" => "monorail",
        _ => "subway|tram|light_rail|rail|narrow_gauge|monorail|funicular",
    }
}

async fn overpass_track(
    lat: f64,
    lon: f64,
    radius_m: f64,
    kind: &str,
) -> Result<Option<TrackInfo>, FetchError> {
    let query = format!(
        "[out:json][timeout:10];\
         way(around:{radius:.0},{lat},{lon})[railway~\"^({pattern})$\"]->.w;\
         .w out tags geom;\
         rel(bw.w)[type=route];\
         out body;",
        radius = radius_m.ceil(),
        pattern = railway_pattern(kind),
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let result = async {
            log::trace!("[http] POST {OVERPASS} {query}");
            let response = client()
                .post(OVERPASS)
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                .form(&[("data", query)])
                .send()
                .await
                .map_err(FetchError::Network)?;
            let status = response.status();
            if !status.is_success() {
                return Err(FetchError::Http(status.as_u16()));
            }
            let text = response.text().await.map_err(FetchError::Network)?;
            serde_json::from_str::<Value>(&text).map_err(|_| FetchError::InvalidData)
        }
        .await;
        let _ = tx.send(result);
    });
    let response = rx.await.unwrap_or(Err(FetchError::TaskFailed))?;
    Ok(pick_track(elements(response), lat, lon))
}

/// Of the ways found around the tap, the nearest one carrying a transit route (parallel tracks
/// of one line are often separate ways, one per direction), else the nearest one at all.
fn pick_track(found: Vec<Element>, lat: f64, lon: f64) -> Option<TrackInfo> {
    let (ways, relations): (Vec<Element>, Vec<Element>) =
        found.into_iter().partition(|e| e.kind == "way");
    let routes_of = |way: &Element| -> Vec<Element> {
        relations
            .iter()
            .filter(|relation| {
                relation
                    .members
                    .iter()
                    .any(|member| member.kind == "way" && member.id == way.id)
            })
            .map(|relation| Element {
                kind: relation.kind.clone(),
                id: relation.id,
                tags: relation.tags.clone(),
                lat: None,
                lon: None,
                nodes: Vec::new(),
                members: Vec::new(),
                geometry: Vec::new(),
            })
            .collect()
    };
    let mut ranked: Vec<(f64, &Element, Vec<RouteInfo>)> = ways
        .iter()
        .map(|way| {
            (
                distance_to_path_m(lat, lon, &way.geometry),
                way,
                routes_from(routes_of(way)),
            )
        })
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let index = ranked
        .iter()
        .position(|(_, _, routes)| !routes.is_empty())
        .unwrap_or(0);
    let (_, way, routes) = ranked.into_iter().nth(index)?;
    Some(TrackInfo {
        name: way.tag("name"),
        railway: way.tag("railway"),
        operator: way.tag("operator"),
        geometry: way.geometry.iter().map(|p| (p.lon, p.lat)).collect(),
        routes,
    })
}

/// Shortest distance (meters) from a point to a polyline, on a local flat projection.
fn distance_to_path_m(lat: f64, lon: f64, path: &[LatLon]) -> f64 {
    let meters_per_deg_lat = 111_320.0;
    let meters_per_deg_lon = 111_320.0 * lat.to_radians().cos();
    let project = |p: &LatLon| {
        (
            (p.lon - lon) * meters_per_deg_lon,
            (p.lat - lat) * meters_per_deg_lat,
        )
    };
    let points: Vec<(f64, f64)> = path.iter().map(project).collect();
    let segment_distance = |a: (f64, f64), b: (f64, f64)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length_sq = dx * dx + dy * dy;
        let t = if length_sq > 0.0 {
            (-(a.0 * dx + a.1 * dy) / length_sq).clamp(0.0, 1.0)
        } else {
            0.0
        };
        ((a.0 + t * dx).powi(2) + (a.1 + t * dy).powi(2)).sqrt()
    };
    match points.as_slice() {
        [] => f64::MAX,
        [only] => (only.0.powi(2) + only.1.powi(2)).sqrt(),
        _ => points
            .windows(2)
            .map(|pair| segment_distance(pair[0], pair[1]))
            .fold(f64::MAX, f64::min),
    }
}

/// The tapped way's name and geometry, plus the transit routes using it.
pub async fn track_info(way_id: u64) -> Result<TrackInfo, FetchError> {
    let full = get(format!("{OSM_API}/way/{way_id}/full.json"));
    let relations = get(format!("{OSM_API}/way/{way_id}/relations.json"));
    let (full, relations) = (elements(full.await?), elements(relations.await?));

    let nodes: rustc_hash::FxHashMap<u64, (f64, f64)> = full
        .iter()
        .filter(|e| e.kind == "node")
        .filter_map(|e| Some((e.id, (e.lon?, e.lat?))))
        .collect();
    let way = full
        .iter()
        .find(|e| e.kind == "way" && e.id == way_id)
        .ok_or(FetchError::InvalidData)?;

    Ok(TrackInfo {
        name: way.tag("name"),
        railway: way.tag("railway"),
        operator: way.tag("operator"),
        geometry: way
            .nodes
            .iter()
            .filter_map(|node| nodes.get(node).copied())
            .collect(),
        routes: routes_from(relations),
    })
}

/// Transit route relations, one per line and direction collapsed into one per line: the two
/// directions of a line share number and network and differ only in `from`/`to`.
fn routes_from(relations: Vec<Element>) -> Vec<RouteInfo> {
    let mut routes: Vec<RouteInfo> = Vec::new();
    for relation in relations {
        if relation.tag("type").as_deref() != Some("route") {
            continue;
        }
        let Some(route) = relation.tag("route") else {
            continue;
        };
        if !TRANSIT_ROUTES.contains(&route.as_str()) {
            continue;
        }
        let line = relation
            .tag("ref")
            .or_else(|| relation.tag("name"))
            .unwrap_or_default();
        let network = relation.tag("network");
        if routes
            .iter()
            .any(|known| known.line == line && known.network == network)
        {
            continue;
        }
        routes.push(RouteInfo {
            relation_id: relation.id,
            mode: route_mode(&route),
            name: relation.tag("name").unwrap_or_else(|| line.clone()),
            line,
            color: relation.tag("colour").as_deref().and_then(parse_hex_color),
            network,
            operator: relation.tag("operator"),
            from: relation.tag("from"),
            to: relation.tag("to"),
        });
    }
    routes.sort_by_key(|route| natural_key(&route.line));
    routes
}

/// Orders `"2"` before `"10"`, and letters after numbers.
fn natural_key(line: &str) -> (u64, String) {
    let digits: String = line.chars().filter(char::is_ascii_digit).collect();
    (digits.parse().unwrap_or(u64::MAX), line.to_string())
}

fn route_mode(route: &str) -> Mode {
    match route {
        "subway" => Mode::Subway,
        "tram" | "light_rail" | "funicular" => Mode::Tram,
        "train" | "railway" | "monorail" => Mode::Rail,
        "bus" | "trolleybus" => Mode::Bus,
        "ferry" => Mode::Ferry,
        _ => Mode::Other,
    }
}

/// The lines a `transit_lines` tile feature (rusty_map's own tiles) names in its
/// `route_<n>_*` attributes, in the tile's order (rail first). Empty for any other feature.
pub fn routes_from_tile(properties: &map_component::util::renderer::Properties) -> Vec<RouteInfo> {
    use mvt_reader::feature::Value;
    let text = |key: String| match properties.get(&key) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    };
    let number = |key: String| match properties.get(&key) {
        Some(Value::Int(v) | Value::SInt(v)) => u64::try_from(*v).ok(),
        Some(Value::UInt(v)) => Some(*v),
        _ => None,
    };
    (1..)
        .map_while(|n| {
            let prefix = format!("route_{n}_");
            let line = text(format!("{prefix}ref"));
            let name = text(format!("{prefix}name"));
            if line.is_none() && name.is_none() {
                return None;
            }
            Some(RouteInfo {
                relation_id: number(format!("{prefix}id")).unwrap_or(0),
                mode: route_mode(&text(format!("{prefix}mode")).unwrap_or_default()),
                line: line.clone().or(name.clone()).unwrap_or_default(),
                name: name.or(line).unwrap_or_default(),
                color: text(format!("{prefix}colour"))
                    .as_deref()
                    .and_then(parse_hex_color),
                network: text(format!("{prefix}network")),
                operator: None,
                from: None,
                to: None,
            })
        })
        .collect()
}

/// The relation's member ways and named stops, for drawing the whole line. A line-level
/// `route_master` (what rusty_map's tiles reference) is drawn as all its member routes.
pub async fn route_shape(relation_id: u64) -> Result<RouteShape, FetchError> {
    let full = elements(get(format!("{OSM_API}/relation/{relation_id}/full.json")).await?);
    let relation = full
        .iter()
        .find(|e| e.kind == "relation" && e.id == relation_id)
        .ok_or(FetchError::InvalidData)?;

    if relation.tag("type").as_deref() == Some("route_master") {
        // Variants (RER A1/A2/…, both directions) each fetched in full; capped, a few suffice to
        // cover the whole network of a line.
        let members: Vec<u64> = relation
            .members
            .iter()
            .filter(|member| member.kind == "relation")
            .map(|member| member.id)
            .take(MAX_MASTER_MEMBERS)
            .collect();
        let mut shape = RouteShape {
            ways: Vec::new(),
            stops: Vec::new(),
        };
        for member in members {
            let full = elements(get(format!("{OSM_API}/relation/{member}/full.json")).await?);
            let part = shape_of(&full, member)?;
            shape.ways.extend(part.ways);
            for stop in part.stops {
                if !shape.stops.iter().any(|(known, _, _)| *known == stop.0) {
                    shape.stops.push(stop);
                }
            }
        }
        return Ok(shape);
    }
    shape_of(&full, relation_id)
}

/// How many of a `route_master`'s routes [`route_shape`] fetches.
const MAX_MASTER_MEMBERS: usize = 8;

fn shape_of(full: &[Element], relation_id: u64) -> Result<RouteShape, FetchError> {
    let relation = full
        .iter()
        .find(|e| e.kind == "relation" && e.id == relation_id)
        .ok_or(FetchError::InvalidData)?;
    let nodes: rustc_hash::FxHashMap<u64, &Element> = full
        .iter()
        .filter(|e| e.kind == "node")
        .map(|e| (e.id, e))
        .collect();
    let ways: rustc_hash::FxHashMap<u64, &Element> = full
        .iter()
        .filter(|e| e.kind == "way")
        .map(|e| (e.id, e))
        .collect();

    let mut shape = RouteShape {
        ways: Vec::new(),
        stops: Vec::new(),
    };
    for member in &relation.members {
        match member.kind.as_str() {
            "way" if member.role.is_empty() || member.role.starts_with("forward") => {
                if let Some(way) = ways.get(&member.id) {
                    let path: Vec<(f64, f64)> = way
                        .nodes
                        .iter()
                        .filter_map(|id| {
                            let node = nodes.get(id)?;
                            Some((node.lon?, node.lat?))
                        })
                        .collect();
                    if path.len() >= 2 {
                        shape.ways.push(path);
                    }
                }
            }
            "node" if member.role.starts_with("stop") || member.role.starts_with("platform") => {
                if let Some(node) = nodes.get(&member.id)
                    && let (Some(name), Some(lon), Some(lat)) =
                        (node.tag("name"), node.lon, node.lat)
                    && !shape.stops.iter().any(|(known, _, _)| *known == name)
                {
                    shape.stops.push((name, lon, lat));
                }
            }
            _ => {}
        }
    }
    Ok(shape)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relation(id: u64, tags: &[(&str, &str)]) -> Element {
        Element {
            kind: "relation".into(),
            id,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
                .collect(),
            lat: None,
            lon: None,
            nodes: Vec::new(),
            members: Vec::new(),
            geometry: Vec::new(),
        }
    }

    #[test]
    fn collapses_directions_and_skips_non_transit() {
        let routes = routes_from(vec![
            relation(
                1,
                &[
                    ("type", "route"),
                    ("route", "subway"),
                    ("ref", "12"),
                    ("network", "Métro de Paris"),
                    ("name", "Ligne 12 : A → B"),
                    ("colour", "#00643c"),
                ],
            ),
            relation(
                2,
                &[
                    ("type", "route"),
                    ("route", "subway"),
                    ("ref", "12"),
                    ("network", "Métro de Paris"),
                    ("name", "Ligne 12 : B → A"),
                ],
            ),
            relation(
                3,
                &[("type", "route"), ("route", "hiking"), ("ref", "GR 75")],
            ),
            relation(
                4,
                &[
                    ("type", "route"),
                    ("route", "subway"),
                    ("ref", "2"),
                    ("network", "Métro de Paris"),
                ],
            ),
        ]);
        let lines: Vec<&str> = routes.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["2", "12"]);
        assert_eq!(routes[1].color, Some(0x00643c));
        assert_eq!(routes[1].mode, Mode::Subway);
    }

    #[test]
    fn picks_nearest_way_with_routes() {
        let way = |id: u64, lon: f64| {
            let mut element = relation(id, &[("name", &format!("way {id}"))]);
            element.kind = "way".into();
            element.geometry = vec![LatLon { lat: 48.0, lon }, LatLon { lat: 48.001, lon }];
            element
        };
        let mut route = relation(10, &[("type", "route"), ("route", "subway"), ("ref", "1")]);
        route.members = vec![Member {
            kind: "way".into(),
            id: 2,
            role: String::new(),
        }];
        // Way 1 is nearer but carries no route; way 2 (a few meters further) does.
        let track = pick_track(vec![way(1, 2.00001), way(2, 2.0001), route], 48.0005, 2.0).unwrap();
        assert_eq!(track.name.as_deref(), Some("way 2"));
        assert_eq!(track.routes.len(), 1);
    }
}
