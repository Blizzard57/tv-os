//! The single content model: every item in the UI — game, movie, episode,
//! ROM — is a `ContentItem`, no matter which source produced it.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Game,
    Video,
    Movie,
    Series,
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
}

/// One horizontal row on the home screen.
#[derive(Serialize)]
pub struct Row {
    pub title: String,
    pub items: Vec<ContentItem>,
}
