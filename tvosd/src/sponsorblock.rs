use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const TTL: Duration = Duration::from_secs(24 * 60 * 60);
const ALLOWED: &[&str] = &[
    "sponsor",
    "selfpromo",
    "interaction",
    "intro",
    "outro",
    "preview",
    "filler",
];
static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<Segment>)>>> =
    LazyLock::new(Mutex::default);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub category: String,
}

pub fn segments(video_id: &str) -> Vec<Segment> {
    let settings = crate::settings::STORE.get();
    if !settings.sponsorblock_enabled || video_id.trim().is_empty() {
        return Vec::new();
    }
    let categories = settings
        .sponsorblock_categories
        .split([',', ' '])
        .map(str::trim)
        .filter(|value| ALLOWED.contains(value))
        .collect::<Vec<_>>();
    let categories = if categories.is_empty() {
        vec!["sponsor"]
    } else {
        categories
    };
    let key = format!("{}:{}", video_id, categories.join(","));
    if let Some((at, value)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if at.elapsed() < TTL {
            return value.clone();
        }
    }
    let hash = crate::install::sha256_hex(video_id.as_bytes());
    let prefix = &hash[..4];
    let category_json =
        serde_json::to_string(&categories).unwrap_or_else(|_| "[\"sponsor\"]".into());
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };
    let response = client
        .get(format!(
            "https://sponsor.ajay.app/api/skipSegments/{prefix}"
        ))
        .query(&[
            ("categories", category_json.as_str()),
            ("actionTypes", "[\"skip\"]"),
        ])
        .send();
    let found = response
        .ok()
        .filter(|response| response.status().is_success())
        .and_then(|response| response.json::<serde_json::Value>().ok())
        .map(|value| parse_payload(video_id, &categories, &value))
        .unwrap_or_default();
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), found.clone()));
    found
}

fn parse_payload(video_id: &str, categories: &[&str], values: &serde_json::Value) -> Vec<Segment> {
    let mut found = Vec::new();
    let entry = values.as_array().and_then(|entries| {
        entries
            .iter()
            .find(|entry| entry.get("videoID").and_then(|value| value.as_str()) == Some(video_id))
    });
    if let Some(items) = entry
        .and_then(|entry| entry.get("segments"))
        .and_then(|value| value.as_array())
    {
        for item in items {
            let range = item.get("segment").and_then(|value| value.as_array());
            let start = range
                .and_then(|value| value.first())
                .and_then(|value| value.as_f64());
            let end = range
                .and_then(|value| value.get(1))
                .and_then(|value| value.as_f64());
            let category = item.get("category").and_then(|value| value.as_str());
            if let (Some(start), Some(end), Some(category)) = (start, end, category) {
                if start >= 0.0
                    && end > start
                    && end - start < 3600.0
                    && categories.contains(&category)
                {
                    found.push(Segment {
                        start,
                        end,
                        category: category.into(),
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| a.start.total_cmp(&b.start));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_exact_valid_requested_segments() {
        let payload = serde_json::json!([
            {"videoID":"collision","segments":[{"segment":[1,2],"category":"sponsor"}]},
            {"videoID":"wanted","segments":[
                {"segment":[20,30],"category":"sponsor"},
                {"segment":[3,7],"category":"intro"},
                {"segment":[9,4],"category":"sponsor"},
                {"segment":["bad",12],"category":"sponsor"}
            ]}
        ]);
        let segments = parse_payload("wanted", &["sponsor"], &payload);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].start, 20.0);
        assert_eq!(segments[0].end, 30.0);
    }

    #[test]
    fn malformed_payload_is_non_fatal() {
        assert!(
            parse_payload("wanted", &["sponsor"], &serde_json::json!({"oops": true})).is_empty()
        );
    }
}
