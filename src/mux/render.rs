//! Aligned rows and the `list` table.

use mnemoloom::index::tui::text::{fit, width};

use super::model::{Agent, State};

const W_PANE: usize = 20;
const W_STATE: usize = 5;
const W_KIND: usize = 8;
const W_NAME: usize = 26;
const W_REPO: usize = 22;

/// SGR code per state.
fn sgr(state: State) -> &'static str {
    match state {
        State::Input => "1;31",
        State::Busy => "1;32",
        State::Bg => "1;36",
        State::Idle => "2;37",
    }
}

fn colour(state: State, text: &str, on: bool) -> String {
    if on {
        format!("\x1b[{}m{text}\x1b[0m", sgr(state))
    } else {
        text.to_owned()
    }
}

/// Compact age of the last submitted input.
pub fn input_age(agent: &Agent) -> String {
    match agent.input_age_seconds {
        None => "unknown".into(),
        Some(age) if age >= 86400.0 => format!("{:.1}d", age / 86400.0),
        Some(age) if age >= 3600.0 => format!("{:.1}h", age / 3600.0),
        Some(age) => format!("{}m", (age / 60.0).floor()),
    }
}

/// A row split around the state cell, so callers can style that cell.
pub struct RowParts {
    pub pane: String,
    pub state: String,
    pub rest: String,
}

pub fn row_parts(agent: &Agent) -> RowParts {
    let branch = if agent.branch.is_empty() {
        String::new()
    } else {
        format!("@{}", agent.branch)
    };
    let name = if agent.name.is_empty() {
        "-"
    } else {
        &agent.name
    };
    RowParts {
        pane: format!("{} ", fit(&agent.pane, W_PANE, false)),
        state: format!("{:<W_STATE$}", agent.state.as_str()),
        rest: format!(
            " {:<W_KIND$} {:>9} {:<8} {} {} {}",
            agent.kind.as_str(),
            input_age(agent),
            agent.inactivity.as_str(),
            fit(name, W_NAME, false),
            fit(&format!("{}{branch}", agent.repo), W_REPO, false),
            agent.info,
        ),
    }
}

pub fn row(agent: &Agent, colour_on: bool) -> String {
    let parts = row_parts(agent);
    format!(
        "{}{}{}",
        parts.pane,
        colour(agent.state, &parts.state, colour_on),
        parts.rest
    )
}

/// Summary, header, and one row per agent.
pub fn table(agents: &[Agent], colour_on: bool) -> String {
    if agents.is_empty() {
        return "no agents found".into();
    }
    let summary: Vec<String> = State::ALL
        .iter()
        .filter_map(|&state| {
            let n = agents.iter().filter(|a| a.state == state).count();
            (n > 0).then(|| colour(state, &format!("{}={n}", state.as_str()), colour_on))
        })
        .collect();
    let header = format!(
        "{:<W_PANE$} {:<W_STATE$} {:<W_KIND$} {:>9} {:<8} {:<W_NAME$} {:<W_REPO$} CONTEXT",
        "PANE", "STATE", "KIND", "INPUT AGE", "ACTIVITY", "NAME", "REPO@BRANCH"
    );
    let rule = "─".repeat((width(&header) + 20).min(120));
    let mut lines = vec![
        format!("{} agents   {}", agents.len(), summary.join("  ")),
        rule.clone(),
        header,
        rule,
    ];
    lines.extend(agents.iter().map(|a| row(a, colour_on)));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::model::{Inactivity, Kind};

    fn agent() -> Agent {
        let mut a = Agent::new("work:5.1".into(), "work".into(), Kind::Claude);
        a.state = State::Bg;
        a.name = "Add CLI access to the staging cluster".into();
        a.repo = "webapp".into();
        a.branch = "alice/ABC-123".into();
        a.info = "⏵⏵ bypass permissions on · 1 shell".into();
        a.inactivity = Inactivity::Working;
        a
    }

    #[test]
    fn row_layout() {
        assert_eq!(
            row(&agent(), false),
            "work:5.1             bg    claude     unknown working  Add CLI access to the sta… webapp@alice/ABC-123   ⏵⏵ bypass permissions on · 1 shell"
        );
        assert_eq!(
            row(&agent(), true).split_whitespace().nth(1),
            Some("\x1b[1;36mbg")
        );
        let mut a = agent();
        let baseline = row_parts(&a);
        a.pane = "日本😀".repeat(10);
        a.name = "日本😀".repeat(10);
        a.repo = "日本😀".repeat(10);
        let wide = row_parts(&a);
        assert_eq!(width(&wide.pane), width(&baseline.pane));
        assert_eq!(width(&wide.rest), width(&baseline.rest));
    }

    #[test]
    fn kind_column_fits_every_kind() {
        let mut a = agent();
        let baseline = row_parts(&a);
        for (kind, text) in [(Kind::Pi, " pi       "), (Kind::OpenCode, " opencode ")] {
            a.kind = kind;
            let parts = row_parts(&a);
            assert!(parts.rest.starts_with(text), "{}", parts.rest);
            assert_eq!(width(&parts.rest), width(&baseline.rest));
        }
    }

    #[test]
    fn ages() {
        let mut a = agent();
        for (age, text) in [
            (None, "unknown"),
            (Some(59.0), "0m"),
            (Some(3599.0), "59m"),
            (Some(3600.0), "1.0h"),
            (Some(112320.0), "1.3d"),
        ] {
            a.input_age_seconds = age;
            assert_eq!(input_age(&a), text);
        }
    }

    #[test]
    fn table_layout() {
        assert_eq!(table(&[], false), "no agents found");
        let mut idle = agent();
        idle.state = State::Idle;
        let t = table(&[agent(), idle], false);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "2 agents   bg=1  idle=1");
        assert_eq!(lines[1].chars().count(), 120);
        assert_eq!(
            lines[2],
            "PANE                 STATE KIND     INPUT AGE ACTIVITY NAME                       REPO@BRANCH            CONTEXT"
        );
        assert_eq!(lines.len(), 6);
    }
}
