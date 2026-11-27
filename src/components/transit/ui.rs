//! Building blocks shared by the transit drawer views: line badges, icons, status lines and
//! French clock times.

use crate::mobile::{TOUCH, is_compact, text_size};
use bytes::Bytes;
use freya::{icons::lucide, prelude::*};

use map_component::{Insets, MapController, RouteOverlay};

use super::api::{self, Mode};

pub const TEXT: Color = Color::from_rgb(32, 33, 36);
pub const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
pub const TEXT_FAINT: Color = Color::from_rgb(140, 144, 150);
pub const LATE: Color = Color::from_rgb(197, 34, 31);
pub const LIVE: Color = Color::from_rgb(24, 128, 56);
pub const DIVIDER: Color = Color::from_rgb(232, 234, 237);
pub const BADGE_FALLBACK: Color = Color::from_rgb(95, 99, 104);
pub const LINK: Color = Color::from_rgb(26, 115, 232);

pub fn icon(svg: Bytes, color: Color, size: f32) -> SvgViewer {
    SvgViewer::new(svg)
        .width(Size::px(size))
        .height(Size::px(size))
        .color(color)
}

pub fn status_line(leading: Element, text: &str) -> Element {
    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(10.0)
        .padding((6.0f32, 0.0f32))
        .child(leading)
        .child(
            label()
                .text(text.to_string())
                .font_size(text_size(12.0))
                .color(TEXT_MUTED),
        )
        .into_element()
}

pub fn rgb(color: u32) -> Color {
    Color::from_rgb((color >> 16) as u8, (color >> 8) as u8, color as u8)
}

pub fn mode_icon(mode: Mode) -> Bytes {
    match mode {
        Mode::Bus => lucide::bus_front(),
        Mode::Tram => lucide::tram_front(),
        Mode::Subway => lucide::train_front_tunnel(),
        Mode::Rail | Mode::Other => lucide::train_front(),
        Mode::Ferry => lucide::ship(),
        Mode::Walk => lucide::footprints(),
    }
}

/// A line's colored badge: mode icon plus its short name (`"42"`, `"RER A"`).
pub fn line_badge(mode: Mode, line: &str, color: Option<u32>, text_color: Option<u32>) -> Element {
    let background = color.map_or(BADGE_FALLBACK, rgb);
    let foreground = text_color.map_or_else(|| contrasting_text(color), rgb);
    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(4.0)
        .width(Size::px(if TOUCH { 64.0 } else { 56.0 }))
        .main_align(Alignment::Center)
        .padding(if TOUCH { (4.0, 6.0) } else { (2.0, 6.0) })
        .corner_radius(6.0f32)
        .background(background)
        .child(icon(mode_icon(mode), foreground, text_size(12.0)))
        .child(
            label()
                .text(line.to_string())
                .font_size(text_size(12.0))
                .font_weight(FontWeight::BOLD)
                .color(foreground)
                .max_lines(1),
        )
        .into_element()
}

/// Black or white, whichever reads better on `background` (OSM routes give a `colour` but no
/// text colour; yellow metro 1 needs black text).
fn contrasting_text(background: Option<u32>) -> Color {
    let Some(color) = background else {
        return Color::WHITE;
    };
    let (r, g, b) = ((color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff);
    let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    if luma > 165.0 {
        Color::BLACK
    } else {
        Color::WHITE
    }
}

/// Whole minutes as `"5 min"` / `"1 h 20"`.
pub fn duration_text(seconds: i64) -> String {
    let minutes = (seconds + 30) / 60;
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{} h {:02}", minutes / 60, minutes % 60)
    }
}

/// `HH:MM` in French local time (CET/CEST). Transitous gives UTC; France is the target region,
/// and pulling in a full time-zone database for one clock label isn't worth it.
pub fn clock_time(unix: i64) -> String {
    let local = unix + central_european_offset(unix);
    let minutes_of_day = local.rem_euclid(86_400) / 60;
    format!("{:02}:{:02}", minutes_of_day / 60, minutes_of_day % 60)
}

/// CET (+1 h), or CEST (+2 h) from the last Sunday of March to the last Sunday of October,
/// switching at 01:00 UTC (EU rule).
pub fn central_european_offset(unix: i64) -> i64 {
    let days = unix.div_euclid(86_400);
    let year = year_of(days);
    let last_sunday = |month| {
        let day31 = api::days_from_civil(year, month, 31);
        // 1970-01-01 was a Thursday: (days + 4) % 7 == 0 on Sundays.
        day31 - (day31 + 4).rem_euclid(7)
    };
    let summer_start = last_sunday(3) * 86_400 + 3_600;
    let summer_end = last_sunday(10) * 86_400 + 3_600;
    if (summer_start..summer_end).contains(&unix) {
        7_200
    } else {
        3_600
    }
}

pub fn year_of(days: i64) -> i64 {
    let mut year = 1970 + days.div_euclid(365);
    while api::days_from_civil(year, 1, 1) > days {
        year -= 1;
    }
    while api::days_from_civil(year + 1, 1, 1) <= days {
        year += 1;
    }
    year
}

/// Draws `overlay` on the map and frames it in the part the drawer or bottom sheet leaves
/// visible (see [`route_insets`]). A view already showing it (another itinerary of the same
/// journey, say) stays put; otherwise the map glides there (see
/// `MapController::frame_world_bounds`).
pub fn show_on_map(map: &mut MapController, overlay: RouteOverlay) {
    if let Some(bounds) = overlay.world_bounds() {
        map.frame_world_bounds(bounds, route_insets(map), ROUTE_MAX_ZOOM);
    }
    map.set_route_overlay(Some(overlay));
}

/// Re-frames the route already drawn, after what covers the map changed (the bottom sheet came
/// to rest at another height): always, gliding — the point is to use the map just uncovered.
/// Nothing without a route.
pub fn refit_route(map: &mut MapController) {
    let bounds = map
        .route_overlay
        .peek()
        .as_ref()
        .and_then(|overlay| overlay.world_bounds());
    if let Some(bounds) = bounds {
        map.glide_to_world_bounds(bounds, route_insets(map), ROUTE_MAX_ZOOM);
    }
}

/// Closest a framed route is shown.
const ROUTE_MAX_ZOOM: f32 = 16.5;
/// Clear space around a framed route.
const ROUTE_MARGIN: f32 = 24.0;

/// Where a route is framed: what the host's UI covers of the map (`MapController::covered`)
/// plus a margin, and the map controls — under the search box on a phone (the top), and down
/// the right edge on desktop (Layers, Favorites, Settings: 12 px from it, ~100 px wide), where a
/// route end in the corner hid under them.
fn route_insets(map: &MapController) -> Insets {
    let covered = map.covered();
    let controls = if is_compact(map.viewport_size.peek().size.width) {
        Insets {
            top: 72.0,
            ..Insets::default()
        }
    } else {
        Insets {
            right: 128.0,
            ..Insets::default()
        }
    };
    Insets {
        top: covered.top + controls.top.max(ROUTE_MARGIN),
        right: covered.right + controls.right.max(ROUTE_MARGIN),
        bottom: covered.bottom + controls.bottom.max(ROUTE_MARGIN),
        left: covered.left + controls.left.max(ROUTE_MARGIN),
    }
}

/// A component's diff key from anything hashable, for `Component::render_key`.
pub fn diff_key(parts: impl std::hash::Hash) -> DiffKey {
    use std::hash::Hasher;
    let mut hasher = std::hash::DefaultHasher::new();
    parts.hash(&mut hasher);
    DiffKey::U64(hasher.finish())
}

pub fn rgb_or(color: Option<u32>, fallback: Color) -> Color {
    color.map_or(fallback, rgb)
}

/// Section title row: icon plus bold text.
pub fn section_title(svg: Bytes, text: &str) -> Element {
    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(8.0)
        .margin((8.0f32, 0.0f32, 6.0f32, 0.0f32))
        .child(icon(svg, TEXT_MUTED, 16.0))
        .child(
            label()
                .text(text.to_string())
                .font_size(text_size(13.0))
                .font_weight(FontWeight::BOLD)
                .color(TEXT),
        )
        .into_element()
}

pub fn divider() -> Element {
    rect()
        .width(Size::fill())
        .height(Size::px(1.0))
        .background(DIVIDER)
        .into_element()
}

pub const ACCENT: Color = Color::from_rgb(26, 115, 232);
