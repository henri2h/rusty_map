//! Street-level photos of a place from [Panoramax](https://panoramax.fr), France's open,
//! OSM-friendly street imagery commons (CC BY-SA photos, contributed by the public — Aix's
//! included): the pictures taken near the place and facing it, or the very picture an OSM
//! `panoramax` tag names. Shown in the place drawer; a tap shows the photo full window.

use bytes::Bytes;
use freya::prelude::*;
use serde::Deserialize;
use torin::prelude::Direction;

use map_component::util::tiles::{FetchError, background_runtime, client};

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use freya::icons::lucide;
use torin::position::Position;

use crate::{
    components::place::section_key,
    mobile::{Haptic, haptic, is_back, tap_padding, text_size, use_screen},
};

const API: &str = "https://api.panoramax.xyz/api/search";

/// Panoramax' meta-catalogue asks no key; identify the app like every other request.
const USER_AGENT: &str = concat!("rusty_map/", env!("CARGO_PKG_VERSION"), " (map@carnot.cc)");

/// Pictures taken further than this from the place rarely show it.
const MAX_DISTANCE_M: u32 = 40;

/// How many photos the strip shows.
const PHOTO_COUNT: usize = 4;

const TEXT_MUTED: Color = Color::from_rgb(95, 99, 104);
const TEXT_FAINT: Color = Color::from_rgb(140, 144, 150);
const SURFACE_ALT: Color = Color::from_rgb(244, 246, 248);

/// One street-level photo, its thumbnail loaded.
#[derive(Clone, PartialEq)]
struct Photo {
    id: String,
    thumb: Bytes,
    /// The larger picture the viewer shows (Panoramax' `sd`, ~2048 px wide), fetched on open.
    large_url: Option<String>,
    /// Who took it, for the CC BY-SA attribution.
    producer: Option<String>,
    /// `2025-10-23`.
    date: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    id: String,
    #[serde(default)]
    properties: Properties,
    assets: Assets,
    #[serde(default)]
    providers: Vec<Provider>,
}

#[derive(Deserialize, Default)]
struct Properties {
    datetime: Option<String>,
}

#[derive(Deserialize)]
struct Assets {
    thumb: Option<Asset>,
    sd: Option<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    href: String,
}

#[derive(Deserialize)]
struct Provider {
    name: String,
    #[serde(default)]
    roles: Vec<String>,
}

/// The photos facing (`lat`, `lon`) from within [`MAX_DISTANCE_M`], `picture` (an OSM
/// `panoramax` tag's id) first when given — thumbnails included.
async fn fetch_photos(
    lat: f64,
    lon: f64,
    picture: Option<String>,
) -> Result<Vec<Photo>, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let _ = tx.send(fetch_photos_io(lat, lon, picture).await);
    });
    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_photos_io(
    lat: f64,
    lon: f64,
    picture: Option<String>,
) -> Result<Vec<Photo>, FetchError> {
    let mut features = Vec::new();
    if let Some(id) = picture {
        features.extend(search(&[("ids", id)]).await.unwrap_or_default());
    }
    features.extend(
        search(&[
            ("place_position", format!("{lon},{lat}")),
            ("place_distance", format!("0-{MAX_DISTANCE_M}")),
            // Pictures whose field of view holds the place, not just any taken nearby.
            ("place_fov_tolerance", "70".to_string()),
            ("limit", (PHOTO_COUNT * 2).to_string()),
        ])
        .await?,
    );

    let mut photos = Vec::new();
    for feature in features {
        if photos.len() == PHOTO_COUNT {
            break;
        }
        if photos.iter().any(|photo: &Photo| photo.id == feature.id) {
            continue;
        }
        let Some(thumb) = feature.assets.thumb else {
            continue;
        };
        let Ok(thumb) = get_bytes(&thumb.href).await else {
            continue;
        };
        let producer = feature
            .providers
            .into_iter()
            .find(|provider| provider.roles.iter().any(|role| role == "producer"))
            .map(|provider| provider.name);
        let date = feature
            .properties
            .datetime
            .and_then(|datetime| datetime.split('T').next().map(str::to_string));
        photos.push(Photo {
            id: feature.id,
            thumb,
            large_url: feature.assets.sd.map(|sd| sd.href),
            producer,
            date,
        });
    }
    Ok(photos)
}

async fn search(query: &[(&str, String)]) -> Result<Vec<Feature>, FetchError> {
    log::trace!("[http] GET {API} {query:?}");
    let response = client()
        .get(API)
        .query(query)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(FetchError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    let text = response.text().await.map_err(FetchError::Network)?;
    let parsed: SearchResponse =
        serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
    Ok(parsed.features)
}

/// `url`'s bytes, off the UI thread.
async fn fetch_bytes(url: String) -> Result<Bytes, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let _ = tx.send(get_bytes(&url).await);
    });
    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

async fn get_bytes(url: &str) -> Result<Bytes, FetchError> {
    log::trace!("[http] GET {url}");
    let response = client()
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(FetchError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    response.bytes().await.map_err(FetchError::Network)
}

/// The place's street-level photos, as a horizontal strip; nothing at all where Panoramax has
/// none. Keyed by the place, so each selection fetches its own.
#[derive(PartialEq)]
pub struct StreetPhotos {
    pub place_id: u64,
    pub lat: f64,
    pub lon: f64,
    /// The OSM `panoramax` tag: a picture the mappers chose for this place.
    pub picture: Option<String>,
}

impl Component for StreetPhotos {
    fn render(&self) -> impl IntoElement {
        let photos = use_state::<Vec<Photo>>(Vec::new);
        // The photo shown large, by index.
        let viewing = use_state::<Option<usize>>(|| None);
        use_hook({
            let (lat, lon, picture) = (self.lat, self.lon, self.picture.clone());
            let mut photos = photos;
            let platform = Platform::get().current_window();
            move || {
                spawn(async move {
                    match fetch_photos(lat, lon, picture).await {
                        Ok(found) if !found.is_empty() => {
                            photos.set(found);
                            platform.send(UserEvent::RequestRedraw);
                        }
                        Ok(_) => {}
                        Err(err) => log::debug!("[panoramax] lookup failed: {err:?}"),
                    }
                });
            }
        });

        let photos = photos.read();
        if photos.is_empty() {
            return rect().into_element();
        }
        let credit = photos
            .iter()
            .filter_map(|photo| photo.producer.clone())
            .fold(Vec::<String>::new(), |mut names, name| {
                if !names.contains(&name) {
                    names.push(name);
                }
                names
            })
            .join(", ");

        rect()
            .vertical()
            .width(Size::fill())
            .spacing(6.0)
            .margin((8.0f32, 0.0f32, 8.0f32, 0.0f32))
            .child(
                label()
                    .text("Street-level photos · Panoramax")
                    .font_size(text_size(12.0))
                    .color(TEXT_MUTED),
            )
            .child(
                ScrollView::new()
                    .direction(Direction::horizontal())
                    .width(Size::fill())
                    // A photo plus its date line: `auto` leaves a gap under a horizontal strip.
                    .height(Size::px(120.0))
                    .show_scrollbar(false)
                    .child(
                        rect().horizontal().spacing(8.0).children(
                            photos
                                .iter()
                                .enumerate()
                                .map(|(index, photo)| photo_card(photo, index, viewing)),
                        ),
                    ),
            )
            .maybe_child((*viewing.read()).map(|index| {
                PhotoViewer {
                    photos: photos.clone(),
                    index,
                    viewing,
                }
                .into_element()
            }))
            .child(
                label()
                    .text(if credit.is_empty() {
                        "CC BY-SA 4.0".to_string()
                    } else {
                        format!("© {credit} · CC BY-SA 4.0")
                    })
                    .font_size(text_size(11.0))
                    .color(TEXT_FAINT)
                    .max_lines(1)
                    .text_overflow(TextOverflow::Ellipsis),
            )
            .into_element()
    }

    fn render_key(&self) -> DiffKey {
        section_key("panoramax", self.place_id)
    }
}

/// When the photo viewer last closed, or `None` while it's open — see [`viewer_handles_back`].
static VIEWER_CLOSED: Mutex<Option<Option<Instant>>> = Mutex::new(None);

/// `true` while the photo viewer is open, or for a moment after back closed it: the drawer's own
/// back handler (another global key handler, run in no set order) must leave the place open.
pub fn viewer_handles_back() -> bool {
    match *VIEWER_CLOSED.lock().unwrap() {
        Some(None) => true,
        Some(Some(closed)) => closed.elapsed() < Duration::from_millis(300),
        None => false,
    }
}

fn set_viewer_open(open: bool) {
    *VIEWER_CLOSED.lock().unwrap() = Some((!open).then(Instant::now));
}

/// A photo's thumbnail with its date; a tap shows it large (see [`PhotoViewer`]).
fn photo_card(photo: &Photo, index: usize, mut viewing: State<Option<usize>>) -> Element {
    rect()
        .vertical()
        .spacing(2.0)
        .cursor(CursorIcon::Pointer)
        .on_press(move |_: Event<PressEventData>| {
            haptic(Haptic::Press);
            set_viewer_open(true);
            viewing.set(Some(index));
        })
        .child(
            rect()
                .width(Size::px(160.0))
                .height(Size::px(100.0))
                .corner_radius(8.0f32)
                .overflow(Overflow::Clip)
                .background(SURFACE_ALT)
                .child(
                    ImageViewer::new((format!("panoramax-{}", photo.id), photo.thumb.clone()))
                        .width(Size::fill())
                        .height(Size::fill())
                        .aspect_ratio(AspectRatio::Max)
                        .image_cover(ImageCover::Center),
                ),
        )
        .maybe_child(photo.date.clone().map(|date| {
            label()
                .text(date)
                .font_size(text_size(11.0))
                .color(TEXT_FAINT)
                .into_element()
        }))
        .into_element()
}

/// A [`round_button`]'s outer size.
const BUTTON_SIZE: f32 = 22.0 + 2.0 * tap_padding(10.0);

/// Width of the column holding each ‹ › button beside the photo.
const ARROW_COLUMN: f32 = BUTTON_SIZE + 16.0;

/// Room for the loading line, the credit and the Panoramax button under the photo.
const BOTTOM_BAR_HEIGHT: f32 = 120.0;

/// A round translucent icon button over the photo.
fn round_button(svg: Bytes, mut on_press: impl FnMut() + 'static) -> Element {
    rect()
        .padding(tap_padding(10.0))
        .corner_radius(24.0f32)
        .background(Color::from_argb(110, 0, 0, 0))
        .cursor(CursorIcon::Pointer)
        .on_press(move |e: Event<PressEventData>| {
            e.stop_propagation();
            haptic(Haptic::Press);
            on_press();
        })
        .child(
            SvgViewer::new(svg)
                .width(Size::px(22.0))
                .height(Size::px(22.0))
                .color(Color::WHITE),
        )
        .into_element()
}

/// The photos full window, one at a time: the larger picture once loaded (the thumbnail
/// meanwhile), ‹ › (or the arrow keys) to step through them, back or ✕ to close, and a link to
/// Panoramax' own viewer — to look around a 360° picture, or walk along its sequence.
#[derive(PartialEq)]
struct PhotoViewer {
    photos: Vec<Photo>,
    index: usize,
    viewing: State<Option<usize>>,
}

impl Component for PhotoViewer {
    fn render(&self) -> impl IntoElement {
        let screen = use_screen();
        let mut viewing = self.viewing;
        // Larger pictures fetched so far, by photo id.
        let mut large = use_state::<HashMap<String, Bytes>>(HashMap::new);
        // Ids whose larger picture was asked for, so each is fetched once.
        let mut requested = use_state(Vec::<String>::new);
        let count = self.photos.len();
        let index = self.index.min(count.saturating_sub(1));
        let photo = self.photos[index].clone();

        // The shown photo's larger picture, fetched once.
        if !large.peek().contains_key(&photo.id)
            && let Some(url) = photo.large_url.clone()
        {
            let id = photo.id.clone();
            if !requested.peek().contains(&id) {
                requested.write().push(id.clone());
                spawn(async move {
                    match fetch_bytes(url).await {
                        Ok(bytes) => {
                            large.write().insert(id, bytes);
                        }
                        Err(err) => log::debug!("[panoramax] large photo failed: {err:?}"),
                    }
                });
            }
        }

        let mut close = move || {
            set_viewer_open(false);
            viewing.set(None);
        };
        let mut step = move |delta: isize| {
            let next = (index as isize + delta).rem_euclid(count as isize) as usize;
            viewing.set(Some(next));
        };

        let loaded = large.read().get(&photo.id).cloned();
        let loading = loaded.is_none() && photo.large_url.is_some();
        let (image_key, image) = match loaded {
            Some(bytes) => (format!("panoramax-large-{}", photo.id), bytes),
            None => (format!("panoramax-{}", photo.id), photo.thumb.clone()),
        };
        let credit = [
            photo.producer.clone().map(|name| format!("© {name}")),
            photo.date.clone(),
            Some("CC BY-SA 4.0".to_string()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let viewer_url = format!("https://api.panoramax.xyz/#focus=pic&pic={}", photo.id);

        // Fixed bar heights, so the photo gets exactly the rest of the window and always fits
        // between them (`Size::flex` doesn't resolve in this window-sized overlay).
        let top_height = 12.0 + screen.insets.top + BUTTON_SIZE + 12.0;
        let bottom_height = BOTTOM_BAR_HEIGHT + screen.insets.bottom;
        let bars = (top_height + bottom_height) as u32;
        let photo_height = Size::func_data(
            move |c| Some((c.root - (top_height + bottom_height)).max(0.0)),
            &bars,
        );
        // The photo between the two arrows.
        let arrows = if count > 1 { 2.0 * ARROW_COLUMN } else { 0.0 };
        let photo_width =
            Size::func_data(move |c| Some((c.root - arrows).max(0.0)), &(arrows as u32));

        let top_bar = rect()
            .horizontal()
            .content(Content::flex())
            .width(Size::fill())
            .height(Size::px(top_height))
            .cross_align(Alignment::Center)
            .padding((12.0 + screen.insets.top, 12.0, 12.0, 12.0))
            .child(
                label()
                    .text(if count > 1 {
                        format!("{} / {count}", index + 1)
                    } else {
                        String::new()
                    })
                    .width(Size::flex(1.0))
                    .font_size(text_size(14.0))
                    .color(Color::WHITE),
            )
            .child(round_button(lucide::x(), close));

        let arrow = move |svg: Bytes, delta: isize| {
            rect()
                .width(Size::px(ARROW_COLUMN))
                .center()
                .child(round_button(svg, move || step(delta)))
                .into_element()
        };
        let middle = rect()
            .horizontal()
            .width(Size::fill())
            .height(photo_height.clone())
            .cross_align(Alignment::Center)
            .maybe_child((count > 1).then(|| arrow(lucide::chevron_left(), -1)))
            .child(
                rect()
                    .width(photo_width.clone())
                    .height(photo_height.clone())
                    .center()
                    .child(
                        ImageViewer::new((image_key, image))
                            .width(photo_width)
                            .height(photo_height)
                            .aspect_ratio(AspectRatio::Min)
                            // The fitted picture is narrower or shorter than the box: centre it
                            // (the default anchors it top-left).
                            .image_cover(ImageCover::Center),
                    ),
            )
            .maybe_child((count > 1).then(|| arrow(lucide::chevron_right(), 1)));

        let faded = Color::from_argb(200, 255, 255, 255);
        let bottom_bar = rect()
            .vertical()
            .width(Size::fill())
            .height(Size::px(bottom_height))
            .spacing(8.0)
            .padding((12.0, 16.0, 16.0 + screen.insets.bottom, 16.0))
            .cross_align(Alignment::Center)
            .maybe_child(loading.then(|| {
                label()
                    .text("Loading the full picture…")
                    .font_size(text_size(12.0))
                    .color(faded)
                    .into_element()
            }))
            .child(
                label()
                    .text(credit)
                    .width(Size::fill())
                    .font_size(text_size(12.0))
                    .color(faded)
                    .text_align(TextAlign::Center),
            )
            .child(
                // A plain outlined pill: the theme's `Button` has a light background, which hid
                // the white label.
                rect()
                    .padding((tap_padding(8.0), 16.0))
                    .corner_radius(20.0f32)
                    .background(Color::from_argb(40, 255, 255, 255))
                    .border(
                        Border::new()
                            .fill(Color::from_argb(160, 255, 255, 255))
                            .width(1.0)
                            .alignment(BorderAlignment::Inner),
                    )
                    .cursor(CursorIcon::Pointer)
                    .on_press(move |e: Event<PressEventData>| {
                        e.stop_propagation();
                        haptic(Haptic::Press);
                        crate::mobile::open_url(viewer_url.clone())
                    })
                    .child(
                        label()
                            .text("Look around in Panoramax")
                            .font_size(text_size(13.0))
                            .color(Color::WHITE),
                    ),
            );

        rect()
            .position(Position::new_global().top(0.).left(0.))
            .width(Size::window_percent(100.))
            .height(Size::window_percent(100.))
            .layer(Layer::OverlayLevel(5))
            .background(Color::from_argb(250, 0, 0, 0))
            .vertical()
            // Swallows presses and drags so nothing under the viewer reacts.
            .on_press(|e: Event<PressEventData>| e.stop_propagation())
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_global_key_down(move |e: Event<KeyboardEventData>| match &e.key {
                key if is_back(key) => close(),
                Key::Named(NamedKey::ArrowLeft) if count > 1 => step(-1),
                Key::Named(NamedKey::ArrowRight) if count > 1 => step(1),
                _ => {}
            })
            .child(top_bar)
            .child(middle)
            .child(bottom_bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_results() {
        let parsed: SearchResponse = serde_json::from_str(
            r#"{"features": [{"id": "3fcb", "properties": {"datetime": "2025-10-23T08:49:49Z"},
                "assets": {"thumb": {"href": "https://example.org/thumb.jpg"}},
                "providers": [{"name": "Moosh", "roles": ["producer"]}]}], "links": []}"#,
        )
        .unwrap();
        let feature = &parsed.features[0];
        assert_eq!(feature.id, "3fcb");
        assert_eq!(
            feature.assets.thumb.as_ref().unwrap().href,
            "https://example.org/thumb.jpg"
        );
        assert_eq!(feature.providers[0].name, "Moosh");
    }
}
