//! Type-to-filter matching, fzf style.

/// Whether every whitespace-separated term of `query` occurs in `haystack`
/// as a subsequence. Smart case: a term containing uppercase is case-sensitive.
pub fn matches(query: &str, haystack: &str) -> bool {
    let lower = haystack.to_lowercase();
    query.split_whitespace().all(|term| {
        if term.chars().any(char::is_uppercase) {
            is_subsequence(term, haystack)
        } else {
            is_subsequence(term, &lower)
        }
    })
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    needle.chars().all(|c| rest.any(|h| h == c))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: &str =
        "0:9.1                idle  codex       1.3d recent   Investigate domain website@main";

    #[test]
    fn empty_query_matches_everything() {
        assert!(matches("", ROW));
        assert!(matches("   ", ROW));
    }

    #[test]
    fn substring_and_fuzzy_terms() {
        assert!(matches("codex", ROW));
        assert!(matches("cdx", ROW));
        assert!(matches("idle website", ROW));
        assert!(!matches("busy", ROW));
        assert!(!matches("codex busy", ROW));
        assert!(!matches("xedoc", ROW));
    }

    #[test]
    fn smart_case() {
        assert!(matches("investigate", ROW));
        assert!(matches("Investigate", ROW));
        assert!(!matches("INVESTIGATE", ROW));
    }
}
