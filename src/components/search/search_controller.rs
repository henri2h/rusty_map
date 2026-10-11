use freya::prelude::*;

use super::util::search::SearchResult;

/// Where the latest search stands, so the dropdown can say "Searching…"/"No results"/"Search
/// failed" instead of silently showing nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchStatus {
    /// Nothing to show: empty/too-short query, or a place is selected.
    Idle,
    /// Debouncing or waiting on Photon.
    Searching,
    /// `results` holds the latest answer (possibly empty).
    Done,
    /// The request failed (network/HTTP/parse error).
    Failed,
}

/// The search box's own live state — text, debounced Photon results, in-flight search task.
/// Not part of `map_component::MapController`: none of this is map
/// rendering state, it just happens to be written from two places ([`super::SearchPanel`]'s own
/// input, and `MapComponent`'s POI-tap handler via `on_place_tapped`/
/// `on_map_tapped`, both wired up by whoever owns both controllers — see `HomePage`).
#[derive(Clone, PartialEq)]
pub struct SearchController {
    /// Also written by a POI tap (mirrors the tapped name) and cleared on deselect, not just
    /// typed into.
    pub query: State<String>,
    pub results: State<Vec<SearchResult>>,
    pub status: State<SearchStatus>,
    pub task: State<Option<TaskHandle>>,
}

impl SearchController {
    /// A hook, like `use_state` — call it directly from a component's `render()` body (never
    /// conditionally), not from inside a plain helper function.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            query: use_state(String::new),
            results: use_state::<Vec<SearchResult>>(Vec::new),
            status: use_state(|| SearchStatus::Idle),
            task: use_state::<Option<TaskHandle>>(|| None),
        }
    }

    /// Cancels the in-flight (debouncing or fetching) search, if any.
    pub fn cancel(&mut self) {
        if let Some(handle) = self.task.peek().as_ref() {
            handle.cancel();
        }
        self.task.set(None);
    }

    /// Drops the results dropdown (cancelling any pending search) and replaces the box's text
    /// with `query` — the selected place's name, or empty when deselecting.
    pub fn reset(&mut self, query: String) {
        self.cancel();
        self.results.set(Vec::new());
        self.status.set(SearchStatus::Idle);
        self.query.set(query);
    }
}
