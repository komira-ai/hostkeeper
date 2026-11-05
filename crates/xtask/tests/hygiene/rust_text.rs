//! A small lexical scan of a Rust source: which bytes are in a comment or a string.

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn mark(mask: &mut [bool], from: usize, to: usize) {
    for slot in mask.iter_mut().take(to).skip(from) {
        *slot = true;
    }
}

/// The offset of the next newline at or after `from`, or the end of the text.
fn line_end(b: &[u8], from: usize) -> usize {
    b[from..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(b.len(), |p| from + p)
}

/// A (nested) block comment that opens at `start`; marks it and returns the offset after it.
fn block_comment(b: &[u8], mask: &mut [bool], start: usize) -> usize {
    let mut depth = 1;
    let mut i = start + 2;
    while i < b.len() && depth > 0 {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    let end = i.min(b.len());
    mark(mask, start, end);
    end
}

/// The body of a string with escapes that starts at `from`, after its opening quote; marks it
/// and returns the offset after the closing quote.
fn string_body(b: &[u8], mask: &mut [bool], from: usize) -> usize {
    let mut i = from;
    while i < b.len() && b[i] != b'"' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    let end = i.min(b.len());
    mark(mask, from, end);
    (end + 1).min(b.len())
}

/// The body of a raw string closed by a quote and `hashes` hashes, from `from`, after its opening
/// quote; marks it and returns the offset after the closer.
fn raw_body(b: &[u8], mask: &mut [bool], from: usize, hashes: usize) -> usize {
    let mut i = from;
    while i < b.len() {
        let closes = b[i] == b'"'
            && b.len() >= i + 1 + hashes
            && b[i + 1..i + 1 + hashes].iter().all(|&c| c == b'#');
        if closes {
            break;
        }
        i += 1;
    }
    let end = i.min(b.len());
    mark(mask, from, end);
    (end + 1 + hashes).min(b.len())
}

/// A run of identifier bytes at `start`; when it is a string prefix (`r`, `b`, `br`, `c`, `cr`)
/// followed by the string, the string is marked. Returns the offset after what was read.
fn word_or_prefixed_string(b: &[u8], mask: &mut [bool], start: usize) -> usize {
    let mut end = start;
    while end < b.len() && is_ident_byte(b[end]) {
        end += 1;
    }
    let word = &b[start..end];
    if matches!(word, b"r" | b"br" | b"cr") {
        let mut quote = end;
        while b.get(quote) == Some(&b'#') {
            quote += 1;
        }
        if b.get(quote) == Some(&b'"') {
            return raw_body(b, mask, quote + 1, quote - end);
        }
    }
    if matches!(word, b"b" | b"c") && b.get(end) == Some(&b'"') {
        return string_body(b, mask, end + 1);
    }
    end
}

/// The offset after what a quote at `at` opens: a character literal (skipped, it holds no host)
/// or, when no closing quote follows the one character, a lifetime or label (just the quote).
pub fn after_quote(text: &str, at: usize) -> usize {
    let b = text.as_bytes();
    let Some(&next) = b.get(at + 1) else {
        return at + 1;
    };
    if next == b'\\' {
        let from = (at + 3).min(b.len());
        let found = b[from..].iter().take(12).position(|&c| c == b'\'');
        return found.map_or(at + 1, |p| from + p + 1);
    }
    let Some(c) = text[at + 1..].chars().next() else {
        return at + 1;
    };
    let after = at + 1 + c.len_utf8();
    if b.get(after) == Some(&b'\'') {
        after + 1
    } else {
        at + 1
    }
}

/// For every byte of a Rust source, whether it is in a comment or a string. Read over the whole
/// file: line comments (doc comments too) anywhere on a line, nested block comments, strings with
/// escapes, byte and C strings, raw strings with any number of hashes (across lines), while
/// character literals and lifetimes are told apart and are not text.
pub fn rust_text_mask(text: &str) -> Vec<bool> {
    let b = text.as_bytes();
    let mut mask = vec![false; b.len()];
    let mut i = 0;
    while i < b.len() {
        i = match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = line_end(b, i);
                mark(&mut mask, i, end);
                end
            }
            b'/' if b.get(i + 1) == Some(&b'*') => block_comment(b, &mut mask, i),
            b'"' => string_body(b, &mut mask, i + 1),
            b'\'' => after_quote(text, i),
            c if is_ident_byte(c) => word_or_prefixed_string(b, &mut mask, i),
            _ => i + 1,
        };
    }
    mask
}
