//! The rules of the hygiene lint, each a pure function of a file's path and text.
//!
//! A rule returns one [`Finding`] per offence, with the path and the 1-based line, and the
//! reason in words. The rules are:
//! - [`dates`]: a date before the cutoff (2026-09-01), of any four-digit year from 0000 to 9999, in a common
//!   spelling: numeric year first (`-`, `/` or `.` between the parts), compact eight digits at word
//!   boundaries, numeric year last, and month names (whole or abbreviated, any case) with a year,
//!   with or without a day, in the orders `Month D, YYYY`, `D Month YYYY`, `D-Mon-YYYY`,
//!   `YYYY-Mon-D` and `YYYY Month D`. A year alone is not a date. The licence text is exempt from
//!   this rule only (see [`lint`]);
//! - [`ipv4s`]: four dotted octets that are not loopback, unspecified or a documentation range;
//! - [`home_paths`]: a path under the home directories of a person or a host;
//! - [`emails`]: an e-mail address, with a plain or a quoted local part, outside the example
//!   domains, apart from the instance name of a systemd unit (`name@instance.service`);
//! - [`hostnames`]: the host of a URL, or a bare dotted name (in any case) ending in a top-level
//!   domain of the pinned IANA list (`iana-tlds.txt`), outside the example domains and the allow
//!   list of public domains;
//! - [`repositories`]: a token whose SHA-256 is on the deny list of internal repository names.
//!
//! In a Rust file a bare dotted name counts only inside a comment or a string, as found by a small
//! lexical scan of the whole file (`rust_text`), and a name followed by `(` never counts, so
//! field and method access is not read as a host. A name whose last label is a top-level domain
//! and also a common file extension (`FILE_SUFFIXES`) is a file name when it has two labels or
//! is part of a path, and a host otherwise. In a TOML file a table header is not
//! read for bare names.

use std::collections::HashSet;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::rust_text::rust_text_mask;
use crate::tlds::{FILE_SUFFIXES, is_tld};

/// The first day that may be named: a date before it is refused.
const CUTOFF: (u32, u32, u32) = (2026, 9, 1);
const CUTOFF_TEXT: &str = "2026-09-01";

/// What the rules need besides the text: the public domains and the denied digests.
pub struct Lists {
    pub public: Vec<String>,
    pub denied: HashSet<String>,
}

/// One offence: where it is and why it is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.reason)
    }
}

fn finding(path: &str, line: usize, reason: String) -> Finding {
    Finding {
        path: path.to_string(),
        line,
        reason,
    }
}

/// The entries of a list file: one per line, with `#` starting a comment, lowercased.
pub fn entries(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let before_comment = line.split('#').next().unwrap_or("");
        let entry = before_comment.trim();
        if !entry.is_empty() {
            out.push(entry.to_lowercase());
        }
    }
    out
}

/// The licence text, verbatim from its authors, dated as they dated it: the one file the date
/// rule does not read. Every other rule reads it.
pub const LICENCE_FILE: &str = "LICENSE";

/// Every rule, over one file. The date rule skips the licence text and nothing else does.
pub fn lint(path: &str, text: &str, lists: &Lists) -> Vec<Finding> {
    let mut out = if path == LICENCE_FILE {
        Vec::new()
    } else {
        dates(path, text)
    };
    out.extend(ipv4s(path, text));
    out.extend(home_paths(path, text));
    out.extend(emails(path, text));
    out.extend(hostnames(path, text, &lists.public));
    out.extend(repositories(path, text, &lists.denied));
    out
}

fn numbered(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines().enumerate().map(|(i, line)| (i + 1, line))
}

// ---------------------------------------------------------------------------------------------
// Dates

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Num,
    Word,
    Sep,
}

/// A run of digits, a run of ASCII letters, or one other character.
struct Tok<'a> {
    kind: Kind,
    text: &'a str,
    start: usize,
    end: usize,
}

fn tokens(line: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let rest = &line[start..];
        let Some(c) = rest.chars().next() else {
            break;
        };
        let kind = if c.is_ascii_digit() {
            Kind::Num
        } else if c.is_ascii_alphabetic() {
            Kind::Word
        } else {
            Kind::Sep
        };
        let len = match kind {
            Kind::Num => rest
                .find(|n: char| !n.is_ascii_digit())
                .unwrap_or(rest.len()),
            Kind::Word => rest
                .find(|n: char| !n.is_ascii_alphabetic())
                .unwrap_or(rest.len()),
            Kind::Sep => c.len_utf8(),
        };
        let end = start + len;
        out.push(Tok {
            kind,
            text: &line[start..end],
            start,
            end,
        });
        start = end;
    }
    out
}

/// The value of a digit run of `min..=max` digits.
fn digits(tok: Option<&Tok<'_>>, min: usize, max: usize) -> Option<u32> {
    let tok = tok?;
    if tok.kind != Kind::Num || !(min..=max).contains(&tok.text.len()) {
        return None;
    }
    tok.text.parse().ok()
}

fn year(tok: Option<&Tok<'_>>) -> Option<u32> {
    digits(tok, 4, 4)
}

fn sep_char(tok: Option<&Tok<'_>>) -> Option<char> {
    let tok = tok?;
    if tok.kind != Kind::Sep {
        return None;
    }
    tok.text.chars().next()
}

fn is_space(tok: &Tok<'_>) -> bool {
    tok.kind == Kind::Sep && tok.text.trim().is_empty()
}

fn is_ordinal(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "st" | "nd" | "rd" | "th"
    )
}

fn skip_space(toks: &[Tok<'_>], mut at: usize) -> usize {
    while toks.get(at).is_some_and(is_space) {
        at += 1;
    }
    at
}

/// True when the token at `at` is the last part of a longer dotted number (`1.2.` before it).
fn dotted_before(toks: &[Tok<'_>], at: usize) -> bool {
    if at < 2 {
        return false;
    }
    sep_char(toks.get(at - 1)) == Some('.') && toks[at - 2].kind == Kind::Num
}

/// True when the token at `at` is followed by `.` and more digits.
fn dotted_after(toks: &[Tok<'_>], at: usize) -> bool {
    let more = toks.get(at + 2).is_some_and(|t| t.kind == Kind::Num);
    sep_char(toks.get(at + 1)) == Some('.') && more
}

/// True when the token touches a letter or an underscore, so it is inside a longer word.
fn touches_word(tok: Option<&Tok<'_>>) -> bool {
    tok.is_some_and(|t| t.kind == Kind::Word || t.text == "_")
}

fn month_of(word: &str) -> Option<u32> {
    let month = match word.to_ascii_lowercase().as_str() {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    };
    Some(month)
}

/// A date found in the tokens: whether it is before the cutoff, and the tokens it spans.
struct Found {
    before: bool,
    first: usize,
    last: usize,
}

fn valid_day_month(month: u32, day: u32) -> bool {
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// `YYYY-M-D`, `YYYY/M/D` or `YYYY.M.D`, the same separator twice.
fn iso_date(toks: &[Tok<'_>], at: usize) -> Option<Found> {
    let y = year(toks.get(at))?;
    let sep = sep_char(toks.get(at + 1))?;
    if !matches!(sep, '-' | '/' | '.') {
        return None;
    }
    let month = digits(toks.get(at + 2), 1, 2)?;
    if sep_char(toks.get(at + 3)) != Some(sep) {
        return None;
    }
    let day = digits(toks.get(at + 4), 1, 2)?;
    if !valid_day_month(month, day) {
        return None;
    }
    if sep == '.' && (dotted_before(toks, at) || dotted_after(toks, at + 4)) {
        return None;
    }
    Some(Found {
        before: (y, month, day) < CUTOFF,
        first: at,
        last: at + 4,
    })
}

/// `YYYYMMDD` as a token of its own, not inside a longer word.
fn compact_date(toks: &[Tok<'_>], at: usize) -> Option<Found> {
    let tok = toks.get(at)?;
    if tok.kind != Kind::Num || tok.text.len() != 8 {
        return None;
    }
    let value: u32 = tok.text.parse().ok()?;
    let (y, month, day) = (value / 10_000, value / 100 % 100, value % 100);
    if !valid_day_month(month, day) {
        return None;
    }
    let previous = at.checked_sub(1).and_then(|i| toks.get(i));
    if touches_word(previous) || touches_word(toks.get(at + 1)) {
        return None;
    }
    Some(Found {
        before: (y, month, day) < CUTOFF,
        first: at,
        last: at,
    })
}

/// `D-M-YYYY`, `M/D/YYYY` and the like: before the cutoff when a reading of it is.
fn year_last_date(toks: &[Tok<'_>], at: usize) -> Option<Found> {
    let a = digits(toks.get(at), 1, 2)?;
    let sep = sep_char(toks.get(at + 1))?;
    if !matches!(sep, '-' | '/' | '.') {
        return None;
    }
    let b = digits(toks.get(at + 2), 1, 2)?;
    if sep_char(toks.get(at + 3)) != Some(sep) {
        return None;
    }
    let y = year(toks.get(at + 4))?;
    let previous = at.checked_sub(1).and_then(|i| toks.get(i));
    if touches_word(previous) || (sep == '.' && dotted_before(toks, at)) {
        return None;
    }
    if sep == '.' && dotted_after(toks, at + 4) {
        return None;
    }
    let mut readable = false;
    let mut before = false;
    for (month, day) in [(a, b), (b, a)] {
        if valid_day_month(month, day) {
            readable = true;
            before |= (y, month, day) < CUTOFF;
        }
    }
    readable.then_some(Found {
        before,
        first: at,
        last: at + 4,
    })
}

/// The day written before a month name (`3 `, `3rd `) and the token it is in.
fn day_before(toks: &[Tok<'_>], at: usize) -> Option<(u32, usize)> {
    let mut j = at.checked_sub(1)?;
    while toks.get(j).is_some_and(is_space) {
        j = j.checked_sub(1)?;
    }
    if toks[j].kind == Kind::Word && is_ordinal(toks[j].text) {
        j = j.checked_sub(1)?;
    }
    let day = digits(toks.get(j), 1, 2)?;
    (1..=31).contains(&day).then_some((day, j))
}

/// A month name with a year: `Month D, YYYY`, `D Month YYYY`, `Month YYYY`.
fn named_date(toks: &[Tok<'_>], at: usize) -> Option<Found> {
    let tok = toks.get(at)?;
    if tok.kind != Kind::Word {
        return None;
    }
    let month = month_of(tok.text)?;
    // A month name that touches digits is part of a longer token, such as a hex string.
    let previous = at.checked_sub(1).and_then(|i| toks.get(i));
    if previous.is_some_and(|t| t.kind == Kind::Num)
        || toks.get(at + 1).is_none_or(|t| t.kind == Kind::Num)
    {
        return None;
    }
    let mut p = at + 1;
    if sep_char(toks.get(p)) == Some('.') {
        p += 1;
    }
    if sep_char(toks.get(p)) == Some(',') {
        p += 1;
    }
    p = skip_space(toks, p);
    if let Some(day) = digits(toks.get(p), 1, 2) {
        let mut q = p + 1;
        if toks
            .get(q)
            .is_some_and(|t| t.kind == Kind::Word && is_ordinal(t.text))
        {
            q += 1;
        }
        if sep_char(toks.get(q)) == Some(',') {
            q += 1;
        }
        q = skip_space(toks, q);
        let y = year(toks.get(q))?;
        if !(1..=31).contains(&day) {
            return None;
        }
        return Some(Found {
            before: (y, month, day) < CUTOFF,
            first: at,
            last: q,
        });
    }
    let y = year(toks.get(p))?;
    let (day, first) = day_before(toks, at).unwrap_or((1, at));
    Some(Found {
        before: (y, month, day) < CUTOFF,
        first,
        last: p,
    })
}

/// `D-Mon-YYYY`, `YYYY-Mon-D`, `Mon-YYYY` and `YYYY-Mon`, the same `-` or `/` on both sides of
/// the month name.
fn joined_named_date(toks: &[Tok<'_>], at: usize) -> Option<Found> {
    let tok = toks.get(at)?;
    if tok.kind != Kind::Word {
        return None;
    }
    let month = month_of(tok.text)?;
    let joiner = |i: Option<usize>| {
        i.and_then(|i| sep_char(toks.get(i)))
            .filter(|&c| matches!(c, '-' | '/'))
    };
    let before = joiner(at.checked_sub(1));
    let after = joiner(Some(at + 1));
    let two_back = at.checked_sub(2);
    let day_first = two_back.and_then(|i| digits(toks.get(i), 1, 2));
    let year_first = two_back.and_then(|i| year(toks.get(i)));
    let day_last = digits(toks.get(at + 2), 1, 2);
    let year_last = year(toks.get(at + 2));
    let make = |y: u32, day: u32, first: usize, last: usize| {
        (1..=31).contains(&day).then_some(Found {
            before: (y, month, day) < CUTOFF,
            first,
            last,
        })
    };
    let both = before.is_some() && before == after;
    if let (true, Some(day), Some(y)) = (both, day_first, year_last) {
        return make(y, day, at - 2, at + 2);
    }
    if let (true, Some(y), Some(day)) = (both, year_first, day_last) {
        return make(y, day, at - 2, at + 2);
    }
    if let (Some(_), Some(y)) = (after, year_last) {
        return make(y, 1, at, at + 2);
    }
    if let (Some(_), Some(y)) = (before, year_first) {
        return make(y, 1, at - 2, at);
    }
    None
}

/// The index of the last token before `at` that is not a space.
fn back_over_space(toks: &[Tok<'_>], mut at: usize) -> Option<usize> {
    while toks.get(at).is_some_and(is_space) {
        at = at.checked_sub(1)?;
    }
    Some(at)
}

/// `YYYY Month D` and `YYYY, Month D`: the year first, then the month name and the day.
fn year_first_named_date(toks: &[Tok<'_>], at: usize) -> Option<Found> {
    let tok = toks.get(at)?;
    if tok.kind != Kind::Word || toks.get(at + 1).is_none_or(|t| t.kind == Kind::Num) {
        return None;
    }
    let month = month_of(tok.text)?;
    let mut j = back_over_space(toks, at.checked_sub(1)?)?;
    if sep_char(toks.get(j)) == Some(',') {
        j = back_over_space(toks, j.checked_sub(1)?)?;
    }
    if j + 1 >= at {
        return None;
    }
    let y = year(toks.get(j))?;
    let mut p = at + 1;
    if sep_char(toks.get(p)) == Some('.') {
        p += 1;
    }
    p = skip_space(toks, p);
    let day = digits(toks.get(p), 1, 2)?;
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(Found {
        before: (y, month, day) < CUTOFF,
        first: j,
        last: p,
    })
}

fn date_at(toks: &[Tok<'_>], at: usize) -> Option<Found> {
    iso_date(toks, at)
        .or_else(|| year_last_date(toks, at))
        .or_else(|| compact_date(toks, at))
        .or_else(|| named_date(toks, at))
        .or_else(|| joined_named_date(toks, at))
        .or_else(|| year_first_named_date(toks, at))
}

/// Dates before the cutoff.
pub fn dates(path: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (n, line) in numbered(text) {
        let toks = tokens(line);
        let mut at = 0;
        while at < toks.len() {
            let Some(found) = date_at(&toks, at) else {
                at += 1;
                continue;
            };
            if found.before {
                let spelled = &line[toks[found.first].start..toks[found.last].end];
                let reason = format!("the date `{spelled}` is before {CUTOFF_TEXT}");
                out.push(finding(path, n, reason));
            }
            at = found.last + 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// IPv4 addresses

fn is_dotted_digit(b: u8) -> bool {
    b.is_ascii_digit() || b == b'.'
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Loopback, unspecified and the three documentation ranges.
fn is_documentation_or_local(octets: &[u8]) -> bool {
    matches!(
        octets,
        [127, ..] | [0, 0, 0, 0] | [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]
    )
}

fn octets(run: &str) -> Option<Vec<u8>> {
    let parts: Vec<&str> = run.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut out = Vec::new();
    for part in parts {
        if part.is_empty() || part.len() > 3 {
            return None;
        }
        out.push(part.parse::<u8>().ok()?);
    }
    Some(out)
}

/// IPv4 literals other than loopback, unspecified and the documentation ranges.
pub fn ipv4s(path: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (n, line) in numbered(text) {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if !is_dotted_digit(b[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && is_dotted_digit(b[i]) {
                i += 1;
            }
            let inside_word = start > 0 && is_word_byte(b[start - 1]);
            let continues = i < b.len() && is_word_byte(b[i]);
            if inside_word || continues {
                continue;
            }
            let run = line[start..i].trim_end_matches('.');
            let Some(found) = octets(run) else {
                continue;
            };
            if !is_documentation_or_local(&found) {
                let reason = format!(
                    "the IPv4 address `{run}` is not loopback, unspecified or a documentation address"
                );
                out.push(finding(path, n, reason));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Home directories

/// The prefixes of the home directories of a person or a host, spelled in two pieces so that
/// this file does not hold them.
const HOME_PREFIXES: [&str; 2] = [concat!("/ho", "me/"), concat!("/Us", "ers/")];

/// Paths under either prefix of `HOME_PREFIXES`.
pub fn home_paths(path: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (n, line) in numbered(text) {
        for prefix in HOME_PREFIXES {
            if line.contains(prefix) {
                let reason = format!("the path prefix `{prefix}` names a home directory");
                out.push(finding(path, n, reason));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Example domains, e-mail addresses

const EXAMPLE_DOMAINS: [&str; 7] = [
    "example.com",
    "example.net",
    "example.org",
    "example",
    "localhost",
    "test",
    "invalid",
];
const EXAMPLE_SUFFIXES: [&str; 7] = [
    ".example.com",
    ".example.net",
    ".example.org",
    ".example",
    ".test",
    ".invalid",
    ".localhost",
];

/// The suffixes of the names of systemd units: `name@instance.service` is not an address.
const UNIT_SUFFIXES: [&str; 11] = [
    "service",
    "socket",
    "device",
    "mount",
    "automount",
    "swap",
    "target",
    "path",
    "timer",
    "slice",
    "scope",
];

fn is_example_domain(host: &str) -> bool {
    EXAMPLE_DOMAINS.contains(&host) || EXAMPLE_SUFFIXES.iter().any(|s| host.ends_with(s))
}

fn is_local_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_domain_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

/// E-mail addresses outside the example domains.
pub fn emails(path: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (n, line) in numbered(text) {
        let b = line.as_bytes();
        for (at, _) in line.match_indices('@') {
            let mut local_start = at;
            while local_start > 0 && is_local_byte(b[local_start - 1]) {
                local_start -= 1;
            }
            // `\n@` in a string is a newline and an at sign, not a local part `n`.
            if local_start < at && local_start > 0 && b[local_start - 1] == b'\\' {
                local_start += 1;
            }
            // A quoted local part: `"any text"@domain`.
            if local_start == at && at > 1 && b[at - 1] == b'"' {
                let open = line[..at - 1].rfind('"').filter(|&o| o + 2 < at);
                local_start = open.unwrap_or(local_start);
            }
            let mut domain_end = at + 1;
            while domain_end < b.len() && is_domain_byte(b[domain_end]) {
                domain_end += 1;
            }
            let domain = line[at + 1..domain_end].trim_end_matches(['.', '-']);
            if local_start == at || !looks_like_mail_domain(domain) {
                continue;
            }
            let lower = domain.to_ascii_lowercase();
            let last = lower.rsplit('.').next().unwrap_or("");
            if UNIT_SUFFIXES.contains(&last) || is_example_domain(&lower) {
                continue;
            }
            let address = &line[local_start..at + 1 + domain.len()];
            let reason = format!("the e-mail address `{address}` is not at an example domain");
            out.push(finding(path, n, reason));
        }
    }
    out
}

/// Dotted labels, the first starting with a letter or a digit and the last of letters only.
fn looks_like_mail_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    let first_is_alnum = labels[0].starts_with(|c: char| c.is_ascii_alphanumeric());
    let last = labels[labels.len() - 1];
    first_is_alnum && last.len() >= 2 && last.bytes().all(|c| c.is_ascii_alphabetic())
}

// ---------------------------------------------------------------------------------------------
// Hostnames

/// True when `host` is an example domain or is, or is under, a domain of the allow list.
fn is_allowed_host(host: &str, public: &[String]) -> bool {
    if is_example_domain(host) {
        return true;
    }
    public.iter().any(|domain| {
        let under = host.strip_suffix(domain.as_str());
        domain == host || under.is_some_and(|rest| rest.ends_with('.'))
    })
}

/// A URL in a line: its host and the byte span from `://` to the end of the URL.
struct Url {
    host: String,
    start: usize,
    end: usize,
}

fn ends_url(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '`' | ',')
}

fn urls(line: &str) -> Vec<Url> {
    let mut out = Vec::new();
    for (idx, _) in line.match_indices("://") {
        let after = idx + 3;
        let rest = &line[after..];
        let end = after + rest.find(ends_url).unwrap_or(rest.len());
        let token = &line[after..end];
        let cut = token.find(['/', '?', '#']).unwrap_or(token.len());
        let authority = &token[..cut];
        let host_port = authority.rsplit('@').next().unwrap_or("");
        let host = host_port
            .split(':')
            .next()
            .unwrap_or("")
            .trim_end_matches('.');
        let valid = host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        let numeric = host.bytes().all(|b| b.is_ascii_digit() || b == b'.');
        if host.is_empty() || !valid || numeric {
            continue;
        }
        out.push(Url {
            host: host.to_ascii_lowercase(),
            start: idx,
            end,
        });
    }
    out
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')
}

fn is_toml_table_header(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('[') && line.ends_with(']')
}

/// Hosts under a non-example domain that is not on `public`.
pub fn hostnames(path: &str, text: &str, public: &[String]) -> Vec<Finding> {
    let is_rust = path.ends_with(".rs");
    let is_toml = path.ends_with(".toml");
    let mask = if is_rust {
        rust_text_mask(text)
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    let mut base = 0;
    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let n = index + 1;
        let line_start = base;
        base += raw.len();
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let found_urls = urls(line);
        for url in &found_urls {
            if !is_allowed_host(&url.host, public) {
                out.push(finding(path, n, host_reason(&url.host)));
            }
        }
        if is_toml && is_toml_table_header(line) {
            continue;
        }
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if !is_name_byte(b[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && is_name_byte(b[i]) {
                i += 1;
            }
            let end = i;
            let inside_url = found_urls.iter().any(|u| start < u.end && end > u.start);
            let after_at = start > 0 && b[start - 1] == b'@';
            let call = b.get(end) == Some(&b'(');
            let name = line[start..end].trim_end_matches('.').to_ascii_lowercase();
            let before = start.checked_sub(1).map(|p| b[p]);
            let after = b.get(end).copied();
            if inside_url || after_at || call || !is_bare_host(&name, before, after) {
                continue;
            }
            if is_rust && !mask[line_start + start] {
                continue;
            }
            if !is_allowed_host(&name, public) {
                out.push(finding(path, n, host_reason(&name)));
            }
        }
    }
    out
}

fn host_reason(host: &str) -> String {
    format!(
        "the host `{host}` is not an example domain and not on tests/hygiene/public_domains.txt"
    )
}

/// True when a byte next to a name makes it part of a path.
fn is_path_edge(b: Option<u8>) -> bool {
    b == Some(b'/')
}

/// Dotted labels of letters, digits and hyphens, none empty, at least two, in any case, the last
/// a top-level domain of the IANA list. When the last label is also a file extension of this
/// repository (`FILE_SUFFIXES`), the name is a file name, not a host, if it has exactly two labels
/// (`lib.rs`) or is part of a path (`/` before or after it); with three or more labels and no
/// path it is a host.
fn is_bare_host(name: &str, before: Option<u8>, after: Option<u8>) -> bool {
    let lower = name.to_ascii_lowercase();
    let plain = lower
        .bytes()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-'));
    if !plain {
        return false;
    }
    let labels: Vec<&str> = lower.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    let last = labels[labels.len() - 1];
    if !is_tld(last) {
        return false;
    }
    // A component of a path (a `/` next to it) is a file name, whatever its suffix; so is
    // `stem.ext` when the suffix is also a file extension.
    let in_path = is_path_edge(before) || is_path_edge(after);
    let file_like = in_path || (FILE_SUFFIXES.contains(&last) && labels.len() == 2);
    !file_like
}

// ---------------------------------------------------------------------------------------------
// Internal repository names

/// The lowercase hex SHA-256 of a token.
pub fn digest_of(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Tokens whose digest is on the deny list. The text is lowercased and split at every character
/// outside `[a-z0-9_-]`; the name itself is never printed, only its digest.
pub fn repositories(path: &str, text: &str, denied: &HashSet<String>) -> Vec<Finding> {
    let mut out = Vec::new();
    for (n, line) in numbered(text) {
        let lower = line.to_lowercase();
        let tokens = lower.split(|c: char| !matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'));
        for token in tokens {
            if token.is_empty() {
                continue;
            }
            let digest = digest_of(token);
            if denied.contains(&digest) {
                let reason = format!(
                    "a name on the deny list of internal repository names (sha-256 {digest})"
                );
                out.push(finding(path, n, reason));
            }
        }
    }
    out
}
