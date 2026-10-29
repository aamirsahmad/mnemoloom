//! Match snippets as plain text plus highlight ranges.
//!
//! Snippets are never HTML: `text` is the raw transcript excerpt and
//! `highlights` are byte ranges into it, so JSON gets clean text (no tags or
//! entities) and the TUI styles the ranges.

use std::ops::Range;

use tantivy::Searcher;
use tantivy::query::Query;
use tantivy::snippet::SnippetGenerator;

/// A snippet: plain text and sorted, non-overlapping byte ranges to highlight.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    pub highlights: Vec<Range<usize>>,
}

impl Snippet {
    /// Split into `(segment, highlighted)` parts in order.
    pub fn parts(&self) -> Vec<(&str, bool)> {
        let mut out = Vec::new();
        let mut pos = 0;
        for r in &self.highlights {
            if r.start > pos {
                out.push((&self.text[pos..r.start], false));
            }
            out.push((&self.text[r.clone()], true));
            pos = r.end;
        }
        if pos < self.text.len() {
            out.push((&self.text[pos..], false));
        }
        out
    }
}

/// Highlight keywords: the query's units (see `query::query_units`),
/// lowercased, with surrounding punctuation trimmed (`TODO:` -> `todo`).
pub fn keywords(query: &str) -> Vec<String> {
    super::query::query_units(query)
        .into_iter()
        .flat_map(str::split_whitespace)
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Snippet for one document's `content`.
pub fn make_snippet(
    searcher: &Searcher,
    query: &dyn Query,
    content_field: tantivy::schema::Field,
    content: &str,
    kws: &[String],
) -> Snippet {
    let generated = SnippetGenerator::create(searcher, query, content_field)
        .map(|mut g| {
            g.set_max_num_chars(200);
            g.snippet(content)
        })
        .ok();
    let Some(s) = generated.filter(|s| !s.fragment().is_empty()) else {
        return extract_snippet(content, kws, 100);
    };
    let text = s.fragment().to_string();
    if kws.len() > 1 {
        let highlights = keyword_ranges(&text, kws);
        return Snippet { text, highlights };
    }
    if s.highlighted().is_empty() {
        return extract_snippet(content, kws, 100);
    }
    let highlights = merge_adjacent(&text, s.highlighted());
    Snippet { text, highlights }
}

/// Merge highlights separated only by up to three spaces or one newline.
fn merge_adjacent(text: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in ranges {
        if let Some(last) = out.last_mut() {
            let gap = &text[last.end..r.start.max(last.end)];
            if matches!(gap, " " | "  " | "   " | "\n") {
                last.end = r.end;
                continue;
            }
        }
        out.push(r.clone());
    }
    out
}

/// Byte ranges in `hay` of case-insensitive matches of `needle` (already
/// lowercase). Handles lowercase expansion (`İ` -> `i̇`) by mapping back to
/// whole original chars.
pub fn find_ci(hay: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut lower = String::with_capacity(hay.len());
    // For each byte of `lower`: the original char's byte range.
    let mut origin: Vec<Range<usize>> = Vec::with_capacity(hay.len());
    for (i, c) in hay.char_indices() {
        let r = i..i + c.len_utf8();
        for lc in c.to_lowercase() {
            lower.push(lc);
            origin.extend(std::iter::repeat_n(r.clone(), lc.len_utf8()));
        }
    }
    lower
        .match_indices(needle)
        .map(|(s, m)| origin[s].start..origin[s + m.len() - 1].end)
        .collect()
}

/// All occurrences of any keyword, sorted and merged.
pub fn keyword_ranges(text: &str, kws: &[String]) -> Vec<Range<usize>> {
    let mut all: Vec<Range<usize>> = kws.iter().flat_map(|k| find_ci(text, k)).collect();
    all.sort_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in all {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Fallback window around the first phrase/keyword match.
pub fn extract_snippet(content: &str, kws: &[String], window_chars: usize) -> Snippet {
    let phrase = (kws.len() > 1).then(|| kws.join(" "));
    let hit = phrase
        .iter()
        .chain(kws)
        .find_map(|k| find_ci(content, k).into_iter().next());
    let Some(hit) = hit else {
        return head_snippet(content, window_chars);
    };
    let half = window_chars / 2;
    let mut start = back_chars(content, hit.start, half);
    let mut end = fwd_chars(content, hit.end, half);
    while start > 0 && !prev_char(content, start).is_whitespace() {
        start -= prev_char(content, start).len_utf8();
    }
    while end < content.len() && !next_char(content, end).is_whitespace() {
        end += next_char(content, end).len_utf8();
    }
    let body = content[start..end].trim();
    let mut text = String::new();
    if start > 0 {
        text.push_str("...");
    }
    text.push_str(body);
    if end < content.len() {
        text.push_str("...");
    }
    let highlights = keyword_ranges(&text, kws);
    Snippet { text, highlights }
}

fn head_snippet(content: &str, window_chars: usize) -> Snippet {
    if content.chars().count() <= window_chars {
        return Snippet {
            text: content.to_string(),
            highlights: Vec::new(),
        };
    }
    let cut = fwd_chars(content, 0, window_chars);
    let head = &content[..cut];
    let head = head.rfind(char::is_whitespace).map_or(head, |i| &head[..i]);
    Snippet {
        text: format!("{}...", head.trim_end()),
        highlights: Vec::new(),
    }
}

fn prev_char(s: &str, i: usize) -> char {
    s[..i].chars().next_back().unwrap_or(' ')
}

fn next_char(s: &str, i: usize) -> char {
    s[i..].chars().next().unwrap_or(' ')
}

fn back_chars(s: &str, from: usize, n: usize) -> usize {
    s[..from]
        .char_indices()
        .rev()
        .take(n)
        .last()
        .map_or(from, |(i, _)| i)
}

fn fwd_chars(s: &str, from: usize, n: usize) -> usize {
    s[from..]
        .char_indices()
        .nth(n)
        .map_or(s.len(), |(i, _)| from + i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(q: &str) -> Vec<String> {
        keywords(q)
    }

    #[test]
    fn ci_matching_handles_expansion() {
        assert_eq!(find_ci("İstanbul is", "i"), vec![0..2, 10..11]);
        assert_eq!(find_ci("Hello HELLO", "hello"), vec![0..5, 6..11]);
        // Case folding that changes byte lengths: must not panic.
        let s = extract_snippet("İİİ abc İ", &kw("i̇ abc"), 100);
        assert!(s.text.contains("abc"));
    }

    #[test]
    fn window_and_markers() {
        let content = format!("{} needle here {}", "a ".repeat(60), "b ".repeat(60));
        let s = extract_snippet(&content, &kw("needle"), 100);
        assert!(s.text.starts_with("...") && s.text.ends_with("..."));
        let parts = s.parts();
        assert!(parts.contains(&("needle", true)));
        let s = extract_snippet("short text", &kw("zzz"), 100);
        assert_eq!(s.text, "short text");
        let s = extract_snippet(&"word ".repeat(40), &kw("zzz"), 100);
        assert!(s.text.ends_with("word..."));
        assert!(s.text.chars().count() <= 103);
    }

    #[test]
    fn phrase_first_then_keywords() {
        let s = extract_snippet("beta x alpha beta", &kw("alpha beta"), 100);
        assert_eq!(s.text, "beta x alpha beta");
        assert_eq!(s.highlights, vec![0..4, 7..12, 13..17]);
        assert_eq!(keyword_ranges("abab", &kw("ab ba")), vec![0..4]);
    }

    #[test]
    fn merge_rules() {
        let t = "foo bar\nbaz    qux";
        let r = merge_adjacent(t, &[0..3, 4..7, 8..11, 15..18]);
        assert_eq!(r, vec![0..11, 15..18]);
    }

    #[test]
    fn keyword_parsing() {
        assert_eq!(kw("\"Foo Bar\""), vec!["foo", "bar"]);
        assert_eq!(
            kw("TODO: (x) C++ std::fs"),
            vec!["todo", "x", "c", "std::fs"]
        );
    }
}
