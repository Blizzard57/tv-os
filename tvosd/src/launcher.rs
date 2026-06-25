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
    if let Some(mut old) = player.take() {
        let _ = old.kill();
        let _ = old.wait();
    }

    let mut cmd = Command::new("mpv");
    cmd.args(["--fs", "--no-terminal", "--force-window=immediate"]);
    cmd.args(&profile.args);
    if profile
        .args
        .iter()
        .any(|a| a.starts_with("--glsl-shaders="))
    {
        if let Some(script) = ensure_toggle_script() {
            cmd.arg(format!("--script={}", script.display()));
        }
    }
    cmd.arg(target);
    println!(
        "playing [{}] {target} args={:?}",
        profile.name, profile.args
    );

    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start mpv: {e}"))?;
    *player = Some(child);
    Ok(())
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
