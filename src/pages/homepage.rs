use freya::prelude::*;
use map_component::{MapComponent, MapController};

use crate::components::{
    place::PlaceController,
    search::{SearchController, SearchPanel},
};

#[derive(PartialEq)]
pub struct HomePage {}

impl Component for HomePage {
    fn render(&self) -> impl IntoElement {
        let controller = MapController::new();
        let places = PlaceController::new();
        let search = SearchController::new();

        // Mirror the selected place's pin onto the map.
        use_side_effect({
            let mut marker = controller.marker_world;
            let platform = controller.platform.clone();
            let selected = places.selected_place;
            move || {
                let world = selected.read().as_ref().and_then(|p| p.marker_world);
                if *marker.peek() != world {
                    marker.set(world);
                    platform.send(UserEvent::RequestRedraw);
                }
            }
        });

        rect()
            .child(
                MapComponent::new(controller.clone())
                    .width(Size::percent(100.))
                    .height(Size::percent(100.))
                    .on_place_tapped({
                        let mut places = places.clone();
                        let mut search = search.clone();
                        let platform = controller.platform.clone();
                        move |tap: map_component::TappedFeature| {
                            search.reset(tap.title.clone());
                            places.select_tap(tap, platform.clone());
                        }
                    })
                    .on_map_tapped({
                        let mut places = places.clone();
                        let mut search = search.clone();
                        move |()| {
                            search.reset(String::new());
                            places.clear();
                        }
                    }),
            )
            .child(ZoomBadge {
                zoom: controller.zoom,
            })
            .child(SearchPanel::new(controller, places, search))
    }
}

/// Current zoom level, top-right. Its own component so that reading `zoom` subscribes only this
/// label: read from `HomePage::render`, every wheel tick would re-render `HomePage` and diff its
/// whole tree, `MapComponent` included.
#[derive(PartialEq)]
struct ZoomBadge {
    zoom: State<f32>,
}

impl Component for ZoomBadge {
    fn render(&self) -> impl IntoElement {
        let zoom = *self.zoom.read();
        rect()
            .padding((4.0f32, 10.0f32))
            .background(Color::from_argb(220, 255, 255, 255))
            .corner_radius(12.0f32)
            .shadow((
                0.0f32,
                1.0f32,
                4.0f32,
                0.0f32,
                Color::from_argb(50, 0, 0, 0),
            ))
            .position(Position::new_absolute().top(12.).right(12.))
            .layer(Layer::Overlay)
            .child(
                label()
                    .text(format!("Zoom {zoom:.1}"))
                    .color(Color::from_rgb(60, 60, 60))
                    .font_size(12.0),
            )
    }
}
