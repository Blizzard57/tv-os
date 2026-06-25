//! Retro games: ROMs on disk + a downloadable catalog, launched in RetroArch.
//!
//! ROMs live in one directory per system (EmuDeck's layout):
//!
//!   ~/ROMs/nes/Nova the Squirrel.nes      (override root with TVOS_ROM_DIR)
//!
//! Installed ROMs join the same "Games" row as Steam/Epic — retro is not a
//! separate world. Box art comes from the libretro thumbnail CDN, keyed by
//! the file name (No-Intro naming gives the best hit rate).
//!
//! The "Homebrew & Retro" row is a downloadable catalog: a built-in manifest
//! of freely licensed homebrew (data/homebrew.json), plus any extra manifest
//! files listed in TVOS_ROM_SOURCES (comma-separated paths) — the seed of the
//! plan's ROM-source addons. Installing downloads straight into the ROM tree;
//! no manual file transfer.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::install::InstallManager;
use crate::launcher;
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::util::percent_encode;

/// dir name under the ROM root, display name, libretro thumbnail repo name,
/// file extensions, libretro core candidates (best first).
struct System {
    dir: &'static str,
    name: &'static str,
    thumb_repo: &'static str,
    extensions: &'static [&'static str],
    cores: &'static [&'static str],
}

const SYSTEMS: [System; 8] = [
    System {
        dir: "nes",
        name: "NES",
        thumb_repo: "Nintendo - Nintendo Entertainment System",
        extensions: &["nes"],
        cores: &["mesen", "nestopia", "fceumm"],
    },
    System {
        dir: "snes",
        name: "Super Nintendo",
        thumb_repo: "Nintendo - Super Nintendo Entertainment System",
        extensions: &["sfc", "smc"],
        cores: &["snes9x"],
    },
    System {
        dir: "gb",
        name: "Game Boy",
        thumb_repo: "Nintendo - Game Boy",
        extensions: &["gb"],
        cores: &["gambatte", "sameboy"],
    },
    System {
        dir: "gbc",
        name: "Game Boy Color",
        thumb_repo: "Nintendo - Game Boy Color",
        extensions: &["gbc"],
        cores: &["gambatte", "sameboy"],
    },
    System {
        dir: "gba",
        name: "Game Boy Advance",
        thumb_repo: "Nintendo - Game Boy Advance",
        extensions: &["gba"],
        cores: &["mgba"],
    },
    System {
        dir: "genesis",
        name: "Sega Genesis",
        thumb_repo: "Sega - Mega Drive - Genesis",
        extensions: &["md", "gen", "68k"],
        cores: &["genesis_plus_gx", "picodrive"],
    },
    System {
        dir: "n64",
        name: "Nintendo 64",
        thumb_repo: "Nintendo - Nintendo 64",
        extensions: &["n64", "z64", "v64"],
        cores: &["mupen64plus_next", "parallel_n64"],
    },
    System {
        dir: "psx",
        name: "PlayStation",
        thumb_repo: "Sony - PlayStation",
        extensions: &["chd", "cue", "pbp"],
        cores: &["swanstation", "mednafen_psx_hw", "mednafen_psx"],
    },
];

struct CatalogEntry {
    title: String,
    system: String,
    filename: String,
    url: String,
    art: Option<String>,
}

pub struct Retro {
    rom_dir: PathBuf,
    catalog: Vec<CatalogEntry>,
}

impl Retro {
    pub fn new() -> Self {
        let rom_dir = std::env::var("TVOS_ROM_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("ROMs")
            });

        let mut catalog = parse_manifest(include_str!("../../data/homebrew.json"));
        if let Ok(extra) = std::env::var("TVOS_ROM_SOURCES") {
            for path in extra.split(',').filter(|p| !p.is_empty()) {
                match std::fs::read_to_string(path) {
                    Ok(text) => catalog.extend(parse_manifest(&text)),
                    Err(e) => eprintln!("rom source {path}: {e}"),
                }
            }
        }
        Self { rom_dir, catalog }
    }

    fn rom_path(&self, system_dir: &str, filename: &str) -> PathBuf {
        self.rom_dir.join(system_dir).join(filename)
    }

    fn installed(&self) -> Vec<ContentItem> {
        let mut items: Vec<ContentItem> = SYSTEMS
            .iter()
            .flat_map(|system| {
                let Ok(entries) = self.rom_dir.join(system.dir).read_dir() else {
                    return Vec::new();
                };
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|x| system.extensions.contains(&x.to_lowercase().as_str()))
                    })
                    .filter_map(|p| {
                        let filename = p.file_name()?.to_string_lossy().into_owned();
                        let stem = p.file_stem()?.to_string_lossy().into_owned();
                        // Catalog art (exact match) beats the thumbnail guess.
                        let art = self
                            .catalog
                            .iter()
                            .find(|c| c.system == system.dir && c.filename == filename)
                            .and_then(|c| c.art.clone())
                            .or_else(|| Some(thumbnail_url(system.thumb_repo, &stem)));
                        Some(ContentItem {
                            id: format!("rom:{}/{filename}", system.dir),
                            kind: Kind::Game,
                            title: stem,
                            art,
                            action: Action::Play,
                        })
                    })
                    .collect()
            })
            .collect();
        items.sort_by_key(|item| item.title.to_lowercase());
        items
    }

    fn downloadable(&self) -> Vec<ContentItem> {
        self.catalog
            .iter()
            .filter(|c| !self.rom_path(&c.system, &c.filename).exists())
            .map(|c| ContentItem {
                id: format!("rom:{}/{}", c.system, c.filename),
                kind: Kind::Game,
                title: c.title.clone(),
                art: c.art.clone(),
                action: Action::Install,
            })
            .collect()
    }
}

impl Source for Retro {
    fn id(&self) -> &'static str {
        "rom"
    }

    /// Always on: the catalog row should appear even before RetroArch is
    /// installed — launching explains what's missing.
    fn available(&self) -> bool {
        true
    }

    fn rows(&self) -> Vec<Row> {
        vec![
            Row {
                title: "Games".to_string(),
                items: self.installed(),
            },
            Row {
                title: "Homebrew & Retro".to_string(),
                items: self.downloadable(),
            },
        ]
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let (system_dir, filename) = parse_id(item_id)?;
        let system = system_for_dir(system_dir)?;
        let rom = self.rom_path(system_dir, filename);
        if !rom.exists() {
            return Err(format!("{filename} is not installed"));
        }
        let retroarch = find_retroarch().ok_or(
            "RetroArch not found — install it: flatpak install flathub org.libretro.RetroArch",
        )?;
        let core = find_core(&retroarch, system).ok_or_else(|| {
            format!(
                "No {} core installed — in RetroArch: Online Updater → Core Downloader → {}",
                system.name, system.cores[0]
            )
        })?;
        retroarch
            .run(&["-f", "-L", &core.to_string_lossy(), &rom.to_string_lossy()])
            .map_err(|e| format!("could not start RetroArch: {e}"))
    }

    fn install(&self, item_id: &str, jobs: &InstallManager) -> Result<(), String> {
        let (system_dir, filename) = parse_id(item_id)?;
        let entry = self
            .catalog
            .iter()
            .find(|c| c.system == system_dir && c.filename == filename)
            .ok_or_else(|| format!("{filename} is not in any ROM catalog"))?;
        jobs.start_download(
            item_id,
            &entry.title,
            entry.url.clone(),
            self.rom_path(system_dir, filename),
        )
    }
}

/// "rom:gb/Libbet and the Magic Floor.gb" → ("gb", "Libbet and the Magic Floor.gb")
fn parse_id(item_id: &str) -> Result<(&str, &str), String> {
    item_id
        .strip_prefix("rom:")
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(|| format!("bad rom id '{item_id}'"))
}

fn system_for_dir(dir: &str) -> Result<&'static System, String> {
    SYSTEMS
        .iter()
        .find(|s| s.dir == dir)
        .ok_or_else(|| format!("unknown system '{dir}'"))
}

/// How RetroArch is installed; cores live in different places for each.
enum RetroArch {
    Native,
    Flatpak,
}

impl RetroArch {
    fn core_dirs(&self) -> Vec<PathBuf> {
        let home = std::env::var("HOME").unwrap_or_default();
        match self {
            RetroArch::Native => vec![
                Path::new(&home).join(".config/retroarch/cores"),
                PathBuf::from("/usr/lib/libretro"),
                PathBuf::from("/usr/lib64/libretro"),
            ],
            RetroArch::Flatpak => {
                vec![Path::new(&home).join(".var/app/org.libretro.RetroArch/config/retroarch/cores")]
            }
        }
    }

    fn run(&self, args: &[&str]) -> std::io::Result<()> {
        match self {
            RetroArch::Native => launcher::spawn_detached("retroarch", args),
            RetroArch::Flatpak => {
                let mut all = vec!["run", "org.libretro.RetroArch"];
                all.extend(args);
                launcher::spawn_detached("flatpak", &all)
            }
        }
    }
}

fn find_retroarch() -> Option<RetroArch> {
    let native = Command::new("retroarch")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if native {
        return Some(RetroArch::Native);
    }
    let flatpak = Command::new("flatpak")
        .args(["info", "org.libretro.RetroArch"])
        .output()
        .is_ok_and(|o| o.status.success());
    flatpak.then_some(RetroArch::Flatpak)
}

/// First of the system's preferred cores that is actually installed.
fn find_core(retroarch: &RetroArch, system: &System) -> Option<PathBuf> {
    for dir in retroarch.core_dirs() {
        for core in system.cores {
            let path = dir.join(format!("{core}_libretro.so"));
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

/// Box art from the libretro thumbnail CDN. Thumbnails are named after the
/// game (No-Intro name); libretro replaces characters illegal in filenames
/// with `_`, and the path must be percent-encoded.
fn thumbnail_url(thumb_repo: &str, game_name: &str) -> String {
    let safe: String = game_name
        .chars()
        .map(|c| {
            if matches!(
                c,
                '&' | '*' | '/' | ':' | '`' | '<' | '>' | '?' | '\\' | '|'
            ) {
                '_'
            } else {
                c
            }
        })
        .collect();
    format!(
        "https://thumbnails.libretro.com/{}/Named_Boxarts/{}.png",
        percent_encode(thumb_repo),
        percent_encode(&safe)
    )
}

fn parse_manifest(json: &str) -> Vec<CatalogEntry> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(items) = value.get("items").and_then(|i| i.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|e| {
            let entry = CatalogEntry {
                title: e.get("title")?.as_str()?.to_string(),
                system: e.get("system")?.as_str()?.to_string(),
                filename: e.get("filename")?.as_str()?.to_string(),
                url: e.get("url")?.as_str()?.to_string(),
                art: e.get("art").and_then(|a| a.as_str()).map(String::from),
            };
            // Only accept systems we can actually launch.
            system_for_dir(&entry.system).ok()?;
            Some(entry)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrip() {
        assert_eq!(
            parse_id("rom:gb/Libbet and the Magic Floor.gb"),
            Ok(("gb", "Libbet and the Magic Floor.gb"))
        );
        assert!(parse_id("rom:no-slash").is_err());
        assert!(parse_id("steam:620").is_err());
    }

    #[test]
    fn thumbnail_urls_are_encoded_like_libretro() {
        assert_eq!(
            thumbnail_url("Nintendo - Game Boy", "Mario & Wario (Japan)"),
            "https://thumbnails.libretro.com/Nintendo%20-%20Game%20Boy/Named_Boxarts/Mario%20_%20Wario%20(Japan).png"
        );
    }

    #[test]
    fn builtin_manifest_parses_fully() {
        let catalog = parse_manifest(include_str!("../../data/homebrew.json"));
        assert_eq!(catalog.len(), 4);
        assert!(catalog.iter().all(|c| c.url.starts_with("https://")));
    }

    #[test]
    fn manifest_rejects_unknown_systems_and_garbage() {
        let json = r#"{"items":[
            {"title":"Ok","system":"gb","filename":"ok.gb","url":"https://x/ok.gb"},
            {"title":"Bad","system":"atari9000","filename":"bad.bin","url":"https://x/bad.bin"},
            {"title":"Missing fields","system":"gb"}
        ]}"#;
        let catalog = parse_manifest(json);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].title, "Ok");
        assert!(parse_manifest("not json").is_empty());
    }
}
