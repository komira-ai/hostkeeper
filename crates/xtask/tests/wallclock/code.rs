//! The code of a Rust source as the wall-clock rule reads it: its tokens outside comments,
//! strings and character literals, and the paths among them, each with its first segment replaced
//! by what a `use` of the file brought in under that name. A `use` item and an item behind
//! `#[cfg(test)]` hold no path: what the attribute applies to ends at the first `;` at its own
//! depth, at the end of the first braced block at its own depth, at the first `,` at its own
//! depth when it is a field, an arm or an argument rather than an item, or at the closer of what
//! encloses it.

use std::collections::BTreeMap;

use crate::rust_text::{after_quote, rust_text_mask};

/// The code of one source.
pub struct Code {
    toks: Vec<Tok>,
    chains: Vec<Chain>,
}

impl Code {
    /// The code of the source `text`.
    pub fn of(text: &str) -> Code {
        let toks = tokens(text);
        let chains = chains(&toks);
        Code { toks, chains }
    }

    /// The paths of the code, in order.
    pub fn chains(&self) -> &[Chain] {
        &self.chains
    }

    /// The line (from 1) a path starts on.
    pub fn line(&self, chain: &Chain) -> usize {
        self.toks[chain.start].line
    }

    /// The paths inside the parentheses that follow `chain` when it is called; none otherwise.
    pub fn args(&self, chain: &Chain) -> &[Chain] {
        if text_at(&self.toks, chain.end) != "(" {
            return &[];
        }
        let close = close_of(&self.toks, chain.end);
        let from = self.chains.partition_point(|c| c.start <= chain.end);
        let to = self.chains.partition_point(|c| c.start < close);
        &self.chains[from..to.max(from)]
    }
}

/// A token of code: an identifier, `::`, or one other byte; with its line (from 1).
struct Tok {
    text: String,
    line: usize,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_ident(tok: &Tok) -> bool {
    tok.text.bytes().next().is_some_and(is_ident_byte)
}

/// The tokens of `text` outside its comments, strings and character literals.
fn tokens(text: &str) -> Vec<Tok> {
    let mask = rust_text_mask(text);
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line += 1;
            i += 1;
        } else if mask[i] || c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'\'' {
            i = after_quote(text, i);
        } else if is_ident_byte(c) {
            let start = i;
            while i < b.len() && is_ident_byte(b[i]) && !mask[i] {
                i += 1;
            }
            out.push(Tok {
                text: text[start..i].to_owned(),
                line,
            });
        } else if c == b':' && b.get(i + 1) == Some(&b':') {
            out.push(Tok {
                text: "::".to_owned(),
                line,
            });
            i += 2;
        } else {
            let len = text[i..].chars().next().map_or(1, char::len_utf8);
            out.push(Tok {
                text: text[i..i + len].to_owned(),
                line,
            });
            i += len;
        }
    }
    out
}

fn text_at(toks: &[Tok], i: usize) -> &str {
    toks.get(i).map_or("", |t| t.text.as_str())
}

/// Whether `#[cfg(test)]` starts at `i`.
fn is_cfg_test(toks: &[Tok], i: usize) -> bool {
    ["#", "[", "cfg", "(", "test", ")", "]"]
        .iter()
        .enumerate()
        .all(|(k, want)| text_at(toks, i + k) == *want)
}

/// The keywords that start an item, whose text may hold a comma at its own depth (`fn f<A, B>()`),
/// so that only a `;` or a braced block ends it.
const ITEM_KEYWORDS: &[&str] = &[
    "async",
    "const",
    "enum",
    "extern",
    "fn",
    "impl",
    "macro_rules",
    "mod",
    "static",
    "struct",
    "trait",
    "type",
    "union",
    "unsafe",
    "use",
];

/// Whether what starts at `from` (after its attributes and visibility) is an item, not a field,
/// a match arm, an argument or another element of a comma-separated list.
fn is_item(toks: &[Tok], mut from: usize) -> bool {
    loop {
        match text_at(toks, from) {
            "#" if text_at(toks, from + 1) == "[" => from = close_of(toks, from + 1) + 1,
            "pub" if text_at(toks, from + 1) == "(" => from = close_of(toks, from + 1) + 1,
            "pub" => from += 1,
            word => return ITEM_KEYWORDS.contains(&word),
        }
    }
}

/// The end (exclusive) of what the `#[cfg(test)]` before `from` applies to: after the first `;`
/// at its depth, after the first braced block at its depth, after the first `,` at its depth
/// when it is not an item, or at the closer of what encloses it.
fn item_end(toks: &[Tok], from: usize) -> usize {
    let comma_ends = !is_item(toks, from);
    let mut depth = 0usize;
    let mut braced = false;
    for (k, tok) in toks.iter().enumerate().skip(from) {
        match tok.text.as_str() {
            "(" | "[" | "{" => {
                braced |= depth == 0 && tok.text == "{";
                depth += 1;
            }
            ")" | "]" | "}" => {
                if depth == 0 {
                    return k;
                }
                depth -= 1;
                if depth == 0 && braced {
                    return k + 1;
                }
            }
            ";" if depth == 0 => return k + 1,
            "," if depth == 0 && comma_ends => return k + 1,
            _ => {}
        }
    }
    toks.len()
}

/// Reads the use tree at `i` under `prefix` into `names` (the name each import binds, and the
/// full path it stands for); returns the offset after the tree.
fn use_tree(
    toks: &[Tok],
    mut i: usize,
    prefix: &mut Vec<String>,
    names: &mut BTreeMap<String, Vec<String>>,
) -> usize {
    let base = prefix.len();
    loop {
        let text = text_at(toks, i);
        if text == "::" {
            i += 1;
        } else if text == "{" {
            i += 1;
            loop {
                i = use_tree(toks, i, prefix, names);
                if text_at(toks, i) != "," {
                    break;
                }
                i += 1;
            }
            i += usize::from(text_at(toks, i) == "}");
            prefix.truncate(base);
            return i;
        } else if text == "as" {
            let alias = text_at(toks, i + 1).to_owned();
            bind(prefix, base, Some(alias), names);
            prefix.truncate(base);
            return i + 2;
        } else if text == "*" {
            prefix.truncate(base);
            return i + 1;
        } else if toks.get(i).is_some_and(is_ident) {
            prefix.push(text.to_owned());
            i += 1;
        } else {
            bind(prefix, base, None, names);
            prefix.truncate(base);
            return i;
        }
    }
}

/// Binds the import `prefix` (when this tree added segments to it) under `alias` or its last
/// segment; a trailing `self` stands for the path before it, and `_` binds nothing. A glob
/// (`*`) binds no name.
fn bind(
    prefix: &[String],
    base: usize,
    alias: Option<String>,
    names: &mut BTreeMap<String, Vec<String>>,
) {
    if prefix.len() <= base {
        return;
    }
    let path = match prefix.split_last() {
        Some((last, before)) if last == "self" => before.to_vec(),
        _ => prefix.to_vec(),
    };
    let Some(name) = alias.or_else(|| path.last().cloned()) else {
        return;
    };
    if name != "_" {
        names.insert(name, path);
    }
}

/// A path in the code: its first token, the offset after it, and its segments with the first
/// replaced by what the file imported under that name.
pub struct Chain {
    start: usize,
    end: usize,
    pub segments: Vec<String>,
}

/// The offset of the closer of the `(`, `[` or `{` at `open` (counting every bracket kind), or
/// the end of the tokens.
fn close_of(toks: &[Tok], open: usize) -> usize {
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

/// The paths of the code, outside `use` items and `#[cfg(test)]` items. The imports are read
/// first, so a `use` below the path it names counts as it does for the compiler.
fn chains(toks: &[Tok]) -> Vec<Chain> {
    let mut names = BTreeMap::new();
    let mut code = vec![true; toks.len()];
    let mut i = 0;
    while i < toks.len() {
        let end = if is_cfg_test(toks, i) {
            item_end(toks, i + 7)
        } else if toks[i].text == "use" {
            use_tree(toks, i + 1, &mut Vec::new(), &mut names);
            item_end(toks, i + 1)
        } else {
            i += 1;
            continue;
        };
        code[i..end].fill(false);
        i = end;
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if !code[i] || !is_ident(&toks[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut segments = vec![toks[i].text.clone()];
        i += 1;
        while text_at(toks, i) == "::" && toks.get(i + 1).is_some_and(is_ident) {
            segments.push(toks[i + 1].text.clone());
            i += 2;
        }
        let method = start > 0 && toks[start - 1].text == ".";
        if let Some(full) = names.get(&segments[0]).filter(|_| !method) {
            segments = full.iter().chain(&segments[1..]).cloned().collect();
        }
        out.push(Chain {
            start,
            end: i,
            segments,
        });
    }
    out
}
