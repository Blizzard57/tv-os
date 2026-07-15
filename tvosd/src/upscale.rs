//! Content-aware upscaling: picks the best mpv enhancement chain for what's
//! about to play. This is the phase-4 "auto-selection engine" from PLAN.md;
//! the VapourSynth/TensorRT chain slots in behind the same resolver later.
//!
//! Decision inputs:
//!   - the user's Enhance mode  (Auto / Quality / Performance / Off)
//!   - GPU tier                 (NVIDIA / AMD / none — Auto degrades gracefully)
//!   - content class            (anime vs live-action, from name heuristics)
//!   - source resolution: ffprobe for local files, else the file name —
//!     4K sources skip the upscaling chain
//!
//! Shaders live in the TV OS profile dir on macOS and
//! ~/.local/share/tvos/shaders elsewhere (TVOS_SHADER_DIR to override);
//! system/get-shaders.sh downloads them. Chains only reference shaders that
//! are actually on disk, so a missing download degrades to mpv's own
//! high-quality scalers instead of failing playback.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::settings::EnhanceMode;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VisualClass {
    Anime,
    Cartoon,
    LiveAction,
    Sports,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Gpu {
    Nvidia,
    Amd,
    None,
}

/// What play_video applies. `name` is logged and shown in mpv's OSD title.
pub struct Profile {
    pub name: String,
    pub args: Vec<String>,
}

/// One switchable upscaler option for the player's realtime menu.
pub struct Preset {
    pub name: String,
    pub hint: String,
    /// ':'-joined absolute shader paths; empty string means "off".
    pub shaders: String,
}

/// The named upscaler presets, in menu order: (label, hint, anime?, mode).
const PRESET_DEFS: [(&str, &str, bool, EnhanceMode); 4] = [
    (
        "Anime — Quality",
        "Anime4K Mode A (HQ)",
        true,
        EnhanceMode::Quality,
    ),
    (
        "Anime — Fast",
        "Anime4K (light)",
        true,
        EnhanceMode::Performance,
    ),
    (
        "Live Action — Quality",
        "FSRCNNX x2 16",
        false,
        EnhanceMode::Quality,
    ),
    (
        "Live Action — Fast",
        "FSRCNNX x2 8",
        false,
        EnhanceMode::Performance,
    ),
];

/// Every upscaler the in-player menu can switch to live: "Off" first, then each
/// preset whose shaders are actually present on disk (so the menu never offers
/// a chain that would fail).
pub fn presets() -> Vec<Preset> {
    let dir = shader_dir();
    let mut out = vec![Preset {
        name: "Off".to_string(),
        hint: "Original — mpv scalers only".to_string(),
        shaders: String::new(),
    }];
    for (name, hint, anime, mode) in PRESET_DEFS {
        let chain = shader_chain(&dir, mode, anime);
        if !chain.is_empty() {
            out.push(Preset {
                name: name.to_string(),
                hint: hint.to_string(),
                shaders: join_paths(&chain),
            });
        }
    }
    out
}

/// The preset name the auto-resolver picks for `target` — what the player
/// starts on and marks active in the menu.
pub fn active_preset(mode: EnhanceMode, target: &str) -> String {
    active_preset_for(mode, target, VisualClass::Unknown)
}

pub fn active_preset_for(mode: EnhanceMode, target: &str, class: VisualClass) -> String {
    let mode = effective(mode, gpu());
    if mode == EnhanceMode::Off || source_height(target).is_some_and(|h| h >= 2160) {
        return "Off".to_string();
    }
    let anime = matches!(class, VisualClass::Anime | VisualClass::Cartoon)
        || (class == VisualClass::Unknown && looks_like_anime(target));
    let mode = if class == VisualClass::Sports {
        EnhanceMode::Performance
    } else {
        mode
    };
    PRESET_DEFS
        .iter()
        .find(|(_, _, a, m)| *a == anime && *m == mode)
        .map(|(name, ..)| name.to_string())
        .unwrap_or_else(|| "Off".to_string())
}

/// Joins shader paths the way mpv's glsl-shaders list expects.
fn join_paths(chain: &[PathBuf]) -> String {
    chain
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

pub fn resolve(mode: EnhanceMode, target: &str) -> Profile {
    resolve_for(mode, target, VisualClass::Unknown)
}

pub fn resolve_for(mode: EnhanceMode, target: &str, class: VisualClass) -> Profile {
    let mode = effective(mode, gpu());
    if mode == EnhanceMode::Off {
        return Profile {
            name: "off".to_string(),
            args: Vec::new(),
        };
    }

    // Already-4K sources need no upscaling chain, just good scalers.
    if source_height(target).is_some_and(|h| h >= 2160) {
        return Profile {
            name: "source-4k".to_string(),
            args: scaler_args(),
        };
    }

    let anime = matches!(class, VisualClass::Anime | VisualClass::Cartoon)
        || (class == VisualClass::Unknown && looks_like_anime(target));
    let mode = if class == VisualClass::Sports {
        EnhanceMode::Performance
    } else {
        mode
    };
    let chain = shader_chain(&shader_dir(), mode, anime);
    let name = format!(
        "{}-{}",
        if anime { "anime" } else { "live" },
        if mode == EnhanceMode::Quality {
            "quality"
        } else {
            "performance"
        }
    );

    let mut args = scaler_args();
    if !chain.is_empty() {
        args.push(format!("--glsl-shaders={}", join_paths(&chain)));
    }
    Profile { name, args }
}

pub fn classify(title: &str, genres: &[String], live_sports: bool) -> VisualClass {
    if live_sports {
        return VisualClass::Sports;
    }
    let text = format!("{} {}", title, genres.join(" ")).to_ascii_lowercase();
    if text.contains("anime") || text.contains("animation japanese") || text.contains("anilist") {
        VisualClass::Anime
    } else if text.contains("animation") || text.contains("cartoon") || text.contains("animated") {
        VisualClass::Cartoon
    } else if !title.trim().is_empty() {
        VisualClass::LiveAction
    } else {
        VisualClass::Unknown
    }
}

pub fn capability_status() -> serde_json::Value {
    let vfx = std::env::var("TVOS_NVVFX_LIBRARY")
        .ok()
        .is_some_and(|p| Path::new(&p).exists());
    let vapoursynth = command_exists("vspipe");
    let tensorrt = command_exists("trtexec");
    let shaders = presets().len().saturating_sub(1);
    let backend = if gpu() == Gpu::Nvidia && vfx {
        "nvidia_vfx_vsr"
    } else if gpu() == Gpu::Nvidia && vapoursynth && tensorrt {
        "vs_mlrt_tensorrt"
    } else if shaders > 0 {
        "glsl"
    } else {
        "builtin"
    };
    serde_json::json!({
        "gpu": format!("{:?}", gpu()).to_ascii_lowercase(), "backend": backend,
        "nvidia_vfx": vfx, "vapoursynth": vapoursynth, "tensorrt": tensorrt,
        "shader_presets": shaders,
        "fallback_reason": if backend == "glsl" { "Optional NVIDIA AI runtime not installed; using realtime portable shaders" } else { "" },
    })
}

fn command_exists(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

/// Env defaults for launching modern games: engine-level upscalers beat any
/// video upscaler, so opt into them per GPU (FSR4 redirect needs a Proton
/// build that supports PROTON_FSR4_UPGRADE, e.g. Proton-CachyOS).
pub fn game_env() -> Vec<(&'static str, &'static str)> {
    match gpu() {
        // PROTON_FSR4_UPGRADE only helps on FSR4-capable AMD (RDNA4+); on
        // RDNA2/3 it's unsupported and can break launches. Only set it when a
        // capability signal is present or the user explicitly opts in, so older
        // AMD cards are left alone.
        Gpu::Amd if fsr4_supported() => vec![("PROTON_FSR4_UPGRADE", "1")],
        Gpu::Amd => Vec::new(),
        Gpu::Nvidia => vec![("PROTON_ENABLE_NVAPI", "1")],
        Gpu::None => Vec::new(),
    }
}

/// Whether to enable the Proton FSR4 upgrade. Off by default (safe for
/// RDNA2/3); enabled by the explicit `TVOS_FSR4=1` opt-in — the capability
/// signal the launcher/setup can set once it knows the card is FSR4-capable.
fn fsr4_supported() -> bool {
    matches!(std::env::var("TVOS_FSR4").as_deref(), Ok("1"))
}

/// Auto picks quality when there's a real GPU to drive it.
fn effective(mode: EnhanceMode, gpu: Gpu) -> EnhanceMode {
    match mode {
        EnhanceMode::Auto if gpu == Gpu::None => EnhanceMode::Performance,
        EnhanceMode::Auto => EnhanceMode::Quality,
        other => other,
    }
}

/// mpv's best built-in scalers — the floor every enhanced profile stands on.
fn scaler_args() -> Vec<String> {
    vec![
        "--profile=high-quality".to_string(),
        "--scale=ewa_lanczossharp".to_string(),
        "--cscale=ewa_lanczossharp".to_string(),
    ]
}

/// The shader files for a chain, keeping only those present on disk.
fn shader_chain(dir: &Path, mode: EnhanceMode, anime: bool) -> Vec<PathBuf> {
    let names: &[&str] = match (anime, mode) {
        // Anime4K "Mode A (HQ)" — restore then 2x CNN upscale, twice.
        (true, EnhanceMode::Quality) => &[
            "Anime4K_Clamp_Highlights.glsl",
            "Anime4K_Restore_CNN_VL.glsl",
            "Anime4K_Upscale_CNN_x2_VL.glsl",
            "Anime4K_AutoDownscalePre_x2.glsl",
            "Anime4K_AutoDownscalePre_x4.glsl",
            "Anime4K_Upscale_CNN_x2_M.glsl",
        ],
        (true, _) => &[
            "Anime4K_Clamp_Highlights.glsl",
            "Anime4K_Restore_CNN_M.glsl",
            "Anime4K_Upscale_CNN_x2_M.glsl",
        ],
        (false, EnhanceMode::Quality) => &["FSRCNNX_x2_16-0-4-1.glsl"],
        (false, _) => &["FSRCNNX_x2_8-0-4-1.glsl"],
    };
    names
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.exists())
        .collect()
}

pub fn shader_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TVOS_SHADER_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "macos") {
        return crate::settings::profile_dir().join("shaders");
    }
    // Honor XDG_DATA_HOME; fall back to an absolute home so shaders never land
    // in a relative path (which would depend on the daemon's cwd) if HOME is
    // unset.
    let data_home = match std::env::var("XDG_DATA_HOME") {
        Ok(v) if v.starts_with('/') => PathBuf::from(v),
        _ => home_dir().join(".local/share"),
    };
    data_home.join("tvos/shaders")
}

/// An absolute home directory, falling back to `/root` rather than a relative
/// path when HOME is unset.
fn home_dir() -> PathBuf {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => PathBuf::from("/root"),
    }
}

/// Anime detection without metadata: fansub-style "[Group] Title - 01" names
/// or an anime folder anywhere in the path. Wrong guesses still get a good
/// general-purpose chain, so this only needs to be roughly right.
fn looks_like_anime(target: &str) -> bool {
    let lower = target.to_lowercase();
    // A path/URL *segment* that is exactly "anime" (a genre folder), not any
    // path that merely contains the substring (e.g. ".../Reanimated/…" or a
    // movie literally called "Anime"): match whole components only, splitting
    // on both separators so Windows-style paths work too.
    if lower
        .split(['/', '\\'])
        .any(|seg| seg == "anime" || seg == "animes")
    {
        return true;
    }
    // Fansub-style release names: "[Group] Title - 01".
    let name = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    name.starts_with('[') && name.contains(']')
}

/// Source height: ffprobe for local files when available, else "2160p" /
/// "1080p" style markers in the name.
fn source_height(target: &str) -> Option<u32> {
    let path = Path::new(target);
    if path.is_file() {
        if let Some(h) = ffprobe_height(path) {
            return Some(h);
        }
    }
    let lower = target.to_lowercase();
    [2160, 1440, 1080, 720, 480]
        .into_iter()
        .find(|h| lower.contains(&format!("{h}p")) || lower.contains(&format!("{h}i")))
}

fn ffprobe_height(path: &Path) -> Option<u32> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .flatten()
}

fn gpu() -> Gpu {
    static GPU: OnceLock<Gpu> = OnceLock::new();
    *GPU.get_or_init(detect_gpu)
}

/// PCI vendor ids in DRM sysfs.
const VENDOR_AMD: &str = "0x1002";
const VENDOR_NVIDIA: &str = "0x10de";
const VENDOR_INTEL: &str = "0x8086";

fn detect_gpu() -> Gpu {
    match std::env::var("TVOS_GPU").as_deref() {
        Ok("nvidia") => return Gpu::Nvidia,
        Ok("amd") => return Gpu::Amd,
        Ok("none") => return Gpu::None,
        _ => {}
    }

    // Prefer the DRM device the compositor is actually driving. WLR/most
    // Wayland compositors honor WLR_DRM_DEVICE / the card bound to the session;
    // classify by *that* card's vendor so a secondary/idle GPU doesn't win.
    if let Some(gpu) = compositor_gpu() {
        return gpu;
    }

    // nvidia-smi -L prints one "GPU 0: …" line per card; a 0 exit with no such
    // line (some stub/driver states) is *not* a usable NVIDIA GPU.
    let nvidia = Command::new("nvidia-smi")
        .arg("-L")
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("GPU "));
    if nvidia {
        return Gpu::Nvidia;
    }

    // Fall back to scanning DRM cards by PCI vendor. NVIDIA first (its shaders
    // want the NVAPI path), then AMD; Intel is deliberately *not* mapped to
    // NVIDIA — it has no usable NVIDIA/AMD engine path here, so it's None.
    for card in 0..8 {
        match card_vendor(&format!("card{card}")).as_deref() {
            Some(VENDOR_NVIDIA) => return Gpu::Nvidia,
            Some(VENDOR_AMD) => return Gpu::Amd,
            _ => {}
        }
    }
    Gpu::None
}

/// The GPU vendor of the DRM node the compositor is bound to, if we can tell.
/// Reads WLR_DRM_DEVICE (or a caller-provided TVOS_DRM_DEVICE) — a path like
/// /dev/dri/renderD128 or /dev/dri/card0 — and resolves it back to a sysfs
/// vendor id.
fn compositor_gpu() -> Option<Gpu> {
    let node = std::env::var("TVOS_DRM_DEVICE")
        .or_else(|_| std::env::var("WLR_DRM_DEVICE"))
        .ok()?;
    let name = Path::new(&node).file_name()?.to_str()?;
    // renderD* nodes and card* nodes both expose the vendor under the same
    // /sys/class/drm/<name>/device/vendor path.
    match card_vendor(name).as_deref() {
        Some(VENDOR_NVIDIA) => Some(Gpu::Nvidia),
        Some(VENDOR_AMD) => Some(Gpu::Amd),
        Some(VENDOR_INTEL) => Some(Gpu::None), // Intel: no NVIDIA/AMD path
        _ => None,
    }
}

/// The PCI vendor id string for a DRM node name (e.g. "card0", "renderD128").
fn card_vendor(name: &str) -> Option<String> {
    let path = format!("/sys/class/drm/{name}/device/vendor");
    std::fs::read_to_string(path)
        .ok()
        .map(|v| v.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_degrades_without_a_gpu() {
        assert_eq!(
            effective(EnhanceMode::Auto, Gpu::Nvidia),
            EnhanceMode::Quality
        );
        assert_eq!(effective(EnhanceMode::Auto, Gpu::Amd), EnhanceMode::Quality);
        assert_eq!(
            effective(EnhanceMode::Auto, Gpu::None),
            EnhanceMode::Performance
        );
        assert_eq!(effective(EnhanceMode::Off, Gpu::Nvidia), EnhanceMode::Off);
    }

    #[test]
    fn detects_anime_style_names() {
        assert!(looks_like_anime("/media/anime/show/episode 01.mkv"));
        assert!(looks_like_anime("[SubGroup] Some Show - 01 (1080p).mkv"));
        assert!(looks_like_anime(r"D:\Anime\Show\ep01.mkv")); // windows separators
        assert!(!looks_like_anime("/media/movies/Heat (1995).mkv"));
        assert!(!looks_like_anime("https://example.com/movie.mp4"));
        // Substring "anime" inside another word must not false-positive.
        assert!(!looks_like_anime("/media/movies/Reanimated (2020).mkv"));
        assert!(!looks_like_anime("/tv/The Animeals Show/ep.mkv"));
    }

    #[test]
    fn visual_class_prefers_metadata_and_live_sports() {
        assert_eq!(classify("Anything", &[], true), VisualClass::Sports);
        assert_eq!(
            classify("Blue Eye Samurai", &["Animation".into()], false),
            VisualClass::Cartoon
        );
        assert_eq!(classify("Anime series", &[], false), VisualClass::Anime);
        assert_eq!(
            classify("Heat", &["Crime".into()], false),
            VisualClass::LiveAction
        );
    }

    #[test]
    fn reads_height_from_names() {
        assert_eq!(source_height("Show.S01E01.1080p.mkv"), Some(1080));
        assert_eq!(source_height("Movie.2160p.HDR.mkv"), Some(2160));
        assert_eq!(source_height("https://cdn/clip_720p.mp4"), Some(720));
        assert_eq!(source_height("unknown.mkv"), None);
    }

    #[test]
    fn chains_only_reference_shaders_on_disk() {
        let dir = std::env::temp_dir().join(format!("tvos-shaders-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("FSRCNNX_x2_16-0-4-1.glsl"), "x").unwrap();

        let live_q = shader_chain(&dir, EnhanceMode::Quality, false);
        assert_eq!(live_q.len(), 1);
        assert!(live_q[0].ends_with("FSRCNNX_x2_16-0-4-1.glsl"));

        // Nothing downloaded for anime → empty chain, playback still fine.
        assert!(shader_chain(&dir, EnhanceMode::Quality, true).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
