//! The favorites page: every saved place, with live departures for the ones that are stops,
//! and the lines saved at a stop in one direction, each with its next few times.
//! Drawn over the map inside `HomePage` (not a separate route), so the map, the selected place
//! and the transit views keep their state: tapping a departure opens its trip in the drawer
//! (the page steps aside until the trip is closed), "Show on map" and "Directions" close it.

use freya::{icons::lucide, prelude::*};
use map_component::MapController;

use crate::{
    components::{
        favorites::{Favorite, Favorites, StopLine},
        place::PlaceController,
        search::{SearchController, fly_to},
        transit::{Departures, Endpoint, NextTimes, TransitController, line_badge},
    },
    mobile::{TOUCH, is_back, text_size, use_screen},
};

const TEXT: Color = Color::from_rgb(32, 33, 36);
const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const TEXT_FAINT: Color = Color::from_rgb(140, 144, 150);
const ACCENT: Color = Color::from_rgb(26, 115, 232);
const DIVIDER: Color = Color::from_rgb(232, 234, 237);
const PAGE: Color = Color::from_rgb(246, 247, 249);
const STAR: Color = Color::from_rgb(242, 153, 0);

/// Maximum width of the centred column holding the cards.
const COLUMN_WIDTH: f32 = 640.0;

#[derive(PartialEq)]
pub struct FavoritesPage {
    pub map: MapController,
    pub places: PlaceController,
    pub search: SearchController,
}

impl Component for FavoritesPage {
    fn render(&self) -> impl IntoElement {
        let favorites = use_consume::<Favorites>();
        let transit = use_consume::<TransitController>();
        let mut page_open = favorites.page_open;
        let insets = use_screen().insets;
        // A trip opened from one of the boards shows in the drawer instead, until closed.
        if !*page_open.read() || transit.view.read().is_some() {
            return rect().into_element();
        }
        let list = favorites.list.read().clone();
        let stop_lines = favorites.stop_lines.read().clone();
        let (stops, others): (Vec<Favorite>, Vec<Favorite>) =
            list.into_iter().partition(|favorite| favorite.stop);

        let mut column: Vec<Element> = Vec::new();
        if stops.is_empty() && others.is_empty() && stop_lines.is_empty() {
            column.push(
                label()
                    .text(
                        "No favorites yet. Open a place on the map and press Save, or open a \
                         departure and save its line from that stop.",
                    )
                    .font_size(text_size(14.0))
                    .color(TEXT_MUTED)
                    .margin((24.0f32, 0.0f32))
                    .into_element(),
            );
        }
        if !stops.is_empty() || !stop_lines.is_empty() {
            column.push(section_title(lucide::clock(), "Live departures"));
            for stop_line in stop_lines {
                column.push(
                    rect()
                        .key(stop_line.key())
                        .width(Size::fill())
                        .child(stop_line_card(stop_line, favorites))
                        .into_element(),
                );
            }
            for favorite in stops {
                column.push(self.card(favorite, &transit, favorites));
            }
        }

        if !others.is_empty() {
            column.push(section_title(lucide::star(), "Places"));
            for favorite in others {
                column.push(self.card(favorite, &transit, favorites));
            }
        }

        rect()
            .position(Position::new_absolute().top(0.).left(0.))
            .width(Size::percent(100.))
            .height(Size::percent(100.))
            .layer(Layer::OverlayLevel(3))
            .background(PAGE)
            .vertical()
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if is_back(&e.key) {
                    page_open.set(false);
                }
            })
            .child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(10.0)
                    .padding((14.0 + insets.top, 16.0, 14.0, 16.0))
                    .background(Color::WHITE)
                    .border(
                        Border::new()
                            .fill(DIVIDER)
                            .width(1.0)
                            .alignment(BorderAlignment::Inner),
                    )
                    .child(
                        Button::new()
                            .flat()
                            .maybe(!TOUCH, |button| button.compact())
                            .on_press(move |_: Event<PressEventData>| page_open.set(false))
                            .child(icon(lucide::arrow_left(), TEXT_MUTED, 18.0)),
                    )
                    .child(icon(lucide::star(), STAR, 18.0))
                    .child(
                        label()
                            .text("Favorites")
                            .font_size(text_size(18.0))
                            .font_weight(FontWeight::BOLD)
                            .color(TEXT),
                    ),
            )
            .child(
                ScrollView::new()
                    .width(Size::fill())
                    .height(Size::fill())
                    .direction(Direction::vertical())
                    .child(
                        rect()
                            .width(Size::fill())
                            .main_align(Alignment::Center)
                            .horizontal()
                            .child(
                                rect()
                                    // Narrower on a phone.
                                    .width(Size::fill())
                                    .max_width(Size::px(COLUMN_WIDTH))
                                    .padding((8.0, 16.0, 24.0 + insets.bottom, 16.0))
                                    .vertical()
                                    .children(column),
                            ),
                    ),
            )
            .into_element()
    }
}

impl FavoritesPage {
    /// One saved place: name, kind, actions, and (stops) its departures board.
    fn card(
        &self,
        favorite: Favorite,
        transit: &TransitController,
        mut favorites: Favorites,
    ) -> Element {
        let show = {
            let map = self.map.clone();
            let mut places = self.places.clone();
            let mut search = self.search.clone();
            let mut transit = transit.clone();
            let favorite = favorite.clone();
            let mut page_open = favorites.page_open;
            move || {
                page_open.set(false);
                transit.close_planner();
                fly_to(&map, favorite.lat, favorite.lon);
                search.reset(favorite.name.clone());
                places.select_favorite(&favorite, map.platform.clone());
            }
        };
        let directions = {
            let mut transit = transit.clone();
            let favorite = favorite.clone();
            let mut page_open = favorites.page_open;
            move || {
                page_open.set(false);
                transit.open_planner(Some(Endpoint {
                    name: favorite.name.clone(),
                    lat: favorite.lat,
                    lon: favorite.lon,
                }));
            }
        };
        let remove = {
            let favorite = favorite.clone();
            move || favorites.remove(&favorite)
        };

        rect()
            .vertical()
            .width(Size::fill())
            .padding(14.0f32)
            .margin((0.0f32, 0.0f32, 12.0f32, 0.0f32))
            .corner_radius(12.0f32)
            .background(Color::WHITE)
            .shadow((
                0.0f32,
                1.0f32,
                3.0f32,
                0.0f32,
                Color::from_argb(30, 0, 0, 0),
            ))
            .child(
                rect()
                    .horizontal()
                    .content(Content::flex())
                    .width(Size::fill())
                    .cross_align(Alignment::Center)
                    .spacing(10.0)
                    .child(icon(
                        if favorite.stop {
                            lucide::train_front()
                        } else {
                            lucide::map_pin()
                        },
                        STAR,
                        18.0,
                    ))
                    .child(
                        rect()
                            .vertical()
                            .width(Size::flex(1.0))
                            .child(
                                label()
                                    .text(favorite.name.clone())
                                    .font_size(text_size(15.0))
                                    .font_weight(FontWeight::BOLD)
                                    .color(TEXT)
                                    .max_lines(1)
                                    .text_overflow(TextOverflow::Ellipsis),
                            )
                            .maybe_child((!favorite.kind.is_empty()).then(|| {
                                label()
                                    .text(favorite.kind.clone())
                                    .font_size(text_size(12.0))
                                    .color(TEXT_FAINT)
                                    .into_element()
                            })),
                    )
                    .child(action(lucide::map(), "Show on map", show))
                    .child(action(lucide::route(), "Directions", directions))
                    .child(
                        Button::new()
                            .flat()
                            .maybe(!TOUCH, |button| button.compact())
                            .on_press({
                                let mut remove = remove;
                                move |_: Event<PressEventData>| remove()
                            })
                            .child(icon(lucide::trash_2(), TEXT_FAINT, 16.0)),
                    ),
            )
            .maybe_child(favorite.stop.then(|| {
                rect()
                    .width(Size::fill())
                    .margin((12.0f32, 0.0f32, 0.0f32, 0.0f32))
                    .child(Departures {
                        place_id: favorite.key(),
                        lat: favorite.lat,
                        lon: favorite.lon,
                        name: favorite.name.clone(),
                        line: None,
                    })
                    .into_element()
            }))
            .into_element()
    }
}

/// A saved line and direction at a stop: badge, direction, stop, and its next times.
fn stop_line_card(stop_line: StopLine, mut favorites: Favorites) -> Element {
    let remove = stop_line.clone();
    rect()
        .vertical()
        .width(Size::fill())
        .spacing(10.0)
        .padding(14.0f32)
        .margin((0.0f32, 0.0f32, 12.0f32, 0.0f32))
        .corner_radius(12.0f32)
        .background(Color::WHITE)
        .shadow((
            0.0f32,
            1.0f32,
            3.0f32,
            0.0f32,
            Color::from_argb(30, 0, 0, 0),
        ))
        .child(
            rect()
                .horizontal()
                .content(Content::flex())
                .width(Size::fill())
                .cross_align(Alignment::Center)
                .spacing(10.0)
                .child(line_badge(
                    stop_line.mode,
                    &stop_line.line,
                    stop_line.color,
                    stop_line.text_color,
                ))
                .child(
                    rect()
                        .vertical()
                        .width(Size::flex(1.0))
                        .child(
                            label()
                                .text(format!("→ {}", stop_line.headsign))
                                .font_size(text_size(14.0))
                                .font_weight(FontWeight::BOLD)
                                .color(TEXT)
                                .max_lines(1)
                                .text_overflow(TextOverflow::Ellipsis),
                        )
                        .child(
                            label()
                                .text(format!("from {}", stop_line.stop))
                                .font_size(text_size(12.0))
                                .color(TEXT_FAINT),
                        ),
                )
                .child(
                    Button::new()
                        .flat()
                        .maybe(!TOUCH, |button| button.compact())
                        .on_press(move |_: Event<PressEventData>| {
                            favorites.toggle_stop_line(remove.clone())
                        })
                        .child(icon(lucide::trash_2(), TEXT_FAINT, 16.0)),
                ),
        )
        .child(NextTimes {
            stop_line: stop_line.clone(),
        })
        .into_element()
}

fn action(svg: Bytes, text: &'static str, mut on_press: impl FnMut() + 'static) -> Element {
    Button::new()
        .flat()
        .maybe(!TOUCH, |button| button.compact())
        .on_press(move |_: Event<PressEventData>| on_press())
        .child(
            rect()
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(5.0)
                .child(icon(svg, ACCENT, 15.0))
                .child(label().text(text).font_size(text_size(12.0)).color(ACCENT)),
        )
        .into_element()
}

fn section_title(svg: Bytes, text: &'static str) -> Element {
    rect()
        .horizontal()
        .cross_align(Alignment::Center)
        .spacing(8.0)
        .margin((12.0f32, 4.0f32, 8.0f32, 4.0f32))
        .child(icon(svg, TEXT_MUTED, 15.0))
        .child(
            label()
                .text(text)
                .font_size(text_size(13.0))
                .font_weight(FontWeight::BOLD)
                .color(TEXT_MUTED),
        )
        .into_element()
}

fn icon(svg: Bytes, color: Color, size: f32) -> SvgViewer {
    SvgViewer::new(svg)
        .width(Size::px(size))
        .height(Size::px(size))
        .color(color)
}
