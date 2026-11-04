//! The tokens of a Rust source: identifiers (Unicode, and raw ones such as `r#use`, which keep
//! their prefix so they are never keywords), `::`, and single characters, each with its line,
//! outside comments, strings and character literals (the hygiene lint's `rust_text_mask`).

use crate::rust_text::{after_quote, rust_text_mask};

/// A token of code with its line (from 1).
pub struct Tok {
    pub text: String,
    pub line: usize,
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

pub fn is_ident(tok: &Tok) -> bool {
    tok.text.chars().next().is_some_and(is_ident_char)
}

/// A path segment: the identifier without its raw prefix.
pub fn segment(tok: &Tok) -> String {
    tok.text
        .strip_prefix("r#")
        .unwrap_or(tok.text.as_str())
        .to_owned()
}

/// The offset after the identifier characters from `i` that are code.
fn word_end(text: &str, mask: &[bool], mut i: usize) -> usize {
    while let Some(c) = text[i..].chars().next() {
        if mask[i] || !is_ident_char(c) {
            break;
        }
        i += c.len_utf8();
    }
    i
}

/// The tokens of `text` outside its comments, strings and character literals.
pub fn tokens(text: &str) -> Vec<Tok> {
    let mask = rust_text_mask(text);
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let start = i;
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if mask[i] || c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        if c == '\'' {
            i = after_quote(text, i);
            continue;
        }
        if is_ident_char(c) {
            i = word_end(text, &mask, i);
            let raw = &text[start..i] == "r"
                && text[i..].starts_with('#')
                && text[i + 1..].chars().next().is_some_and(is_ident_start)
                && !mask[i + 1];
            if raw {
                i = word_end(text, &mask, i + 1);
            }
        } else if text[i..].starts_with("::") {
            i += 2;
        } else {
            i += c.len_utf8();
        }
        out.push(Tok {
            text: text[start..i].to_owned(),
            line,
        });
    }
    out
}

pub fn text_at(toks: &[Tok], i: usize) -> &str {
    toks.get(i).map_or("", |t| t.text.as_str())
}

/// The offset of the closer of the bracket at `open` (counting every kind), or the end.
pub fn close_of(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0usize;
    for (k, tok) in toks.iter().enumerate().skip(open) {
        match tok.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return k;
                }
            }
            _ => {}
        }
    }
    toks.len()
}
