//! Small helpers shared across modules.

/// Minimal percent-encoding for URL path segments. Encodes every byte outside
/// the RFC 3986 *unreserved* set (`ALPHA / DIGIT / - . _ ~`); everything else —
/// including `'`, `(`, `)`, `:` and space — is escaped.
pub fn percent_encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Redacts credentials from a URL/error string before it goes to the logs.
/// Drops the query string (tokens, api_key, access_token often live there) and
/// masks anything that looks like a `token`/`key`/`secret`/`password` value.
pub fn scrub_secrets(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for word in input.split_inclusive(char::is_whitespace) {
        // Trim the trailing whitespace we kept via split_inclusive so it can be
        // re-appended verbatim after scrubbing the token.
        let (core, trailing) = match word.char_indices().rev().find(|(_, c)| !c.is_whitespace()) {
            Some((i, c)) => word.split_at(i + c.len_utf8()),
            None => ("", word),
        };
        out.push_str(&scrub_token(core));
        out.push_str(trailing);
    }
    out
}

/// Scrubs a single whitespace-delimited token: strips any query string, and if
/// the remaining text names a secret parameter, redacts its value.
fn scrub_token(token: &str) -> String {
    // Anything after '?' (a query string) is dropped wholesale.
    if let Some((head, _query)) = token.split_once('?') {
        return format!("{head}?<redacted>");
    }
    // `key=value` / `token: value` forms embedded in error text.
    let lower = token.to_ascii_lowercase();
    const SECRET_HINTS: [&str; 7] = [
        "token", "api_key", "apikey", "secret", "password", "access_token", "client_secret",
    ];
    if SECRET_HINTS.iter().any(|h| lower.contains(h)) {
        if let Some((name, _)) = token.split_once(['=', ':']) {
            return format!("{name}=<redacted>");
        }
    }
    token.to_string()
}

#[cfg(test)]
mod tests {
    use super::{percent_encode, scrub_secrets};

    #[test]
    fn encodes_path_segments() {
        assert_eq!(percent_encode("tt0903747:2:8"), "tt0903747%3A2%3A8");
        assert_eq!(percent_encode("Top Films"), "Top%20Films");
        // The old whitelist wrongly left these raw; they must be escaped now.
        assert_eq!(percent_encode("safe-chars_(ok)"), "safe-chars_%28ok%29");
        assert_eq!(percent_encode("a'b"), "a%27b");
    }

    #[test]
    fn scrubs_query_strings_and_secret_params() {
        assert_eq!(
            scrub_secrets("fetch https://api.tld/x?token=abc123 failed"),
            "fetch https://api.tld/x?<redacted> failed"
        );
        assert_eq!(
            scrub_secrets("access_token=deadbeef expired"),
            "access_token=<redacted> expired"
        );
        assert_eq!(scrub_secrets("plain error, no secrets"), "plain error, no secrets");
    }
}
