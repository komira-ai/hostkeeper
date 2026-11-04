//! The config template: its `@@NAME@@` placeholders, filling them for a test, and the shapes
//! of a secret that must not be in it.

use std::collections::{BTreeMap, BTreeSet};

const MARK: &str = "@@";

/// Every `@@NAME@@` in `text` (NAME of capital letters, digits and `_`), by name.
pub fn tokens(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find(MARK) {
        let after = &rest[start + MARK.len()..];
        let name_len = after
            .bytes()
            .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
            .count();
        if name_len > 0 && after[name_len..].starts_with(MARK) {
            out.insert(after[..name_len].to_owned());
            rest = &after[name_len + MARK.len()..];
        } else {
            rest = after;
        }
    }
    out
}

/// `text` with each quoted value that starts with a placeholder (`"@@NAME@@ ..."`, the quotes
/// included) replaced by `values[NAME]`, which is TOML (`"text"`, `true`). A placeholder with no
/// value is refused. A placeholder anywhere else is left as it is (see [`tokens`]).
pub fn fill(text: &str, values: &BTreeMap<&str, &str>) -> Result<String, String> {
    let opening = format!("\"{MARK}");
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(&opening) {
        out.push_str(&rest[..start]);
        let inside = &rest[start + opening.len()..];
        let name_end = inside
            .find(MARK)
            .ok_or_else(|| "a placeholder without its closing @@".to_owned())?;
        let name = &inside[..name_end];
        let close = inside[name_end..]
            .find('"')
            .ok_or_else(|| format!("the value of @@{name}@@ is not closed by a quote"))?;
        let value = values
            .get(name)
            .ok_or_else(|| format!("no value for @@{name}@@"))?;
        out.push_str(value);
        rest = &inside[name_end + close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A run of the characters of base64, hex and most tokens.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"+/=_-".contains(&b)
}

/// The prefixes of tokens that are published formats: GitHub's, and a JSON Web Token's header.
const TOKEN_PREFIXES: [&str; 5] = ["ghp_", "gho_", "ghs_", "github_pat_", "eyJ"];

/// What in `text` has the shape of a secret, one line each: a PEM block; a run of 32 or more
/// token characters that is all hex digits, or that mixes letters and digits (a key, a digest
/// of something secret, a base64 blob); a bearer token; a token with a published prefix. The
/// numbers of `[bounds]` are digits only and short, so none of them is such a run.
pub fn secret_shapes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let n = at + 1;
        if line.contains("-----BEGIN") {
            out.push(format!("line {n}: a PEM block"));
        }
        if line.to_ascii_lowercase().contains("bearer ") {
            out.push(format!("line {n}: a bearer token"));
        }
        for run in line.split(|c: char| !(c.is_ascii() && is_token_byte(c as u8))) {
            let hex = run.bytes().all(|b| b.is_ascii_hexdigit());
            let mixed = run.bytes().any(|b| b.is_ascii_digit())
                && run.bytes().any(|b| b.is_ascii_alphabetic());
            if run.len() >= 32 && (hex || mixed) {
                out.push(format!("line {n}: a run of {} key characters", run.len()));
            }
            if let Some(prefix) = TOKEN_PREFIXES
                .iter()
                .find(|p| run.starts_with(**p) && run.len() >= p.len() + 10)
            {
                out.push(format!("line {n}: a token starting {prefix:?}"));
            }
        }
    }
    out
}
