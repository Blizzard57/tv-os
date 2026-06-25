//! Small helpers shared across modules.

/// Minimal percent-encoding for URL path segments.
pub fn percent_encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'('
            | b')'
            | b'\'' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::percent_encode;

    #[test]
    fn encodes_path_segments() {
        assert_eq!(percent_encode("tt0903747:2:8"), "tt0903747%3A2%3A8");
        assert_eq!(percent_encode("Top Films"), "Top%20Films");
        assert_eq!(percent_encode("safe-chars_(ok)"), "safe-chars_(ok)");
    }
}
