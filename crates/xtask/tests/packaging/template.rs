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
/// included) replaced by `values[NAME]`, which is TOML (`"text"`, `true`), except the value of
/// the placeholder `keep`, which is left exactly as written. A placeholder with no value (and
/// not `keep`) is refused. A placeholder anywhere else is left as it is (see [`tokens`]).
pub fn fill(
    text: &str,
    values: &BTreeMap<&str, &str>,
    keep: Option<&str>,
) -> Result<String, String> {
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
        let end = start + opening.len() + name_end + close + 1;
        if keep == Some(name) {
            out.push_str(&rest[start..end]);
        } else {
            let value = values
                .get(name)
                .ok_or_else(|| format!("no value for @@{name}@@"))?;
            out.push_str(value);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The characters of base64 (`+/=`) and of base64url and most tokens (`_-`), besides letters
/// and digits.
const BASE64: &[u8] = b"+/=";
const BASE64URL: &[u8] = b"_-";

/// The length from which a run of one alphabet is taken for a key, a token or a digest.
pub const LONG_RUN: usize = 32;

/// The runs of `line` in the alphabet of letters, digits and `extra`.
fn runs<'a>(line: &'a str, extra: &'a [u8]) -> impl Iterator<Item = &'a str> {
    line.split(move |c: char| {
        !(c.is_ascii_alphanumeric() || (c.is_ascii() && extra.contains(&(c as u8))))
    })
}

/// The prefixes of tokens that are published formats: GitHub's, and a JSON Web Token's header.
const TOKEN_PREFIXES: [&str; 5] = ["ghp_", "gho_", "ghs_", "github_pat_", "eyJ"];

/// What in `text` has the shape of a secret, one line each: a PEM block; a run of
/// [`LONG_RUN`] or more characters of base64 (letters, digits, `+/=`) or of base64url (letters,
/// digits, `_-`), with or without digits (a key, a token, a digest of something secret); a
/// bearer token; a token with a published prefix. The template's longest such runs are its
/// paths and key names, well under the length, and the numbers of `[bounds]` are short.
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
        for (alphabet, extra) in [("base64", BASE64), ("base64url", BASE64URL)] {
            for run in runs(line, extra).filter(|run| run.len() >= LONG_RUN) {
                out.push(format!(
                    "line {n}: a run of {} {alphabet} characters",
                    run.len()
                ));
            }
        }
        for run in runs(line, BASE64URL) {
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
