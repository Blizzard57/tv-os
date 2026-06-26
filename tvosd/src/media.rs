//! Shared types for the details page: rich metadata (with episode lists) and
//! the different kinds of playable stream a Stremio addon can return.

use serde::{Deserialize, Serialize};

/// A details-page summary for any entry — movie, series, or game.
#[derive(Serialize, Default, Clone)]
pub struct Meta {
    pub id: String,
    pub kind: String, // movie | series | game
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Studio that made it (games: developer). Empty for most video.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer: Option<String>,
    /// Who released it (games: publisher).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    pub genres: Vec<String>,
    /// Short feature/category tags (games: Single-player, Co-op, Controller…).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Screenshot/preview image URLs — the gallery on the details page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<String>,
    /// Episodes for a series; empty for movies and games.
    pub episodes: Vec<Episode>,
}

#[derive(Serialize, Clone)]
pub struct Episode {
    /// Stremio stream id, e.g. "tt0944947:1:1".
    pub id: String,
    pub title: String,
    pub season: i64,
    pub episode: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    /// Directly playable URL (HTTP, debrid, …) → our mpv + upscaler.
    Direct,
    /// YouTube id → mpv via yt-dlp.
    Youtube,
    /// A link to an external service/app (WatchHub) → opened with the system.
    External,
    /// A BitTorrent magnet (Torrentio) → streamed via a torrent helper.
    Torrent,
}

/// One pickable source on the details page.
#[derive(Serialize, Deserialize, Clone)]
pub struct Stream {
    pub kind: StreamKind,
    /// Direct/debrid URL, externalUrl, YouTube watch URL, or magnet URI.
    pub url: String,
    /// Short label — provider + quality, or the service name (WatchHub).
    pub name: String,
    /// Detail line — filename + seeders/size, or empty.
    #[serde(default)]
    pub title: String,
    /// Which file inside a torrent to play.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_idx: Option<i64>,
}
