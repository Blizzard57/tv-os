//! The single content model: every item in the UI — game, movie, episode,
//! ROM — is a `ContentItem`, no matter which source produced it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, Serializer};

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

#[derive(Deserialize, Clone)]
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

/// Extra presentation data is deliberately derived at the wire boundary. This
/// keeps every existing source backwards compatible while giving the shell a
/// typed contract instead of making it reverse-engineer id prefixes.
impl Serialize for ContentItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("kind", &self.kind)?;
        map.serialize_entry("title", &self.title)?;
        if let Some(art) = &self.art {
            map.serialize_entry("art", art)?;
            map.serialize_entry("images", &serde_json::json!({ "landscape": art }))?;
        }
        map.serialize_entry("action", &self.action)?;
        if let Some(note) = &self.note {
            map.serialize_entry("note", note)?;
        }
        if let Some(progress) = crate::profile::STORE.progress(&self.id) {
            map.serialize_entry("progress", &progress.percentage)?;
            map.serialize_entry("playback", &progress)?;
        }
        let source = self.id.split(':').next().unwrap_or("local");
        map.serialize_entry("source", source)?;
        map.serialize_entry("domain", &domain_for(self))?;
        map.serialize_entry(
            "availability",
            match self.action {
                Action::Play => "available",
                Action::Install => "installable",
                Action::None => "upcoming",
            },
        )?;
        let external_ids = external_ids(&self.id);
        if !external_ids.is_empty() {
            map.serialize_entry("external_ids", &external_ids)?;
        }
        if self.id.starts_with("yt:") {
            map.serialize_entry("creator_type", "video")?;
        } else if let Some(rest) = self.id.strip_prefix("twitch:") {
            let kind = rest.split(':').next().unwrap_or("live");
            map.serialize_entry(
                "creator_type",
                match kind {
                    "vod" => "vod",
                    "channel" => "channel",
                    "category" => "category",
                    _ => "live_stream",
                },
            )?;
        }
        map.end()
    }
}

fn domain_for(item: &ContentItem) -> &'static str {
    match item.kind {
        Kind::Game => "games",
        Kind::Live => "sports",
        Kind::Movie => "movies",
        Kind::Series => "shows",
        Kind::Video if item.id.starts_with("twitch:") => "twitch",
        Kind::Video if item.id.starts_with("yt:") => "youtube",
        Kind::Video => "video",
    }
}

fn external_ids(id: &str) -> BTreeMap<&'static str, String> {
    let mut ids = BTreeMap::new();
    let parts: Vec<&str> = id.split(':').collect();
    match parts.as_slice() {
        ["tmdb", media, value, ..] => {
            ids.insert("tmdb", (*value).to_string());
            ids.insert("media_type", (*media).to_string());
        }
        ["strm", _, value, ..] if value.starts_with("tt") => {
            ids.insert("imdb", (*value).to_string());
        }
        ["yt", value, ..] => {
            ids.insert("youtube", (*value).to_string());
        }
        ["twitch", kind, value, ..] => {
            ids.insert("twitch", (*value).to_string());
            ids.insert("twitch_type", (*kind).to_string());
        }
        _ => {}
    }
    ids
}

/// One horizontal row on the home screen.
#[derive(Clone)]
pub struct Row {
    pub title: String,
    pub items: Vec<ContentItem>,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum RowPurpose {
    ContinueWatching,
    TopPicks,
    BecauseYouWatched,
    IndianSpotlight,
    LiveNow,
    StartingSoon,
    Schedule,
    Creators,
    Games,
    Library,
    Discovery,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CardLayout {
    Landscape,
    Portrait,
    Progress,
    #[allow(dead_code)]
    Circle,
    LiveEvent,
    Game,
}

impl Row {
    pub fn purpose(&self) -> RowPurpose {
        let t = self.title.to_ascii_lowercase();
        if t.starts_with("continue") {
            RowPurpose::ContinueWatching
        } else if t.starts_with("top picks") {
            RowPurpose::TopPicks
        } else if t.starts_with("because you") {
            RowPurpose::BecauseYouWatched
        } else if t.contains("indian spotlight") {
            RowPurpose::IndianSpotlight
        } else if t == "live now" {
            RowPurpose::LiveNow
        } else if t == "starting soon" {
            RowPurpose::StartingSoon
        } else if t.starts_with("today") || t.starts_with("tomorrow") {
            RowPurpose::Schedule
        } else if self
            .items
            .iter()
            .any(|i| matches!(domain_for(i), "youtube" | "twitch"))
        {
            RowPurpose::Creators
        } else if self.items.iter().all(|i| i.kind == Kind::Game) {
            RowPurpose::Games
        } else if t.starts_with("my ") || t.contains("watchlist") {
            RowPurpose::Library
        } else {
            RowPurpose::Discovery
        }
    }

    pub fn layout(&self) -> CardLayout {
        match self.purpose() {
            RowPurpose::ContinueWatching => CardLayout::Progress,
            RowPurpose::LiveNow | RowPurpose::StartingSoon | RowPurpose::Schedule => {
                CardLayout::LiveEvent
            }
            // Creator *videos* and live streams use their native 16:9 art.
            // Circular cards are reserved for an explicit channel directory.
            RowPurpose::Creators => CardLayout::Landscape,
            RowPurpose::Games => CardLayout::Game,
            RowPurpose::IndianSpotlight => CardLayout::Portrait,
            _ => CardLayout::Landscape,
        }
    }

    fn stable_id(&self) -> String {
        let slug: String = self
            .title
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        slug.split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl Serialize for Row {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let purpose = self.purpose();
        let mut row = serializer.serialize_struct("Row", 7)?;
        row.serialize_field("id", &self.stable_id())?;
        row.serialize_field(
            "destination",
            match purpose {
                RowPurpose::LiveNow | RowPurpose::StartingSoon | RowPurpose::Schedule => "live",
                RowPurpose::Creators => "creators",
                RowPurpose::Games => "games",
                RowPurpose::Library => "library",
                _ => "home",
            },
        )?;
        row.serialize_field("title", &self.title)?;
        row.serialize_field("purpose", &purpose)?;
        row.serialize_field("layout", &self.layout())?;
        let explanation = match purpose {
            RowPurpose::BecauseYouWatched => Some("Inspired by your viewing history"),
            RowPurpose::TopPicks => Some("Ranked privately on this device"),
            RowPurpose::IndianSpotlight => Some("Indian cinema and series selected for you"),
            _ => None,
        };
        row.serialize_field("explanation", &explanation)?;
        row.serialize_field("items", &self.items)?;
        row.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_streams_use_landscape_semantics() {
        let row = Row {
            title: "Twitch · Followed live".into(),
            items: vec![ContentItem {
                id: "twitch:live:creator".into(),
                kind: Kind::Video,
                title: "Live stream".into(),
                art: Some("thumb".into()),
                action: Action::Play,
                note: None,
            }],
        };
        let value = serde_json::to_value(row).unwrap();
        assert_eq!(value["destination"], "creators");
        assert_eq!(value["layout"], "landscape");
        assert_eq!(value["items"][0]["creator_type"], "live_stream");
    }
}
