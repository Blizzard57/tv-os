//! Search across the entire content space.
//!
//! Two tiers, both fed by the same pools:
//!
//! * `flat` — the fast as-you-type tier: local library (fuzzy) + TMDB title
//!   search + addon catalog search, merged into one ranked list.
//! * `deep` — the "press Enter" tier: everything `flat` knows, plus people
//!   (an actor's filmography), TMDB keywords (plot ideas: "time travel"),
//!   and genre/idiom discovery ("k drama", "anime", "romcom"), returned as
//!   titled sections the shell renders as rows.
//!
//! Query understanding is deliberately simple and fast: a small idiom table
//! (language/region phrases), the static TMDB genre tables, and whatever text
//! remains is tried as a TMDB keyword. All network pools run in parallel.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::fuzzy;
use crate::model::{ContentItem, Kind, Row};
use crate::sources::{stremio, tmdb, youtube};

const RESULT_LIMIT: usize = 48;
const SECTION_LIMIT: usize = 20;
const PERSON_LIMIT: usize = 2;
const DEEP_TTL: Duration = Duration::from_secs(300);
/// Minimum fuzzy score for a library item to count as a match. Scattered
/// subsequences score below this ("anime" ⊂ "Adventure Time"), deliberate
/// abbreviations ("brdl" → Borderlands) above it.
const LOCAL_FLOOR: i32 = 10;

fn local_score(query: &str, title: &str) -> Option<i32> {
    fuzzy::score(query, title).filter(|s| *s >= LOCAL_FLOOR)
}

/// Unwraps a scoped-thread join result, logging a warning (instead of silently
/// swallowing) when the pool thread panicked, then falling back to a default.
fn joined<T: Default>(result: std::thread::Result<T>, pool: &str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => {
            crate::log_warn!("search pool {pool} panicked — skipping its results");
            T::default()
        }
    }
}

static DEEP_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<Row>)>>> =
    LazyLock::new(Mutex::default);

// ---- Fast tier -------------------------------------------------------------

/// As-you-type search: library + TMDB titles + addon catalogs, one ranked list.
pub fn flat(query: &str, library: Vec<Row>) -> Vec<ContentItem> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    // The pools are independent network/CLI work — fetch in parallel.
    let (tmdb_items, addon_items) = std::thread::scope(|s| {
        let tmdb = s.spawn(|| tmdb::search(query));
        let addon = s.spawn(|| stremio::search(query));
        (
            joined(tmdb.join(), "tmdb::search"),
            joined(addon.join(), "stremio::search"),
        )
    });

    let local = library
        .into_iter()
        .flat_map(|row| row.items)
        .map(|item| (item, true));
    let catalog = tmdb_items
        .into_iter()
        .chain(addon_items)
        .map(|item| (item, false));

    let mut dedupe = Dedupe::default();
    let mut scored: Vec<(i32, usize, ContentItem)> = Vec::new();
    for (idx, (item, is_local)) in local.chain(catalog).enumerate() {
        if !dedupe.insert(&item) {
            continue;
        }
        let score = if is_local {
            // Local items must genuinely match — and then lead the list.
            match local_score(query, &item.title) {
                Some(s) => s + 30,
                None => continue,
            }
        } else {
            // Catalog results already matched server-side (alternative titles,
            // translations), so keep them even when the title doesn't fuzz.
            fuzzy::score(query, &item.title).unwrap_or(0)
        };
        scored.push((score, idx, item));
    }
    // Best score first; upstream order breaks ties.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .map(|(_, _, item)| item)
        .take(RESULT_LIMIT)
        .collect()
}

// ---- Deep tier -------------------------------------------------------------

/// Full-space search: sections of results, best section first. Cached briefly.
pub fn deep(query: &str, library: Vec<Row>) -> Vec<Row> {
    let query = normalize(query);
    if query.is_empty() {
        return Vec::new();
    }
    if let Some((at, rows)) = DEEP_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&query)
    {
        if at.elapsed() < DEEP_TTL {
            return rows.clone();
        }
    }

    let plan = parse_plan(&query);

    // Everything the network can answer, in parallel.
    let (titles_and_people, addon_items, discover_rows) = std::thread::scope(|s| {
        let multi = s.spawn(|| {
            let (titles, persons) = tmdb::multi_search(&query);
            // Filmographies for the people the query most plausibly named.
            let people_rows: Vec<Row> = persons
                .into_iter()
                .take(PERSON_LIMIT)
                .filter_map(|p| {
                    let items = tmdb::person_credits(p.id);
                    (!items.is_empty()).then(|| Row {
                        title: format!("With {}", p.name),
                        items,
                    })
                })
                .collect();
            (titles, people_rows)
        });
        let addon = s.spawn(|| stremio::search(&query));
        let disc = s.spawn(|| discover_sections(&query, plan.as_ref()));
        let yt = s.spawn(|| youtube::search(&query));
        (
            joined(multi.join(), "tmdb::multi_search"),
            joined(addon.join(), "stremio::search"),
            (
                joined(disc.join(), "discover_sections"),
                joined(yt.join(), "youtube::search"),
            ),
        )
    });
    let (tmdb_titles, people_rows) = titles_and_people;
    let (discover_rows, yt_items) = discover_rows;

    // Local library, fuzzy-matched.
    let mut local: Vec<(i32, ContentItem)> = library
        .into_iter()
        .flat_map(|row| row.items)
        .filter_map(|item| local_score(&query, &item.title).map(|s| (s, item)))
        .collect();
    local.sort_by(|a, b| b.0.cmp(&a.0));

    // Title matches: TMDB + addons, fuzzy-ranked like the fast tier.
    let mut titles: Vec<(i32, usize, ContentItem)> = tmdb_titles
        .into_iter()
        .chain(addon_items)
        .enumerate()
        .map(|(idx, item)| (fuzzy::score(&query, &item.title).unwrap_or(0), idx, item))
        .collect();
    titles.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    // Assemble in display order; one item appears once, in its best section.
    let mut dedupe = Dedupe::default();
    let mut rows: Vec<Row> = Vec::new();
    let mut push = |title: String, items: Vec<ContentItem>, rows: &mut Vec<Row>| {
        let items: Vec<ContentItem> = items
            .into_iter()
            .filter(|i| dedupe.insert(i))
            .take(SECTION_LIMIT)
            .collect();
        if !items.is_empty() {
            rows.push(Row { title, items });
        }
    };

    push(
        "In your library".into(),
        local.into_iter().map(|(_, i)| i).collect(),
        &mut rows,
    );
    // For vibe/theme queries the discover section is the intent — literal
    // title matches ("K-Drama Ramen") rank below it. Ditto an actor's
    // filmography. Plain title queries have neither, so titles lead anyway.
    for row in discover_rows {
        push(row.title, row.items, &mut rows);
    }
    for row in people_rows {
        push(row.title, row.items, &mut rows);
    }
    push(
        "Top matches".into(),
        titles.into_iter().map(|(_, _, i)| i).collect(),
        &mut rows,
    );
    // A different medium for the same query — always last, never competing
    // with the catalog sections above.
    push("YouTube".into(), yt_items, &mut rows);

    let mut cache = DEEP_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 32 {
        cache.clear();
    }
    cache.insert(query, (Instant::now(), rows.clone()));
    rows
}

/// The genre/idiom/keyword part of deep search. With a plan ("k drama",
/// "romcom"…) it discovers matching titles, folding any leftover words in as
/// TMDB keywords ("kdrama hospital"). Without one, the whole query is tried
/// as a keyword — that's what catches plot ideas ("time travel", "heist").
fn discover_sections(query: &str, plan: Option<&Plan>) -> Vec<Row> {
    // Broad genre browses want canon (high vote floor); keyword digs want
    // niche finds, so the floor drops when keywords narrow the request.
    match plan {
        Some(plan) => {
            let keywords = keyword_matches(&plan.remainder);
            let label = match keywords.first() {
                Some((_, name)) => format!("{} · {}", plan.label, title_case(name)),
                None => plan.label.clone(),
            };
            let kw_ids: Vec<i64> = keywords.iter().map(|(id, _)| *id).collect();
            let (tv_floor, movie_floor) = if kw_ids.is_empty() {
                (150, 300)
            } else {
                (20, 50)
            };
            // If leftover words resolved to no keyword, they were probably a
            // title fragment — Top matches covers that; discover without them.
            let mut items = Vec::new();
            if plan.tv {
                items.extend(tmdb::discover(
                    "tv",
                    &plan.tv_genres,
                    &plan.without_genres,
                    plan.language,
                    &kw_ids,
                    tv_floor,
                ));
            }
            if plan.movies {
                items.extend(tmdb::discover(
                    "movie",
                    &plan.movie_genres,
                    &plan.without_genres,
                    plan.language,
                    &kw_ids,
                    movie_floor,
                ));
            }
            if items.is_empty() {
                return Vec::new();
            }
            vec![Row {
                title: label,
                items,
            }]
        }
        None => {
            let keywords = keyword_matches(query);
            let Some((id, name)) = keywords.first() else {
                return Vec::new();
            };
            let mut items = tmdb::discover("movie", &[], &[], None, &[*id], 50);
            items.extend(tmdb::discover("tv", &[], &[], None, &[*id], 20));
            if items.is_empty() {
                return Vec::new();
            }
            vec![Row {
                title: format!("Theme · {}", title_case(name)),
                items,
            }]
        }
    }
}

/// TMDB keywords for free text, kept only when they genuinely match it.
fn keyword_matches(text: &str) -> Vec<(i64, String)> {
    let text = text.trim();
    if text.len() < 3 {
        return Vec::new();
    }
    tmdb::keyword_ids(text)
        .into_iter()
        .filter(|(_, name)| fuzzy::score(text, name).is_some())
        .take(2)
        .collect()
}

// ---- Query understanding ---------------------------------------------------

/// What a vibe-style query asks for, distilled from idioms + genre words.
#[derive(Debug)]
pub struct Plan {
    pub label: String,
    pub movie_genres: Vec<i64>,
    pub tv_genres: Vec<i64>,
    pub without_genres: Vec<i64>,
    pub language: Option<&'static str>,
    pub movies: bool,
    pub tv: bool,
    /// Query text not consumed by idiom/genre words (tried as a keyword).
    pub remainder: String,
}

/// Genres the X-drama idioms exclude: animation, plus the variety-show block
/// (talk, reality, news) that otherwise dominates popularity-sorted Korean/
/// Japanese TV (Running Man et al. are not what "k drama" means).
const VARIETY: &[i64] = &[16, 10767, 10764, 10763];

/// Region/language idioms:
/// (phrases, label, language, with genres, without genres, tv, movies).
const IDIOMS: &[(&[&str], &str, &str, &[i64], &[i64], bool, bool)] = &[
    (
        &["kdrama", "k drama", "korean drama", "korean"],
        "K-Drama",
        "ko",
        &[],
        VARIETY,
        true,
        false,
    ),
    (
        &["cdrama", "c drama", "chinese drama", "chinese"],
        "C-Drama",
        "zh",
        &[],
        VARIETY,
        true,
        false,
    ),
    (
        &["jdrama", "j drama", "japanese drama"],
        "J-Drama",
        "ja",
        &[],
        VARIETY,
        true,
        false,
    ),
    (&["anime"], "Anime", "ja", &[16], &[], true, true),
    (
        &["bollywood", "hindi"],
        "Bollywood",
        "hi",
        &[],
        &[],
        false,
        true,
    ),
];

/// Genre vocabulary: (phrase, movie genre ids, tv genre ids). Ids are TMDB's
/// static genre lists (stable for years). A phrase may map to several ids
/// ("romcom") and to nothing for one medium (TV has no Romance genre).
const GENRES: &[(&str, &[i64], &[i64])] = &[
    ("action", &[28], &[10759]),
    ("adventure", &[12], &[10759]),
    ("animation", &[16], &[16]),
    ("animated", &[16], &[16]),
    ("cartoon", &[16], &[16]),
    ("comedy", &[35], &[35]),
    ("crime", &[80], &[80]),
    ("documentary", &[99], &[99]),
    ("drama", &[18], &[18]),
    ("family", &[10751], &[10751]),
    ("fantasy", &[14], &[10765]),
    ("history", &[36], &[]),
    ("historical", &[36], &[]),
    ("horror", &[27], &[9648]),
    ("music", &[10402], &[]),
    ("musical", &[10402], &[]),
    ("mystery", &[9648], &[9648]),
    ("romance", &[10749], &[]),
    ("romantic", &[10749], &[]),
    ("romcom", &[10749, 35], &[35]),
    ("rom com", &[10749, 35], &[35]),
    ("science fiction", &[878], &[10765]),
    ("sci fi", &[878], &[10765]),
    ("scifi", &[878], &[10765]),
    ("thriller", &[53], &[9648]),
    ("war", &[10752], &[10768]),
    ("western", &[37], &[37]),
    ("kids", &[10751], &[10762]),
    ("reality", &[], &[10764]),
];

/// Parses idiom + genre words out of a normalized query. None = the query has
/// no vibe words and is a plain title/person/keyword lookup.
pub fn parse_plan(query: &str) -> Option<Plan> {
    let mut text = format!(" {} ", normalize(query));
    let mut labels: Vec<String> = Vec::new();
    let mut plan = Plan {
        label: String::new(),
        movie_genres: Vec::new(),
        tv_genres: Vec::new(),
        without_genres: Vec::new(),
        language: None,
        movies: true,
        tv: true,
        remainder: String::new(),
    };
    let mut matched = false;

    for (phrases, label, lang, with, without, tv, movies) in IDIOMS {
        if let Some(phrase) = phrases.iter().find(|p| text.contains(&format!(" {p} "))) {
            text = text.replace(&format!(" {phrase} "), " ");
            labels.push((*label).to_string());
            plan.language = Some(lang);
            plan.tv = *tv;
            plan.movies = *movies;
            plan.movie_genres.extend_from_slice(with);
            plan.tv_genres.extend_from_slice(with);
            plan.without_genres.extend_from_slice(without);
            matched = true;
            break; // one region idiom per query
        }
    }

    for (phrase, movie_ids, tv_ids) in GENRES {
        let padded = format!(" {phrase} ");
        if text.contains(&padded) {
            text = text.replace(&padded, " ");
            labels.push(title_case(phrase));
            plan.movie_genres.extend_from_slice(movie_ids);
            plan.tv_genres.extend_from_slice(tv_ids);
            matched = true;
        }
    }

    // Media words pin the medium ("korean movies", "action shows") but don't
    // make a plan on their own — "movies" alone is not a searchable vibe.
    const MEDIA_WORDS: &[(&str, bool, bool)] = &[
        ("movie", true, false),
        ("movies", true, false),
        ("film", true, false),
        ("films", true, false),
        ("show", false, true),
        ("shows", false, true),
        ("series", false, true),
        ("tv", false, true),
    ];
    for (word, movies, tv) in MEDIA_WORDS {
        let padded = format!(" {word} ");
        if text.contains(&padded) {
            text = text.replace(&padded, " ");
            if matched {
                plan.movies = *movies;
                plan.tv = *tv;
            }
        }
    }

    if !matched {
        return None;
    }
    // A medium with no way to express the request drops out (e.g. pure
    // "romance": TMDB TV has no Romance genre, so search movies only) —
    // unless an idiom pinned the media (K-Drama stays TV).
    if plan.language.is_none() {
        if plan.movie_genres.is_empty() && !plan.tv_genres.is_empty() {
            plan.movies = false;
        }
        if plan.tv_genres.is_empty() && !plan.movie_genres.is_empty() {
            plan.tv = false;
        }
    }
    plan.label = labels.join(" · ");
    plan.remainder = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(plan)
}

// ---- Small helpers ----------------------------------------------------------

/// Dedupes by id, and by title+kind (the same film arrives from TMDB and an
/// addon under different ids — to the eye it's the same card).
#[derive(Default)]
struct Dedupe {
    ids: HashSet<String>,
    titles: HashSet<(Kind, String)>,
}

impl Dedupe {
    fn insert(&mut self, item: &ContentItem) -> bool {
        self.ids.insert(item.id.clone())
            && self.titles.insert((item.kind, item.title.to_lowercase()))
    }
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdrama_idiom_maps_to_korean_tv() {
        let plan = parse_plan("k drama").unwrap();
        assert_eq!(plan.language, Some("ko"));
        assert!(plan.tv && !plan.movies);
        // Excludes animation and the variety-show block (talk/reality/news).
        assert_eq!(plan.without_genres, VARIETY.to_vec());
        assert_eq!(plan.label, "K-Drama");
        assert!(plan.remainder.is_empty());
    }

    #[test]
    fn idiom_plus_genre_plus_leftover_words() {
        let plan = parse_plan("kdrama romance hospital").unwrap();
        assert_eq!(plan.language, Some("ko"));
        assert_eq!(plan.movie_genres, vec![10749]);
        assert_eq!(plan.label, "K-Drama · Romance");
        assert_eq!(plan.remainder, "hospital");
    }

    #[test]
    fn anime_forces_animation_genre_on_both_media() {
        let plan = parse_plan("anime").unwrap();
        assert_eq!(plan.language, Some("ja"));
        assert!(plan.tv && plan.movies);
        assert!(plan.movie_genres.contains(&16) && plan.tv_genres.contains(&16));
    }

    #[test]
    fn plain_genres_combine() {
        let plan = parse_plan("action comedy").unwrap();
        assert_eq!(plan.movie_genres, vec![28, 35]);
        assert_eq!(plan.tv_genres, vec![10759, 35]);
        assert_eq!(plan.label, "Action · Comedy");
    }

    #[test]
    fn movie_only_genre_drops_tv() {
        let plan = parse_plan("romance").unwrap();
        assert!(plan.movies && !plan.tv);
    }

    #[test]
    fn media_words_pin_the_medium() {
        let plan = parse_plan("korean movies").unwrap();
        assert!(plan.movies && !plan.tv);
        assert_eq!(plan.language, Some("ko"));
        // …but a media word alone is not a plan.
        assert!(parse_plan("movies").is_none());
    }

    #[test]
    fn multiword_genres_match() {
        let plan = parse_plan("science fiction").unwrap();
        assert_eq!(plan.movie_genres, vec![878]);
        assert_eq!(plan.tv_genres, vec![10765]);
    }

    #[test]
    fn titles_are_not_plans() {
        assert!(parse_plan("dune part two").is_none());
        assert!(parse_plan("breaking bad").is_none());
        // "dramatic" must not match the "drama" genre (word boundaries).
        assert!(parse_plan("dramatic exit").is_none());
    }

    #[test]
    fn dedupe_rejects_same_title_from_second_source() {
        let mut d = Dedupe::default();
        let a = ContentItem {
            id: "tmdb:movie:1".into(),
            kind: Kind::Movie,
            title: "Dune".into(),
            art: None,
            action: crate::model::Action::Play,
        };
        let mut b = a.clone();
        b.id = "strm:movie:tt1160419".into();
        assert!(d.insert(&a));
        assert!(!d.insert(&b));
    }
}
