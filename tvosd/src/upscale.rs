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
//! Shaders live in ~/.local/share/tvos/shaders (TVOS_SHADER_DIR to override);
//! system/get-shaders.sh downloads them. Chains only reference shaders that
//! are actually on disk, so a missing download degrades to mpv's own
//! high-quality scalers instead of failing playback.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::settings::EnhanceMode;

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

pub fn resolve(mode: EnhanceMode, target: &str) -> Profile {
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

    let anime = looks_like_anime(target);
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
        let joined = chain
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":");
        args.push(format!("--glsl-shaders={joined}"));
    }
    Profile { name, args }
}

/// Env defaults for launching modern games: engine-level upscalers beat any
/// video upscaler, so opt into them per GPU (FSR4 redirect needs a Proton
/// build that supports PROTON_FSR4_UPGRADE, e.g. Proton-CachyOS).
pub fn game_env() -> Vec<(&'static str, &'static str)> {
    match gpu() {
        Gpu::Amd => vec![("PROTON_FSR4_UPGRADE", "1")],
        Gpu::Nvidia => vec![("PROTON_ENABLE_NVAPI", "1")],
        Gpu::None => Vec::new(),
    }
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

fn shader_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TVOS_SHADER_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/tvos/shaders")
}

/// Anime detection without metadata: fansub-style "[Group] Title - 01" names
/// or an anime folder anywhere in the path. Wrong guesses still get a good
/// general-purpose chain, so this only needs to be roughly right.
fn looks_like_anime(target: &str) -> bool {
    let lower = target.to_lowercase();
    if lower.split(['/', '\\']).any(|seg| seg == "anime") || lower.contains("anime") {
        return true;
    }
    let name = lower.rsplit('/').next().unwrap_or(&lower);
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

fn detect_gpu() -> Gpu {
    match std::env::var("TVOS_GPU").as_deref() {
        Ok("nvidia") => return Gpu::Nvidia,
        Ok("amd") => return Gpu::Amd,
        Ok("none") => return Gpu::None,
        _ => {}
    }
    let nvidia = Command::new("nvidia-smi")
        .arg("-L")
        .output()
        .is_ok_and(|o| o.status.success());
    if nvidia {
        return Gpu::Nvidia;
    }
    // AMD: any DRM card with the AMD PCI vendor id (0x1002).
    for card in 0..4 {
        let vendor = format!("/sys/class/drm/card{card}/device/vendor");
        if std::fs::read_to_string(vendor).is_ok_and(|v| v.trim() == "0x1002") {
            return Gpu::Amd;
        }
    }
    Gpu::None
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
        assert!(!looks_like_anime("/media/movies/Heat (1995).mkv"));
        assert!(!looks_like_anime("https://example.com/movie.mp4"));
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
