//! Host-drawn layers: a rotated boat icon, a polygon, a dashed polyline and a label.
//!
//! Usage: cargo run -p map_component --example layers

use freya::prelude::*;
use map_component::{
    IconGlyph, MapComponent, MapController, MapItem, MapLayer,
    util::viewport::{Viewport, lonlat_to_center},
};
use torin::geometry::CursorPoint;

const BOAT: (f64, f64) = (-1.35, 46.15);

fn main() {
    launch(LaunchConfig::new().with_window(WindowConfig::new(app).with_size(900.0, 700.0)));
}

fn app() -> impl IntoElement {
    let mut controller = MapController::new();

    use_hook({
        let mut controller = controller.clone();
        move || {
            let zoom = 11.0;
            let raw = lonlat_to_center(BOAT.0, BOAT.1);
            let offset =
                Viewport::new(CursorPoint::zero(), zoom, 14).pixels_to_center_delta(450.0, 350.0);
            controller
                .center
                .set(CursorPoint::new(raw.x - offset.x, raw.y - offset.y));
            controller.zoom.set(zoom);
            controller.set_layer(MapLayer::new(
                1,
                vec![
                    MapItem::polygon_lonlat(
                        &[
                            (-1.45, 46.10),
                            (-1.25, 46.08),
                            (-1.22, 46.20),
                            (-1.40, 46.24),
                        ],
                        Some(Color::from_argb(60, 30, 120, 220)),
                        Some((Color::from_rgb(30, 120, 220), 2.0)),
                    ),
                    MapItem::polyline_lonlat(
                        &[(-1.50, 46.05), (-1.42, 46.12), BOAT, (-1.28, 46.22)],
                        Color::from_rgb(220, 60, 60),
                        3.0,
                        true,
                    ),
                    MapItem::icon_lonlat(
                        7,
                        BOAT.0,
                        BOAT.1,
                        IconGlyph::Boat,
                        45.0,
                        Color::from_rgb(230, 120, 20),
                        32.0,
                    ),
                    MapItem::label_lonlat(
                        BOAT.0,
                        BOAT.1 - 0.02,
                        "My boat",
                        Color::from_rgb(40, 40, 40),
                        14.0,
                    ),
                ],
            ));
        }
    });

    rect()
        .expanded()
        .child(
            MapComponent::new(controller.clone()).on_item_tapped(move |(layer, item)| {
                println!("tapped item {item} of layer {layer}");
                let current = controller
                    .layers
                    .peek()
                    .iter()
                    .find(|l| l.id == layer)
                    .cloned();
                if let Some(layer) = current {
                    let mut layer = (*layer).clone();
                    for entry in &mut layer.items {
                        if let MapItem::Icon { rotation_deg, .. } = entry {
                            *rotation_deg = (*rotation_deg + 45.0) % 360.0;
                        }
                    }
                    controller.set_layer(layer);
                }
            }),
        )
}
