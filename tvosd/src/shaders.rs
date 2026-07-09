//! Auto-provisions the Enhance upscaling shaders (PLAN §5, and §3's
//! "everything configures itself in the background"). Without the shader
//! files, every Enhance profile silently degrades to mpv's built-in scalers —
//! so the daemon downloads them once at startup, on a background thread:
//!
//!   Anime4K v4  (anime chains)        — MIT, github.com/bloc97/Anime4K
//!   FSRCNNX     (live-action chains)  — github.com/igv/FSRCNN-TensorFlow
//!
//! Idempotent: already-present files are never re-fetched, and a failed
//! download just retries on the next daemon start. system/get-shaders.sh
//! remains as a manual/offline alternative writing the same directory.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// Every shader referenced by an upscale.rs chain. When all of these exist
/// the presets are fully live and nothing is downloaded.
const REQUIRED: [&str; 9] = [
    "Anime4K_Clamp_Highlights.glsl",
    "Anime4K_Restore_CNN_VL.glsl",
    "Anime4K_Restore_CNN_M.glsl",
    "Anime4K_Upscale_CNN_x2_VL.glsl",
    "Anime4K_Upscale_CNN_x2_M.glsl",
    "Anime4K_AutoDownscalePre_x2.glsl",
    "Anime4K_AutoDownscalePre_x4.glsl",
    "FSRCNNX_x2_16-0-4-1.glsl",
    "FSRCNNX_x2_8-0-4-1.glsl",
];

const FSRCNNX_BASE: &str = "https://github.com/igv/FSRCNN-TensorFlow/releases/download/1.1";
const ANIME4K_ZIP: &str =
    "https://github.com/bloc97/Anime4K/releases/download/v4.0.1/Anime4K_v4.0.zip";

/// Makes sure the shader chains can run; returns whether they all can.
pub fn ensure() -> bool {
    let dir = crate::upscale::shader_dir();
    let missing: Vec<&str> = REQUIRED
        .iter()
        .filter(|name| !dir.join(name).exists())
        .copied()
        .collect();
    if missing.is_empty() {
        return true;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::log_warn!("cannot create shader dir {}: {e}", dir.display());
        return false;
    }
    crate::log_info!("downloading {} upscaler shaders…", missing.len());

    for name in missing.iter().filter(|n| n.starts_with("FSRCNNX")) {
        match fetch(&format!("{FSRCNNX_BASE}/{name}")) {
            Ok(bytes) => {
                let _ = std::fs::write(dir.join(name), bytes);
            }
            Err(e) => crate::log_warn!("shader {name}: {e}"),
        }
    }

    if missing.iter().any(|n| n.starts_with("Anime4K")) {
        match fetch(ANIME4K_ZIP).and_then(|bytes| extract_glsl(&bytes, &dir)) {
            Ok(n) => crate::log_info!("Anime4K: installed {n} shaders"),
            Err(e) => crate::log_warn!("Anime4K: {e}"),
        }
    }

    REQUIRED.iter().all(|name| dir.join(name).exists())
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("download failed: {e}"))?;
    let mut bytes = Vec::new();
    response
        .take(64 * 1024 * 1024) // sanity cap
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Writes every .glsl in the archive straight into `dir` (flattened, like
/// `unzip -j`), skipping everything else. Returns how many were written.
fn extract_glsl(zip_bytes: &[u8], dir: &Path) -> Result<usize, String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|e| e.to_string())?;
    let mut count = 0;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_string();
        if !name.ends_with(".glsl") {
            continue;
        }
        let Some(base) = Path::new(&name).file_name() else {
            continue;
        };
        let mut out = std::fs::File::create(dir.join(base)).map_err(|e| e.to_string())?;
        std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn extracts_only_glsl_files_flattened() {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut buf);
        let opts = SimpleFileOptions::default();
        writer.start_file("glsl/Upscale/Test_Upscale.glsl", opts).unwrap();
        writer.write_all(b"//shader").unwrap();
        writer.start_file("README.md", opts).unwrap();
        writer.write_all(b"docs").unwrap();
        writer.finish().unwrap();
        let bytes = buf.into_inner();

        let dir = std::env::temp_dir().join(format!("tvos-shaderzip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let n = extract_glsl(&bytes, &dir).unwrap();
        assert_eq!(n, 1);
        assert!(dir.join("Test_Upscale.glsl").exists()); // flattened
        assert!(!dir.join("README.md").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
