//! Local video source: anything in ~/Videos. Streaming catalogs (TMDB,
//! Stremio addons) provide everything else.

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
        vec![Row {
            title: "My Videos".to_string(),
            items: local_videos(),
        }]
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let target = item_id.strip_prefix("video:").unwrap_or_default();
        let mode = settings::STORE.get().enhance;
        let profile = upscale::resolve(mode, target);
        launcher::play_video(target, &profile, mode, target, Some(item_id), Some(item_id))
    }
}

const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "mkv", "webm", "avi", "mov", "m4v"];

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
