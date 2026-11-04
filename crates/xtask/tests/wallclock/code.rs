//! The code of a Rust source as the wall-clock rule reads it.
//!
//! - Tokens: see `lex.rs`.
//! - Skipped: a `use` item (one followed by a path, not a `use<..>` capture bound), and what a
//!   `#[cfg(..)]` whose predicate implies `test` applies to (`test`, or `all(..)` holding it). An
//!   item ends after its body (the first braced block outside its generics) or its `;`; a field,
//!   an arm, an argument or a statement ends at its `,` or `;`, or after an arm's `=> {..}`;
//!   either ends at the closer of what encloses it. An inner `#![cfg(test)]` skips to the end of
//!   what encloses it.
//! - Paths: `a::b::c` and `<T>::f`, each with what it may stand for: as written, with its first
//!   segment replaced by the plain import of that name, and, for a lone name (called or not),
//!   under each glob import. Renames (`use .. as ..`, `type .. = ..;`) are listed, not followed;
//!   [`Code::resolve`] follows a rename's source through the file's imports.

use std::collections::{BTreeMap, BTreeSet};

use crate::lex::{Tok, is_ident, segment, text_at, tokens};
use crate::skip::{enclosing_end, item_end, test_attr};

/// The code of one source.
pub struct Code {
    toks: Vec<Tok>,
    chains: Vec<Chain>,
    renames: Vec<Rename>,
    imports: Imports,
}

/// A path in the code, with the full paths it may stand for.
pub struct Chain {
    start: usize,
    end: usize,
    pub paths: Vec<Vec<String>>,
}

/// A rename: `use <path> as <alias>` or `type <alias> = <path>;`, as `spelling`.
pub struct Rename {
    pub line: usize,
    pub path: Vec<String>,
    pub spelling: String,
}

impl Code {
    /// The code of the source `text`.
    pub fn of(text: &str) -> Code {
        let toks = tokens(text);
        let mut imports = Imports::default();
        let code = code_mask(&toks, &mut imports);
        let chains = chains(&toks, &code, &imports);
        let mut renames = std::mem::take(&mut imports.renames);
        renames.extend((0..toks.len()).filter_map(|k| type_rename(&toks, &code, k)));
        for pair in toks.windows(2) {
            if pair[0].text == "mod" && is_ident(&pair[1]) {
                imports.mods.insert(segment(&pair[1]));
            }
        }
        Code {
            toks,
            chains,
            renames,
            imports,
        }
    }

    /// `path` with its first segment replaced, as long as it is a name a `use` of the file
    /// binds (plainly or renamed), by the path that name stands for (at most 8 steps, so a
    /// cycle ends).
    pub fn resolve(&self, path: &[String]) -> Vec<String> {
        let mut out = path.to_vec();
        for _ in 0..8 {
            let Some(first) = out.first() else {
                break;
            };
            let full = self
                .imports
                .names
                .get(first)
                .or(self.imports.aliases.get(first));
            match full {
                Some(full) if full.len() > 1 || full.first() != Some(first) => {
                    out = full.iter().chain(&out[1..]).cloned().collect();
                }
                _ => break,
            }
        }
        out
    }

    /// Whether a path starting with `first` names something of this crate: `crate`, `self`,
    /// `super`, or a module the file declares.
    pub fn is_local(&self, first: &str) -> bool {
        matches!(first, "crate" | "self" | "super") || self.imports.mods.contains(first)
    }

    /// The paths of the code, in order.
    pub fn chains(&self) -> &[Chain] {
        &self.chains
    }

    /// The renames of the source.
    pub fn renames(&self) -> &[Rename] {
        &self.renames
    }

    /// The line (from 1) a path starts on.
    pub fn line(&self, chain: &Chain) -> usize {
        self.toks[chain.start].line
    }

    /// Whether the tokens right after `chain` are `next`.
    pub fn followed_by(&self, chain: &Chain, next: &[&str]) -> bool {
        next.iter()
            .enumerate()
            .all(|(k, want)| text_at(&self.toks, chain.end + k) == *want)
    }
}

/// The imports of a file: each name a plain `use` binds, with its path; each alias a renaming
/// `use` binds (followed only by [`Code::resolve`]); the glob prefixes; the renames; and the
/// modules the file declares.
#[derive(Default)]
struct Imports {
    names: BTreeMap<String, Vec<String>>,
    aliases: BTreeMap<String, Vec<String>>,
    globs: Vec<Vec<String>>,
    renames: Vec<Rename>,
    mods: BTreeSet<String>,
}

impl Imports {
    /// The full paths `segments` may stand for (see the module header).
    fn paths_of(&self, segments: Vec<String>) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        if let Some(full) = self.names.get(&segments[0]) {
            out.push(full.iter().chain(&segments[1..]).cloned().collect());
        }
        if segments.len() == 1 {
            for glob in &self.globs {
                out.push(glob.iter().chain(&segments).cloned().collect());
            }
        }
        out.push(segments);
        out
    }
}

/// Whether a `use` item starts at `i` (not a `use<..>` capture bound).
fn is_use(toks: &[Tok], i: usize) -> bool {
    toks[i].text == "use"
        && (toks.get(i + 1).is_some_and(is_ident) || matches!(text_at(toks, i + 1), "::" | "{"))
}

/// `prefix` without a trailing `self`.
fn without_self(prefix: &[String]) -> Vec<String> {
    match prefix.split_last() {
        Some((last, before)) if last == "self" => before.to_vec(),
        _ => prefix.to_vec(),
    }
}

/// Reads the use tree at `i` under `prefix` into `imports`; returns the offset after the tree.
fn use_tree(toks: &[Tok], mut i: usize, prefix: &mut Vec<String>, imports: &mut Imports) -> usize {
    let base = prefix.len();
    loop {
        let text = text_at(toks, i);
        if text == "::" {
            i += 1;
            continue;
        }
        if text != "as" && toks.get(i).is_some_and(is_ident) {
            prefix.push(segment(&toks[i]));
            i += 1;
            continue;
        }
        if text == "{" {
            i += 1;
            loop {
                i = use_tree(toks, i, prefix, imports);
                if text_at(toks, i) != "," {
                    break;
                }
                i += 1;
            }
            i += usize::from(text_at(toks, i) == "}");
        } else if text == "*" {
            imports.globs.push(prefix.clone());
            i += 1;
        } else if text == "as" {
            let alias = text_at(toks, i + 1);
            if prefix.len() > base && alias != "_" {
                let path = without_self(prefix);
                let name = alias.strip_prefix("r#").unwrap_or(alias).to_owned();
                imports.aliases.insert(name, path.clone());
                imports.renames.push(Rename {
                    line: toks[i].line,
                    spelling: format!("{} as {alias}", path.join("::")),
                    path,
                });
            }
            i += 2;
        } else if prefix.len() > base {
            let path = without_self(prefix);
            if let Some(name) = path.last() {
                imports.names.insert(name.clone(), path);
            }
        }
        prefix.truncate(base);
        return i;
    }
}

/// For every token, whether it is code the rule reads: not in a `use` item or a test-only item.
fn code_mask(toks: &[Tok], imports: &mut Imports) -> Vec<bool> {
    let mut code = vec![true; toks.len()];
    let mut i = 0;
    while i < toks.len() {
        let end = if let Some(after) = test_attr(toks, i) {
            if text_at(toks, i + 1) == "!" {
                enclosing_end(toks, after)
            } else {
                item_end(toks, after)
            }
        } else if is_use(toks, i) {
            use_tree(toks, i + 1, &mut Vec::new(), imports);
            item_end(toks, i + 1)
        } else {
            i += 1;
            continue;
        };
        code[i..end].fill(false);
        i = end;
    }
    code
}

/// The rename `type <alias> = <path>;` at `k`, when the right side is a path alone.
fn type_rename(toks: &[Tok], code: &[bool], k: usize) -> Option<Rename> {
    if !code[k] || toks[k].text != "type" || !toks.get(k + 1).is_some_and(is_ident) {
        return None;
    }
    let semi = (k..toks.len()).find(|&j| text_at(toks, j) == ";")?;
    let eq = (k..semi).find(|&j| text_at(toks, j) == "=")?;
    let mut j = eq + 1 + usize::from(text_at(toks, eq + 1) == "::");
    let mut path = Vec::new();
    while toks.get(j).is_some_and(is_ident) {
        path.push(segment(&toks[j]));
        j += 1;
        if text_at(toks, j) != "::" {
            break;
        }
        j += 1;
    }
    if j != semi || path.is_empty() {
        return None;
    }
    let spelling = format!("type {} = {}", toks[k + 1].text, path.join("::"));
    Some(Rename {
        line: toks[k].line,
        path,
        spelling,
    })
}

/// The paths of the code, outside `use` items and test-only items.
fn chains(toks: &[Tok], code: &[bool], imports: &Imports) -> Vec<Chain> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if !code[i] || !is_ident(&toks[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut segments = vec![segment(&toks[i])];
        i += 1;
        loop {
            if text_at(toks, i) == "::" && toks.get(i + 1).is_some_and(is_ident) {
                segments.push(segment(&toks[i + 1]));
                i += 2;
            } else if text_at(toks, i) == ">"
                && text_at(toks, i + 1) == "::"
                && toks.get(i + 2).is_some_and(is_ident)
            {
                segments.push(segment(&toks[i + 2]));
                i += 3;
            } else {
                break;
            }
        }
        let method = start > 0 && toks[start - 1].text == ".";
        let paths = if method {
            vec![segments]
        } else {
            imports.paths_of(segments)
        };
        out.push(Chain {
            start,
            end: i,
            paths,
        });
    }
    out
}
