//! Tiny, dependency-free logger.
//!
//! Everything diagnostic goes through here so it lands in *two* places at once:
//! the terminal (as before) **and** a persistent file
//! (`~/.local/share/tvos/logs/tvosd.log`, override with `TVOS_LOG_DIR`). The
//! gamescope session doesn't capture the daemon's stdout, so without the file
//! there were effectively no logs — failures (a stream that won't launch, an
//! addon that won't answer) vanished. The file is rotated at 5 MiB (keeping 3
//! generations) so it can't grow without bound.
//!
//! The output of the players we spawn (mpv, webtorrent-cli) is routed into the
//! same file via [`child_output`], so the actual reason a torrent or stream
//! fails — no peers, no space, missing mpv — is always recorded.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate the log once it passes this size so it never grows unbounded.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// How many rotated generations to keep (tvosd.log.1 .. tvosd.log.3).
const LOG_GENERATIONS: u32 = 3;

/// The single shared append handle used for our own log lines.
static FILE: LazyLock<Mutex<Option<File>>> = LazyLock::new(|| Mutex::new(init_file()));

pub fn log_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TVOS_LOG_DIR") {
        return PathBuf::from(dir);
    }
    crate::settings::profile_dir().join("logs")
}

pub fn log_path() -> PathBuf {
    log_dir().join("tvosd.log")
}

/// Opens (creating the dir) an append handle to the log file, rotating first if
/// it has grown past the size cap.
fn init_file() -> Option<File> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("tvosd.log");
    rotate_if_needed(&path);
    append_handle(&path)
}

/// If the active log exceeds the size cap, shift the generations along
/// (`.2`→`.3`, `.1`→`.2`, live→`.1`) and drop the oldest.
fn rotate_if_needed(path: &std::path::Path) {
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) <= MAX_LOG_BYTES {
        return;
    }
    let gen_path = |n: u32| path.with_file_name(format!("tvosd.log.{n}"));
    // Oldest first: drop generation N, then rename N-1 → N, … , 1 → 2.
    for n in (1..LOG_GENERATIONS).rev() {
        let _ = std::fs::rename(gen_path(n), gen_path(n + 1));
    }
    let _ = std::fs::rename(path, gen_path(1));
}

/// Re-opens the shared append handle onto a freshly rotated file. Called from
/// the write path once the live log crosses the size cap.
fn reopen(guard: &mut Option<File>) {
    let path = log_path();
    rotate_if_needed(&path);
    *guard = append_handle(&path);
}

fn append_handle(path: &std::path::Path) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Ensures the logger is initialised and records a startup banner. Calling this
/// early means the log file exists from boot, not just after the first event.
pub fn init() {
    write_line("INFO", &format!("logging to {}", log_path().display()));
}

/// Writes one timestamped line to stdout and the log file. Used via the
/// `log_info!` / `log_warn!` / `log_error!` macros.
pub fn write_line(level: &str, msg: &str) {
    let line = format!("{} [{level}] {msg}", timestamp());
    println!("{line}");
    if let Ok(mut guard) = FILE.lock() {
        // Rotate before writing when the live file has grown past the cap, so a
        // long-running daemon keeps at most MAX_LOG_BYTES * (generations+1).
        let over_cap = guard
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len() > MAX_LOG_BYTES)
            .unwrap_or(false);
        if over_cap {
            reopen(&mut guard);
        }
        if let Some(file) = guard.as_mut() {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
    }
}

/// A fresh append handle to the log file for a child process's stdout/stderr,
/// so spawned players write their diagnostics into the same log. Falls back to
/// discarding output if the file can't be opened.
pub fn child_output() -> Stdio {
    append_handle(&log_path())
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null)
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC) without pulling in a date crate, via Howard
/// Hinnant's civil-from-days algorithm.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);

    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Logs an informational line to stdout + the persistent log file.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::write_line("INFO", &format!($($arg)*)) };
}

/// Logs a warning line.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logging::write_line("WARN", &format!($($arg)*)) };
}

/// Logs an error line.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::write_line("ERROR", &format!($($arg)*)) };
}
