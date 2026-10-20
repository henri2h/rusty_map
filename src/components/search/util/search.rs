//! Forward address/place search via the [Photon](https://photon.komoot.io) geocoder (a public,
//! no-API-key OSM/Nominatim-backed instance run by Komoot), plus the raw OSM API to enrich a
//! selected result with its full OSM tags (opening hours, phone, website, …) — a second request
//! keyed by the `osm_type`/`osm_id` Photon already gives each result (see
//! [`fetch_osm_element`]). Trimmed port of `shom_client`'s `src/search.rs`: only forward search
//! and detail-tag enrichment are kept, not its WFS-specific lookups.

use serde::Deserialize;
use serde_json::Value;

use map_component::util::tiles::{FetchError, background_runtime, client};

/// One geocoded search result: enough to recenter the map, label a dropped marker, and (via
/// [`fetch_osm_element`]) fetch its full OSM tag set.
#[derive(Clone)]
pub struct SearchResult {
    /// Primary name (e.g. a POI or street name); empty for results with no name (bare
    /// addresses), in which case [`display_name`](Self::display_name) is used instead.
    pub name: String,
    /// Human-readable line for the results dropdown: name plus address parts.
    pub display_name: String,
    pub lat: f64,
    pub lon: f64,
    /// Photon's `osm_value` (falling back to `osm_key`), e.g. `restaurant`, `street` — shown as
    /// a subtitle.
    pub category: String,
    /// Photon's `osm_key` (e.g. `amenity`, `shop`, `highway`): the coarser class `category`
    /// belongs to, for picking an icon when there's none for `category` itself.
    pub osm_key: String,
    /// OSM element type, single-letter form (`N`/`W`/`R`) — [`fetch_osm_element`] accepts either
    /// this or the full word, so it's kept as Photon gives it rather than re-normalized.
    pub osm_type: String,
    pub osm_id: u64,
}

impl SearchResult {
    /// The result's name, or its address line for unnamed results (bare addresses).
    pub fn title(&self) -> &str {
        if self.name.is_empty() {
            &self.display_name
        } else {
            &self.name
        }
    }
}

/// Runs a forward search against Photon, bridged onto the shared background Tokio runtime the
/// same way tile/style fetches are (see `tiles::fetch_tile`) — freya's own executor can't drive
/// `reqwest` directly. `bias` (current map center as lon/lat) nudges results toward what's
/// currently on screen without restricting to it.
pub async fn search_address(
    query: String,
    bias: Option<(f64, f64)>,
) -> Result<Vec<SearchResult>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(search_io(query, bias).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn search_io(
    query: String,
    bias: Option<(f64, f64)>,
) -> Result<Vec<SearchResult>, FetchError> {
    let mut params = vec![
        ("q", query),
        ("limit", "8".to_string()),
        ("lang", "fr".to_string()),
    ];
    if let Some((lon, lat)) = bias {
        params.push(("lon", lon.to_string()));
        params.push(("lat", lat.to_string()));
    }
    photon(params).await
}

/// Quick addresses and places from IGN's Géoplateforme geocoder (France only, no key): it answers
/// in a fraction of a second where Photon's public instance often takes several, so the dropdown
/// shows these first and Photon's (OSM, with ids for the full details) replace them once in.
pub async fn search_ign(
    query: String,
    bias: Option<(f64, f64)>,
) -> Result<Vec<SearchResult>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let _ = tx.send(search_ign_io(query, bias).await);
    });
    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn search_ign_io(
    query: String,
    bias: Option<(f64, f64)>,
) -> Result<Vec<SearchResult>, FetchError> {
    let mut params = vec![
        ("q", query),
        ("index", "address,poi".to_string()),
        ("limit", "8".to_string()),
    ];
    if let Some((lon, lat)) = bias {
        params.push(("lon", lon.to_string()));
        params.push(("lat", lat.to_string()));
    }
    log::trace!("[http] GET https://data.geopf.fr/geocodage/search {params:?}");
    let response = client()
        .get("https://data.geopf.fr/geocodage/search")
        .query(&params)
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
    Ok(parsed["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(ign_result)
        .collect())
}

/// One IGN geocoder feature: an address (`name`/`label` strings, `type` `street`/`housenumber`/
/// …) or a BD TOPO place (`name`/`category` lists).
fn ign_result(feature: &Value) -> Option<SearchResult> {
    let lon = feature["geometry"]["coordinates"][0].as_f64()?;
    let lat = feature["geometry"]["coordinates"][1].as_f64()?;
    let p = &feature["properties"];
    let first = |value: &Value| {
        value
            .as_str()
            .or_else(|| value[0].as_str())
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
    };
    let name = first(&p["name"]).unwrap_or_default();
    let city = first(&p["city"]).unwrap_or_default();
    let display_name = match p["label"].as_str() {
        Some(label) => label.split_whitespace().collect::<Vec<_>>().join(" "),
        None if !city.is_empty() => format!("{name}, {city}"),
        None => name.clone(),
    };
    let category = first(&p["category"])
        .or_else(|| first(&p["type"]))
        .unwrap_or_default();
    Some(SearchResult {
        name,
        display_name,
        lat,
        lon,
        category,
        osm_key: String::new(),
        // No OSM element: the drawer shows the address and coordinates only.
        osm_type: String::new(),
        osm_id: 0,
    })
}

/// Places of the given kinds (`key.value` OSM tags, e.g. `amenity.restaurant`) within `bounds`
/// (`((min_lon, min_lat), (max_lon, max_lat))`, the visible map), nearest to `center` first —
/// Photon's `include` category filter, which needs no query text. Widened to a `center`-biased
/// search when nothing lies within `bounds`.
pub async fn search_category(
    tags: &'static [&'static str],
    bounds: ((f64, f64), (f64, f64)),
    center: (f64, f64),
) -> Result<Vec<SearchResult>, FetchError> {
    let include = tags
        .iter()
        .map(|tag| format!("osm.{tag}"))
        .collect::<Vec<_>>()
        .join(",");
    search_area(vec![("include", include)], bounds, center).await
}

/// Every match for `query` within `bounds` (the visible map), nearest to `center` first — what
/// pressing Enter in the search box lists, rather than the handful of best matches anywhere.
/// Widened to a `center`-biased search when nothing lies within `bounds`.
pub async fn search_in_area(
    query: String,
    bounds: ((f64, f64), (f64, f64)),
    center: (f64, f64),
) -> Result<Vec<SearchResult>, FetchError> {
    search_area(vec![("q", query)], bounds, center).await
}

async fn search_area(
    filter: Vec<(&'static str, String)>,
    bounds: ((f64, f64), (f64, f64)),
    center: (f64, f64),
) -> Result<Vec<SearchResult>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(search_area_io(filter, bounds, center).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn search_area_io(
    mut params: Vec<(&'static str, String)>,
    ((min_lon, min_lat), (max_lon, max_lat)): ((f64, f64), (f64, f64)),
    (lon, lat): (f64, f64),
) -> Result<Vec<SearchResult>, FetchError> {
    params.push(("limit", "30".to_string()));
    params.push(("lang", "fr".to_string()));
    let mut in_view = params.clone();
    in_view.push(("bbox", format!("{min_lon},{min_lat},{max_lon},{max_lat}")));
    let mut results = photon(in_view).await?;
    if results.is_empty() {
        let mut nearby = params;
        nearby.push(("lon", lon.to_string()));
        nearby.push(("lat", lat.to_string()));
        results = photon(nearby).await?;
    }
    // Photon ranks by importance, not distance.
    results.sort_by(|a, b| {
        distance_m(lat, lon, a.lat, a.lon).total_cmp(&distance_m(lat, lon, b.lat, b.lon))
    });
    Ok(results)
}

/// Great-circle distance in meters.
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * a.sqrt().asin()
}

/// `"350 m"`, `"1.2 km"`, `"14 km"`.
pub fn format_distance(meters: f64) -> String {
    if meters < 1_000.0 {
        format!("{} m", (meters / 10.0).round() as i64 * 10)
    } else if meters < 10_000.0 {
        format!("{:.1} km", meters / 1_000.0)
    } else {
        format!("{} km", (meters / 1_000.0).round() as i64)
    }
}

async fn photon(params: Vec<(&str, String)>) -> Result<Vec<SearchResult>, FetchError> {
    log::trace!("[http] GET https://photon.komoot.io/api/ {params:?}");
    let response = client()
        .get("https://photon.komoot.io/api/")
        .query(&params)
        // Required by the underlying Nominatim/OSM data usage policy, same as `tiles::client`'s
        // callers — identifies the app, not a browser UA.
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;

    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }

    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: PhotonResponse =
        serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;

    Ok(parsed.features.into_iter().map(to_result).collect())
}

/// Only the [Photon GeoJSON response](https://photon.komoot.io/) fields this app uses.
#[derive(Deserialize)]
struct PhotonResponse {
    #[serde(default)]
    features: Vec<PhotonFeature>,
}

#[derive(Deserialize)]
struct PhotonFeature {
    geometry: PhotonGeometry,
    #[serde(default)]
    properties: PhotonProperties,
}

#[derive(Deserialize)]
struct PhotonGeometry {
    /// GeoJSON order: `[lon, lat]`.
    coordinates: (f64, f64),
}

#[derive(Deserialize, Default)]
struct PhotonProperties {
    name: Option<String>,
    street: Option<String>,
    housenumber: Option<String>,
    city: Option<String>,
    postcode: Option<String>,
    osm_key: Option<String>,
    osm_value: Option<String>,
    osm_type: Option<String>,
    osm_id: Option<u64>,
}

fn to_result(feature: PhotonFeature) -> SearchResult {
    let (lon, lat) = feature.geometry.coordinates;
    let p = feature.properties;
    let name = p.name.unwrap_or_default();

    let address_line = [p.housenumber, p.street, p.postcode, p.city]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");

    let display_name = match (name.is_empty(), address_line.is_empty()) {
        (false, false) => format!("{name}, {address_line}"),
        (false, true) => name.clone(),
        (true, _) => address_line,
    };

    SearchResult {
        name,
        display_name,
        lat,
        lon,
        category: p.osm_value.or(p.osm_key.clone()).unwrap_or_default(),
        osm_key: p.osm_key.unwrap_or_default(),
        osm_type: p.osm_type.unwrap_or_default(),
        osm_id: p.osm_id.unwrap_or_default(),
    }
}

/// Fetches an OSM element's raw tags plus its edit metadata (timestamp/editor) in a single
/// request, keyed by the `osm_type`/`osm_id` a [`SearchResult`] or a decoded tile feature id
/// (see `poi_lookup::decode_feature_id`) already carries. The OSM API's own
/// `/api/0.6/{kind}/{id}.json` (not Nominatim's `/details`) returns both the element's complete
/// `tags` object and its `timestamp`/`user` in one response — strictly better than the old
/// two-request approach (Nominatim `/details` for tags, this same endpoint again for meta): one
/// fewer round trip, and raw OSM tags rather than Nominatim's filtered `extratags` subset. Only
/// [`fetch_reverse`] (used when no id is known upfront) still goes through Nominatim, since
/// discovering the id in the first place needs it.
pub async fn fetch_osm_element(
    osm_type: String,
    osm_id: u64,
) -> Result<Vec<(String, String)>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(fetch_osm_element_io(osm_type, osm_id).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_osm_element_io(
    osm_type: String,
    osm_id: u64,
) -> Result<Vec<(String, String)>, FetchError> {
    let kind = match osm_type.as_str() {
        "N" | "node" => "node",
        "W" | "way" => "way",
        "R" | "relation" => "relation",
        _ => return Err(FetchError::InvalidData),
    };

    log::trace!("[http] GET https://api.openstreetmap.org/api/0.6/{kind}/{osm_id}.json");
    let response = client()
        .get(format!(
            "https://api.openstreetmap.org/api/0.6/{kind}/{osm_id}.json"
        ))
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;

    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }

    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
    let element = &parsed["elements"][0];

    let mut tags: Vec<(String, String)> = element["tags"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
        .collect();

    // See `push_osm_identity`'s doc comment — lets `place::enrich_from_tags` build a "View on
    // OpenStreetMap" link the same way regardless of which enrichment path produced `tags`.
    push_osm_identity(&mut tags, kind, &osm_id.to_string());
    // Piggybacked on the same response instead of a separate `fetch_osm_element_meta` call — see
    // `place::enrich_from_tags`'s use of these pseudo-tags.
    if let (Some(timestamp), Some(user)) = (element["timestamp"].as_str(), element["user"].as_str())
    {
        tags.push(("_last_edited_timestamp".to_string(), timestamp.to_string()));
        tags.push(("_last_edited_user".to_string(), user.to_string()));
    }

    Ok(tags)
}

/// Reverse-geocodes a point via Nominatim's `/reverse` — used to enrich a tapped POI, which
/// (unlike a search result) has no OSM id of its own in our vector tiles to look up directly.
/// `zoom` steers which OSM feature granularity Nominatim resolves to (building/POI-level at
/// high zoom, city/region-level at low zoom); Nominatim only accepts up to 18.
pub async fn fetch_reverse(
    lat: f64,
    lon: f64,
    zoom: u32,
) -> Result<Vec<(String, String)>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(fetch_reverse_io(lat, lon, zoom.min(18)).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_reverse_io(
    lat: f64,
    lon: f64,
    zoom: u32,
) -> Result<Vec<(String, String)>, FetchError> {
    log::trace!(
        "[http] GET https://nominatim.openstreetmap.org/reverse lat={lat} lon={lon} zoom={zoom}"
    );
    let response = client()
        .get("https://nominatim.openstreetmap.org/reverse")
        .query(&[
            ("lat", lat.to_string()),
            ("lon", lon.to_string()),
            ("zoom", zoom.to_string()),
            ("format", "jsonv2".to_string()),
            ("addressdetails", "1".to_string()),
            ("extratags", "1".to_string()),
            // Without this, a point that sits on/near a street way (as a POI icon's anchor
            // often does relative to the road centerline) can resolve to that street instead
            // of the POI itself, even at zoom 18 — biasing to the `poi` layer fixes the
            // McDonald's-on-Champs-Élysées-returning-street-tags case.
            ("layer", "poi".to_string()),
        ])
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;

    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }

    let text = response.text().await.map_err(FetchError::Network)?;
    let mut tags = extratags_from_json(&text)?;
    // `/reverse` doesn't already know its own result's identity the way `fetch_osm_element_io`
    // does — pull it from the response's own top-level `osm_type`/`osm_id` (Nominatim always
    // includes these, regardless of `format`).
    let parsed: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
    if let (Some(osm_type), Some(osm_id)) = (
        parsed["osm_type"].as_str(),
        parsed["osm_id"].as_u64().map(|id| id.to_string()),
    ) {
        push_osm_identity(&mut tags, osm_type, &osm_id);
    }
    Ok(tags)
}

/// The address at a point, from Nominatim's reverse geocoder.
#[derive(Debug, Clone, PartialEq)]
pub struct Address {
    /// Short form for a title: `"12 Rue de Rivoli"`, or the street, or the nearest named place.
    pub title: String,
    /// The full address line Nominatim gives (`display_name`).
    pub line: String,
}

/// Reverse-geocodes `lat`/`lon` to the nearest address (building level), or `None` where there
/// is none (open sea).
pub async fn fetch_address(lat: f64, lon: f64) -> Result<Option<Address>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(fetch_address_io(lat, lon).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_address_io(lat: f64, lon: f64) -> Result<Option<Address>, FetchError> {
    log::trace!(
        "[http] GET https://nominatim.openstreetmap.org/reverse lat={lat} lon={lon} (address)"
    );
    let response = client()
        .get("https://nominatim.openstreetmap.org/reverse")
        .query(&[
            ("lat", lat.to_string()),
            ("lon", lon.to_string()),
            ("zoom", "18".to_string()),
            ("format", "jsonv2".to_string()),
            ("addressdetails", "1".to_string()),
            // Addresses and streets, not the POI or landuse polygon the point happens to be in.
            ("layer", "address".to_string()),
        ])
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;

    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
    Ok(address_from_reverse(&parsed))
}

/// Pulls an [`Address`] out of a `/reverse?format=jsonv2&addressdetails=1` response. A point
/// with nothing nearby gets `{"error": "Unable to geocode"}`.
fn address_from_reverse(parsed: &Value) -> Option<Address> {
    let line = parsed["display_name"].as_str()?.to_string();
    let part = |key: &str| {
        parsed["address"][key]
            .as_str()
            .filter(|value| !value.is_empty())
    };
    let street = ["road", "pedestrian", "footway", "path", "square", "place"]
        .into_iter()
        .find_map(part);
    let title = match (part("house_number"), street) {
        (Some(number), Some(street)) => format!("{number} {street}"),
        (None, Some(street)) => street.to_string(),
        _ => parsed["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .or_else(|| line.split(',').next())
            .unwrap_or(&line)
            .trim()
            .to_string(),
    };
    Some(Address { title, line })
}

/// `/details`' `osmtype` param and `/reverse`'s own `osm_type` field disagree on shape (single
/// letter `"W"` vs. full word `"way"`) — normalized here to the full word `osm.org/<kind>/<id>`
/// URLs use, and stashed as a pseudo-tag (`_osm_type`/`_osm_id`, never real OSM keys) so
/// `place::enrich_from_tags` can build a "View on OpenStreetMap" link the same way regardless of
/// which endpoint produced `tags`.
fn push_osm_identity(tags: &mut Vec<(String, String)>, osm_type: &str, osm_id: &str) {
    let kind = match osm_type {
        "N" | "node" => "node",
        "W" | "way" => "way",
        "R" | "relation" => "relation",
        _ => return,
    };
    tags.push(("_osm_type".to_string(), kind.to_string()));
    tags.push(("_osm_id".to_string(), osm_id.to_string()));
}

/// Pulls the `extratags` object (shared shape between `/details` and `/reverse` responses) out
/// of a raw Nominatim JSON body.
fn extratags_from_json(text: &str) -> Result<Vec<(String, String)>, FetchError> {
    let parsed: Value = serde_json::from_str(text).map_err(|_| FetchError::InvalidData)?;
    Ok(parsed
        .get("extratags")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
        .collect())
}

/// When this OSM element was last edited, and by whom — straight from the OSM API itself (not
/// Nominatim, which doesn't expose edit history), since that's the only source for it. `kind` is
/// `"node"`/`"way"`/`"relation"` (see [`push_osm_identity`]'s normalization).
pub async fn fetch_osm_element_meta(
    kind: String,
    osm_id: u64,
) -> Result<Option<(String, String)>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(fetch_osm_element_meta_io(kind, osm_id).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_osm_element_meta_io(
    kind: String,
    osm_id: u64,
) -> Result<Option<(String, String)>, FetchError> {
    log::trace!("[http] GET https://api.openstreetmap.org/api/0.6/{kind}/{osm_id}.json");
    let response = client()
        .get(format!(
            "https://api.openstreetmap.org/api/0.6/{kind}/{osm_id}.json"
        ))
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;

    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }

    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
    let element = &parsed["elements"][0];

    let (Some(timestamp), Some(user)) = (
        element["timestamp"].as_str().map(str::to_string),
        element["user"].as_str().map(str::to_string),
    ) else {
        return Ok(None);
    };
    Ok(Some((timestamp, user)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_title_prefers_number_and_street() {
        let parsed: Value = serde_json::from_str(
            r#"{"name": "", "display_name": "12, Rue de Rivoli, Paris, 75004, France",
                "address": {"house_number": "12", "road": "Rue de Rivoli", "city": "Paris"}}"#,
        )
        .unwrap();
        assert_eq!(
            address_from_reverse(&parsed),
            Some(Address {
                title: "12 Rue de Rivoli".to_string(),
                line: "12, Rue de Rivoli, Paris, 75004, France".to_string(),
            })
        );
    }

    #[test]
    fn address_title_falls_back_to_first_part() {
        let parsed: Value = serde_json::from_str(
            r#"{"display_name": "Ormoy, Auxerre, Yonne, France", "address": {"village": "Ormoy"}}"#,
        )
        .unwrap();
        assert_eq!(address_from_reverse(&parsed).unwrap().title, "Ormoy");
    }

    #[test]
    fn formats_distances() {
        assert_eq!(format_distance(343.0), "340 m");
        assert_eq!(format_distance(1_234.0), "1.2 km");
        assert_eq!(format_distance(14_400.0), "14 km");
        let paris_lyon = distance_m(48.8566, 2.3522, 45.764, 4.8357);
        assert!((390_000.0..395_000.0).contains(&paris_lyon), "{paris_lyon}");
    }

    #[test]
    fn reads_ign_addresses_and_places() {
        let street: Value = serde_json::from_str(
            r#"{"geometry": {"coordinates": [5.449173, 43.526816]}, "properties": {"label":
                "Cours  Mirabeau 13100 Aix-en-Provence", "name": "Cours  Mirabeau", "type": "street",
                "city": "Aix-en-Provence"}}"#,
        )
        .unwrap();
        let street = ign_result(&street).unwrap();
        assert_eq!(street.name, "Cours Mirabeau");
        assert_eq!(street.display_name, "Cours Mirabeau 13100 Aix-en-Provence");
        assert_eq!(street.category, "street");
        let place: Value = serde_json::from_str(
            r#"{"geometry": {"coordinates": [5.4, 43.5]}, "properties": {"name": ["Gare"],
                "category": ["gare"], "city": ["Aix-en-Provence"]}}"#,
        )
        .unwrap();
        assert_eq!(
            ign_result(&place).unwrap().display_name,
            "Gare, Aix-en-Provence"
        );
    }

    #[test]
    fn nothing_to_geocode() {
        let parsed: Value = serde_json::from_str(r#"{"error": "Unable to geocode"}"#).unwrap();
        assert_eq!(address_from_reverse(&parsed), None);
    }
}
