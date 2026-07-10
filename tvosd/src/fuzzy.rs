//! Fuzzy title matching for search: an fzy-style in-order scorer, case-
//! insensitive, that rewards word-start hits and consecutive runs and
//! penalizes gaps. `score` returns None when the query simply isn't in the
//! title, so callers can filter and rank in one pass. Out-of-order queries
//! ("wars star") still match: each query word is scored independently.

/// How well `query` matches `target`. Higher is better; None = no match.
pub fn score(query: &str, target: &str) -> Option<i32> {
    let q = normalize(query);
    if q.is_empty() {
        return None;
    }
    let t = normalize(target);
    if let Some(s) = score_in_order(&q, &t) {
        return Some(s);
    }
    // Words out of order: every word must match somewhere on its own.
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }
    let mut total = 0;
    for word in words {
        total += score_in_order(word, &t)?;
    }
    Some(total - 5)
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Greedy left-to-right subsequence match of `q` (lowercase) in `t` (lowercase).
fn score_in_order(q: &str, t: &str) -> Option<i32> {
    let chars: Vec<char> = t.chars().collect();
    let mut score = 0i32;
    let mut next = 0usize;
    let mut last: Option<usize> = None;
    for qc in q.chars() {
        let i = (next..chars.len()).find(|&i| chars[i] == qc)?;
        let word_start = i == 0 || !chars[i - 1].is_alphanumeric();
        score += if word_start { 8 } else { 1 };
        if let Some(l) = last {
            if i == l + 1 {
                score += 6; // consecutive run
            } else {
                score -= ((i - l - 1) as i32).min(5); // gap penalty
                                                      // Skipping into the middle of a later word is barely a match
                                                      // ("anime" ⊂ "Adve*n*ture T*ime*") — punish it beyond the
                                                      // plain gap. Landing on a word start (initials) stays cheap.
                if !word_start && chars[l..i].iter().any(|c| !c.is_alphanumeric()) {
                    score -= 6;
                }
            }
        }
        last = Some(i);
        next = i + 1;
    }
    // Whole-query bonuses so real substrings beat scattered letters.
    if t == q {
        score += 40;
    } else if t.starts_with(q) {
        score += 25;
    } else if t.contains(q) {
        score += 15;
    }
    // Prefer shorter titles when the match quality ties.
    Some(score - (chars.len() as i32) / 8)
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn exact_beats_prefix_beats_substring() {
        let exact = score("dune", "Dune").unwrap();
        let prefix = score("dune", "Dune: Part Two").unwrap();
        let sub = score("dune", "Children of Dune").unwrap();
        assert!(exact > prefix, "{exact} vs {prefix}");
        assert!(prefix > sub, "{prefix} vs {sub}");
    }

    #[test]
    fn subsequence_matches_and_misses() {
        assert!(score("brbd", "Breaking Bad").is_some());
        assert!(score("xyz", "Breaking Bad").is_none());
    }

    #[test]
    fn word_crossing_scatter_scores_below_single_word_abbreviations() {
        // "anime" appears scattered across "Adventure Time" — a non-match to
        // a human. "brdl" is a deliberate Borderlands abbreviation.
        let scatter = score("anime", "Adventure Time").unwrap_or(-100);
        let abbrev = score("brdl", "Borderlands 2").unwrap();
        assert!(scatter < abbrev, "{scatter} vs {abbrev}");
        assert!(
            scatter < 10,
            "scatter should fall below the library floor: {scatter}"
        );
        assert!(
            abbrev >= 10,
            "abbreviations must survive the floor: {abbrev}"
        );
    }

    #[test]
    fn word_initials_score_well() {
        let initials = score("got", "Game of Thrones").unwrap();
        let scattered = score("got", "Ghostwriter Notes").unwrap();
        assert!(initials > scattered, "{initials} vs {scattered}");
    }

    #[test]
    fn out_of_order_words_still_match() {
        assert!(score("wars star", "Star Wars").is_some());
        assert!(score("wars star", "Star Trek").is_none());
    }

    #[test]
    fn case_and_whitespace_are_ignored() {
        assert_eq!(score("  DUNE ", "dune"), score("dune", "Dune"));
    }

    #[test]
    fn empty_query_never_matches() {
        assert!(score("", "Dune").is_none());
        assert!(score("   ", "Dune").is_none());
    }
}
