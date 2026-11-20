//! Text as the JSON the gateway reads: a flag's value by the kind of its field, a string, a
//! secret's bytes as base64. All of it is on bytes: an argument, a file and stdin are bytes, and
//! a value is copied into the request as it came.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_proto::ctl::Field;

use crate::Stop;
use crate::table::{Enc, enc_of};

/// Appends `text` to `out` as a JSON string: `"` and `\` escaped, bytes 0x20 to 0x7E as they
/// are, the controls below 0x20 as `\u00xx` in lower case, and every other byte as it is.
pub(crate) fn json_string(text: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    for &byte in text {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0..0x20 => out.extend_from_slice(format!("\\u{byte:04x}").as_bytes()),
            _ => out.push(byte),
        }
    }
    out.push(b'"');
}

/// `text` as one JSON string.
pub(crate) fn json_text(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 2);
    json_string(text, &mut out);
    out
}

/// Whether `text` is non-empty visible ASCII with no backslash.
pub(crate) fn visible(text: &[u8]) -> bool {
    !text.is_empty()
        && text
            .iter()
            .all(|b| (0x21..=0x7e).contains(b) && *b != b'\\')
}

/// `text` without leading and trailing space, tab, newline and carriage return.
pub(crate) fn trim(text: &[u8]) -> &[u8] {
    let is_space = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r');
    let start = text.iter().position(|b| !is_space(b)).unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|b| !is_space(b))
        .map_or(start, |i| i + 1);
    text.get(start..end).unwrap_or_default()
}

/// `text` without one trailing newline.
pub(crate) fn without_newline(text: &[u8]) -> &[u8] {
    text.strip_suffix(b"\n").unwrap_or(text)
}

/// The standard base64 of `bytes`, with `=` padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Whether `text` is a whole number of seconds a timeout or a hold takes: 1 to 999999, no sign,
/// no leading zero.
pub(crate) fn seconds(text: &[u8]) -> Option<u32> {
    let first = text.first()?;
    if !(b'1'..=b'9').contains(first) || text.len() > 6 || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(text).ok()?.parse().ok()
}

/// Whether `text` is a number as `strtod` writes one, that is
/// `-?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?`.
fn is_number(text: &[u8]) -> bool {
    fn digits(text: &[u8]) -> &[u8] {
        let n = text.iter().take_while(|b| b.is_ascii_digit()).count();
        text.get(n..).unwrap_or_default()
    }
    let text = text.strip_prefix(b"-").unwrap_or(text);
    let after_int = digits(text);
    let rest = if after_int.len() < text.len() {
        match after_int.strip_prefix(b".") {
            Some(fraction) => digits(fraction),
            None => after_int,
        }
    } else {
        match after_int.strip_prefix(b".") {
            Some(fraction) if digits(fraction).len() < fraction.len() => digits(fraction),
            _ => return false,
        }
    };
    if rest.is_empty() {
        return true;
    }
    let Some(exponent) = rest.strip_prefix(b"e").or_else(|| rest.strip_prefix(b"E")) else {
        return false;
    };
    let exponent = exponent
        .strip_prefix(b"+")
        .or_else(|| exponent.strip_prefix(b"-"))
        .unwrap_or(exponent);
    digits(exponent).is_empty() && digits(exponent).len() < exponent.len()
}

/// An integer flag's text as a JSON value: a number for 32 bits and a string for 64, as proto3
/// JSON writes them; the digits only, with an optional `-`, leading zeros dropped, `-0` as `0`.
fn integer(field: &Field, enc: Enc, text: &[u8]) -> Result<Vec<u8>, Stop> {
    let (what, signed, max): (&str, bool, u128) = match enc {
        Enc::I32 => ("a 32-bit signed integer", true, 1 << 31),
        Enc::U32 => ("a 32-bit unsigned integer", false, u128::from(u32::MAX)),
        Enc::I64 => ("a 64-bit signed integer", true, 1 << 63),
        _ => ("a 64-bit unsigned integer", false, u128::from(u64::MAX)),
    };
    let refuse = || Stop::usage(format!("--{} takes {what}", field.flag));
    let (negative, digits) = match text.strip_prefix(b"-") {
        Some(digits) => (true, digits),
        None => (false, text),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Err(refuse());
    }
    let significant = digits
        .iter()
        .position(|b| *b != b'0')
        .unwrap_or(digits.len() - 1);
    let digits = digits.get(significant..).unwrap_or(digits);
    let negative = negative && digits != b"0";
    if negative && !signed {
        return Err(refuse());
    }
    // A signed type reaches one further below zero than above it.
    let limit = if signed && !negative { max - 1 } else { max };
    let magnitude: u128 = std::str::from_utf8(digits)
        .ok()
        .and_then(|d| d.parse().ok())
        .ok_or_else(refuse)?;
    if magnitude > limit {
        return Err(refuse());
    }
    let mut out = Vec::new();
    let quoted = matches!(enc, Enc::I64 | Enc::U64);
    if quoted {
        out.push(b'"');
    }
    if negative {
        out.push(b'-');
    }
    out.extend_from_slice(digits);
    if quoted {
        out.push(b'"');
    }
    Ok(out)
}

/// `text` as one value of `field`, as JSON.
pub(crate) fn encode(field: &Field, text: &[u8]) -> Result<Vec<u8>, Stop> {
    match enc_of(field.kind) {
        Enc::String => Ok(json_text(text)),
        Enc::Bool => {
            if text == b"true" || text == b"false" {
                Ok(text.to_vec())
            } else {
                Err(Stop::usage(format!("--{} takes true or false", field.flag)))
            }
        }
        Enc::Enum => {
            if let Some(value) = field.enum_values.iter().find(|v| v.flag.as_bytes() == text) {
                return Ok(format!("\"{}\"", value.name).into_bytes());
            }
            let names: Vec<&str> = field.enum_values.iter().map(|v| v.flag).collect();
            Err(Stop::usage(format!(
                "--{} takes one of: {}",
                field.flag,
                names.join(", ")
            )))
        }
        Enc::Float => {
            if !(matches!(text, b"NaN" | b"Infinity" | b"-Infinity") || is_number(text)) {
                return Err(Stop::usage(format!("--{} takes a number", field.flag)));
            }
            let mut out = vec![b'"'];
            out.extend_from_slice(text);
            out.push(b'"');
            Ok(out)
        }
        enc => integer(field, enc, text),
    }
}
