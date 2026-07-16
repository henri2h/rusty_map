use freya::prelude::*;

use crate::components::map_component::MapComponent;

#[derive(PartialEq)]
pub struct HomePage {}

impl Component for HomePage {
    fn render(&self) -> impl IntoElement {
        rect().child(
            MapComponent::new()
                .width(Size::percent(100.))
                .height(Size::percent(100.)),
        )
    }
}
