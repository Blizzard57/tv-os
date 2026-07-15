//! Twitch creator source using the official Helix API for discovery and the
//! public channel URL for playback through mpv/yt-dlp.

use serde_json::Value;

use crate::media::Meta;
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::{launcher, settings, upscale, util};

pub struct Twitch;

impl Source for Twitch {
    fn id(&self) -> &'static str {
        "twitch"
    }

    fn available(&self) -> bool {
        let s = settings::STORE.get();
        !s.twitch_client_id.is_empty() && !s.twitch_token.is_empty()
    }

    fn rows(&self) -> Vec<Row> {
        creator_rows()
    }

    fn launch(&self, item_id: &str) -> Result<(), String> {
        let mut parts = item_id.splitn(3, ':');
        let _ = parts.next();
        let kind = parts.next().unwrap_or("live");
        let value = parts
            .next()
            .ok_or_else(|| format!("bad Twitch id '{item_id}'"))?;
        let url = if kind == "vod" {
            format!("https://www.twitch.tv/videos/{value}")
        } else {
            format!("https://www.twitch.tv/{value}")
        };
        let mode = settings::STORE.get().enhance;
        let profile = upscale::resolve(mode, "twitch");
        let meta = if kind == "live" {
            let mut meta = launcher::PlayerMeta::new(value).live();
            meta.visual_class = crate::upscale::VisualClass::LiveAction;
            meta
        } else {
            launcher::PlayerMeta::new(value)
        };
        launcher::play_video(
            &url,
            &profile,
            mode,
            "Twitch",
            Some(item_id),
            Some(item_id),
            Some(&meta),
        )
    }
}

pub fn creator_rows() -> Vec<Row> {
    let Some(user_id) = current_user_id() else {
        return Vec::new();
    };
    let followed = helix(&format!("streams/followed?user_id={user_id}&first=100"))
        .map(|v| stream_items(&v))
        .unwrap_or_default();
    let popular = helix("streams?first=24")
        .map(|v| stream_items(&v))
        .unwrap_or_default();
    let mut rows = Vec::new();
    if !followed.is_empty() {
        rows.push(Row {
            title: "Twitch · Followed live".into(),
            items: followed,
        });
    }
    if !popular.is_empty() {
        rows.push(Row {
            title: "Twitch · Live creators".into(),
            items: popular,
        });
    }
    rows
}

pub fn search(query: &str) -> Vec<ContentItem> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let path = format!(
        "search/channels?first=20&live_only=false&query={}",
        util::percent_encode(query)
    );
    let Some(value) = helix(&path) else {
        return Vec::new();
    };
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let login = v.get("broadcaster_login")?.as_str()?;
            let live = v.get("is_live").and_then(Value::as_bool).unwrap_or(false);
            Some(ContentItem {
                id: format!("twitch:{}:{login}", if live { "live" } else { "channel" }),
                kind: Kind::Video,
                title: v
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or(login)
                    .to_string(),
                art: v
                    .get("thumbnail_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                action: Action::Play,
                note: Some(
                    if live {
                        "LIVE · Twitch"
                    } else {
                        "Twitch creator"
                    }
                    .to_string(),
                ),
            })
        })
        .collect()
}

pub fn video_meta(item_id: &str) -> Option<Meta> {
    let value = item_id.split(':').nth(2)?;
    let channel = helix(&format!("users?login={}", util::percent_encode(value)))?;
    let user = channel.get("data")?.as_array()?.first()?;
    Some(Meta {
        id: item_id.to_string(),
        kind: "video".into(),
        title: user.get("display_name")?.as_str()?.to_string(),
        description: user
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        poster: user
            .get("profile_image_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        background: user
            .get("offline_image_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        release_info: Some("Twitch creator".into()),
        ..Default::default()
    })
}

pub fn status() -> serde_json::Value {
    let s = settings::STORE.get();
    if s.twitch_client_id.is_empty() || s.twitch_token.is_empty() {
        return serde_json::json!({"connected": false, "detail": "Add a Twitch client id and user token"});
    }
    match current_user_id() {
        Some(_) => serde_json::json!({"connected": true, "detail": "Connected to Twitch"}),
        None => {
            serde_json::json!({"connected": false, "detail": "Twitch credentials were rejected"})
        }
    }
}

fn current_user_id() -> Option<String> {
    helix("users")?
        .get("data")?
        .as_array()?
        .first()?
        .get("id")?
        .as_str()
        .map(str::to_owned)
}

fn stream_items(value: &Value) -> Vec<ContentItem> {
    value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let login = v.get("user_login")?.as_str()?;
            let art = v
                .get("thumbnail_url")
                .and_then(Value::as_str)
                .map(|s| s.replace("{width}", "640").replace("{height}", "360"));
            Some(ContentItem {
                id: format!("twitch:live:{login}"),
                kind: Kind::Video,
                title: v
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| v.get("user_name").and_then(Value::as_str).unwrap_or(login))
                    .to_string(),
                art,
                action: Action::Play,
                note: Some(format!(
                    "LIVE · {} · {} viewers",
                    v.get("user_name").and_then(Value::as_str).unwrap_or(login),
                    v.get("viewer_count").and_then(Value::as_i64).unwrap_or(0)
                )),
            })
        })
        .collect()
}

fn helix(path: &str) -> Option<Value> {
    let s = settings::STORE.get();
    if s.twitch_client_id.is_empty() || s.twitch_token.is_empty() {
        return None;
    }
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?
        .get(format!("https://api.twitch.tv/helix/{path}"))
        .header("Client-Id", s.twitch_client_id)
        .bearer_auth(s.twitch_token)
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .ok()
}
