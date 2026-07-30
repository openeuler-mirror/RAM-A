use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn canonical_json(value: &Value) -> String {
    let mut output = String::new();
    write_canonical(value, &mut output);
    output
}

pub fn stable_hash(values: &[Value]) -> String {
    let payload = canonical_json(&Value::Array(values.to_vec()));
    let digest = Sha256::digest(payload.as_bytes());
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn estimate_tokens(text: &str) -> usize {
    static TOKEN_RE: OnceLock<Regex> = OnceLock::new();
    TOKEN_RE
        .get_or_init(|| {
            Regex::new(r"[A-Za-z0-9]+|[\u{3400}-\u{4dbf}\u{4e00}-\u{9fff}\u{f900}-\u{faff}]|[^\s]")
                .expect("token regex is valid")
        })
        .find_iter(text)
        .count()
}

fn write_canonical(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => {
            let encoded = value.to_string();
            if !encoded.contains(['.', 'e', 'E']) {
                output.push_str(&encoded);
            } else {
                let parsed = value
                    .as_f64()
                    .or_else(|| encoded.parse::<f64>().ok())
                    .expect("valid JSON number parses as f64 or integer");
                if parsed.is_infinite() {
                    // `arbitrary_precision` keeps the original valid JSON number even when it
                    // does not fit in f64. Preserve that representation instead of emitting the
                    // non-JSON `Infinity` literals, which cannot be read back by serde_json.
                    output.push_str(&encoded);
                } else {
                    output.push_str(&python_float(parsed));
                }
            }
        }
        Value::String(value) => {
            output.push_str(&serde_json::to_string(value).expect("JSON strings always serialize"))
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical(value, output);
            }
            output.push(']');
        }
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            output.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(
                    &serde_json::to_string(key).expect("JSON object keys always serialize"),
                );
                output.push(':');
                write_canonical(&values[key], output);
            }
            output.push('}');
        }
    }
}

fn python_float(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("scientific float contains exponent");
    let exponent = exponent.parse::<i32>().expect("valid float exponent");
    if (-4..16).contains(&exponent) {
        let fixed = value.to_string();
        if fixed.contains(['e', 'E']) {
            scientific_to_fixed(mantissa, exponent)
        } else if fixed.contains('.') {
            fixed
        } else {
            format!("{fixed}.0")
        }
    } else {
        format!(
            "{mantissa}e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        )
    }
}

fn scientific_to_fixed(mantissa: &str, exponent: i32) -> String {
    let negative = mantissa.starts_with('-');
    let digits = mantissa
        .trim_start_matches('-')
        .chars()
        .filter(|character| *character != '.')
        .collect::<String>();
    let decimal = exponent + 1;
    let body = if decimal <= 0 {
        format!("0.{}{}", "0".repeat((-decimal) as usize), digits)
    } else if decimal as usize >= digits.len() {
        format!("{}{}", digits, "0".repeat(decimal as usize - digits.len()))
    } else {
        format!(
            "{}.{}",
            &digits[..decimal as usize],
            &digits[decimal as usize..]
        )
    };
    let body = if body.contains('.') {
        body
    } else {
        format!("{body}.0")
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}
