//! The download manager: runs installs as background jobs and tracks their
//! progress so the UI can show it. Two kinds of job share one lifecycle:
//!
//!   start_command  — wraps a store CLI (e.g. `legendary install …`),
//!                    progress parsed from its stderr
//!   start_download — plain HTTP download (e.g. a ROM) straight to disk,
//!                    progress from bytes/content-length, with an optional
//!                    sha256 integrity check and a hard size cap
//!
//! Jobs are cancellable ([`InstallManager::cancel`]) and finished jobs are
//! evicted so the table stays bounded on a long-running daemon.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Hard cap on a single download (content-length *and* bytes written). Guards
/// against a hostile/misconfigured source filling the disk.
const MAX_DOWNLOAD_BYTES: u64 = 8 * 1024 * 1024 * 1024; // 8 GiB
/// If a CLI install produces no new output/progress for this long, it's treated
/// as wedged and failed rather than hanging forever.
const CLI_WATCHDOG: Duration = Duration::from_secs(600); // 10 min
/// Keep at most this many finished (Done/Failed) jobs in the table; older ones
/// are evicted so the map can't grow without bound over a long-running daemon.
const MAX_FINISHED_JOBS: usize = 100;

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
    /// When the job reached a terminal state (Done/Failed), for eviction. Not
    /// serialized — internal bookkeeping only.
    #[serde(skip)]
    finished_at: Option<Instant>,
}

#[derive(Default)]
pub struct InstallManager {
    jobs: Arc<Mutex<BTreeMap<String, Job>>>,
    /// Per-running-job cancellation flags, keyed by id. A worker checks its flag
    /// and aborts cooperatively; [`InstallManager::cancel`] sets it.
    cancels: Arc<Mutex<BTreeMap<String, Arc<AtomicBool>>>>,
}

/// A claimed slot in the job table; the worker thread reports through it.
struct JobHandle {
    id: String,
    jobs: Arc<Mutex<BTreeMap<String, Job>>>,
    cancels: Arc<Mutex<BTreeMap<String, Arc<AtomicBool>>>>,
    cancel: Arc<AtomicBool>,
}

impl JobHandle {
    fn update(&self, progress: Option<f32>, detail: Option<&str>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        // The job should exist while running, but a race with eviction/cancel
        // shouldn't panic — just drop the update if the slot is gone.
        let Some(job) = jobs.get_mut(&self.id) else {
            return;
        };
        if let Some(p) = progress {
            job.progress = p;
        }
        if let Some(d) = detail {
            job.detail = d.to_string();
        }
    }

    /// Whether this job has been asked to cancel.
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn finish(self, ok: bool, detail: &str) {
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(job) = jobs.get_mut(&self.id) {
                job.status = if ok { Status::Done } else { Status::Failed };
                if ok {
                    job.progress = 100.0;
                }
                job.detail = detail.to_string();
                job.finished_at = Some(Instant::now());
            }
            evict_finished(&mut jobs);
        }
        // The job is no longer running: drop its cancellation flag.
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// Keeps at most [`MAX_FINISHED_JOBS`] terminal jobs, evicting the oldest by
/// completion time so the table doesn't grow forever. Running jobs are kept.
fn evict_finished(jobs: &mut BTreeMap<String, Job>) {
    let mut finished: Vec<(Instant, String)> = jobs
        .values()
        .filter_map(|j| j.finished_at.map(|t| (t, j.id.clone())))
        .collect();
    if finished.len() <= MAX_FINISHED_JOBS {
        return;
    }
    finished.sort_by_key(|(t, _)| *t); // oldest first
    let drop_count = finished.len() - MAX_FINISHED_JOBS;
    for (_, id) in finished.into_iter().take(drop_count) {
        jobs.remove(&id);
    }
}

impl InstallManager {
    pub fn jobs(&self) -> Vec<Job> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Cancels a running install by id: signals its worker to stop. Returns an
    /// error if there's no such running job. The worker flips the job to Failed
    /// and cleans up its partial file when it observes the flag.
    pub fn cancel(&self, id: &str) -> Result<(), String> {
        match self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
        {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                Ok(())
            }
            None => Err("no such running install".to_string()),
        }
    }

    /// Registers a running job for `id`, refusing duplicates.
    fn claim(&self, id: &str, title: &str) -> Result<JobHandle, String> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
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
                finished_at: None,
            },
        );
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), Arc::clone(&cancel));
        Ok(JobHandle {
            id: id.to_string(),
            jobs: Arc::clone(&self.jobs),
            cancels: Arc::clone(&self.cancels),
            cancel,
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
            // Read stderr on a helper thread so the main worker can enforce a
            // watchdog: if no new line arrives (and no cancel) for CLI_WATCHDOG,
            // the install is considered wedged and killed.
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let reader = std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });

            let mut wedged = false;
            loop {
                match rx.recv_timeout(CLI_WATCHDOG) {
                    Ok(line) => handle.update(parse_progress(&line), displayable(&line)),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break, // stderr closed
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        wedged = true;
                        break;
                    }
                }
                if handle.cancelled() {
                    break;
                }
            }

            let cancelled = handle.cancelled();
            if wedged || cancelled {
                let _ = child.kill();
            }
            let ok = child.wait().map(|s| s.success()).unwrap_or(false) && !wedged && !cancelled;
            let _ = reader.join();
            handle.finish(
                ok,
                if ok {
                    "Installed"
                } else if cancelled {
                    "Cancelled"
                } else if wedged {
                    "Failed — installer stalled (no progress)"
                } else {
                    "Failed — see daemon log"
                },
            );
        });
        Ok(())
    }

    /// Downloads `url` to `dest` as an install job. Writes to `dest.part`
    /// first so an interrupted download never leaves a half-written file.
    /// `sha256` (lowercase hex), when supplied, is verified after download and
    /// the job fails on mismatch. The download is cancellable, size-capped, and
    /// aborts if it exceeds [`MAX_DOWNLOAD_BYTES`].
    pub fn start_download(
        &self,
        id: &str,
        title: &str,
        url: String,
        dest: PathBuf,
        sha256: Option<String>,
    ) -> Result<(), String> {
        let handle = self.claim(id, title)?;
        std::thread::spawn(
            move || match download(&url, &dest, sha256.as_deref(), &handle) {
                Ok(()) => handle.finish(true, "Installed"),
                Err(DownloadError::Cancelled) => handle.finish(false, "Cancelled"),
                Err(DownloadError::Failed(e)) => handle.finish(false, &e),
            },
        );
        Ok(())
    }
}

enum DownloadError {
    Cancelled,
    Failed(String),
}

impl From<String> for DownloadError {
    fn from(e: String) -> Self {
        DownloadError::Failed(e)
    }
}

fn download(
    url: &str,
    dest: &std::path::Path,
    sha256: Option<&str>,
    handle: &JobHandle,
) -> Result<(), DownloadError> {
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
    // Reject an oversized download up front where the server declares its size.
    if total > MAX_DOWNLOAD_BYTES {
        return Err(format!("too large ({} MB) — refusing", mb(total)).into());
    }
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut hasher = sha256.is_some().then(Sha256::new);
    let mut received: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    let cleanup = |file: std::fs::File| {
        drop(file);
        let _ = std::fs::remove_file(&part);
    };
    loop {
        if handle.cancelled() {
            cleanup(file);
            return Err(DownloadError::Cancelled);
        }
        let n = match response.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                cleanup(file);
                return Err(format!("download interrupted: {e}").into());
            }
        };
        if n == 0 {
            break;
        }
        // Cap on bytes actually written, in case content-length lied or was
        // absent — never let a runaway body fill the disk.
        received += n as u64;
        if received > MAX_DOWNLOAD_BYTES {
            cleanup(file);
            return Err("exceeded size cap — refusing".to_string().into());
        }
        if let Err(e) = file.write_all(&buf[..n]) {
            cleanup(file);
            return Err(e.to_string().into());
        }
        if let Some(h) = hasher.as_mut() {
            h.update(&buf[..n]);
        }
        if total > 0 {
            let pct = received as f32 / total as f32 * 100.0;
            handle.update(
                Some(pct),
                Some(&format!("{} of {} MB", mb(received), mb(total))),
            );
        }
    }
    // Verify the checksum before publishing the file.
    if let (Some(expected), Some(h)) = (sha256, hasher) {
        let got = h.finish_hex();
        if !got.eq_ignore_ascii_case(expected.trim()) {
            cleanup(file);
            return Err(format!("checksum mismatch (expected {expected}, got {got})").into());
        }
    }
    drop(file);
    std::fs::rename(&part, dest).map_err(|e| e.to_string().into())
}

fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// Minimal, dependency-free SHA-256 (FIPS 180-4) for verifying downloads. The
/// project pulls in no hashing crate, and a heavy dep isn't warranted for one
/// optional integrity check, so this self-contained streaming implementation
/// covers it. Feed bytes with [`Sha256::update`], read hex with
/// [`Sha256::finish_hex`].
struct Sha256 {
    state: [u32; 8],
    len_bits: u64,
    buf: [u8; 64],
    buf_len: usize,
}

impl Sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            len_bits: 0,
            buf: [0u8; 64],
            buf_len: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.len_bits = self.len_bits.wrapping_add((data.len() as u64) * 8);
        while !data.is_empty() {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.process(&block);
                self.buf_len = 0;
            }
        }
    }

    fn process(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = self.state;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(Self::K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for (s, x) in self.state.iter_mut().zip(v) {
            *s = s.wrapping_add(x);
        }
    }

    fn finish_hex(mut self) -> String {
        let len_bits = self.len_bits;
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0u8]);
        }
        self.update(&len_bits.to_be_bytes());
        let mut hex = String::with_capacity(64);
        for word in self.state {
            hex.push_str(&format!("{word:08x}"));
        }
        hex
    }
}

/// Extracts a percentage from lines like
/// `[DLManager] INFO: = Progress: 42.99% (1175/2732), ETA: 00:00:18`.
///
/// Anchors on a "progress" label (case-insensitive) followed by any run of
/// spaces/`:`/`=`, then the number up to the `%`, so minor formatting drift in
/// the installer's output (extra spaces, `Progress =`, lower-case) still parses
/// instead of silently reporting no progress.
fn parse_progress(line: &str) -> Option<f32> {
    let lower = line.to_lowercase();
    let idx = lower.find("progress")?;
    // Skip the label and any separator characters after it.
    let after = line[idx + "progress".len()..].trim_start_matches([' ', ':', '=', '\t']);
    let number = after.split('%').next()?.trim();
    // Keep only the leading numeric token (guards against "not a number").
    let number: String = number
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let pct: f32 = number.parse().ok()?;
    (0.0..=100.0).contains(&pct).then_some(pct)
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

    #[test]
    fn parses_progress_with_formatting_drift() {
        assert_eq!(parse_progress("[DL] Progress = 12.5% done"), Some(12.5));
        assert_eq!(parse_progress("progress:   99%"), Some(99.0));
        assert_eq!(parse_progress("Progress: 150%"), None); // out of range
    }

    #[test]
    fn sha256_matches_known_vectors() {
        let empty = {
            let h = Sha256::new();
            h.finish_hex()
        };
        assert_eq!(
            empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            h.finish_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn cancel_of_unknown_job_errors() {
        let mgr = InstallManager::default();
        assert!(mgr.cancel("rom:gb/missing.gb").is_err());
    }

    #[test]
    fn finished_jobs_are_evicted() {
        let mut jobs: BTreeMap<String, Job> = BTreeMap::new();
        for n in 0..(MAX_FINISHED_JOBS + 10) {
            jobs.insert(
                format!("id{n}"),
                Job {
                    id: format!("id{n}"),
                    title: "t".into(),
                    status: Status::Done,
                    progress: 100.0,
                    detail: "done".into(),
                    finished_at: Some(Instant::now()),
                },
            );
        }
        evict_finished(&mut jobs);
        assert_eq!(jobs.len(), MAX_FINISHED_JOBS);
    }
}
