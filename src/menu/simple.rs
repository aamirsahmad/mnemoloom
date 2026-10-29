//! `--simple-ui` (and non-TTY) numbered text menu. Generic over input/output so it is testable.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use super::forms::{RolloverOptions, RolloverType, SMART_TRIM_DEFAULT, TrimOptions};
use super::session::SessionView;
use super::state::Action;

/// A user's answer to a choice prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice<T> {
    Pick(T),
    Invalid,
    /// Enter on a menu without a default.
    Cancel,
}

/// Main menu choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainChoice {
    ResumeMenu,
    Path,
    Copy,
    Clone,
    Export,
    Continue,
}

/// Resume submenu choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeChoice {
    Resume,
    Trim,
    SmartTrim,
}

/// Parse the action-menu answer. Sub-agent sessions have no default.
pub fn parse_main(answer: &str, is_sidechain: bool) -> Choice<MainChoice> {
    use MainChoice::*;
    let answer = answer.trim();
    if is_sidechain {
        return match answer {
            "" => Choice::Cancel,
            "1" => Choice::Pick(Path),
            "2" => Choice::Pick(Copy),
            "3" => Choice::Pick(Export),
            _ => Choice::Invalid,
        };
    }
    match answer {
        "" | "1" => Choice::Pick(ResumeMenu),
        "2" => Choice::Pick(Path),
        "3" => Choice::Pick(Copy),
        "4" => Choice::Pick(Clone),
        "5" => Choice::Pick(Export),
        "6" => Choice::Pick(Continue),
        _ => Choice::Invalid,
    }
}

/// Parse the read-only action menu (pi / opencode: resume, path, export;
/// sub-agent sessions: path, export, no default).
pub fn parse_read_only(answer: &str, is_sidechain: bool) -> Choice<MainChoice> {
    use MainChoice::*;
    match (answer.trim(), is_sidechain) {
        ("", true) => Choice::Cancel,
        ("1", true) => Choice::Pick(Path),
        ("2", true) => Choice::Pick(Export),
        ("" | "1", false) => Choice::Pick(ResumeMenu),
        ("2", false) => Choice::Pick(Path),
        ("3", false) => Choice::Pick(Export),
        _ => Choice::Invalid,
    }
}

/// Parse the resume-submenu answer (Enter = resume as is).
pub fn parse_resume(answer: &str) -> Choice<ResumeChoice> {
    match answer.trim() {
        "" | "1" => Choice::Pick(ResumeChoice::Resume),
        "2" => Choice::Pick(ResumeChoice::Trim),
        "3" => Choice::Pick(ResumeChoice::SmartTrim),
        _ => Choice::Invalid,
    }
}

/// Parse the candidate-picker answer (Enter = first).
pub fn parse_index(answer: &str, len: usize) -> Choice<usize> {
    match answer.trim() {
        "" => Choice::Pick(0),
        a => match a.parse::<usize>() {
            Ok(n) if (1..=len).contains(&n) => Choice::Pick(n - 1),
            _ => Choice::Invalid,
        },
    }
}

/// Parse the three trim prompts; `Err` on a non-integer number.
pub fn parse_trim(
    tools: &str,
    threshold: &str,
    assistant: &str,
) -> Result<TrimOptions, std::num::ParseIntError> {
    let tools = tools.trim();
    let threshold = threshold.trim();
    let assistant = assistant.trim();
    Ok(TrimOptions {
        tools: (!tools.is_empty()).then(|| tools.to_string()),
        threshold: if threshold.is_empty() {
            500
        } else {
            threshold.parse()?
        },
        trim_assistant: if assistant.is_empty() {
            None
        } else {
            Some(assistant.parse()?)
        },
    })
}

/// Prompt-driven session: reads answers from `input`, writes to `out`.
pub struct Prompter<R, W> {
    pub input: R,
    pub out: W,
}

impl<R: BufRead, W: Write> Prompter<R, W> {
    /// Print `prompt` and read a line; `None` on EOF (prints `Cancelled.`).
    fn ask(&mut self, prompt: &str) -> io::Result<Option<String>> {
        write!(self.out, "{prompt}")?;
        self.out.flush()?;
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            writeln!(self.out, "\nCancelled.")?;
            return Ok(None);
        }
        Ok(Some(line.trim().to_string()))
    }

    /// Resolve a [`Choice`]: printing `Invalid choice.` / `Cancelled.`.
    fn settle<T>(&mut self, choice: Choice<T>) -> io::Result<Option<T>> {
        match choice {
            Choice::Pick(t) => Ok(Some(t)),
            Choice::Invalid => {
                writeln!(self.out, "Invalid choice.")?;
                Ok(None)
            }
            Choice::Cancel => {
                writeln!(self.out, "Cancelled.")?;
                Ok(None)
            }
        }
    }

    /// Full flow: pick a candidate (if several), then an action.
    pub fn run(
        &mut self,
        sessions: &[SessionView],
        scope_line: Option<&str>,
    ) -> io::Result<Option<(usize, Action)>> {
        let index = match sessions.len() {
            0 => return Ok(None),
            1 => 0,
            n => {
                if let Some(scope) = scope_line {
                    writeln!(self.out, "\n{scope}")?;
                }
                writeln!(self.out)?;
                for (i, s) in sessions.iter().enumerate() {
                    writeln!(
                        self.out,
                        "{}. [{}] {} | {} | {} | {}",
                        i + 1,
                        s.agent.label(),
                        s.session_id.chars().take(8).collect::<String>(),
                        s.project,
                        s.branch.as_deref().unwrap_or(""),
                        s.date_range()
                    )?;
                }
                writeln!(self.out)?;
                let Some(a) = self.ask(&format!("Enter choice [1-{n}] (or Enter for 1): "))? else {
                    return Ok(None);
                };
                match self.settle(parse_index(&a, n))? {
                    Some(i) => i,
                    None => return Ok(None),
                }
            }
        };
        Ok(self.action(&sessions[index])?.map(|a| (index, a)))
    }

    fn header(&mut self, s: &SessionView) -> io::Result<()> {
        let id: String = s.session_id.chars().take(8).collect();
        writeln!(self.out, "\n=== Session: {id}... ===")?;
        writeln!(self.out, "Agent: {}", s.agent.as_str().to_uppercase())?;
        writeln!(self.out, "Project: {}", s.project)?;
        if let Some(b) = &s.branch {
            writeln!(self.out, "Branch: {b}")?;
        }
        Ok(())
    }

    /// The action menu for one session.
    pub fn action(&mut self, s: &SessionView) -> io::Result<Option<Action>> {
        self.header(s)?;
        if !s.agent.is_rewritable() {
            return self.read_only_action(s);
        }
        let prompt = if s.is_sidechain {
            writeln!(
                self.out,
                "\n[Note: This is a sub-agent session and cannot be resumed directly]"
            )?;
            writeln!(self.out, "\nWhat would you like to do?")?;
            writeln!(self.out, "1. Show session file path")?;
            writeln!(
                self.out,
                "2. Copy session file to file (*.jsonl) or directory"
            )?;
            writeln!(self.out, "3. Export to text file (.txt)")?;
            "Enter choice [1-3] (or Enter to cancel): "
        } else {
            writeln!(self.out, "\nWhat would you like to do?")?;
            writeln!(self.out, "1. Resume session (default)")?;
            writeln!(self.out, "2. Show session file path")?;
            writeln!(
                self.out,
                "3. Copy session file to file (*.jsonl) or directory"
            )?;
            writeln!(self.out, "4. Clone session and resume clone")?;
            writeln!(self.out, "5. Export to text file (.txt)")?;
            writeln!(self.out, "6. Continue with context in fresh session")?;
            "Enter choice [1-6] (or Enter for 1): "
        };
        writeln!(self.out)?;
        let Some(a) = self.ask(prompt)? else {
            return Ok(None);
        };
        let Some(choice) = self.settle(parse_main(&a, s.is_sidechain))? else {
            return Ok(None);
        };
        match choice {
            MainChoice::ResumeMenu => self.resume_menu(),
            MainChoice::Path => Ok(Some(Action::Path)),
            MainChoice::Copy => self.copy(),
            MainChoice::Clone => Ok(Some(Action::Clone)),
            MainChoice::Export => self.export(s),
            MainChoice::Continue => self.rollover(s),
        }
    }

    /// Action menu for sessions mnemoloom can't rewrite (pi, opencode).
    fn read_only_action(&mut self, s: &SessionView) -> io::Result<Option<Action>> {
        writeln!(self.out, "\nWhat would you like to do?")?;
        let prompt = if s.is_sidechain {
            writeln!(self.out, "1. Show session file path")?;
            writeln!(self.out, "2. Export to text file (.txt)")?;
            "Enter choice [1-2] (or Enter to cancel): "
        } else {
            writeln!(self.out, "1. Resume session (default)")?;
            writeln!(self.out, "2. Show session file path")?;
            writeln!(self.out, "3. Export to text file (.txt)")?;
            "Enter choice [1-3] (or Enter for 1): "
        };
        writeln!(self.out)?;
        let Some(a) = self.ask(prompt)? else {
            return Ok(None);
        };
        match self.settle(parse_read_only(&a, s.is_sidechain))? {
            Some(MainChoice::ResumeMenu) => Ok(Some(Action::Resume)),
            Some(MainChoice::Path) => Ok(Some(Action::Path)),
            Some(MainChoice::Export) => self.export(s),
            _ => Ok(None),
        }
    }

    fn resume_menu(&mut self) -> io::Result<Option<Action>> {
        writeln!(self.out, "\nResume options:")?;
        writeln!(self.out, "1. Default, just resume as is (default)")?;
        writeln!(
            self.out,
            "2. Trim session (tool results + assistant messages) and resume"
        )?;
        writeln!(
            self.out,
            "3. Smart trim (EXPERIMENTAL - using Claude SDK agents) and resume"
        )?;
        writeln!(self.out)?;
        let Some(a) = self.ask("Enter choice [1-3] (or Enter for 1): ")? else {
            return Ok(None);
        };
        match self.settle(parse_resume(&a))? {
            Some(ResumeChoice::Resume) => Ok(Some(Action::Resume)),
            Some(ResumeChoice::Trim) => self.trim(),
            Some(ResumeChoice::SmartTrim) => Ok(Some(Action::SmartTrim {
                instructions: SMART_TRIM_DEFAULT.to_string(),
            })),
            None => Ok(None),
        }
    }

    /// Trim options prompt.
    fn trim(&mut self) -> io::Result<Option<Action>> {
        writeln!(self.out, "\nTrim session options:")?;
        writeln!(
            self.out,
            "Enter tool names to trim (comma-separated, e.g., 'bash,read,edit')"
        )?;
        writeln!(self.out, "Or press Enter to trim all tools:")?;
        let Some(tools) = self.ask("Tools (or Enter for all): ")? else {
            return Ok(None);
        };
        writeln!(
            self.out,
            "\nEnter length threshold in characters (default: 500):"
        )?;
        let Some(threshold) = self.ask("Threshold (or Enter for 500): ")? else {
            return Ok(None);
        };
        writeln!(self.out, "\nTrim assistant messages (optional):")?;
        writeln!(
            self.out,
            "  • Positive number (e.g., 10): Trim first 10 messages exceeding threshold"
        )?;
        writeln!(
            self.out,
            "  • Negative number (e.g., -5): Trim all except last 5 messages exceeding threshold"
        )?;
        writeln!(
            self.out,
            "  • Press Enter to skip (no assistant message trimming)"
        )?;
        let Some(assistant) = self.ask("Assistant messages (or Enter to skip): ")? else {
            return Ok(None);
        };
        match parse_trim(&tools, &threshold, &assistant) {
            Ok(opts) => Ok(Some(Action::Trim(opts))),
            Err(_) => {
                writeln!(self.out, "Invalid value entered.")?;
                Ok(None)
            }
        }
    }

    /// Copy destination prompt.
    fn copy(&mut self) -> io::Result<Option<Action>> {
        match self.ask("\nEnter destination file or directory path: ")? {
            Some(dest) if !dest.is_empty() => Ok(Some(Action::Copy { dest })),
            Some(_) => {
                writeln!(self.out, "Cancelled.")?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Export path prompt.
    fn export(&mut self, s: &SessionView) -> io::Result<Option<Action>> {
        writeln!(
            self.out,
            "\nDefault export path: {}",
            s.default_export_path.display()
        )?;
        Ok(self
            .ask("Path (or Enter for default): ")?
            .map(|p| Action::Export {
                dest: if p.is_empty() {
                    s.default_export_path.clone()
                } else {
                    PathBuf::from(p)
                },
            }))
    }

    /// Rollover custom-instructions prompt
    /// (context recovery, same agent).
    fn rollover(&mut self, s: &SessionView) -> io::Result<Option<Action>> {
        writeln!(
            self.out,
            "\nEnter custom summarization instructions (or press Enter to skip):"
        )?;
        Ok(self.ask("> ")?.map(|prompt| {
            Action::Rollover(RolloverOptions {
                agent: s.agent,
                rollover_type: RolloverType::Context,
                prompt,
            })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::session::fixture::view;
    use crate::session::Agent;

    fn run(sessions: &[SessionView], input: &str) -> (Option<(usize, Action)>, String) {
        let mut p = Prompter {
            input: input.as_bytes(),
            out: Vec::new(),
        };
        let r = p
            .run(sessions, Some("Latest Claude/Codex sessions for 'proj'"))
            .unwrap();
        (r, String::from_utf8(p.out).unwrap())
    }

    #[test]
    fn parsing() {
        assert_eq!(parse_main("", false), Choice::Pick(MainChoice::ResumeMenu));
        assert_eq!(parse_main(" 6 ", false), Choice::Pick(MainChoice::Continue));
        assert_eq!(parse_main("7", false), Choice::Invalid);
        assert_eq!(parse_main("", true), Choice::Cancel);
        assert_eq!(parse_main("3", true), Choice::Pick(MainChoice::Export));
        assert_eq!(parse_main("4", true), Choice::Invalid);
        assert_eq!(parse_resume(""), Choice::Pick(ResumeChoice::Resume));
        assert_eq!(parse_resume("3"), Choice::Pick(ResumeChoice::SmartTrim));
        assert_eq!(parse_resume("x"), Choice::Invalid);
        assert_eq!(parse_index("", 2), Choice::Pick(0));
        assert_eq!(parse_index("2", 2), Choice::Pick(1));
        assert_eq!(parse_index("3", 2), Choice::Invalid);
        assert_eq!(parse_index("0", 2), Choice::Invalid);
        assert_eq!(
            parse_trim("", "", "").unwrap(),
            TrimOptions {
                tools: None,
                threshold: 500,
                trim_assistant: None
            }
        );
        assert_eq!(
            parse_trim(" bash ", "300", "-5").unwrap(),
            TrimOptions {
                tools: Some("bash".into()),
                threshold: 300,
                trim_assistant: Some(-5)
            }
        );
        assert!(parse_trim("", "abc", "").is_err());
    }

    #[test]
    fn default_is_resume() {
        let s = [view(Agent::Claude, "c1a0de00-1234-4abc-8def-012345678916")];
        let (r, out) = run(&s, "\n\n");
        assert_eq!(r, Some((0, Action::Resume)));
        assert!(out.starts_with(
            "\n=== Session: c1a0de00... ===\nAgent: CLAUDE\nProject: proj\nBranch: main\n"
        ));
        assert!(out.contains(
            "6. Continue with context in fresh session\n\nEnter choice [1-6] (or Enter for 1): "
        ));
        assert!(out.contains("Resume options:\n1. Default, just resume as is (default)"));
    }

    #[test]
    fn trim_flow_and_errors() {
        let s = [view(Agent::Claude, "abc")];
        let (r, out) = run(&s, "1\n2\nbash\n\n3\n");
        assert_eq!(
            r,
            Some((
                0,
                Action::Trim(TrimOptions {
                    tools: Some("bash".into()),
                    threshold: 500,
                    trim_assistant: Some(3)
                })
            ))
        );
        assert!(out.contains("Assistant messages (or Enter to skip): "));
        let (r, out) = run(&s, "1\n2\n\nzz\n\n");
        assert_eq!(r, None);
        assert!(out.ends_with("Invalid value entered.\n"));
        let (r, out) = run(&s, "9\n");
        assert_eq!(r, None);
        assert!(out.ends_with("Invalid choice.\n"));
        let (r, out) = run(&s, "");
        assert_eq!(r, None);
        assert!(out.ends_with("\nCancelled.\n"));
    }

    #[test]
    fn read_only_agents_offer_resume_path_export() {
        let s = [view(Agent::Pi, "abc")];
        let (got, out) = run(&s, "\n");
        assert_eq!(got, Some((0, Action::Resume)));
        assert!(!out.contains("Copy") && !out.contains("Clone"), "{out}");
        assert_eq!(run(&s, "2\n").0, Some((0, Action::Path)));
        assert!(matches!(
            run(&s, "3\n\n").0,
            Some((0, Action::Export { .. }))
        ));
        assert_eq!(run(&s, "4\n").0, None);
        let mut sub = view(Agent::OpenCode, "abc");
        sub.is_sidechain = true;
        assert_eq!(run(&[sub.clone()], "\n").0, None);
        assert!(matches!(
            run(&[sub], "2\n\n").0,
            Some((0, Action::Export { .. }))
        ));
    }

    #[test]
    fn other_actions() {
        let s = [view(Agent::Codex, "abc")];
        assert_eq!(run(&s, "2\n").0, Some((0, Action::Path)));
        assert_eq!(
            run(&s, "3\n/tmp/x\n").0,
            Some((
                0,
                Action::Copy {
                    dest: "/tmp/x".into()
                }
            ))
        );
        assert_eq!(run(&s, "3\n\n").0, None);
        assert_eq!(run(&s, "4\n").0, Some((0, Action::Clone)));
        assert_eq!(
            run(&s, "5\n\n").0,
            Some((
                0,
                Action::Export {
                    dest: s[0].default_export_path.clone()
                }
            ))
        );
        assert_eq!(
            run(&s, "6\nfocus on tests\n").0,
            Some((
                0,
                Action::Rollover(RolloverOptions {
                    agent: Agent::Codex,
                    rollover_type: RolloverType::Context,
                    prompt: "focus on tests".into()
                })
            ))
        );
        assert_eq!(
            run(&s, "1\n3\n").0,
            Some((
                0,
                Action::SmartTrim {
                    instructions: SMART_TRIM_DEFAULT.into()
                }
            ))
        );
    }

    #[test]
    fn sidechain_and_picker() {
        let mut sub = view(Agent::Claude, "abc");
        sub.is_sidechain = true;
        let (r, out) = run(std::slice::from_ref(&sub), "\n");
        assert_eq!(r, None);
        assert!(out.contains("[Note: This is a sub-agent session and cannot be resumed directly]"));
        assert!(out.ends_with("Enter choice [1-3] (or Enter to cancel): Cancelled.\n"));

        let two = [
            view(Agent::Claude, "aaaa1111"),
            view(Agent::Codex, "bbbb2222"),
        ];
        let (r, out) = run(&two, "2\n4\n");
        assert_eq!(r, Some((1, Action::Clone)));
        assert!(out.contains("Latest Claude/Codex sessions for 'proj'"));
        assert!(out.contains("2. [Codex] bbbb2222 | proj | main | "));
        assert!(out.contains("Agent: CODEX"));
    }
}
