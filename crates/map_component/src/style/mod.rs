//! A small MapLibre/Mapbox-GL-style-inspired styling model: a `StyleSpec` is an ordered list of
//! layers, each optionally filtering and coloring features from a named MVT source layer using a
//! tiny expression language (`ColorExpr`/`NumExpr`/`Filter`). Ported from `shom_client`'s style
//! system (`src/map/style/mod.rs`), trimmed to what `rusty_map` actually renders — layers are
//! `Background`/`Fill`/`Line`/`Symbol` (text labels, extracted at compile time by
//! `map_component::util::renderer`'s internal `labels` module and placed at paint time by
//! `map_component::util::labels`).

use freya::engine::prelude::Color;
use mvt_reader::feature::Value;

use crate::util::renderer::Properties;

pub mod default;
pub mod parser;

pub struct StyleSpec {
    pub layers: Vec<StyleLayer>,
    /// Vector tile URL, with `{z}`/`{x}`/`{y}` placeholders, resolved from the style JSON's
    /// `sources` (either an inline `tiles` template or a further-fetched TileJSON's `tiles`).
    pub tile_url_template: String,
    /// Base sprite URL from the style JSON's `sprite` field (`.json`/`.png` suffixes select
    /// index/atlas), if any.
    pub sprite_url: Option<String>,
    /// User-facing zoom floor, from the vector source's `minzoom` (inline or TileJSON).
    pub min_zoom: f32,
    /// The tile source's actual deepest zoom level, from the vector source's `maxzoom` (inline
    /// or TileJSON) — `Viewport` never requests real tiles past this; digital over-zoom past it
    /// is a UI-only concern layered on top (see `viewport::max_zoom_ui`).
    pub max_real_zoom: u32,
}

pub enum StyleLayer {
    Background(BgLayer),
    Fill(FillLayer),
    Line(LineLayer),
    Symbol(SymbolLayer),
}

pub struct BgLayer {
    pub color: Color,
}

pub struct FillLayer {
    pub source_layer: String,
    pub min_zoom: f64,
    pub max_zoom: f64,
    pub filter: Option<Filter>,
    pub color: ColorExpr,
    pub opacity: NumExpr,
    pub outline_color: Option<Color>,
    /// `fill-translate`: screen-pixel (dx, dy) offset, e.g. the pseudo-3D "building-top" overlay
    /// a couple pixels up-left of the base building fill. `None` for the common no-offset case.
    pub translate: Option<(NumExpr, NumExpr)>,
}

pub struct LineLayer {
    pub source_layer: String,
    pub min_zoom: f64,
    pub max_zoom: f64,
    pub filter: Option<Filter>,
    pub color: ColorExpr,
    pub width: NumExpr,
    pub opacity: NumExpr,
    /// Constant dash intervals in multiples of the line's width (MapLibre `line-dasharray`
    /// semantics); `None` = solid.
    pub dasharray: Option<Vec<f32>>,
    pub cap: LineCap,
    pub join: LineJoin,
}

/// Own enums rather than Skia's `PaintCap`/`PaintJoin`, so the style model stays parseable and
/// testable without a Skia dependency; the renderer maps them.
#[derive(Clone, Copy, PartialEq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// A text-label layer. Only the properties the neutrino style actually uses are modeled —
/// no icons, offsets, wrapping, or letter-spacing yet.
pub struct SymbolLayer {
    pub source_layer: String,
    pub min_zoom: f64,
    pub max_zoom: f64,
    pub filter: Option<Filter>,
    pub text_field: Option<TextExpr>,
    pub text_size: NumExpr,
    /// `text-font` contains a `*_bold` face.
    pub text_font_bold: bool,
    pub text_transform: TextTransform,
    pub placement: TextPlacement,
    /// `symbol-sort-key`, reduced to a numeric property lookup (see [`SortKey`]).
    pub sort_key: Option<SortKey>,
    pub text_color: ColorExpr,
    pub halo_color: Color,
    /// `text-halo-blur` isn't modeled: it's a sub-pixel softening in practice (osm-bright: 0.5-1px),
    /// and a blur mask filter on every halo forced Skia's uncached slow path for those labels'
    /// glyphs every frame.
    pub halo_width: f32,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum TextPlacement {
    #[default]
    Point,
    Line,
}

/// The two `symbol-sort-key` shapes neutrino uses: `["get", key]` and
/// `["-", ["to-number", ["get", key], 0]]` (descending sort by e.g. population).
pub struct SortKey {
    pub key: String,
    pub negate: bool,
}

impl SortKey {
    /// Lower value = higher placement priority (MapLibre sort-key semantics).
    pub fn eval(&self, props: &Properties) -> f64 {
        let value = props.get(&self.key).and_then(value_as_f64).unwrap_or(0.0);
        if self.negate { -value } else { value }
    }
}

/// A `text-field` expression: a literal, a property lookup, first-non-empty of several, or a
/// concatenation (which is also how `"{ref}"`-style templates parse: literal and `Get` parts
/// concatenated in order).
pub enum TextExpr {
    Static(String),
    Get(String),
    Coalesce(Vec<TextExpr>),
    Concat(Vec<TextExpr>),
}

impl TextExpr {
    pub fn eval(&self, props: &Properties) -> String {
        match self {
            TextExpr::Static(s) => s.clone(),
            TextExpr::Get(key) => props.get(key).map(value_as_str).unwrap_or("").to_string(),
            TextExpr::Coalesce(parts) => parts
                .iter()
                .map(|part| part.eval(props))
                .find(|s| !s.is_empty())
                .unwrap_or_default(),
            TextExpr::Concat(parts) => parts.iter().map(|part| part.eval(props)).collect(),
        }
    }
}

// ─── Expressions ───────────────────────────────────────────────────────────

pub enum ColorExpr {
    Static(Color),
    /// `["match", ["get", key], label, color, ..., default]` — a label may be a string, number
    /// or bool (and an array of labels expands to one case each).
    Match {
        key: String,
        cases: Vec<(MatchValue, Color)>,
        default: Color,
    },
    ZoomInterp(Vec<(f64, Color)>),
    ZoomStep {
        default: Color,
        stops: Vec<(f64, Color)>,
    },
}

pub enum NumExpr {
    Static(f64),
    ZoomInterp {
        base: f64,
        stops: Vec<(f64, f64)>,
    },
    ZoomStep {
        default: f64,
        stops: Vec<(f64, f64)>,
    },
}

// ─── Filters ─────────────────────────────────────────────────────────────────

pub enum Filter {
    All(Vec<Filter>),
    Any(Vec<Filter>),
    Not(Box<Filter>),
    Eq(String, MatchValue),
    Neq(String, MatchValue),
    In(String, Vec<MatchValue>),
    Lt(String, f64),
    Lte(String, f64),
    Gt(String, f64),
    Gte(String, f64),
    Has(String),
    NotHas(String),
}

#[derive(Clone)]
pub enum MatchValue {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

impl MatchValue {
    fn eq_value(&self, v: &Value) -> bool {
        match (self, v) {
            (MatchValue::Str(s), Value::String(vs)) => s == vs,
            (MatchValue::Num(n), _) => value_as_f64(v).is_some_and(|f| (n - f).abs() < 1e-9),
            (MatchValue::Bool(b), Value::Bool(vb)) => b == vb,
            _ => false,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        if let MatchValue::Num(n) = self {
            Some(*n)
        } else {
            None
        }
    }
}

fn value_as_str(v: &Value) -> &str {
    match v {
        Value::String(s) => s,
        _ => "",
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match *v {
        Value::Float(f) => Some(f as f64),
        Value::Double(f) => Some(f),
        Value::Int(i) => Some(i as f64),
        Value::UInt(i) => Some(i as f64),
        Value::SInt(i) => Some(i as f64),
        _ => None,
    }
}

// ─── Evaluators ──────────────────────────────────────────────────────────────

impl ColorExpr {
    pub fn eval(&self, props: &Properties, zoom: f64) -> Color {
        match self {
            ColorExpr::Static(c) => *c,
            ColorExpr::Match {
                key,
                cases,
                default,
            } => props
                .get(key)
                .and_then(|v| cases.iter().find(|(label, _)| label.eq_value(v)))
                .map_or(*default, |(_, c)| *c),
            ColorExpr::ZoomInterp(stops) => interp_color(stops, zoom),
            ColorExpr::ZoomStep { default, stops } => step(*default, stops, zoom),
        }
    }
}

impl NumExpr {
    pub fn eval(&self, zoom: f64) -> f64 {
        match self {
            NumExpr::Static(v) => *v,
            NumExpr::ZoomInterp { base, stops } => interp_num(*base, stops, zoom),
            NumExpr::ZoomStep { default, stops } => step(*default, stops, zoom),
        }
    }
}

impl Filter {
    pub fn eval(&self, props: &Properties) -> bool {
        match self {
            Filter::All(fs) => fs.iter().all(|f| f.eval(props)),
            Filter::Any(fs) => fs.iter().any(|f| f.eval(props)),
            Filter::Not(f) => !f.eval(props),
            Filter::Eq(k, v) => props.get(k).is_some_and(|pv| v.eq_value(pv)),
            Filter::Neq(k, v) => !props.get(k).is_some_and(|pv| v.eq_value(pv)),
            Filter::In(k, vs) => props
                .get(k)
                .is_some_and(|pv| vs.iter().any(|v| v.eq_value(pv))),
            Filter::Lt(k, n) => props.get(k).and_then(value_as_f64).is_some_and(|v| v < *n),
            Filter::Lte(k, n) => props.get(k).and_then(value_as_f64).is_some_and(|v| v <= *n),
            Filter::Gt(k, n) => props.get(k).and_then(value_as_f64).is_some_and(|v| v > *n),
            Filter::Gte(k, n) => props.get(k).and_then(value_as_f64).is_some_and(|v| v >= *n),
            Filter::Has(k) => props.contains_key(k),
            Filter::NotHas(k) => !props.contains_key(k),
        }
    }
}

fn step<T: Copy>(default: T, stops: &[(f64, T)], zoom: f64) -> T {
    let mut out = default;
    for (z, v) in stops {
        if zoom >= *z {
            out = *v;
        }
    }
    out
}

fn interp_color(stops: &[(f64, Color)], zoom: f64) -> Color {
    let Some(&(first_z, first_c)) = stops.first() else {
        return Color::BLACK;
    };
    if zoom <= first_z {
        return first_c;
    }
    let &(last_z, last_c) = stops.last().unwrap();
    if zoom >= last_z {
        return last_c;
    }
    for w in stops.windows(2) {
        let (z0, c0) = w[0];
        let (z1, c1) = w[1];
        if zoom <= z1 {
            let t = ((zoom - z0) / (z1 - z0)) as f32;
            return lerp_color(c0, c1, t);
        }
    }
    last_c
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color::from_argb(
        lerp_u8(a.a(), b.a(), t),
        lerp_u8(a.r(), b.r(), t),
        lerp_u8(a.g(), b.g(), t),
        lerp_u8(a.b(), b.b(), t),
    )
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

fn interp_num(base: f64, stops: &[(f64, f64)], zoom: f64) -> f64 {
    let Some(&(first_z, first_v)) = stops.first() else {
        return 0.0;
    };
    if zoom <= first_z {
        return first_v;
    }
    let &(last_z, last_v) = stops.last().unwrap();
    if zoom >= last_z {
        return last_v;
    }
    for w in stops.windows(2) {
        let (z0, v0) = w[0];
        let (z1, v1) = w[1];
        if zoom <= z1 {
            let t = (zoom - z0) / (z1 - z0);
            let factor = if (base - 1.0).abs() < 1e-9 {
                t
            } else {
                (base.powf(t) - 1.0) / (base - 1.0)
            };
            return v0 + (v1 - v0) * factor;
        }
    }
    last_v
}

// ─── Color string parsing (used by the JSON importer) ────────────────────────

pub fn parse_color_str(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    if s.starts_with("rgb") {
        return parse_rgb_fn(s);
    }
    if s.starts_with("hsl") {
        return parse_hsl_fn(s);
    }
    named_color(s)
}

fn parse_hex(hex: &str) -> Option<Color> {
    let component = |slice: &str| {
        let full = if slice.len() == 1 {
            slice.repeat(2)
        } else {
            slice.to_string()
        };
        u8::from_str_radix(&full, 16).ok()
    };
    match hex.len() {
        3 => Some(Color::from_rgb(
            component(&hex[0..1])?,
            component(&hex[1..2])?,
            component(&hex[2..3])?,
        )),
        // CSS `#RGBA`: alpha is the *last* digit.
        4 => Some(Color::from_argb(
            component(&hex[3..4])?,
            component(&hex[0..1])?,
            component(&hex[1..2])?,
            component(&hex[2..3])?,
        )),
        6 => Some(Color::from_rgb(
            component(&hex[0..2])?,
            component(&hex[2..4])?,
            component(&hex[4..6])?,
        )),
        8 => Some(Color::from_argb(
            component(&hex[6..8])?,
            component(&hex[0..2])?,
            component(&hex[2..4])?,
            component(&hex[4..6])?,
        )),
        _ => None,
    }
}

fn parse_rgb_fn(s: &str) -> Option<Color> {
    let inner = s.split('(').nth(1)?.trim_end_matches(')');
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();

    // `50%` scales to 0..255; a bare number is already 0..255.
    let channel = |p: &str| -> Option<u8> {
        let v = if let Some(pct) = p.strip_suffix('%') {
            pct.trim().parse::<f64>().ok()? * 255.0 / 100.0
        } else {
            p.parse::<f64>().ok()?
        };
        Some(v.round().clamp(0.0, 255.0) as u8)
    };
    // Alpha is a 0..1 fraction (or a percentage), unlike the color channels.
    let alpha = |p: &str| -> Option<u8> {
        let v = if let Some(pct) = p.strip_suffix('%') {
            pct.trim().parse::<f64>().ok()? / 100.0
        } else {
            p.parse::<f64>().ok()?
        };
        Some((v.clamp(0.0, 1.0) * 255.0).round() as u8)
    };

    match parts.as_slice() {
        [r, g, b] => Some(Color::from_rgb(channel(r)?, channel(g)?, channel(b)?)),
        [r, g, b, a] => Some(Color::from_argb(
            alpha(a)?,
            channel(r)?,
            channel(g)?,
            channel(b)?,
        )),
        _ => None,
    }
}

/// `hsl(h, s%, l%)` / `hsla(h, s%, l%, a)` — the CSS color space osm-bright's style JSON uses
/// throughout (`hsla(30, 19%, 90%, 0.4)` etc); without this every layer painted with it fell
/// through to `named_color` (miss) and rendered fully transparent.
fn parse_hsl_fn(s: &str) -> Option<Color> {
    let inner = s.split('(').nth(1)?.trim_end_matches(')');
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();

    let hue = parts.first()?.trim_end_matches("deg").parse::<f64>().ok()?;
    let saturation = parts.get(1)?.trim_end_matches('%').parse::<f64>().ok()? / 100.0;
    let lightness = parts.get(2)?.trim_end_matches('%').parse::<f64>().ok()? / 100.0;
    let alpha = match parts.get(3) {
        Some(a) => a
            .strip_suffix('%')
            .map(|pct| pct.parse::<f64>().ok().map(|v| v / 100.0))
            .unwrap_or_else(|| a.parse::<f64>().ok())?,
        None => 1.0,
    };

    let (r, g, b) = hsl_to_rgb(hue, saturation.clamp(0.0, 1.0), lightness.clamp(0.0, 1.0));
    Some(Color::from_argb(
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        r,
        g,
        b,
    ))
}

/// Standard CSS HSL→RGB conversion (hue in degrees, saturation/lightness as 0..1 fractions).
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let h = (h.rem_euclid(360.0)) / 360.0;

    let hue_to_rgb = |p: f64, q: f64, mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };

    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);

    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn named_color(s: &str) -> Option<Color> {
    Some(match s {
        "white" => Color::from_rgb(255, 255, 255),
        "black" => Color::from_rgb(0, 0, 0),
        "red" => Color::from_rgb(255, 0, 0),
        "green" => Color::from_rgb(0, 128, 0),
        "blue" => Color::from_rgb(0, 0, 255),
        "yellow" => Color::from_rgb(255, 255, 0),
        "orange" => Color::from_rgb(255, 165, 0),
        "gray" | "grey" => Color::from_rgb(128, 128, 128),
        "silver" => Color::from_rgb(192, 192, 192),
        "transparent" => Color::from_argb(0, 0, 0, 0),
        _ => return None,
    })
}
