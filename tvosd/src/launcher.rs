//! Process helpers shared by sources. The daemon runs inside the gamescope
//! session (started by system/tvos-shell), so spawned windows — the Steam
//! client, mpv — appear on the TV in front of the shell UI.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::upscale::Profile;

/// The currently playing mpv instance, if any. Starting a new video stops the
/// previous one so two players never fight over the screen.
static PLAYER: Mutex<Option<Child>> = Mutex::new(None);

/// Plays `target` fullscreen in mpv with the resolved enhance profile.
pub fn play_video(target: &str, profile: &Profile) -> Result<(), String> {
    let mut player = PLAYER.lock().unwrap();
    stop(&mut player);

    let mut cmd = Command::new("mpv");
    cmd.args(mpv_args(profile));
    cmd.arg(target);
    println!("playing [{}] {target}", profile.name);

    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start mpv: {e}"))?;
    *player = Some(child);
    Ok(())
}

/// Opens a link (a WatchHub service, an addon's /configure page) in the
/// system's default handler — the browser, or the streaming service's app.
pub fn open_external(url: &str) -> Result<(), String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    spawn_detached(opener, &[url]).map_err(|e| format!("could not open link: {e}"))
}

/// Streams a torrent magnet into our mpv (keeping the upscaler) by piping
/// webtorrent-cli's output into the player. For full seeking, configure the
/// addon (e.g. Torrentio) with a debrid service so it returns direct URLs,
/// which take the `play_video` path instead.
pub fn play_torrent(magnet: &str, file_idx: Option<i64>, profile: &Profile) -> Result<(), String> {
    if !command_exists("webtorrent") {
        return Err(
            "Torrent playback needs webtorrent-cli (`npm install -g webtorrent-cli`), \
             or configure the addon with a debrid service (e.g. RealDebrid) for direct streams."
                .to_string(),
        );
    }
    let mut player = PLAYER.lock().unwrap();
    stop(&mut player);

    let select = file_idx
        .map(|i| format!("--select {i}"))
        .unwrap_or_default();
    let mpv = mpv_args(profile)
        .iter()
        .map(|a| sh_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    // webtorrent streams to stdout; mpv reads it from stdin ("-").
    let pipeline = format!(
        "webtorrent download {} {select} --stdout 2>/dev/null | mpv {mpv} -",
        sh_quote(magnet)
    );
    println!("playing [torrent {}] {}", profile.name, magnet);

    let child = Command::new("bash")
        .arg("-c")
        .arg(&pipeline)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start torrent stream: {e}"))?;
    *player = Some(child);
    Ok(())
}

/// mpv flags for a profile: fullscreen, the enhance args, and the A/B toggle
/// script when a shader chain is active.
fn mpv_args(profile: &Profile) -> Vec<String> {
    let mut args: Vec<String> = ["--fs", "--no-terminal", "--force-window=immediate"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(profile.args.iter().cloned());
    if profile
        .args
        .iter()
        .any(|a| a.starts_with("--glsl-shaders="))
    {
        if let Some(script) = ensure_toggle_script() {
            args.push(format!("--script={}", script.display()));
        }
    }
    args
}

fn stop(player: &mut Option<Child>) {
    if let Some(mut old) = player.take() {
        let _ = old.kill();
        let _ = old.wait();
    }
}

fn command_exists(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Single-quotes a string for safe inclusion in a `bash -c` pipeline.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Writes the embedded A/B-comparison mpv script next to the UI data so it
/// always matches this daemon build.
fn ensure_toggle_script() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".local/share/tvos/mpv/enhance-toggle.lua");
    let script = include_str!("../data/enhance-toggle.lua");
    if std::fs::read_to_string(&path).is_ok_and(|on_disk| on_disk == script) {
        return Some(path);
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, script).ok()?;
    Some(path)
}

pub fn spawn_detached(program: &str, args: &[&str]) -> std::io::Result<()> {
    spawn_detached_env(program, args, &[])
}

pub fn spawn_detached_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> std::io::Result<()> {
    Command::new(program)
        .args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}
