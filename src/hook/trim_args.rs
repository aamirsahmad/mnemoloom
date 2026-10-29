//! Shape-based arguments shared by trim previews and hostile-state validation.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::ops::trim::TrimOptions;

pub(super) const DEFAULT_THRESHOLD: usize = 500;
const MAX_TOOL_NAME_CHARS: usize = 128;
const MAX_NUMERIC_TOKEN_DIGITS: usize = 12;

// Python's Unicode \w is alphanumeric or underscore, not combining marks.
static TOOL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z][\p{L}\p{N}_.-]*\z").expect("constant regex"));
static DECIMAL_DIGIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\p{Nd}\z").expect("constant regex"));

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct Options {
    pub threshold: Option<usize>,
    pub trim_assistant: Option<i64>,
    pub tools: Vec<String>,
}

impl Options {
    pub fn api_options(&self) -> TrimOptions {
        TrimOptions {
            target_tools: (!self.tools.is_empty()).then(|| self.tools.iter().cloned().collect()),
            threshold: self.threshold.unwrap_or(DEFAULT_THRESHOLD),
            trim_assistant: self.trim_assistant,
        }
    }

    pub fn is_valid(&self) -> bool {
        let mut seen = HashSet::new();
        self.threshold != Some(0)
            && self.trim_assistant != Some(0)
            && self
                .tools
                .iter()
                .all(|name| is_saved_tool_name(name) && seen.insert(name))
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(n) = self.trim_assistant {
            parts.push(if n < 0 {
                format!("keep last {} long assistant msgs", n.unsigned_abs())
            } else {
                format!("trim first {n} long assistant msgs")
            });
        }
        parts.push(format!(
            "threshold {} chars",
            self.threshold.unwrap_or(DEFAULT_THRESHOLD)
        ));
        parts.push(if self.tools.is_empty() {
            "all tools".to_owned()
        } else {
            format!("tools: {}", self.tools.join(","))
        });
        parts.join(", ")
    }
}

pub(super) fn is_saved_tool_name(name: &str) -> bool {
    name.chars().count() <= MAX_TOOL_NAME_CHARS
        && name.to_lowercase() == name
        && TOOL_NAME.is_match(name)
}

pub(super) fn is_python_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{001c}'..='\u{001f}')
}

fn is_decimal(c: char) -> bool {
    let mut bytes = [0; 4];
    DECIMAL_DIGIT.is_match(c.encode_utf8(&mut bytes))
}

// Python str.isdigit() also accepts these non-decimal digits. int() does
// not, so they take the numeric-error branch rather than "unrecognized".
fn is_digit(c: char) -> bool {
    is_decimal(c)
        || matches!(
            c,
            '\u{00b2}'..='\u{00b3}'
                | '\u{00b9}'
                | '\u{1369}'..='\u{1371}'
                | '\u{19da}'
                | '\u{2070}'
                | '\u{2074}'..='\u{2079}'
                | '\u{2080}'..='\u{2089}'
                | '\u{2460}'..='\u{2468}'
                | '\u{2474}'..='\u{247c}'
                | '\u{2488}'..='\u{2490}'
                | '\u{24ea}'
                | '\u{24f5}'..='\u{24fd}'
                | '\u{24ff}'
                | '\u{2776}'..='\u{277e}'
                | '\u{2780}'..='\u{2788}'
                | '\u{278a}'..='\u{2792}'
                | '\u{10a40}'..='\u{10a43}'
                | '\u{10e60}'..='\u{10e68}'
                | '\u{11052}'..='\u{1105a}'
                | '\u{1f100}'..='\u{1f10a}'
        )
}

fn parse_number(token: &str) -> Option<i64> {
    if token.chars().count() > MAX_NUMERIC_TOKEN_DIGITS {
        return None;
    }
    token
        .chars()
        .try_fold(0, |n, c| Some(n * 10 + i64::from(decimal_value(c)?)))
}

fn decimal_value(c: char) -> Option<u32> {
    if !is_decimal(c) {
        return None;
    }
    // Decimal digits occupy contiguous blocks of ten. Some mathematical
    // alphabets put several blocks next to each other.
    let mut start = u32::from(c);
    while let Some(previous) = start.checked_sub(1).and_then(char::from_u32) {
        if !is_decimal(previous) {
            break;
        }
        start -= 1;
    }
    Some((u32::from(c) - start) % 10)
}

pub(super) fn parse_python_float(value: &str) -> Option<f64> {
    let normalized: String = value
        .trim_matches(is_python_whitespace)
        .chars()
        .map(|c| {
            if c.is_ascii() {
                Some(c)
            } else {
                char::from_digit(decimal_value(c)?, 10)
            }
        })
        .collect::<Option<_>>()?;
    let bytes = normalized.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'_'
            && (i == 0
                || !bytes[i - 1].is_ascii_digit()
                || !bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            return None;
        }
    }
    normalized.replace('_', "").parse().ok()
}

pub(super) fn parse_trim_args(arg: &str) -> Result<Options, String> {
    let mut opts = Options::default();
    for token in arg.split(is_python_whitespace).filter(|s| !s.is_empty()) {
        let signed = token
            .strip_prefix(['+', '-'])
            .filter(|s| !s.is_empty() && s.chars().all(is_decimal));
        if let Some(digits) = signed {
            if opts.trim_assistant.is_some() {
                return Err(format!("More than one assistant spec ('{token}')."));
            }
            let value = parse_number(digits).ok_or("Assistant spec is too large.")?;
            if value == 0 {
                return Err("Assistant spec must be non-zero (e.g. -20).".to_owned());
            }
            opts.trim_assistant = Some(if token.starts_with('-') {
                -value
            } else {
                value
            });
        } else if token.chars().all(is_digit) {
            if opts.threshold.is_some() {
                return Err(format!("More than one threshold ('{token}')."));
            }
            let value = parse_number(token).ok_or("Threshold is too large.")?;
            if value <= 0 {
                return Err("Threshold must be a positive number.".to_owned());
            }
            opts.threshold = Some(usize::try_from(value).map_err(|_| "Threshold is too large.")?);
        } else if token.split(',').all(|name| TOOL_NAME.is_match(name)) {
            for name in token.to_lowercase().split(',') {
                if name.chars().count() > MAX_TOOL_NAME_CHARS {
                    return Err("Tool name is too long.".to_owned());
                }
                if opts.tools.iter().any(|saved| saved == name) {
                    return Err(format!("Duplicate tool name ('{name}')."));
                }
                opts.tools.push(name.to_owned());
            }
        } else {
            return Err(format!("Unrecognized token: '{token}'."));
        }
    }
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_order_free_options_and_defaults() {
        assert_eq!(parse_trim_args("").unwrap(), Options::default());
        for arg in [
            "-20 800 Bash,Read",
            "Bash,Read 800 -20",
            "800 -20 Bash Read",
        ] {
            let opts = parse_trim_args(arg).unwrap();
            assert_eq!(opts.threshold, Some(800));
            assert_eq!(opts.trim_assistant, Some(-20));
            assert_eq!(opts.tools, ["bash", "read"]);
            assert_eq!(
                opts.describe(),
                "keep last 20 long assistant msgs, threshold 800 chars, tools: bash,read"
            );
        }
        assert_eq!(
            parse_trim_args("+2").unwrap().describe(),
            "trim first 2 long assistant msgs, threshold 500 chars, all tools"
        );
    }

    #[test]
    fn reports_exact_errors() {
        for (arg, error) in [
            ("-1 +2", "More than one assistant spec ('+2')."),
            ("1 2", "More than one threshold ('2')."),
            ("-0", "Assistant spec must be non-zero (e.g. -20)."),
            ("+000", "Assistant spec must be non-zero (e.g. -20)."),
            ("0", "Threshold must be a positive number."),
            ("1234567890123", "Threshold is too large."),
            ("-1234567890123", "Assistant spec is too large."),
            ("Bash,read bash", "Duplicate tool name ('bash')."),
            ("bash,", "Unrecognized token: 'bash,'."),
            (",bash", "Unrecognized token: ',bash'."),
            ("bash,,read", "Unrecognized token: 'bash,,read'."),
            ("--len", "Unrecognized token: '--len'."),
            ("1.5", "Unrecognized token: '1.5'."),
            ("-1.5", "Unrecognized token: '-1.5'."),
            ("_bash", "Unrecognized token: '_bash'."),
            ("1bash", "Unrecognized token: '1bash'."),
        ] {
            assert_eq!(parse_trim_args(arg).unwrap_err(), error, "{arg}");
        }
        assert!(parse_trim_args("yes bash").is_ok());
        assert_eq!(
            parse_trim_args(&"a".repeat(129)).unwrap_err(),
            "Tool name is too long."
        );
        assert_eq!(
            parse_trim_args(&"1".repeat(10_000)).unwrap_err(),
            "Threshold is too large."
        );
        assert_eq!(
            parse_trim_args("999999999999").unwrap().threshold,
            Some(999_999_999_999)
        );
    }

    #[test]
    fn python_unicode_numeric_and_word_semantics() {
        assert_eq!(
            parse_trim_args("-20\u{001c}800\u{001f}Bash").unwrap(),
            parse_trim_args("-20 800 Bash").unwrap()
        );
        assert_eq!(
            parse_trim_args("\u{0668}\u{0660}\u{0660} -\u{ff12}\u{ff10}").unwrap(),
            Options {
                threshold: Some(800),
                trim_assistant: Some(-20),
                tools: Vec::new(),
            }
        );
        assert_eq!(
            parse_trim_args("\u{00b2}").unwrap_err(),
            "Threshold is too large."
        );
        assert_eq!(
            parse_trim_args("-\u{00b2}").unwrap_err(),
            "Unrecognized token: '-\u{00b2}'."
        );
        assert!(parse_trim_args("b\u{00e9} b\u{00b2}").is_ok());
        assert!(parse_trim_args("b\u{0301}").is_err());
    }
}
