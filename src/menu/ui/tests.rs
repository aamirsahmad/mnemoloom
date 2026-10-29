use super::footer::footer_line;
use super::session_pane::session_lines;
use super::*;
use crate::cli::StartScreen;
use crate::index::tui::text::width as display_width;
use crate::menu::session::fixture::view;
use crate::menu::session::{Details, LineageNode};
use crate::menu::state::{DirInfo, Key, TrimInfo};
use crate::session::Derivation;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

const ID: &str = "c1a0de00-1234-4abc-8def-012345678916";

fn render(state: &State, sessions: &[SessionView], w: u16, h: u16) -> Buffer {
    let ctx = Ctx {
        sessions,
        results_title: " Select a session ",
        scope_line: Some("Latest Claude/Codex sessions for 'proj'"),
    };
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, state, &ctx)).unwrap();
    term.backend().buffer().clone()
}

fn rows(buf: &Buffer) -> Vec<String> {
    let a = buf.area;
    (0..a.height)
        .map(|y| (0..a.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

fn text(buf: &Buffer) -> String {
    rows(buf).join("\n")
}

/// Render after `keys`, wide (120x30) and narrow (60x18).
fn at(sessions: &[SessionView], start: StartScreen, keys: &[Key]) -> (State, Buffer, Buffer) {
    let mut s = State::new(start, sessions.len());
    for &k in keys {
        s.handle(k, sessions);
    }
    let (wide, narrow) = (render(&s, sessions, 120, 30), render(&s, sessions, 60, 18));
    (s, wide, narrow)
}

fn one() -> Vec<SessionView> {
    vec![view(Agent::Claude, ID)]
}

/// Position of `needle` as (column, row).
fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
    rows(buf)
        .iter()
        .enumerate()
        .find_map(|(y, r)| {
            let i = r.find(needle)?;
            Some((display_width(&r[..i]) as u16, y as u16))
        })
        .unwrap_or_else(|| panic!("{needle:?} not in\n{}", text(buf)))
}

#[test]
fn action_screen_wide() {
    let (_, b, _) = at(&one(), StartScreen::Action, &[Key::Down]);
    let t = text(&b);
    for s in [
        "╭ SESSION ",
        "╭ ACTIONS ",
        "set up the spec harness",
        "claude · c1a0de00 · 3 user msgs · Oct",
        "/work/proj @ main",
        "First user message",
        "Last user message",
        "fix the failing spec",
        " 1  Show session file path",
        "▌ 2  Copy session file",
        " 5  Resume/trim session...",
        " ACTIONS   ⏎ select  j/k move  1-9 jump  esc back",
    ] {
        assert!(t.contains(s), "{s:?} not in\n{t}");
    }
    assert!(rows(&b)[29].ends_with("? help"));
    // SESSION left of ACTIONS, side by side.
    let (sx, sy) = find(&b, "╭ SESSION");
    let (ax, ay) = find(&b, "╭ ACTIONS");
    assert!(sx < ax && sy == ay);
    // Selected row: accent bar on the selection background.
    let (x, y) = find(&b, "▌ 2  Copy");
    assert_eq!(b[(x, y)].fg, theme::ACCENT);
    assert_eq!(b[(x + 6, y)].bg, theme::BG_SEL);
    assert_eq!(b[(x + 6, y - 1)].bg, theme::BG);
    // Focused pane border is ACCENT, the other BORDER; chip inverse.
    assert_eq!(b[(ax, ay)].fg, theme::ACCENT);
    assert_eq!(b[(sx, sy)].fg, theme::BORDER);
    assert_eq!(b[(1, 29)].bg, theme::ACCENT);
}

#[test]
fn narrow_hides_session_pane() {
    let (_, _, b) = at(&one(), StartScreen::Action, &[]);
    let t = text(&b);
    assert!(!t.contains("SESSION"), "{t}");
    assert!(t.contains("╭ ACTIONS CLD c1a0de00 "), "{t}");
    assert!(t.contains("▌ 1  Show session file path"));
    assert!(t.contains(" ACTIONS   ⏎ select  j/k move  1-9 jump  esc back"));
    // Full width is 70+; one column less already drops the pane.
    let s = State::new(StartScreen::Action, 1);
    assert!(text(&render(&s, &one(), NARROW, 20)).contains("SESSION"));
    assert!(!text(&render(&s, &one(), NARROW - 1, 20)).contains("SESSION"));
}

#[test]
fn header_lineage_and_sidechain() {
    let mut ss = one();
    ss[0].derivation = Derivation::Trimmed;
    ss[0].is_sidechain = true;
    ss[0].details = Some(Details {
        title: Some("Fix login flow".into()),
        lineage: vec![
            LineageNode {
                file: format!("{ID}.jsonl"),
                derivation: Derivation::Trimmed,
            },
            LineageNode {
                file: "b0b0cafe-0000-0000-0000-000000000000.jsonl".into(),
                derivation: Derivation::Original,
            },
        ],
        ..ss[0].details.clone().unwrap()
    });
    let (_, b, _) = at(&ss, StartScreen::Action, &[]);
    let t = text(&b);
    assert!(t.contains("│ Fix login flow"), "{t}");
    assert!(t.contains("lineage  b0b0cafe ─trim─▶ c1a0de00"));
    assert!(t.contains("⚠ Sub-agent session (not resumable)"));
    assert!(!t.contains("Resume/trim"), "sub-agents have no resume item");
    let (x, y) = find(&b, "lineage");
    assert_eq!(b[(x, y)].fg, theme::LINK);
    let lines = session_lines(&ss[0], 40);
    assert_eq!(lines[0].spans[0].content, "Fix login flow");
    // Details still loading.
    ss[0].details = None;
    let t = text(&at(&ss, StartScreen::Action, &[]).1);
    assert!(t.contains("loading…") && t.contains("│ set up the spec harness"));
}

#[test]
fn header_uses_display_title() {
    let mut ss = one();
    ss[0].details.as_mut().unwrap().title = None;
    ss[0].first_title = Some("see https://x.dev deploy".into());
    ss[0].first_message = Some("[Image #1] see https://x.dev deploy".into());
    let lines = session_lines(&ss[0], 60);
    assert_eq!(lines[0].spans[0].content, "see https://x.dev deploy");
    let body: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert!(body.contains(&"[Image #1] see https://x.dev deploy".to_string()));
    assert!(
        body[1].starts_with("claude · c1a0de00 · 3 user msgs · "),
        "{body:?}"
    );
    ss.push(view(Agent::Codex, "bbbb2222-y"));
    let (_, b, _) = at(&ss, StartScreen::Action, &[]);
    assert!(
        text(&b).contains(" 1  CLD see https://x.dev deploy"),
        "{}",
        text(&b)
    );
}

#[test]
fn submenus() {
    let (_, b, n) = at(&one(), StartScreen::Resume, &[]);
    let t = text(&b);
    assert!(t.contains("╭ RESUME / TRIM "));
    for l in [
        "▌ 1  Resume as-is",
        " 2  Clone session and resume clone",
        " 3  Trim + resume...",
        " 4  Smart trim + resume...",
        " 5  Rollover: handoff work to fresh session...",
    ] {
        assert!(t.contains(l), "{l}");
    }
    assert!(text(&n).contains(" 5  Rollover: handoff work to fresh"));
    let t = text(&at(&one(), StartScreen::TrimMenu, &[]).1);
    assert!(t.contains("╭ TRIM SESSION "));
    assert!(t.contains("▌ 1  Trim + resume..."));
}

#[test]
fn trim_form() {
    let (_, b, n) = at(&one(), StartScreen::TrimMenu, &[Key::Enter, Key::Enter]);
    let t = text(&b);
    assert!(t.contains("╭ TRIM OPTIONS "));
    assert!(t.contains("  Tools to trim"));
    assert!(t.contains("│   comma-separated, e.g. bash,read,edit"));
    assert!(t.contains("│   › (all tools)"));
    assert!(t.contains("▌ Length threshold in characters"));
    assert!(t.contains("│   › 500 "));
    assert!(t.contains("│   › (skip)"));
    assert!(t.contains(" TRIM   ⏎ next/submit  ↑/↓ field"));
    assert!(rows(&b)[29].ends_with("esc back"));
    let (x, y) = find(&b, "▌ Length");
    assert_eq!(b[(x + 2, y)].fg, theme::ACCENT);
    let (x, y) = find(&b, "  Tools to trim");
    assert_eq!(b[(x + 2, y)].fg, theme::DIM);
    let (x, y) = find(&b, "500");
    assert_eq!((b[(x, y)].fg, b[(x + 3, y)].bg), (theme::FG, theme::FG));
    // Narrow: hints wrap; the focused field scrolls into view.
    let (_, _, n2) = at(
        &one(),
        StartScreen::TrimMenu,
        &[Key::Enter, Key::Enter, Key::Enter],
    );
    assert!(text(&n).contains("▌ Length threshold"));
    let t = text(&n2);
    assert!(t.contains("▌ Trim assistant messages (optional)"), "{t}");
    assert!(t.contains("(skip)"), "{t}");
}

#[test]
fn smart_trim_and_query() {
    let (_, b, _) = at(&one(), StartScreen::SmartTrimForm, &[]);
    let t = text(&b);
    assert!(t.contains("╭ SMART TRIM "));
    assert!(t.contains("▌ Trim instructions"));
    assert!(t.contains("Tell the agent what to look for when trimming"));
    assert!(t.contains("trim messages that are not relevant"));
    let (x, y) = find(&b, "trim messages that");
    assert_eq!(b[(x, y)].fg, theme::DIM, "untyped default is a placeholder");

    let (_, b, n) = at(&one(), StartScreen::Query, &[]);
    let t = text(&b);
    assert!(t.contains("╭ QUERY "));
    assert!(t.contains("Summarize what was accomplished in this session"));
    assert!(t.contains(" QUERY   ⏎ run query"));
    assert!(!t.contains("? help"), "`?` is text here");
    assert!(text(&n).contains("Summarize what was"));
    let t = text(
        &at(
            &one(),
            StartScreen::Query,
            &[Key::Char('w'), Key::Char('?')],
        )
        .1,
    );
    assert!(t.contains("│   › w? "), "{t}");
    let t = text(&at(&one(), StartScreen::Query, &[Key::Char(' '), Key::Enter]).1);
    assert!(t.contains("Query cannot be empty"));
    assert!(t.contains("⏎/esc back"));
}

#[test]
fn lineage_and_rollover() {
    let t = text(&at(&one(), StartScreen::Lineage, &[]).1);
    assert!(t.contains("╭ LINEAGE "));
    assert!(t.contains("This is the original session (no continuation history)"));
    assert!(t.contains(" LINEAGE   ⏎ options  esc back"));
    let (_, b, n) = at(&one(), StartScreen::Lineage, &[Key::Enter]);
    let t = text(&b);
    assert!(t.contains("╭ ROLLOVER "));
    assert!(t.contains("▌ Agent"));
    assert!(t.contains("  [1] Claude    2  Codex"));
    assert!(t.contains("  [1] Quick Resume"));
    assert!(t.contains("   2  Resume with Context Recovery"));
    assert!(!t.contains("Custom context recovery"));
    assert!(text(&n).contains("▌ Agent"));
    let ks = [Key::Enter, Key::Enter, Key::Char('2'), Key::Enter];
    let (_, b, n) = at(&one(), StartScreen::Lineage, &ks);
    let t = text(&b);
    assert!(t.contains("  [2] Resume with Context Recovery"));
    assert!(t.contains("▌ Custom context recovery instructions (optional)"));
    assert!(t.contains("(default: recover context of last task)"));
    let t = text(&n);
    assert!(
        t.contains("▌ Custom context recovery"),
        "scrolled into view:\n{t}"
    );
}

#[test]
fn nonlaunch_screens() {
    let (_, b, _) = at(&one(), StartScreen::Action, &[Key::Enter]);
    let t = text(&b);
    assert!(t.contains("╭ PATH "));
    assert!(t.contains(&format!("/tmp/{ID}.jsonl")));
    assert!(t.contains(" PATH   ⏎ print & exit  esc back"));
    let t = text(&at(&one(), StartScreen::Action, &[Key::Char('3'), Key::Enter]).1);
    assert!(t.contains("╭ EXPORT "));
    assert!(t.contains("▌ Export path"));
    assert!(t.contains("/work/proj/exported-sessions/claude/x.txt"));
    let ks = [Key::Char('2'), Key::Enter, Key::Char('/'), Key::Char('o')];
    let t = text(&at(&one(), StartScreen::Action, &ks).1);
    assert!(t.contains("╭ COPY ") && t.contains("│   › /o "), "{t}");
    let ks = [Key::Char('2'), Key::Enter, Key::Enter];
    let t = text(&at(&one(), StartScreen::Action, &ks).1);
    assert!(t.contains("Destination required"));
}

#[test]
fn results_picker() {
    let mut ss = vec![
        view(Agent::Claude, "aaaa1111-x"),
        view(Agent::Codex, "bbbb2222-y"),
    ];
    ss[1].derivation = Derivation::Trimmed;
    ss[1].first_message = None;
    ss[1].first_title = None;
    ss[1].details = None;
    let (_, b, n) = at(&ss, StartScreen::Action, &[Key::Down]);
    let t = text(&b);
    assert!(t.contains("╭ SELECT A SESSION "), "{t}");
    assert!(t.contains("Latest Claude/Codex sessions for 'proj'"));
    assert!(t.contains(" 1  CLD set up the spec harness"));
    assert!(t.contains("proj @ main · 3 user msgs"));
    assert!(t.contains("▌ 2  CDX bbbb2222"));
    assert!(t.contains("proj @ main · (t)"));
    assert!(t.contains("(t)=trimmed"));
    assert!(t.contains(" SESSIONS   ⏎ select  j/k move  1-9 jump  esc quit"));
    // SESSION pane follows the hovered candidate.
    assert!(t.contains("codex · bbbb2222"), "{t}");
    let (x, y) = find(&b, " 2  CDX");
    assert_eq!(b[(x - 1, y)].fg, theme::ACCENT);
    assert_eq!(b[(x, y + 1)].bg, theme::BG_SEL);
    let t = text(&n);
    assert!(
        t.contains("▌ 2  CDX bbbb2222") && !t.contains("╭ SESSION"),
        "{t}"
    );
    assert!(
        t.contains("╭ SELECT A SESSION ─"),
        "no compact badge on results"
    );
}

#[test]
fn dialogs() {
    let s = State::dialog(Screen::TrimConfirm(TrimInfo {
        new_session_id: Some("0123456789abcdef".into()),
        lines_trimmed: 7,
        tokens_saved: 12345,
        output_file: "/x/new.jsonl".into(),
        ..TrimInfo::default()
    }));
    for b in [render(&s, &[], 120, 30), render(&s, &[], 60, 18)] {
        let t = text(&b);
        assert!(t.contains("╭ TRIM COMPLETE "), "{t}");
        assert!(t.contains("✓ New session: 0123456789ab..."));
        assert!(t.contains("✓ Lines trimmed: 7"));
        assert!(t.contains("✓ Tokens saved: ~12,345"));
        assert!(t.contains("  /x/new.jsonl"));
        assert!(t.contains("▌ 1  Resume trimmed session"));
        assert!(t.contains(" 2  Delete session file & exit"));
        assert!(!t.contains("╭ SESSION"));
    }
    let t = text(&render(&s, &[], 120, 30));
    assert!(t.contains("esc cancel (keep file)"));
    let s = State::dialog(Screen::TrimConfirm(TrimInfo {
        nothing_to_trim: true,
        ..TrimInfo::default()
    }));
    let t = text(&render(&s, &[], 120, 30));
    assert!(t.contains("╭ NOTHING TO TRIM "));
    assert!(t.contains("✓ Session is already well-optimized"));
    assert!(t.contains("▌ 1  Resume original session"));
    assert!(t.contains(" 2  Back to menu"));
    let s = State::dialog(Screen::DirConfirm(DirInfo {
        current_dir: "/a".into(),
        session_dir: "/b".into(),
    }));
    let t = text(&render(&s, &[], 60, 18));
    assert!(t.contains("╭ DIFFERENT DIRECTORY "));
    assert!(t.contains("│   Current") && t.contains("│   › /a") && t.contains("│   › /b"));
    assert!(t.contains("▌ 1  Yes, change directory and proceed"));
    assert!(t.contains(" 2  No, proceed without changing directory"));
    assert!(t.contains(" CONFIRM "));
}

#[test]
fn help_overlay() {
    let (s, b, n) = at(&one(), StartScreen::Action, &[Key::Char('?')]);
    assert!(s.help);
    let t = text(&b);
    assert!(t.contains("╭ KEYS "), "{t}");
    assert!(t.contains("  ⏎  select"));
    assert!(t.contains("esc  back"));
    assert!(t.contains("  q  same as esc"));
    assert!(t.contains(" ^c  quit now"));
    assert!(t.contains(" HELP   any key close"));
    assert!(text(&n).contains("╭ KEYS "));
    let t = text(&at(&one(), StartScreen::Action, &[Key::Char('?'), Key::Esc]).1);
    assert!(!t.contains("KEYS"));
}

#[test]
fn footer_fits_width() {
    let s = State::new(StartScreen::Action, 1);
    for w in [0, 5, 12, 30, 60, 200] {
        let l = footer_line(&s, w);
        assert!(l.width() <= usize::from(w).max(9), "{w}: {l:?}");
    }
    assert_eq!(footer_line(&s, 120).width(), 120);
}

#[test]
fn tiny_terminals_do_not_panic() {
    let ss = one();
    let screens = [
        State::new(StartScreen::Action, 1),
        State::new(StartScreen::Lineage, 1),
        State::new(StartScreen::Query, 1),
        State::dialog(Screen::DirConfirm(DirInfo::default())),
    ];
    for s in &screens {
        for (w, h) in [(1, 1), (2, 3), (20, 5), (69, 4), (70, 2)] {
            render(s, &ss, w, h);
        }
    }
    let mut s = State::new(StartScreen::Action, 1);
    s.handle(Key::Char('?'), &ss);
    for (w, h) in [(1, 1), (8, 3), (20, 5)] {
        render(&s, &ss, w, h);
    }
}
