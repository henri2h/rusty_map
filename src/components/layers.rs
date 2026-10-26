//! The map's optional layers (public transport, EV chargers) behind one "Layers" control: it
//! opens a modal picking what the map shows — Normal (no overlay) or one of the layers, one tile
//! each, the shown one highlighted. A bottom sheet in the compact (phone) layout, a card under
//! the control otherwise.

use bytes::Bytes;
use freya::{icons::lucide, prelude::*};
use torin::position::Position;

use crate::mobile::{Haptic, Screen, haptic, is_back, use_screen};

const TEXT: Color = Color::from_rgb(32, 33, 36);
const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const ACCENT: Color = Color::from_rgb(26, 115, 232);
const DIVIDER: Color = Color::from_rgb(232, 234, 237);

/// One layer the map can show over the normal map.
#[derive(Clone, PartialEq)]
pub struct MapLayer {
    pub name: &'static str,
    pub icon: Bytes,
    /// Tile colour while the layer is on.
    pub color: Color,
    pub enabled: State<bool>,
}

/// The control opening [`LayersModal`]. `top`: its offset below [`Screen::controls_top`].
/// Two siblings rather than one component wrapping both: a wrapping element would sit in the
/// page's flow, under the full-height map, taking the absolutely positioned control off-screen.
#[derive(PartialEq)]
pub struct LayersButton {
    pub layers: Vec<MapLayer>,
    pub open: State<bool>,
    pub top: f32,
}

/// The layer tiles, while [`LayersButton`]'s `open` is set.
#[derive(PartialEq)]
pub struct LayersModal {
    pub layers: Vec<MapLayer>,
    pub open: State<bool>,
    /// Same as [`LayersButton::top`], to place the card under it.
    pub top: f32,
}

impl Component for LayersModal {
    fn render(&self) -> impl IntoElement {
        // Hooks before the early return: their count must not change between renders.
        let screen = use_screen();
        let pull = use_sheet_pull();
        let open = *self.open.read();
        // The sheet stays mounted to play its close animation.
        if !open && !screen.compact {
            return rect().into_element();
        }
        layers_modal(
            &self.layers,
            self.open,
            screen,
            self.top + screen.controls_top(),
            pull,
        )
    }
}

impl Component for LayersButton {
    fn render(&self) -> impl IntoElement {
        let screen = use_screen();
        let top = self.top + screen.controls_top();
        let mut open = self.open;
        let any_on = self.layers.iter().any(|layer| *layer.enabled.read());
        let foreground = if any_on {
            ACCENT
        } else {
            Color::from_rgb(60, 60, 60)
        };

        rect()
            .position(Position::new_absolute().top(top).right(12.))
            .layer(Layer::Overlay)
            .padding((6.0f32, 12.0f32))
            .corner_radius(16.0f32)
            .background(Color::from_argb(235, 255, 255, 255))
            .shadow((
                0.0f32,
                1.0f32,
                4.0f32,
                0.0f32,
                Color::from_argb(60, 0, 0, 0),
            ))
            .cursor(CursorIcon::Pointer)
            .horizontal()
            .cross_align(Alignment::Center)
            .spacing(6.0)
            .on_press(move |_: Event<PressEventData>| {
                haptic(Haptic::Press);
                open.toggle();
            })
            .child(
                SvgViewer::new(lucide::layers())
                    .width(Size::px(16.))
                    .height(Size::px(16.))
                    .color(foreground),
            )
            .child(label().text("Layers").font_size(13.0).color(foreground))
    }
}

fn layers_modal(
    layers: &[MapLayer],
    mut open: State<bool>,
    screen: Screen,
    top: f32,
    pull: SheetPull,
) -> Element {
    // Picking one tile switches every layer to match: on for its own, off for the rest.
    let show_only = |shown: Option<usize>| {
        let layers = layers.to_vec();
        move || {
            if pull.moved() {
                return;
            }
            for (index, layer) in layers.iter().enumerate() {
                let mut enabled = layer.enabled;
                if *enabled.peek() != (Some(index) == shown) {
                    enabled.set(Some(index) == shown);
                }
            }
        }
    };
    let normal = layers.iter().all(|layer| !*layer.enabled.read());
    let tiles = rect()
        .horizontal()
        .spacing(12.0)
        .child(layer_tile(
            "Normal",
            lucide::map(),
            ACCENT,
            normal,
            show_only(None),
        ))
        .children(layers.iter().enumerate().map(|(index, layer)| {
            layer_tile(
                layer.name,
                layer.icon.clone(),
                layer.color,
                *layer.enabled.read(),
                show_only(Some(index)),
            )
        }));

    let header = label()
        .text("Map layers")
        .font_size(16.0)
        .font_weight(FontWeight::BOLD)
        .color(TEXT)
        .margin((0.0f32, 0.0f32, 12.0f32, 0.0f32));

    // Compact: a bottom sheet, pulled down anywhere to close (so do its grab bar, a press
    // outside and back).
    if screen.compact {
        let shown = *open.read();
        return BottomSheet::new()
            .on_close_request(move |_| {
                haptic(Haptic::Press);
                open.set(false);
            })
            .pull(pull)
            .layer(Layer::OverlayLevel(4))
            .bottom_inset(screen.insets.bottom)
            .padding((0.0, 20.0, 20.0, 20.0))
            .background(Color::WHITE)
            .color(TEXT)
            .handle_color(DIVIDER)
            .backdrop(Color::from_argb(70, 0, 0, 0))
            .shadow(Color::from_argb(70, 0, 0, 0))
            .maybe(shown, |sheet| sheet.child(header).child(tiles))
            .into_element();
    }

    // Over the whole window: a press outside the card closes it, and the map under it can't be
    // panned meanwhile.
    rect()
        .position(Position::new_absolute().top(0.).left(0.))
        .width(Size::window_percent(100.))
        .height(Size::window_percent(100.))
        .layer(Layer::OverlayLevel(4))
        .on_press(move |_: Event<PressEventData>| open.set(false))
        .on_global_key_down(move |e: Event<KeyboardEventData>| {
            if is_back(&e.key) {
                open.set(false);
            }
        })
        .child(
            rect()
                .position(Position::new_absolute().top(top + 40.).right(12.))
                .padding(16.0f32)
                .corner_radius(12.0f32)
                .background(Color::WHITE)
                .shadow((
                    0.0f32,
                    2.0f32,
                    12.0f32,
                    0.0f32,
                    Color::from_argb(70, 0, 0, 0),
                ))
                .vertical()
                // A press inside the card must not reach the scrim, which closes the modal.
                .on_press(|e: Event<PressEventData>| e.stop_propagation())
                .child(header)
                .child(tiles),
        )
        .into_element()
}

/// A square tile per choice: its icon, coloured and outlined while `selected`.
fn layer_tile(
    name: &'static str,
    icon: Bytes,
    color: Color,
    selected: bool,
    mut on_select: impl FnMut() + 'static,
) -> Element {
    let (background, foreground) = if selected {
        (color, Color::WHITE)
    } else {
        (Color::from_rgb(244, 246, 248), Color::from_rgb(60, 60, 60))
    };

    rect()
        .vertical()
        .cross_align(Alignment::Center)
        .spacing(6.0)
        .width(Size::px(80.))
        .cursor(CursorIcon::Pointer)
        .on_press(move |e: Event<PressEventData>| {
            e.stop_propagation();
            haptic(Haptic::Toggle);
            on_select();
        })
        .child(
            rect()
                .width(Size::px(64.))
                .height(Size::px(64.))
                .center()
                .corner_radius(14.0f32)
                .background(background)
                .border(
                    Border::new()
                        .fill(if selected { color } else { DIVIDER })
                        .width(2.0)
                        .alignment(BorderAlignment::Inner),
                )
                .child(
                    SvgViewer::new(icon)
                        .width(Size::px(26.))
                        .height(Size::px(26.))
                        .color(foreground),
                ),
        )
        .child(
            label()
                .text(name)
                .font_size(12.0)
                .text_align(TextAlign::Center)
                .color(if selected { TEXT } else { TEXT_MUTED }),
        )
        .into_element()
}
