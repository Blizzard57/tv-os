//! The single content model: every item in the UI — game, movie, episode,
//! ROM — is a `ContentItem`, no matter which source produced it.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Game,
    Video,
    Movie,
    Series,
    /// A live stream (a sports channel, a YouTube live broadcast). Rendered
    /// under the "Live" tab. `Action::Play` = currently live and playable;
    /// `Action::None` = a scheduled/upcoming event shown for information.
    Live,
}

/// What pressing A/Enter on the item should do. Decided by the daemon so the
/// UI stays a dumb renderer.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Play,
    Install,
    /// Browsable but not yet playable (e.g. a TMDB catalog entry before the
    /// stream-source phase lands).
    None,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ContentItem {
    /// Launchable id; the prefix names the owning source,
    /// e.g. "steam:620", "epic:Sugar", "video:https://…", "tmdb:movie:603".
    pub id: String,
    pub kind: Kind,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub art: Option<String>,
    pub action: Action,
    /// Optional subtitle override the card shows instead of the kind-derived
    /// line — e.g. a live fixture's carrier ("On Star Sports 1"). Most items
    /// leave this None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One horizontal row on the home screen.
#[derive(Serialize, Clone)]
pub struct Row {
    pub title: String,
    pub items: Vec<ContentItem>,
}
