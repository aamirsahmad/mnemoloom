//! Form state for the trim, smart-trim, rollover and query screens, and
//! the actions they submit. Pure: no terminal or IO.

use crate::session::Agent;

/// Smart-trim form default prompt.
pub const SMART_TRIM_DEFAULT: &str =
    "trim messages that are not relevant to the last task being worked on in this session";
/// Query screen default query.
pub const QUERY_DEFAULT: &str = "Summarize what was accomplished in this session";

/// `suppress_resume` kwargs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimOptions {
    /// `None` = all tools.
    pub tools: Option<String>,
    pub threshold: usize,
    /// `None` = no assistant trimming.
    pub trim_assistant: Option<i64>,
}

/// Rollover type (`[1] Quick Resume` / `[2] Resume with Context Recovery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloverType {
    Quick,
    Context,
}

/// `continue` kwargs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloverOptions {
    pub agent: Agent,
    pub rollover_type: RolloverType,
    /// Custom context-recovery instructions; empty = default.
    pub prompt: String,
}

/// Editable text with a "placeholder until first keystroke" rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    pub text: String,
    pub typed: bool,
}

impl Input {
    /// Append a char; the first one replaces the placeholder.
    pub fn push(&mut self, c: char) {
        if !self.typed {
            self.typed = true;
            self.text.clear();
        }
        self.text.push(c);
    }

    /// Delete the last char (no-op before typing).
    pub fn backspace(&mut self) {
        if self.typed {
            self.text.pop();
        }
    }

    /// The typed text, or `default` if nothing was typed.
    pub fn value_or(&self, default: &str) -> String {
        if self.typed {
            self.text.clone()
        } else {
            default.to_string()
        }
    }
}

/// Trim form fields, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimField {
    Tools,
    Threshold,
    Assistant,
}

/// ` Trim options ` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimForm {
    pub field: TrimField,
    pub tools: String,
    pub threshold: String,
    pub assistant: String,
}

impl Default for TrimForm {
    fn default() -> Self {
        TrimForm {
            field: TrimField::Tools,
            tools: String::new(),
            threshold: "500".into(),
            assistant: String::new(),
        }
    }
}

impl TrimForm {
    /// Down arrow / Enter order (wraps).
    pub fn next_field(&mut self) {
        self.field = match self.field {
            TrimField::Tools => TrimField::Threshold,
            TrimField::Threshold => TrimField::Assistant,
            TrimField::Assistant => TrimField::Tools,
        };
    }

    /// Up arrow (wraps).
    pub fn prev_field(&mut self) {
        self.field = match self.field {
            TrimField::Tools => TrimField::Assistant,
            TrimField::Threshold => TrimField::Tools,
            TrimField::Assistant => TrimField::Threshold,
        };
    }

    fn current(&mut self) -> &mut String {
        match self.field {
            TrimField::Tools => &mut self.tools,
            TrimField::Threshold => &mut self.threshold,
            TrimField::Assistant => &mut self.assistant,
        }
    }

    pub fn push(&mut self, c: char) {
        self.current().push(c);
    }

    pub fn backspace(&mut self) {
        self.current().pop();
    }

    /// Submit: blank tools = all; threshold `Number(x) || 500`;
    /// assistant blank or non-numeric = none.
    pub fn options(&self) -> TrimOptions {
        TrimOptions {
            tools: Some(self.tools.clone()).filter(|t| !t.is_empty()),
            threshold: self
                .threshold
                .trim()
                .parse()
                .ok()
                .filter(|&n| n > 0)
                .unwrap_or(500),
            trim_assistant: self.assistant.trim().parse().ok(),
        }
    }
}

/// Rollover form fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinueField {
    Agent,
    RolloverType,
    Prompt,
}

/// ` 🔄 ROLLOVER ` form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueForm {
    pub field: ContinueField,
    pub agent: Agent,
    pub rollover_type: RolloverType,
    pub prompt: String,
}

impl ContinueForm {
    /// Defaults: the session's agent, Quick.
    pub fn new(agent: Agent) -> Self {
        ContinueForm {
            field: ContinueField::Agent,
            agent,
            rollover_type: RolloverType::Quick,
            prompt: String::new(),
        }
    }

    /// Field after the current one; `None` = submit position.
    pub fn next_field(&self) -> Option<ContinueField> {
        match self.field {
            ContinueField::Agent => Some(ContinueField::RolloverType),
            ContinueField::RolloverType if self.rollover_type == RolloverType::Context => {
                Some(ContinueField::Prompt)
            }
            ContinueField::RolloverType | ContinueField::Prompt => None,
        }
    }

    /// The field before the current one (wrapping).
    pub fn prev_field(&self) -> ContinueField {
        match self.field {
            ContinueField::Prompt => ContinueField::RolloverType,
            ContinueField::RolloverType => ContinueField::Agent,
            ContinueField::Agent if self.rollover_type == RolloverType::Context => {
                ContinueField::Prompt
            }
            ContinueField::Agent => ContinueField::RolloverType,
        }
    }

    /// Typed char: `1`/`2` select on choice fields, text on the prompt.
    pub fn push(&mut self, c: char) {
        match (self.field, c) {
            (ContinueField::Agent, '1') => self.agent = Agent::Claude,
            (ContinueField::Agent, '2') => self.agent = Agent::Codex,
            (ContinueField::RolloverType, '1') => self.rollover_type = RolloverType::Quick,
            (ContinueField::RolloverType, '2') => self.rollover_type = RolloverType::Context,
            (ContinueField::Prompt, c) => self.prompt.push(c),
            _ => {}
        }
    }

    pub fn backspace(&mut self) {
        if self.field == ContinueField::Prompt {
            self.prompt.pop();
        }
    }

    /// Submitted kwargs (prompt ignored for Quick).
    pub fn options(&self) -> RolloverOptions {
        RolloverOptions {
            agent: self.agent,
            rollover_type: self.rollover_type,
            prompt: match self.rollover_type {
                RolloverType::Context => self.prompt.clone(),
                RolloverType::Quick => String::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_placeholder_rules() {
        let mut i = Input::default();
        i.backspace();
        assert_eq!(i.value_or("dflt"), "dflt");
        i.push('a');
        i.push('b');
        i.backspace();
        assert_eq!(i.value_or("dflt"), "a");
        i.backspace();
        assert_eq!(i.value_or("dflt"), "");
    }

    #[test]
    fn trim_form_options() {
        let mut f = TrimForm::default();
        assert_eq!(
            f.options(),
            TrimOptions {
                tools: None,
                threshold: 500,
                trim_assistant: None
            }
        );
        "bash,read".chars().for_each(|c| f.push(c));
        f.next_field();
        f.backspace();
        f.backspace();
        f.backspace();
        "x".chars().for_each(|c| f.push(c));
        f.next_field();
        "-5".chars().for_each(|c| f.push(c));
        assert_eq!(
            f.options(),
            TrimOptions {
                tools: Some("bash,read".into()),
                threshold: 500,
                trim_assistant: Some(-5)
            }
        );
        f.prev_field();
        f.backspace();
        "300".chars().for_each(|c| f.push(c));
        assert_eq!(f.options().threshold, 300);
        f.prev_field();
        f.prev_field();
        assert_eq!(f.field, TrimField::Assistant);
        f.next_field();
        assert_eq!(f.field, TrimField::Tools);
    }

    #[test]
    fn continue_form_navigation() {
        let mut f = ContinueForm::new(Agent::Codex);
        assert_eq!(
            (f.agent, f.rollover_type),
            (Agent::Codex, RolloverType::Quick)
        );
        f.push('1');
        assert_eq!(f.agent, Agent::Claude);
        f.push('x');
        assert_eq!(f.prev_field(), ContinueField::RolloverType);
        f.field = ContinueField::RolloverType;
        assert_eq!(f.next_field(), None);
        f.push('2');
        assert_eq!(f.next_field(), Some(ContinueField::Prompt));
        f.field = ContinueField::Prompt;
        "keep tests".chars().for_each(|c| f.push(c));
        f.backspace();
        assert_eq!(f.options().prompt, "keep test");
        f.field = ContinueField::Agent;
        assert_eq!(f.prev_field(), ContinueField::Prompt);
        f.field = ContinueField::RolloverType;
        f.push('1');
        assert_eq!(f.options().prompt, "");
    }
}
