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

use crate::components::search::util::search::{self, SearchResult};

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
}

/// Identifies one enrichment fetch for [`EnrichCache`] purposes: a search result's stable OSM
/// id, or a POI tap's reverse-geocode point quantized to ~0.1m (`lat`/`lon` × 1e7, matching the
/// precision `labels::icon_identity` already uses for the same kind of float-key quantization)
/// plus the zoom bucket that was reverse-geocoded (`search::fetch_reverse`'s `zoom` param changes
/// which feature radius resolves, so it's part of the cache identity too).
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum EnrichKey {
    Details(String, u64),
    Reverse(i64, i64, u32),
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
        self.select_and_enrich(place, platform, async move {
            let key = EnrichKey::Details(osm_type.clone(), osm_id);
            enrich_cached(&cache, key, || search::fetch_osm_element(osm_type, osm_id)).await
        });
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
            updated.apply(enriched);
            selected.set(Some(updated));
            platform.send(UserEvent::RequestRedraw);
        });
    }
}

/// Source of [`SelectedPlace::id`].
static NEXT_PLACE_ID: AtomicU64 = AtomicU64::new(1);

fn next_place_id() -> u64 {
    NEXT_PLACE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Immediate (not-yet-enriched) place for a tapped POI. No `marker_world` — the tapped
/// sprite/label is already drawn on the map, so no extra pin is needed.
pub fn place_from_tap(tap: &TappedFeature) -> SelectedPlace {
    let (details, raw_tags) = details_from_properties(&tap.properties);
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
            ("Coordinates".to_string(), format!("{lat:.5}, {lon:.5}")),
        ],
        raw_tags: Vec::new(),
        links: Vec::new(),
        marker_world: Some(lonlat_to_world(lon, lat)),
        loading: true,
        image: None,
    }
}

/// Fetches the rich OSM tags (opening hours, phone, website, Wikipedia, …) for a tapped POI. Uses
/// the OSM id carried by the tile when there is one; otherwise falls back to reverse-geocoding,
/// which can resolve to the wrong nearby feature (e.g. a street) instead of the POI itself.
pub async fn enrich_tap(cache: &EnrichCache, tap: &TappedFeature) -> EnrichedDetails {
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
        self.loading = false;
    }
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
    ("opening_hours", "Opening hours"),
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
    ("source", "Source"),
    ("source:date", "Source date"),
    ("ref:mhs", "Mérimée reference"),
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
                // yes/no/limited-style enums: "limited" → "Limited".
                "Wheelchair access" | "Internet access" | "Outdoor seating" | "Takeaway"
                | "Vegetarian" | "Vegan" | "Fee" => humanize(&value),
                "Height" if value.parse::<f64>().is_ok() => format!("{value} m"),
                _ => value,
            };
            seen_labels
                .insert(*label)
                .then(|| (label.to_string(), value))
        }))
        .collect();

    let known: HashSet<String> = RICH_DETAIL_KEYS
        .iter()
        .map(|(k, _)| k.to_string())
        .chain(ADDRESS_PARTS.iter().map(|part| format!("addr:{part}")))
        .collect();
    let raw: Vec<(String, String)> = tags
        .iter()
        .filter(|(k, _)| !known.contains(k) && !is_noise_tag(k))
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

/// [`details_from_tags`] plus:
/// - a clickable Website link, when `website`/`contact:website` is present;
/// - a Wikipedia intro row + a clickable "Open Wikipedia" link, when this place's own
///   `wikipedia` OSM tag is present (`lang:Title`, e.g. `"fr:Tour Eiffel"` — deliberately not
///   falling back to resolving a `wikidata` QID's sitelinks: if the OSM element itself doesn't
///   link to an article, there's no text to show);
/// - Population/Elevation/Since rows, a preview image, and a clickable Wikivoyage link, all from
///   the place's Wikidata entity (when a `wikidata` tag is present) — independent of the
///   `wikipedia` tag, since plenty of POIs carry one but not the other.
///
/// The Wikipedia-intro and Wikidata-entity fetches run concurrently (`tokio::join!`) — they're
/// independent, both keyed off `tags` rather than each other's result — only the Commons
/// thumbnail download stays sequential after, since it needs the Wikidata fetch's own filename.
///
/// Shared by the search-result (`search::fetch_osm_element`) and POI-tap (`search::fetch_reverse`)
/// enrichment paths, which differ only in how they get `tags`.
pub async fn enrich_from_tags(tags: &[(String, String)]) -> EnrichedDetails {
    use crate::components::wikipedia;

    let (mut rows, raw) = details_from_tags(tags);
    let mut links = Vec::new();

    if let Some((_, website)) = tags
        .iter()
        .find(|(k, _)| k == "website" || k == "contact:website")
    {
        links.push(("Website".to_string(), website.clone(), website.clone()));
    }

    // See `search::push_osm_identity` — both `fetch_osm_element` and `fetch_reverse` stash the
    // result's own identity this way so this one link works for either enrichment path.
    if let (Some((_, kind)), Some((_, id))) = (
        tags.iter().find(|(k, _)| k == "_osm_type"),
        tags.iter().find(|(k, _)| k == "_osm_id"),
    ) {
        links.push((
            "OpenStreetMap".to_string(),
            "View on OpenStreetMap".to_string(),
            format!("https://www.openstreetmap.org/{kind}/{id}"),
        ));
    }

    let wiki_tag = tags
        .iter()
        .find(|(k, _)| k == "wikipedia")
        .and_then(|(_, v)| wikipedia::parse_wikipedia_tag(v));
    let qid = tags
        .iter()
        .find(|(k, _)| k == "wikidata")
        .map(|(_, v)| v.clone());
    let osm_image = tags
        .iter()
        .find(|(k, _)| k == "image")
        .map(|(_, v)| v.clone());
    let osm_identity = match (
        tags.iter().find(|(k, _)| k == "_osm_type"),
        tags.iter().find(|(k, _)| k == "_osm_id"),
    ) {
        (Some((_, kind)), Some((_, id))) => id.parse::<u64>().ok().map(|id| (kind.clone(), id)),
        _ => None,
    };
    // `search::fetch_osm_element` (the search-result/POI-tap path) piggybacks last-edited info
    // onto its one tag-fetching request already — only `search::fetch_reverse` (Nominatim, used
    // when no id is known upfront) needs a separate network call for it below.
    let own_osm_meta = match (
        tags.iter().find(|(k, _)| k == "_last_edited_timestamp"),
        tags.iter().find(|(k, _)| k == "_last_edited_user"),
    ) {
        (Some((_, ts)), Some((_, user))) => Some((ts.clone(), user.clone())),
        _ => None,
    };

    // All four are independent, keyed only off `tags` (not off each other's result), so they
    // run concurrently rather than as four sequential round trips.
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
        if own_osm_meta.is_some() {
            return None; // already have it, see `own_osm_meta` above.
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

    if let Some(extract) = intro {
        rows.push(("Wikipedia".to_string(), extract));
    }
    if let Some((lang, title)) = &wiki_tag {
        links.push((
            "Wikipedia".to_string(),
            "Open Wikipedia".to_string(),
            wikipedia::article_url(lang, title),
        ));
    }

    if is_monument_historique(tags) {
        rows.push((
            "Heritage".to_string(),
            "Monument historique (France)".to_string(),
        ));
    }

    if let Some((timestamp, user)) = own_osm_meta.or(osm_meta) {
        // Wikidata time values are full ISO instants ("2023-08-09T14:02:11Z"); the date alone
        // is what's actually useful in a one-line row.
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
        // Don't duplicate a "Since" row the OSM `start_date` tag already produced above.
        if let Some(year) = info.inception_year.clone()
            && !rows.iter().any(|(label, _)| label == "Since")
        {
            rows.push(("Since".to_string(), year));
        }
        if let Some(url) = info.wikivoyage_url() {
            links.push(("Wikivoyage".to_string(), "Open Wikivoyage".to_string(), url));
        }
        // The OSM `image` tag (already tried above) wins when present — it's the mapper's own
        // pick for this specific feature, where Wikidata's P18 is often just "some photo of the
        // general topic". Only fall back to Wikidata's image when OSM's tag was absent/failed.
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
        raw_tags: raw,
        links,
        image,
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
        };
        place.apply(EnrichedDetails {
            rows: tags(&[("Opening hours", "Mo-Fr 09:00-18:00"), ("Phone", "1")]),
            raw_tags: tags(&[("brand", "y")]),
            links: Vec::new(),
            image: None,
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
    fn humanize_snake_case() {
        assert_eq!(humanize("fast_food"), "Fast food");
        assert_eq!(humanize(""), "");
    }
}
