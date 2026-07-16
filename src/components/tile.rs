use freya::{
    engine::prelude::{Paint, PaintStyle},
    prelude::*,
};

#[derive(PartialEq)]
struct Tile {}

impl Component for Tile {
    fn render(&self) -> impl IntoElement {
        canvas(RenderCallback::new(|context| {
            let center_x = context.size.width / 2.0;
            let center_y = context.size.height / 2.0;

            let mut paint = Paint::default();
            paint.set_anti_alias(true);
            paint.set_style(PaintStyle::Fill);
            paint.set_color(Color::BLUE);

            context
                .canvas
                .draw_circle((center_x, center_y), 50.0, &paint);
        }))
        .width(Size::px(100.))
        .height(Size::px(100.))
    }
}
