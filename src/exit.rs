//! Controlled process exit carried through `anyhow` errors.
//!
//! Commands return `Err(Exit { .. }.into())` when they need a specific exit
//! code and exact stderr text (e.g. `Session selection cancelled.`); `main`
//! prints `message` verbatim (if any) and exits with `code`. Any other
//! error is printed as `Error: <err>` with exit code 1.

use std::fmt;

/// Exit with `code`, printing `message` (verbatim, to stderr) if set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub code: u8,
    pub message: Option<String>,
}

impl Exit {
    /// Exit 1 with a verbatim stderr message.
    pub fn fail(message: impl Into<String>) -> Exit {
        Exit {
            code: 1,
            message: Some(message.into()),
        }
    }

    /// Exit with `code`, printing nothing.
    pub fn code(code: u8) -> Exit {
        Exit {
            code,
            message: None,
        }
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.message {
            Some(m) => f.write_str(m),
            None => write!(f, "exit {}", self.code),
        }
    }
}

impl std::error::Error for Exit {}
