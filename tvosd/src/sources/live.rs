//! Live source — the "Live" tab.
//!
//! Aggregates genuinely-live streams from two providers, keyless:
//!   1. **YouTube Live** — curated + followed sport channels are probed with
//!      `yt-dlp …/@handle/live`; a channel only appears while it is actually
//!      broadcasting (`live_status == is_live`). Playback reuses the YouTube
//!      path (mpv's ytdl hook resolves the live HLS).
//!   2. **IPTV** — the public iptv-org catalog (sports + the user's region) plus
//!      any M3U playlists the user adds in Settings. Channels are classified
//!      into per-sport rows by name/group keywords and played by handing the
//!      stream URL straight to mpv (native HLS), with any referrer/user-agent
//!      the playlist specifies.
//!
//! Curation lives in `data/live_sources.json` (handles + keywords per sport);
//! the region defaults to India and is set in Settings. Everything is bounded
//! and cached, and degrades to "no Live rows" rather than stalling the home
//! load. Live items never resume or scrobble.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::Value;

use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::{launcher, settings, upscale};

const YTDLP_TIMEOUT_SECS: &str = "8";
const YT_TTL: Duration = Duration::from_secs(120); // live state flips quickly
const IPTV_TTL: Duration = Duration::from_secs(6 * 3600);
const SCHED_TTL: Duration = Duration::from_secs(900); // 15 min
const MAX_HANDLES_PER_SPORT: usize = 3;
/// Cap on total YouTube-live probes per refresh — each is a yt-dlp process, so
/// with many followed sports we don't spawn dozens at once.
const MAX_YT_PROBES: usize = 18;
/// Cap on how many sports we fetch fixtures for (TheSportsDB free = 30 req/min,
/// 2 requests each).
const MAX_SCHED_SPORTS: usize = 12;
const MAX_ITEMS_PER_ROW: usize = 40;
const MAX_IPTV_TOTAL: usize = 500;
const MAX_USER_IPTV_TOTAL: usize = 400;
const MAX_SCHED_PER_SPORT: usize = 12;
/// TheSportsDB free test key (30 req/min). Fixtures are cached, so a home load
/// makes at most a couple of calls.
const TSDB_KEY: &str = "123";
/// How long after kickoff a fixture is still treated as "in progress" when its
/// status field doesn't say (cricket runs long).
const LIVE_WINDOW_SECS: i64 = 6 * 3600;
/// Only surface fixtures starting within this horizon.
const UPCOMING_HORIZON_SECS: i64 = 48 * 3600;

// ---- curated seed ----

#[derive(Deserialize, Clone)]
struct SportSeed {
    id: String,
    label: String,
    /// TheSportsDB sport name for the schedule (e.g. "Cricket", "Soccer",
    /// "Motorsport"); absent = no fixtures fetched for this bucket.
    #[serde(default)]
    tsdb: Option<String>,
    #[serde(default)]
    youtube: Vec<String>,
    #[serde(default)]
    keywords: Vec<String>,
}

#[derive(Deserialize)]
struct Seed {
    sports: Vec<SportSeed>,
}

static SEED: LazyLock<Vec<SportSeed>> = LazyLock::new(|| {
    let text = std::env::var("TVOS_LIVE_SOURCES")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| include_str!("../../data/live_sources.json").to_string());
    serde_json::from_str::<Seed>(&text)
        .map(|s| s.sports)
        .unwrap_or_default()
});

// ---- IPTV stream registry (id → how to play it) ----

#[derive(Clone)]
struct StreamInfo {
    url: String,
    referrer: Option<String>,
    user_agent: Option<String>,
    title: String,
}

static IPTV_STREAMS: LazyLock<Mutex<HashMap<String, StreamInfo>>> = LazyLock::new(Mutex::default);

/// yt-dlp probe results, cached (including "not live", so a dead handle isn't
/// re-probed every home load). handle → (fetched at, maybe live item).
static YT_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Option<ContentItem>)>>> =
    LazyLock::new(Mutex::default);
/// url → (fetched at, parsed channels). Keyed by the playlist URL (user M3U).
static IPTV_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<Channel>)>>> =
    LazyLock::new(Mutex::default);
/// region → (built at, joined iptv-org API channels).
static API_CHANNELS: LazyLock<Mutex<HashMap<String, (Instant, Vec<Channel>)>>> =
    LazyLock::new(Mutex::default);
/// Stream URL → (checked at, reachable). A background sweep fills this in; dead
/// streams are pruned on later loads. Empty = unknown (shown optimistically).
static STREAM_HEALTH: LazyLock<Mutex<HashMap<String, (Instant, bool)>>> =
    LazyLock::new(Mutex::default);
/// True while a health sweep thread is running, so we never start two at once.
static SWEEP_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// TheSportsDB sport name → (fetched at, raw events for today+tomorrow).
static SCHED_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<Value>)>>> =
    LazyLock::new(Mutex::default);
/// EPG source URL → (fetched at, channel tvg-id → programmes). XMLTV guides.
static EPG_CACHE: LazyLock<Mutex<HashMap<String, (Instant, HashMap<String, Vec<Programme>>)>>> =
    LazyLock::new(Mutex::default);
/// eventId → the carrier channel we resolved for a fixture (built during rows()),
/// so a `live:match:<eventId>` card can play it.
static MATCH_REGISTRY: LazyLock<Mutex<HashMap<String, MatchInfo>>> = LazyLock::new(Mutex::default);

/// How to play a matched fixture's channel.
#[derive(Clone)]
enum PlayTarget {
    Iptv(String), // stream_key
    Yt(String),   // youtube video id
}

#[derive(Clone)]
struct MatchInfo {
    target: PlayTarget,
    channel: String, // carrier's display name, for the card's "On …" note
}

/// One XMLTV programme, times as epoch seconds.
#[derive(Clone)]
struct Programme {
    start: i64,
    stop: i64,
    title: String,
}

pub struct Live {
    yt_dlp: bool,
}

impl Live {
    pub fn detect() -> Self {
        let yt_dlp = Command::new("yt-dlp")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        Self { yt_dlp }
    }
}

impl Source for Live {
    fn id(&self) -> &'static str {
        "live"
    }

    fn available(&self) -> bool {
        // The tab exists whenever we could produce a live stream: yt-dlp for
        // YouTube live, or the always-on IPTV catalog.
        true
    }

    fn rows(&self) -> Vec<Row> {
        let cfg = settings::STORE.get();
        let followed = followed_sports(&cfg.live_sports);

        // Provider 1: YouTube live probes, per followed sport, in parallel.
        let yt_by_sport: HashMap<String, Vec<ContentItem>> = if self.yt_dlp {
            probe_youtube_live(&followed)
        } else {
            HashMap::new()
        };

        // Provider 2: IPTV — public catalog (sport-classified) + the user's own
        // playlists (shown in full).
        let iptv = collect_iptv(&cfg);

        // The fixture→channel resolver (carriers = the channels we just gathered
        // + any EPG guide). Built before the schedule so fixtures can be matched.
        let matcher = Matcher::build(&cfg, &iptv, &yt_by_sport);

        // Provider 3: schedule/EPG — what's live now and coming up, with a
        // playable carrier where we can resolve one.
        let sched_by_sport = collect_schedule(&followed, &matcher);

        build_rows(&followed, yt_by_sport, iptv, sched_by_sport)
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let rest = item_id.strip_prefix("live:").unwrap_or_default();
        let (kind, id) = rest.split_once(':').ok_or("bad live id")?;
        match kind {
            "yt" => launch_youtube(id),
            "iptv" => launch_iptv(id),
            // A fixture we resolved to a live carrier channel.
            "match" => launch_match(id),
            // Schedule cards are informational (Action::None) — no stream to play.
            "sched" => Err("This is a scheduled fixture — no live stream yet.".to_string()),
            _ => Err(format!("unknown live id '{item_id}'")),
        }
    }
}

/// A truthful snapshot of the Live tab's configuration + guide state, for the
/// Settings panel (`/api/live/status`). Reads EPG from cache where possible.
pub fn status() -> serde_json::Value {
    let cfg = settings::STORE.get();
    let region = if cfg.live_region.trim().is_empty() {
        "IN".to_string()
    } else {
        cfg.live_region.trim().to_ascii_uppercase()
    };
    let count = |s: &str| s.split([',', '\n', ' ']).filter(|x| !x.trim().is_empty()).count();
    let epg_srcs: Vec<String> = cfg
        .epg_urls
        .split([',', '\n', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let now = now_epoch();
    let (mut programmes, mut loaded) = (0usize, 0usize);
    for url in &epg_srcs {
        let n: usize = fetch_epg(url, now).values().map(Vec::len).sum();
        programmes += n;
        if n > 0 {
            loaded += 1;
        }
    }
    let matches = MATCH_REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).len();

    let detail = if epg_srcs.is_empty() {
        "No program guide set — add an XMLTV EPG URL to make live matches playable"
            .to_string()
    } else if programmes > 0 {
        format!("Guide loaded — {programmes} programmes, {matches} live matches resolved")
    } else {
        "Program guide set but no programmes loaded — check the EPG URL".to_string()
    };

    serde_json::json!({
        "region": region,
        "sports_followed": followed_sports(&cfg.live_sports).len(),
        "iptv_playlists": count(&cfg.iptv_playlists),
        "epg_sources": epg_srcs.len(),
        "epg_loaded": loaded,
        "programmes": programmes,
        "matches": matches,
        "detail": detail,
    })
}

/// The sports to show, in order: the user's followed list (matched against seed
/// ids/labels) or, when empty, every seeded sport.
fn followed_sports(raw: &str) -> Vec<SportSeed> {
    let wanted: Vec<String> = raw
        .split([',', ' ', '\n'])
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if wanted.is_empty() {
        return SEED.clone();
    }
    SEED.iter()
        .filter(|s| {
            let label = s.label.to_lowercase();
            wanted
                .iter()
                .any(|w| s.id == *w || label.contains(w.as_str()) || w.contains(&s.id))
        })
        .cloned()
        .collect()
}

// ---- YouTube live ----

fn probe_youtube_live(sports: &[SportSeed]) -> HashMap<String, Vec<ContentItem>> {
    std::thread::scope(|scope| {
        let jobs: Vec<_> = sports
            .iter()
            .flat_map(|sport| {
                sport
                    .youtube
                    .iter()
                    .take(MAX_HANDLES_PER_SPORT)
                    .map(move |handle| (sport.id.clone(), handle.clone()))
            })
            .take(MAX_YT_PROBES)
            .map(|(sport_id, handle)| {
                scope.spawn(move || (sport_id, probe_handle(&handle)))
            })
            .collect();
        let mut out: HashMap<String, Vec<ContentItem>> = HashMap::new();
        for job in jobs {
            if let Ok((sport_id, Some(item))) = job.join() {
                out.entry(sport_id).or_default().push(item);
            }
        }
        out
    })
}

fn probe_handle(handle: &str) -> Option<ContentItem> {
    {
        let cache = YT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, item)) = cache.get(handle) {
            if at.elapsed() < YT_TTL {
                return item.clone();
            }
        }
    }
    let item = fetch_live(handle);
    YT_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(handle.to_string(), (Instant::now(), item.clone()));
    item
}

fn fetch_live(handle: &str) -> Option<ContentItem> {
    let url = format!("https://www.youtube.com/{handle}/live");
    let out = Command::new("timeout")
        .args([
            YTDLP_TIMEOUT_SECS,
            "yt-dlp",
            "--dump-single-json",
            "--no-playlist",
            "--no-warnings",
        ])
        .arg(&url)
        .output()
        .ok()?;
    if !out.status.success() {
        return None; // not live, private, or unreachable
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    if v.get("live_status").and_then(|s| s.as_str()) != Some("is_live") {
        return None;
    }
    let id = v.get("id")?.as_str()?;
    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or(handle);
    Some(ContentItem {
        id: format!("live:yt:{id}"),
        kind: Kind::Live,
        title: title.to_string(),
        art: Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
        action: Action::Play,
        note: None,
        })
}

fn launch_youtube(video: &str) -> Result<(), String> {
    if video.is_empty() {
        return Err("bad live YouTube id".to_string());
    }
    let url = format!("https://www.youtube.com/watch?v={video}");
    let mode = settings::STORE.get().enhance;
    let profile = upscale::resolve(mode, "youtube");
    launcher::play_video(
        &url,
        &profile,
        mode,
        "YouTube Live",
        None, // live: no resume
        None, // live: no scrobble
        Some(&launcher::PlayerMeta::new("Live").live()),
    )
}

// ---- IPTV ----

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Channel {
    name: String,
    /// Grouping label: an iptv-org category ("news", "movies") or an M3U
    /// group-title. Non-sport channels become rows keyed by this.
    group: String,
    logo: Option<String>,
    url: String,
    referrer: Option<String>,
    user_agent: Option<String>,
    /// iptv-org categories (from the API); empty for user M3U channels.
    #[serde(default)]
    categories: Vec<String>,
    /// 2-letter country from the API; None for user M3U channels.
    #[serde(default)]
    country: Option<String>,
    /// EPG/XMLTV channel id (iptv-org channel id, or the M3U `tvg-id`) — the key
    /// that joins a channel to its programme guide for fixture matching.
    #[serde(default)]
    tvg_id: Option<String>,
    #[serde(default)]
    region: bool,
}

/// The IPTV layer: sport-classified channels from the public catalog, plus the
/// user's own playlists shown in full (grouped by their `group-title`, so a
/// pasted M3U just appears — sports or not).
struct IptvContent {
    by_sport: HashMap<String, Vec<Channel>>,
    /// (group title, channels) in first-seen order — non-sport live TV from the
    /// region catalog and the user's own playlists.
    groups: Vec<(String, Vec<Channel>)>,
}

// ---- iptv-org API ingestion (streams + channels + logos) ----

#[derive(serde::Deserialize)]
struct ApiChannel {
    id: String,
    name: String,
    #[serde(default)]
    categories: Vec<String>,
    country: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiStream {
    channel: Option<String>,
    url: String,
    referrer: Option<String>,
    user_agent: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiLogo {
    channel: Option<String>,
    url: String,
    #[serde(default)]
    width: i64,
    #[serde(default)]
    height: i64,
    #[serde(default)]
    in_use: bool,
}

fn api_json<T: serde::de::DeserializeOwned>(file: &str) -> Vec<T> {
    match http_get(&format!("https://iptv-org.github.io/api/{file}")) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Best logo per channel: prefer `in_use`, then the largest artwork.
fn build_logo_map() -> HashMap<String, String> {
    let mut best: HashMap<String, (i64, String)> = HashMap::new();
    for logo in api_json::<ApiLogo>("logos.json") {
        let Some(ch) = logo.channel else { continue };
        if logo.url.is_empty() {
            continue;
        }
        let score = if logo.in_use { 1 << 40 } else { 0 } + logo.width * logo.height;
        match best.get(&ch) {
            Some((s, _)) if *s >= score => {}
            _ => {
                best.insert(ch, (score, logo.url));
            }
        }
    }
    best.into_iter().map(|(k, (_, url))| (k, url)).collect()
}

/// Joins the iptv-org API into playable channels, keeping the world's sports
/// channels plus everything from the user's region (any category). One stream
/// per channel (first seen). Cached on disk so restarts are instant.
fn collect_api_channels(region_cc: &str) -> Vec<Channel> {
    {
        let cache = API_CHANNELS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, chans)) = cache.get(region_cc) {
            if at.elapsed() < IPTV_TTL {
                return chans.clone();
            }
        }
    }
    if let Some(chans) = read_disk_cache(region_cc) {
        API_CHANNELS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(region_cc.to_string(), (Instant::now(), chans.clone()));
        return chans;
    }

    let meta: HashMap<String, ApiChannel> = api_json::<ApiChannel>("channels.json")
        .into_iter()
        .map(|c| (c.id.clone(), c))
        .collect();
    if meta.is_empty() {
        return Vec::new();
    }
    let logos = build_logo_map();

    let mut out = Vec::new();
    let mut seen_channels = std::collections::HashSet::new();
    for st in api_json::<ApiStream>("streams.json") {
        let Some(cid) = st.channel else { continue };
        let Some(c) = meta.get(&cid) else { continue };
        let is_sports = c.categories.iter().any(|k| k.eq_ignore_ascii_case("sports"));
        let in_region = c.country.as_deref() == Some(region_cc);
        if !is_sports && !in_region {
            continue;
        }
        if !seen_channels.insert(cid.clone()) {
            continue; // one card per channel
        }
        out.push(Channel {
            name: c.name.clone(),
            group: c.categories.first().cloned().unwrap_or_default(),
            logo: logos.get(&cid).cloned(),
            url: st.url,
            referrer: st.referrer,
            user_agent: st.user_agent,
            categories: c.categories.clone(),
            country: c.country.clone(),
            tvg_id: Some(cid.clone()),
            region: in_region,
        });
    }

    write_disk_cache(region_cc, &out);
    API_CHANNELS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(region_cc.to_string(), (Instant::now(), out.clone()));
    out
}

fn disk_cache_path(region_cc: &str) -> std::path::PathBuf {
    settings::profile_dir()
        .join("cache")
        .join(format!("live-iptv-{region_cc}.json"))
}

fn read_disk_cache(region_cc: &str) -> Option<Vec<Channel>> {
    let path = disk_cache_path(region_cc);
    let age = std::fs::metadata(&path).ok()?.modified().ok()?.elapsed().ok()?;
    if age > IPTV_TTL {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()
}

fn write_disk_cache(region_cc: &str, chans: &[Channel]) {
    let path = disk_cache_path(region_cc);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(chans) {
        let _ = std::fs::write(path, json);
    }
}

// ---- assembling rows ----

/// Accumulates channels into per-sport rows and grouped "live TV" rows, applying
/// the caps and de-duplicating by URL.
struct Acc {
    by_sport: HashMap<String, Vec<Channel>>,
    groups: Vec<(String, Vec<Channel>)>,
    group_index: HashMap<String, usize>,
    seen_urls: std::collections::HashSet<String>,
    sport_total: usize,
    group_total: usize,
}

impl Acc {
    fn new() -> Self {
        Self {
            by_sport: HashMap::new(),
            groups: Vec::new(),
            group_index: HashMap::new(),
            seen_urls: std::collections::HashSet::new(),
            sport_total: 0,
            group_total: 0,
        }
    }

    /// Routes one channel: a recognised sport → its row; any other sports
    /// channel → "Live Sports"; else, if `groupable`, into a category row.
    fn add(&mut self, ch: Channel, groupable: bool) {
        if is_dead_stream(&ch.url) || !self.seen_urls.insert(ch.url.clone()) {
            return;
        }
        let bucket = classify_sport(&ch).or_else(|| is_sports_channel(&ch).then(|| "general".to_string()));
        if let Some(sport) = bucket {
            let row = self.by_sport.entry(sport).or_default();
            if self.sport_total < MAX_IPTV_TOTAL && row.len() < MAX_ITEMS_PER_ROW {
                register_stream(&ch);
                row.push(ch);
                self.sport_total += 1;
            }
        } else if groupable && self.group_total < MAX_USER_IPTV_TOTAL {
            let key = group_label(&ch);
            let idx = *self.group_index.entry(key.clone()).or_insert_with(|| {
                self.groups.push((key.clone(), Vec::new()));
                self.groups.len() - 1
            });
            if self.groups[idx].1.len() < MAX_ITEMS_PER_ROW {
                register_stream(&ch);
                self.groups[idx].1.push(ch);
                self.group_total += 1;
            }
        }
    }
}

fn collect_iptv(cfg: &settings::Settings) -> IptvContent {
    let region = if cfg.live_region.trim().is_empty() {
        "IN".to_string()
    } else {
        cfg.live_region.trim().to_ascii_uppercase()
    };

    let mut acc = Acc::new();

    // The user's own playlists first (their explicit choice — never crowded out).
    for url in cfg
        .iptv_playlists
        .split([',', '\n', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        for ch in fetch_playlist(url, true) {
            acc.add(ch, true);
        }
    }
    // Then the iptv-org catalog: world sports + the region's live TV.
    for ch in collect_api_channels(&region) {
        let groupable = ch.region;
        acc.add(ch, groupable);
    }

    // Region channels first within each sport.
    for list in acc.by_sport.values_mut() {
        list.sort_by_key(|c| !c.region);
    }
    kick_off_health_sweep(&acc);

    IptvContent {
        by_sport: acc.by_sport,
        groups: acc.groups,
    }
}

/// Category/group-title → a tidy Title-Case row label.
fn group_label(ch: &Channel) -> String {
    let g = clean_name(&ch.group);
    if g.is_empty() {
        return "Live TV".to_string();
    }
    g.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_sports_channel(ch: &Channel) -> bool {
    ch.categories.iter().any(|c| c.eq_ignore_ascii_case("sports"))
        || contains_word(&ch.group.to_lowercase(), "sport")
        || contains_word(&ch.group.to_lowercase(), "sports")
}

fn fetch_playlist(url: &str, user_supplied: bool) -> Vec<Channel> {
    {
        let cache = IPTV_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, chans)) = cache.get(url) {
            if at.elapsed() < IPTV_TTL {
                return chans.clone();
            }
        }
    }
    if user_supplied {
        // User playlists are untrusted — SSRF-guard (but allow public http,
        // which most IPTV playlists use).
        if crate::addons::validate_stream_url(url).is_err() {
            return Vec::new();
        }
    }
    let text = match http_get(url) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let chans = parse_m3u(&text);
    IPTV_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(url.to_string(), (Instant::now(), chans.clone()));
    chans
}

fn http_get(url: &str) -> Result<String, String> {
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    resp.text().map_err(|e| e.to_string())
}

/// Fetches an XMLTV guide, transparently gunzipping `.xml.gz` bodies (EPG guides
/// are very commonly gzipped).
fn http_get_xml(url: &str) -> Result<String, String> {
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let bytes = resp.bytes().map_err(|e| e.to_string())?;
    Ok(decode_body(&bytes))
}

/// Decodes a fetched body to text, transparently gunzipping gzip (magic 1f 8b).
fn decode_body(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        use std::io::Read;
        let mut out = String::new();
        if flate2::read::GzDecoder::new(bytes).read_to_string(&mut out).is_ok() {
            return out;
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Parses an M3U/M3U8 playlist into channels, honouring the `#EXTVLCOPT`
/// referrer/user-agent hints iptv-org attaches to streams that need them.
fn parse_m3u(text: &str) -> Vec<Channel> {
    let mut out = Vec::new();
    let mut name = String::new();
    let mut group = String::new();
    let mut logo: Option<String> = None;
    let mut tvg_id: Option<String> = None;
    let mut referrer: Option<String> = None;
    let mut user_agent: Option<String> = None;
    let mut pending = false;

    for line in text.lines() {
        let line = line.trim();
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            name = info.rsplit_once(',').map(|(_, n)| n.trim()).unwrap_or("").to_string();
            group = attr(info, "group-title").unwrap_or_default();
            logo = attr(info, "tvg-logo").filter(|s| s.starts_with("http"));
            // Feed suffixes ("Foo.us@SD") aren't in XMLTV — strip to the base id.
            tvg_id = attr(info, "tvg-id")
                .filter(|s| !s.is_empty())
                .map(|s| s.split('@').next().unwrap_or(&s).to_string());
            referrer = None;
            user_agent = None;
            pending = true;
        } else if let Some(g) = line.strip_prefix("#EXTGRP:") {
            if group.is_empty() {
                group = g.trim().to_string();
            }
        } else if let Some(v) = line.strip_prefix("#EXTVLCOPT:http-referrer=") {
            referrer = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("#EXTVLCOPT:http-user-agent=") {
            user_agent = Some(v.trim().to_string());
        } else if !line.is_empty() && !line.starts_with('#') && pending {
            if line.starts_with("http") && !name.is_empty() {
                out.push(Channel {
                    name: std::mem::take(&mut name),
                    group: std::mem::take(&mut group),
                    logo: logo.take(),
                    url: line.to_string(),
                    referrer: referrer.take(),
                    user_agent: user_agent.take(),
                    categories: Vec::new(),
                    country: None,
                    tvg_id: tvg_id.take(),
                    region: false,
                });
            }
            pending = false;
        }
    }
    out
}

/// Extracts a `key="value"` attribute from an `#EXTINF` line.
fn attr(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = line.find(&needle)? + needle.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

/// Strips trailing quality/status tags iptv-org appends — "(1080p)",
/// "[Geo-blocked]", "[Not 24/7]" — for cleaner titles and matching.
fn clean_name(name: &str) -> String {
    let mut s = name.trim();
    loop {
        let t = s.trim_end();
        let trimmed = t
            .strip_suffix(')')
            .and_then(|_| t.rfind('(').map(|i| &t[..i]))
            .or_else(|| t.strip_suffix(']').and_then(|_| t.rfind('[').map(|i| &t[..i])));
        match trimmed {
            Some(shorter) if shorter.trim() != s => s = shorter.trim(),
            _ => break,
        }
    }
    let out = s.trim();
    if out.is_empty() { name.trim().to_string() } else { out.to_string() }
}

/// Whole-word substring test: `kw` must sit on non-alphanumeric boundaries, so
/// "a sports" matches "A Sports" but not "America Sports", and "espn" doesn't
/// match "espndeportes". Multi-word keywords ("formula 1") work too.
fn contains_word(hay: &str, kw: &str) -> bool {
    if kw.is_empty() {
        return false;
    }
    let bytes = hay.as_bytes();
    let mut from = 0;
    while let Some(pos) = hay[from..].find(kw) {
        let i = from + pos;
        let end = i + kw.len();
        let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let after_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = i + 1;
    }
    false
}

/// Which specific sport a channel belongs to (first whole-word keyword match, in
/// seed order). The catch-all "general" and "news" buckets are never returned
/// here — a sports channel with no specific match falls to "general" upstream.
fn classify_sport(ch: &Channel) -> Option<String> {
    let hay = format!("{} {}", clean_name(&ch.name), ch.group).to_lowercase();
    SEED.iter()
        .filter(|s| s.id != "general" && s.id != "news")
        .find(|s| s.keywords.iter().any(|k| contains_word(&hay, &k.to_lowercase())))
        .map(|s| s.id.clone())
}

// ---- reachability sweep ----

/// A stream known to be dead (checked recently and unreachable).
fn is_dead_stream(url: &str) -> bool {
    STREAM_HEALTH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(url)
        .is_some_and(|(at, alive)| !alive && at.elapsed() < Duration::from_secs(3600))
}

/// After rows are built, probe the shown streams we haven't checked lately (in
/// the background, bounded) so a later load can drop the dead ones. Never blocks
/// the current load, and only one sweep runs at a time.
fn kick_off_health_sweep(acc: &Acc) {
    use std::sync::atomic::Ordering;
    if SWEEP_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut urls: Vec<String> = Vec::new();
    {
        let health = STREAM_HEALTH.lock().unwrap_or_else(|e| e.into_inner());
        let mut consider = |ch: &Channel| {
            let stale = health
                .get(&ch.url)
                .map_or(true, |(at, _)| at.elapsed() > Duration::from_secs(3600));
            if stale && urls.len() < 240 {
                urls.push(ch.url.clone());
            }
        };
        for list in acc.by_sport.values() {
            list.iter().for_each(&mut consider);
        }
        for (_, list) in &acc.groups {
            list.iter().for_each(&mut consider);
        }
    }
    if urls.is_empty() {
        SWEEP_RUNNING.store(false, Ordering::SeqCst);
        return;
    }
    std::thread::spawn(move || {
        let chunks = urls.chunks(16).map(<[String]>::to_vec).collect::<Vec<_>>();
        for chunk in chunks {
            std::thread::scope(|scope| {
                for url in &chunk {
                    scope.spawn(move || {
                        let alive = stream_reachable(url);
                        STREAM_HEALTH
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(url.clone(), (Instant::now(), alive));
                    });
                }
            });
        }
        SWEEP_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// A conservative liveness probe: only clearly-gone streams are pruned. Any HTTP
/// response counts as reachable EXCEPT 404/410 (gone) — a 403/401 usually just
/// means the stream needs a referrer/user-agent, which mpv supplies at play
/// time, so we keep it. A connection error/timeout = dead.
fn stream_reachable(url: &str) -> bool {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(6))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("VLC/3.0.20 LibVLC/3.0.20")
        .build()
    {
        Ok(c) => c,
        Err(_) => return true, // can't probe → don't prune
    };
    match client.get(url).send() {
        Ok(r) => !matches!(r.status().as_u16(), 404 | 410),
        Err(_) => false,
    }
}

/// Stable non-cryptographic key for a stream URL (FNV-1a 64), used as its id so
/// the same channel keeps the same id across refreshes.
fn stream_key(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn register_stream(ch: &Channel) {
    let key = stream_key(&ch.url);
    IPTV_STREAMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            key,
            StreamInfo {
                url: ch.url.clone(),
                referrer: ch.referrer.clone(),
                user_agent: ch.user_agent.clone(),
                title: ch.name.clone(),
            },
        );
}

fn iptv_item(ch: &Channel) -> ContentItem {
    ContentItem {
        id: format!("live:iptv:{}", stream_key(&ch.url)),
        kind: Kind::Live,
        title: clean_name(&ch.name),
        art: ch.logo.clone(),
        action: Action::Play,
        note: None,
    }
}

/// Plays the channel we resolved for a fixture (`live:match:<eventId>`).
fn launch_match(event_id: &str) -> Result<(), String> {
    let info = MATCH_REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(event_id)
        .cloned()
        .ok_or("this match isn't on a channel we can play right now")?;
    match info.target {
        PlayTarget::Iptv(key) => launch_iptv(&key),
        PlayTarget::Yt(video) => launch_youtube(&video),
    }
}

fn launch_iptv(key: &str) -> Result<(), String> {
    let info = IPTV_STREAMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        .cloned()
        .ok_or("this channel is no longer available — reopen Live")?;
    let mode = settings::STORE.get().enhance;
    let profile = upscale::resolve(mode, &info.url);
    let mut meta = launcher::PlayerMeta::new(if info.title.is_empty() {
        "Live TV".to_string()
    } else {
        info.title.clone()
    })
    .live();
    meta.referrer = info.referrer.clone();
    meta.user_agent = info.user_agent.clone();
    launcher::play_video(&info.url, &profile, mode, "live-tv", None, None, Some(&meta))
}

// ---- fixture → stream matching ----

/// A channel that could be carrying a fixture, indexed for matching.
struct MatchChannel {
    name: String,
    tvg_id: Option<String>,
    target: PlayTarget,
    region: bool,
}

/// The resolver: the live channels we could tune to, plus any EPG guide, so a
/// fixture can be matched to the channel currently (or soon) showing it.
struct Matcher {
    channels: Vec<MatchChannel>,
    /// tvg-id → programmes (merged across EPG sources).
    epg: HashMap<String, Vec<Programme>>,
}

/// A resolved carrier for a fixture.
struct Resolved {
    channel: String,
    target: PlayTarget,
    /// The EPG says this programme is airing right now (strongest "it's on now").
    epg_now: bool,
}

impl Matcher {
    fn build(cfg: &settings::Settings, iptv: &IptvContent, yt: &HashMap<String, Vec<ContentItem>>) -> Self {
        let mut channels = Vec::new();
        let mut push_ch = |ch: &Channel| {
            channels.push(MatchChannel {
                name: clean_name(&ch.name),
                tvg_id: ch.tvg_id.clone(),
                target: PlayTarget::Iptv(stream_key(&ch.url)),
                region: ch.region,
            });
        };
        for list in iptv.by_sport.values() {
            list.iter().for_each(&mut push_ch);
        }
        for (_, list) in &iptv.groups {
            list.iter().for_each(&mut push_ch);
        }
        // YouTube-live streams are carriers too (id = live:yt:<video>).
        for items in yt.values() {
            for it in items {
                if let Some(video) = it.id.strip_prefix("live:yt:") {
                    channels.push(MatchChannel {
                        name: it.title.clone(),
                        tvg_id: None,
                        target: PlayTarget::Yt(video.to_string()),
                        region: false,
                    });
                }
            }
        }

        let now = now_epoch();
        let mut epg: HashMap<String, Vec<Programme>> = HashMap::new();
        for url in cfg
            .epg_urls
            .split([',', '\n', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            for (id, mut progs) in fetch_epg(url, now) {
                epg.entry(id).or_default().append(&mut progs);
            }
        }
        Self { channels, epg }
    }

    /// Best carrier for a fixture, if we can resolve one confidently.
    fn resolve(&self, ev: &Value, start: i64, now: i64) -> Option<Resolved> {
        let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let (home, away, league, event) =
            (s("strHomeTeam"), s("strAwayTeam"), s("strLeague"), s("strEvent"));

        // Layer 1 — EPG: a channel whose programme at kickoff names the fixture.
        let mut best: Option<(i32, &MatchChannel, bool)> = None;
        for ch in &self.channels {
            let Some(id) = &ch.tvg_id else { continue };
            let Some(progs) = self.epg.get(id) else { continue };
            let prog = progs
                .iter()
                .find(|p| p.start <= start && start < p.stop)
                .or_else(|| progs.iter().find(|p| p.start <= now && now < p.stop));
            if let Some(p) = prog {
                let score = fixture_programme_match(&p.title, home, away, league, event);
                if score > 0 {
                    let total = score + i32::from(ch.region);
                    if best.as_ref().map_or(true, |(b, _, _)| total > *b) {
                        best = Some((total, ch, p.start <= now && now < p.stop));
                    }
                }
            }
        }
        if let Some((_, ch, epg_now)) = best {
            return Some(Resolved {
                channel: ch.name.clone(),
                target: ch.target.clone(),
                epg_now,
            });
        }

        // Layer 2 — the fixture's own broadcaster field → a channel by name.
        let station = s("strTVStation");
        if !station.is_empty() {
            for st in station.split([',', '/']).map(str::trim).filter(|s| s.len() >= 3) {
                if let Some(ch) = self
                    .channels
                    .iter()
                    .filter(|ch| station_matches(&ch.name, st))
                    .max_by_key(|ch| i32::from(ch.region))
                {
                    return Some(Resolved {
                        channel: ch.name.clone(),
                        target: ch.target.clone(),
                        epg_now: false,
                    });
                }
            }
        }
        None
    }
}

/// Does the programme title name this fixture? Requires both teams (strong), or
/// the full event name — so we never claim a wrong match.
fn fixture_programme_match(title: &str, home: &str, away: &str, league: &str, event: &str) -> i32 {
    let pt = title.to_lowercase();
    if event.len() >= 8 && pt.contains(&event.to_lowercase()) {
        return 5;
    }
    let mut score = 0;
    if name_hit(&pt, home) {
        score += 2;
    }
    if name_hit(&pt, away) {
        score += 2;
    }
    if score >= 4 {
        return score;
    }
    if score >= 2 && !league.is_empty() && name_hit(&pt, league) {
        return score + 1;
    }
    0
}

/// Whether `hay` contains `name` — the whole (multi-word) name, or its longest
/// significant word (so "India" hits, "Los Angeles Lakers" hits on "lakers").
fn name_hit(hay: &str, name: &str) -> bool {
    let n = name.trim().to_lowercase();
    if n.len() >= 4 && hay.contains(&n) {
        return true;
    }
    n.split_whitespace()
        .filter(|w| w.len() >= 4)
        .max_by_key(|w| w.len())
        .is_some_and(|w| contains_word(hay, w))
}

/// A broadcaster name (from TheSportsDB) matching a channel name, both cleaned.
fn station_matches(channel: &str, station: &str) -> bool {
    let c = channel.to_lowercase();
    let s = station.trim().to_lowercase();
    if s.len() < 3 {
        return false;
    }
    c.contains(&s) || s.contains(&c) || {
        // Significant-word overlap (≥2 shared words ≥4 chars).
        let cw: std::collections::HashSet<&str> =
            c.split_whitespace().filter(|w| w.len() >= 4).collect();
        s.split_whitespace()
            .filter(|w| w.len() >= 4)
            .filter(|w| cw.contains(w))
            .count()
            >= 2
    }
}

// ---- EPG (XMLTV) ----

/// Fetches & parses an XMLTV guide (cached), keeping only programmes within a
/// window around `now`. Returns tvg-id → programmes.
fn fetch_epg(url: &str, now: i64) -> HashMap<String, Vec<Programme>> {
    {
        let cache = EPG_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, map)) = cache.get(url) {
            if at.elapsed() < Duration::from_secs(3600) {
                return map.clone();
            }
        }
    }
    if crate::addons::validate_stream_url(url).is_err() {
        return HashMap::new();
    }
    let map = match http_get_xml(url) {
        Ok(xml) => parse_xmltv(&xml, now),
        Err(_) => HashMap::new(),
    };
    EPG_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(url.to_string(), (Instant::now(), map.clone()));
    map
}

/// Parses XMLTV `<programme>` entries into tvg-id → programmes, keeping only
/// those airing within roughly [-12h, +36h] of now to bound memory.
fn parse_xmltv(xml: &str, now: i64) -> HashMap<String, Vec<Programme>> {
    let (lo, hi) = (now - 12 * 3600, now + 36 * 3600);
    let mut map: HashMap<String, Vec<Programme>> = HashMap::new();
    for chunk in xml.split("<programme").skip(1) {
        let Some(head_end) = chunk.find('>') else { continue };
        let head = &chunk[..head_end];
        let (Some(channel), Some(start), Some(stop)) = (
            attr(head, "channel"),
            attr(head, "start").and_then(|s| parse_xmltv_time(&s)),
            attr(head, "stop").and_then(|s| parse_xmltv_time(&s)),
        ) else {
            continue;
        };
        if stop < lo || start > hi {
            continue;
        }
        let body = &chunk[head_end..];
        let Some(title) = tag_text(body, "title") else { continue };
        map.entry(channel).or_default().push(Programme { start, stop, title });
    }
    map
}

/// Extracts the text of the first `<tag …>text</tag>` in `s`.
fn tag_text(s: &str, tag: &str) -> Option<String> {
    let open = s.find(&format!("<{tag}"))?;
    let gt = s[open..].find('>')? + open + 1;
    let close = s[gt..].find(&format!("</{tag}>"))? + gt;
    let raw = s[gt..close].trim();
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\""),
    )
}

/// Parses an XMLTV timestamp "YYYYMMDDHHMMSS +0000" (offset optional) to epoch.
fn parse_xmltv_time(s: &str) -> Option<i64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).take(14).collect();
    if digits.len() < 14 {
        return None;
    }
    let p = |a: usize, b: usize| digits[a..b].parse::<i64>().ok();
    let base = days_from_civil(p(0, 4)?, p(4, 6)?, p(6, 8)?) * 86400
        + p(8, 10)? * 3600
        + p(10, 12)? * 60
        + p(12, 14)?;
    // Optional " +HHMM" / "-HHMM" offset → convert local to UTC.
    let offset = s
        .rsplit(' ')
        .next()
        .filter(|t| t.len() == 5 && (t.starts_with('+') || t.starts_with('-')))
        .and_then(|t| {
            let sign = if t.starts_with('-') { -1 } else { 1 };
            let h: i64 = t[1..3].parse().ok()?;
            let m: i64 = t[3..5].parse().ok()?;
            Some(sign * (h * 3600 + m * 60))
        })
        .unwrap_or(0);
    Some(base - offset)
}

// ---- schedule (TheSportsDB) ----

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days from 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's
/// algorithm) — enough to turn TheSportsDB's UTC timestamps into epoch seconds
/// without a date crate.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parses "YYYY-MM-DDTHH:MM:SS" (assumed UTC) to epoch seconds.
fn parse_iso_utc(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(['T', ' '])?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.trim().parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next().unwrap_or("0").parse().ok()?;
    let se: i64 = t
        .next()
        .map(|x| x.get(0..2).unwrap_or("0"))
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    Some(days_from_civil(y, mo, da) * 86400 + h * 3600 + mi * 60 + se)
}

/// UTC date string `d` days from now, e.g. "2026-07-11".
fn utc_date_offset(days: i64) -> String {
    let secs = now_epoch() + days * 86400;
    let z = secs.div_euclid(86400);
    // inverse of days_from_civil
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

const FINISHED_STATUSES: &[&str] = &[
    "FT", "AET", "AOT", "AP", "Match Finished", "Finished", "Cancelled", "CANC",
    "Postponed", "PPD", "Abandoned", "Abd", "AWD", "WO",
];

fn is_finished(status: &str) -> bool {
    let s = status.trim();
    FINISHED_STATUSES
        .iter()
        .any(|f| s.eq_ignore_ascii_case(f))
}

/// Fetches today + tomorrow's fixtures for each followed sport that maps to a
/// TheSportsDB sport, classified into live/upcoming schedule cards.
fn collect_schedule(followed: &[SportSeed], matcher: &Matcher) -> HashMap<String, Vec<ContentItem>> {
    let now = now_epoch();
    std::thread::scope(|scope| {
        let jobs: Vec<_> = followed
            .iter()
            .filter_map(|s| s.tsdb.as_ref().map(|t| (s.id.clone(), t.clone())))
            .take(MAX_SCHED_SPORTS)
            .map(|(sport_id, tsdb)| {
                scope.spawn(move || (sport_id, schedule_items(&tsdb, now, matcher)))
            })
            .collect();
        let mut out = HashMap::new();
        for job in jobs {
            if let Ok((sport_id, items)) = job.join() {
                if !items.is_empty() {
                    out.insert(sport_id, items);
                }
            }
        }
        out
    })
}

fn schedule_items(tsdb_sport: &str, now: i64, matcher: &Matcher) -> Vec<ContentItem> {
    let mut items: Vec<(i64, bool, ContentItem)> = Vec::new();
    for ev in fetch_events(tsdb_sport) {
        if let Some(item) = event_item(&ev, now, matcher) {
            items.push(item);
        }
    }
    // Playable-now first, then soonest upcoming.
    items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    items.truncate(MAX_SCHED_PER_SPORT);
    items.into_iter().map(|(_, _, it)| it).collect()
}

/// Raw events for today + tomorrow for a TheSportsDB sport, cached.
fn fetch_events(tsdb_sport: &str) -> Vec<Value> {
    {
        let cache = SCHED_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, events)) = cache.get(tsdb_sport) {
            if at.elapsed() < SCHED_TTL {
                return events.clone();
            }
        }
    }
    let mut all = Vec::new();
    for day in 0..2 {
        let url = format!(
            "https://www.thesportsdb.com/api/v1/json/{TSDB_KEY}/eventsday.php?d={}&s={}",
            utc_date_offset(day),
            tsdb_sport.replace(' ', "%20"),
        );
        if let Ok(text) = http_get(&url) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(events) = v.get("events").and_then(|e| e.as_array()) {
                    all.extend(events.iter().cloned());
                }
            }
        }
    }
    SCHED_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(tsdb_sport.to_string(), (Instant::now(), all.clone()));
    all
}

/// Turns one TheSportsDB event into a schedule card, or None if finished /
/// out of the time window. When a carrying channel resolves, a live fixture
/// becomes directly playable (`live:match:`); an upcoming one is annotated with
/// its channel and auto-upgrades once that channel is live. Returns
/// (start_epoch, playable_now, item) for sorting.
fn event_item(ev: &Value, now: i64, matcher: &Matcher) -> Option<(i64, bool, ContentItem)> {
    let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let status = s("strStatus").unwrap_or("");
    if is_finished(status) {
        return None;
    }
    let start = s("strTimestamp")
        .and_then(parse_iso_utc)
        .or_else(|| match (s("dateEvent"), s("strTime")) {
            (Some(d), Some(t)) => parse_iso_utc(&format!("{d}T{t}")),
            (Some(d), None) => parse_iso_utc(&format!("{d}T00:00:00")),
            _ => None,
        })?;
    let diff = start - now;
    let live = diff <= 0 && diff > -LIVE_WINDOW_SECS;
    if !live && !(diff > 0 && diff < UPCOMING_HORIZON_SECS) {
        return None;
    }
    let title = s("strEvent")
        .map(str::to_string)
        .or_else(|| match (s("strHomeTeam"), s("strAwayTeam")) {
            (Some(h), Some(a)) => Some(format!("{h} vs {a}")),
            _ => None,
        })?;
    let event_id = s("idEvent").unwrap_or("0").to_string();
    let art = s("strThumb").or_else(|| s("strPoster")).map(str::to_string);
    let matched = matcher.resolve(ev, start, now);

    // Playable now = we have a carrier AND (the fixture is live OR the EPG says
    // its programme is airing right now).
    if let Some(m) = &matched {
        if live || m.epg_now {
            MATCH_REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).insert(
                event_id.clone(),
                MatchInfo {
                    target: m.target.clone(),
                    channel: m.channel.clone(),
                },
            );
            return Some((
                start,
                true, // playable now → sorts to the front of its row
                ContentItem {
                    id: format!("live:match:{event_id}"),
                    kind: Kind::Live,
                    title,
                    art,
                    action: Action::Play,
                    note: Some(format!("On {}", m.channel)),
                },
            ));
        }
    }

    // Otherwise informational. A resolved-but-future fixture shows its carrier;
    // it flips to playable on a later refresh once that channel goes live. Live
    // (but unresolved) fixtures still sort ahead of upcoming ones.
    let state = if live { "live" } else { "up" };
    let note = matched.map(|m| format!("On {}", m.channel));
    Some((
        start,
        live,
        ContentItem {
            id: format!("live:sched:{state}:{start}:{event_id}"),
            kind: Kind::Live,
            title,
            art,
            action: Action::None,
            note,
        },
    ))
}

// ---- rows ----

fn build_rows(
    followed: &[SportSeed],
    mut yt_by_sport: HashMap<String, Vec<ContentItem>>,
    iptv: IptvContent,
    mut sched_by_sport: HashMap<String, Vec<ContentItem>>,
) -> Vec<Row> {
    let IptvContent {
        mut by_sport,
        groups,
    } = iptv;
    let iptv_by_sport = &mut by_sport;
    let mut rows = Vec::new();

    // "Live now": every currently-live YouTube broadcast across sports.
    let mut live_now: Vec<ContentItem> = followed
        .iter()
        .flat_map(|s| yt_by_sport.get(&s.id).cloned().unwrap_or_default())
        .collect();
    live_now.truncate(MAX_ITEMS_PER_ROW);
    if !live_now.is_empty() {
        rows.push(Row {
            title: "Live now".to_string(),
            items: live_now,
        });
    }

    // One row per followed sport: playable streams first (its live YouTube
    // broadcasts, then its channels), then the schedule — live fixtures we
    // hold no stream for, then what's coming up.
    for sport in followed {
        let mut items: Vec<ContentItem> = yt_by_sport.remove(&sport.id).unwrap_or_default();
        for ch in iptv_by_sport.remove(&sport.id).unwrap_or_default() {
            items.push(iptv_item(&ch));
        }
        for sched in sched_by_sport.remove(&sport.id).unwrap_or_default() {
            items.push(sched);
        }
        items.truncate(MAX_ITEMS_PER_ROW);
        if !items.is_empty() {
            rows.push(Row {
                title: sport.label.clone(),
                items,
            });
        }
    }

    // Region + user live TV, in full, grouped by category. Rows whose title
    // matches a sport row above merge into it (Registry merges by title).
    for (group, channels) in groups {
        let items: Vec<ContentItem> = channels.iter().map(iptv_item).collect();
        if !items.is_empty() {
            rows.push(Row { title: group, items });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_m3u_with_headers_and_group() {
        let text = "#EXTM3U\n\
            #EXTINF:-1 tvg-logo=\"http://x/y.png\" group-title=\"Sports\",Willow Cricket\n\
            #EXTVLCOPT:http-referrer=http://ref/\n\
            #EXTVLCOPT:http-user-agent=Mozilla/5.0\n\
            http://stream/cricket.m3u8\n\
            #EXTINF:-1,No URL Follows\n\
            #EXTINF:-1 group-title=\"News\",WION\n\
            http://stream/wion.m3u8\n";
        let chans = parse_m3u(text);
        assert_eq!(chans.len(), 2);
        assert_eq!(chans[0].name, "Willow Cricket");
        assert_eq!(chans[0].group, "Sports");
        assert_eq!(chans[0].referrer.as_deref(), Some("http://ref/"));
        assert_eq!(chans[0].user_agent.as_deref(), Some("Mozilla/5.0"));
        assert_eq!(chans[0].logo.as_deref(), Some("http://x/y.png"));
        assert_eq!(chans[1].name, "WION");
    }

    fn chan(name: &str, group: &str, url: &str) -> Channel {
        Channel {
            name: name.into(),
            group: group.into(),
            logo: None,
            url: url.into(),
            referrer: None,
            user_agent: None,
            categories: Vec::new(),
            country: None,
            tvg_id: None,
            region: false,
        }
    }

    #[test]
    fn classifies_by_keyword_in_seed_order() {
        assert_eq!(
            classify_sport(&chan("Willow Cricket HD", "Sports", "http://a")).as_deref(),
            Some("cricket")
        );
        // A generic sports channel is not a specific sport…
        let generic = chan("America Sports", "Sports", "http://b");
        assert_eq!(classify_sport(&generic), None);
        // …but is recognised as a sports channel (→ "Live Sports" upstream).
        assert!(is_sports_channel(&generic));
        // A movie channel is neither.
        let movie = chan("Random Movie Channel", "Movies", "http://c");
        assert_eq!(classify_sport(&movie), None);
        assert!(!is_sports_channel(&movie));
    }

    #[test]
    fn stream_key_is_stable_and_roundtrips_through_registry() {
        let mut ch = chan("Test", "Sports", "http://stream/x.m3u8");
        ch.referrer = Some("http://ref".into());
        let k1 = stream_key(&ch.url);
        let k2 = stream_key(&ch.url);
        assert_eq!(k1, k2);
        register_stream(&ch);
        let got = IPTV_STREAMS.lock().unwrap().get(&k1).cloned().unwrap();
        assert_eq!(got.url, "http://stream/x.m3u8");
        assert_eq!(got.referrer.as_deref(), Some("http://ref"));
    }

    #[test]
    #[ignore = "hits the network (iptv-org public catalog)"]
    fn collect_iptv_finds_sports_channels() {
        let cfg = settings::Settings {
            live_region: "IN".to_string(),
            ..Default::default()
        };
        let by_sport = collect_iptv(&cfg).by_sport;
        let total: usize = by_sport.values().map(|v| v.len()).sum();
        eprintln!("classified {total} channels across {} sports:", by_sport.len());
        for (id, list) in &by_sport {
            eprintln!("  {id}: {} (e.g. {:?})", list.len(), list.first().map(|c| &c.name));
        }
        assert!(total > 0, "expected the public catalog to yield sports channels");
        // Every classified channel must be registered for playback.
        let reg = IPTV_STREAMS.lock().unwrap();
        for list in by_sport.values() {
            for ch in list {
                assert!(reg.contains_key(&stream_key(&ch.url)));
            }
        }
    }

    #[test]
    fn word_boundary_matching_avoids_false_positives() {
        assert!(contains_word("a sports hd", "a sports"));
        assert!(!contains_word("america sports", "a sports")); // the fix
        assert!(contains_word("sky sports f1", "sky sports"));
        assert!(!contains_word("espndeportes", "espn"));
        assert!(contains_word("formula 1 channel", "formula 1"));
    }

    #[test]
    fn clean_name_strips_quality_and_geo_tags() {
        assert_eq!(clean_name("Willow Cricket HD (1080p)"), "Willow Cricket HD");
        assert_eq!(clean_name("F1 Channel (1080p) [Geo-blocked]"), "F1 Channel");
        assert_eq!(clean_name("Star Sports 1"), "Star Sports 1");
        assert_eq!(clean_name("(720p)"), "(720p)"); // never blank out the title
    }

    #[test]
    fn group_label_is_title_cased() {
        assert_eq!(group_label(&chan("X", "news", "u")), "News");
        assert_eq!(group_label(&chan("X", "entertainment", "u")), "Entertainment");
        assert_eq!(group_label(&chan("X", "", "u")), "Live TV");
    }

    #[test]
    fn parses_utc_timestamps_to_epoch() {
        assert_eq!(parse_iso_utc("1970-01-01T00:00:00"), Some(0));
        assert_eq!(parse_iso_utc("2021-01-01T00:00:00"), Some(1_609_459_200));
        assert_eq!(parse_iso_utc("2026-07-11T16:00:00"), Some(1_783_785_600));
        assert_eq!(parse_iso_utc("2021-01-01 00:00:00"), Some(1_609_459_200));
        assert_eq!(parse_iso_utc("garbage"), None);
    }

    #[test]
    fn utc_date_offset_roundtrips_through_parser() {
        let today = utc_date_offset(0);
        let epoch = parse_iso_utc(&format!("{today}T00:00:00")).unwrap();
        // Same civil day as now (both truncated to whole UTC days).
        assert_eq!(epoch / 86400, now_epoch().div_euclid(86400));
    }

    fn empty_matcher() -> Matcher {
        Matcher { channels: Vec::new(), epg: HashMap::new() }
    }

    #[test]
    fn event_item_classifies_live_upcoming_and_skips_finished() {
        let now = 1_000_000_000;
        let m = empty_matcher();
        let ev = |ts: i64, status: &str| {
            serde_json::json!({
                "idEvent": "1", "strEvent": "A vs B",
                "strTimestamp": super_ts(ts), "strStatus": status,
            })
        };
        // upcoming (in 1h) — informational (no carrier)
        let up = event_item(&ev(now + 3600, "NS"), now, &m).unwrap();
        assert!(!up.1 && up.2.id.starts_with("live:sched:up:"));
        // live (started 30m ago, status says in play) — no carrier, still sched
        let live = event_item(&ev(now - 1800, "2H"), now, &m).unwrap();
        assert!(live.1 && live.2.id.starts_with("live:sched:live:"));
        // finished → skipped
        assert!(event_item(&ev(now - 1800, "FT"), now, &m).is_none());
        // far future → skipped
        assert!(event_item(&ev(now + 5 * 86400, "NS"), now, &m).is_none());
    }

    #[test]
    fn live_fixture_with_matching_channel_becomes_playable() {
        let now = 2_000_000_000;
        // A live channel carrying the match, joined by EPG.
        let m = Matcher {
            channels: vec![MatchChannel {
                name: "Star Sports 1".into(),
                tvg_id: Some("StarSports1.in".into()),
                target: PlayTarget::Iptv("abc123".into()),
                region: true,
            }],
            epg: HashMap::from([(
                "StarSports1.in".to_string(),
                vec![Programme {
                    start: now - 600,
                    stop: now + 3600,
                    title: "Live Cricket: India vs Australia".into(),
                }],
            )]),
        };
        let ev = serde_json::json!({
            "idEvent": "999", "strEvent": "India vs Australia",
            "strHomeTeam": "India", "strAwayTeam": "Australia", "strLeague": "ODI",
            "strTimestamp": super_ts(now - 600), "strStatus": "2H",
        });
        let (_, playable, item) = event_item(&ev, now, &m).unwrap();
        assert!(playable);
        assert_eq!(item.id, "live:match:999");
        assert!(item.action == Action::Play);
        assert_eq!(item.note.as_deref(), Some("On Star Sports 1"));
        // …and it's registered so launch can resolve the channel.
        let reg = MATCH_REGISTRY.lock().unwrap();
        assert!(reg.contains_key("999"));
    }

    #[test]
    fn future_fixture_with_channel_is_annotated_not_playable() {
        let now = 2_100_000_000;
        let m = Matcher {
            channels: vec![MatchChannel {
                name: "Sky Sports Cricket".into(),
                tvg_id: None,
                target: PlayTarget::Iptv("k".into()),
                region: false,
            }],
            epg: HashMap::new(),
        };
        // Broadcaster field resolves the channel; fixture is 2h away.
        let ev = serde_json::json!({
            "idEvent": "42", "strEvent": "Eng vs Aus",
            "strTVStation": "Sky Sports Cricket",
            "strTimestamp": super_ts(now + 7200), "strStatus": "NS",
        });
        let (_, playable, item) = event_item(&ev, now, &m).unwrap();
        assert!(!playable);
        assert!(item.id.starts_with("live:sched:up:"));
        assert_eq!(item.note.as_deref(), Some("On Sky Sports Cricket"));
    }

    #[test]
    fn fixture_programme_matching_is_strict() {
        // both teams present → match
        assert!(fixture_programme_match("cricket: india vs australia", "India", "Australia", "", "") >= 4);
        // only one team → no match
        assert_eq!(fixture_programme_match("india news tonight", "India", "Australia", "", ""), 0);
        // exact event name → match
        assert!(fixture_programme_match("the ashes 1st test", "", "", "", "The Ashes 1st Test") >= 5);
    }

    #[test]
    fn parses_xmltv() {
        let now = parse_xmltv_time("20260712140000 +0000").unwrap();
        let xml = format!(
            "<tv><programme start=\"20260712133000 +0000\" stop=\"20260712173000 +0000\" \
             channel=\"StarSports1.in\"><title lang=\"en\">India vs Australia</title></programme></tv>"
        );
        let map = parse_xmltv(&xml, now);
        let progs = map.get("StarSports1.in").unwrap();
        assert_eq!(progs.len(), 1);
        assert_eq!(progs[0].title, "India vs Australia");
        assert!(progs[0].start <= now && now < progs[0].stop);
    }

    #[test]
    fn xmltv_time_applies_offset() {
        // 14:00 at +0100 is 13:00 UTC.
        let utc = parse_xmltv_time("20260712140000 +0000").unwrap();
        let plus1 = parse_xmltv_time("20260712140000 +0100").unwrap();
        assert_eq!(utc - plus1, 3600);
    }

    #[test]
    fn decode_body_gunzips_and_passes_plain_through() {
        use std::io::Write;
        let xml = "<tv><programme>hi</programme></tv>";
        // plain passes through
        assert_eq!(decode_body(xml.as_bytes()), xml);
        // gzip is transparently decoded
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(xml.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        assert_eq!(gz[0], 0x1f);
        assert_eq!(decode_body(&gz), xml);
    }

    #[test]
    fn station_matching_and_name_hit() {
        assert!(station_matches("Sky Sports Cricket HD", "Sky Sports Cricket"));
        assert!(!station_matches("Random Movie Channel", "Sky Sports Cricket"));
        assert!(name_hit("los angeles lakers vs celtics", "Los Angeles Lakers"));
        assert!(!name_hit("celtics tonight", "Los Angeles Lakers"));
    }

    // Build an ISO-UTC string for an epoch, for the test above.
    fn super_ts(epoch: i64) -> String {
        let days = epoch.div_euclid(86400);
        let rem = epoch.rem_euclid(86400);
        let date = {
            let z = days + 719468;
            let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
            let doe = z - era * 146097;
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
            let y = yoe + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            let y = if m <= 2 { y + 1 } else { y };
            format!("{y:04}-{m:02}-{d:02}")
        };
        format!("{date}T{:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
    }

    #[test]
    #[ignore = "hits the network (a real public playlist)"]
    fn user_playlist_channels_all_show_grouped() {
        let cfg = settings::Settings {
            live_region: "IN".to_string(),
            iptv_playlists:
                "https://raw.githubusercontent.com/Free-TV/IPTV/master/playlist.m3u8".to_string(),
            ..Default::default()
        };
        let content = collect_iptv(&cfg);
        let total: usize = content.groups.iter().map(|(_, v)| v.len()).sum();
        eprintln!("user playlist → {} groups, {total} channels:", content.groups.len());
        for (g, v) in content.groups.iter().take(10) {
            eprintln!("  {g}: {} (e.g. {:?})", v.len(), v.first().map(|c| clean_name(&c.name)));
        }
        assert!(total > 0, "a pasted playlist's channels must all show, sports or not");
    }

    #[test]
    #[ignore = "hits the network (TheSportsDB)"]
    fn schedule_fetches_real_fixtures() {
        let now = now_epoch();
        let m = empty_matcher();
        let mut any = 0;
        for sport in ["Cricket", "Soccer", "Motorsport", "Tennis"] {
            let items = schedule_items(sport, now, &m);
            eprintln!("{sport}: {} fixtures", items.len());
            for it in items.iter().take(3) {
                eprintln!("   [{}] {}", it.id.split(':').nth(2).unwrap_or("?"), it.title);
                assert!(it.id.starts_with("live:sched:") || it.id.starts_with("live:match:"));
                assert_eq!(it.kind, Kind::Live);
            }
            any += items.len();
        }
        assert!(any > 0, "expected at least one fixture across four sports today/tomorrow");
    }

    #[test]
    fn followed_defaults_to_all_then_filters() {
        assert_eq!(followed_sports("").len(), SEED.len());
        let only = followed_sports("cricket, f1");
        let ids: Vec<&str> = only.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"cricket"));
        assert!(ids.contains(&"f1"));
        assert!(!ids.contains(&"tennis"));
    }
}
