//! Offline label-pass check: compiles a disk-cached z14 tile against the cached osm-bright style
//! and runs candidate collection + placement as the paint loop would at a given zoom, printing
//! what got placed per style layer.
//!
//! Usage: cargo run -p map_component --example label_debug -- [zoom] [x] [y]

use std::sync::Arc;

use freya::engine::prelude::raster_n32_premul;
use map_component::{
    style::{StyleLayer, StyleSpec, parser::parse_style},
    util::{
        labels::{self, IconCandidate, LabelAnimator},
        renderer::TILE_SIZE,
        sprites::load_sprites,
        tiles::fetch_tile,
        viewport::Viewport,
    },
};
use torin::geometry::CursorPoint;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let zoom: f64 = args.get(1).and_then(|z| z.parse().ok()).unwrap_or(18.0);
    let x: u32 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(8297);
    let y: u32 = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(5636);

    let style_path = dirs::cache_dir().unwrap().join(
        "rusty_map/styles/https___openmaptiles.geo.data.gouv.fr_styles_osm-bright_style.json",
    );
    let parsed = parse_style(&std::fs::read_to_string(style_path).unwrap()).unwrap();
    let style = Arc::new(StyleSpec {
        layers: parsed.layers,
        tile_url_template: "https://openmaptiles.data.gouv.fr/data/planet-vector/{z}/{x}/{y}.pbf"
            .to_string(),
        sprite_url: None,
        min_zoom: 0.0,
        max_real_zoom: 14,
    });

    let tile = fetch_tile((14, x, y), style.clone())
        .await
        .expect("tile in disk cache");
    let line_labels = tile
        .labels
        .iter()
        .filter(|l| {
            matches!(
                l.anchor,
                map_component::util::renderer::LabelAnchor::Line { .. }
            )
        })
        .count();
    println!("tile labels: {} ({} line)", tile.labels.len(), line_labels);

    let (w, h) = (1920.0f32, 1080.0f32);
    let tile_px = TILE_SIZE * 2f32.powf(zoom as f32 - 14.0);
    // Screen centered on the tile's center.
    let (sx, sy) = (w / 2.0 - tile_px / 2.0, h / 2.0 - tile_px / 2.0);

    let mut candidates = Vec::new();
    labels::collect_candidates(
        &tile.labels,
        &style,
        zoom,
        sx,
        sy,
        tile_px,
        w,
        h,
        None,
        &mut candidates,
    );
    println!(
        "zoom {zoom}: tile_px {tile_px}, candidates {}",
        candidates.len()
    );
    let mut per_layer = std::collections::BTreeMap::<String, usize>::new();
    for c in &candidates {
        *per_layer
            .entry(layer_name(&style, c.label.style_index))
            .or_default() += 1;
    }
    println!("candidates per layer: {per_layer:?}");

    let sheet = load_sprites("https://openmaptiles.data.gouv.fr/sprites/sprite")
        .await
        .ok();
    let icons: Vec<IconCandidate> = tile
        .pois
        .iter()
        .map(|poi| IconCandidate {
            poi,
            screen_x: sx + poi.x * tile_px,
            screen_y: sy + poi.y * tile_px,
        })
        .filter(|c| {
            c.screen_x >= -32.0
                && c.screen_y >= -32.0
                && c.screen_x <= w + 32.0
                && c.screen_y <= h + 32.0
        })
        .collect();
    println!(
        "icon candidates {} (sprite sheet loaded: {})",
        icons.len(),
        sheet.is_some()
    );

    {
        // Diagnostics: do POI name candidates share a position with an icon candidate?
        let icon_pos: std::collections::HashMap<(u32, u32), &IconCandidate> = icons
            .iter()
            .map(|c| ((c.screen_x.to_bits(), c.screen_y.to_bits()), c))
            .collect();
        let (mut matched, mut with_sprite, mut unmatched) = (0, 0, 0);
        let mut kinds_without_sprite = std::collections::BTreeMap::<String, usize>::new();
        for c in candidates
            .iter()
            .filter(|c| layer_name(&style, c.label.style_index).ends_with(" poi"))
        {
            match icon_pos.get(&(c.screen_x.to_bits(), c.screen_y.to_bits())) {
                Some(icon) => {
                    matched += 1;
                    if sheet.as_ref().is_some_and(|s| {
                        s.resolve(&icon.poi.kind).is_some()
                            || icon
                                .poi
                                .class
                                .as_deref()
                                .is_some_and(|c| s.resolve(c).is_some())
                    }) {
                        with_sprite += 1;
                    } else {
                        *kinds_without_sprite
                            .entry(icon.poi.kind.clone())
                            .or_default() += 1;
                    }
                }
                None => unmatched += 1,
            }
        }
        println!(
            "poi names: {matched} at an icon position ({with_sprite} with sprite), {unmatched} not"
        );
        println!("kinds without sprite: {kinds_without_sprite:?}");
    }

    let mut surface = raster_n32_premul((w as i32, h as i32)).unwrap();
    let mut animator = LabelAnimator::default();
    let viewport = Viewport::new(CursorPoint::new(0.0, 0.0), zoom as f32, 14);
    // Three frames, as the paint loop would run them: the first one measures every label's text
    // (cold), later ones reuse that and last frame's placements.
    let mut previously_placed = rustc_hash::FxHashSet::default();
    let mut placed = Vec::new();
    for frame in 0..3 {
        let start = std::time::Instant::now();
        placed = labels::draw_labels(
            surface.canvas(),
            &style,
            candidates.clone(),
            zoom,
            false,
            &previously_placed,
            &mut animator,
            &viewport,
            icons.clone(),
            sheet.as_ref(),
        );
        println!(
            "frame {frame}: placed {} in {:.2} ms",
            placed.len(),
            start.elapsed().as_secs_f64() * 1000.0
        );
        previously_placed = placed
            .iter()
            .map(|p| labels::label_identity(&p.text, p.style_index))
            .collect();
    }
    let mut placed_per_layer = std::collections::BTreeMap::<String, usize>::new();
    for p in &placed {
        let name = if p.sprite.is_some() {
            "icon".to_string()
        } else {
            layer_name(&style, p.style_index)
        };
        *placed_per_layer.entry(name).or_default() += 1;
    }
    // POI names drawn with their icon directly above (icon rect bottom just above text top).
    let icons_placed: Vec<_> = placed.iter().filter(|p| p.sprite.is_some()).collect();
    let paired = placed
        .iter()
        .filter(|p| p.sprite.is_none())
        .filter(|t| {
            icons_placed.iter().any(|i| {
                (i.rect.center_x() - t.rect.center_x()).abs() < 0.5
                    && t.rect.top > i.rect.bottom
                    && t.rect.top - i.rect.bottom < 12.0
            })
        })
        .count();
    let overlapping = placed
        .iter()
        .filter(|p| p.sprite.is_none())
        .filter(|t| icons_placed.iter().any(|i| i.rect.intersects(t.rect)))
        .count();
    println!("names under their icon: {paired}, names overlapping an icon: {overlapping}");
    println!("placed per layer: {placed_per_layer:?}");
}

fn layer_name(style: &StyleSpec, index: usize) -> String {
    match style.layers.get(index) {
        Some(StyleLayer::Symbol(s)) => format!("#{index} {}", s.source_layer),
        _ => format!("#{index}"),
    }
}
