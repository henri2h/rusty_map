//! The settings page: about the app, and the credits for every data source it shows — the
//! attribution their licences ask for, in one place, beyond the map's own corner credit.
//! Drawn over the map inside `HomePage`, like the favorites page.

use bytes::Bytes;
use freya::{icons::lucide, prelude::*};

use crate::{
    components::{charging, transit},
    mobile::{Haptic, TOUCH, haptic, is_back, tap_padding, text_size, use_screen},
};

const TEXT: Color = Color::from_rgb(32, 33, 36);
const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const ACCENT: Color = Color::from_rgb(26, 115, 232);
const DIVIDER: Color = Color::from_rgb(232, 234, 237);
const PAGE: Color = Color::from_rgb(246, 247, 249);

/// Maximum width of the centred column holding the cards.
const COLUMN_WIDTH: f32 = 640.0;

/// A data source the app shows, and how it must be credited.
struct Source {
    name: &'static str,
    /// What the app takes from it.
    provides: &'static str,
    /// The licence, when one licence covers it (Transitous' feeds each have their own).
    licence: Option<&'static str>,
    url: &'static str,
}

const SOURCES: &[Source] = &[
    Source {
        name: "© OpenStreetMap contributors",
        provides: "Map data, places, addresses, transit lines",
        licence: Some("ODbL 1.0"),
        url: "https://www.openstreetmap.org/copyright",
    },
    Source {
        name: "© OpenMapTiles",
        provides: "Vector tile schema and the OSM Bright map style",
        licence: None,
        url: "https://openmaptiles.org/",
    },
    Source {
        name: "Photon by komoot",
        provides: "Place and address search",
        licence: None,
        url: "https://photon.komoot.io/",
    },
    Source {
        name: "Transitous",
        provides: "Departures, trips and journey planning",
        licence: Some("per feed, see its sources"),
        url: transit::SOURCES_URL,
    },
    Source {
        name: "QualiCharge",
        provides: "EV chargers and their live availability",
        licence: None,
        url: charging::api::SOURCE_URL,
    },
    Source {
        name: "Wikipedia, Wikimedia Commons",
        provides: "Place descriptions and photos",
        licence: Some("CC BY-SA 4.0"),
        url: "https://creativecommons.org/licenses/by-sa/4.0/",
    },
    Source {
        name: "Wikidata",
        provides: "Place facts",
        licence: Some("CC0 1.0"),
        url: "https://www.wikidata.org/wiki/Wikidata:Licensing",
    },
    Source {
        name: "Lucide",
        provides: "Icons",
        licence: Some("ISC"),
        url: "https://lucide.dev/license",
    },
];

#[derive(PartialEq)]
pub struct SettingsPage {
    pub open: State<bool>,
}

impl Component for SettingsPage {
    fn render(&self) -> impl IntoElement {
        // Hooks before the early return: their count must not change between renders.
        let insets = use_screen().insets;
        let mut open = self.open;
        if !*open.read() {
            return rect().into_element();
        }

        let mut column: Vec<Element> = vec![
            section_title(lucide::info(), "About"),
            card(vec![
                label()
                    .text("Rusty Map")
                    .font_size(text_size(15.0))
                    .font_weight(FontWeight::BOLD)
                    .color(TEXT)
                    .into_element(),
                label()
                    .text(format!("Version {}", env!("CARGO_PKG_VERSION")))
                    .font_size(text_size(13.0))
                    .color(TEXT_MUTED)
                    .into_element(),
            ]),
            section_title(lucide::database(), "Data & attribution"),
        ];
        let rows = SOURCES
            .iter()
            .enumerate()
            .flat_map(|(index, source)| {
                let divider = (index > 0).then(|| {
                    rect()
                        .width(Size::fill())
                        .height(Size::px(1.0))
                        .background(DIVIDER)
                        .into_element()
                });
                divider.into_iter().chain([source_row(source)])
            })
            .collect();
        column.push(card(rows));

        rect()
            .position(Position::new_absolute().top(0.).left(0.))
            .width(Size::percent(100.))
            .height(Size::percent(100.))
            // As the favorites page; the two are never open together.
            .layer(Layer::OverlayLevel(3))
            .background(PAGE)
            .vertical()
            .on_global_key_down(move |e: Event<KeyboardEventData>| {
                if is_back(&e.key) {
                    open.set(false);
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
                            .on_press(move |_: Event<PressEventData>| open.set(false))
                            .child(icon(lucide::arrow_left(), TEXT_MUTED, 18.0)),
                    )
                    .child(icon(lucide::settings(), TEXT_MUTED, 18.0))
                    .child(
                        label()
                            .text("Settings")
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

/// One source: its name and what it provides, its licence; a tap opens its credits page.
fn source_row(source: &Source) -> Element {
    let url = source.url;
    rect()
        .horizontal()
        .content(Content::flex())
        .width(Size::fill())
        .cross_align(Alignment::Center)
        .spacing(12.0)
        .padding((tap_padding(10.0), 0.0))
        .cursor(CursorIcon::Pointer)
        .on_press(move |_: Event<PressEventData>| {
            haptic(Haptic::Press);
            Platform::get().open_url(url);
        })
        .child(
            rect()
                .vertical()
                .width(Size::flex(1.0))
                .spacing(2.0)
                .child(
                    label()
                        .text(source.name)
                        .font_size(text_size(14.0))
                        .font_weight(FontWeight::BOLD)
                        .color(TEXT),
                )
                .child(
                    label()
                        .text(source.provides)
                        .font_size(text_size(12.0))
                        .color(TEXT_MUTED),
                )
                .maybe_child(source.licence.map(|licence| {
                    label()
                        .text(licence)
                        .font_size(text_size(12.0))
                        .color(ACCENT)
                        .into_element()
                })),
        )
        .child(icon(lucide::external_link(), TEXT_MUTED, 16.0))
        .into_element()
}

fn card(children: Vec<Element>) -> Element {
    rect()
        .vertical()
        .width(Size::fill())
        .spacing(4.0)
        .padding((12.0f32, 16.0f32))
        .corner_radius(12.0f32)
        .background(Color::WHITE)
        .border(
            Border::new()
                .fill(DIVIDER)
                .width(1.0)
                .alignment(BorderAlignment::Inner),
        )
        .children(children)
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
