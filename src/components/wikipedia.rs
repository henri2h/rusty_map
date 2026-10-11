//! Wikipedia/Wikidata enrichment for a selected place, resolved from the OSM `wikipedia`/
//! `wikidata` tags a [`crate::components::search::util::search::fetch_osm_element`]/
//! [`crate::components::search::util::search::fetch_reverse`] call returns. Trimmed port of
//! `shom_client`'s `src/wikipedia/` module: the direct-tag, short plain-text intro lookup (no
//! Wikidata QID→sitelink text resolution — a place with no `wikipedia` tag of its own just
//! doesn't get a Wikipedia row), plus — unlike the text — the Wikidata "image" claim (P18) *is*
//! used when present, since many POIs carry a `wikidata` QID but no direct `wikipedia` tag, same
//! as `shom_client`'s popup image.

use bytes::Bytes;
use serde_json::Value;

use map_component::util::tiles::{FetchError, background_runtime, client};

/// Parses an OSM `wikipedia` tag (`"fr:Tour Eiffel"`) into `(lang, title)`.
pub fn parse_wikipedia_tag(tag: &str) -> Option<(String, String)> {
    let (lang, title) = tag.split_once(':')?;
    if lang.is_empty() || title.is_empty() {
        return None;
    }
    Some((lang.to_string(), title.to_string()))
}

/// The article's own URL, for a clickable "Open Wikipedia" link.
pub fn article_url(lang: &str, title: &str) -> String {
    format!(
        "https://{lang}.wikipedia.org/wiki/{}",
        title.replace(' ', "_")
    )
}

/// Fetches a short plain-text intro (first ~5 sentences) for a Wikipedia article, via
/// MediaWiki's own `action=query&prop=extracts` — the same endpoint/params `shom_client`'s
/// `fetch_intro` used.
pub async fn fetch_intro(title: String, lang: String) -> Result<Option<String>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(fetch_intro_io(title, lang).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_intro_io(title: String, lang: String) -> Result<Option<String>, FetchError> {
    log::trace!("[http] GET https://{lang}.wikipedia.org/w/api.php title={title:?}");
    let response = client()
        .get(format!("https://{lang}.wikipedia.org/w/api.php"))
        .query(&[
            ("action", "query"),
            ("prop", "extracts"),
            ("exintro", "1"),
            ("exsentences", "5"),
            ("explaintext", "1"),
            ("titles", title.as_str()),
            ("format", "json"),
        ])
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;

    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;

    let extract = parsed["query"]["pages"]
        .as_object()
        .and_then(|pages| pages.values().next())
        .and_then(|page| page["extract"].as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(extract)
}

/// A handful of Wikidata claims/sitelinks worth surfacing for a place — everything `shom_client`
/// showed from its own Wikidata block except the label/description text (that role is filled by
/// [`fetch_intro`] here) and the brand-QID/wikivoyage-extract machinery.
#[derive(Default, Clone)]
pub struct WikidataInfo {
    /// Commons filename (P18), e.g. `"Tour Eiffel Wikimedia Commons.jpg"`.
    pub image_filename: Option<String>,
    /// Population (P1082), for places.
    pub population: Option<i64>,
    /// Elevation in meters (P2044), for peaks/geographic features.
    pub elevation_m: Option<i64>,
    /// Inception year (P571, year component only), for buildings/structures/organizations.
    pub inception_year: Option<String>,
    /// Wikivoyage sitelink, preferring French then English — a QID with a Wikipedia sitelink
    /// doesn't always have a Wikivoyage one (travel-guide coverage is much smaller).
    pub wikivoyage_title: Option<String>,
    pub wikivoyage_lang: Option<String>,
}

impl WikidataInfo {
    pub fn wikivoyage_url(&self) -> Option<String> {
        let title = self.wikivoyage_title.as_deref()?;
        let lang = self.wikivoyage_lang.as_deref().unwrap_or("en");
        Some(format!(
            "https://{lang}.wikivoyage.org/wiki/{}",
            title.replace(' ', "_")
        ))
    }
}

/// Fetches a QID's image claim, population/elevation/inception claims, and Wikivoyage sitelink
/// in one request (`props=claims|sitelinks`) — cheaper than one request per field, and lines up
/// with `enrich_from_tags` running this alongside [`fetch_intro`] via `tokio::join!`.
pub async fn fetch_wikidata_info(qid: String) -> Result<WikidataInfo, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(fetch_wikidata_info_io(qid).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_wikidata_info_io(qid: String) -> Result<WikidataInfo, FetchError> {
    log::trace!("[http] GET https://www.wikidata.org/w/api.php ids={qid}");
    let response = client()
        .get("https://www.wikidata.org/w/api.php")
        .query(&[
            ("action", "wbgetentities"),
            ("ids", qid.as_str()),
            ("format", "json"),
            ("props", "claims|sitelinks"),
        ])
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;

    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
    let entity = &parsed["entities"][qid.as_str()];

    let image_filename = entity["claims"]["P18"][0]["mainsnak"]["datavalue"]["value"]
        .as_str()
        .map(str::to_string);

    let population = claim_quantity(entity, "P1082");
    let elevation_m = claim_quantity(entity, "P2044");
    let inception_year = entity["claims"]["P571"][0]["mainsnak"]["datavalue"]["value"]["time"]
        .as_str()
        // Wikidata time values look like "+1889-03-31T00:00:00Z"; the year is the first 5 chars
        // (sign + 4 digits) for anything in the common era, which is all this app cares about.
        .and_then(|t| t.get(1..5))
        .map(str::to_string);

    let (wikivoyage_title, wikivoyage_lang) = [("frwikivoyage", "fr"), ("enwikivoyage", "en")]
        .into_iter()
        .find_map(|(site, lang)| {
            entity["sitelinks"][site]["title"]
                .as_str()
                .map(|title| (Some(title.to_string()), Some(lang.to_string())))
        })
        .unwrap_or((None, None));

    Ok(WikidataInfo {
        image_filename,
        population,
        elevation_m,
        inception_year,
        wikivoyage_title,
        wikivoyage_lang,
    })
}

/// Reads a Wikidata "quantity" claim's amount (e.g. P1082 population, P2044 elevation) as an
/// integer — quantity values are serialized as decimal strings (`"+324.5"`), sometimes with a
/// leading `+`, so this can't just deserialize as a JSON number.
fn claim_quantity(entity: &Value, property: &str) -> Option<i64> {
    entity["claims"][property][0]["mainsnak"]["datavalue"]["value"]["amount"]
        .as_str()
        .and_then(|s| s.trim_start_matches('+').parse::<f64>().ok())
        .map(|v| v.round() as i64)
}

/// Resolves a Commons filename (from [`fetch_wikidata_info`] or an OSM `image` tag) to a downloaded thumbnail's
/// bytes, capped to 600px wide — a drawer preview, not the full-resolution original.
pub async fn fetch_commons_thumbnail(filename: String) -> Result<Option<Bytes>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    background_runtime().spawn(async move {
        let _ = tx.send(fetch_commons_thumbnail_io(filename).await);
    });

    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_commons_thumbnail_io(filename: String) -> Result<Option<Bytes>, FetchError> {
    let title = if filename.starts_with("File:") {
        filename
    } else {
        format!("File:{filename}")
    };

    log::trace!("[http] GET https://commons.wikimedia.org/w/api.php titles={title:?}");
    let response = client()
        .get("https://commons.wikimedia.org/w/api.php")
        .query(&[
            ("action", "query"),
            ("prop", "imageinfo"),
            ("iiprop", "url"),
            ("iiurlwidth", "600"),
            ("titles", title.as_str()),
            ("format", "json"),
        ])
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;

    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;

    let Some(url) = parsed["query"]["pages"]
        .as_object()
        .and_then(|pages| pages.values().next())
        .and_then(|page| {
            page["imageinfo"][0]["thumburl"]
                .as_str()
                .or_else(|| page["imageinfo"][0]["url"].as_str())
        })
        .map(str::to_string)
    else {
        return Ok(None);
    };

    fetch_url_bytes(&url).await
}

/// Downloads raw bytes from an arbitrary URL (the OSM `image` tag's own direct-URL case — see
/// [`fetch_osm_image`] — and the resolved-thumbnail case both end up here).
async fn fetch_url_bytes(url: &str) -> Result<Option<Bytes>, FetchError> {
    log::trace!("[http] GET {url}");
    let response = client()
        .get(url)
        .header(reqwest::header::USER_AGENT, "rusty_map/1.0")
        .send()
        .await
        .map_err(FetchError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    Ok(Some(response.bytes().await.map_err(FetchError::Network)?))
}

/// Resolves an OSM `image` tag to downloaded bytes — the tag's value is inconsistent across OSM
/// data: a direct image URL (`"http://.../Arc_de_triomphe_Paris.jpg"`), a Wikipedia/Commons
/// *file description page* URL (`"https://fr.wikipedia.org/wiki/Fichier:Tour_Eiffel_….jpg"` —
/// fetching that directly would download HTML, not an image), or a bare Commons filename. All
/// three end up resolved through Commons except the plain direct-URL case.
pub async fn fetch_osm_image(value: String) -> Result<Option<Bytes>, FetchError> {
    if let Some(filename) = commons_filename_from_wiki_url(&value) {
        return fetch_commons_thumbnail(filename).await;
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        let (tx, rx) = tokio::sync::oneshot::channel();
        background_runtime().spawn(async move {
            let _ = tx.send(fetch_url_bytes(&value).await);
        });
        return rx.await.unwrap_or(Err(FetchError::TaskFailed));
    }
    fetch_commons_thumbnail(value).await
}

/// Pulls a Commons filename out of a Wikipedia/Commons file-description page URL, e.g.
/// `".../wiki/Fichier:Tour_Eiffel_Wikimedia_Commons.jpg"` → `"File:Tour_Eiffel_Wikimedia_Commons.jpg"`.
/// `None` for anything else (a direct image URL, a bare filename, …).
fn commons_filename_from_wiki_url(value: &str) -> Option<String> {
    let title = value.split("/wiki/").nth(1)?;
    let name = title
        .strip_prefix("File:")
        .or_else(|| title.strip_prefix("Fichier:"))?;
    Some(format!("File:{name}"))
}
