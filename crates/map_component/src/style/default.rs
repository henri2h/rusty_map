//! Hardcoded fallback style for the `shortbread_v1` schema, used when no style JSON could be
//! fetched/parsed. Ported from `shom_client`'s `style_shortbread.rs`, trimmed to `Background`/
//! `Fill`/`Line` layers (no place/street labels — no text/font pipeline exists yet).

use freya::engine::prelude::Color;

use super::{
    BgLayer, ColorExpr, FillLayer, Filter, LineCap, LineJoin, LineLayer, MatchValue, NumExpr,
    StyleLayer, StyleSpec,
};

pub fn default_style() -> StyleSpec {
    let ocean = Color::from_rgb(184, 212, 236);

    let mut layers = vec![
        StyleLayer::Background(BgLayer { color: ocean }),
        fill("land", Color::from_rgb(228, 221, 212), 0.0),
        fill_match(
            "land",
            "kind",
            &[
                ("forest", Color::from_rgb(90, 160, 80)),
                ("wood", Color::from_rgb(90, 160, 80)),
                ("grass", Color::from_rgb(168, 214, 110)),
                ("meadow", Color::from_rgb(168, 214, 110)),
                ("farmland", Color::from_rgb(220, 215, 170)),
                ("farmyard", Color::from_rgb(210, 200, 155)),
                ("orchard", Color::from_rgb(140, 195, 100)),
                ("vineyard", Color::from_rgb(155, 200, 110)),
                ("scrub", Color::from_rgb(148, 188, 112)),
                ("heath", Color::from_rgb(162, 188, 112)),
                ("beach", Color::from_rgb(240, 222, 148)),
                ("sand", Color::from_rgb(240, 222, 148)),
                ("wetland", Color::from_rgb(150, 195, 185)),
                ("saltmarsh", Color::from_rgb(140, 190, 175)),
                ("tidalflat", Color::from_rgb(168, 200, 190)),
                ("glacier", Color::from_rgb(220, 240, 255)),
                ("urban", Color::from_rgb(216, 208, 200)),
                ("residential", Color::from_rgb(220, 212, 204)),
            ],
            Color::from_rgb(228, 221, 212),
        ),
        fill("water_polygons", ocean, 0.0),
        line("water_lines", None, Color::from_rgb(144, 180, 208), 1.0),
        line(
            "boundaries",
            Some(Filter::Lte("admin_level".to_string(), 2.0)),
            Color::from_argb(180, 160, 100, 120),
            1.5,
        ),
        line(
            "boundaries",
            Some(Filter::All(vec![
                Filter::Gt("admin_level".to_string(), 2.0),
                Filter::Lte("admin_level".to_string(), 4.0),
            ])),
            Color::from_argb(140, 160, 100, 120),
            1.0,
        ),
    ];

    // Roads: one `line` layer per tier, tiered by `kind` — no separate casing pass.
    let road_tiers: &[(&[&str], Color, f64)] = &[
        (
            &["motorway", "motorway_link"],
            Color::from_rgb(252, 176, 60),
            10.0,
        ),
        (
            &["trunk", "trunk_link"],
            Color::from_rgb(252, 200, 100),
            8.0,
        ),
        (
            &["primary", "primary_link"],
            Color::from_rgb(255, 240, 120),
            7.0,
        ),
        (
            &["secondary", "secondary_link"],
            Color::from_rgb(255, 255, 255),
            6.0,
        ),
        (
            &["tertiary", "tertiary_link"],
            Color::from_rgb(255, 255, 255),
            5.0,
        ),
        (
            &["unclassified", "residential", "living_street"],
            Color::from_rgb(248, 244, 240),
            4.0,
        ),
        (&["service", "track"], Color::from_rgb(248, 244, 240), 2.5),
    ];
    for (kinds, color, width) in road_tiers {
        layers.push(line("streets", road_filter(kinds), *color, *width));
    }

    StyleSpec {
        layers,
        tile_url_template: DEFAULT_TILE_URL_TEMPLATE.to_string(),
        sprite_url: Some(DEFAULT_SPRITE_URL.to_string()),
        min_zoom: DEFAULT_MIN_ZOOM,
        max_real_zoom: DEFAULT_MAX_REAL_ZOOM,
        poi_filter: None,
        icon_min_zoom: crate::util::labels::ICON_MIN_ZOOM,
        pick_transit_lines: false,
    }
}

/// Vector tile source matching this fallback style's `shortbread_v1` schema.
pub const DEFAULT_TILE_URL_TEMPLATE: &str =
    "https://vector.openstreetmap.org/shortbread_v1/{z}/{x}/{y}.mvt";

/// Sprite base URL matching this fallback style's icons.
pub const DEFAULT_SPRITE_URL: &str =
    "https://vector.openstreetmap.org/styles/shortbread/sprites/basics/sprites";

/// Fallback zoom bounds used when a style's vector source specifies neither `minzoom` nor
/// `maxzoom` (inline or via TileJSON) — matches `shortbread_v1`'s actual range (confirmed
/// directly: `z15` and up return HTTP 410 on `vector.openstreetmap.org`'s shortbread endpoint).
pub const DEFAULT_MIN_ZOOM: f32 = 4.0;
pub const DEFAULT_MAX_REAL_ZOOM: u32 = 14;

fn fill(source_layer: &str, color: Color, min_zoom: f64) -> StyleLayer {
    StyleLayer::Fill(FillLayer {
        source_layer: source_layer.to_string(),
        min_zoom,
        max_zoom: 24.0,
        filter: None,
        color: ColorExpr::Static(color),
        opacity: NumExpr::Static(1.0),
        outline_color: None,
        translate: None,
    })
}

fn fill_match(
    source_layer: &str,
    key: &str,
    cases: &[(&str, Color)],
    default: Color,
) -> StyleLayer {
    StyleLayer::Fill(FillLayer {
        source_layer: source_layer.to_string(),
        min_zoom: 0.0,
        max_zoom: 24.0,
        filter: None,
        color: ColorExpr::Match {
            key: key.to_string(),
            cases: cases
                .iter()
                .map(|(k, c)| (MatchValue::Str(k.to_string()), *c))
                .collect(),
            default,
        },
        opacity: NumExpr::Static(1.0),
        outline_color: None,
        translate: None,
    })
}

fn line(source_layer: &str, filter: Option<Filter>, color: Color, width: f64) -> StyleLayer {
    StyleLayer::Line(LineLayer {
        source_layer: source_layer.to_string(),
        min_zoom: 0.0,
        max_zoom: 24.0,
        filter,
        color: ColorExpr::Static(color),
        width: NumExpr::Static(width),
        opacity: NumExpr::Static(1.0),
        dasharray: None,
        cap: LineCap::Round,
        join: LineJoin::Round,
    })
}

fn road_filter(kinds: &[&str]) -> Option<Filter> {
    Some(Filter::In(
        "kind".to_string(),
        kinds
            .iter()
            .map(|k| MatchValue::Str(k.to_string()))
            .collect(),
    ))
}
