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
        category: p.osm_value.or(p.osm_key).unwrap_or_default(),
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
