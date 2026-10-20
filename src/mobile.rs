//! Phone layout support: whether the window is narrow enough for the compact (bottom sheet)
//! layout, and the system bars the app draws under.
//!
//! Android draws edge to edge (enforced from API 35): `MainActivity` reports the status and
//! navigation bar heights over JNI and the overlays pad themselves. Both are zero on desktop.

use std::sync::LazyLock;

use freya::prelude::*;
use tokio::sync::watch;
use torin::geometry::Area;

/// Compact layout: the bottom sheet's height, as a percentage of the window, until expanded.
pub const SHEET_PERCENT: f32 = 55.0;

/// Below this window width (logical pixels) the drawer becomes a bottom sheet and the search
/// box spans the width: a 360 px side drawer would cover a phone's whole map.
const COMPACT_WIDTH: f32 = 600.0;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Insets {
    /// Status bar (and display cutout).
    pub top: f32,
    /// Navigation bar: gesture handle or buttons.
    pub bottom: f32,
}

/// Typical values until Android reports the real ones.
const DEFAULT_INSETS: Insets = if cfg!(target_os = "android") {
    Insets {
        top: 40.,
        bottom: 20.,
    }
} else {
    Insets {
        top: 0.,
        bottom: 0.,
    }
};

/// Android may report insets before the UI exists, hence a static rather than a `State`.
static INSETS: LazyLock<watch::Sender<Insets>> =
    LazyLock::new(|| watch::Sender::new(DEFAULT_INSETS));

/// `MainActivity.nativeSetInsets(top, bottom)`, in dp.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_rustymap_app_MainActivity_nativeSetInsets(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    top: f32,
    bottom: f32,
) {
    INSETS.send_if_modified(|current| {
        let insets = Insets { top, bottom };
        let changed = *current != insets;
        *current = insets;
        changed
    });
}

/// Whether a window `width` wide gets the compact layout. The map's width is zero until its
/// first layout: guess from the platform rather than flash the wrong layout.
pub fn is_compact(width: f32) -> bool {
    if width > 0.0 {
        width < COMPACT_WIDTH
    } else {
        cfg!(target_os = "android")
    }
}

/// Escape on desktop, the system back button or gesture on Android.
pub fn is_back(key: &Key) -> bool {
    matches!(key, Key::Named(NamedKey::Escape | NamedKey::BrowserBack))
}

/// Sizes for fingers rather than a mouse pointer. A build-time switch (Android) rather than
/// [`Screen::compact`]: a phone is touched whatever its width, a narrow desktop window isn't.
pub const TOUCH: bool = cfg!(target_os = "android");

/// A font size, a little larger on touch screens, where text sits further from the eye and
/// small labels double as tap targets.
pub const fn text_size(desktop: f32) -> f32 {
    if TOUCH { desktop + 2.0 } else { desktop }
}

/// Vertical padding of a tappable row or chip: on touch screens, enough for a ~44–48 px
/// target around one line of text.
pub const fn tap_padding(desktop: f32) -> f32 {
    if TOUCH && desktop < 12.0 {
        12.0
    } else {
        desktop
    }
}

/// A pull on a bottom sheet released further down than this closes it; less springs it back.
const PULL_TO_CLOSE: f32 = 60.0;
/// A pull past this isn't a tap any more: what it's released over isn't pressed.
const PULL_SLOP: f32 = 8.0;

/// A bottom sheet being pulled down. Wire [`on_pull_start`] on what grabs the sheet,
/// [`on_pull_move`]/[`on_pull_end`] on the sheet itself, and offset its bottom edge by
/// `-offset`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Pull {
    /// Window y where the pointer went down; `None` while not pulling.
    start: Option<f64>,
    /// How far down the sheet is pulled.
    pub offset: f32,
    /// The pull went past [`PULL_SLOP`]; kept until the next press, so that whatever the pull
    /// was released over ignores its press.
    pub moved: bool,
}

/// Stops the press there: a sheet's own [`on_sheet_pull_start`] mustn't then cancel the pull.
pub fn on_pull_start(mut pull: State<Pull>) -> impl FnMut(Event<PointerEventData>) + 'static {
    move |e: Event<PointerEventData>| {
        e.stop_propagation();
        pull.set(Pull {
            start: Some(e.global_location().y),
            ..Pull::default()
        });
    }
}

/// Like [`on_pull_start`], for a whole sheet whose content scrolls: a press starts a pull only
/// while that content is scrolled to its top, so dragging scrolled content scrolls it back
/// rather than dragging the sheet.
pub fn on_sheet_pull_start(
    mut pull: State<Pull>,
    scroll: SheetScroll,
) -> impl FnMut(Event<PointerEventData>) + 'static {
    move |e: Event<PointerEventData>| {
        pull.set(Pull {
            start: scroll.at_top().then(|| e.global_location().y),
            ..Pull::default()
        });
    }
}

/// Where a sheet's vertical `ScrollView` and its content start on screen, to tell whether the
/// content is scrolled (Freya's `ScrollController` keeps its offset private). Wire with
/// [`SheetScroll::track`] on the view and [`SheetScroll::marker`] as its first child. The sheet
/// shows one such view at a time, so one pair of positions serves them all.
#[derive(Clone, Copy, PartialEq)]
pub struct SheetScroll {
    viewport_top: State<f32>,
    content_top: State<f32>,
}

pub fn use_sheet_scroll() -> SheetScroll {
    SheetScroll {
        viewport_top: use_state(|| 0.0),
        content_top: use_state(|| 0.0),
    }
}

impl SheetScroll {
    /// Not scrolled down (pulled past the top, by overscroll, counts too).
    pub fn at_top(&self) -> bool {
        *self.content_top.peek() >= *self.viewport_top.peek() - 1.0
    }

    pub fn track(self, view: ScrollView) -> ScrollView {
        let mut viewport_top = self.viewport_top;
        view.on_sized(move |e: Event<SizedEventData>| viewport_top.set(e.area.min_y()))
    }

    /// Zero-height, at the content's top: moves with the scroll.
    pub fn marker(self) -> Element {
        let mut content_top = self.content_top;
        rect()
            .width(Size::fill())
            .height(Size::px(0.0))
            .on_sized(move |e: Event<SizedEventData>| content_top.set(e.area.min_y()))
            .into_element()
    }
}

/// The sheet follows the pointer down, never up past where it rests.
pub fn on_pull_move(mut pull: State<Pull>) -> impl FnMut(Event<PointerEventData>) + 'static {
    move |e: Event<PointerEventData>| {
        let Some(start) = pull.peek().start else {
            return;
        };
        let offset = (e.global_location().y - start).max(0.0) as f32;
        let moved = pull.peek().moved || offset > PULL_SLOP;
        pull.set(Pull {
            start: Some(start),
            offset,
            moved,
        });
    }
}

/// Runs `on_close` when the pull is released far enough down.
pub fn on_pull_end(
    mut pull: State<Pull>,
    mut on_close: impl FnMut() + 'static,
) -> impl FnMut(Event<PointerEventData>) + 'static {
    move |_: Event<PointerEventData>| {
        let Pull {
            start,
            offset,
            moved,
        } = *pull.peek();
        if start.is_none() {
            return;
        }
        pull.set(Pull {
            start: None,
            offset: 0.0,
            moved,
        });
        if offset > PULL_TO_CLOSE {
            haptic(Haptic::Press);
            on_close();
        }
    }
}

/// Shares `text` (`subject` as its title) through the system share sheet. Elsewhere there's
/// none: copies `text` to the clipboard instead, returning `true` then.
pub fn share(subject: &str, text: &str) -> bool {
    #[cfg(target_os = "android")]
    {
        if let Err(e) = call_activity(
            "share",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[Arg::Str(subject), Arg::Str(text)],
        ) {
            log::warn!("share failed: {e}");
        }
        false
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = subject;
        Clipboard::set(text.to_string()).is_ok()
    }
}

/// Opens `url` in the app that handles it: the browser, or for `tel:` the phone app with the
/// number dialled (Android) — freya's own `Platform::open_url` runs `xdg-open`, which Android
/// doesn't have.
pub fn open_url(url: impl Into<String>) {
    let url = url.into();
    #[cfg(target_os = "android")]
    if let Err(e) = call_activity("openUrl", "(Ljava/lang/String;)V", &[Arg::Str(&url)]) {
        log::warn!("open_url({url}) failed: {e}");
    }
    #[cfg(not(target_os = "android"))]
    Platform::get().open_url(url);
}

/// Keeps the screen on (Android), e.g. while guiding a walk. No-op on desktop.
pub fn keep_screen_on(on: bool) {
    #[cfg(target_os = "android")]
    if let Err(e) = call_activity("setKeepScreenOn", "(Z)V", &[Arg::Bool(on)]) {
        log::warn!("keep_screen_on({on}) failed: {e}");
    }
    #[cfg(not(target_os = "android"))]
    let _ = on;
}

/// Kinds of haptic feedback, mapped to Android's `HapticFeedbackConstants` in `MainActivity`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Haptic {
    /// A button or control press.
    Press = 0,
    /// Something switched on or off (a layer, location).
    Toggle = 1,
    /// A place or marker picked on the map.
    Select = 2,
}

/// A short vibration, where the device has one and the user hasn't turned touch feedback off.
/// No-op on desktop.
pub fn haptic(kind: Haptic) {
    #[cfg(target_os = "android")]
    if let Err(e) = call_activity("haptic", "(I)V", &[Arg::Int(kind as i32)]) {
        log::debug!("haptic({kind:?}) failed: {e}");
    }
    #[cfg(not(target_os = "android"))]
    let _ = kind;
}

/// An argument to a `MainActivity` method, for [`call_activity`].
#[cfg(target_os = "android")]
pub(crate) enum Arg<'a> {
    Bool(bool),
    Int(i32),
    Str(&'a str),
}

/// Set by `android_main`, for [`call_activity`].
#[cfg(target_os = "android")]
pub(crate) static ANDROID_APP: std::sync::OnceLock<winit::platform::android::activity::AndroidApp> =
    std::sync::OnceLock::new();

/// Calls `MainActivity.<method>` from any thread. A Java exception is logged (its stack trace
/// under `System.err`, its message under `rusty_map`) and cleared — the JNI error alone only
/// says one was thrown.
#[cfg(target_os = "android")]
pub(crate) fn call_activity(
    method: &str,
    signature: &str,
    args: &[Arg],
) -> jni::errors::Result<()> {
    use jni::{
        JavaVM,
        objects::{JObject, JValueOwned},
    };

    // The activity itself: `ndk_context`'s context is the `Application`, which has none of
    // `MainActivity`'s methods.
    let Some(app) = ANDROID_APP.get() else {
        return Err(jni::errors::Error::NullPtr("AndroidApp not set"));
    };
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let args = args
        .iter()
        .map(|arg| {
            Ok(match arg {
                Arg::Bool(value) => JValueOwned::Bool((*value).into()),
                Arg::Int(value) => JValueOwned::Int(*value),
                Arg::Str(value) => JValueOwned::Object(env.new_string(value)?.into()),
            })
        })
        .collect::<jni::errors::Result<Vec<_>>>()?;
    let args: Vec<_> = args.iter().map(|arg| arg.borrow()).collect();
    let result = env.call_method(&activity, method, signature, &args);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        if let Ok(exception) = env.exception_occurred() {
            let _ = env.exception_clear();
            // Into our own log too (tag `rusty_map`): `System.err` is easy to filter out.
            let message = env
                .call_method(&exception, "toString", "()Ljava/lang/String;", &[])
                .and_then(|value| value.l())
                .and_then(|text| env.get_string(&text.into()).map(String::from));
            let _ = env.exception_clear();
            log::warn!("MainActivity.{method}: {}", message.unwrap_or_default());
        }
    }
    result.map(|_| ())
}

/// What the overlays lay themselves out against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen {
    pub compact: bool,
    pub insets: Insets,
}

impl Screen {
    /// Top offset of the map controls down the right edge: below the status bar, and in the
    /// compact layout below the search box, which then spans the window's width.
    pub fn controls_top(&self) -> f32 {
        self.insets.top + if self.compact { 64. } else { 0. }
    }
}

/// Provides the [`Screen`] to the tree under it, derived from the map's `viewport_size`. A memo,
/// so that a window resize only re-renders the overlays when it crosses [`COMPACT_WIDTH`].
pub fn use_provide_screen(viewport: State<Area>) -> Memo<Screen> {
    let insets = use_insets();
    let screen = use_memo(move || Screen {
        compact: is_compact(viewport.read().width()),
        insets: *insets.read(),
    });
    use_provide_context(move || screen);
    screen
}

/// The [`Screen`] provided by [`use_provide_screen`], subscribing the caller to its changes.
pub fn use_screen() -> Screen {
    let screen = use_consume::<Memo<Screen>>();
    *screen.read()
}

fn use_insets() -> State<Insets> {
    use_watch(&INSETS)
}

/// The value of a process-wide `watch` channel, fed by a platform callback (Android JNI), as a
/// `State` re-rendering its readers when it changes.
pub fn use_watch<T: Clone + 'static>(sender: &'static watch::Sender<T>) -> State<T> {
    let mut state = use_state(|| sender.borrow().clone());
    use_hook(move || {
        let mut rx = sender.subscribe();
        spawn(async move {
            while rx.changed().await.is_ok() {
                let value = rx.borrow_and_update().clone();
                state.set(value);
            }
        });
    });
    state
}
