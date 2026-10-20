use std::{
    cell::RefCell,
    collections::HashSet,
    future::Future,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use freya::prelude::*;
use rustc_hash::FxHashMap;

use map_component::{
    TappedFeature,
    util::{renderer::Properties, tiles::FetchError, viewport::lonlat_to_world},
};

use crate::components::{
    charging::api::Charger,
    favorites::Favorite,
    search::util::search::{self, SearchResult},
};

/// `(mapped rows, unmapped tags)` — see [`details_from_properties`]/[`details_from_tags`].
type DetailRows = (Vec<(String, String)>, Vec<(String, String)>);

/// Everything a background enrichment fetch (`search::fetch_osm_element`/`search::fetch_reverse` +
/// [`enrich_from_tags`]) can add to a [`SelectedPlace`] once it lands.
#[derive(Clone, Default)]
pub struct EnrichedDetails {
    pub rows: Vec<(String, String)>,
    pub raw_tags: Vec<(String, String)>,
    /// Clickable rows — `(label, display text, URL)` — opened in the system browser by the
    /// drawer, not shown as plain text like [`Self::rows`].
    pub links: Vec<(String, String, String)>,
    /// A preview image from the place's Wikidata "image" claim (P18).
    pub image: Option<bytes::Bytes>,
    /// A better title than the placeholder shown meanwhile (a dropped pin's street address).
    pub title: Option<String>,
}

/// Identifies one enrichment fetch for [`EnrichCache`] purposes: a search result's stable OSM
/// id, or a POI tap's reverse-geocode point quantized to ~0.1m (`lat`/`lon` × 1e7, matching the
/// precision `labels::icon_identity` already uses for the same kind of float-key quantization)
/// plus the zoom bucket that was reverse-geocoded (`search::fetch_reverse`'s `zoom` param changes
/// which feature radius resolves, so it's part of the cache identity too).
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum EnrichKey {
    Details(String, u64),
    /// Only [`online_extras`], for a POI whose tags came from the tile itself.
    Extras(String, u64),
    Reverse(i64, i64, u32),
    /// A dropped pin's address, at the same ~1 m quantization as [`format_coordinates`] shows.
    Address(i64, i64),
}

impl EnrichKey {
    pub fn reverse(lat: f64, lon: f64, zoom: u32) -> Self {
        EnrichKey::Reverse((lat * 1e7).round() as i64, (lon * 1e7).round() as i64, zoom)
    }
}

/// Cache of already-fetched+enriched results, shared across every selection for the app's
/// lifetime (see `PlaceController::new`) — re-selecting the same
/// search result or tapping the same POI again skips the Nominatim/Wikidata/Wikipedia round
/// trips entirely instead of re-fetching.
pub type EnrichCache = Rc<RefCell<FxHashMap<EnrichKey, EnrichedDetails>>>;

pub fn new_enrich_cache() -> EnrichCache {
    Rc::new(RefCell::new(FxHashMap::default()))
}

/// Runs `fetch_tags` (only on a cache miss) and [`enrich_from_tags`], caching the result under
/// `key`. `fetch_tags` is a closure (not an already-awaited `Future`) specifically so a cache hit
/// skips the network call entirely rather than just skipping the enrichment step. A failed
/// fetch is not cached, so re-selecting the place retries instead of staying empty for the rest
/// of the session.
pub async fn enrich_cached<Fut>(
    cache: &EnrichCache,
    key: EnrichKey,
    fetch_tags: impl FnOnce() -> Fut,
) -> EnrichedDetails
where
    Fut: Future<Output = Result<Vec<(String, String)>, FetchError>>,
{
    if let Some(cached) = cache.borrow().get(&key).cloned() {
        return cached;
    }

    let tags = match fetch_tags().await {
        Ok(tags) => tags,
        Err(err) => {
            log::warn!("[place] enrichment fetch failed: {err:?}");
            return EnrichedDetails::default();
        }
    };
    let result = enrich_from_tags(&tags).await;
    cache.borrow_mut().insert(key, result.clone());
    result
}

/// Place-selection state owned by the app (not the map): the place currently shown in the search
/// drawer — written by a POI tap ([`Self::select_tap`]) or a picked search result
/// ([`Self::select_search_result`]), read by the drawer — plus the shared enrichment cache.
#[derive(Clone)]
pub struct PlaceController {
    pub selected_place: State<Option<SelectedPlace>>,
    pub enrich_cache: EnrichCache,
}

impl PartialEq for PlaceController {
    fn eq(&self, other: &Self) -> bool {
        self.selected_place == other.selected_place
            && Rc::ptr_eq(&self.enrich_cache, &other.enrich_cache)
    }
}

impl PlaceController {
    /// A hook, like `use_state` — call it directly from a component's `render()` body.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            selected_place: use_state::<Option<SelectedPlace>>(|| None),
            enrich_cache: use_hook(new_enrich_cache),
        }
    }

    /// Shows a tapped POI right away, then fills in its full OSM tags/Wikipedia/Wikidata details
    /// in the background (see [`enrich_tap`]).
    pub fn select_tap(&mut self, tap: TappedFeature, platform: PlatformWindow) {
        let place = place_from_tap(&tap);
        let cache = self.enrich_cache.clone();
        self.select_and_enrich(
            place,
            platform,
            async move { enrich_tap(&cache, &tap).await },
            None,
        );
    }

    /// Shows a picked search result right away (Address/Coordinates rows plus a dropped pin),
    /// then fills in its full OSM tags the same way as [`Self::select_tap`]. The OSM API's
    /// `/api/0.6/{kind}/{id}.json` gives the place's full raw tags (opening hours, phone,
    /// website, …) plus edit metadata in one request (see `search::fetch_osm_element`).
    pub fn select_search_result(&mut self, result: &SearchResult, platform: PlatformWindow) {
        let place = place_from_search_result(result);
        let cache = self.enrich_cache.clone();
        let (osm_type, osm_id) = (result.osm_type.clone(), result.osm_id);
        self.select_and_enrich(
            place,
            platform,
            async move {
                let key = EnrichKey::Details(osm_type.clone(), osm_id);
                enrich_cached(&cache, key, || search::fetch_osm_element(osm_type, osm_id)).await
            },
            None,
        );
    }

    /// Shows a point picked on the map (no POI there) as a dropped pin with its coordinates,
    /// then fills in the address at that point (see `search::fetch_address`). The drawer's title
    /// is the search field's text: `query` gets the placeholder now and the street once known.
    pub fn select_point(
        &mut self,
        lat: f64,
        lon: f64,
        mut query: State<String>,
        platform: PlatformWindow,
    ) {
        query.set(DROPPED_PIN.to_string());
        let place = SelectedPlace {
            id: next_place_id(),
            title: DROPPED_PIN.to_string(),
            subtitle: String::new(),
            details: vec![("Coordinates".to_string(), format_coordinates(lat, lon))],
            raw_tags: Vec::new(),
            links: Vec::new(),
            marker_world: Some(lonlat_to_world(lon, lat)),
            loading: true,
            image: None,
            transit_stop: None,
            coords: Some((lat, lon)),
            charger: None,
        };
        let cache = self.enrich_cache.clone();
        let enrich = async move {
            let key = EnrichKey::Address((lat * 1e5).round() as i64, (lon * 1e5).round() as i64);
            if let Some(cached) = cache.borrow().get(&key).cloned() {
                return cached;
            }
            match search::fetch_address(lat, lon).await {
                Ok(address) => {
                    let details = match address {
                        Some(address) => EnrichedDetails {
                            rows: vec![("Address".to_string(), address.line)],
                            title: Some(address.title),
                            ..Default::default()
                        },
                        // Open sea, mostly.
                        None => EnrichedDetails::default(),
                    };
                    cache.borrow_mut().insert(key, details.clone());
                    details
                }
                Err(err) => {
                    log::warn!("[place] address lookup failed: {err:?}");
                    EnrichedDetails::default()
                }
            }
        };
        self.select_and_enrich(place, platform, enrich, Some(query));
    }

    /// Shows a saved favorite, pinned, then its full OSM details when it is an OSM element.
    pub fn select_favorite(&mut self, favorite: &Favorite, platform: PlatformWindow) {
        let (lat, lon) = (favorite.lat, favorite.lon);
        let place = SelectedPlace {
            id: next_place_id(),
            title: favorite.name.clone(),
            subtitle: favorite.kind.clone(),
            details: vec![("Coordinates".to_string(), format_coordinates(lat, lon))],
            raw_tags: Vec::new(),
            links: Vec::new(),
            marker_world: Some(lonlat_to_world(lon, lat)),
            loading: favorite.osm.is_some(),
            image: None,
            transit_stop: favorite.stop.then_some((lat, lon)),
            coords: Some((lat, lon)),
            charger: favorite.charger.clone(),
        };
        let Some((osm_type, osm_id)) = favorite.osm.clone() else {
            self.selected_place.set(Some(place));
            return;
        };
        let cache = self.enrich_cache.clone();
        self.select_and_enrich(
            place,
            platform,
            async move {
                let key = EnrichKey::Details(osm_type.clone(), osm_id);
                enrich_cached(&cache, key, || search::fetch_osm_element(osm_type, osm_id)).await
            },
            None,
        );
    }

    /// Shows a fast charger tapped on the map, from the charger list itself (no OSM lookup);
    /// its live charge points load in the drawer.
    pub fn select_charger(&mut self, charger: &Charger) {
        let mut details = Vec::new();
        if let Some(operator) = &charger.operator {
            details.push(("Operator".to_string(), operator.clone()));
        }
        let plugs = if charger.plugs.is_empty() {
            String::new()
        } else {
            format!(" · {}", charger.plugs.join(", "))
        };
        details.push((
            "Charge points".to_string(),
            format!("{}{plugs}", charger.points),
        ));
        if let Some(access) = &charger.access {
            details.push(("Access".to_string(), access.clone()));
        }
        if charger.card_payment {
            details.push(("Payment".to_string(), "Bank card accepted".to_string()));
        }
        if let Some(price) = &charger.price {
            details.push(("Price".to_string(), price.clone()));
        }
        details.push((
            "Coordinates".to_string(),
            format_coordinates(charger.lat, charger.lon),
        ));
        self.selected_place.set(Some(SelectedPlace {
            id: next_place_id(),
            title: charger.name.clone(),
            subtitle: format!("Fast charger · up to {:.0} kW", charger.max_kw),
            details,
            raw_tags: Vec::new(),
            links: Vec::new(),
            // The charger's own marker is drawn, highlighted.
            marker_world: None,
            loading: false,
            image: None,
            transit_stop: None,
            coords: Some((charger.lat, charger.lon)),
            charger: Some(charger.id.clone()),
        }));
    }

    pub fn clear(&mut self) {
        if self.selected_place.peek().is_some() {
            self.selected_place.set(None);
        }
    }

    fn select_and_enrich(
        &mut self,
        place: SelectedPlace,
        platform: PlatformWindow,
        enrich: impl Future<Output = EnrichedDetails> + 'static,
        mut title_field: Option<State<String>>,
    ) {
        let id = place.id;
        self.selected_place.set(Some(place));
        let mut selected = self.selected_place;
        // `spawn_in_window`, not `spawn`: both callers are event handlers (a POI tap, or a result
        // row press — and that row is removed from the tree by the selection itself), so there's
        // no component scope that outlives the fetch to attach a plain `spawn` task to.
        spawn_in_window(async move {
            let enriched = enrich.await;
            // Matched by `id`, not by title or position: two POIs can share a name (two
            // branches of the same chain), and the user may have picked another one meanwhile.
            let current = selected
                .peek()
                .as_ref()
                .filter(|place| place.id == id)
                .cloned();
            let Some(mut updated) = current else {
                return;
            };
            if let (Some(field), Some(title)) = (title_field.as_mut(), &enriched.title) {
                field.set(title.clone());
            }
            updated.apply(enriched);
            selected.set(Some(updated));
            platform.send(UserEvent::RequestRedraw);
        });
    }
}

/// Title of a point picked on the map until its address is known.
const DROPPED_PIN: &str = "Dropped pin";

/// Source of [`SelectedPlace::id`].
static NEXT_PLACE_ID: AtomicU64 = AtomicU64::new(1);

fn next_place_id() -> u64 {
    NEXT_PLACE_ID.fetch_add(1, Ordering::Relaxed)
}

/// The tapped POI's OSM tags from the tile's `poi_details` layer, as the OSM API would give
/// them (plus the `_osm_type`/`_osm_id` identity), or `None` when the tile source has no such
/// layer.
fn tile_tags(tap: &TappedFeature) -> Option<Vec<(String, String)>> {
    let details = tap.details.as_ref()?;
    let mut tags: Vec<(String, String)> = details
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "osm_type" | "osm_id"))
        .filter_map(|(key, value)| Some((key.clone(), value_to_string(value)?)))
        .collect();
    if let (Some(kind), Some(id)) = (
        details.get("osm_type").and_then(value_to_string),
        details.get("osm_id").and_then(value_to_string),
    ) {
        tags.push(("_osm_type".to_string(), kind));
        tags.push(("_osm_id".to_string(), id));
    }
    Some(tags)
}

/// Immediate place for a tapped POI: from the tile's own tags (fully detailed when the tile has
/// `poi_details`). No `marker_world` — the tapped sprite/label is already drawn on the map, so
/// no extra pin is needed.
pub fn place_from_tap(tap: &TappedFeature) -> SelectedPlace {
    let mut place = place_from_tap_properties(tap);
    if let Some(tags) = tile_tags(tap) {
        let loading = place.loading;
        place.apply(local_details(&tags));
        // Still loading the Wikipedia/Wikidata extras.
        place.loading = loading;
    }
    place
}

fn place_from_tap_properties(tap: &TappedFeature) -> SelectedPlace {
    let (mut details, raw_tags) = details_from_properties(&tap.properties);
    details.push((
        "Coordinates".to_string(),
        format_coordinates(tap.lat, tap.lon),
    ));
    SelectedPlace {
        id: next_place_id(),
        title: tap.title.clone(),
        subtitle: poi_kind(&tap.properties),
        details,
        raw_tags,
        links: Vec::new(),
        marker_world: None,
        loading: true,
        image: None,
        transit_stop: is_transit_poi(&tap.properties).then_some((tap.lat, tap.lon)),
        coords: Some((tap.lat, tap.lon)),
        charger: None,
    }
}

/// Immediate (not-yet-enriched) place for a picked search result, pinned at its coordinates.
pub fn place_from_search_result(result: &SearchResult) -> SelectedPlace {
    let (lat, lon) = (result.lat, result.lon);
    SelectedPlace {
        id: next_place_id(),
        title: result.title().to_string(),
        subtitle: humanize(&result.category),
        details: vec![
            ("Address".to_string(), result.display_name.clone()),
            ("Coordinates".to_string(), format_coordinates(lat, lon)),
        ],
        raw_tags: Vec::new(),
        links: Vec::new(),
        marker_world: Some(lonlat_to_world(lon, lat)),
        loading: true,
        image: None,
        transit_stop: TRANSIT_KINDS
            .contains(&result.category.as_str())
            .then_some((lat, lon)),
        coords: Some((lat, lon)),
        charger: None,
    }
}

/// `"48.85837, 2.29448"`: decimal degrees, latitude first, as GPS apps and map searches take
/// them. Five decimals is ~1 m.
pub fn format_coordinates(lat: f64, lon: f64) -> String {
    format!("{lat:.5}, {lon:.5}")
}

/// OSM values (`highway=bus_stop`, `railway=station`, `public_transport=platform`, …) marking a
/// place where a timetable applies — OpenMapTiles `subclass`, shortbread `kind`, or Photon's
/// `osm_value`.
const TRANSIT_KINDS: &[&str] = &[
    "bus_stop",
    "bus_station",
    "station",
    "halt",
    "tram_stop",
    "subway",
    "stop",
    "stop_position",
    "platform",
    "stop_area",
    "ferry_terminal",
];

/// `true` for a tapped stop/station POI. OpenMapTiles puts every railway stop (station, halt,
/// metro, tram) under `class=railway` and bus stops under `class=bus`.
fn is_transit_poi(properties: &Properties) -> bool {
    let is = |key: &str, values: &[&str]| {
        properties
            .get(key)
            .and_then(value_to_string)
            .is_some_and(|value| values.contains(&value.as_str()))
    };
    is("class", &["railway", "bus"]) || is("subclass", TRANSIT_KINDS) || is("kind", TRANSIT_KINDS)
}

/// Fetches the rich OSM tags (opening hours, phone, website, Wikipedia, …) for a tapped POI. Uses
/// the OSM id carried by the tile when there is one; otherwise falls back to reverse-geocoding,
/// which can resolve to the wrong nearby feature (e.g. a street) instead of the POI itself.
pub async fn enrich_tap(cache: &EnrichCache, tap: &TappedFeature) -> EnrichedDetails {
    // The tile already had this POI's tags (see `tile_tags`), shown right away: only the
    // Wikipedia/Wikidata extras are left to fetch, and the OSM API isn't needed at all.
    if let Some(tags) = tile_tags(tap) {
        let (kind, id) = tap.osm_identity.clone().unwrap_or_default();
        let key = EnrichKey::Extras(kind, id);
        if let Some(cached) = cache.borrow().get(&key).cloned() {
            return cached;
        }
        let extras = online_extras(&tags, false).await;
        cache.borrow_mut().insert(key, extras.clone());
        return extras;
    }
    match tap.osm_identity.clone() {
        Some((osm_type, osm_id)) => {
            let key = EnrichKey::Details(osm_type.clone(), osm_id);
            enrich_cached(cache, key, || search::fetch_osm_element(osm_type, osm_id)).await
        }
        None => {
            // Always 18 (Nominatim's max): this is always a POI tap, so we want building/POI-level
            // resolution, not street/neighborhood granularity.
            let (lat, lon, zoom) = (tap.lat, tap.lon, 18);
            log::debug!(
                "[poi] {:?} at ({lat}, {lon}) has no feature id from its own tile — falling back to reverse-geocoding, which may return the wrong feature.",
                tap.title
            );
            let key = EnrichKey::reverse(lat, lon, zoom);
            enrich_cached(cache, key, || search::fetch_reverse(lat, lon, zoom)).await
        }
    }
}

/// A single selected place shown in the info drawer — either a POI tapped on the map (its icon
/// is already drawn by the tile it came from, so no extra marker is needed) or a geocoded search
/// result (drawn as a dropped pin at `marker_world`, since nothing was already on screen for it).
#[derive(Clone)]
pub struct SelectedPlace {
    /// Unique per selection, so a late enrichment result is only ever applied to the selection
    /// that started it (see [`PlaceController::select_and_enrich`]).
    pub id: u64,
    pub title: String,
    pub subtitle: String,
    /// Extra label/value rows shown in the drawer body — a POI's raw tags (address, opening
    /// hours, phone, …) or a search result's address/coordinates.
    pub details: Vec<(String, String)>,
    /// Tags present on the place but not mapped to one of the curated [`RICH_DETAIL_KEYS`]/
    /// [`DETAIL_KEYS`] rows above — shown in their own "More tags" section (raw `key: value`,
    /// same spirit as `shom_client`'s debug tag dump) instead of silently dropped. See
    /// [`details_from_tags`]/[`details_from_properties`].
    pub raw_tags: Vec<(String, String)>,
    /// Clickable rows (website, Wikipedia, Wikivoyage, …) — see [`EnrichedDetails::links`].
    pub links: Vec<(String, String, String)>,
    /// Zoom-independent world position (see `Viewport::screen_to_world`) for a marker drawn each
    /// frame by the paint loop; `None` for an already-visible POI icon.
    pub marker_world: Option<(f64, f64)>,
    /// `true` from selection until the background enrichment fetch finishes — the drawer shows a
    /// loading hint while this is set, since that fetch is a real network round trip with no
    /// local fallback.
    pub loading: bool,
    /// A preview image from the place's Wikidata "image" claim (P18), when it has a `wikidata`
    /// tag and that entity sets one — see `wikipedia::fetch_wikidata_info`/
    /// `wikipedia::fetch_commons_thumbnail`, same source `shom_client`'s popup image uses.
    pub image: Option<bytes::Bytes>,
    /// Position to look up departures at, when the place is a public-transport stop or station
    /// (see `transit::Departures`).
    pub transit_stop: Option<(f64, f64)>,
    /// `(lat, lon)`, e.g. to plan a journey there.
    pub coords: Option<(f64, f64)>,
    /// A fast charger's `id_station_itinerance`: the drawer shows its live charge points
    /// (`charging::ChargerLive`).
    pub charger: Option<String>,
}

impl SelectedPlace {
    /// Merges a finished enrichment into this place. Rows/tags already present (the tile's own
    /// `opening_hours`/`phone`, or a search result's Photon-built address) are replaced by the
    /// OSM element's value rather than shown twice.
    fn apply(&mut self, enriched: EnrichedDetails) {
        upsert(&mut self.details, enriched.rows);
        upsert(&mut self.raw_tags, enriched.raw_tags);
        for link in enriched.links {
            if !self.links.iter().any(|existing| existing.2 == link.2) {
                self.links.push(link);
            }
        }
        if enriched.image.is_some() {
            self.image = enriched.image;
        }
        if let Some(title) = enriched.title {
            self.title = title;
        }
        self.loading = false;
    }
}

/// Diff key for a drawer section that must start over for each new selection (fresh state,
/// fresh fetch). Mixes in the section's name: several such sections sit side by side in the
/// drawer, and freya rejects sibling components sharing a key.
pub fn section_key(section: &str, place_id: u64) -> DiffKey {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    (section, place_id).hash(&mut hasher);
    DiffKey::U64(hasher.finish())
}

/// Replaces the value of every `(key, _)` in `rows` that `new` also has, in place; appends the rest.
fn upsert(rows: &mut Vec<(String, String)>, new: Vec<(String, String)>) {
    for (key, value) in new {
        match rows.iter_mut().find(|(existing, _)| *existing == key) {
            Some(row) => row.1 = value,
            None => rows.push((key, value)),
        }
    }
}

/// `"fast_food"` → `"Fast food"`: OSM values are lowercase snake_case identifiers.
pub fn humanize(value: &str) -> String {
    let text = value.replace('_', " ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A human-readable subtitle for a tapped POI/label feature — its OSM `subclass` (falling back
/// to `class`), e.g. "Restaurant", not the sprite sheet's own icon identifier (e.g.
/// `restaurant_11`), which means nothing to a user.
pub fn poi_kind(properties: &Properties) -> String {
    ["subclass", "class"]
        .into_iter()
        .find_map(|key| properties.get(key).and_then(value_to_string))
        .map(|kind| humanize(&kind))
        .unwrap_or_default()
}

/// The `addr:*` parts folded into one "Address" row by [`address_line`], in display order.
const ADDRESS_PARTS: &[&str] = &["housenumber", "street", "postcode", "city"];

/// Joins address parts into one line, e.g. "12 Rue de Rivoli, 75001 Paris". `part` looks one up
/// by its [`ADDRESS_PARTS`] name.
fn address_line(part: impl Fn(&str) -> Option<String>) -> Option<String> {
    let join = |keys: [&str; 2]| {
        keys.into_iter()
            .filter_map(&part)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let line = [join(["housenumber", "street"]), join(["postcode", "city"])]
        .into_iter()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    (!line.is_empty()).then_some(line)
}

/// Tag keys worth surfacing in the drawer for a tapped POI/label feature, in display order
/// (after the [`address_line`] row). `class`/`subclass` aren't here: they're the drawer's
/// subtitle already (see [`poi_kind`]).
const DETAIL_KEYS: &[(&str, &str)] = &[
    ("amenity", "Amenity"),
    ("shop", "Shop"),
    ("cuisine", "Cuisine"),
    ("opening_hours", "Opening hours"),
    ("phone", "Phone"),
];

/// Pulls the [`DETAIL_KEYS`] worth showing out of a tapped feature's raw MVT properties, in
/// order, skipping absent/empty ones. Address parts are checked under both OpenMapTiles'
/// (`addr:street`) and shortbread's (`street`) naming (see `renderer::POI_KIND_KEYS`'s doc
/// comment on why both schemas need checking). The second element is every other tag on the
/// feature, for the drawer's "More tags" fallback section.
pub fn details_from_properties(properties: &Properties) -> DetailRows {
    let address = address_line(|part| {
        properties
            .get(&format!("addr:{part}"))
            .and_then(value_to_string)
            .or_else(|| properties.get(part).and_then(value_to_string))
    });
    let rows: Vec<(String, String)> = address
        .map(|line| ("Address".to_string(), line))
        .into_iter()
        .chain(DETAIL_KEYS.iter().filter_map(|(key, label)| {
            let value = value_to_string(properties.get(*key)?)?;
            Some((label.to_string(), value))
        }))
        .collect();

    let raw: Vec<(String, String)> = properties
        .iter()
        .filter(|(k, _)| {
            let key = k.as_str();
            let is_address = ADDRESS_PARTS
                .iter()
                .any(|part| key == *part || key.strip_prefix("addr:") == Some(part));
            !is_address
                && !matches!(key, "class" | "subclass")
                && !DETAIL_KEYS.iter().any(|(known, _)| *known == key)
                && !is_noise_tag(key)
        })
        .filter_map(|(k, v)| Some((k.clone(), value_to_string(v)?)))
        .collect();
    if !raw.is_empty() {
        log::debug!(
            "[place] {} tag(s) on this feature aren't mapped to a drawer row, shown under \"More tags\": {}",
            raw.len(),
            raw.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    (rows, raw)
}

fn value_to_string(value: &mvt_reader::feature::Value) -> Option<String> {
    use mvt_reader::feature::Value;
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::String(_) | Value::Null => None,
        Value::Float(f) => Some(f.to_string()),
        Value::Double(d) => Some(d.to_string()),
        Value::Int(i) | Value::SInt(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::Bool(b) => Some(b.to_string()),
    }
}

/// Tag keys worth surfacing from Nominatim's full OSM `extratags` (see
/// `search::fetch_osm_element`) — richer than a vector tile's own tags, since this comes straight
/// from the OSM element rather than whatever a style's source layer happened to carry. Each
/// entry lists fallback key names in priority order, mirroring the primary/`contact:*` fallback
/// pairs `shom_client`'s own `search.rs` used. `website`/`contact:website` deliberately isn't
/// here — it becomes a clickable link instead (see [`enrich_from_tags`]).
const RICH_DETAIL_KEYS: &[(&str, &str)] = &[
    // What the place is.
    ("shop", "Shop"),
    ("amenity", "Amenity"),
    ("office", "Office"),
    ("craft", "Craft"),
    ("healthcare", "Healthcare"),
    ("healthcare:speciality", "Speciality"),
    ("tourism", "Tourism"),
    ("leisure", "Leisure"),
    ("historic", "Historic"),
    ("artwork_type", "Artwork"),
    ("artist_name", "Artist"),
    ("building", "Building"),
    ("landuse", "Land use"),
    ("opening_hours", "Opening hours"),
    // A post box's pick-up times, in `opening_hours` syntax.
    ("collection_times", "Collection times"),
    ("phone", "Phone"),
    ("contact:phone", "Phone"),
    ("email", "Email"),
    ("contact:email", "Email"),
    ("cuisine", "Cuisine"),
    ("wheelchair", "Wheelchair access"),
    ("fee", "Fee"),
    ("toll", "Fee"),
    ("fare", "Fare"),
    ("brand", "Brand"),
    ("operator", "Operator"),
    ("description:fr", "Description"),
    ("description", "Description"),
    ("internet_access", "Internet access"),
    ("outdoor_seating", "Outdoor seating"),
    ("takeaway", "Takeaway"),
    ("diet:vegetarian", "Vegetarian"),
    ("diet:vegan", "Vegan"),
    ("start_date", "Since"),
    ("height", "Height"),
    ("level", "Floor"),
    ("smoking", "Smoking"),
    // Bus stops.
    ("local_ref", "Platform"),
    ("route_ref", "Lines"),
    ("network", "Network"),
    ("shelter", "Shelter"),
    ("bench", "Bench"),
    ("tactile_paving", "Tactile paving"),
    ("capacity", "Capacity"),
    ("access", "Access"),
    ("religion", "Religion"),
    ("denomination", "Denomination"),
    ("delivery", "Delivery"),
    ("drive_through", "Drive-through"),
    ("toilets:wheelchair", "Wheelchair toilets"),
    ("air_conditioning", "Air conditioning"),
    ("self_service", "Self service"),
    ("ref:FR:SIRET", "SIRET"),
    ("check_date", "Last checked"),
    ("source", "Source"),
    ("source:date", "Source date"),
    ("ref:mhs", "Mérimée reference"),
];

/// Families of `prefix:<thing>=yes` tags folded into one row listing the things.
const TAG_GROUPS: &[(&str, &str)] = &[
    ("payment:", "Payment"),
    ("fuel:", "Fuels"),
    ("socket:", "Sockets"),
    ("recycling:", "Recycling"),
];

/// Social profile tags, as `(key, link label, URL for a bare handle)`: a value that's already a
/// URL is used as is.
const SOCIAL_KEYS: &[(&str, &str, &str)] = &[
    ("contact:facebook", "Facebook", "https://www.facebook.com/"),
    ("facebook", "Facebook", "https://www.facebook.com/"),
    (
        "contact:instagram",
        "Instagram",
        "https://www.instagram.com/",
    ),
    ("instagram", "Instagram", "https://www.instagram.com/"),
    ("contact:tiktok", "TikTok", "https://www.tiktok.com/@"),
    ("contact:youtube", "YouTube", "https://www.youtube.com/"),
    (
        "contact:linkedin",
        "LinkedIn",
        "https://www.linkedin.com/company/",
    ),
];

/// `true` when the place is registered in France's *Monuments Historiques* heritage list —
/// `heritage:operator=mhs` is how that's tagged in OSM
/// (<https://wiki.openstreetmap.org/wiki/Tag:heritage:operator=mhs>).
fn is_monument_historique(tags: &[(String, String)]) -> bool {
    tags.iter()
        .any(|(k, v)| k == "heritage:operator" && v == "mhs")
}

/// Extra OSM/MVT keys that are deliberately not shown as their own drawer row, nor in the
/// "More tags" fallback, despite not being "unsupported" — either handled elsewhere
/// (`wikipedia`/`wikidata` drive the Wikipedia/Wikivoyage links, image, and population/
/// elevation/inception rows via [`enrich_from_tags`], not a generic tag row; `website`/
/// `contact:website` become a link, not a text row) or purely internal (`rank`: OpenMapTiles'
/// POI label-priority order within a category — steers which icons win at low zoom, means
/// nothing to a user).
const IGNORED_TAG_KEYS: &[&str] = &[
    "wikipedia",
    "wikidata",
    "image",
    "wikimedia_commons",
    "rank",
    "website",
    "contact:website",
    // Consumed by `is_monument_historique` into its own badge row instead of a raw tag row —
    // the bare `heritage` level number (1-4) means nothing without `heritage:operator` telling
    // you *which* country's/registry's scale it's on.
    "heritage",
    "heritage:operator",
    // Not real OSM tags — `search::push_osm_identity`'s stand-in for the response's own
    // osm_type/osm_id, read by `enrich_from_tags` to build the "View on OpenStreetMap" link.
    "_osm_type",
    "_osm_id",
    // Not a real OSM tag either — `search::fetch_osm_element`'s piggybacked last-edited info,
    // read by `enrich_from_tags` into the "Last edited" row instead of shown raw.
    "_last_edited_timestamp",
    "_last_edited_user",
];

/// `true` for a tag that's noise in "More tags": an [`IGNORED_TAG_KEYS`] entry, or a name
/// variant — `name:en`/`name:de`/… (OSM's `lang`-suffixed convention), `name_en`/`name_de`/…
/// (some vector tile sources use `_` instead of `:`), or `int_name`/`alt_name`/`old_name`/
/// `official_name`/`short_name`/`loc_name`/`nat_name`/`reg_name`, plus bare `name` — all already
/// shown as the drawer's own title, and OSM elements often carry dozens of these for well-known
/// places. The `*_name` variants take language suffixes too (`alt_name:sr`).
fn is_noise_tag(key: &str) -> bool {
    let base = key.split(':').next().unwrap_or(key);
    base == "name"
        || key.starts_with("name_")
        || base.ends_with("_name")
        || IGNORED_TAG_KEYS.contains(&key)
}

/// Pulls the [`RICH_DETAIL_KEYS`] worth showing out of a selected place's full OSM `extratags`
/// (see `search::fetch_osm_element`), in order, skipping absent/empty ones and showing only the
/// first fallback key found per label. The second element is every other tag (minus
/// [`IGNORED_TAG_KEYS`]), for the drawer's "More tags" fallback section — logged too, so a gap
/// in [`RICH_DETAIL_KEYS`] shows up in the log even when nobody scrolls down to check.
pub fn details_from_tags(tags: &[(String, String)]) -> DetailRows {
    let tag = |key: &str| {
        tags.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };

    let mut seen_labels = HashSet::new();
    let rows: Vec<(String, String)> = address_line(|part| tag(&format!("addr:{part}")))
        .map(|line| ("Address".to_string(), line))
        .into_iter()
        .chain(RICH_DETAIL_KEYS.iter().filter_map(|(key, label)| {
            // Look the value up before claiming the label, so a missing primary key (`phone`)
            // still falls back to the next one (`contact:phone`).
            let value = tag(key)?;
            let value = match *label {
                // `amenity=pharmacy` + `healthcare=pharmacy`: one row says it.
                "Healthcare" if tag("amenity").as_deref() == Some(value.as_str()) => return None,
                // OSM values and yes/no/limited-style enums: "estate_agent" → "Estate agent".
                "Wheelchair access" | "Internet access" | "Outdoor seating" | "Takeaway"
                | "Vegetarian" | "Vegan" | "Fee" | "Shop" | "Amenity" | "Office" | "Craft"
                | "Healthcare" | "Tourism" | "Leisure" | "Historic" | "Artwork" | "Land use"
                | "Shelter" | "Bench" | "Tactile paving" | "Access" | "Religion"
                | "Denomination" | "Delivery" | "Drive-through" | "Wheelchair toilets"
                | "Air conditioning" | "Self service" => humanize(&value),
                "Speciality" => value
                    .split(';')
                    .map(|part| humanize(part.trim()))
                    .collect::<Vec<_>>()
                    .join(", "),
                // `building=yes` only says "a building".
                "Building" if value == "yes" => return None,
                "Building" => humanize(&value),
                "Height" if value.parse::<f64>().is_ok() => format!("{value} m"),
                "Floor" => floor_label(&value),
                "Smoking" => smoking_label(&value),
                "SIRET" => format_siret(&value),
                "Lines" => value.replace(';', ", "),
                _ => value,
            };
            seen_labels
                .insert(*label)
                .then(|| (label.to_string(), value))
        }))
        .chain(TAG_GROUPS.iter().filter_map(|(prefix, label)| {
            // `payment:cash=yes`, `payment:visa=yes` → "Cash, Visa".
            let accepted: Vec<String> = tags
                .iter()
                .filter(|(k, v)| k.starts_with(prefix) && v != "no")
                .map(|(k, v)| {
                    let name = humanize(&k[prefix.len()..]);
                    if v == "yes" {
                        name
                    } else {
                        format!("{name} ({v})")
                    }
                })
                .collect();
            (!accepted.is_empty()).then(|| (label.to_string(), accepted.join(", ")))
        }))
        .collect();

    let known: HashSet<String> = RICH_DETAIL_KEYS
        .iter()
        .map(|(k, _)| k.to_string())
        .chain(ADDRESS_PARTS.iter().map(|part| format!("addr:{part}")))
        .chain(SOCIAL_KEYS.iter().map(|(k, _, _)| k.to_string()))
        .collect();
    let grouped = |k: &str| TAG_GROUPS.iter().any(|(prefix, _)| k.starts_with(prefix));
    let raw: Vec<(String, String)> = tags
        .iter()
        .filter(|(k, _)| !known.contains(k) && !grouped(k) && !is_noise_tag(k))
        .cloned()
        .collect();
    if !raw.is_empty() {
        log::debug!(
            "[place] {} OSM tag(s) aren't mapped to a drawer row, shown under \"More tags\": {}",
            raw.len(),
            raw.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    (rows, raw)
}

/// `level` (`"0"`, `"1"`, `"-1"`, `"0;1"`) as a floor: `"Ground floor"`, `"Floor 1"`, `"Basement 1"`.
fn floor_label(level: &str) -> String {
    let floor = |level: &str| match level.trim().parse::<f64>() {
        Ok(0.0) => "Ground floor".to_string(),
        Ok(n) if n < 0.0 => format!("Basement {}", -n),
        Ok(n) => format!("Floor {n}"),
        Err(_) => level.trim().to_string(),
    };
    level.split(';').map(floor).collect::<Vec<_>>().join(", ")
}

/// The OSM `smoking` values (<https://wiki.openstreetmap.org/wiki/Key:smoking>) in words.
fn smoking_label(value: &str) -> String {
    match value {
        "yes" => "Allowed".to_string(),
        "no" => "Not allowed".to_string(),
        "outside" => "Outside only".to_string(),
        "separated" => "In a separate area".to_string(),
        "isolated" => "In an isolated room".to_string(),
        "dedicated" => "Smokers' venue".to_string(),
        other => humanize(other),
    }
}

/// A SIRET (14 digits: the SIREN company number, then the establishment's) as written in
/// France: `"123 456 789 00012"`.
fn format_siret(value: &str) -> String {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 14 {
        return value.to_string();
    }
    format!(
        "{} {} {} {}",
        &digits[..3],
        &digits[3..6],
        &digits[6..9],
        &digits[9..]
    )
}

/// Everything derived from `tags` alone, with no network: [`details_from_tags`]'s rows, the
/// Website / OpenStreetMap / Wikipedia links, and the heritage row.
pub fn local_details(tags: &[(String, String)]) -> EnrichedDetails {
    use crate::components::wikipedia;

    let tag = |key: &str| tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let (mut rows, raw_tags) = details_from_tags(tags);
    let mut links = Vec::new();

    if let Some(website) = tag("website").or_else(|| tag("contact:website")) {
        links.push(("Website".to_string(), website.clone(), website));
    }
    // See `search::push_osm_identity`: every enrichment path stashes the element's identity
    // this way.
    if let (Some(kind), Some(id)) = (tag("_osm_type"), tag("_osm_id")) {
        links.push((
            "OpenStreetMap".to_string(),
            "View on OpenStreetMap".to_string(),
            format!("https://www.openstreetmap.org/{kind}/{id}"),
        ));
    }
    if let Some((lang, title)) = tag("wikipedia").and_then(|v| wikipedia::parse_wikipedia_tag(&v)) {
        links.push((
            "Wikipedia".to_string(),
            "Open Wikipedia".to_string(),
            wikipedia::article_url(&lang, &title),
        ));
    }
    for (key, label, base) in SOCIAL_KEYS {
        let Some(value) = tag(key) else { continue };
        let url = if value.starts_with("http") {
            value.clone()
        } else {
            format!("{base}{}", value.trim_start_matches('@'))
        };
        if !links.iter().any(|(existing, _, _)| existing == label) {
            links.push((label.to_string(), value, url));
        }
    }
    // France's official business register entry for the establishment.
    if let Some(siret) = tag("ref:FR:SIRET")
        .map(|siret| {
            siret
                .chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
        })
        .filter(|siret| siret.len() == 14)
    {
        links.push((
            "Company record".to_string(),
            "Annuaire des entreprises".to_string(),
            format!("https://annuaire-entreprises.data.gouv.fr/etablissement/{siret}"),
        ));
    }
    if is_monument_historique(tags) {
        rows.push((
            "Heritage".to_string(),
            "Monument historique (France)".to_string(),
        ));
    }

    EnrichedDetails {
        rows,
        raw_tags,
        links,
        image: None,
        title: None,
    }
}

/// [`local_details`] plus [`online_extras`] — the full enrichment for tags fetched from the
/// OSM API or Nominatim (search results, and POIs from tiles without `poi_details`).
pub async fn enrich_from_tags(tags: &[(String, String)]) -> EnrichedDetails {
    let mut details = local_details(tags);
    let extras = online_extras(tags, true).await;
    details.rows.extend(extras.rows);
    details.links.extend(extras.links);
    details.image = extras.image;
    details
}

/// What `tags` only point to, fetched from the network, all concurrently:
/// - the Wikipedia intro, for a `wikipedia` tag (`lang:Title`, e.g. `"fr:Tour Eiffel"` — not
///   resolved from a `wikidata` QID's sitelinks: if the element doesn't link an article, there's
///   no text to show);
/// - Population/Elevation/Since rows, a preview image and a Wikivoyage link from the `wikidata`
///   entity — independent of `wikipedia`, since plenty of POIs carry one but not the other;
/// - the OSM `image` tag's picture (preferred over Wikidata's);
/// - "Last edited", from the OSM API, only when `fetch_edit_info` and not already in `tags`.
pub async fn online_extras(tags: &[(String, String)], fetch_edit_info: bool) -> EnrichedDetails {
    use crate::components::wikipedia;

    let tag = |key: &str| tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let wiki_tag = tag("wikipedia").and_then(|v| wikipedia::parse_wikipedia_tag(&v));
    let qid = tag("wikidata");
    let osm_image = tag("image");
    let osm_identity = match (tag("_osm_type"), tag("_osm_id")) {
        (Some(kind), Some(id)) => id.parse::<u64>().ok().map(|id| (kind, id)),
        _ => None,
    };
    // `search::fetch_osm_element` piggybacks last-edited info onto its own request already.
    let own_osm_meta = match (tag("_last_edited_timestamp"), tag("_last_edited_user")) {
        (Some(ts), Some(user)) => Some((ts, user)),
        _ => None,
    };

    let intro_fut = async {
        match &wiki_tag {
            Some((lang, title)) => wikipedia::fetch_intro(title.clone(), lang.clone())
                .await
                .ok()
                .flatten(),
            None => None,
        }
    };
    let wikidata_fut = async {
        match &qid {
            Some(qid) => wikipedia::fetch_wikidata_info(qid.clone()).await.ok(),
            None => None,
        }
    };
    let osm_image_fut = async {
        match osm_image {
            Some(value) => wikipedia::fetch_osm_image(value).await.ok().flatten(),
            None => None,
        }
    };
    let osm_meta_fut = async {
        if own_osm_meta.is_some() || !fetch_edit_info {
            return None;
        }
        match &osm_identity {
            Some((kind, id)) => search::fetch_osm_element_meta(kind.clone(), *id)
                .await
                .ok()
                .flatten(),
            None => None,
        }
    };
    let (intro, wikidata_info, mut image, osm_meta) =
        tokio::join!(intro_fut, wikidata_fut, osm_image_fut, osm_meta_fut);

    let mut rows = Vec::new();
    let mut links = Vec::new();
    if let Some(extract) = intro {
        rows.push(("Wikipedia".to_string(), extract));
    }
    if let Some((timestamp, user)) = own_osm_meta.or(osm_meta) {
        // The date alone is what's useful in a one-line row.
        let date = timestamp.split('T').next().unwrap_or(&timestamp);
        rows.push(("Last edited".to_string(), format!("{date} by {user}")));
    }
    if let Some(info) = wikidata_info {
        if let Some(population) = info.population {
            rows.push(("Population".to_string(), population.to_string()));
        }
        if let Some(elevation) = info.elevation_m {
            rows.push(("Elevation".to_string(), format!("{elevation} m")));
        }
        // Don't duplicate a "Since" row the OSM `start_date` tag already produced.
        if let Some(year) = info.inception_year.clone()
            && tag("start_date").is_none()
        {
            rows.push(("Since".to_string(), year));
        }
        if let Some(url) = info.wikivoyage_url() {
            links.push(("Wikivoyage".to_string(), "Open Wikivoyage".to_string(), url));
        }
        // The OSM `image` tag wins when present — the mapper's own pick for this feature, where
        // Wikidata's P18 is often just "some photo of the general topic".
        if image.is_none()
            && let Some(filename) = info.image_filename
        {
            image = wikipedia::fetch_commons_thumbnail(filename)
                .await
                .ok()
                .flatten();
        }
    }

    EnrichedDetails {
        rows,
        raw_tags: Vec::new(),
        links,
        image,
        title: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn fallback_key_used_when_primary_missing() {
        let (rows, _) = details_from_tags(&tags(&[("contact:phone", "+33 1 23 45 67 89")]));
        assert_eq!(rows, tags(&[("Phone", "+33 1 23 45 67 89")]));
    }

    #[test]
    fn primary_key_wins_over_fallback() {
        let (rows, raw) = details_from_tags(&tags(&[("phone", "1"), ("contact:phone", "2")]));
        assert_eq!(rows, tags(&[("Phone", "1")]));
        assert!(raw.is_empty());
    }

    #[test]
    fn address_parts_fold_into_one_row() {
        let (rows, raw) = details_from_tags(&tags(&[
            ("addr:street", "Rue de Rivoli"),
            ("addr:housenumber", "12"),
            ("addr:city", "Paris"),
            ("addr:postcode", "75001"),
            ("addr:country", "FR"),
        ]));
        assert_eq!(rows, tags(&[("Address", "12 Rue de Rivoli, 75001 Paris")]));
        assert_eq!(raw, tags(&[("addr:country", "FR")]));
    }

    #[test]
    fn enrichment_replaces_rows_instead_of_duplicating() {
        let mut place = SelectedPlace {
            id: 1,
            title: "Café".into(),
            subtitle: String::new(),
            details: tags(&[
                ("Address", "Photon line"),
                ("Opening hours", "Mo 09:00-10:00"),
            ]),
            raw_tags: tags(&[("brand", "x")]),
            links: Vec::new(),
            marker_world: None,
            loading: true,
            image: None,
            transit_stop: None,
            coords: None,
            charger: None,
        };
        place.apply(EnrichedDetails {
            rows: tags(&[("Opening hours", "Mo-Fr 09:00-18:00"), ("Phone", "1")]),
            raw_tags: tags(&[("brand", "y")]),
            links: Vec::new(),
            image: None,
            title: None,
        });
        assert_eq!(
            place.details,
            tags(&[
                ("Address", "Photon line"),
                ("Opening hours", "Mo-Fr 09:00-18:00"),
                ("Phone", "1"),
            ])
        );
        assert_eq!(place.raw_tags, tags(&[("brand", "y")]));
        assert!(!place.loading);
    }

    #[test]
    fn name_variants_are_noise() {
        for key in [
            "name",
            "name:en",
            "name_de",
            "alt_name",
            "alt_name:sr",
            "old_name:fr",
        ] {
            assert!(is_noise_tag(key), "{key}");
        }
        assert!(!is_noise_tag("architect"));
    }

    #[test]
    fn kinds_and_enums_read_as_words() {
        let (rows, raw) = details_from_tags(&tags(&[
            ("office", "estate_agent"),
            ("building", "yes"),
            ("level", "-1"),
            ("smoking", "outside"),
            ("ref:FR:SIRET", "12345678900012"),
            ("check_date", "2025-06-01"),
            ("amenity", "pharmacy"),
            ("healthcare", "pharmacy"),
        ]));
        assert_eq!(
            rows,
            tags(&[
                ("Amenity", "Pharmacy"),
                ("Office", "Estate agent"),
                ("Floor", "Basement 1"),
                ("Smoking", "Outside only"),
                ("SIRET", "123 456 789 00012"),
                ("Last checked", "2025-06-01"),
            ])
        );
        assert!(raw.is_empty());
        assert_eq!(floor_label("0;1"), "Ground floor, Floor 1");

        let (rows, raw) = details_from_tags(&tags(&[
            ("payment:cash", "yes"),
            ("payment:cheque", "no"),
            ("payment:visa", "yes"),
            ("contact:instagram", "lafarine"),
        ]));
        assert_eq!(rows, tags(&[("Payment", "Cash, Visa")]));
        assert!(raw.is_empty());
        let links = local_details(&tags(&[("contact:instagram", "@lafarine")])).links;
        assert_eq!(links[0].2, "https://www.instagram.com/lafarine");
    }

    #[test]
    fn humanize_snake_case() {
        assert_eq!(humanize("fast_food"), "Fast food");
        assert_eq!(humanize(""), "");
    }
}
