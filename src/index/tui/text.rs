//! Terminal-cell text helpers (unicode width via ratatui) and the single
//! word wrapper used by the preview, snippets and the viewer. Text is
//! measured and cut by grapheme cluster, so emoji sequences (VS16, ZWJ,
//! skin tones, flags) and combining marks are never split.

use std::ops::Range;

use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;

/// Display width of `s` in terminal cells.
pub fn width(s: &str) -> usize {
    Span::raw(s).width()
}

/// Prefix occupying at most `max` cells, without a loss marker.
pub fn prefix(s: &str, max: usize) -> &str {
    let mut w = 0;
    for (i, g) in s.grapheme_indices(true) {
        let gw = width(g);
        if w + gw > max {
            return &s[..i];
        }
        w += gw;
    }
    s
}

/// Suffix occupying at most `max` cells (the tail of an input line).
pub fn suffix(s: &str, max: usize) -> &str {
    let mut w = 0;
    for (i, g) in s.grapheme_indices(true).rev() {
        w += width(g);
        if w > max {
            return &s[i + g.len()..];
        }
    }
    s
}

/// Cut to at most `max` cells, ending in `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        format!("{}…", prefix(s, max - 1))
    }
}

/// Pad (or cut) to exactly `w` cells, left- or right-aligned.
pub fn fit(s: &str, w: usize, right: bool) -> String {
    let t = truncate(s, w);
    let pad = " ".repeat(w.saturating_sub(width(&t)));
    if right { pad + &t } else { t + &pad }
}

/// Greedy wrap of one line into byte ranges of at most `max` cells:
/// breaks after spaces when possible, otherwise mid-word (between grapheme
/// clusters), so every character lands on some row. Always returns at least one range.
pub fn wrap_ranges(text: &str, max: usize) -> Vec<Range<usize>> {
    let max = max.max(1);
    let mut rows = Vec::new();
    let (mut start, mut w, mut brk) = (0, 0, None::<usize>);
    for (i, g) in text.grapheme_indices(true) {
        let gw = width(g);
        if w + gw > max && i > start {
            let cut = brk.filter(|&b| b > start).unwrap_or(i);
            rows.push(start..cut);
            start = cut;
            w = width(&text[cut..i]);
            brk = None;
        }
        w += gw;
        if g == " " {
            brk = Some(i + 1);
        }
    }
    rows.push(start..text.len());
    rows
}

/// Wrap each line of `text` to `max` cells (trailing spaces trimmed).
pub fn wrap_text(text: &str, max: usize) -> Vec<String> {
    text.lines()
        .flat_map(|l| {
            wrap_ranges(l, max)
                .into_iter()
                .map(move |r| l[r].trim_end().to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_edges() {
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(truncate("hello", 1), "…");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("hi", 4), "hi");
        assert_eq!(truncate("hello", 5), "hello");
        // Wide chars count two cells.
        assert_eq!(truncate("日本語です", 5), "日本…");
        assert_eq!(fit("日本", 6, false), "日本  ");
        assert_eq!(fit("ab", 4, true), "  ab");
        assert_eq!(prefix("日本語", 3), "日");
        assert_eq!(prefix("😀x", 2), "😀");
        assert_eq!(prefix("hello", 0), "");
        assert_eq!(prefix("e\u{301}x", 1), "e\u{301}");
        assert_eq!(width("😀e\u{301}"), 3);
        assert_eq!(width(&fit("😀", 5, false)), 5);
    }

    #[test]
    fn emoji_sequences_are_measured_and_cut_whole() {
        let hearts = "❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}"; // VS16
        assert_eq!(width(hearts), 6);
        assert_eq!(fit(hearts, 4, false), "❤\u{fe0f}… ");
        assert!(width(&fit(hearts, 4, false)) <= 4);
        assert_eq!(width(&fit(hearts, 4, true)), 4);
        assert_eq!(prefix(hearts, 3), "❤\u{fe0f}");
        let thumbs = "👍🏽👍🏽👍🏽"; // skin-tone modifier
        assert_eq!(truncate(thumbs, 3), "👍🏽…");
        assert_eq!(truncate(thumbs, 2), "…");
        let family = "👨\u{200d}👩\u{200d}👧x"; // ZWJ sequence
        assert_eq!(width(family), 3);
        assert_eq!(prefix(family, 2), "👨\u{200d}👩\u{200d}👧");
        assert_eq!(prefix(family, 1), "");
        assert_eq!(truncate(family, 2), "…");
        let flags = "🇺🇸🇯🇵"; // regional-indicator pairs
        assert_eq!(prefix(flags, 3), "🇺🇸");
        assert_eq!(suffix(flags, 3), "🇯🇵");
        assert_eq!(suffix("ab日本", 5), "b日本");
        assert_eq!(suffix("ab", 5), "ab");
        assert_eq!(truncate("日本語です", 4), "日…");
        assert_eq!(fit("日本語", 5, false), "日本…");
        assert_eq!(fit("日本語", 4, false), "日… ");
        for s in [
            hearts,
            thumbs,
            family,
            flags,
            "日本語です",
            "e\u{301}e\u{301}",
        ] {
            for w in 0..8 {
                assert_eq!(width(&fit(s, w, false)), w, "{s:?} {w}");
            }
            let rows = wrap_ranges(s, 2);
            assert_eq!(rows.iter().map(|r| r.len()).sum::<usize>(), s.len());
            assert!(
                rows.iter()
                    .all(|r| s.is_char_boundary(r.start) && width(&s[r.clone()]) <= 2)
            );
        }
        assert_eq!(wrap_ranges(hearts, 3), [0..6, 6..12, 12..18]);
    }

    #[test]
    fn wrapping_reaches_every_char() {
        assert_eq!(wrap_text("a bb ccc dddd", 6), ["a bb", "ccc", "dddd"]);
        assert_eq!(wrap_text("x\n\ny", 5), ["x", "", "y"]);
        let long = "x".repeat(25) + " END";
        let rows = wrap_ranges(&long, 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().map(|r| r.len()).sum::<usize>(), long.len());
        assert_eq!(&long[rows[2].clone()], "xxxxx END");
        assert_eq!(wrap_ranges("", 5).len(), 1);
        assert_eq!(wrap_ranges("日本語", 4), [0..6, 6..9]);
    }
}
