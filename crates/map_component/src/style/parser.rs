//! Imports a real MapLibre/Mapbox-GL-style JSON document into a [`StyleSpec`]. Ported from
//! `shom_client`'s `src/map/style/parser.rs`, trimmed to the `background`/`fill`/`line`/`symbol`
//! layer types `rusty_map` actually renders.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};
use serde_json::Value as Jv;

use super::{
    BgLayer, ColorExpr, FillLayer, Filter, LineCap, LineJoin, LineLayer, MatchValue, NumExpr,
    SortKey, StyleLayer, SymbolLayer, TextExpr, TextPlacement, TextTransform, parse_color_str,
};

/// A style JSON parsed into layers plus its raw (not-yet-network-resolved) source info — the
/// caller (which owns the async fetch machinery) resolves [`RawSource`] into a final
/// [`StyleSpec`], since a TileJSON `url` source needs a further HTTP fetch this module can't do.
pub struct ParsedStyle {
    pub layers: Vec<StyleLayer>,
    pub source: RawSource,
}

/// The style's vector tile source and sprite base URL, straight off the JSON, before resolving
/// an indirect TileJSON `url` (if that's what the style uses instead of an inline `tiles` array).
#[derive(Default)]
pub struct RawSource {
    /// Inline `tiles[0]` template (`{z}`/`{x}`/`{y}` placeholders), if the source has one.
    pub tiles_template: Option<String>,
    /// A TileJSON URL to fetch and pull `tiles[0]` from, if the source is `url`-indirected
    /// instead of inline.
    pub tilejson_url: Option<String>,
    pub sprite_url: Option<String>,
    /// The source's own `minzoom`/`maxzoom`, if given inline (the Mapbox style spec allows this
    /// even for a `url`-indirected source) — a TileJSON fetch is only needed for these when
    /// they're absent here.
    pub min_zoom: Option<f64>,
    pub max_zoom: Option<f64>,
}

/// The handful of top-level style JSON fields this module cares about — fixed shape, so a real
/// `Deserialize` struct (unlike the `layers` array below, whose `paint`/`filter` values are
/// genuinely polymorphic per MapLibre's expression grammar: a string, a number, an array-encoded
/// expression, or a `{stops: [...]}` object depending on the property — not something a single
/// `serde` shape can represent without effectively reinventing `Value` as an enum).
#[derive(Deserialize)]
struct StyleDocument {
    #[serde(default)]
    sources: HashMap<String, SourceDef>,
    sprite: Option<String>,
}

#[derive(Deserialize)]
struct SourceDef {
    #[serde(rename = "type")]
    source_type: String,
    tiles: Option<Vec<String>>,
    url: Option<String>,
    minzoom: Option<f64>,
    maxzoom: Option<f64>,
}

pub fn parse_style(json: &str) -> Result<ParsedStyle, String> {
    let v: Jv = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let doc = StyleDocument::deserialize(&v).map_err(|e| e.to_string())?;

    // The layers array still goes through `Value` — see `StyleDocument`'s doc comment.
    let layers = v["layers"]
        .as_array()
        .map(|ls| ls.iter().filter_map(parse_layer).collect())
        .unwrap_or_default();

    Ok(ParsedStyle {
        layers,
        source: parse_source(doc),
    })
}

/// Picks the style's vector tile source (the first `sources` entry with `"type": "vector"`).
fn parse_source(doc: StyleDocument) -> RawSource {
    let Some(source) = doc
        .sources
        .into_values()
        .find(|s| s.source_type == "vector")
    else {
        return RawSource {
            sprite_url: doc.sprite,
            ..Default::default()
        };
    };

    RawSource {
        tiles_template: source.tiles.and_then(|tiles| tiles.into_iter().next()),
        tilejson_url: source.url,
        sprite_url: doc.sprite,
        min_zoom: source.minzoom,
        max_zoom: source.maxzoom,
    }
}

/// The layer-level fields whose shape is fixed regardless of layer type — every genuinely
/// polymorphic `paint`/`layout` property (color, width, opacity, `text-field`, sort-key — see
/// `parse_color_expr`/`parse_num_expr`) is deliberately left out and still read from `Jv`
/// (`LayerDoc::paint`/`LayerDoc::layout_raw`) instead.
#[derive(Deserialize)]
struct LayerDoc {
    #[serde(rename = "type")]
    layer_type: String,
    #[serde(default = "default_min_zoom")]
    minzoom: f64,
    #[serde(default = "default_max_zoom")]
    maxzoom: f64,
    #[serde(rename = "source-layer", default)]
    source_layer: String,
    #[serde(default)]
    layout: LayoutScalars,
    #[serde(default)]
    paint: PaintScalars,
}

fn default_min_zoom() -> f64 {
    0.0
}
fn default_max_zoom() -> f64 {
    24.0
}

/// Every field here is read leniently (`lenient_*`): MapLibre allows most of them to be a zoom
/// function (`{"stops": [...]}`) or an expression too, and one unexpected shape used to fail the
/// whole `LayerDoc` and silently drop the layer — osm-bright's three `highway-shield*` layers
/// (zoom-dependent `symbol-placement`) were never drawn.
#[derive(Deserialize, Default)]
struct LayoutScalars {
    #[serde(default, deserialize_with = "lenient_string")]
    visibility: Option<String>,
    #[serde(rename = "line-cap", default, deserialize_with = "lenient_string")]
    line_cap: Option<String>,
    #[serde(rename = "line-join", default, deserialize_with = "lenient_string")]
    line_join: Option<String>,
    #[serde(
        rename = "symbol-placement",
        default,
        deserialize_with = "lenient_string"
    )]
    symbol_placement: Option<String>,
    #[serde(
        rename = "text-transform",
        default,
        deserialize_with = "lenient_string"
    )]
    text_transform: Option<String>,
    #[serde(rename = "text-font", default, deserialize_with = "lenient_strings")]
    text_font: Vec<String>,
}

#[derive(Deserialize, Default)]
struct PaintScalars {
    #[serde(
        rename = "text-halo-width",
        default,
        deserialize_with = "lenient_number"
    )]
    text_halo_width: Option<f64>,
}

/// A scalar property's single value: the value itself, or — for a legacy zoom function
/// `{"stops": [[z, value], ...]}` — its last stop's value (these properties are modeled as
/// constants, and the last stop is the one that holds at the street-level zooms that matter
/// most). Anything else (an expression) yields `None`, i.e. the property's default.
fn scalar_of(v: Jv) -> Option<Jv> {
    match v {
        Jv::Object(mut map) => match map.remove("stops")? {
            Jv::Array(mut stops) => match stops.pop()? {
                Jv::Array(mut stop) if stop.len() == 2 => stop.pop(),
                _ => None,
            },
            _ => None,
        },
        Jv::Array(_) | Jv::Null => None,
        other => Some(other),
    }
}

fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(scalar_of(Jv::deserialize(d)?).and_then(|v| v.as_str().map(str::to_string)))
}

fn lenient_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(scalar_of(Jv::deserialize(d)?).and_then(|v| v.as_f64()))
}

fn lenient_strings<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Jv::deserialize(d)? {
        Jv::Array(items) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    })
}

fn parse_layer(v: &Jv) -> Option<StyleLayer> {
    let doc = LayerDoc::deserialize(v).ok()?;
    // Still `Jv`, not `LayerDoc`, for the genuinely polymorphic properties (color/num expressions,
    // `text-field`, `symbol-sort-key`) `LayerDoc`'s doc comment calls out.
    let paint = &v["paint"];
    let layout = &v["layout"];

    if doc.layout.visibility.as_deref() == Some("none") {
        return None;
    }

    let min_zoom = doc.minzoom;
    let max_zoom = doc.maxzoom;
    let source_layer = doc.source_layer;
    let filter = v.get("filter").and_then(parse_filter);

    Some(match doc.layer_type.as_str() {
        "background" => StyleLayer::Background(BgLayer {
            color: parse_color_expr(paint.get("background-color")).into_static_color(),
        }),
        "fill" => StyleLayer::Fill(FillLayer {
            source_layer,
            min_zoom,
            max_zoom,
            filter,
            color: parse_color_expr(paint.get("fill-color")),
            opacity: parse_num_expr_or(paint.get("fill-opacity"), 1.0),
            outline_color: parse_color_expr(paint.get("fill-outline-color")).into_static_opt(),
            translate: parse_translate(paint.get("fill-translate")),
        }),
        "line" => StyleLayer::Line(LineLayer {
            source_layer,
            min_zoom,
            max_zoom,
            filter,
            color: parse_color_expr(paint.get("line-color")),
            width: parse_num_expr(paint.get("line-width")),
            opacity: parse_num_expr_or(paint.get("line-opacity"), 1.0),
            dasharray: parse_dasharray(paint.get("line-dasharray")),
            cap: match doc.layout.line_cap.as_deref() {
                Some("round") => LineCap::Round,
                Some("square") => LineCap::Square,
                _ => LineCap::Butt,
            },
            join: match doc.layout.line_join.as_deref() {
                Some("round") => LineJoin::Round,
                Some("bevel") => LineJoin::Bevel,
                _ => LineJoin::Miter,
            },
        }),
        "symbol" => {
            let placement = match doc.layout.symbol_placement.as_deref() {
                Some("line") => TextPlacement::Line,
                _ => TextPlacement::Point,
            };
            // Place names (city/neighbourhood/country) whose style sets no maxzoom would render
            // at every zoom forever — at street-level zooms a neighbourhood name floating over
            // individual buildings is noise. Default them out at 16; explicit maxzoom wins. Only
            // place layers (`place` in OpenMapTiles, `place_labels` in shortbread): POI names are
            // exactly what street-level zooms are for.
            let is_place = matches!(source_layer.as_str(), "place" | "place_labels");
            let max_zoom =
                if placement == TextPlacement::Point && is_place && v.get("maxzoom").is_none() {
                    16.0
                } else {
                    max_zoom
                };
            StyleLayer::Symbol(SymbolLayer {
                source_layer,
                min_zoom,
                max_zoom,
                filter,
                placement,
                text_field: layout.get("text-field").and_then(parse_text_expr),
                text_size: parse_num_expr_or(layout.get("text-size"), 14.0),
                // Font names are e.g. "Noto Sans Bold": case-insensitive.
                text_font_bold: doc
                    .layout
                    .text_font
                    .iter()
                    .any(|f| f.to_ascii_lowercase().contains("bold")),
                text_transform: match doc.layout.text_transform.as_deref() {
                    Some("uppercase") => TextTransform::Uppercase,
                    Some("lowercase") => TextTransform::Lowercase,
                    _ => TextTransform::None,
                },
                sort_key: layout.get("symbol-sort-key").and_then(parse_sort_key),
                text_color: parse_color_expr(paint.get("text-color")),
                halo_color: parse_color_expr(paint.get("text-halo-color")).into_static_color(),
                halo_width: doc.paint.text_halo_width.unwrap_or(0.0) as f32,
            })
        }
        _ => return None,
    })
}

// ─── Color expressions ────────────────────────────────────────────────────────

fn parse_color_expr(v: Option<&Jv>) -> ColorExpr {
    match v {
        None => ColorExpr::Static(freya::engine::prelude::Color::TRANSPARENT),
        Some(Jv::String(s)) => {
            parse_color_str(s)
                .map(ColorExpr::Static)
                .unwrap_or(ColorExpr::Static(
                    freya::engine::prelude::Color::TRANSPARENT,
                ))
        }
        Some(Jv::Array(arr)) => parse_color_array(arr),
        Some(Jv::Object(map)) => parse_color_stops_obj(map),
        _ => ColorExpr::Static(freya::engine::prelude::Color::TRANSPARENT),
    }
}

fn parse_color_array(arr: &[Jv]) -> ColorExpr {
    let op = arr.first().and_then(|v| v.as_str()).unwrap_or("");
    match op {
        // ["interpolate", ["linear"], ["zoom"], z1, c1, z2, c2, ...]
        "interpolate" => ColorExpr::ZoomInterp(parse_zoom_color_stops(arr.get(3..).unwrap_or(&[]))),
        // ["step", ["zoom"], default, z1, c1, z2, c2, ...]
        "step" => ColorExpr::ZoomStep {
            default: parse_color_expr(arr.get(2)).into_static_color(),
            stops: parse_zoom_color_stops(arr.get(3..).unwrap_or(&[])),
        },
        // ["match", ["get", key], label1, c1, label2, c2, ..., default] — a label may be an
        // array of labels sharing one output.
        "match" => {
            let key = extract_key(arr.get(1).unwrap_or(&Jv::Null)).unwrap_or_default();
            let pairs = arr.get(2..arr.len().saturating_sub(1)).unwrap_or(&[]);
            let mut cases = Vec::new();
            for pair in pairs.chunks_exact(2) {
                let color = parse_color_expr(Some(&pair[1])).into_static_color();
                match &pair[0] {
                    Jv::Array(labels) => cases.extend(
                        labels
                            .iter()
                            .map(|label| (json_to_match_value(label), color)),
                    ),
                    label => cases.push((json_to_match_value(label), color)),
                }
            }
            ColorExpr::Match {
                key,
                cases,
                default: parse_color_expr(arr.last()).into_static_color(),
            }
        }
        // ["case", cond, val, cond, val, ..., default] — simplified: just take the default.
        "case" => ColorExpr::Static(parse_color_expr(arr.last()).into_static_color()),
        _ => ColorExpr::Static(freya::engine::prelude::Color::TRANSPARENT),
    }
}

fn parse_color_stops_obj(map: &serde_json::Map<String, Jv>) -> ColorExpr {
    let stops: Vec<(f64, freya::engine::prelude::Color)> = map
        .get("stops")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let a = s.as_array()?;
            let z = a.first()?.as_f64()?;
            Some((z, parse_color_expr(a.get(1)).into_static_color()))
        })
        .collect();

    if stops.is_empty() {
        ColorExpr::Static(freya::engine::prelude::Color::TRANSPARENT)
    } else {
        ColorExpr::ZoomInterp(stops)
    }
}

fn parse_zoom_color_stops(arr: &[Jv]) -> Vec<(f64, freya::engine::prelude::Color)> {
    arr.chunks_exact(2)
        .map(|pair| {
            let z = pair[0].as_f64().unwrap_or(0.0);
            let c = parse_color_expr(Some(&pair[1])).into_static_color();
            (z, c)
        })
        .collect()
}

// ─── Numeric expressions ──────────────────────────────────────────────────────

fn parse_num_expr(v: Option<&Jv>) -> NumExpr {
    match v {
        None => NumExpr::Static(0.0),
        Some(Jv::Number(n)) => NumExpr::Static(n.as_f64().unwrap_or(0.0)),
        Some(Jv::Array(arr)) => parse_num_array(arr),
        Some(Jv::Object(map)) => parse_num_stops_obj(map),
        _ => NumExpr::Static(0.0),
    }
}

fn parse_num_array(arr: &[Jv]) -> NumExpr {
    let op = arr.first().and_then(|v| v.as_str()).unwrap_or("");
    match op {
        "interpolate" => {
            let base = arr
                .get(1)
                .and_then(|a| a.as_array())
                .and_then(|a| {
                    if a.first().and_then(|v| v.as_str()) == Some("exponential") {
                        a.get(1)?.as_f64()
                    } else {
                        Some(1.0)
                    }
                })
                .unwrap_or(1.0);
            NumExpr::ZoomInterp {
                base,
                stops: parse_zoom_num_stops(arr.get(3..).unwrap_or(&[])),
            }
        }
        "step" => NumExpr::ZoomStep {
            default: arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0),
            stops: parse_zoom_num_stops(arr.get(3..).unwrap_or(&[])),
        },
        _ => NumExpr::Static(0.0),
    }
}

fn parse_num_stops_obj(map: &serde_json::Map<String, Jv>) -> NumExpr {
    let base = map.get("base").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let stops: Vec<(f64, f64)> = map
        .get("stops")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let a = s.as_array()?;
            Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
        })
        .collect();

    if stops.is_empty() {
        NumExpr::Static(0.0)
    } else {
        NumExpr::ZoomInterp { base, stops }
    }
}

fn parse_zoom_num_stops(arr: &[Jv]) -> Vec<(f64, f64)> {
    arr.chunks_exact(2)
        .map(|pair| {
            (
                pair[0].as_f64().unwrap_or(0.0),
                pair[1].as_f64().unwrap_or(0.0),
            )
        })
        .collect()
}

/// `fill-translate`: either a constant `[dx, dy]` pair, or `{base, stops: [[z, [dx, dy]], ...]}`
/// zoom-interpolated like other numeric paint properties, just carrying two values per stop
/// instead of one. Splits into independent per-axis `NumExpr`s so the existing zoom evaluator
/// handles each half.
fn parse_translate(v: Option<&Jv>) -> Option<(NumExpr, NumExpr)> {
    match v? {
        Jv::Array(pair) => {
            let dx = pair.first()?.as_f64()?;
            let dy = pair.get(1)?.as_f64()?;
            Some((NumExpr::Static(dx), NumExpr::Static(dy)))
        }
        Jv::Object(map) => {
            let base = map.get("base").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for stop in map.get("stops")?.as_array()? {
                let a = stop.as_array()?;
                let z = a.first()?.as_f64()?;
                let pair = a.get(1)?.as_array()?;
                xs.push((z, pair.first()?.as_f64()?));
                ys.push((z, pair.get(1)?.as_f64()?));
            }
            (!xs.is_empty()).then_some((
                NumExpr::ZoomInterp { base, stops: xs },
                NumExpr::ZoomInterp { base, stops: ys },
            ))
        }
        _ => None,
    }
}

/// Like [`parse_num_expr`] but with a caller-chosen fallback for an absent/unparseable value.
/// Needed because `parse_num_expr` falls back to `0.0` — correct for widths, but reusing it for
/// opacity would silently render the entire map fully transparent.
fn parse_num_expr_or(v: Option<&Jv>, default: f64) -> NumExpr {
    match v {
        None => NumExpr::Static(default),
        Some(Jv::Null) => NumExpr::Static(default),
        Some(_) => parse_num_expr(v),
    }
}

/// `line-dasharray`: only constant numeric arrays (all the neutrino style uses); expressions
/// yield `None` (solid line).
fn parse_dasharray(v: Option<&Jv>) -> Option<Vec<f32>> {
    let arr = v?.as_array()?;
    let intervals: Vec<f32> = arr
        .iter()
        .filter_map(|n| n.as_f64())
        .map(|n| n as f32)
        .collect();
    (intervals.len() == arr.len() && intervals.len() >= 2).then_some(intervals)
}

/// `symbol-sort-key`, reduced to the two shapes the neutrino style uses:
/// `["get", key]` and `["-", ["to-number", ["get", key], fallback]]`. Anything else → `None`.
fn parse_sort_key(v: &Jv) -> Option<SortKey> {
    let arr = v.as_array()?;
    match arr.first()?.as_str()? {
        "get" => Some(SortKey {
            key: arr.get(1)?.as_str()?.to_string(),
            negate: false,
        }),
        "-" if arr.len() == 2 => {
            let inner = arr.get(1)?.as_array()?;
            if inner.first()?.as_str()? != "to-number" {
                return None;
            }
            let get = inner.get(1)?.as_array()?;
            if get.first()?.as_str()? != "get" {
                return None;
            }
            Some(SortKey {
                key: get.get(1)?.as_str()?.to_string(),
                negate: true,
            })
        }
        _ => None,
    }
}

// ─── Text-field expressions ───────────────────────────────────────────────────

/// Parses `text-field`: `["get", key]` / `["coalesce", ...]` / `["concat", ...]` arrays, or a
/// legacy `"{key}"` template string — literal text and `{key}` placeholders concatenated in
/// order (this is what makes the motorway shield's `"{ref}"` work).
fn parse_text_expr(v: &Jv) -> Option<TextExpr> {
    match v {
        Jv::String(s) => parse_text_template(s),
        Jv::Array(arr) => {
            let op = arr.first()?.as_str()?;
            match op {
                "get" => Some(TextExpr::Get(arr.get(1)?.as_str()?.to_string())),
                "coalesce" => Some(TextExpr::Coalesce(
                    arr.iter().skip(1).filter_map(parse_text_expr).collect(),
                )),
                "concat" => Some(TextExpr::Concat(
                    arr.iter().skip(1).filter_map(parse_text_expr).collect(),
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_text_template(s: &str) -> Option<TextExpr> {
    if !s.contains('{') {
        return (!s.is_empty()).then(|| TextExpr::Static(s.to_string()));
    }

    let mut parts = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            parts.push(TextExpr::Static(rest[..open].to_string()));
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            parts.push(TextExpr::Static(rest[open..].to_string()));
            rest = "";
            break;
        };
        parts.push(TextExpr::Get(after[..close].to_string()));
        rest = &after[close + 1..];
    }
    if !rest.is_empty() {
        parts.push(TextExpr::Static(rest.to_string()));
    }

    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(TextExpr::Concat(parts)),
    }
}

// ─── Filters ───────────────────────────────────────────────────────────────────

fn parse_filter(v: &Jv) -> Option<Filter> {
    let arr = v.as_array()?;
    let op = arr.first()?.as_str()?;
    // `$type` is geometry kind (Point/LineString/Polygon), not a feature property — every caller
    // already dispatches per style-layer type against features of the matching geometry variant
    // (see `renderer::compile_tile`), so a `$type` condition is taken as always true in context.
    // Evaluating it as an ordinary property lookup would find nothing and always fail instead —
    // silently dropping every feature from any layer that filters on it (hit osm-bright's
    // `highway-minor`/`highway-minor-casing`, wiping out all residential/service/track roads).
    // An explicit always-true filter (empty `all`), not a dropped condition: dropped inside an
    // `any` it would count as false.
    let always_true = || Filter::All(Vec::new());
    // Legacy `"$type"`, or the expression form `["geometry-type"]`.
    let is_geometry_type = |v: Option<&Jv>| {
        v.is_some_and(|v| match v {
            Jv::String(s) => s == "$type",
            Jv::Array(a) => a.first().and_then(Jv::as_str) == Some("geometry-type"),
            _ => false,
        })
    };
    Some(match op {
        "all" => Filter::All(arr.iter().skip(1).filter_map(parse_filter).collect()),
        "any" => Filter::Any(arr.iter().skip(1).filter_map(parse_filter).collect()),
        "!" => Filter::Not(Box::new(parse_filter(arr.get(1)?)?)),
        // Legacy `["none", f1, f2, ...]`: none of them match.
        "none" => Filter::Not(Box::new(Filter::Any(
            arr.iter().skip(1).filter_map(parse_filter).collect(),
        ))),
        "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" | "!in" if is_geometry_type(arr.get(1)) => {
            always_true()
        }
        "==" | "!=" | "<" | "<=" | ">" | ">=" => {
            // Supports both old (["==", "key", val]) and new (["==", ["get", "key"], val]) syntax.
            let key = extract_key(arr.get(1)?)?;
            let val = json_to_match_value(arr.get(2)?);
            match op {
                "==" => Filter::Eq(key, val),
                "!=" => Filter::Neq(key, val),
                "<" => Filter::Lt(key, val.as_f64()?),
                "<=" => Filter::Lte(key, val.as_f64()?),
                ">" => Filter::Gt(key, val.as_f64()?),
                ">=" => Filter::Gte(key, val.as_f64()?),
                _ => unreachable!(),
            }
        }
        "in" => {
            let key = extract_key(arr.get(1)?)?;
            Filter::In(key, parse_match_values(arr.get(2..).unwrap_or(&[])))
        }
        "!in" => {
            let key = extract_key(arr.get(1)?)?;
            Filter::Not(Box::new(Filter::In(
                key,
                parse_match_values(arr.get(2..).unwrap_or(&[])),
            )))
        }
        "has" => Filter::Has(extract_key(arr.get(1)?)?),
        "!has" => Filter::NotHas(extract_key(arr.get(1)?)?),
        _ => return None,
    })
}

/// Handles both `["in", "key", v1, v2, ...]` and `["in", ["get", "key"], ["literal", [...]]]`.
fn parse_match_values(rest: &[Jv]) -> Vec<MatchValue> {
    if let [Jv::Array(lit)] = rest
        && lit.first().and_then(|v| v.as_str()) == Some("literal")
    {
        return lit
            .get(1)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .map(json_to_match_value)
            .collect();
    }
    rest.iter().map(json_to_match_value).collect()
}

fn extract_key(v: &Jv) -> Option<String> {
    match v {
        Jv::String(s) => Some(s.clone()),
        Jv::Array(a) if a.first().and_then(|v| v.as_str()) == Some("get") => {
            a.get(1)?.as_str().map(str::to_string)
        }
        _ => None,
    }
}

fn json_to_match_value(v: &Jv) -> MatchValue {
    match v {
        Jv::String(s) => MatchValue::Str(s.clone()),
        Jv::Number(n) => MatchValue::Num(n.as_f64().unwrap_or(0.0)),
        Jv::Bool(b) => MatchValue::Bool(*b),
        _ => MatchValue::Null,
    }
}

// ─── Small helpers shared with `default.rs` ───────────────────────────────────

trait IntoStaticColor {
    fn into_static_color(self) -> freya::engine::prelude::Color;
}

impl IntoStaticColor for ColorExpr {
    fn into_static_color(self) -> freya::engine::prelude::Color {
        match self {
            ColorExpr::Static(c) => c,
            ColorExpr::ZoomInterp(stops) => stops
                .first()
                .map(|(_, c)| *c)
                .unwrap_or(freya::engine::prelude::Color::TRANSPARENT),
            ColorExpr::ZoomStep { default, .. } => default,
            ColorExpr::Match { default, .. } => default,
        }
    }
}

trait IntoStaticOpt {
    fn into_static_opt(self) -> Option<freya::engine::prelude::Color>;
}

impl IntoStaticOpt for ColorExpr {
    fn into_static_opt(self) -> Option<freya::engine::prelude::Color> {
        match self {
            ColorExpr::Static(c) if c != freya::engine::prelude::Color::TRANSPARENT => Some(c),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use freya::engine::prelude::Color;
    use mvt_reader::feature::Value;

    use super::*;
    use crate::util::renderer::Properties;

    fn props(pairs: &[(&str, Value)]) -> Properties {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn filter(json: &str) -> Filter {
        parse_filter(&serde_json::from_str(json).unwrap()).unwrap()
    }

    #[test]
    fn match_expands_array_labels_and_numbers() {
        let expr = parse_color_expr(Some(
            &serde_json::from_str(
                r##"["match", ["get", "class"], ["motorway", "trunk"], "#ff0000", 3, "#00ff00", "#0000ff"]"##,
            )
            .unwrap(),
        ));
        let color = |v: Value| expr.eval(&props(&[("class", v)]), 14.0);
        let red = Color::from_rgb(255, 0, 0);
        assert_eq!(color(Value::String("motorway".into())), red);
        assert_eq!(color(Value::String("trunk".into())), red);
        assert_eq!(color(Value::Int(3)), Color::from_rgb(0, 255, 0));
        assert_eq!(
            color(Value::String("path".into())),
            Color::from_rgb(0, 0, 255)
        );
    }

    #[test]
    fn none_negates_every_branch() {
        let f = filter(r#"["none", ["==", "a", 1], ["==", "b", 1]]"#);
        assert!(f.eval(&props(&[("a", Value::Int(0)), ("b", Value::Int(0))])));
        assert!(!f.eval(&props(&[("a", Value::Int(1))])));
        assert!(!f.eval(&props(&[("b", Value::Int(1))])));
    }

    #[test]
    fn geometry_type_conditions_are_true_even_inside_any() {
        let empty = props(&[]);
        assert!(filter(r#"["any", ["==", "$type", "Polygon"]]"#).eval(&empty));
        assert!(filter(r#"["in", "$type", "Point", "LineString"]"#).eval(&empty));
        assert!(filter(r#"["==", ["geometry-type"], "Point"]"#).eval(&empty));
    }

    #[test]
    fn zoom_function_layout_values_do_not_drop_the_layer() {
        let style = parse_style(
            r#"{"sources": {}, "layers": [{
                "id": "shield", "type": "symbol", "source-layer": "transportation_name",
                "layout": {
                    "symbol-placement": {"base": 1, "stops": [[10, "point"], [11, "line"]]},
                    "text-field": "{ref}",
                    "text-font": ["Noto Sans Bold"]
                }
            }]}"#,
        )
        .unwrap();
        let [StyleLayer::Symbol(symbol)] = style.layers.as_slice() else {
            panic!("shield layer dropped");
        };
        assert!(symbol.placement == TextPlacement::Line);
        assert!(symbol.text_font_bold);
    }
}
