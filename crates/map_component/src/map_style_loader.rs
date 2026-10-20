use serde::Deserialize;

use crate::style::{self, StyleSpec};

use super::util::tiles::{fetch_text_cached, invalidate_text_cache};

/// The map's style JSON. Its own `sources`/`sprite` fields (not any hardcoded URL here) drive
/// which tile source and sprite sheet get fetched — see [`resolve_style`].
const STYLE_URL: &str = "https://openmaptiles.geo.data.gouv.fr/styles/osm-bright/style.json";

/// The app's own tiles (`tools/tiles`): the OpenMapTiles schema the style expects plus the
/// `transit_lines` and `poi_details` layers. France only.
const TILE_URL: &str = "https://map.carnot.cc/rusty_map/{z}/{x}/{y}";

/// Environment variable overriding [`TILE_URL`] with another `…/{z}/{x}/{y}…` template in the
/// same schema; `style` keeps the style's own source (data.gouv.fr, whole planet).
const TILE_URL_ENV: &str = "RUSTY_MAP_TILE_URL";

/// Fetches (with a permanent disk cache, like tiles) and parses the real style at [`STYLE_URL`],
/// falling back to `style::default::default_style()` on any network or parse error. A cached copy
/// that fails to parse is deleted and refetched once — a corrupt cache file must not permanently
/// pin the fallback style.
pub async fn load_style() -> StyleSpec {
    match fetch_text_cached(STYLE_URL.to_string()).await {
        Ok(json) => match style::parser::parse_style(&json) {
            Ok(parsed) => {
                log::info!("[style] loaded {STYLE_URL}");
                return resolve_style(parsed).await;
            }
            Err(err) => {
                log::warn!("[style] cached parse failed ({err}), invalidating and refetching");
                invalidate_text_cache(STYLE_URL.to_string()).await;
                if let Ok(json) = fetch_text_cached(STYLE_URL.to_string()).await
                    && let Ok(parsed) = style::parser::parse_style(&json)
                {
                    log::info!("[style] loaded {STYLE_URL} after cache invalidation");
                    return resolve_style(parsed).await;
                }
                log::warn!("[style] refetch failed too, using default");
            }
        },
        Err(err) => log::warn!("[style] fetch failed, using default: {err:?}"),
    }
    style::default::default_style()
}

/// Turns a [`style::parser::ParsedStyle`]'s raw source info into a final [`StyleSpec`]: an
/// inline `tiles` template is used as-is, a `url`-indirected source needs a further TileJSON
/// fetch to pull its `tiles[0]` out. Either side failing falls back to the same
/// `shortbread_v1` source `style::default::default_style()` uses — a style with the wrong tile
/// source renders nothing (source schema and style layers must agree), so silently keeping a
/// hardcoded template here would just reproduce that bug for the next style swap.
async fn resolve_style(parsed: style::parser::ParsedStyle) -> StyleSpec {
    let mut tiles_template = parsed.source.tiles_template;
    let mut min_zoom = parsed.source.min_zoom;
    let mut max_zoom = parsed.source.max_zoom;

    // A TileJSON fetch is only needed for whatever the inline source didn't already give us.
    if let Some(url) = parsed.source.tilejson_url
        && (tiles_template.is_none() || min_zoom.is_none() || max_zoom.is_none())
    {
        match fetch_text_cached(url.clone()).await {
            Ok(json) => match parse_tilejson(&json) {
                Ok(tj) => {
                    tiles_template = tiles_template.or(tj.tiles_template);
                    min_zoom = min_zoom.or(tj.min_zoom);
                    max_zoom = max_zoom.or(tj.max_zoom);
                }
                Err(err) => {
                    log::warn!("[style] TileJSON parse failed ({err}), using default source")
                }
            },
            Err(err) => log::warn!("[style] TileJSON fetch failed ({err:?}), using default source"),
        }
    }

    // Our own tiles, in the same OpenMapTiles schema, replace the style's source — in France,
    // which is all they cover: elsewhere the style's own (whole planet) source still draws.
    let (tile_url_template, fallback_tile_url_template) = match std::env::var(TILE_URL_ENV) {
        Ok(url) if url == "style" => (
            tiles_template.unwrap_or_else(|| {
                log::warn!("[style] no usable vector source, using default source");
                style::default::DEFAULT_TILE_URL_TEMPLATE.to_string()
            }),
            None,
        ),
        Ok(url) if !url.is_empty() => {
            log::info!("[style] tiles from {TILE_URL_ENV}: {url}");
            (url, None)
        }
        _ => (TILE_URL.to_string(), tiles_template),
    };

    StyleSpec {
        layers: parsed.layers,
        tile_url_template,
        fallback_tile_url_template,
        sprite_url: parsed.source.sprite_url,
        min_zoom: min_zoom
            .map(|z| z as f32)
            .unwrap_or(style::default::DEFAULT_MIN_ZOOM),
        max_real_zoom: max_zoom
            .map(|z| z as u32)
            .unwrap_or(style::default::DEFAULT_MAX_REAL_ZOOM),
        poi_filter: None,
        icon_min_zoom: crate::util::labels::ICON_MIN_ZOOM,
        pick_transit_lines: false,
    }
}

/// The [TileJSON spec](https://github.com/mapbox/tilejson-spec) fields this app needs — fixed
/// shape, unlike the style JSON's `layers` (see `style::parser`'s `StyleDocument` doc comment),
/// so a plain `Deserialize` struct fits directly.
#[derive(Deserialize, Default)]
struct TileJson {
    #[serde(default)]
    tiles: Vec<String>,
    minzoom: Option<f64>,
    maxzoom: Option<f64>,
}

struct ResolvedTileJson {
    tiles_template: Option<String>,
    min_zoom: Option<f64>,
    max_zoom: Option<f64>,
}

fn parse_tilejson(json: &str) -> Result<ResolvedTileJson, serde_json::Error> {
    let tj: TileJson = serde_json::from_str(json)?;
    Ok(ResolvedTileJson {
        tiles_template: tj.tiles.into_iter().next(),
        min_zoom: tj.minzoom,
        max_zoom: tj.maxzoom,
    })
}
