//! The source registry — the seed of the addon protocol from PLAN.md.
//!
//! A `Source` is anything that contributes content: a game store, a video
//! folder, a catalog. Each source owns an id prefix ("steam:", "epic:", …)
//! and decides what its items do. Phase 2 sources are compiled in; later
//! phases move this behind the HTTP addon protocol so out-of-tree sources
//! can be installed at runtime.

pub mod epic;
pub mod gamehub;
pub mod gamerec;
pub mod gog;
pub mod hltb;
pub mod retro;
pub mod steam;
pub mod stremio;
pub mod tmdb;
pub mod videos;
pub mod youtube;

use serde::Serialize;

use crate::install::InstallManager;
use crate::model::Row;

/// Maps a streamable item id to `(stremio_kind, stream_id)` for the details
/// endpoints. Handles `strm:` directly and `tmdb:` via a TMDB→IMDb lookup.
pub fn resolve_video(item_id: &str) -> Result<(String, String), String> {
    match item_id.split(':').next().unwrap_or_default() {
        "strm" => {
            let mut parts = item_id.splitn(3, ':');
            parts.next();
            let kind = parts.next().filter(|s| !s.is_empty());
            let id = parts.next().filter(|s| !s.is_empty());
            match (kind, id) {
                (Some(k), Some(i)) => Ok((k.to_string(), i.to_string())),
                _ => Err(format!("bad stream id '{item_id}'")),
            }
        }
        "tmdb" => tmdb::resolve_imdb(item_id),
        _ => Err(format!("'{item_id}' is not a streamable item")),
    }
}

pub trait Source: Send + Sync {
    /// Stable identifier, also the id prefix this source owns ("epic" → "epic:…").
    fn id(&self) -> &'static str;

    /// Whether the source's backing tool/service was detected on this system.
    fn available(&self) -> bool;

    /// Rows this source contributes to the home screen. Rows with the same
    /// title from different sources are merged (e.g. "Games").
    fn rows(&self) -> Vec<Row>;

    fn launch(&self, item_id: &str) -> Result<(), String>;

    fn install(&self, _item_id: &str, _jobs: &InstallManager) -> Result<(), String> {
        Err("this content is not installable".to_string())
    }
}

pub struct Registry {
    sources: Vec<Box<dyn Source>>,
}

#[derive(Serialize)]
pub struct SourceInfo {
    id: &'static str,
    available: bool,
}

impl Registry {
    /// All known sources, in home-screen row order.
    pub fn detect() -> Self {
        Self {
            sources: vec![
                Box::new(steam::Steam::new()),
                Box::new(epic::Epic::detect()),
                Box::new(gog::Gog),
                Box::new(retro::Retro::new()),
                Box::new(videos::Videos),
                Box::new(stremio::Stremio::default()),
                Box::new(tmdb::Tmdb::default()),
                Box::new(youtube::YouTube::detect()),
                Box::new(gamehub::GameShop),
            ],
        }
    }

    pub fn sources(&self) -> Vec<SourceInfo> {
        self.sources
            .iter()
            .map(|s| SourceInfo {
                id: s.id(),
                available: s.available(),
            })
            .collect()
    }

    /// Home screen: every available source's rows, merged by row title so
    /// e.g. Steam and Epic games share one "Games" row.
    pub fn library(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = Vec::new();
        for source in self.sources.iter().filter(|s| s.available()) {
            for row in source.rows() {
                match rows.iter_mut().find(|r| r.title == row.title) {
                    Some(existing) => existing.items.extend(row.items),
                    None => rows.push(row),
                }
            }
        }
        rows.retain(|row| !row.items.is_empty());
        rows
    }

    pub fn launch(&self, item_id: &str) -> Result<(), String> {
        self.owner_of(item_id)?.launch(item_id)
    }

    pub fn install(&self, item_id: &str, jobs: &InstallManager) -> Result<(), String> {
        self.owner_of(item_id)?.install(item_id, jobs)
    }

    fn owner_of(&self, item_id: &str) -> Result<&dyn Source, String> {
        let prefix = item_id.split(':').next().unwrap_or_default();
        self.sources
            .iter()
            .find(|s| s.id() == prefix)
            .map(|s| s.as_ref())
            .ok_or_else(|| format!("no source for '{item_id}'"))
    }
}
