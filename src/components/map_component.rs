use freya::{
    engine::prelude::{Paint, PaintStyle},
    prelude::*,
};
use torin::{geometry::CursorPoint, node::Node, size::Size};

#[derive(PartialEq)]
pub struct MapComponent {
    layout: LayoutData,
}

impl MapComponent {
    pub fn new() -> Self {
        Self {
            layout: Node {
                width: Size::fill(),
                height: Size::fill(),
                ..Default::default()
            }
            .into(),
        }
    }
}

impl LayoutExt for MapComponent {
    fn get_layout(&mut self) -> &mut LayoutData {
        &mut self.layout
    }
}

impl ContainerSizeExt for MapComponent {}

impl Component for MapComponent {
    fn render(&self) -> impl IntoElement {
        // render value

        let mut offset = use_state(|| CursorPoint::zero());

        let mut initial_offset = use_state::<Option<CursorPoint>>(|| None);
        let mut initial_touch = use_state(|| CursorPoint::zero());

        let mut scale = use_state(|| 1.0);

        let on_mouse_move = move |event: Event<MouseEventData>| {
            let pos = event.global_location;
            if let Some(initial_pos) = *initial_offset.read() {
                println!(
                    "Move {:?} {:?} {:?}",
                    initial_pos,
                    pos,
                    initial_touch.read()
                );
                offset.set(initial_pos + (pos - *initial_touch.read()));
            }
        };

        let on_pointer_down = move |event: Event<PointerEventData>| {
            println!("Touch down");
            initial_touch.set(event.global_location());
            initial_offset.set(Some(*offset.peek()));
        };

        let on_global_pointer_press = move |_event: Event<PointerEventData>| {
            println!("Press");
            initial_offset.set(None);
        };

        let on_touch_move = move |event: Event<TouchEventData>| {
            let finger_id = event.finger_id;
            println!("Finger move: {}", finger_id);
        };

        let offset_x = offset.read().x;
        let offset_y = offset.read().y;
        let scale_val = *scale.read();

        canvas(RenderCallback::new(move |context| {
            let off = *offset.peek();
            let scale = *scale.peek();

            println!("Scale: {}", scale);

            let center_x = context.size.width / 2.0;
            let center_y = context.size.height / 2.0;

            context.canvas.translate((off.x as f32, off.y as f32));
            context.canvas.scale((scale, scale));

            let mut paint = Paint::default();
            paint.set_anti_alias(true);
            paint.set_style(PaintStyle::Fill);
            paint.set_color(Color::BLUE);

            context
                .canvas
                .draw_circle((center_x, center_y), 50.0, &paint);
        }))
        .width(self.layout.layout.width.clone())
        .height(self.layout.layout.height.clone())
        // handlers
        .on_mouse_move(on_mouse_move)
        .on_pointer_down(on_pointer_down)
        .on_global_pointer_press(on_global_pointer_press)
        .on_touch_move(on_touch_move)
        .on_wheel(move |e: Event<WheelEventData>| {
            let old_scale = *scale.peek();
            let new_scale = (old_scale + e.delta_y as f32 * 0.01).clamp(0.1, 10.0);

            let off = *offset.peek();
            let mouse = e.element_location;

            // Keep the world point under the mouse fixed while zooming.
            let world_x = (mouse.x - off.x) / old_scale as f64;
            let world_y = (mouse.y - off.y) / old_scale as f64;

            offset.write().x = mouse.x - world_x * new_scale as f64;
            offset.write().y = mouse.y - world_y * new_scale as f64;

            scale.set(new_scale);
        })
        .on_touch_start(|e: Event<TouchEventData>| {
            let id = e.finger_id;
            println!("Touch start: id: {id}");
        })
        .key(scale_val.to_bits())
        // plug the update info
        .offset_x(offset_x as f32)
        .offset_y(offset_y as f32)
    }
}
