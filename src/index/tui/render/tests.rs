use std::collections::HashMap;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::*;
use crate::index::live::LiveState;
use crate::index::query::Filters;
use crate::index::tui::app::{Focus, Mode};
use crate::index::tui::facets::{self, tests::row};
use crate::index::tui::view::{Preview, ViewState};
use crate::session::Homes;

struct Fixture {
    rows: Vec<Row>,
    snippets: HashMap<DocAddress, Snippet>,
    live: LiveMap,
}

fn fixture() -> Fixture {
    let mut rows = vec![
        row(0, Agent::Claude, "webapp"),
        row(1, Agent::Codex, "infra"),
        row(2, Agent::Claude, "website"),
    ];
    rows[0].custom_title = "Add CLI access to webapp".into();
    rows[0].branch = "alice/ABC-123".into();
    rows[1].derivation_type = "trimmed".into();
    let snippets = rows
        .iter()
        .map(|r| {
            let text = format!(
                "failing with an auth timeout after refresh {}",
                r.session_id
            );
            let at = text.find("auth").unwrap();
            (
                r.doc,
                Snippet {
                    text,
                    highlights: std::iter::once(at..at + 4).collect(),
                },
            )
        })
        .collect();
    let live = [(rows[0].file_path.clone(), LiveState::Running)].into();
    Fixture {
        rows,
        snippets,
        live,
    }
}

fn app(f: &Fixture, query: &str) -> App {
    let filters = Filters::new(&Homes::new("/h/c", "/h/x"), "/w".into());
    let mut a = App::new(filters, query.into(), false, None);
    a.set_results((0..f.rows.len()).collect());
    let base: Vec<usize> = a.results.clone();
    a.counts = facets::count(&f.rows, &base, &a.filters, &a.projects);
    let text = vec![
        "> the sync keeps failing with an auth timeout".to_string(),
        String::new(),
        "⏺ dex's default auth timeout is 30s".to_string(),
        String::new(),
        "⏺ [Tool: Bash]".to_string(),
        "  kubectl version --client".to_string(),
    ];
    let kws = crate::index::snippet::keywords(query);
    a.preview = Some(Box::new(Preview {
        view: ViewState::new(f.rows[0].clone(), text, kws),
        parent: Some((crate::session::Derivation::Trimmed, "b0b0cafe".into())),
        fresh: true,
    }));
    a
}

/// Lay out and draw `app` at `w` x `h`.
fn render(app: &mut App, f: &Fixture, w: u16, h: u16) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    crate::index::tui::layout(app, Rect::new(0, 0, w, h));
    let data = Data {
        rows: &f.rows,
        snippets: &f.snippets,
        live: &f.live,
        indexing: false,
        building: false,
    };
    t.draw(|fr| draw(fr, app, &data)).unwrap();
    t.backend().buffer().clone()
}

fn line(b: &Buffer, y: u16) -> String {
    (0..b.area.width)
        .map(|x| b[(x, y)].symbol().to_string())
        .collect()
}

/// (column, row) of the first occurrence of `s`, in cells.
fn find(b: &Buffer, s: &str) -> Option<(u16, u16)> {
    (0..b.area.height).find_map(|y| {
        let cells: Vec<&str> = (0..b.area.width).map(|x| b[(x, y)].symbol()).collect();
        let joined: String = cells.concat();
        let byte = joined.find(s)?;
        // Map the byte offset back to a cell index.
        let mut acc = 0;
        let x = cells.iter().position(|c| {
            let hit = acc >= byte;
            acc += c.len();
            hit
        })?;
        Some((x as u16, y))
    })
}

fn has(b: &Buffer, s: &str) -> bool {
    find(b, s).is_some()
}

#[test]
fn wide_layout_matches_the_mockup() {
    let f = fixture();
    let mut a = app(&f, "auth timeout");
    let b = render(&mut a, &f, 140, 36);
    // Filters pane: 28 columns at the left.
    assert_eq!(find(&b, "FILTERS"), Some((3, 1)));
    assert_eq!(find(&b, "Agent"), Some((3, 3)));
    let claude = line(&b, 4);
    assert!(claude[..90].contains("[x] claude"), "{claude}");
    assert_eq!(b[(25, 4)].symbol(), "2", "count right-aligned");
    assert_eq!(b[(3, 4)].fg, theme::OK);
    for s in [
        "Project",
        "[x] webapp",
        "When",
        "( ) today",
        "(•) all",
        "Kind",
        "[x] originals",
        "[ ] sidechains",
    ] {
        assert!(has(&b, s), "missing {s}");
    }
    // When: three rows of two radios, columns aligned, all in the pane.
    let (tx, ty) = find(&b, "( ) today  ( ) yesterday").unwrap();
    assert_eq!(find(&b, "( ) 3d     ( ) 7d"), Some((tx, ty + 1)));
    assert_eq!(find(&b, "( ) 30d    (•) all"), Some((tx, ty + 2)));
    assert!(tx + 24 <= 27, "yesterday fits inside the border");
    assert_eq!(b[(27, 0)].symbol(), "╮");
    // Results pane: query line, then cards; it has focus.
    assert_eq!(find(&b, "/ auth timeout"), Some((31, 1)));
    assert!(line(&b, 1).contains("3 hits · relevance"));
    assert_eq!(b[(29, 0)].fg, theme::ACCENT);
    assert_eq!(b[(0, 0)].fg, theme::BORDER);
    let (bx, by) = find(&b, "CLD").unwrap();
    assert_eq!((bx, by), (32, 3));
    assert_eq!(b[(bx, by)].fg, theme::CLAUDE);
    assert_eq!(b[(30, 3)].symbol(), "▌");
    assert_eq!(b[(30, 5)].symbol(), "▌");
    assert_eq!(b[(40, 3)].bg, theme::BG_SEL);
    assert_eq!(b[(30, 6)].symbol(), " ", "unselected cards have no bar");
    assert!(line(&b, 3).contains("Add CLI access to webapp"));
    assert!(line(&b, 4).contains("webapp @ al… · 42 user msgs · ● live"));
    assert!(line(&b, 7).contains("infra @ ma… · 42 user msgs · trimmed"));
    let (hx, hy) = find(&b, "…failing with an auth").unwrap();
    assert_eq!(hy, 5);
    let auth = hx + "…failing with an ".chars().count() as u16;
    assert_eq!(b[(auth, 5)].bg, theme::MATCH);
    assert_eq!(b[(hx + 1, 5)].fg, theme::FG_SOFT);
    // Preview pane: header block, transcript, footer.
    assert_eq!(find(&b, "Add CLI access to webapp"), Some((76, 1)));
    assert!(has(&b, "claude · 00000000 · 42 user msgs ·"));
    assert!(has(&b, "/w/webapp @ alice/ABC-123"));
    let (lx, ly) = find(&b, "lineage  b0b0cafe ─trim─▶ 00000000").unwrap();
    assert_eq!(b[(lx, ly)].fg, theme::LINK);
    let (ux, uy) = find(&b, "    you the sync").unwrap();
    assert_eq!(b[(ux + 4, uy)].fg, theme::OK);
    assert!(has(&b, " claude dex's default"));
    assert!(has(&b, "   tool Bash"));
    assert!(
        has(&b, "match 1/4   n/N jump"),
        "auth + timeout, twice each"
    );
    let (gx, _) = find(&b, "▮▮▮▮▮▮▮▮ 100%").unwrap();
    assert_eq!(gx + 13, 138, "gauge right-aligned in the pane");
    // Footer: chip, hints, right-aligned help.
    let foot = line(&b, 35);
    assert!(
        foot.starts_with(" RESULTS   h/l/tab pane  j/k move  ⏎ resume  a actions"),
        "{foot}"
    );
    assert!(foot.trim_end().ends_with("? help  q quit"));
    assert_eq!(foot.trim_end().chars().count(), 140);
    assert_eq!(b[(1, 35)].bg, theme::ACCENT);
    assert_eq!(b[(0, 20)].bg, theme::BG, "frame background");
}

#[test]
fn preview_highlights_current_match() {
    let f = fixture();
    let mut a = app(&f, "auth");
    let b = render(&mut a, &f, 140, 36);
    let rows: Vec<u16> = (0..36)
        .filter(|&y| line(&b, y).contains("    you the sync"))
        .collect();
    let y = rows[0];
    let x = find(&b, "    you the sync").unwrap().0;
    let auth = x + "    you the sync keeps failing with an ".chars().count() as u16;
    assert_eq!(b[(auth, y)].bg, theme::ACCENT, "current match");
    let (cx, cy) = find(&b, " claude dex's default ").unwrap();
    let other = cx + " claude dex's default ".chars().count() as u16;
    assert_eq!(b[(other, cy)].bg, theme::MATCH, "other match");
    // n moves the current match.
    a.on_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Char('n'),
    ));
    let b = render(&mut a, &f, 140, 36);
    assert!(has(&b, "match 2/2"));
}

#[test]
fn focus_and_query_mode_show_in_chrome() {
    let f = fixture();
    let mut a = app(&f, "");
    assert_eq!(a.mode, Mode::Query);
    let b = render(&mut a, &f, 140, 36);
    assert!(line(&b, 35).starts_with(" QUERY "));
    assert!(line(&b, 35).trim_end().ends_with("esc results"));
    assert!(has(&b, "/ ▏"));
    assert!(has(&b, "3 sessions"));
    a.mode = Mode::Normal;
    a.focus = Focus::Filters;
    let b = render(&mut a, &f, 140, 36);
    assert!(line(&b, 35).starts_with(" FILTERS "));
    assert!(has(&b, "press / to search"));
    assert_eq!(b[(0, 0)].fg, theme::ACCENT);
    assert_eq!(b[(29, 0)].fg, theme::BORDER);
    // The facet cursor: ACCENT bar + selected background.
    assert_eq!(b[(1, 4)].symbol(), "▌");
    assert_eq!(b[(10, 4)].bg, theme::BG_SEL);
    a.focus = Focus::Preview;
    let b = render(&mut a, &f, 140, 36);
    assert!(line(&b, 35).starts_with(" PREVIEW "));
}

#[test]
fn narrow_widths_drop_panes() {
    let f = fixture();
    let mut a = app(&f, "auth");
    let b = render(&mut a, &f, 80, 24);
    assert!(!has(&b, "FILTERS"));
    assert_eq!(find(&b, "/ auth"), Some((2, 1)));
    assert!(has(&b, "lineage"), "preview kept at 80 columns");
    assert!(line(&b, 23).starts_with(" RESULTS "));
    assert!(line(&b, 23).trim_end().ends_with("? help  q quit"));
    let b = render(&mut a, &f, 50, 15);
    assert!(!has(&b, "FILTERS"));
    assert!(!has(&b, "lineage"), "preview dropped below 70 columns");
    assert!(has(&b, "CLD"));
    assert!(line(&b, 14).starts_with(" RESULTS "));
    // Toggled back on, the panes squeeze in without panicking.
    a.show_filters = Some(true);
    a.show_preview = Some(true);
    let b = render(&mut a, &f, 50, 15);
    assert!(has(&b, "FILTERS"));
}

#[test]
fn tiny_terminals_never_panic() {
    let f = fixture();
    let modes = [
        Mode::Normal,
        Mode::Query,
        Mode::Help,
        Mode::Command,
        Mode::Scope(1),
        Mode::Filter(3),
        Mode::Actions(2),
        Mode::ConfirmExit,
        Mode::ConfirmDelete,
        Mode::Projects {
            query: String::new(),
            cursor: 0,
        },
        Mode::Projects {
            query: "web".into(),
            cursor: 9,
        },
    ];
    for (w, h) in [
        (1, 1),
        (5, 2),
        (12, 3),
        (20, 5),
        (30, 8),
        (69, 10),
        (110, 12),
        (200, 60),
    ] {
        for m in &modes {
            for flip in [false, true] {
                let mut a = app(&f, "auth");
                a.mode = m.clone();
                a.show_filters = Some(flip);
                a.show_preview = Some(flip);
                a.focus = Focus::Preview;
                render(&mut a, &f, w, h);
            }
        }
        let mut a = app(&f, "auth");
        let v = a.preview.as_ref().unwrap().view.clone();
        a.mode = Mode::View(Box::new(v));
        render(&mut a, &f, w, h);
    }
}

#[test]
fn help_overlay_and_full_view() {
    let f = fixture();
    let mut a = app(&f, "auth");
    a.mode = Mode::Help;
    let b = render(&mut a, &f, 140, 36);
    assert!(has(&b, " Keys "));
    assert!(has(&b, "move pane focus"));
    assert!(line(&b, 35).starts_with(" HELP "));
    let v = a.preview.as_ref().unwrap().view.clone();
    a.mode = Mode::View(Box::new(v));
    let b = render(&mut a, &f, 140, 36);
    assert!(line(&b, 35).starts_with(" VIEW "));
    assert!(has(&b, "    you the sync"));
    assert!(has(&b, "lineage  b0b0cafe"));
    assert!(!has(&b, "FILTERS"));
}

#[test]
fn project_picker_overlay() {
    let f = fixture();
    let mut a = app(&f, "auth");
    a.counts.projects = (0..40).map(|i| (format!("proj{i:02}"), 50 - i)).collect();
    a.projects.hidden.insert("proj01".into());
    a.mode = Mode::Normal;
    a.focus = Focus::Filters;
    a.facet_cursor = 4 + facets::TOP_PROJECTS;
    let b = render(&mut a, &f, 140, 36);
    let (mx, my) = find(&b, "…32 more  ⏎ all").expect("more row");
    assert_eq!(b[(mx, my)].fg, theme::DIM);
    assert_eq!(b[(1, my)].symbol(), "▌", "the more row takes the cursor");
    assert!(line(&b, 35).contains("o only"));

    a.mode = Mode::Projects {
        query: "proj".into(),
        cursor: 30,
    };
    let b = render(&mut a, &f, 140, 36);
    assert!(has(&b, " PROJECTS "));
    let (px, py) = find(&b, "/ proj▏").unwrap();
    assert!(line(&b, py).contains("40 projects"));
    assert_eq!(b[(px - 2, py - 1)].fg, theme::ACCENT, "accent border");
    // Scrolled so the cursor row (proj30) is visible, with the bar.
    let (cx, cy) = find(&b, "[x] proj30").unwrap();
    assert_eq!(b[(cx - 2, cy)].symbol(), "▌");
    assert_eq!(b[(cx, cy)].bg, theme::BG_SEL);
    assert!(line(&b, cy).contains(" 20 "));
    assert!(
        find(&b, "proj00").is_none_or(|(x, _)| x < 28),
        "top rows scrolled away (the filter pane behind still lists it)"
    );
    assert!(line(&b, 35).starts_with(" PROJECTS "));
    assert!(line(&b, 35).contains("space toggle  ⏎ only this  ^a all"));
    assert!(line(&b, 35).trim_end().ends_with("esc close"));
    // Unchecked rows are dim; the filter narrows the list.
    a.mode = Mode::Projects {
        query: "OJ0".into(),
        cursor: 0,
    };
    let b = render(&mut a, &f, 140, 36);
    assert!(has(&b, "10 projects"));
    let (ux, uy) = find(&b, "[ ] proj01").unwrap();
    assert_eq!(b[(ux, uy)].fg, theme::DIM);
    for (w, h) in [(10, 4), (30, 6)] {
        render(&mut a, &f, w, h);
    }
}

#[test]
fn titles_drop_paste_wrappers() {
    let mut f = fixture();
    f.rows[1].first_user_msg_content =
        "[Image #1] <pasted_content id=\"p0\">deploy  the\nwebapp stack</pasted_content>".into();
    f.snippets.clear();
    let mut a = app(&f, "auth");
    a.selected = 1;
    a.preview.as_mut().unwrap().view.row = f.rows[1].clone();
    let b = render(&mut a, &f, 140, 36);
    let (_, y) = find(&b, "CDX").unwrap();
    assert!(
        line(&b, y).contains("deploy the webapp stack"),
        "{}",
        line(&b, y)
    );
    assert!(
        line(&b, y + 2).contains("…deploy the webapp stack"),
        "snippet fallback"
    );
    assert!(
        line(&b, 1).contains("deploy the webapp stack"),
        "preview header"
    );
    assert!(!has(&b, "pasted_content") && !has(&b, "[Image"));
}

#[test]
fn layered_spans() {
    let q = [0..3, 8..11];
    let p = vec![4..7, 20..21];
    let hl = theme::hit_current();
    let qs = theme::hit();
    let out = spans(
        "Foo bar foo",
        2..11,
        Style::default(),
        &[(&q, qs), (&p, hl)],
    );
    let texts: Vec<&str> = out.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(texts, ["o", " ", "bar", " ", "foo"]);
    assert_eq!(out[2].style.bg, Some(theme::ACCENT));
    assert_eq!(out[0].style.bg, Some(theme::MATCH));
    let s = Snippet {
        text: "alpha beta\ngamma".into(),
        highlights: std::iter::once(6..10).collect(),
    };
    let one = results::snippet_spans(&s, 12, Style::default(), hl);
    let texts: Vec<&str> = one.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(texts, ["alpha ", "beta", " "]);
}

#[test]
fn pi_and_opencode_badges_facets_and_preview() {
    let mut f = fixture();
    f.rows[0].agent = Agent::Pi;
    f.rows[1].agent = Agent::OpenCode;
    f.rows[1].derivation_type = String::new();
    let mut a = app(&f, "auth");
    let b = render(&mut a, &f, 140, 36);
    let (px, py) = find(&b, "PI ").expect("pi badge");
    assert_eq!((b[(px, py)].fg, b[(px + 1, py)].fg), (theme::PI, theme::PI));
    let (ox, oy) = find(&b, "OPC").expect("opencode badge");
    assert_eq!(b[(ox, oy)].fg, theme::OPENCODE);
    assert!(oy > py, "cards in result order");
    // All four agents in the filter pane, with counts.
    for (i, name) in ["claude", "codex", "pi", "opencode"]
        .into_iter()
        .enumerate()
    {
        let l = line(&b, 4 + i as u16);
        assert!(l[..90].contains(&format!("[x] {name}")), "{l}");
    }
    assert_eq!(b[(25, 4)].symbol(), "1", "claude");
    assert_eq!(b[(25, 5)].symbol(), "0", "codex");
    assert_eq!(b[(25, 6)].symbol(), "1", "pi");
    assert_eq!(b[(25, 7)].symbol(), "1", "opencode");
    // The preview header names the agent; the gutter labels pi's replies.
    assert!(has(&b, "pi · 00000000"));
    let (gx, gy) = find(&b, "    pi dex's default").expect("pi gutter");
    assert_eq!(b[(gx + 4, gy)].fg, theme::PI);
}
