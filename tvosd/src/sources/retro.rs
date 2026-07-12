//! Retro games: ROMs on disk + a downloadable catalog, launched in RetroArch.
//!
//! ROMs live in one directory per system (EmuDeck's layout):
//!
//!   ~/ROMs/nes/Nova the Squirrel.nes      (override root with TVOS_ROM_DIR)
//!
//! Installed ROMs join the same "Ready to Play" row as Steam/Epic — retro is not a
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

/// dir name under the ROM root, display name, libretro thumbnail repo name,
/// file extensions, libretro core candidates (best first).
struct System {
    dir: &'static str,
    name: &'static str,
    thumb_repo: &'static str,
    extensions: &'static [&'static str],
    cores: &'static [&'static str],
}

const SYSTEMS: [System; 17] = [
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
        dir: "nds",
        name: "Nintendo DS",
        thumb_repo: "Nintendo - Nintendo DS",
        extensions: &["nds"],
        cores: &["melonds", "desmume"],
    },
    System {
        dir: "genesis",
        name: "Sega Genesis",
        thumb_repo: "Sega - Mega Drive - Genesis",
        extensions: &["md", "gen", "68k", "smd", "bin"],
        cores: &["genesis_plus_gx", "picodrive"],
    },
    System {
        dir: "sms",
        name: "Sega Master System",
        thumb_repo: "Sega - Master System - Mark III",
        extensions: &["sms"],
        cores: &["genesis_plus_gx", "picodrive"],
    },
    System {
        dir: "gg",
        name: "Sega Game Gear",
        thumb_repo: "Sega - Game Gear",
        extensions: &["gg"],
        cores: &["genesis_plus_gx"],
    },
    System {
        dir: "saturn",
        name: "Sega Saturn",
        thumb_repo: "Sega - Saturn",
        extensions: &["chd", "cue", "m3u"],
        cores: &["mednafen_saturn", "kronos", "yabause"],
    },
    System {
        dir: "dreamcast",
        name: "Sega Dreamcast",
        thumb_repo: "Sega - Dreamcast",
        extensions: &["chd", "cdi", "gdi", "m3u"],
        cores: &["flycast"],
    },
    System {
        dir: "pcengine",
        name: "PC Engine",
        thumb_repo: "NEC - PC Engine - TurboGrafx 16",
        extensions: &["pce", "chd", "cue", "m3u"],
        cores: &["mednafen_pce", "mednafen_pce_fast"],
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
        extensions: &["chd", "cue", "pbp", "m3u"],
        cores: &["swanstation", "mednafen_psx_hw", "mednafen_psx"],
    },
    System {
        dir: "psp",
        name: "PlayStation Portable",
        thumb_repo: "Sony - PlayStation Portable",
        extensions: &["iso", "cso", "chd", "pbp"],
        cores: &["ppsspp"],
    },
    System {
        dir: "arcade",
        name: "Arcade",
        thumb_repo: "MAME",
        extensions: &["zip", "chd"],
        cores: &["mame", "fbneo", "mame2003_plus"],
    },
    System {
        dir: "dos",
        name: "MS-DOS",
        thumb_repo: "DOS",
        extensions: &["zip", "exe", "bat", "conf"],
        cores: &["dosbox_pure", "dosbox_core"],
    },
];

struct CatalogEntry {
    title: String,
    system: String,
    filename: String,
    url: String,
    art: Option<String>,
    /// Optional lowercase-hex sha256; when present the download is verified
    /// against it and the install fails on mismatch (No-Intro/Redump style).
    sha256: Option<String>,
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
                    Err(e) => crate::log_warn!("rom source {path}: {e}"),
                }
            }
        }
        Self { rom_dir, catalog }
    }

    fn rom_path(&self, system_dir: &str, filename: &str) -> PathBuf {
        self.rom_dir.join(system_dir).join(filename)
    }

    /// Builds the ROM path for launch and confirms it stays inside the
    /// system's directory — guards against a crafted id (`..`, absolute path,
    /// path separators) escaping the ROM tree.
    fn safe_rom_path(&self, system_dir: &str, filename: &str) -> Result<PathBuf, String> {
        let system_root = self.rom_dir.join(system_dir);
        let rom = system_root.join(filename);
        // Compare against the canonicalized system dir so symlinks and `..`
        // can't point the ROM outside it.
        let root = system_root
            .canonicalize()
            .map_err(|_| format!("no ROMs for '{system_dir}'"))?;
        let real = rom
            .canonicalize()
            .map_err(|_| format!("{filename} is not installed"))?;
        if !real.starts_with(&root) {
            return Err(format!("bad rom id '{filename}'"));
        }
        Ok(real)
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
                            note: None,
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
                note: None,
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
                title: "Ready to Play".to_string(),
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
        // Canonicalizes and verifies the ROM stays under its system dir.
        let rom = self.safe_rom_path(system_dir, filename)?;
        let retroarch = find_retroarch().ok_or(
            "RetroArch not found — install it: flatpak install flathub org.libretro.RetroArch",
        )?;
        // Prefer one of the system's known cores; if none is installed, let
        // RetroArch pick its own core association rather than hard-erroring.
        match find_core(&retroarch, system) {
            Some(core) => retroarch
                .run(&["-f", "-L", &core.to_string_lossy(), &rom.to_string_lossy()])
                .map_err(|e| format!("could not start RetroArch: {e}")),
            None => {
                crate::log_warn!(
                    "no known {} core installed — letting RetroArch pick its default \
                     (install one of: {})",
                    system.name,
                    system.cores.join(", ")
                );
                retroarch
                    .run(&["-f", &rom.to_string_lossy()])
                    .map_err(|e| format!("could not start RetroArch: {e}"))
            }
        }
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
            entry.sha256.clone(),
        )
    }
}

/// "rom:gb/Libbet and the Magic Floor.gb" → ("gb", "Libbet and the Magic Floor.gb")
///
/// The filename is a single path component: reject anything containing a path
/// separator, `..`, or that looks absolute, so a crafted id can't reference a
/// file outside its system directory.
fn parse_id(item_id: &str) -> Result<(&str, &str), String> {
    let (system_dir, filename) = item_id
        .strip_prefix("rom:")
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(|| format!("bad rom id '{item_id}'"))?;
    let unsafe_component = |s: &str| {
        s.is_empty()
            || s == ".."
            || s == "."
            || s.contains('/')
            || s.contains('\\')
            || s.contains('\0')
    };
    if unsafe_component(system_dir) || unsafe_component(filename) {
        return Err(format!("bad rom id '{item_id}'"));
    }
    Ok((system_dir, filename))
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
                Path::new(&home).join(".local/share/libretro/cores"),
                Path::new(&home).join(".local/lib/libretro"),
                PathBuf::from("/usr/lib/libretro"),
                PathBuf::from("/usr/lib64/libretro"),
                PathBuf::from("/usr/local/lib/libretro"),
                PathBuf::from("/app/lib/libretro"), // some flatpak runtimes
            ],
            RetroArch::Flatpak => vec![
                Path::new(&home).join(".var/app/org.libretro.RetroArch/config/retroarch/cores"),
                Path::new(&home).join(".var/app/org.libretro.RetroArch/.config/retroarch/cores"),
            ],
        }
    }

    fn run(&self, args: &[&str]) -> std::io::Result<()> {
        // Route the emulator through the gamescope wrapper so it inherits the
        // per-item TV profile (resolution/FSR/frame cap) when in the TV session;
        // no-ops to a direct spawn in windowed/app mode.
        match self {
            RetroArch::Native => launcher::spawn_detached_game("retroarch", args, &[]),
            RetroArch::Flatpak => {
                let mut all = vec!["run", "org.libretro.RetroArch"];
                all.extend(args);
                launcher::spawn_detached_game("flatpak", &all, &[])
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
        encode_libretro(thumb_repo),
        encode_libretro(&safe)
    )
}

/// Percent-encode a libretro thumbnail path segment. Matches the CDN's own
/// convention: unreserved chars plus parentheses are left literal (No-Intro
/// names like `(Japan)` appear verbatim in the URL); everything else — spaces
/// especially — is percent-encoded. This intentionally differs from the strict
/// RFC-3986 [`crate::util::percent_encode`], which would escape `(`/`)`.
fn encode_libretro(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'(' | b')' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
                sha256: e
                    .get("sha256")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_ascii_lowercase()),
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
    fn parse_id_rejects_path_traversal() {
        // A crafted filename or system dir must never carry path separators or
        // `..`, so it can't escape the ROM tree.
        assert!(parse_id("rom:gb/../../../etc/passwd").is_err());
        assert!(parse_id("rom:gb/sub/rom.gb").is_err());
        assert!(parse_id("rom:../gb/rom.gb").is_err());
        assert!(parse_id("rom:gb/..").is_err());
        assert!(parse_id("rom:gb/").is_err());
        assert!(parse_id(r"rom:gb/rom\..\..\x.gb").is_err());
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
