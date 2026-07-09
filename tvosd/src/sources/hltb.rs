//! HowLongToBeat lookup — best effort.
//!
//! HLTB has no official API; its site calls /api/search/<key> where the key
//! is baked into their JS bundle and changes with deployments. We scrape the
//! bundle once (cached), try the keyed endpoint, and fall back to the bare
//! endpoint. When any step fails the game page simply shows no HLTB line.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TTL: Duration = Duration::from_secs(86_400);
/// Failures (network hiccup, HLTB changing their scheme, or a title we simply
/// couldn't match) get a much shorter TTL so a transient miss isn't cached for
/// a whole day.
const FAILURE_TTL: Duration = Duration::from_secs(900);
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Option<Times>)>>> =
    LazyLock::new(Mutex::default);

/// Hours for the three standard completion styles.
#[derive(Clone, serde::Serialize)]
pub struct Times {
    pub main: f64,
    pub main_extra: f64,
    pub completionist: f64,
}

fn client() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        // Browser-ish UA + referer: the endpoint rejects obvious bots.
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36")
        .build()
        .ok()
}

pub fn lookup(title: &str) -> Option<Times> {
    let key = title.to_lowercase();
    if let Some((at, times)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        // A hit lives for the full day; a cached miss expires quickly so we
        // retry soon (the miss may have been a transient failure).
        let ttl = if times.is_some() { TTL } else { FAILURE_TTL };
        if at.elapsed() < ttl {
            return times.clone();
        }
    }
    let times = search(title);
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), times.clone()));
    times
}

fn search(title: &str) -> Option<Times> {
    let http = client()?;
    let body = json!({
        "searchType": "games",
        "searchTerms": title.split_whitespace().collect::<Vec<_>>(),
        "searchPage": 1,
        "size": 1,
        "searchOptions": {
            "games": {
                "userId": 0, "platform": "", "sortCategory": "popular",
                "rangeCategory": "main",
                "rangeTime": { "min": null, "max": null },
                "gameplay": { "perspective": "", "flow": "", "genre": "" },
                "rangeYear": { "min": "", "max": "" },
                "modifier": ""
            },
            "users": { "sortCategory": "postcount" },
            "lists": { "sortCategory": "follows" },
            "filter": "", "sort": 0, "randomizer": 0
        }
    });
    // HLTB's current flow: GET /api/bleed/init hands out a short-lived token
    // + honeypot pair, POST /api/bleed runs the search with them. They churn
    // this scheme regularly; every failure path just means "no HLTB line".
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let init: Value = http
        .get(format!("https://howlongtobeat.com/api/bleed/init?t={now}"))
        .header("Referer", "https://howlongtobeat.com/")
        .send()
        .ok()?
        .json()
        .ok()?;
    let token = init["token"].as_str()?;
    let hp_key = init["hpKey"].as_str().unwrap_or_default();
    let hp_val = init["hpVal"].as_str().unwrap_or_default();
    let mut payload = body;
    if !hp_key.is_empty() {
        payload[hp_key] = json!(hp_val);
    }
    payload["useCache"] = json!(true);

    for url in [
        "https://howlongtobeat.com/api/bleed",
        "https://howlongtobeat.com/api/search",
    ] {
        let Ok(res) = http
            .post(url)
            .header("Referer", "https://howlongtobeat.com/")
            .header("Origin", "https://howlongtobeat.com")
            .header("x-auth-token", token)
            .header("x-hp-key", hp_key)
            .header("x-hp-val", hp_val)
            .json(&payload)
            .send()
        else {
            continue;
        };
        if !res.status().is_success() {
            continue;
        }
        let Ok(v) = res.json::<Value>() else { continue };
        let Some(game) = v["data"].as_array().and_then(|d| d.first()) else {
            return None; // API reachable, game unknown
        };
        let hours = |k: &str| game[k].as_f64().unwrap_or(0.0) / 3600.0;
        return Some(Times {
            main: hours("comp_main"),
            main_extra: hours("comp_plus"),
            completionist: hours("comp_100"),
        });
    }
    None
}
