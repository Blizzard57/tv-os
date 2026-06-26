//! Video source for phase 2: a few known-good sample streams (so playback
//! works on a fresh install with no media) plus anything in ~/Videos.
//! Stremio-style stream addons replace this in phase 5.

use std::path::Path;

use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::{launcher, settings, upscale};

pub struct Videos;

impl Source for Videos {
    fn id(&self) -> &'static str {
        "video"
    }

    fn available(&self) -> bool {
        true
    }

    fn rows(&self) -> Vec<Row> {
        vec![
            Row {
                title: "My Videos".to_string(),
                items: local_videos(),
            },
            Row {
                title: "Sample Streams".to_string(),
                items: sample_streams(),
            },
        ]
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let target = item_id.strip_prefix("video:").unwrap_or_default();
        let mode = settings::STORE.get().enhance;
        let profile = upscale::resolve(mode, target);
        launcher::play_video(target, &profile, mode, target)
    }
}

const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "mkv", "webm", "avi", "mov", "m4v"];

/// Blender open movies from Blender's own CDN, posters from Wikimedia.
/// (title, stream url, poster url)
const SAMPLE_STREAMS: [(&str, &str, &str); 3] = [
    (
        "Big Buck Bunny",
        "https://download.blender.org/peach/bigbuckbunny_movies/big_buck_bunny_1080p_h264.mov",
        "https://commons.wikimedia.org/wiki/Special:FilePath/Big_buck_bunny_poster_big.jpg?width=600",
    ),
    (
        "Sintel",
        "https://download.blender.org/durian/movies/Sintel.2010.720p.mkv",
        "https://commons.wikimedia.org/wiki/Special:FilePath/Sintel_poster.jpg?width=600",
    ),
    (
        "Tears of Steel",
        "https://download.blender.org/demo/movies/ToS/tears_of_steel_720p.mov",
        "https://commons.wikimedia.org/wiki/Special:FilePath/Tos-poster.png?width=600",
    ),
];

fn sample_streams() -> Vec<ContentItem> {
    SAMPLE_STREAMS
        .iter()
        .map(|(title, url, poster)| ContentItem {
            id: format!("video:{url}"),
            kind: Kind::Video,
            title: title.to_string(),
            art: Some(poster.to_string()),
            action: Action::Play,
        })
        .collect()
}

fn local_videos() -> Vec<ContentItem> {
    let Ok(home) = std::env::var("HOME") else {
        return Vec::new();
    };
    let Ok(entries) = Path::new(&home).join("Videos").read_dir() else {
        return Vec::new();
    };
    let mut items: Vec<ContentItem> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        })
        .filter_map(|p| {
            let title = p.file_stem()?.to_string_lossy().into_owned();
            Some(ContentItem {
                id: format!("video:{}", p.display()),
                kind: Kind::Video,
                title,
                art: None,
                action: Action::Play,
            })
        })
        .collect();
    items.sort_by_key(|item| item.title.to_lowercase());
    items
}
