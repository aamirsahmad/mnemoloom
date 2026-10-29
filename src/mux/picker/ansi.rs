//! SGR escape sequences (as emitted by `tmux capture-pane -e`) to styled lines.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Parse coloured text into lines; style carries across line breaks.
/// Non-SGR escape sequences and other control characters are dropped.
pub fn to_lines(text: &str) -> Vec<Line<'static>> {
    let mut style = Style::new();
    text.lines()
        .map(|line| parse_line(line, &mut style))
        .collect()
}

fn parse_line(line: &str, style: &mut Style) -> Line<'static> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if c == '\t' {
                buf.push(' ');
            } else if !c.is_control() {
                buf.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                let mut terminator = None;
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        terminator = Some(c);
                        break;
                    }
                    params.push(c);
                }
                if terminator == Some('m') {
                    if !buf.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut buf), *style));
                    }
                    *style = apply_sgr(*style, &params);
                }
            }
            // OSC: skip to BEL or ST.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, *style));
    }
    Line::from(spans)
}

fn apply_sgr(mut style: Style, params: &str) -> Style {
    let codes: Vec<u16> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        match code {
            0 => style = Style::new(),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            5 => style = style.add_modifier(Modifier::SLOW_BLINK),
            7 => style = style.add_modifier(Modifier::REVERSED),
            8 => style = style.add_modifier(Modifier::HIDDEN),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            25 => style = style.remove_modifier(Modifier::SLOW_BLINK),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            28 => style = style.remove_modifier(Modifier::HIDDEN),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style.fg = Some(Color::Indexed((code - 30) as u8)),
            40..=47 => style.bg = Some(Color::Indexed((code - 40) as u8)),
            90..=97 => style.fg = Some(Color::Indexed((code - 90 + 8) as u8)),
            100..=107 => style.bg = Some(Color::Indexed((code - 100 + 8) as u8)),
            39 => style.fg = None,
            49 => style.bg = None,
            38 | 48 => {
                let (color, used) = extended_color(&codes[i + 1..]);
                if code == 38 {
                    style.fg = color.or(style.fg);
                } else {
                    style.bg = color.or(style.bg);
                }
                i += used;
            }
            _ => {}
        }
        i += 1;
    }
    style
}

/// `5;n` (256-colour) or `2;r;g;b` (truecolour); returns params consumed.
fn extended_color(rest: &[u16]) -> (Option<Color>, usize) {
    let byte = |i: usize| rest.get(i).and_then(|&v| u8::try_from(v).ok());
    match rest.first() {
        Some(5) => (byte(1).map(Color::Indexed), 2),
        Some(2) => match (byte(1), byte(2), byte(3)) {
            (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(r, g, b)), 4),
            _ => (None, rest.len()),
        },
        _ => (None, rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_one_span() {
        let lines = to_lines("hello\nworld");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans, vec![Span::raw("hello")]);
    }

    #[test]
    fn sgr_colours_and_reset() {
        let lines =
            to_lines("\x1b[1;31mred\x1b[0m plain \x1b[38;2;1;2;3mrgb\x1b[38;5;200mix\x1b[39mfg");
        let spans = &lines[0].spans;
        assert_eq!(
            spans[0],
            Span::styled(
                "red",
                Style::new()
                    .fg(Color::Indexed(1))
                    .add_modifier(Modifier::BOLD)
            )
        );
        assert_eq!(spans[1], Span::raw(" plain "));
        assert_eq!(spans[2].style.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(spans[3].style.fg, Some(Color::Indexed(200)));
        assert_eq!(spans[4].style.fg, None);
    }

    #[test]
    fn style_carries_across_lines() {
        let lines = to_lines("\x1b[32mgreen\nstill");
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Indexed(2)));
    }

    #[test]
    fn other_sequences_are_dropped() {
        let lines = to_lines("a\x1b[2Kb\x1b]0;title\x07c\x1b]8;;x\x1b\\d\te");
        assert_eq!(lines[0].spans, vec![Span::raw("abcd e")]);
    }

    #[test]
    fn truncated_extended_colour_is_ignored() {
        let lines = to_lines("\x1b[38;2;1mx");
        assert_eq!(lines[0].spans[0].style.fg, None);
    }
}
