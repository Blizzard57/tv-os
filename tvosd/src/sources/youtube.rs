//! YouTube source — keyless, via yt-dlp.
//!
//! Rows come from the channels the user follows (Settings → "YouTube
//! channels", a list of @handles): one home row per channel, like
//! subscriptions. Search (the deep tier) queries ytsearch. Playback hands the
//! watch URL to mpv, whose yt-dlp hook resolves the actual stream — so the
//! player, upscaler and resume machinery all work exactly as for any video.
//!
//! No API key anywhere: listings use yt-dlp's --flat-playlist JSON and
//! artwork is the deterministic i.ytimg.com thumbnail URL.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::media::Meta;
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::{launcher, settings, upscale};

const CHANNEL_TTL: Duration = Duration::from_secs(900);
const SEARCH_TTL: Duration = Duration::from_secs(300);
const CHANNEL_ROWS: usize = 6; // at most this many followed-channel shelves
const ACCOUNT_FEED_LIMIT: usize = 24;
const VIDEOS_PER_CHANNEL: usize = 12;
const SEARCH_LIMIT: usize = 20;
/// yt-dlp is a network tool — bound every call so a hung request can't
/// stall the library or a search.
const YTDLP_TIMEOUT_SECS: &str = "12";

/// url → (fetched at, channel display name, videos).
static CHANNEL_CACHE: LazyLock<Mutex<HashMap<String, (Instant, String, Vec<ContentItem>)>>> =
    LazyLock::new(Mutex::default);
static SEARCH_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<ContentItem>)>>> =
    LazyLock::new(Mutex::default);

pub struct YouTube {
    available: bool,
}

impl YouTube {
    pub fn detect() -> Self {
        let available = Command::new("yt-dlp")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        Self { available }
    }
}

impl Source for YouTube {
    fn id(&self) -> &'static str {
        "yt"
    }

    fn available(&self) -> bool {
        self.available
    }

    /// Personal feeds first (when the account is connected), then one row
    /// per followed channel, newest uploads first.
    fn rows(&self) -> Vec<Row> {
        let settings = settings::STORE.get();
        let handles = channel_handles(&settings.youtube_channels);
        // Every feed is an independent network fetch — do them in parallel.
        std::thread::scope(|scope| {
            let account_jobs: Vec<_> = if settings.youtube_account {
                ACCOUNT_FEEDS
                    .iter()
                    .map(|(title, target)| scope.spawn(move || feed_row(title, target)))
                    .collect()
            } else {
                Vec::new()
            };
            let channel_jobs: Vec<_> = handles
                .iter()
                .take(CHANNEL_ROWS)
                .map(|handle| scope.spawn(move || channel_row(handle)))
                .collect();
            account_jobs
                .into_iter()
                .chain(channel_jobs)
                .filter_map(|j| j.join().ok().flatten())
                .collect()
        })
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let video = item_id.trim_start_matches("yt:");
        if video.is_empty() {
            return Err(format!("bad YouTube id '{item_id}'"));
        }
        let url = format!("https://www.youtube.com/watch?v={video}");
        let mode = settings::STORE.get().enhance;
        let profile = upscale::resolve(mode, "youtube");
        let mut meta = launcher::PlayerMeta::new("YouTube");
        meta.preference_scope = Some("youtube".into());
        meta.preference_provider = Some("youtube".into());
        meta.playback_preference = crate::profile::STORE.playback_preference("youtube", "youtube");
        meta.quality = meta
            .playback_preference
            .as_ref()
            .and_then(|preference| preference.quality_tier.clone());
        meta.sponsorblock_segments = crate::sponsorblock::segments(video);
        meta.content_id = Some(item_id.to_string());
        meta.track_id = Some(item_id.to_string());
        meta.domain = Some("youtube".into());
        meta.autoplay = settings::STORE.get().autoplay;
        meta.autoplay_delay_seconds = settings::STORE.get().autoplay_delay_seconds.clamp(3, 30);
        launcher::play_video(
            &url,
            &profile,
            mode,
            "YouTube",
            Some(item_id),
            Some(item_id),
            Some(&meta),
        )
    }
}

pub fn play_quality(
    item_id: &str,
    title: &str,
    art: Option<&str>,
    quality: &str,
) -> Result<(), String> {
    let video = item_id
        .strip_prefix("yt:")
        .filter(|video| !video.is_empty())
        .ok_or_else(|| format!("bad YouTube id '{item_id}'"))?;
    let settings = settings::STORE.get();
    let profile = upscale::resolve(settings.enhance, "youtube");
    let mut preference = crate::profile::STORE
        .playback_preference("youtube", "youtube")
        .unwrap_or(crate::profile::PlaybackPreference {
            scope_key: "youtube".into(),
            provider: "youtube".into(),
            ..Default::default()
        });
    preference.quality_tier = (!quality.eq_ignore_ascii_case("auto")).then(|| quality.to_string());
    let mut meta = launcher::PlayerMeta::new(title);
    meta.preference_scope = Some("youtube".into());
    meta.preference_provider = Some("youtube".into());
    meta.playback_preference = Some(preference.clone());
    meta.quality = preference.quality_tier.clone();
    meta.sponsorblock_segments = crate::sponsorblock::segments(video);
    meta.content_id = Some(item_id.to_string());
    meta.track_id = Some(item_id.to_string());
    meta.art = art.map(str::to_owned);
    meta.domain = Some("youtube".into());
    meta.autoplay = settings.autoplay;
    meta.autoplay_delay_seconds = settings.autoplay_delay_seconds.clamp(3, 30);
    let result = launcher::play_video(
        &format!("https://www.youtube.com/watch?v={video}"),
        &profile,
        settings.enhance,
        "youtube",
        Some(item_id),
        Some(item_id),
        Some(&meta),
    );
    if result.is_ok() {
        let _ = crate::profile::STORE.save_playback_preference(&preference);
    }
    result
}

/// Signed-in feeds (yt-dlp aliases; both need account cookies).
const ACCOUNT_FEEDS: &[(&str, &str)] = &[("For you", ":ytrec"), ("Subscriptions", ":ytsubs")];

/// The TV OS browser profile. tvos-app launches Chromium with
/// --password-store=basic, so yt-dlp can decrypt its cookies without a
/// keyring — signing in to YouTube inside the app is all it takes.
fn browser_profile() -> Option<String> {
    let profile = std::env::var("TVOS_BROWSER_PROFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| crate::settings::profile_dir().join("browser/Default"));
    profile
        .join("Cookies")
        .exists()
        .then(|| profile.to_string_lossy().into_owned())
}

/// Whether the signed-in feeds are reachable, with a human reason if not.
/// Used by /api/youtube/status so Settings can show a truthful state.
pub fn account_status() -> (bool, String) {
    let Some(profile) = browser_profile() else {
        return (
            false,
            "Not signed in — open YouTube inside TV OS and sign in first".to_string(),
        );
    };
    match flat_playlist_auth(":ytsubs", 5, Some(&profile)) {
        Some(json) => {
            let n = parse_entries(&json).len();
            if n > 0 {
                (
                    true,
                    "Connected — your subscriptions feed works".to_string(),
                )
            } else {
                (
                    false,
                    "Cookies found but the feed is empty — sign in to YouTube inside TV OS again"
                        .to_string(),
                )
            }
        }
        None => (
            false,
            "Could not read the signed-in feed — sign in to YouTube inside TV OS, then retry"
                .to_string(),
        ),
    }
}

fn feed_row(title: &str, target: &str) -> Option<Row> {
    let profile = browser_profile()?;
    {
        let cache = CHANNEL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, _, items)) = cache.get(target) {
            if at.elapsed() < CHANNEL_TTL {
                return row_of_titled(title, items.clone());
            }
        }
    }
    let json = flat_playlist_auth(target, ACCOUNT_FEED_LIMIT, Some(&profile))?;
    let items = parse_entries(&json);
    CHANNEL_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            target.to_string(),
            (Instant::now(), title.to_string(), items.clone()),
        );
    row_of_titled(title, items)
}

fn row_of_titled(title: &str, items: Vec<ContentItem>) -> Option<Row> {
    (!items.is_empty()).then(|| Row {
        title: title.to_string(),
        items,
    })
}

/// "veritasium, @kurzgesagt https://youtube.com/@mkbhd" → normalized handles.
fn channel_handles(raw: &str) -> Vec<String> {
    raw.split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let s = s
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_start_matches("www.")
                .trim_start_matches("youtube.com/")
                .trim_end_matches('/');
            let s = s.strip_suffix("/videos").unwrap_or(s);
            if s.starts_with('@') {
                s.to_string()
            } else {
                format!("@{s}")
            }
        })
        .collect()
}

fn channel_row(handle: &str) -> Option<Row> {
    let url = format!("https://www.youtube.com/{handle}/videos");
    {
        let cache = CHANNEL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, name, items)) = cache.get(&url) {
            if at.elapsed() < CHANNEL_TTL {
                return row_of(name, items.clone());
            }
        }
    }
    let json = flat_playlist(&url, VIDEOS_PER_CHANNEL)?;
    let name = json
        .get("channel")
        .or_else(|| json.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or(handle)
        .to_string();
    let items = parse_entries(&json);
    CHANNEL_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(url, (Instant::now(), name.clone(), items.clone()));
    row_of(&name, items)
}

fn row_of(name: &str, items: Vec<ContentItem>) -> Option<Row> {
    (!items.is_empty()).then(|| Row {
        title: name.to_string(),
        items,
    })
}

/// Deep-search tier: ytsearch results, cached per query.
pub fn search(query: &str) -> Vec<ContentItem> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let cache_key = query.to_lowercase();
    if let Some((at, items)) = SEARCH_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
    {
        if at.elapsed() < SEARCH_TTL {
            return items.clone();
        }
    }
    let items = flat_playlist(&format!("ytsearch{SEARCH_LIMIT}:{query}"), SEARCH_LIMIT)
        .map(|json| parse_entries(&json))
        .unwrap_or_default();
    let mut cache = SEARCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 64 {
        cache.clear();
    }
    cache.insert(cache_key, (Instant::now(), items.clone()));
    items
}

pub fn autoplay_recommendation(current_id: &str) -> Option<ContentItem> {
    let settings = settings::STORE.get();
    if settings.youtube_account {
        for (_, target) in ACCOUNT_FEEDS {
            if let Some(row) = feed_row("For you", target) {
                if let Some(item) = row.items.into_iter().find(|item| item.id != current_id) {
                    return Some(item);
                }
            }
        }
    }
    for handle in channel_handles(&settings.youtube_channels)
        .into_iter()
        .take(6)
    {
        if let Some(row) = channel_row(&handle) {
            if let Some(item) = row.items.into_iter().find(|item| item.id != current_id) {
                return Some(item);
            }
        }
    }
    None
}

static META_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Meta)>>> =
    LazyLock::new(Mutex::default);

/// Full metadata for one video (description, channel, views) — feeds the
/// hero preview and the details page. One yt-dlp call per video, cached.
pub fn video_meta(item_id: &str) -> Option<Meta> {
    let video = item_id.trim_start_matches("yt:");
    if video.is_empty() {
        return None;
    }
    if let Some((at, meta)) = META_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(video)
    {
        if at.elapsed() < CHANNEL_TTL {
            return Some(meta.clone());
        }
    }
    let out = Command::new("timeout")
        .args([
            YTDLP_TIMEOUT_SECS,
            "yt-dlp",
            "--dump-single-json",
            "--no-playlist",
        ])
        .arg(format!("https://www.youtube.com/watch?v={video}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let mut description = s("description").unwrap_or_default();
    if description.len() > 600 {
        // The hero clamps to a few lines anyway; keep the payload sane.
        description = format!("{}…", description.chars().take(600).collect::<String>());
    }
    let views = v
        .get("view_count")
        .and_then(|x| x.as_i64())
        .map(format_views);
    let release_info = match (s("channel"), views) {
        (Some(c), Some(w)) => Some(format!("{c} · {w}")),
        (c, w) => c.or(w),
    };
    let meta = Meta {
        id: item_id.to_string(),
        kind: "video".to_string(),
        title: s("title").unwrap_or_default(),
        description: (!description.is_empty()).then_some(description),
        background: s("thumbnail"),
        release_info,
        runtime: s("duration_string"),
        ..Default::default()
    };
    META_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(video.to_string(), (Instant::now(), meta.clone()));
    Some(meta)
}

fn format_views(n: i64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B views", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M views", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.0}K views", n as f64 / 1e3),
        n => format!("{n} views"),
    }
}

fn flat_playlist(target: &str, limit: usize) -> Option<Value> {
    flat_playlist_auth(target, limit, None)
}

/// `profile` = a Chromium profile directory whose cookies yt-dlp should use
/// (the signed-in feeds need them); None = anonymous.
fn flat_playlist_auth(target: &str, limit: usize, profile: Option<&str>) -> Option<Value> {
    let mut cmd = Command::new("timeout");
    cmd.args([
        YTDLP_TIMEOUT_SECS,
        "yt-dlp",
        "--flat-playlist",
        "--dump-single-json",
    ])
    .args(["--playlist-items", &format!("1-{limit}")]);
    if let Some(profile) = profile {
        cmd.args(["--cookies-from-browser", &format!("chromium:{profile}")]);
    }
    let out = cmd.arg(target).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn parse_entries(json: &Value) -> Vec<ContentItem> {
    json.get("entries")
        .and_then(|e| e.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| {
                    let id = e.get("id")?.as_str()?;
                    Some(ContentItem {
                        id: format!("yt:{id}"),
                        kind: Kind::Video,
                        title: e.get("title")?.as_str()?.to_string(),
                        // Deterministic thumbnail — no need to parse the list.
                        art: Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
                        action: Action::Play,
                        note: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_normalized() {
        let handles =
            channel_handles("veritasium, @kurzgesagt https://www.youtube.com/@mkbhd/videos");
        assert_eq!(handles, vec!["@veritasium", "@kurzgesagt", "@mkbhd"]);
        assert!(channel_handles("  ").is_empty());
    }

    #[test]
    fn parses_flat_entries_with_thumbnails() {
        let json: Value = serde_json::from_str(
            r#"{"channel": "Chan", "entries": [
                {"id": "abc123", "title": "First"},
                {"title": "no id, skipped"}
            ]}"#,
        )
        .unwrap();
        let items = parse_entries(&json);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "yt:abc123");
        assert_eq!(items[0].kind, Kind::Video);
        assert_eq!(
            items[0].art.as_deref(),
            Some("https://i.ytimg.com/vi/abc123/hqdefault.jpg")
        );
    }
}
