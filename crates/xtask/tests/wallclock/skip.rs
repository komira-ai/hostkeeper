//! What the wall-clock rule does not read: a `use` item, and what a `#[cfg(..)]` whose
//! predicate implies `test` applies to (see the header of `code.rs`).

use crate::lex::{Tok, close_of, text_at};

/// The offset of the first `,` at depth 0 in `from..to`, or `to`.
fn element_end(toks: &[Tok], from: usize, to: usize) -> usize {
    let mut depth = 0usize;
    for k in from..to {
        match text_at(toks, k) {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            "," if depth == 0 => return k,
            _ => {}
        }
    }
    to
}

/// Whether one of the comma-separated cfg predicates in `from..to` implies `test`: `test`
/// itself, or `all(..)` holding one that does. `any(..)` and `not(..)` do not.
fn implies_test(toks: &[Tok], from: usize, to: usize) -> bool {
    let mut k = from;
    while k < to {
        let end = element_end(toks, k, to);
        let holds = match text_at(toks, k) {
            "test" => end == k + 1,
            "all" if text_at(toks, k + 1) == "(" => {
                implies_test(toks, k + 2, close_of(toks, k + 1))
            }
            _ => false,
        };
        if holds {
            return true;
        }
        k = end + 1;
    }
    false
}

/// When a `#[cfg(..)]` or `#![cfg(..)]` whose predicate implies `test` starts at `i`, the offset
/// after its `]`.
pub fn test_attr(toks: &[Tok], i: usize) -> Option<usize> {
    if text_at(toks, i) != "#" {
        return None;
    }
    let open = if text_at(toks, i + 1) == "!" {
        i + 2
    } else {
        i + 1
    };
    if text_at(toks, open) != "[" || text_at(toks, open + 1) != "cfg" {
        return None;
    }
    if text_at(toks, open + 2) != "(" {
        return None;
    }
    let close = close_of(toks, open + 2);
    (text_at(toks, close + 1) == "]" && implies_test(toks, open + 3, close)).then_some(close + 2)
}

/// The keywords that start an item.
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
/// an arm, an argument or a statement.
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

/// Whether the token at `k` follows `=>`.
fn after_fat_arrow(toks: &[Tok], k: usize) -> bool {
    k >= 2 && text_at(toks, k - 1) == ">" && text_at(toks, k - 2) == "="
}

/// The end (exclusive) of what the attribute before `from` applies to (see the module header).
pub fn item_end(toks: &[Tok], from: usize) -> usize {
    let item = is_item(toks, from);
    let mut depth = 0usize;
    let mut angle = 0usize;
    let mut body = false;
    for k in from..toks.len() {
        let text = text_at(toks, k);
        let arrow = k > 0 && matches!(text_at(toks, k - 1), "-" | "=");
        match text {
            "<" if item && depth == 0 => angle += 1,
            ">" if item && depth == 0 && !arrow => angle = angle.saturating_sub(1),
            "(" | "[" | "{" => {
                let opens_body = text == "{"
                    && depth == 0
                    && if item {
                        angle == 0
                    } else {
                        after_fat_arrow(toks, k)
                    };
                body |= opens_body;
                depth += 1;
            }
            ")" | "]" | "}" => {
                if depth == 0 {
                    return k;
                }
                depth -= 1;
                if depth == 0 && body {
                    return k + 1;
                }
            }
            ";" if depth == 0 => return k + 1,
            "," if depth == 0 && !item => return k + 1,
            _ => {}
        }
    }
    toks.len()
}

/// The offset of the closer of what encloses `from`, or the end.
pub fn enclosing_end(toks: &[Tok], from: usize) -> usize {
    let mut depth = 0usize;
    for (k, tok) in toks.iter().enumerate().skip(from) {
        match tok.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" if depth == 0 => return k,
            ")" | "]" | "}" => depth -= 1,
            _ => {}
        }
    }
    toks.len()
}
