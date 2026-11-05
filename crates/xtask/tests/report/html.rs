//! The tags, attributes, text and numbers of the report, read the way the cell rules need them.
//!
//! This is not an HTML parser. A tag is a `<` followed by a letter or `/`, up to the first `>`
//! outside quotes on the same line; a `<` with no such `>` is text, and so is everything else. That is enough for the report, whose only tags are
//! the `<span>` and `<br>` elements its cells are written with.

/// Elements that have no end tag.
const VOID: &[&str] = &[
    "br", "hr", "img", "meta", "link", "input", "col", "wbr", "source",
];

/// One start or end tag.
pub struct Tag<'a> {
    pub name: &'a str,
    pub attrs: &'a str,
    /// Byte offset of the `<`.
    pub start: usize,
    /// Byte offset just after the `>`.
    pub end: usize,
    pub closing: bool,
    /// A start tag that opens an element with an end tag (not void, not `/>`).
    pub open: bool,
}

/// Every tag of `text`, in order.
pub fn tags(text: &str) -> Vec<Tag<'_>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let next = bytes.get(i + 1).copied().unwrap_or(b' ');
        if !(next.is_ascii_alphabetic() || next == b'/') {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut quote = 0u8;
        while j < bytes.len() {
            let c = bytes[j];
            if c == b'\n' {
                break;
            } else if quote != 0 {
                if c == quote {
                    quote = 0;
                }
            } else if c == b'"' || c == b'\'' {
                quote = c;
            } else if c == b'>' {
                break;
            }
            j += 1;
        }
        if j >= bytes.len() || bytes[j] == b'\n' {
            i += 1;
            continue;
        }
        let inner = &text[i + 1..j];
        let closing = inner.starts_with('/');
        let inner = inner.trim_start_matches('/');
        let name_end = inner
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(inner.len());
        let name = &inner[..name_end];
        let attrs = &inner[name_end..];
        let open = !closing && !attrs.trim_end().ends_with('/') && !VOID.contains(&name);
        out.push(Tag {
            name,
            attrs,
            start: i,
            end: j + 1,
            closing,
            open,
        });
        i = j + 1;
    }
    out
}

/// How an attribute is written in a start tag.
pub enum Value<'a> {
    Double(&'a str),
    Single,
    Bare,
    Missing,
}

pub struct Attr<'a> {
    pub name: &'a str,
    pub value: Value<'a>,
}

/// `s` cut at the first `end`: the text before it and the text after it.
fn cut(s: &str, end: char) -> (&str, &str) {
    let at = s.find(end).unwrap_or(s.len());
    (&s[..at], s.get(at + 1..).unwrap_or(""))
}

/// The attributes of the attribute text of a tag, in order.
pub fn attributes(attrs: &str) -> Vec<Attr<'_>> {
    let mut out = Vec::new();
    let mut rest = attrs;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            return out;
        }
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let (name, after) = rest.split_at(name_end);
        let after = after.trim_start();
        let Some(raw) = after.strip_prefix('=') else {
            out.push(Attr {
                name,
                value: Value::Missing,
            });
            rest = after;
            continue;
        };
        let raw = raw.trim_start();
        let (value, tail) = if let Some(quoted) = raw.strip_prefix('"') {
            let (text, tail) = cut(quoted, '"');
            (Value::Double(text), tail)
        } else if let Some(quoted) = raw.strip_prefix('\'') {
            (Value::Single, cut(quoted, '\'').1)
        } else {
            let (_, tail) = raw.split_at(raw.find(char::is_whitespace).unwrap_or(raw.len()));
            (Value::Bare, tail)
        };
        out.push(Attr { name, value });
        rest = tail;
    }
}

/// The double-quoted value of attribute `name` (in any letter case) in the attribute text of a
/// tag. A value in single quotes or in none is not read here; the quote rule refuses it.
pub fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    attributes(attrs).into_iter().find_map(|a| match a.value {
        Value::Double(text) if a.name.eq_ignore_ascii_case(name) => Some(text),
        _ => None,
    })
}

/// The 1-based line of a byte offset.
pub fn line_of(text: &str, offset: usize) -> usize {
    1 + text.as_bytes()[..offset]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
}

/// The byte offset of the end tag of the element whose start tag is `tags[k]`, or `None` when it
/// is never closed.
pub fn element_end(tags: &[Tag<'_>], k: usize) -> Option<usize> {
    let first = &tags[k];
    let mut depth = 1usize;
    for tag in &tags[k + 1..] {
        if tag.name != first.name {
            continue;
        }
        if tag.closing {
            depth -= 1;
            if depth == 0 {
                return Some(tag.start);
            }
        } else if tag.open {
            depth += 1;
        }
    }
    None
}

/// `inner` without its tags and `&name;` entities, its numeric character references decoded.
pub fn plain(inner: &str) -> String {
    let mut out = String::new();
    let mut rest = inner;
    while let Some(at) = rest.find('<') {
        let after = rest.as_bytes().get(at + 1).copied().unwrap_or(b' ');
        if after.is_ascii_alphabetic() || after == b'/' {
            out.push_str(&rest[..at]);
            match rest[at..].find('>') {
                Some(end) => rest = &rest[at + end + 1..],
                None => return strip_entities(&out),
            }
        } else {
            out.push_str(&rest[..=at]);
            rest = &rest[at + 1..];
        }
    }
    out.push_str(rest);
    strip_entities(&out)
}

/// `text` with each numeric character reference (`&#53;`, `&#x35;`) replaced by the character it
/// names (U+FFFD when it names none) and each other `&name;` removed, so an encoded digit is
/// read like a written one.
pub fn strip_entities(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        let tail = &rest[at + 1..];
        let end = tail.find(';');
        let is_entity = end.is_some_and(|e| {
            (1..=8).contains(&e)
                && tail[..e]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '#')
        });
        out.push_str(&rest[..at]);
        match (is_entity, end) {
            (true, Some(e)) => {
                if let Some(code) = tail[..e].strip_prefix('#') {
                    out.push(decode_reference(code));
                }
                rest = &tail[e + 1..];
            }
            _ => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character a numeric reference names, `code` being what follows the `#`: decimal digits,
/// or `x`/`X` and hex digits. U+FFFD when it is neither or names no character.
fn decode_reference(code: &str) -> char {
    let value = match code.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => code.parse(),
    };
    value
        .ok()
        .and_then(char::from_u32)
        .unwrap_or(char::REPLACEMENT_CHARACTER)
}

/// The number-like tokens printed in `text`, as printed (a thousands separator removed). A
/// token starts at a digit, or at a point followed by a digit, that does not follow a letter, a
/// digit, a point, an underscore, or a hyphen after a letter (so `p50`, `v1`, `S3`, `arm64`,
/// `x86_64`, `SHA-256` and `P-256` hold none, while `1-63` holds 1 and 63), and is digits with
/// points between them; a comma followed by three digits is a thousands separator. Each comes
/// with the char index just after it.
fn tokens_at(text: &str) -> Vec<(String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let digit_at = |k: usize| chars.get(k).is_some_and(char::is_ascii_digit);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let after_word = i > 0
            && (chars[i - 1].is_ascii_alphanumeric()
                || chars[i - 1] == '.'
                || chars[i - 1] == '_'
                || (chars[i - 1] == '-' && i > 1 && chars[i - 2].is_ascii_alphabetic()));
        let starts =
            !after_word && (chars[i].is_ascii_digit() || (chars[i] == '.' && digit_at(i + 1)));
        if !starts {
            i += 1;
            continue;
        }
        let mut token = String::new();
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_digit() || (c == '.' && digit_at(i + 1)) {
                token.push(c);
                i += 1;
            } else if c == ','
                && digit_at(i + 1)
                && digit_at(i + 2)
                && digit_at(i + 3)
                && !digit_at(i + 4)
            {
                i += 1;
            } else {
                break;
            }
        }
        out.push((token, i));
    }
    out
}

/// The number-like tokens printed in `text`, as [`tokens_at`] reads them.
pub fn tokens(text: &str) -> Vec<String> {
    tokens_at(text).into_iter().map(|(t, _)| t).collect()
}

/// The numbers printed in `text` (as [`numbers`] reads them), each with the word of letters
/// that follows it after white space on the same line, if any: its unit (`45 s`, `16 KiB`).
pub fn quantities(text: &str) -> Vec<(f64, Option<String>)> {
    let chars: Vec<char> = text.chars().collect();
    tokens_at(text)
        .into_iter()
        .filter(|(t, _)| !is_version(t))
        .filter_map(|(t, end)| {
            let value = t.parse().ok()?;
            let start = (end..chars.len())
                .find(|&k| chars[k] != ' ' && chars[k] != '\u{a0}')
                .unwrap_or(chars.len());
            let word: String = chars[start..]
                .iter()
                .take_while(|c| c.is_alphabetic())
                .collect();
            Some((value, (!word.is_empty()).then_some(word)))
        })
        .collect()
}

/// Whether a token is version-like: two or more points, so not a number.
pub fn is_version(token: &str) -> bool {
    token.matches('.').count() > 1
}

/// The numbers printed in `text` (`.5` is 0.5). A version-like token (`1.2.3`) is not one.
pub fn numbers(text: &str) -> Vec<f64> {
    tokens(text)
        .iter()
        .filter(|t| !is_version(t))
        .filter_map(|t| t.parse().ok())
        .collect()
}
