//! The Markdown files the report links into: their headings, read as GitHub makes their anchors.
//!
//! A link of the report into a Markdown file of the repository may name a heading of it
//! (`…/design.md#54-the-rollout-gate`); [`anchors`] says which anchors the file has. A heading
//! inside a fenced block is no heading.

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
