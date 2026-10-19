//! Public-transport overlay for the OpenMapTiles schema (the data.gouv.fr tiles the app uses):
//! fades the road network and draws rail, metro, tram and bus-lane lines plus station names on
//! top of the base style, and keeps only transit POI icons. Everything comes from data the tiles
//! already carry (`transportation` class `rail`/`transit`/`busway`, `poi` class
//! `railway`/`bus`), so no extra source is fetched. On a non-OpenMapTiles base (the shortbread
//! fallback style) the added layers simply match nothing.

use freya::engine::prelude::Color;

use super::{
    ColorExpr, Filter, LineCap, LineJoin, LineLayer, MatchValue, NumExpr, StyleLayer, StyleSpec,
    SymbolLayer, TextExpr, TextPlacement, TextTransform,
};

/// Bus lines clutter the map below this zoom (one per street), so they're drawn — and tappable —
/// only from here.
pub const BUS_LINES_MIN_ZOOM: f64 = 15.0;

/// Opacity multiplier for the base style's own `transportation` lines, so transit stands out.
const ROAD_FADE: f64 = 0.35;

const RAIL: Color = Color::from_rgb(70, 76, 86);
const SUBWAY: Color = Color::from_rgb(30, 91, 216);
const TRAM: Color = Color::from_rgb(214, 51, 108);
const OTHER_TRANSIT: Color = Color::from_rgb(123, 75, 183);
const BUSWAY: Color = Color::from_rgb(240, 140, 0);
const CASING: Color = Color::from_argb(230, 255, 255, 255);
const STATION_TEXT: Color = Color::from_rgb(25, 50, 120);

/// `base` with the transit overlay applied.
pub fn with_transit(base: &StyleSpec) -> StyleSpec {
    let mut layers: Vec<StyleLayer> = base
        .layers
        .iter()
        .map(|layer| match layer {
            StyleLayer::Line(line) if line.source_layer == "transportation" => {
                let mut faded = line.clone();
                faded.opacity = line.opacity.scaled(ROAD_FADE);
                StyleLayer::Line(faded)
            }
            other => other.clone(),
        })
        .collect();

    let rail = Filter::All(vec![
        Filter::Eq("class".into(), str_value("rail")),
        Filter::NotHas("service".into()),
    ]);
    let transit = |subclasses: &[&str]| {
        Filter::All(vec![
            Filter::Eq("class".into(), str_value("transit")),
            Filter::In(
                "subclass".into(),
                subclasses.iter().map(|s| str_value(s)).collect(),
            ),
        ])
    };
    let other_transit = Filter::All(vec![
        Filter::Eq("class".into(), str_value("transit")),
        Filter::Not(Box::new(Filter::In(
            "subclass".into(),
            ["subway", "tram", "light_rail"]
                .iter()
                .map(|s| str_value(s))
                .collect(),
        ))),
    ]);

    // Casing first, then the colored line over it, per kind; drawn after (above) the whole base.
    let line_kinds = [
        (
            rail,
            RAIL,
            6.0,
            [(6.0, 0.8), (12.0, 1.8), (16.0, 3.5), (19.0, 6.0)],
        ),
        (
            transit(&["subway"]),
            SUBWAY,
            10.0,
            [(10.0, 1.4), (13.0, 2.6), (16.0, 4.5), (19.0, 8.0)],
        ),
        (
            transit(&["tram", "light_rail"]),
            TRAM,
            11.0,
            [(11.0, 1.0), (13.0, 2.0), (16.0, 3.5), (19.0, 6.5)],
        ),
        (
            other_transit,
            OTHER_TRANSIT,
            11.0,
            [(11.0, 1.0), (13.0, 2.0), (16.0, 3.5), (19.0, 6.0)],
        ),
        (
            Filter::Eq("class".into(), str_value("busway")),
            BUSWAY,
            13.0,
            [(13.0, 1.0), (16.0, 2.5), (19.0, 4.5), (20.0, 5.0)],
        ),
    ];
    for (filter, color, min_zoom, widths) in line_kinds {
        let casing_widths = widths.map(|(z, w)| (z, w + 2.5));
        layers.push(transit_line(
            filter.clone(),
            CASING,
            min_zoom,
            &casing_widths,
        ));
        layers.push(transit_line(filter, color, min_zoom, &widths));
    }

    // rusty_map's own tiles: each line in its own colour (`route_1_colour`, the first and most
    // important line on that stretch), drawn over the generic per-mode lines above, which still
    // show wherever a track carries no route. Other tile sources have no `transit_lines`, so these
    // match nothing there.
    let route_kinds = [
        (
            &["train"][..],
            RAIL,
            6.0,
            [(6.0, 1.2), (12.0, 2.4), (16.0, 4.0), (19.0, 6.5)],
        ),
        (
            &["subway"][..],
            SUBWAY,
            9.0,
            [(9.0, 1.6), (13.0, 3.0), (16.0, 5.0), (19.0, 8.5)],
        ),
        (
            &["tram", "light_rail", "monorail", "funicular"][..],
            TRAM,
            10.0,
            [(10.0, 1.3), (13.0, 2.4), (16.0, 4.0), (19.0, 7.0)],
        ),
        (
            &["bus", "trolleybus"][..],
            BUSWAY,
            BUS_LINES_MIN_ZOOM,
            [(15.0, 1.2), (17.0, 2.2), (19.0, 3.5), (20.0, 4.0)],
        ),
    ];
    for (modes, fallback, min_zoom, widths) in route_kinds {
        let filter = Filter::In(
            "mode".into(),
            modes.iter().map(|mode| str_value(mode)).collect(),
        );
        let casing_widths = widths.map(|(z, w)| (z, w + 2.5));
        layers.push(line_layer(
            ROUTE_SOURCE,
            filter.clone(),
            ColorExpr::Static(CASING),
            min_zoom,
            &casing_widths,
        ));
        layers.push(line_layer(
            ROUTE_SOURCE,
            filter,
            ColorExpr::Property {
                key: "route_1_colour".into(),
                default: fallback,
            },
            min_zoom,
            &widths,
        ));
    }

    layers.push(station_labels(
        &["station", "halt"],
        11.0,
        [(11.0, 11.0), (16.0, 14.0)],
    ));
    layers.push(station_labels(
        &["subway", "tram_stop"],
        14.0,
        [(14.0, 11.0), (18.0, 13.0)],
    ));

    StyleSpec {
        layers,
        tile_url_template: base.tile_url_template.clone(),
        sprite_url: base.sprite_url.clone(),
        min_zoom: base.min_zoom,
        max_real_zoom: base.max_real_zoom,
        poi_filter: Some(Filter::In(
            "class".into(),
            ["railway", "bus", "ferry_terminal", "aerialway"]
                .iter()
                .map(|s| str_value(s))
                .collect(),
        )),
        icon_min_zoom: base.icon_min_zoom,
        pick_transit_lines: true,
    }
}

fn str_value(value: &str) -> MatchValue {
    MatchValue::Str(value.to_string())
}

/// rusty_map's own tiles' layer of transit lines with their routes (see `tools/tiles`).
const ROUTE_SOURCE: &str = "transit_lines";

fn transit_line(filter: Filter, color: Color, min_zoom: f64, widths: &[(f64, f64)]) -> StyleLayer {
    line_layer(
        "transportation",
        filter,
        ColorExpr::Static(color),
        min_zoom,
        widths,
    )
}

fn line_layer(
    source_layer: &str,
    filter: Filter,
    color: ColorExpr,
    min_zoom: f64,
    widths: &[(f64, f64)],
) -> StyleLayer {
    StyleLayer::Line(LineLayer {
        source_layer: source_layer.into(),
        min_zoom,
        max_zoom: 24.0,
        filter: Some(filter),
        color,
        width: NumExpr::ZoomInterp {
            base: 1.4,
            stops: widths.to_vec(),
        },
        opacity: NumExpr::Static(1.0),
        dasharray: None,
        cap: LineCap::Round,
        join: LineJoin::Round,
    })
}

fn station_labels(subclasses: &[&str], min_zoom: f64, sizes: [(f64, f64); 2]) -> StyleLayer {
    StyleLayer::Symbol(SymbolLayer {
        source_layer: "poi".into(),
        min_zoom,
        max_zoom: 24.0,
        filter: Some(Filter::All(vec![
            Filter::Eq("class".into(), str_value("railway")),
            Filter::In(
                "subclass".into(),
                subclasses.iter().map(|s| str_value(s)).collect(),
            ),
            Filter::Has("name".into()),
        ])),
        text_field: Some(TextExpr::Coalesce(vec![
            TextExpr::Get("name:fr".into()),
            TextExpr::Get("name".into()),
        ])),
        text_size: NumExpr::ZoomInterp {
            base: 1.0,
            stops: sizes.to_vec(),
        },
        text_font_bold: true,
        text_transform: TextTransform::None,
        placement: TextPlacement::Point,
        sort_key: None,
        text_color: ColorExpr::Static(STATION_TEXT),
        halo_color: Color::WHITE,
        halo_width: 1.5,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_roads_and_adds_transit_layers() {
        let mut base = crate::style::default::default_style();
        base.layers.push(transit_line(
            Filter::Eq("class".into(), str_value("primary")),
            Color::WHITE,
            0.0,
            &[(0.0, 1.0)],
        ));
        let transit = with_transit(&base);

        let opacity = |layer: &StyleLayer| match layer {
            StyleLayer::Line(line) => Some(line.opacity.eval(14.0)),
            _ => None,
        };
        // The base road layer keeps its place, faded.
        let road = opacity(&transit.layers[base.layers.len() - 1]).unwrap();
        assert!((road - ROAD_FADE).abs() < 1e-9);
        // Casing + line for each of the 5 track kinds and 4 route kinds, plus 2 station labels.
        assert_eq!(transit.layers.len(), base.layers.len() + 20);
        assert!(transit.poi_filter.is_some());
    }
}
