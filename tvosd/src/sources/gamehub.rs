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

use serde::{Deserialize, Serialize};
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
/// Cache of Steam store-search top-seller appid lists, keyed by request URL.
static TOPSELLER_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<i64>)>>> =
    LazyLock::new(Mutex::default);
static OFFERS_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<StoreOffer>)>>> =
    LazyLock::new(Mutex::default);
static RATES_CACHE: LazyLock<Mutex<Option<(Instant, String, HashMap<String, f64>)>>> =
    LazyLock::new(Mutex::default);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Money {
    pub amount: f64,
    pub currency: String,
    pub formatted: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PriceVerification {
    Regional,
    EstimatedForeign,
    NativeForeign,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreOffer {
    pub store: String,
    pub native: Money,
    pub display: Option<Money>,
    pub country: String,
    pub verification: PriceVerification,
    pub discount_percent: i64,
    pub url: String,
    pub drm: Vec<String>,
    pub platforms: Vec<String>,
    pub checked_at: i64,
    pub conversion_date: Option<String>,
    pub best_regional: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PricingContext {
    pub country: String,
    pub currency: String,
    pub region_mode: String,
    pub detected_country: String,
    pub detected_currency: String,
}
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

pub fn pricing_context(
    country_override: Option<&str>,
    currency_override: Option<&str>,
) -> PricingContext {
    let settings = settings::STORE.get();
    let detected_country = locale_country();
    let detected_currency = currency_for_country(&detected_country).to_string();
    let manual = settings.game_region_mode == "manual";
    let country = country_override
        .and_then(valid_country)
        .or_else(|| manual.then(|| settings.game_region.clone()))
        .and_then(|v| valid_country(&v))
        .unwrap_or_else(|| detected_country.clone());
    let currency = currency_override
        .and_then(valid_currency)
        .or_else(|| valid_currency(&settings.game_currency))
        .unwrap_or_else(|| currency_for_country(&country).into());
    PricingContext {
        country,
        currency,
        region_mode: if country_override.is_some() {
            "request".into()
        } else if manual {
            "manual".into()
        } else {
            "auto".into()
        },
        detected_country,
        detected_currency,
    }
}

fn region() -> String {
    pricing_context(None, None).country
}
fn valid_country(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 2 && value.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| value.to_ascii_uppercase())
}
fn valid_currency(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 3 && value.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| value.to_ascii_uppercase())
}
fn locale_country() -> String {
    ["LC_ALL", "LC_MONETARY", "LANG"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find_map(|raw| {
            let clean = raw
                .split('.')
                .next()
                .unwrap_or(&raw)
                .split('@')
                .next()
                .unwrap_or(&raw);
            clean
                .split_once('_')
                .or_else(|| clean.split_once('-'))
                .and_then(|(_, country)| valid_country(country))
        })
        .unwrap_or_else(|| "US".into())
}
fn currency_for_country(country: &str) -> &'static str {
    match country {
        "AT" | "BE" | "CY" | "DE" | "EE" | "ES" | "FI" | "FR" | "GR" | "HR" | "IE" | "IT"
        | "LT" | "LU" | "LV" | "MT" | "NL" | "PT" | "SI" | "SK" => "EUR",
        "GB" => "GBP",
        "IN" => "INR",
        "JP" => "JPY",
        "CA" => "CAD",
        "AU" => "AUD",
        "NZ" => "NZD",
        "CH" => "CHF",
        "SE" => "SEK",
        "NO" => "NOK",
        "DK" => "DKK",
        "PL" => "PLN",
        "CZ" => "CZK",
        "HU" => "HUF",
        "RO" => "RON",
        "BR" => "BRL",
        "MX" => "MXN",
        "KR" => "KRW",
        "CN" => "CNY",
        "ZA" => "ZAR",
        "TR" => "TRY",
        _ => "USD",
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
    if let Some(&ok) = PORTRAIT_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&appid)
    {
        return ok;
    }
    let ok = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        .user_agent(concat!("tvos/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()
        .and_then(|c| c.head(crate::sources::steam::art_url(appid)).send().ok())
        .is_some_and(|r| r.status().is_success());
    PORTRAIT_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(appid, ok);
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
        note: None,
    }
}

/// Steam store-search top-seller appids (most popular first), optionally within
/// a store tag (genre). The paginated search endpoint returns ~100 per call —
/// the reason we use it over `getappsingenre`/`featuredcategories`, which cap at
/// 10 and leave nothing after a big library's owned titles are filtered out.
/// Cached by URL (15-min TTL).
fn topseller_ids(region: &str, tag: Option<u32>) -> Vec<i64> {
    let tag_q = tag.map(|t| format!("&tags={t}")).unwrap_or_default();
    let url = format!(
        "https://store.steampowered.com/search/results/?filter=topsellers{tag_q}\
         &start=0&count=100&cc={region}&l=en&json=1&infinite=1"
    );
    if let Some((at, ids)) = TOPSELLER_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&url)
    {
        if at.elapsed() < CHARTS_TTL {
            return ids.clone();
        }
    }
    let ids = search_appids(&url);
    if !ids.is_empty() {
        TOPSELLER_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url, (Instant::now(), ids.clone()));
    }
    ids
}

/// Pulls the `data-ds-appid` ids out of a store-search results page (the search
/// endpoint returns rendered HTML rows inside JSON). Bundles carry a
/// comma-separated list — we take the first id. De-duped, order preserved.
fn search_appids(url: &str) -> Vec<i64> {
    let Ok(json) = http_get_quick(url) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&json) else {
        return Vec::new();
    };
    let html = v
        .get("results_html")
        .and_then(|h| h.as_str())
        .unwrap_or_default();
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for chunk in html.split("data-ds-appid=\"").skip(1) {
        let Some(end) = chunk.find('"') else { continue };
        let first = chunk[..end].split(',').next().unwrap_or_default();
        if let Ok(id) = first.parse::<i64>() {
            if seen.insert(id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// appids → shop items: fetches each title's brief (name) in parallel, keeping
/// only real games whose portrait box art exists and that aren't owned by
/// title. Order is preserved (popularity).
fn resolve_shop_items(ids: Vec<i64>, owned_titles: &HashSet<String>) -> Vec<ContentItem> {
    std::thread::scope(|scope| {
        let briefs: Vec<_> = ids
            .into_iter()
            .map(|id| scope.spawn(move || app_brief(id).map(|name| (id, name))))
            .collect();
        briefs
            .into_iter()
            .filter_map(|b| b.join().ok().flatten())
            .filter(|(_, name)| !owned_titles.contains(&name.to_lowercase()))
            .map(|(id, name)| shop_item(id, name))
            .collect()
    })
}

/// Candidate pool for the game recommender (gamerec.rs decides the order): the
/// region's global top sellers minus everything already owned, box-art
/// verified. Falls back to the featured charts if search is unavailable.
pub fn recommend_pool(library: &[Row]) -> Vec<ContentItem> {
    let (owned_titles, owned_apps) = owned_sets(library);
    let region = region();
    let mut ids: Vec<i64> = topseller_ids(&region, None)
        .into_iter()
        .filter(|id| !owned_apps.contains(id))
        .take(60)
        .collect();
    if ids.is_empty() {
        // Fallback: the small featured charts (10-ish each) still beats nothing.
        ids = charts(&region)
            .into_iter()
            .filter(|(id, _, _)| !owned_apps.contains(id))
            .map(|(id, _, _)| id)
            .collect();
    }
    resolve_shop_items(ids, &owned_titles)
}

/// One featured category ("specials" = deals, "new_releases") as unowned,
/// art-verified shop items. Currently unused — the home is recommendation-only
/// (no deals/new-release browse rows) — but kept for a future browse setting.
#[allow(dead_code)]
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

/// Top sellers within one store genre `tag` (e.g. Action=19, RPG=122) as
/// unowned, art-verified shop items, popularity order. Uses the paginated store
/// search (100/call) so a big library still leaves plenty after owned-filtering.
pub fn genre_row(tag: u32, library: &[Row], limit: usize) -> Vec<ContentItem> {
    let (owned_titles, owned_apps) = owned_sets(library);
    let ids: Vec<i64> = topseller_ids(&region(), Some(tag))
        .into_iter()
        .filter(|id| !owned_apps.contains(id))
        .take(limit * 3) // room for brief/portrait/owned-title misses
        .collect();
    resolve_shop_items(ids, &owned_titles)
        .into_iter()
        .take(limit)
        .collect()
}

/// Name for an appid, only when it's a normal game with portrait box art.
fn app_brief(appid: i64) -> Option<String> {
    {
        let cache = BRIEF_CACHE.lock().unwrap_or_else(|e| e.into_inner());
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
    if let Some((at, items)) = CHARTS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
    {
        if at.elapsed() < CHARTS_TTL {
            return items.clone();
        }
    }
    let url = format!("https://store.steampowered.com/api/featuredcategories?cc={region}&l=en");
    let mut items: Vec<(i64, String, String)> = Vec::new();
    let mut seen = HashSet::new();
    if let Ok(json) = http_get_quick(&url) {
        if let Ok(v) = serde_json::from_str::<Value>(&json) {
            let entries = v
                .get(list)
                .and_then(|l| l.get("items"))
                .and_then(|i| i.as_array());
            for e in entries.into_iter().flatten() {
                let (Some(id), Some(name)) = (
                    e.get("id").and_then(|i| i.as_i64()),
                    e.get("name").and_then(|n| n.as_str()),
                ) else {
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
    offers_structured(appid, None, None)
        .into_iter()
        .map(|offer| {
            let price = offer.display.as_ref().unwrap_or(&offer.native);
            let estimate = match offer.verification {
                PriceVerification::Regional => {
                    if offer.discount_percent > 0 {
                        format!(
                            "-{}% · verified for {}",
                            offer.discount_percent, offer.country
                        )
                    } else {
                        format!("Verified for {}", offer.country)
                    }
                }
                PriceVerification::EstimatedForeign => format!(
                    "Estimate from {} · checkout may differ",
                    offer.native.formatted
                ),
                PriceVerification::NativeForeign => format!("Foreign price · checkout may differ"),
            };
            external(
                &format!(
                    "{} · {}{}",
                    offer.store,
                    if offer.verification == PriceVerification::Regional {
                        ""
                    } else {
                        "≈ "
                    },
                    price.formatted
                ),
                &estimate,
                &offer.url,
            )
        })
        .collect()
}

pub fn offers_structured(
    appid: &str,
    country_override: Option<&str>,
    currency_override: Option<&str>,
) -> Vec<StoreOffer> {
    let context = pricing_context(country_override, currency_override);
    let region = context.country.clone();
    let cache_key = format!(
        "{appid}:{}:{}:{}",
        region,
        context.currency,
        pricing_revision()
    );
    if let Some((at, offers)) = OFFERS_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cache_key)
    {
        if at.elapsed() < OFFERS_TTL {
            return offers.clone();
        }
    }

    let mut priced: Vec<StoreOffer> = Vec::new();
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
                    priced.push(regional_offer(
                        "Steam",
                        0.0,
                        &context.currency,
                        "Free",
                        &region,
                        0,
                        page,
                    ));
                } else if let Some(p) = data.get("price_overview") {
                    let amount =
                        p.get("final").and_then(|f| f.as_i64()).unwrap_or(0) as f64 / 100.0;
                    let label = p
                        .get("final_formatted")
                        .and_then(|f| f.as_str())
                        .unwrap_or("");
                    let discount = p
                        .get("discount_percent")
                        .and_then(|d| d.as_i64())
                        .unwrap_or(0);
                    let currency = p
                        .get("currency")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&context.currency);
                    priced.push(regional_offer(
                        "Steam", amount, currency, label, &region, discount, page,
                    ));
                }
            }
        }
    }

    // GOG and CheapShark both only need the title + region, so once Steam has
    // handed those over run the two branches concurrently rather than back to
    // back (each is several serial HTTP calls).
    if !title.is_empty() {
        let (gog, itad, cheap) = std::thread::scope(|scope| {
            // GOG — regional pricing (DRM-free).
            let gog = scope.spawn(|| {
                let (gog_id, slug) = gog_lookup(&title)?;
                let (amount, label) = gog_price(gog_id, &region)?;
                let currency = label.split_whitespace().last().unwrap_or(&context.currency);
                Some(regional_offer(
                    "GOG",
                    amount,
                    currency,
                    &label,
                    &region,
                    0,
                    format!("https://www.gog.com/en/game/{slug}"),
                ))
            });
            let itad = scope.spawn(|| isthereanydeal(appid, &region, &context.currency));
            let cheap = scope.spawn(|| cheapshark_offers(&title, &context));
            (
                gog.join().ok().flatten(),
                itad.join().unwrap_or_default(),
                cheap.join().unwrap_or_default(),
            )
        });
        if let Some(gog) = gog {
            priced.push(gog);
        }
        priced.extend(itad);
        priced.extend(cheap);
    }

    let rates = exchange_rates();
    for offer in &mut priced {
        if offer.native.currency == context.currency {
            offer.display = Some(offer.native.clone());
        } else if let Some((date, amount)) = convert(
            offer.native.amount,
            &offer.native.currency,
            &context.currency,
            rates.as_ref(),
        ) {
            offer.display = Some(money(amount, &context.currency, ""));
            offer.conversion_date = Some(date);
        }
    }
    let mut seen_stores = HashSet::new();
    priced.retain(|offer| seen_stores.insert(offer.store.to_ascii_lowercase().replace(' ', "")));
    priced.sort_by(|a, b| {
        let group = |offer: &StoreOffer| {
            if offer.verification == PriceVerification::Regional {
                0
            } else {
                1
            }
        };
        group(a)
            .cmp(&group(b))
            .then_with(|| match (a.display.as_ref(), b.display.as_ref()) {
                (Some(a), Some(b)) => a.amount.total_cmp(&b.amount),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            })
    });
    if let Some(best) = priced
        .iter_mut()
        .find(|offer| offer.verification == PriceVerification::Regional)
    {
        best.best_regional = true;
    }

    let mut cache = OFFERS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 64 {
        cache.clear();
    }
    cache.insert(cache_key, (Instant::now(), priced.clone()));
    priced
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

fn money(amount: f64, currency: &str, formatted: &str) -> Money {
    Money {
        amount,
        currency: currency.to_ascii_uppercase(),
        formatted: if formatted.trim().is_empty() {
            format!("{amount:.2} {}", currency.to_ascii_uppercase())
        } else {
            formatted.into()
        },
    }
}
fn regional_offer(
    store: &str,
    amount: f64,
    currency: &str,
    formatted: &str,
    country: &str,
    discount: i64,
    url: String,
) -> StoreOffer {
    StoreOffer {
        store: store.into(),
        native: money(amount, currency, formatted),
        display: None,
        country: country.into(),
        verification: PriceVerification::Regional,
        discount_percent: discount,
        url,
        drm: vec![],
        platforms: vec!["Linux".into(), "Windows".into()],
        checked_at: unix_now(),
        conversion_date: None,
        best_regional: false,
    }
}
fn pricing_revision() -> String {
    let settings = settings::STORE.get();
    format!(
        "{}:{}:{}:{}",
        settings.game_region_mode,
        settings.game_region,
        settings.game_currency,
        !settings.itad_api_key.is_empty()
    )
}
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn cheapshark_offers(title: &str, context: &PricingContext) -> Vec<StoreOffer> {
    cheapshark(title, &context.country)
        .into_iter()
        .map(|(amount, stream)| StoreOffer {
            store: stream
                .name
                .split('·')
                .next()
                .unwrap_or("Store")
                .trim()
                .into(),
            native: money(amount, "USD", &format!("${amount:.2}")),
            display: None,
            country: "US".into(),
            verification: if context.country == "US" {
                PriceVerification::Regional
            } else {
                PriceVerification::EstimatedForeign
            },
            discount_percent: 0,
            url: stream.url,
            drm: vec![],
            platforms: vec!["Windows".into()],
            checked_at: unix_now(),
            conversion_date: None,
            best_regional: false,
        })
        .collect()
}

fn isthereanydeal(appid: &str, country: &str, currency: &str) -> Vec<StoreOffer> {
    let key = settings::STORE.get().itad_api_key;
    if key.is_empty() {
        return Vec::new();
    }
    let lookup = format!(
        "https://api.isthereanydeal.com/lookup/id/shop/61/{appid}/v1?key={}",
        percent_encode(&key)
    );
    let Some(id) = http_get_quick(&lookup)
        .ok()
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
        .and_then(|value| {
            value
                .get("game")
                .and_then(|v| v.get("id"))
                .or_else(|| value.get("id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
    else {
        return Vec::new();
    };
    let url = format!(
        "https://api.isthereanydeal.com/games/prices/v3?key={}&country={}&currency={}",
        percent_encode(&key),
        country,
        currency
    );
    let response = (|| -> Option<Value> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .ok()?;
        let body = serde_json::to_vec(&serde_json::json!([id])).ok()?;
        let text = client
            .post(url)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .ok()?
            .text()
            .ok()?;
        serde_json::from_str(&text).ok()
    })();
    response
        .and_then(|v| v.as_array().and_then(|a| a.first()).cloned())
        .and_then(|v| v.get("deals").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|deal| {
            let store = deal.get("shop")?.get("name")?.as_str()?;
            let price = deal.get("price")?;
            let amount = price.get("amount")?.as_f64()?;
            let curr = price.get("currency")?.as_str()?;
            let url = deal.get("url")?.as_str()?;
            Some(StoreOffer {
                store: store.into(),
                native: money(amount, curr, ""),
                display: None,
                country: country.into(),
                verification: PriceVerification::Regional,
                discount_percent: deal.get("cut").and_then(|v| v.as_i64()).unwrap_or(0),
                url: url.into(),
                drm: deal
                    .get("drm")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.get("name").and_then(|v| v.as_str()).map(str::to_string))
                    .collect(),
                platforms: deal
                    .get("platforms")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.get("name").and_then(|v| v.as_str()).map(str::to_string))
                    .collect(),
                checked_at: unix_now(),
                conversion_date: None,
                best_regional: false,
            })
        })
        .collect()
}

fn exchange_rates() -> Option<(String, HashMap<String, f64>)> {
    if let Some((at, date, rates)) = &*RATES_CACHE.lock().unwrap_or_else(|e| e.into_inner()) {
        if at.elapsed() < Duration::from_secs(24 * 3600) {
            return Some((date.clone(), rates.clone()));
        }
    }
    let xml =
        http_get_quick("https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml").ok()?;
    let date = between(&xml, "time='", "'").or_else(|| between(&xml, "time=\"", "\""))?;
    let mut rates = HashMap::from([("EUR".into(), 1.0)]);
    for chunk in xml.split("<Cube ") {
        let currency =
            between(chunk, "currency='", "'").or_else(|| between(chunk, "currency=\"", "\""));
        let rate = between(chunk, "rate='", "'")
            .or_else(|| between(chunk, "rate=\"", "\""))
            .and_then(|v| v.parse::<f64>().ok());
        if let (Some(currency), Some(rate)) = (currency, rate) {
            rates.insert(currency, rate);
        }
    }
    *RATES_CACHE.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((Instant::now(), date.clone(), rates.clone()));
    Some((date, rates))
}
fn convert(
    amount: f64,
    from: &str,
    to: &str,
    rates: Option<&(String, HashMap<String, f64>)>,
) -> Option<(String, f64)> {
    let (date, rates) = rates?;
    let from = *rates.get(&from.to_ascii_uppercase())?;
    let to = *rates.get(&to.to_ascii_uppercase())?;
    Some((date.clone(), amount / from * to))
}
fn between(text: &str, start: &str, end: &str) -> Option<String> {
    let after = text.split_once(start)?.1;
    Some(after.split_once(end)?.0.to_string())
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
    for deal in v
        .get("deals")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
    {
        let (Some(store_id), Some(price), Some(deal_id)) = (
            deal.get("storeID").and_then(|s| s.as_str()),
            deal.get("price")
                .and_then(|p| p.as_str())
                .and_then(|p| p.parse::<f64>().ok()),
            deal.get("dealID").and_then(|d| d.as_str()),
        ) else {
            continue;
        };
        let Some(&store) = stores.get(store_id) else {
            continue; // Steam/GOG come from their own regional lookups
        };
        let entry = best
            .entry(store.to_string())
            .or_insert((f64::MAX, String::new()));
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
pub(crate) fn similar_title(a: &str, b: &str) -> bool {
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
    [
        "edition",
        "goty",
        "cut",
        "remaster",
        "definitive",
        "complete",
        "enhanced",
    ]
    .iter()
    .any(|s| remainder.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_match_loosely_but_not_wrongly() {
        assert!(similar_title("Celeste", "CELESTE"));
        assert!(similar_title(
            "The Witcher 3: Wild Hunt",
            "the witcher 3 wild hunt"
        ));
        assert!(!similar_title("Celeste", "Celeste Classic 2: Lani's Trek"));
        assert!(similar_title(
            "Cyberpunk 2077",
            "Cyberpunk 2077: Ultimate Edition"
        ));
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
                note: None,
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

    #[test]
    fn country_currency_mapping_covers_selected_regions() {
        assert_eq!(currency_for_country("NL"), "EUR");
        assert_eq!(currency_for_country("GB"), "GBP");
        assert_eq!(currency_for_country("IN"), "INR");
        assert_eq!(currency_for_country("US"), "USD");
    }

    #[test]
    fn ecb_cross_conversion_uses_euro_base() {
        let rates = HashMap::from([
            ("EUR".into(), 1.0),
            ("USD".into(), 1.2),
            ("INR".into(), 100.0),
        ]);
        let converted = convert(12.0, "USD", "INR", Some(&("2026-01-01".into(), rates))).unwrap();
        assert!((converted.1 - 1000.0).abs() < 0.001);
    }

    #[test]
    fn estimates_never_start_as_best_regional() {
        let context = PricingContext {
            country: "NL".into(),
            currency: "EUR".into(),
            region_mode: "manual".into(),
            detected_country: "NL".into(),
            detected_currency: "EUR".into(),
        };
        let offer = cheapshark_offers_from_fixture(&context, 10.0);
        assert_eq!(offer.verification, PriceVerification::EstimatedForeign);
        assert!(!offer.best_regional);
    }

    fn cheapshark_offers_from_fixture(context: &PricingContext, amount: f64) -> StoreOffer {
        StoreOffer {
            store: "Example".into(),
            native: money(amount, "USD", "$10.00"),
            display: None,
            country: "US".into(),
            verification: if context.country == "US" {
                PriceVerification::Regional
            } else {
                PriceVerification::EstimatedForeign
            },
            discount_percent: 0,
            url: "https://example.com".into(),
            drm: vec![],
            platforms: vec![],
            checked_at: 0,
            conversion_date: None,
            best_regional: false,
        }
    }
}
