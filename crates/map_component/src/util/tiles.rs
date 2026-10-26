use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use bytes::Bytes;
use freya::engine::prelude::Image;
use mvt_reader::{Reader, feature::Feature, layer::Layer};
use reqwest::Client;
use skia_safe::Picture;
use tokio::runtime::Runtime;

use crate::style::StyleSpec;

use super::renderer::{self, CompiledTile, ZoomDependentEntry};

pub type TileKey = (u32, u32, u32);

/// A tile's decoded layers, each paired with its features. Coordinates in each feature's
/// geometry are tile-local (`0..layer.extent`, normally 4096), not world/screen pixels —
/// projecting them is the renderer's job, not this module's.
pub(crate) type DecodedTile = Vec<(Layer, Vec<Feature>)>;

pub fn background_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            // `spawn_blocking`'s pool otherwise grows to hundreds of threads: an initial load
            // kicks off dozens of decode/compile jobs at once, and running them all in parallel
            // oversubscribes every core — observed as wildly inflated per-stage timings (a
            // 1-feature tile "decoding" for 77 ms while 12k-feature tiles take 11 ms). A small
            // cap keeps each job running at full speed and the rest queued.
            .max_blocking_threads(
                std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(2)),
            )
            .enable_all()
            .build()
            .expect("failed to start background tile-fetching runtime")
    })
}

/// Runs `future` on [`background_runtime`] and awaits its output from whatever executor polls
/// this (freya's, in practice). Freya's own task executor isn't a Tokio runtime, so `tokio::fs`,
/// `tokio::time` and `reqwest` (which need one) can't run on it directly — they'd panic — and
/// CPU-bound work would stall painting; `tokio::sync::oneshot::Receiver` is executor-agnostic to
/// poll. `None` only if the task panicked.
async fn on_background<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
) -> Option<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let _ = tx.send(future.await);
    });
    rx.await.ok()
}

/// Runs CPU-bound `work` on [`background_runtime`]'s blocking pool (bounded, see there) and
/// awaits its result. `None` only if `work` panicked.
async fn on_blocking_pool<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn_blocking(move || {
        let _ = tx.send(work());
    });
    rx.await.ok()
}

/// Fetches, decodes, and compiles a tile against `style` (disk cache, then network, then MVT
/// decode, then style filtering/coloring), off the UI thread. `is_wanted` is asked before each
/// stage: a fast zoom queues hundreds of tiles, and without it the ones still on screen waited
/// seconds behind levels already zoomed past (disk reads share the small blocking pool with
/// decoding).
pub async fn fetch_tile(
    key: TileKey,
    style: Arc<StyleSpec>,
    is_wanted: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<CompiledTile, FetchError> {
    on_background(fetch_and_decode(key, style, is_wanted))
        .await
        .unwrap_or(Err(FetchError::TaskFailed))
}

async fn fetch_and_decode(
    key: TileKey,
    style: Arc<StyleSpec>,
    is_wanted: impl Fn() -> bool + Sync,
) -> Result<CompiledTile, FetchError> {
    if !is_wanted() {
        return Err(FetchError::Obsolete);
    }
    let io_start = std::time::Instant::now();
    let mut template = style.tile_url_for(key).to_string();
    let (data, from_disk) = match fetch_tile_io(key, &template, &is_wanted).await {
        // The primary source down or missing this tile: the fallback has the same schema.
        Err(err) if template == style.tile_url_template => {
            match &style.fallback_tile_url_template {
                Some(fallback) => {
                    let (z, x, y) = key;
                    log::debug!("[fetch {z}/{x}/{y}] {err:?}, trying the fallback source");
                    template = fallback.clone();
                    fetch_tile_io(key, &template, &is_wanted).await?
                }
                None => return Err(err),
            }
        }
        result => result?,
    };
    let io_ms = io_start.elapsed().as_millis();
    if io_ms > 1 {
        let (z, x, y) = key;
        log::debug!("[fetch {z}/{x}/{y}] io {io_ms} ms (disk cache or network)");
    }
    if !is_wanted() {
        return Err(FetchError::Obsolete);
    }
    match decode_tile(data, style.clone(), key).await {
        // A corrupt cached copy (e.g. a write cut short) would otherwise fail this tile on every
        // run, forever: drop it and fetch the tile again.
        Err(FetchError::InvalidData) if from_disk => {
            if let Some(path) = tile_cache_path(&template, key) {
                let _ = tokio::fs::remove_file(&path).await;
            }
            let (data, _) = fetch_tile_io(key, &template, &is_wanted).await?;
            decode_tile(data, style, key).await
        }
        result => result,
    }
}

/// Decodes raw MVT bytes into layers/features, builds their render-ready `Path`s, and compiles
/// the result against `style`. This is real CPU work (protobuf decode, eager per-feature geometry
/// construction, path building, style filtering), so it runs on Tokio's dedicated blocking-thread
/// pool via `spawn_blocking` rather than inline — valid here since this always executes from
/// inside a task already running on `background_runtime()`. Skia's `Path`/`Paint` are plain owned
/// FFI values (explicitly `Send`), so building them off the UI thread and moving the result back
/// is sound.
async fn decode_tile(
    data: Bytes,
    style: Arc<StyleSpec>,
    key: TileKey,
) -> Result<CompiledTile, FetchError> {
    let (zoom_z, x, y) = key;
    tokio::task::spawn_blocking(move || {
        let decode_start = std::time::Instant::now();
        let reader = Reader::new(data.to_vec()).map_err(|_| FetchError::InvalidData)?;
        let layers = reader
            .get_layer_metadata()
            .map_err(|_| FetchError::InvalidData)?;

        let decoded: DecodedTile = layers
            .into_iter()
            .enumerate()
            .map(|(index, layer)| {
                let features = reader
                    .get_features(index)
                    .map_err(|_| FetchError::InvalidData)?;
                Ok((layer, features))
            })
            .collect::<Result<_, FetchError>>()?;
        let decode_ms = decode_start.elapsed().as_millis();

        let build_start = std::time::Instant::now();
        let renderable = renderer::build_renderable_tile(decoded, &style);
        let build_ms = build_start.elapsed().as_millis();

        let compile_start = std::time::Instant::now();
        let compiled = renderer::compile_tile(&renderable, &style, zoom_z);
        let compile_ms = compile_start.elapsed().as_millis();

        let feature_count: usize = renderable.source_layers.values().map(Vec::len).sum();
        log::debug!(
            "[compile {zoom_z}/{x}/{y}] mvt-decode {decode_ms} ms, path-build {build_ms} ms, \
             style-compile {compile_ms} ms ({feature_count} features, \
             {} picture ops, {} zoom-dep layers, {} labels)",
            compiled.picture.approximate_op_count(),
            compiled.zoom_dependent.len(),
            compiled.labels.len()
        );
        Ok(compiled)
    })
    .await
    .unwrap_or(Err(FetchError::TaskFailed))
}

/// The tile's bytes, from the disk cache when present (second value `true`), else the network.
async fn fetch_tile_io(
    key: TileKey,
    tile_url_template: &str,
    is_wanted: &(dyn Fn() -> bool + Sync),
) -> Result<(Bytes, bool), FetchError> {
    let (z, x, y) = key;
    let path = tile_cache_path(tile_url_template, key);

    if let Some(path) = &path
        && let Ok(data) = tokio::fs::read(path).await
    {
        return Ok((Bytes::from(data), true));
    }
    if !is_wanted() {
        return Err(FetchError::Obsolete);
    }

    let url = tile_url_template
        .replace("{z}", &z.to_string())
        .replace("{x}", &x.to_string())
        .replace("{y}", &y.to_string());
    let data = http_get(&url).await?;
    if data.is_empty() {
        return Err(FetchError::NoData);
    }
    if let Some(path) = &path {
        write_cache_file(path, &data).await;
    }
    Ok((data, false))
}

/// Best-effort cache write: to a temporary sibling first, then renamed into place, so a crash or
/// full disk mid-write never leaves a truncated file behind to be read back as the real thing.
async fn write_cache_file(path: &std::path::Path, data: &[u8]) {
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = tokio::fs::create_dir_all(parent).await;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    if tokio::fs::write(&tmp, data).await.is_ok() && tokio::fs::rename(&tmp, path).await.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
}

/// Background-thread bridge for `renderer::rasterize_cell` (see `cache::cell_level`).
/// `is_current` is checked once a worker actually picks the job up; `None` = it had become
/// obsolete by then and was skipped.
pub(crate) async fn rasterize_cell_in_background(
    picture: Picture,
    zoom_dependent: Vec<ZoomDependentEntry>,
    style: Arc<StyleSpec>,
    zoom: f64,
    cell: (u32, u32, u32),
    size: i32,
    is_current: impl FnOnce() -> bool + Send + 'static,
) -> Option<Image> {
    on_blocking_pool(move || {
        is_current().then(|| {
            let start = std::time::Instant::now();
            let image =
                renderer::rasterize_cell(&picture, &zoom_dependent, &style, zoom, cell, size);
            let (level, cx, cy) = cell;
            log::debug!(
                "[cell] L{level} {cx},{cy} {size}px took {} ms",
                start.elapsed().as_millis()
            );
            image
        })
    })
    .await
    .flatten()
}

/// Background-thread bridge for `renderer::rasterize_zoom_dependent` (a tile's line overlay).
/// Same `is_current` contract as [`rasterize_cell_in_background`].
pub(crate) async fn rasterize_zoom_dependent_in_background(
    zoom_dependent: Vec<ZoomDependentEntry>,
    style: Arc<StyleSpec>,
    zoom: f64,
    size: i32,
    is_current: impl FnOnce() -> bool + Send + 'static,
) -> Option<Image> {
    on_blocking_pool(move || {
        is_current()
            .then(|| renderer::rasterize_zoom_dependent(&zoom_dependent, &style, zoom, size))
    })
    .await
    .flatten()
}

/// Waits `delay` on the background runtime's timer (`tokio::time::sleep` can't run on freya's
/// executor) — e.g. to debounce search input. The `Sleep` is created inside the spawned future:
/// `tokio::time::sleep` registers with the current runtime's timer as soon as it's called, so
/// calling it here on freya's thread panics with "no reactor running".
pub async fn background_delay(delay: std::time::Duration) {
    on_background(async move { tokio::time::sleep(delay).await }).await;
}

/// Fetches text over HTTP (style JSON, TileJSON) with a permanent disk cache: a disk hit means
/// zero network, a miss fetches then writes the cache best-effort. Invalidation is manual (delete
/// the file) or via [`invalidate_text_cache`] when the cached content turns out to be unusable
/// (e.g. fails to parse).
pub async fn fetch_text_cached(url: String) -> Result<String, FetchError> {
    let bytes = fetch_bytes_cached(url).await?;
    String::from_utf8(bytes.to_vec()).map_err(|_| FetchError::InvalidData)
}

/// Binary sibling of [`fetch_text_cached`]: same permanent disk cache, for sprite sheets etc.
pub async fn fetch_bytes_cached(url: String) -> Result<Bytes, FetchError> {
    on_background(async move {
        let path = text_cache_path(&url);
        if let Some(path) = &path
            && let Ok(data) = tokio::fs::read(path).await
        {
            return Ok(Bytes::from(data));
        }
        let data = http_get(&url).await?;
        if let Some(path) = &path {
            write_cache_file(path, &data).await;
        }
        Ok(data)
    })
    .await
    .unwrap_or(Err(FetchError::TaskFailed))
}

/// Deletes `url`'s cached copy (see [`fetch_text_cached`]) so the next fetch goes to the network
/// — used when a cached style fails to parse, which must not permanently pin the fallback style.
pub async fn invalidate_text_cache(url: String) {
    on_background(async move {
        if let Some(path) = text_cache_path(&url) {
            let _ = tokio::fs::remove_file(&path).await;
        }
    })
    .await;
}

/// Cache file for a fetched document (style JSON, TileJSON, sprite sheet), keyed by the URL with
/// every path-hostile character flattened to `_` — deterministic and human-readable, which beats
/// a hash for the handful of URLs this fetches.
fn text_cache_path(url: &str) -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| {
        dir.join("rusty_map")
            .join("styles")
            .join(flatten_path_chars(url, &['.', '_', '-']))
    })
}

/// `s` with every character that isn't ASCII alphanumeric or in `keep` replaced by `_`.
fn flatten_path_chars(s: &str, keep: &[char]) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || keep.contains(&c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `GET url` with the shared client; non-2xx statuses are errors. Must run on
/// [`background_runtime`] (`reqwest` needs Tokio).
async fn http_get(url: &str) -> Result<Bytes, FetchError> {
    log::trace!("[http] GET {url}");
    let response = client()
        .get(url)
        .send()
        .await
        .map_err(FetchError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }
    response.bytes().await.map_err(FetchError::Network)
}

/// `tiles/<source>/z/x/y.mvt` — per tile source, since different styles' sources use different
/// schemas (the offline default is shortbread, osm-bright is OpenMapTiles): keyed by z/x/y alone,
/// tiles fetched under the default style during startup were later read back by osm-bright,
/// which found none of its layers in them and drew those tiles blank, across restarts too.
fn tile_cache_path(tile_url_template: &str, (z, x, y): TileKey) -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| {
        dir.join("rusty_map")
            .join("tiles")
            .join(source_dir_name(tile_url_template))
            .join(z.to_string())
            .join(x.to_string())
            .join(format!("{y}.mvt"))
    })
}

/// Readable directory name for a tile source: its URL template minus the scheme and the
/// `{z}/{x}/{y}` part, with path-hostile characters flattened — e.g.
/// `openmaptiles.data.gouv.fr_data_planet-vector`.
fn source_dir_name(tile_url_template: &str) -> String {
    let base = tile_url_template
        .split_once("://")
        .map_or(tile_url_template, |(_, rest)| rest);
    let base = base
        .split("{z}")
        .next()
        .unwrap_or(base)
        .trim_end_matches('/');
    flatten_path_chars(base, &['.', '-'])
}

pub fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        // A timeout keeps one hung request from pinning one of the runtime's only two worker
        // threads indefinitely.
        Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("building a reqwest client with only a timeout set cannot fail")
    })
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum FetchError {
    Network(reqwest::Error),
    NoData,
    Http(u16),
    InvalidData,
    TaskFailed,
    /// Dropped unfetched: the view moved on before its turn came (see [`fetch_tile`]).
    Obsolete,
}
