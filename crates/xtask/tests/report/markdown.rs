//! The Markdown of the report: what is code and what is prose, its links, its headings and its
//! diagrams.
//!
//! Code is never read as prose: a fenced block (three or more backticks or tildes), an inline
//! code span and an HTML comment are blanked out (each byte but a newline made a space, so byte
//! offsets and line numbers stay those of the file) before the cells are found and before the
//! prose is scanned for numbers. A code span quotes an identifier, a path or a literal of the
//! code; a number a reader should take as a fact is written in a cell instead.

use crate::cells::{CELL_ATTRS, Cell};
use crate::html::{Value, attributes, line_of, strip_entities, tags, tokens};

/// `bytes[from..to]` made spaces, its newlines kept.
fn blank(bytes: &mut [u8], from: usize, to: usize) {
    for b in &mut bytes[from..to] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

/// The opening fence of a line: its character and length, when the line (after up to three
/// spaces) starts with three or more backticks or tildes.
fn fence(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let c = *trimmed.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = trimmed.bytes().take_while(|b| *b == c).count();
    (n >= 3).then_some((c, n))
}

/// A fenced block: its info string (the first word after the opening fence), its body (the
/// lines between the fences) and the byte range from the opening fence to the end of the
/// closing one. An unclosed fence runs to the end of the text.
pub struct Block<'a> {
    pub info: &'a str,
    pub body: &'a str,
    pub line: usize,
    pub span: (usize, usize),
}

/// The fenced blocks of `text`.
pub fn blocks(text: &str) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut offset = 0;
    let mut open: Option<(u8, usize, usize, usize, &str)> = None;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        match open {
            None => {
                if let Some((c, n)) = fence(line) {
                    let info = line.trim_start_matches(' ')[n..]
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    open = Some((c, n, start, offset, info));
                }
            }
            Some((c, n, from, body, info)) => {
                let closes = fence(line).is_some_and(|(cc, nn)| cc == c && nn >= n)
                    && line.trim().bytes().all(|b| b == c);
                if closes {
                    out.push(Block {
                        info,
                        body: &text[body..start],
                        line: line_of(text, from),
                        span: (from, offset),
                    });
                    open = None;
                }
            }
        }
    }
    if let Some((_, _, from, body, info)) = open {
        out.push(Block {
            info,
            body: &text[body..],
            line: line_of(text, from),
            span: (from, text.len()),
        });
    }
    out
}

/// `text` with its fenced blocks blanked out.
fn mask_fences(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    for block in blocks(text) {
        blank(&mut bytes, block.span.0, block.span.1);
    }
    bytes
}

/// `text` with its fenced blocks, inline code spans and HTML comments blanked out, in that
/// order. A code span ends at the next run of as many backticks in the same paragraph (it never
/// crosses a blank line), and a comment ends at its `-->`; an unmatched run of backticks and an
/// unclosed `<!--` are text, as Markdown reads them.
pub fn mask_code(text: &str) -> String {
    let mut bytes = mask_fences(text);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let n = bytes[i..].iter().take_while(|b| **b == b'`').count();
        match closing_run(&bytes, i + n, n) {
            Some(end) => {
                blank(&mut bytes, i, end + n);
                i = end + n;
            }
            None => i += n,
        }
    }
    let mut from = 0;
    while let Some(at) = find(&bytes, b"<!--", from) {
        match find(&bytes, b"-->", at + 4) {
            Some(end) => {
                blank(&mut bytes, at, end + 3);
                from = end + 3;
            }
            None => break,
        }
    }
    String::from_utf8(bytes).expect("blanking whole ASCII-delimited ranges keeps UTF-8")
}

/// The offset of the first run of exactly `n` backticks at or after `from`, before the end of
/// the paragraph (a line that is empty or only white space).
fn closing_run(bytes: &[u8], from: usize, n: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let rest = &bytes[i + 1..];
            let line = rest.split(|b| *b == b'\n').next().unwrap_or(rest);
            if line.iter().all(u8::is_ascii_whitespace) {
                return None;
            }
            i += 1;
            continue;
        }
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let run = bytes[i..].iter().take_while(|b| **b == b'`').count();
        if run == n {
            return Some(i);
        }
        i += run;
    }
    None
}

fn find(bytes: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| at + from)
}

/// The inline links `[text](target)` of `masked`: the byte range of each `(target)` and the
/// target, a title after it left out.
fn links(masked: &str) -> Vec<((usize, usize), &str)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = masked[from..].find("](").map(|at| at + from) {
        let open = at + 1;
        let Some(len) = masked[open..].find(')') else {
            break;
        };
        let target = masked[open + 1..open + len]
            .split_whitespace()
            .next()
            .unwrap_or("");
        out.push(((open, open + len + 1), target));
        from = open + len + 1;
    }
    out
}

/// The link targets of `masked` (code already blanked out).
pub fn link_targets(masked: &str) -> Vec<&str> {
    links(masked)
        .into_iter()
        .map(|(_, target)| target)
        .collect()
}

/// One line per number printed in the prose of `masked`: outside every cell, every tag and every
/// link target. The text of a link is prose, so a citation names its section in words. A number
/// in the value of a tag's attribute that is not a cell attribute (`title="12 ms"`) is prose too.
pub fn loose_numbers(masked: &str, cells: &[Cell]) -> Vec<String> {
    let mut bytes = masked.as_bytes().to_vec();
    for cell in cells {
        blank(&mut bytes, cell.span.0, cell.span.1);
    }
    let mut found = Vec::new();
    for tag in tags(masked) {
        let inside = cells
            .iter()
            .any(|c| c.span.0 <= tag.start && tag.end <= c.span.1);
        if !inside {
            for a in attributes(tag.attrs) {
                let cell_attr = CELL_ATTRS.iter().any(|c| a.name.eq_ignore_ascii_case(c));
                if let (false, Value::Double(value)) = (cell_attr, a.value) {
                    for token in tokens(&strip_entities(value)) {
                        found.push(format!(
                            "line {}: the number {token} stands in the attribute {} outside a cell",
                            line_of(masked, tag.start),
                            a.name
                        ));
                    }
                }
            }
        }
        blank(&mut bytes, tag.start, tag.end);
    }
    for (range, _) in links(masked) {
        blank(&mut bytes, range.0, range.1);
    }
    let prose = String::from_utf8(bytes).expect("blanking keeps UTF-8");
    for (n, line) in prose.lines().enumerate() {
        for token in tokens(&strip_entities(line)) {
            found.push(format!(
                "line {}: the number {token} stands outside a cell",
                n + 1
            ));
        }
    }
    found
}

/// The named character references the report may use; none of them is a number.
const ENTITIES: &[&str] = &[
    "amp", "lt", "gt", "quot", "apos", "nbsp", "mdash", "ndash", "rsquo", "lsquo", "rdquo",
    "ldquo", "hellip", "middot", "rarr", "larr", "times",
];

/// One line per character of `masked` (code blanked out) that is a digit or a number of a script
/// other than ASCII (`５`, `½`, `²`, `①`), which the number rules do not read, and per named
/// character reference not in [`ENTITIES`] (`&frac12;`), which they drop.
pub fn glyph_problems(masked: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (n, line) in masked.lines().enumerate() {
        for c in line
            .chars()
            .filter(|c| c.is_numeric() && !c.is_ascii_digit())
        {
            found.push(format!(
                "line {}: {c:?} is a number not written in ASCII digits",
                n + 1
            ));
        }
        for (at, _) in line.match_indices('&') {
            let rest = &line[at + 1..];
            let Some(end) = rest.find(';') else {
                continue;
            };
            let name = &rest[..end];
            let named = !name.is_empty()
                && name.len() <= 8
                && name.chars().all(|c| c.is_ascii_alphanumeric())
                && name.starts_with(|c: char| c.is_ascii_alphabetic());
            if named && !ENTITIES.contains(&name) {
                found.push(format!(
                    "line {}: the character reference &{name}; is not one the report may use",
                    n + 1
                ));
            }
        }
    }
    found
}

/// The GitHub anchor of a heading's text: lower case, letters, digits, `-` and `_` kept, each
/// space made `-`, everything else dropped.
pub fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The anchors of the ATX headings (`#` to `######`) of `text`, outside fenced blocks, as GitHub
/// makes them: a code span keeps its text (its backticks are dropped), a link keeps its text and
/// loses its target, and a heading whose anchor was already made gets `-1`, `-2` and so on.
pub fn anchors(text: &str) -> Vec<String> {
    let masked = String::from_utf8(mask_fences(text)).expect("blanking keeps UTF-8");
    let mut seen: Vec<String> = Vec::new();
    for line in masked.lines() {
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        let rest = &line[hashes..];
        if !((1..=6).contains(&hashes) && rest.starts_with(' ')) {
            continue;
        }
        let mut shown = rest.to_owned();
        for (range, _) in links(rest).into_iter().rev() {
            shown.replace_range(range.0..range.1, "");
        }
        let base = slug(&shown);
        let mut anchor = base.clone();
        let mut n = 0;
        while seen.contains(&anchor) {
            n += 1;
            anchor = format!("{base}-{n}");
        }
        seen.push(anchor);
    }
    seen
}
