//! The download manager: runs installs as background jobs and tracks their
//! progress so the UI can show it. Two kinds of job share one lifecycle:
//!
//!   start_command  — wraps a store CLI (e.g. `legendary install …`),
//!                    progress parsed from its stderr
//!   start_download — plain HTTP download (e.g. a ROM) straight to disk,
//!                    progress from bytes/content-length

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

#[derive(Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Done,
    Failed,
}

#[derive(Serialize, Clone)]
pub struct Job {
    /// Content id being installed, e.g. "epic:Sugar" or "rom:gb/Libbet.gb".
    pub id: String,
    pub title: String,
    pub status: Status,
    /// 0–100. Stays at 0 if the tool reports no progress.
    pub progress: f32,
    /// Last interesting output line — shown as the job's detail text.
    pub detail: String,
}

#[derive(Default)]
pub struct InstallManager {
    jobs: Arc<Mutex<BTreeMap<String, Job>>>,
}

/// A claimed slot in the job table; the worker thread reports through it.
struct JobHandle {
    id: String,
    jobs: Arc<Mutex<BTreeMap<String, Job>>>,
}

impl JobHandle {
    fn update(&self, progress: Option<f32>, detail: Option<&str>) {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs.get_mut(&self.id).expect("job exists while running");
        if let Some(p) = progress {
            job.progress = p;
        }
        if let Some(d) = detail {
            job.detail = d.to_string();
        }
    }

    fn finish(self, ok: bool, detail: &str) {
        let mut jobs = self.jobs.lock().unwrap();
        let job = jobs.get_mut(&self.id).expect("job exists while running");
        job.status = if ok { Status::Done } else { Status::Failed };
        if ok {
            job.progress = 100.0;
        }
        job.detail = detail.to_string();
    }
}

impl InstallManager {
    pub fn jobs(&self) -> Vec<Job> {
        self.jobs.lock().unwrap().values().cloned().collect()
    }

    /// Registers a running job for `id`, refusing duplicates.
    fn claim(&self, id: &str, title: &str) -> Result<JobHandle, String> {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.get(id).is_some_and(|j| j.status == Status::Running) {
            return Err(format!("{title} is already downloading"));
        }
        jobs.insert(
            id.to_string(),
            Job {
                id: id.to_string(),
                title: title.to_string(),
                status: Status::Running,
                progress: 0.0,
                detail: "Starting…".to_string(),
            },
        );
        Ok(JobHandle {
            id: id.to_string(),
            jobs: Arc::clone(&self.jobs),
        })
    }

    /// Runs `cmd` as an install job, parsing progress from its stderr
    /// (where legendary and friends log).
    pub fn start_command(&self, id: &str, title: &str, mut cmd: Command) -> Result<(), String> {
        let handle = self.claim(id, title)?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                handle.finish(false, "Installer did not start");
                return Err(format!("could not start installer: {e}"));
            }
        };
        let stderr = child.stderr.take().expect("stderr was piped");

        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                handle.update(parse_progress(&line), displayable(&line));
            }
            let ok = child.wait().map(|s| s.success()).unwrap_or(false);
            handle.finish(
                ok,
                if ok {
                    "Installed"
                } else {
                    "Failed — see daemon log"
                },
            );
        });
        Ok(())
    }

    /// Downloads `url` to `dest` as an install job. Writes to `dest.part`
    /// first so an interrupted download never leaves a half-written file.
    pub fn start_download(
        &self,
        id: &str,
        title: &str,
        url: String,
        dest: PathBuf,
    ) -> Result<(), String> {
        let handle = self.claim(id, title)?;
        std::thread::spawn(move || match download(&url, &dest, &handle) {
            Ok(()) => handle.finish(true, "Installed"),
            Err(e) => handle.finish(false, &e),
        });
        Ok(())
    }
}

fn download(url: &str, dest: &std::path::Path, handle: &JobHandle) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let mut response = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("download failed: {e}"))?;

    let total = response.content_length().unwrap_or(0);
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut received: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = response
            .read(&mut buf)
            .map_err(|e| format!("download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        received += n as u64;
        if total > 0 {
            let pct = received as f32 / total as f32 * 100.0;
            handle.update(
                Some(pct),
                Some(&format!("{} of {} MB", mb(received), mb(total))),
            );
        }
    }
    drop(file);
    std::fs::rename(&part, dest).map_err(|e| e.to_string())
}

fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// Extracts a percentage from lines like
/// `[DLManager] INFO: = Progress: 42.99% (1175/2732), ETA: 00:00:18`.
fn parse_progress(line: &str) -> Option<f32> {
    let after = line.split("Progress: ").nth(1)?;
    let number = after.split('%').next()?;
    number.trim().parse().ok()
}

/// Filters installer log noise down to lines worth showing in the UI.
fn displayable(line: &str) -> Option<&str> {
    let message = line
        .split("INFO: ")
        .nth(1)?
        .trim_start_matches(['=', ' ', '+'])
        .trim();
    (!message.is_empty()).then_some(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legendary_progress_lines() {
        let line =
            "[DLManager] INFO: = Progress: 42.99% (1175/2732), Running for 00:00:14, ETA: 00:00:18";
        assert_eq!(parse_progress(line), Some(42.99));
    }

    #[test]
    fn ignores_lines_without_progress() {
        assert_eq!(parse_progress("[Core] INFO: Login successful"), None);
        assert_eq!(parse_progress("Progress: not a number%"), None);
    }

    #[test]
    fn extracts_display_detail() {
        let line = "[DLManager] INFO: = Progress: 42.99% (1175/2732)";
        assert_eq!(displayable(line), Some("Progress: 42.99% (1175/2732)"));
        assert_eq!(displayable("random stderr noise"), None);
    }

    #[test]
    fn duplicate_running_jobs_are_refused() {
        let mgr = InstallManager::default();
        mgr.claim("rom:gb/x.gb", "X").unwrap();
        assert!(mgr.claim("rom:gb/x.gb", "X").is_err());
    }
}
