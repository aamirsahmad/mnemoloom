//! Python `json.dumps`-compatible serialization of [`serde_json::Value`].
//!
//! Default `json.dumps`: separators `", "` / `": "`, `ensure_ascii=True`
//! (non-ASCII as `\uXXXX`, astral chars as surrogate pairs). With an indent:
//! newline + indent per level and `","` item separator (Python 3 behaviour).
//! Key order is preserved (serde_json `preserve_order`).

use serde_json::Value;

/// `json.dumps(v)`.
pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, None, 0);
    out
}

/// `json.dumps(v, indent=n)`.
pub fn dumps_indent(v: &Value, indent: usize) -> String {
    let mut out = String::new();
    write_value(&mut out, v, Some(indent), 0);
    out
}

fn newline(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(n) = indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', n * level));
    }
}

fn write_value(out: &mut String, v: &Value, indent: Option<usize>, level: usize) {
    let item_sep = if indent.is_some() { "," } else { ", " };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match n.as_f64().filter(|_| n.is_f64()) {
            Some(f) => out.push_str(&float_repr(f)),
            None => out.push_str(&n.to_string()),
        },
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, indent, level + 1);
                write_value(out, item, indent, level + 1);
            }
            newline(out, indent, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, indent, level + 1);
                write_str(out, k);
                out.push_str(": ");
                write_value(out, val, indent, level + 1);
            }
            newline(out, indent, level);
            out.push('}');
        }
    }
}

/// Python `repr(float)`: shortest round-trip digits; positional when the
/// decimal exponent is in `-4..16`, else `d.ddde±XX` (2+ exponent digits).
pub fn float_repr(f: f64) -> String {
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if f.is_sign_negative() { "-" } else { "" };
    let body = if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = exp as usize + 1;
            let padded = format!("{digits:0<int_len$}");
            let (int, frac) = padded.split_at(int_len);
            format!("{int}.{}", if frac.is_empty() { "0" } else { frac })
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{esign}{:02}", exp.abs())
    };
    format!("{sign}{body}")
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_python_default() {
        let v = json!({"b": 1, "a": [1, "x", null, true], "é": "😀\n", "e": {}, "f": []});
        // python3 -c 'import json;print(json.dumps({"b":1,"a":[1,"x",None,True],"é":"😀\n","e":{},"f":[]}))'
        assert_eq!(
            dumps(&v),
            r#"{"b": 1, "a": [1, "x", null, true], "\u00e9": "\ud83d\ude00\n", "e": {}, "f": []}"#
        );
    }

    #[test]
    fn matches_python_indent() {
        let v = json!([{"a": 1, "b": []}]);
        assert_eq!(
            dumps_indent(&v, 2),
            "[\n  {\n    \"a\": 1,\n    \"b\": []\n  }\n]"
        );
    }

    #[test]
    fn floats_match_python_repr() {
        // python3 -c 'print([repr(x) for x in [1e-05,1e16,1.5e16,0.0001,123.456,1.0,-2.5e-07,1234567890123456.0,0.1]])'
        for (f, py) in [
            (1e-05, "1e-05"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (0.0001, "0.0001"),
            (123.456, "123.456"),
            (1.0, "1.0"),
            (-2.5e-07, "-2.5e-07"),
            (1234567890123456.0, "1234567890123456.0"),
            (0.1, "0.1"),
            (0.0, "0.0"),
        ] {
            assert_eq!(float_repr(f), py);
        }
        assert_eq!(
            dumps(&json!({"a": 1e-5, "b": 3})),
            r#"{"a": 1e-05, "b": 3}"#
        );
    }

    #[test]
    fn del_char_escaped() {
        assert_eq!(dumps(&json!("\u{7f}")), r#""\u007f""#);
    }
}
