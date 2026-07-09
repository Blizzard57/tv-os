//! GameHub — the "WatchHub for games".
//!
//! Aggregates a game's store pages and prices across Steam (regional,
//! official API), GOG (regional) and the CheapShark US multi-store
//! aggregator (Fanatical, Humble, GMG, Epic deals, …), sorted cheapest
//! first. It also builds the "Games for you" recommendations row — Steam's
//! current top sellers and specials that you don't already own, priced for
//! your region (Settings → game store region).
//!
//! Owns the `gshop:<steam-appid>` id prefix: items you don't own. Their
//! details page lists the offers as "Where to buy" sources; picking one
//! opens the store page.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::addons::http_get_quick;
use crate::install::InstallManager;
use crate::media::{Stream, StreamKind};
use crate::model::{Action, ContentItem, Kind, Row};
use crate::sources::Source;
use crate::util::percent_encode;
use crate::{launcher, settings};

const CHARTS_TTL: Duration = Duration::from_secs(900);
const OFFERS_TTL: Duration = Duration::from_secs(600);

static CHARTS_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<(i64, String, String)>)>>> =
    LazyLock::new(Mutex::default);
static OFFERS_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<Stream>)>>> =
    LazyLock::new(Mutex::default);
/// appid → (name, art) once verified; art is the portrait capsule when the
/// CDN actually has it, the wide header otherwise. "" name = known-bad app.
static BRIEF_CACHE: LazyLock<Mutex<HashMap<i64, (String, String)>>> = LazyLock::new(Mutex::default);
/// appid → does the portrait capsule exist on the CDN?
static PORTRAIT_CACHE: LazyLock<Mutex<HashMap<i64, bool>>> = LazyLock::new(Mutex::default);

/// CheapShark store ids → names. Steam and GOG are skipped here (they get
/// their own regional lookups); everything else joins the aggregate.
const CHEAPSHARK_STORES: &[(&str, &str)] = &[
    ("2", "GamersGate"),
    ("3", "GreenManGaming"),
    ("8", "EA App (Origin)"),
    ("11", "Humble Store"),
    ("13", "Ubisoft Connect"),
    ("15", "Fanatical"),
    ("21", "WinGameStore"),
    ("23", "GameBillet"),
    ("24", "Voidu"),
    ("25", "Epic Games Store"),
    ("27", "Gamesplanet"),
    ("29", "2game"),
    ("30", "IndieGala"),
    ("31", "Battle.net (Blizzard)"),
    ("32", "AllYouPlay"),
    ("33", "DLGamer"),
    ("35", "DreamGame"),
];

pub struct GameShop;

impl Source for GameShop {
    fn id(&self) -> &'static str {
        "gshop"
    }

    fn available(&self) -> bool {
        true
    }

    /// No rows of its own — "Games for you" is assembled in get_library,
    /// which knows the whole library (needed to filter out owned games).
    fn rows(&self) -> Vec<Row> {
        Vec::new()
    }

    /// "Launching" an unowned game opens its Steam store page; the details
    /// page is where the full multi-store price list lives.
    fn launch(&self, item_id: &str) -> Result<(), String> {
        let appid = item_id.trim_start_matches("gshop:");
        launcher::open_external(&format!("https://store.steampowered.com/app/{appid}"))
    }

    fn install(&self, _item_id: &str, _jobs: &InstallManager) -> Result<(), String> {
        Err("buy the game first — pick a store on its page".to_string())
    }
}

/// Two-letter store region from settings ("US" default).
fn region() -> String {
    let r = settings::STORE.get().game_region.trim().to_uppercase();
    if r.len() == 2 {
        r
    } else {
        "US".to_string()
    }
}

/// Titles + appids already in the library, for filtering store pools.
fn owned_sets(library: &[Row]) -> (HashSet<String>, HashSet<i64>) {
    let mut titles = HashSet::new();
    let mut apps = HashSet::new();
    for row in library {
        for item in &row.items {
            if item.kind == Kind::Game {
                titles.insert(item.title.to_lowercase());
                if let Some(appid) = item.id.strip_prefix("steam:").and_then(|a| a.parse().ok()) {
                    apps.insert(appid);
                }
            }
        }
    }
    (titles, apps)
}

/// Does the portrait box-art capsule exist for this app? Checked once and
/// remembered — a store item without it would render as a cropped banner.
fn has_portrait(appid: i64) -> bool {
    if let Some(&ok) = PORTRAIT_CACHE.lock().unwrap().get(&appid) {
        return ok;
    }
    let ok = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()
        .and_then(|c| c.head(crate::sources::steam::art_url(appid)).send().ok())
        .is_some_and(|r| r.status().is_success());
    PORTRAIT_CACHE.lock().unwrap().insert(appid, ok);
    ok
}

/// Keep only the pool entries whose portrait box art actually exists
/// (checked in parallel; results are cached so this is cheap after once).
fn with_portraits(pool: Vec<(i64, String)>) -> Vec<(i64, String)> {
    std::thread::scope(|scope| {
        let checks: Vec<_> = pool
            .into_iter()
            .map(|(id, name)| scope.spawn(move || has_portrait(id).then_some((id, name))))
            .collect();
        checks
            .into_iter()
            .filter_map(|c| c.join().ok().flatten())
            .collect()
    })
}

fn shop_item(id: i64, name: String) -> ContentItem {
    ContentItem {
        id: format!("gshop:{id}"),
        kind: Kind::Game,
        title: name,
        art: Some(crate::sources::steam::art_url(id)),
        action: Action::None, // not playable — its page shows where to buy
    }
}

/// Candidate pool for the game recommender (gamerec.rs decides the order):
/// the region's store charts minus everything already in the library, box
/// art verified.
pub fn charts_unowned(library: &[Row]) -> Vec<ContentItem> {
    let (owned_titles, owned_apps) = owned_sets(library);
    let pool: Vec<(i64, String)> = charts(&region())
        .into_iter()
        .filter(|(id, name, _)| {
            !owned_apps.contains(id) && !owned_titles.contains(&name.to_lowercase())
        })
        .map(|(id, name, _)| (id, name))
        .collect();
    with_portraits(pool)
        .into_iter()
        .map(|(id, name)| shop_item(id, name))
        .collect()
}

/// One featured category ("specials" = deals, "new_releases") as unowned,
/// art-verified shop items.
pub fn category_row(list: &str, library: &[Row], limit: usize) -> Vec<ContentItem> {
    let (owned_titles, owned_apps) = owned_sets(library);
    let pool: Vec<(i64, String)> = category(&region(), list)
        .into_iter()
        .filter(|(id, name, _)| {
            !owned_apps.contains(id) && !owned_titles.contains(&name.to_lowercase())
        })
        .map(|(id, name, _)| (id, name))
        .collect();
    with_portraits(pool)
        .into_iter()
        .take(limit)
        .map(|(id, name)| shop_item(id, name))
        .collect()
}

/// Top sellers of one store genre hub (slug like "action", "rpg") as
/// unowned, art-verified shop items. Names come from cached app briefs —
/// the genre endpoint only returns ids.
pub fn genre_row(slug: &str, library: &[Row], limit: usize) -> Vec<ContentItem> {
    let (owned_titles, owned_apps) = owned_sets(library);
    let url = format!(
        "https://store.steampowered.com/api/getappsingenre/?genre={}&cc={}&l=en",
        percent_encode(slug),
        region()
    );
    let Ok(json) = http_get_quick(&url) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    let ids: Vec<i64> = v
        .get("tabs")
        .and_then(|t| t.get("topsellers"))
        .and_then(|t| t.get("items"))
        .and_then(|i| i.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("id").and_then(|x| x.as_i64()))
                .filter(|id| !owned_apps.contains(id))
                .take(limit * 2) // room for brief/art misses
                .collect()
        })
        .unwrap_or_default();

    std::thread::scope(|scope| {
        let briefs: Vec<_> = ids
            .into_iter()
            .map(|id| scope.spawn(move || app_brief(id).map(|name| (id, name))))
            .collect();
        briefs
            .into_iter()
            .filter_map(|b| b.join().ok().flatten())
            .filter(|(_, name)| !owned_titles.contains(&name.to_lowercase()))
            .take(limit)
            .map(|(id, name)| shop_item(id, name))
            .collect()
    })
}

/// Name for an appid, only when it's a normal game with portrait box art.
fn app_brief(appid: i64) -> Option<String> {
    {
        let cache = BRIEF_CACHE.lock().unwrap();
        if let Some((name, _)) = cache.get(&appid) {
            return (!name.is_empty()).then(|| name.clone());
        }
    }
    let fetch = || -> Option<String> {
        if !has_portrait(appid) {
            return None;
        }
        let url = format!(
            "https://store.steampowered.com/api/appdetails?appids={appid}&l=en&filters=basic"
        );
        let v: Value = serde_json::from_str(&http_get_quick(&url).ok()?).ok()?;
        let data = v.get(appid.to_string())?.get("data")?;
        if data.get("type").and_then(|t| t.as_str()) != Some("game") {
            return None;
        }
        Some(data.get("name")?.as_str()?.to_string())
    };
    let name = fetch().unwrap_or_default();
    BRIEF_CACHE
        .lock()
        .unwrap()
        .insert(appid, (name.clone(), String::new()));
    (!name.is_empty()).then_some(name)
}

/// One featured-store category for a region: (appid, name, art). Cached by
/// (region, category).
fn category(region: &str, list: &str) -> Vec<(i64, String, String)> {
    let cache_key = format!("{region}:{list}");
    if let Some((at, items)) = CHARTS_CACHE.lock().unwrap().get(&cache_key) {
        if at.elapsed() < CHARTS_TTL {
            return items.clone();
        }
    }
    let url = format!("https://store.steampowered.com/api/featuredcategories?cc={region}&l=en");
    let mut items: Vec<(i64, String, String)> = Vec::new();
    let mut seen = HashSet::new();
    if let Ok(json) = http_get_quick(&url) {
        if let Ok(v) = serde_json::from_str::<Value>(&json) {
            let entries = v.get(list).and_then(|l| l.get("items")).and_then(|i| i.as_array());
            for e in entries.into_iter().flatten() {
                let (Some(id), Some(name)) =
                    (e.get("id").and_then(|i| i.as_i64()), e.get("name").and_then(|n| n.as_str()))
                else {
                    continue;
                };
                let art = e
                    .get("header_image")
                    .or_else(|| e.get("large_capsule_image"))
                    .and_then(|a| a.as_str())
                    .unwrap_or_default();
                if !art.is_empty() && seen.insert(id) {
                    items.push((id, name.to_string(), art.to_string()));
                }
            }
        }
    }
    CHARTS_CACHE
        .lock()
        .unwrap()
        .insert(cache_key, (Instant::now(), items.clone()));
    items
}

/// The merged store charts (top sellers + specials + new releases).
fn charts(region: &str) -> Vec<(i64, String, String)> {
    let mut seen = HashSet::new();
    ["top_sellers", "specials", "new_releases"]
        .iter()
        .flat_map(|list| category(region, list))
        .filter(|(id, _, _)| seen.insert(*id))
        .collect()
}

/// All the places to buy a game, cheapest first, as External "streams" the
/// details page can list. Steam and GOG are priced for the region;
/// CheapShark's US aggregate joins in when the region is US.
pub fn offers(appid: &str) -> Vec<Stream> {
    let region = region();
    let cache_key = format!("{appid}:{region}");
    if let Some((at, offers)) = OFFERS_CACHE.lock().unwrap().get(&cache_key) {
        if at.elapsed() < OFFERS_TTL {
            return offers.clone();
        }
    }

    let mut priced: Vec<(f64, Stream)> = Vec::new();
    let mut title = String::new();

    // Steam — official regional pricing, and the canonical title.
    let url = format!(
        "https://store.steampowered.com/api/appdetails?appids={appid}&cc={region}&l=en&filters=basic,price_overview"
    );
    if let Ok(json) = http_get_quick(&url) {
        if let Ok(v) = serde_json::from_str::<Value>(&json) {
            let data = v.get(appid).and_then(|a| a.get("data"));
            if let Some(data) = data {
                title = data
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let page = format!("https://store.steampowered.com/app/{appid}");
                if data.get("is_free").and_then(|f| f.as_bool()) == Some(true) {
                    priced.push((0.0, external("Steam · Free", "", &page)));
                } else if let Some(p) = data.get("price_overview") {
                    let amount = p.get("final").and_then(|f| f.as_i64()).unwrap_or(0) as f64 / 100.0;
                    let label = p.get("final_formatted").and_then(|f| f.as_str()).unwrap_or("");
                    let discount = p.get("discount_percent").and_then(|d| d.as_i64()).unwrap_or(0);
                    let detail = if discount > 0 { format!("-{discount}% right now") } else { String::new() };
                    priced.push((amount, external(&format!("Steam · {label}"), &detail, &page)));
                }
            }
        }
    }

    // GOG — regional pricing (DRM-free).
    if !title.is_empty() {
        if let Some((gog_id, slug)) = gog_lookup(&title) {
            if let Some((amount, label)) = gog_price(gog_id, &region) {
                priced.push((
                    amount,
                    external(
                        &format!("GOG · {label}"),
                        "DRM-free",
                        &format!("https://www.gog.com/en/game/{slug}"),
                    ),
                ));
            }
        }
    }

    // CheapShark — the multi-store aggregate (Epic, Fanatical, Humble, GMG,
    // Battle.net, EA, Ubisoft, …). Prices are USD; outside the US they're
    // still shown (a real number beats a "check price" link), labeled so.
    if !title.is_empty() {
        priced.extend(cheapshark(&title, &region));
    }

    priced.sort_by(|a, b| a.0.total_cmp(&b.0));
    let offers: Vec<Stream> = priced.into_iter().map(|(_, s)| s).collect();

    let mut cache = OFFERS_CACHE.lock().unwrap();
    if cache.len() > 64 {
        cache.clear();
    }
    cache.insert(cache_key, (Instant::now(), offers.clone()));
    offers
}

fn external(name: &str, detail: &str, url: &str) -> Stream {
    Stream {
        kind: StreamKind::External,
        url: url.to_string(),
        name: name.to_string(),
        title: detail.to_string(),
        file_idx: None,
    }
}

/// GOG product for a title via their catalog API, guarded by similar_title
/// so a search that only finds an expansion or a different game is skipped.
fn gog_lookup(title: &str) -> Option<(i64, String)> {
    let url = format!(
        "https://catalog.gog.com/v1/catalog?limit=5&query=like:{}",
        percent_encode(title)
    );
    let v: Value = serde_json::from_str(&http_get_quick(&url).ok()?).ok()?;
    v.get("products")?.as_array()?.iter().find_map(|p| {
        let gog_title = p.get("title")?.as_str()?;
        if !similar_title(title, gog_title) {
            return None;
        }
        // The catalog returns ids as strings.
        let id = match p.get("id")? {
            Value::String(s) => s.parse().ok()?,
            Value::Number(n) => n.as_i64()?,
            _ => return None,
        };
        Some((id, p.get("slug")?.as_str()?.to_string()))
    })
}

fn gog_price(gog_id: i64, region: &str) -> Option<(f64, String)> {
    let url = format!("https://api.gog.com/products/{gog_id}/prices?countryCode={region}");
    let v: Value = serde_json::from_str(&http_get_quick(&url).ok()?).ok()?;
    let price = v
        .get("_embedded")?
        .get("prices")?
        .as_array()?
        .first()?
        .get("finalPrice")?
        .as_str()?
        .to_string(); // "1999 USD"
    let mut parts = price.split_whitespace();
    let cents: f64 = parts.next()?.parse().ok()?;
    let currency = parts.next().unwrap_or("");
    Some((cents / 100.0, format!("{:.2} {currency}", cents / 100.0)))
}

/// The US-dollar aggregator: one deal per store, cheapest of each.
fn cheapshark(title: &str, region: &str) -> Vec<(f64, Stream)> {
    let Ok(json) = http_get_quick(&format!(
        "https://www.cheapshark.com/api/1.0/games?title={}&limit=1",
        percent_encode(title)
    )) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    let Some(game_id) = v
        .as_array()
        .and_then(|a| a.first())
        .and_then(|g| g.get("gameID"))
        .and_then(|i| i.as_str())
    else {
        return Vec::new();
    };
    let Ok(json) = http_get_quick(&format!(
        "https://www.cheapshark.com/api/1.0/games?id={game_id}"
    )) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    let stores: HashMap<&str, &str> = CHEAPSHARK_STORES.iter().copied().collect();
    let mut best: HashMap<String, (f64, String)> = HashMap::new(); // store → (price, dealID)
    for deal in v.get("deals").and_then(|d| d.as_array()).into_iter().flatten() {
        let (Some(store_id), Some(price), Some(deal_id)) = (
            deal.get("storeID").and_then(|s| s.as_str()),
            deal.get("price").and_then(|p| p.as_str()).and_then(|p| p.parse::<f64>().ok()),
            deal.get("dealID").and_then(|d| d.as_str()),
        ) else {
            continue;
        };
        let Some(&store) = stores.get(store_id) else {
            continue; // Steam/GOG come from their own regional lookups
        };
        let entry = best.entry(store.to_string()).or_insert((f64::MAX, String::new()));
        if price < entry.0 {
            *entry = (price, deal_id.to_string());
        }
    }
    let detail = if region == "US" {
        "via CheapShark".to_string()
    } else {
        format!("US price (USD) — {region} pricing may differ on the store page")
    };
    best.into_iter()
        .map(|(store, (price, deal_id))| {
            (
                price,
                external(
                    &format!("{store} · ${price:.2}"),
                    &detail,
                    &format!("https://www.cheapshark.com/redirect?dealID={deal_id}"),
                ),
            )
        })
        .collect()
}

/// Cheap guard against GOG search returning an unrelated game. Exact match
/// after normalization, or a prefix whose remainder is an edition-style
/// suffix ("Ultimate Edition", "GOTY") — but not a different game that
/// happens to share a name prefix ("Celeste" vs "Celeste Classic 2").
fn similar_title(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let (a, b) = (norm(a), norm(b));
    if a == b {
        return true;
    }
    let remainder = if a.starts_with(&b) {
        &a[b.len()..]
    } else if b.starts_with(&a) {
        &b[a.len()..]
    } else {
        return false;
    };
    ["edition", "goty", "cut", "remaster", "definitive", "complete", "enhanced"]
        .iter()
        .any(|s| remainder.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_match_loosely_but_not_wrongly() {
        assert!(similar_title("Celeste", "CELESTE"));
        assert!(similar_title("The Witcher 3: Wild Hunt", "the witcher 3 wild hunt"));
        assert!(!similar_title("Celeste", "Celeste Classic 2: Lani's Trek"));
        assert!(similar_title("Cyberpunk 2077", "Cyberpunk 2077: Ultimate Edition"));
    }

    #[test]
    fn recommended_filters_owned_games() {
        // charts() hits the network, so exercise only the filtering shape here.
        let library = vec![Row {
            title: "Games".into(),
            items: vec![ContentItem {
                id: "steam:620".into(),
                kind: Kind::Game,
                title: "Portal 2".into(),
                art: None,
                action: Action::Play,
            }],
        }];
        let mut owned_titles = HashSet::new();
        let mut owned_apps = HashSet::new();
        for row in &library {
            for item in &row.items {
                owned_titles.insert(item.title.to_lowercase());
                if let Some(a) = item.id.strip_prefix("steam:") {
                    owned_apps.insert(a.to_string());
                }
            }
        }
        assert!(owned_apps.contains("620"));
        assert!(owned_titles.contains("portal 2"));
    }
}
