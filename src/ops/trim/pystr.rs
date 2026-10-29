//! Python-style `str()` / `repr()` formatting of JSON values. The trimmers
//! measure and flatten non-string content with this formatting (e.g. image blocks inside a
//! tool result become a Python-style dict repr), so lengths and flattened
//! text are stable across trims and match previously trimmed sessions.
//!
//! String escaping follows `str.isprintable` for ASCII and the common
//! non-printable Unicode ranges; rare unassigned code points are kept as is.

use serde_json::{Number, Value};

/// Python `str(v)` (strings are returned unquoted).
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Number of code points in `py_str(v)` (Python `len(str(v))`).
pub fn py_str_len(v: &Value) -> usize {
    match v {
        Value::String(s) => s.chars().count(),
        other => py_repr(other).chars().count(),
    }
}

/// Python `repr(v)`.
pub fn py_repr(v: &Value) -> String {
    let mut out = String::new();
    write_repr(&mut out, v);
    out
}

fn write_repr(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("None"),
        Value::Bool(true) => out.push_str("True"),
        Value::Bool(false) => out.push_str("False"),
        Value::Number(n) => out.push_str(&number_repr(n)),
        Value::String(s) => write_str_repr(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_repr(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str_repr(out, k);
                out.push_str(": ");
                write_repr(out, val);
            }
            out.push('}');
        }
    }
}

fn number_repr(n: &Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    n.as_f64().map_or_else(|| n.to_string(), float_repr)
}

/// Python `repr(float)`: shortest round-trip digits, scientific notation
/// when the exponent is < -4 or >= 16.
fn float_repr(f: f64) -> String {
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let body = if (-4..16).contains(&exp) {
        fixed_notation(&digits, exp)
    } else {
        let (head, tail) = digits.split_at(1);
        let frac = if tail.is_empty() {
            String::new()
        } else {
            format!(".{tail}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{head}{frac}e{esign}{:02}", exp.abs())
    };
    format!("{sign}{body}")
}

fn fixed_notation(digits: &str, exp: i32) -> String {
    let n = digits.len() as i32;
    if exp < 0 {
        let zeros = "0".repeat((-exp - 1) as usize);
        format!("0.{zeros}{digits}")
    } else if n - 1 <= exp {
        let zeros = "0".repeat((exp - (n - 1)) as usize);
        format!("{digits}{zeros}.0")
    } else {
        let (int, frac) = digits.split_at((exp + 1) as usize);
        format!("{int}.{frac}")
    }
}

fn write_str_repr(out: &mut String, s: &str) {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !is_printable(c) => {
                let cp = c as u32;
                if cp < 0x100 {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
}

/// Approximation of Python `str.isprintable` for one character.
fn is_printable(c: char) -> bool {
    let cp = c as u32;
    !matches!(cp,
        0x00..=0x1f | 0x7f..=0xa0 | 0xad
        | 0x0600..=0x0605 | 0x061c | 0x06dd | 0x070f
        | 0x1680 | 0x180e
        | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064 | 0x2066..=0x206f
        | 0x3000
        | 0xd800..=0xf8ff
        | 0xfeff | 0xfff9..=0xfffb
        | 0xe0001 | 0xe0020..=0xe007f
        | 0xf0000..=0x10ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_matches_python() {
        let v = json!({"type": "image", "source": {"data": "a'b", "n": 1, "f": 1.5, "z": null, "t": true}});
        assert_eq!(
            py_repr(&v),
            r#"{'type': 'image', 'source': {'data': "a'b", 'n': 1, 'f': 1.5, 'z': None, 't': True}}"#
        );
        assert_eq!(py_repr(&json!("x\n\"y'\u{7}é")), r#"'x\n"y\'\x07é'"#);
        assert_eq!(py_repr(&json!([1, "a", []])), "[1, 'a', []]");
        assert_eq!(py_str(&json!("plain")), "plain");
        assert_eq!(py_str_len(&json!(null)), 4);
    }

    #[test]
    fn floats_match_python() {
        for (f, want) in [
            (1.0, "1.0"),
            (1.5, "1.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456.789, "123456.789"),
            (-2.5e-7, "-2.5e-07"),
            (1e15, "1000000000000000.0"),
        ] {
            assert_eq!(float_repr(f), want, "{f}");
        }
    }
}
