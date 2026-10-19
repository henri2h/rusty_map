//! The elevation profile of a walk: an area chart of height against distance, with the total
//! climb and descent, and a crosshair readout under the pointer (or finger, dragged along it).

use std::rc::Rc;

use freya::{
    engine::prelude::{Paint, PaintCap, PaintJoin, PaintStyle, PathBuilder},
    icons::lucide,
    prelude::*,
};
use torin::content::Content;

use crate::mobile::text_size;

use super::{
    elevation::{Profile, fetch_profile},
    maneuvers::distance_text,
};

const TEXT: Color = Color::from_rgb(32, 33, 36);
const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const TEXT_FAINT: Color = Color::from_rgb(140, 144, 150);
/// The series hue (the dataviz palette's slot 1, 4.2:1 on the sheet); its area a ~10% wash.
const SERIES: Color = Color::from_rgb(42, 120, 214);
const GRID: Color = Color::from_rgb(232, 234, 237);
const SURFACE: Color = Color::from_argb(250, 255, 255, 255);

const CHART_HEIGHT: f32 = 96.0;
/// A flat walk still gets this much vertical range, so a 2 m bump doesn't look like a hill.
const MIN_RANGE_M: f64 = 20.0;

#[derive(Clone, PartialEq)]
enum Load {
    Loading,
    Ready(Rc<Profile>),
    Failed,
}

/// The profile of the walk along `points` (`(lon, lat)`); `length_m` keys it.
#[derive(PartialEq)]
pub struct ElevationProfile {
    pub points: Rc<Vec<(f64, f64)>>,
    pub length_m: f64,
}

impl Component for ElevationProfile {
    fn render(&self) -> impl IntoElement {
        let state = use_state(|| Load::Loading);
        // Fraction of the chart's width under the pointer.
        let mut hover = use_state(|| None::<f32>);
        let mut width = use_state(|| 1.0f32);

        use_hook({
            let points = self.points.clone();
            let mut state = state;
            move || {
                spawn(async move {
                    let result = fetch_profile(points.to_vec()).await;
                    state.set(match result {
                        Ok(profile) => Load::Ready(Rc::new(profile)),
                        Err(err) => {
                            log::warn!("[elevation] profile failed: {err:?}");
                            Load::Failed
                        }
                    });
                });
            }
        });

        let profile = match &*state.read() {
            Load::Ready(profile) => profile.clone(),
            // A missing profile isn't worth a line in the route: nothing.
            Load::Failed => return rect().into_element(),
            Load::Loading => {
                return rect()
                    .height(Size::px(CHART_HEIGHT + 40.0))
                    .width(Size::fill())
                    .center()
                    .child(CircularLoader::new().size(16.0))
                    .into_element();
            }
        };

        let length = profile.samples.last().map_or(0.0, |(at, _)| *at);
        let (low, high) = value_range(&profile);
        let hovered = (*hover.read()).map(|fraction| {
            let along = length * fraction as f64;
            (fraction, along, profile.height_at(along))
        });

        let header = rect()
            .horizontal()
            .content(Content::flex())
            .width(Size::fill())
            .cross_align(Alignment::Center)
            .spacing(8.0)
            .margin((0.0f32, 0.0f32, 6.0f32, 0.0f32))
            .child(
                label()
                    .text("Elevation")
                    .width(Size::flex(1.0))
                    .font_size(text_size(12.0))
                    .font_weight(FontWeight::BOLD)
                    .color(TEXT_MUTED),
            )
            .child(match hovered {
                Some((_, along, height)) => label()
                    .text(format!("{} · {:.0} m", distance_text(along), height))
                    .font_size(text_size(12.0))
                    .font_weight(FontWeight::BOLD)
                    .color(TEXT)
                    .into_element(),
                None => rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(4.0)
                    .child(small_icon(lucide::trending_up()))
                    .child(stat(profile.climb_m))
                    .child(rect().width(Size::px(6.0)))
                    .child(small_icon(lucide::trending_down()))
                    .child(stat(profile.descent_m))
                    .into_element(),
            });

        let drawn = profile.clone();
        let canvas = rect()
            .width(Size::flex(1.0))
            .height(Size::px(CHART_HEIGHT))
            .on_sized(move |e: Event<SizedEventData>| width.set(e.area.width().max(1.0)))
            // Dragging the chart reads it: it mustn't also pull the sheet closed.
            .on_pointer_down(move |e: Event<PointerEventData>| {
                e.stop_propagation();
                let x = e.element_location().x as f32;
                hover.set(Some((x / *width.peek()).clamp(0.0, 1.0)));
            })
            .on_pointer_move(move |e: Event<PointerEventData>| {
                let x = e.element_location().x as f32;
                hover.set(Some((x / *width.peek()).clamp(0.0, 1.0)));
            })
            .on_pointer_leave(move |_: Event<PointerEventData>| hover.set(None))
            .background(RenderCallback::new(move |context| {
                draw(
                    context,
                    &drawn,
                    length,
                    low,
                    high,
                    hovered.map(|(f, _, _)| f),
                );
            }));

        rect()
            .vertical()
            .width(Size::fill())
            .margin((4.0f32, 0.0f32, 12.0f32, 0.0f32))
            .child(header)
            .child(
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .spacing(6.0)
                    .child(
                        // Axis labels: the chart's top and bottom heights.
                        rect()
                            .vertical()
                            .height(Size::px(CHART_HEIGHT))
                            .main_align(Alignment::SpaceBetween)
                            .cross_align(Alignment::End)
                            .child(axis_label(format!("{high:.0} m")))
                            .child(axis_label(format!("{low:.0} m"))),
                    )
                    .child(canvas),
            )
            .child(
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .margin((2.0f32, 0.0f32, 0.0f32, 0.0f32))
                    .child(rect().width(Size::px(AXIS_GUTTER)))
                    .child(axis_label("0".to_string()).width(Size::flex(1.0)))
                    .child(axis_label(distance_text(length))),
            )
            .into_element()
    }

    fn render_key(&self) -> DiffKey {
        DiffKey::U64(self.length_m.to_bits() ^ self.points.len() as u64)
    }
}

/// Room the height labels take left of the chart (kept clear under it too).
const AXIS_GUTTER: f32 = 44.0;

/// The chart's height range: the profile's, padded, and at least [`MIN_RANGE_M`] tall.
fn value_range(profile: &Profile) -> (f64, f64) {
    let span = (profile.max_m - profile.min_m).max(MIN_RANGE_M);
    let middle = (profile.max_m + profile.min_m) / 2.0;
    let pad = span * 0.1;
    let low = (middle - span / 2.0 - pad).floor();
    let high = (middle + span / 2.0 + pad).ceil();
    (low, high)
}

fn draw(
    context: &mut FillRenderContext,
    profile: &Profile,
    length: f64,
    low: f64,
    high: f64,
    hover: Option<f32>,
) {
    let samples = &profile.samples;
    // Freya has already moved the canvas to this element's top-left (and scaled it): draw in
    // local coordinates. `context.origin` is where that is in the window, not an offset to add.
    let size = context.size;
    let (width, height) = (size.width, size.height);
    if samples.len() < 2 || length <= 0.0 || width <= 0.0 {
        return;
    }
    let x = |along: f64| (along / length) as f32 * width;
    let y = |z: f64| ((high - z) / (high - low)) as f32 * height;
    let canvas = context.canvas;

    // Recessive grid: hairlines at the top, middle and bottom.
    let mut grid = Paint::default();
    grid.set_anti_alias(true);
    grid.set_style(PaintStyle::Stroke);
    grid.set_stroke_width(1.0);
    grid.set_color(GRID);
    for fraction in [0.0f32, 0.5, 1.0] {
        let line_y = (height - 1.0) * fraction + 0.5;
        canvas.draw_line((0.0, line_y), (width, line_y), &grid);
    }

    let mut line = PathBuilder::new();
    let mut area = PathBuilder::new();
    area.move_to((x(0.0), height));
    for (index, (along, z)) in samples.iter().enumerate() {
        let point = (x(*along), y(*z));
        if index == 0 {
            line.move_to(point);
        } else {
            line.line_to(point);
        }
        area.line_to(point);
    }
    area.line_to((x(length), height));
    area.close();

    let mut fill = Paint::default();
    fill.set_anti_alias(true);
    fill.set_color(Color::from_argb(26, 42, 120, 214));
    canvas.draw_path(&area.detach(), &fill);

    let mut stroke = Paint::default();
    stroke.set_anti_alias(true);
    stroke.set_style(PaintStyle::Stroke);
    stroke.set_stroke_width(2.0);
    stroke.set_stroke_join(PaintJoin::Round);
    stroke.set_stroke_cap(PaintCap::Round);
    stroke.set_color(SERIES);
    canvas.draw_path(&line.detach(), &stroke);

    if let Some(fraction) = hover {
        let along = length * fraction as f64;
        let z = profile.height_at(along);
        let cross_x = x(along);
        let mut cross = Paint::default();
        cross.set_anti_alias(true);
        cross.set_style(PaintStyle::Stroke);
        cross.set_stroke_width(1.0);
        cross.set_color(TEXT_FAINT);
        canvas.draw_line((cross_x, 0.0), (cross_x, height), &cross);
        // The point, ringed in the surface colour so it stands off the line.
        let mut dot = Paint::default();
        dot.set_anti_alias(true);
        dot.set_color(SURFACE);
        canvas.draw_circle((cross_x, y(z)), 6.0, &dot);
        dot.set_color(SERIES);
        canvas.draw_circle((cross_x, y(z)), 4.0, &dot);
    }
}

fn small_icon(svg: bytes::Bytes) -> SvgViewer {
    SvgViewer::new(svg)
        .width(Size::px(14.))
        .height(Size::px(14.))
        .color(TEXT_MUTED)
}

fn stat(metres: f64) -> Element {
    label()
        .text(format!("{metres:.0} m"))
        .font_size(text_size(12.0))
        .color(TEXT)
        .into_element()
}

fn axis_label(text: String) -> Label {
    label()
        .text(text)
        .font_size(text_size(10.0))
        .color(TEXT_FAINT)
}
