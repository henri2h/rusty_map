//! The compact layout's drawer sheet (a place, a transit view, the journey planner): it rests at
//! one of three [`Detent`]s and follows the finger between them, so the map behind can be
//! uncovered — to pick a point on it, or to see the whole of a trip — without closing what the
//! sheet shows.
//!
//! The drag is the sheet's own (not freya's `SheetPull`, which only pulls down to close): a press
//! on the grab bar, or on the content while that isn't scrolled, moves the sheet either way; the
//! release settles it on a detent with a short animation. Below [`Detent::Full`] the content
//! doesn't drag-scroll (see [`SheetState::content_scrolls`]), or both would move at once.

use freya::{animation::*, prelude::*};
use torin::geometry::Area;

use crate::mobile::{Haptic, SHEET_PERCENT, Screen, haptic};

/// Where the drawer sheet rests.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Detent {
    /// Its header only: most of the map shows.
    Collapsed,
    Half,
    Full,
}

/// The collapsed sheet's height above the navigation bar: the grab bar and the first rows of
/// content (a place's name, the planner's title and both of its fields, a trip's summary).
const COLLAPSED_HEIGHT: f32 = 196.0;
/// A drag shorter than this settles back where it started.
const DRAG_SLOP: f32 = 40.0;
/// Pulling the collapsed sheet down by more than this closes what it shows.
const PULL_TO_CLOSE: f32 = 72.0;
/// A press that moved further than this was a drag, not a tap.
const TAP_SLOP: f32 = 8.0;
const SETTLE_MS: u64 = 220;

impl Detent {
    const ALL: [Detent; 3] = [Detent::Collapsed, Detent::Half, Detent::Full];

    /// Height in logical pixels, from the window's bottom edge, in a window `window_height` tall.
    fn height(self, screen: Screen, window_height: f32) -> f32 {
        let full = (window_height - screen.insets.top).max(0.0);
        match self {
            Self::Collapsed => (COLLAPSED_HEIGHT + screen.insets.bottom).min(full),
            Self::Half => window_height * SHEET_PERCENT / 100.0,
            Self::Full => full,
        }
    }

    /// What a tap on the grab bar opens: the next height up, or back to half from full.
    fn tapped(self) -> Self {
        match self {
            Self::Collapsed => Self::Half,
            Self::Half => Self::Full,
            Self::Full => Self::Half,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
struct Drag {
    /// Window y the press started at; `None` while not dragging.
    start: Option<f64>,
    /// How far down the sheet is dragged (negative: up).
    offset: f32,
    moved: bool,
}

/// What the release of a drag asks for, see [`SheetState::on_release`].
enum Settle {
    Stay,
    Rest(Detent),
    Close,
}

/// The sheet's detent and ongoing drag, shared with what's inside it through context.
#[derive(Clone, Copy, PartialEq)]
pub struct SheetState {
    detent: State<Detent>,
    drag: State<Drag>,
    /// Bumped with where a settle starts from, which (re)runs the settle animation.
    settle: State<(u32, i32)>,
    /// What detent heights derive from: the screen (its insets) and the window, which the map
    /// fills.
    screen: Memo<Screen>,
    window: State<Area>,
}

/// The drawer sheet's state, provided to its content. `window` is the map's area, which fills
/// the window.
pub fn use_sheet_state(window: State<Area>) -> SheetState {
    let screen = use_consume::<Memo<Screen>>();
    let state = SheetState {
        detent: use_state(|| Detent::Half),
        drag: use_state(Drag::default),
        settle: use_state(|| (0, 0)),
        screen,
        window,
    };
    use_provide_context(move || state);
    state
}

impl SheetState {
    /// The detent, subscribing to it.
    pub fn detent(&self) -> Detent {
        *self.detent.read()
    }

    pub fn peek_detent(&self) -> Detent {
        *self.detent.peek()
    }

    /// `detent`'s height on this screen, in logical pixels.
    pub fn height(&self, detent: Detent) -> f32 {
        detent.height(*self.screen.peek(), self.window.peek().size.height)
    }

    /// How much of the map the sheet covers at `detent`, for framing things above it: at most its
    /// half height, as a full-height sheet leaves no map to frame anything in — things are placed
    /// for when it comes back down.
    pub fn cover(&self, detent: Detent) -> f32 {
        self.height(detent).min(self.height(Detent::Half))
    }

    /// Rests at `detent`, animated from where the sheet is now.
    pub fn set_detent(&mut self, detent: Detent) {
        let current = *self.detent.peek();
        if current == detent {
            return;
        }
        let from = self.height(detent) - self.height(current);
        self.detent.set(detent);
        self.start_settle(from);
    }

    /// Back to half height without animating: for a sheet that opens (nothing to animate from).
    pub fn reset(&mut self) {
        if *self.detent.peek() != Detent::Half {
            self.detent.set(Detent::Half);
        }
    }

    /// Whether the content's scroll view drags (touch) — at full height only: below it, a drag
    /// moves the sheet instead. Subscribes to the detent.
    pub fn content_scrolls(&self) -> bool {
        *self.detent.read() == Detent::Full
    }

    fn start_settle(&mut self, from: f32) {
        let (generation, _) = *self.settle.peek();
        self.settle
            .set((generation.wrapping_add(1), from.round() as i32));
    }

    /// The bar atop the sheet: dragging it moves the sheet between its detents; a tap raises it
    /// a step, or lowers it from full height.
    pub fn grab_bar(self, color: Color) -> Element {
        let (mut drag, mut state) = (self.drag, self);
        rect()
            .width(Size::fill())
            .padding((12.0f32, 0.0f32))
            .center()
            .cursor(CursorIcon::Pointer)
            .on_pointer_down(move |e: Event<PointerEventData>| {
                e.stop_propagation();
                drag.set(Drag {
                    start: Some(e.global_location().y),
                    ..Drag::default()
                });
            })
            .on_press(move |_: Event<PressEventData>| {
                if !state.drag.peek().moved {
                    haptic(Haptic::Press);
                    let detent = state.peek_detent().tapped();
                    state.set_detent(detent);
                }
            })
            .child(
                rect()
                    .width(Size::px(36.))
                    .height(Size::px(4.))
                    .corner_radius(2.)
                    .background(color),
            )
            .into_element()
    }

    /// A press anywhere on the sheet: a drag unless it's on content scrolled down at full height
    /// (that drag scrolls it back).
    pub fn on_sheet_grab(
        self,
        scroll: SheetScroll,
    ) -> impl FnMut(Event<PointerEventData>) + 'static {
        let (mut drag, detent) = (self.drag, self.detent);
        move |e: Event<PointerEventData>| {
            let grabs = *detent.peek() != Detent::Full || scroll.at_top();
            drag.set(Drag {
                start: grabs.then(|| e.global_location().y),
                ..Drag::default()
            });
        }
    }

    /// The sheet follows the pointer: down as far as it goes, up to full height (at full height,
    /// up is the content's scroll).
    pub fn on_move(self) -> impl FnMut(Event<PointerEventData>) + 'static {
        let (state, mut drag) = (self, self.drag);
        move |e: Event<PointerEventData>| {
            let Drag { start, moved, .. } = *drag.peek();
            let Some(start) = start else {
                return;
            };
            let highest = state.height(state.peek_detent()) - state.height(Detent::Full);
            let offset = ((e.global_location().y - start) as f32).max(highest);
            drag.set(Drag {
                start: Some(start),
                offset,
                moved: moved || offset.abs() > TAP_SLOP,
            });
        }
    }

    /// Ends a drag, settling the sheet on the detent nearest where it was let go, one at least
    /// in the drag's direction. `on_close` runs for a pull down from the collapsed sheet. Wire as
    /// a *capture* global pointer up: a drag's release then doesn't press what it ends over (a
    /// trip card, which would re-fit the map to the sheet as it was mid-drag).
    pub fn on_release(
        self,
        mut on_close: impl FnMut() + 'static,
    ) -> impl FnMut(Event<PointerEventData>) + 'static {
        let mut state = self;
        move |e: Event<PointerEventData>| {
            let Drag {
                start,
                offset,
                moved,
            } = *state.drag.peek();
            if start.is_none() {
                return;
            }
            if moved {
                e.prevent_default();
            }
            state.drag.set(Drag {
                start: None,
                offset: 0.0,
                moved,
            });
            let current = state.peek_detent();
            match state.settle(current, offset) {
                Settle::Stay => state.start_settle(offset),
                Settle::Rest(detent) => {
                    // Animated from where it was let go, not from the old detent.
                    let released = state.height(current) - offset;
                    state.detent.set(detent);
                    state.start_settle(state.height(detent) - released);
                }
                Settle::Close => on_close(),
            }
        }
    }

    /// Where a drag `offset` down from `current` comes to rest.
    fn settle(&self, current: Detent, offset: f32) -> Settle {
        if offset.abs() < DRAG_SLOP {
            return Settle::Stay;
        }
        if current == Detent::Collapsed && offset > PULL_TO_CLOSE {
            return Settle::Close;
        }
        let released = self.height(current) - offset;
        let down = offset > 0.0;
        Detent::ALL
            .into_iter()
            .filter(|detent| {
                if down {
                    *detent < current
                } else {
                    *detent > current
                }
            })
            .min_by(|a, b| {
                let distance = |detent: &Detent| (self.height(*detent) - released).abs();
                distance(a).total_cmp(&distance(b))
            })
            .map_or(Settle::Stay, Settle::Rest)
    }

    /// How far down the sheet is drawn from its detent's height (negative: up), the ongoing drag
    /// or settle animation. A hook (it animates): call it once per render. Subscribes to both.
    pub fn use_offset(&self) -> f32 {
        let settle = *self.settle.read();
        let animation = use_animation_with_dependencies(&settle, |conf, (_, from)| {
            conf.on_creation(OnCreation::Finish);
            conf.on_change(OnChange::Rerun);
            AnimNum::new(*from as f32, 0.0)
                .time(SETTLE_MS)
                .ease(Ease::Out)
                .function(Function::Expo)
        });
        let drag = *self.drag.read();
        if drag.start.is_some() {
            drag.offset
        } else {
            animation.get().value()
        }
    }
}
