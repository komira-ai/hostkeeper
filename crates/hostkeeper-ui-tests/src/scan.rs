//! What the web app's sources must never contain, each as a pure function of a file's path and
//! text that returns one [`Finding`] per offence.
//!
//! - [`token_keepers`]: a place a token could be kept past the tab or shown: browser storage,
//!   a cookie, the address (the location's hash and search, `history.`) and the console;
//! - [`network_and_code`]: a URL of another origin (`http://`, `https://`, protocol-relative
//!   `//`), a stylesheet import, code made from text (`eval(`, `Function(`, `import(`), markup
//!   made from text (`innerHTML` and its kin), and any other channel than `fetch`;
//! - [`inline_code`]: what the page's Content-Security-Policy (`default-src 'self'`) refuses:
//!   in HTML a script that is not a `src` reference, a `style` element or attribute and an
//!   `on*=` handler; in JavaScript an `on*` handler property and an `on*` attribute;
//! - [`fetches`]: a call of `fetch`, which only the data layer and the entry module may make.
//!
//! A needle that opens with a letter matches only where no letter, digit, `_` or `$` comes
//! before it (`eval(` is not found in `retrieval(`).

use std::fmt;

use crate::lexis::{joined, line_of, skip_blank};

/// One offence: the file, the 1-based line and what was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub what: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.what)
    }
}

fn finding(path: &str, line: usize, what: impl Into<String>) -> Finding {
    Finding {
        path: path.to_owned(),
        line,
        what: what.into(),
    }
}

/// Every place in `text` where a needle of `list` is, one finding each, in line order (and in
/// the list's order within a line). A needle that ends in `(` is a call: its name followed by
/// any white space and comments, then `(` (`eval /* x */ (text)` is `eval(`).
fn needles(path: &str, text: &str, list: &[&str]) -> Vec<Finding> {
    let mut out = Vec::new();
    for needle in list {
        let (stem, call) = needle
            .strip_suffix('(')
            .map_or((*needle, false), |stem| (stem, true));
        let word = stem.starts_with(|c: char| c.is_ascii_alphabetic());
        for (at, _) in text.match_indices(stem) {
            let called = !call || skip_blank(&text[at + stem.len()..]).starts_with('(');
            if called && !(word && joined(text, at)) {
                out.push(finding(path, line_of(text, at), format!("`{needle}`")));
            }
        }
    }
    out.sort_by_key(|f| f.line);
    out
}

/// Where a token could be kept past the tab, put in the address, or logged.
pub const TOKEN_KEEPERS: &[&str] = &[
    "localStorage",
    "sessionStorage",
    "indexedDB",
    "cookie",
    "location.hash",
    // Spelled in two pieces: the name ends in a word the hygiene lint reads as a domain.
    concat!("location", ".search"),
    "history.",
    "console.",
];

/// The places [`TOKEN_KEEPERS`] names, in any file.
pub fn token_keepers(path: &str, text: &str) -> Vec<Finding> {
    needles(path, text, TOKEN_KEEPERS)
}

/// A URL of another origin, a load the app does not need, code or markup made from text, and a
/// channel to the network other than `fetch`.
pub const NETWORK_AND_CODE: &[&str] = &[
    "http://",
    "https://",
    "\"//",
    "'//",
    "`//",
    "(//",
    "=//",
    "//cdn",
    "url(",
    "@import",
    "eval(",
    "Function(",
    "import(",
    "innerHTML",
    "outerHTML",
    "insertAdjacentHTML",
    "document.write",
    "srcdoc",
    "javascript:",
    "XMLHttpRequest",
    "WebSocket",
    "EventSource",
    "sendBeacon",
];

/// The one address the app may name: the documentation as GitHub renders it, which a page links to
/// (`docLink` and `deployDocLink` in `view.js`, which refuse any `href` outside `docs/operations/`
/// and `docs/deploy/`). A link is a navigation the operator chooses, not a load; any other
/// `https://` is still found.
pub const DOCS_LINK: &str = "https://github.com/komira-ai/hostkeeper/blob/main/docs/";

/// The places [`NETWORK_AND_CODE`] names, in any file, but for [`DOCS_LINK`].
pub fn network_and_code(path: &str, text: &str) -> Vec<Finding> {
    let mut found = needles(path, text, NETWORK_AND_CODE);
    found.retain(|f| {
        !(f.what == "`https://`"
            && text
                .lines()
                .nth(f.line - 1)
                .is_some_and(|l| l.matches("https://").count() == l.matches(DOCS_LINK).count()))
    });
    found
}

/// Whether `rest`, the text after `on`, is the rest of a handler being set: one or more letters,
/// then `=` (not `==`), spaces allowed before it.
fn sets_handler(rest: &str) -> bool {
    let name = rest.len()
        - rest
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .len();
    let after = rest[name..].trim_start();
    name > 0 && after.starts_with('=') && !after.starts_with("==")
}

/// Where lower-case HTML sets the attribute `name`: the name after white space, then `=` after
/// any white space (`\n  STYLE =` once lowered).
fn attributes(lower: &str, name: &str) -> Vec<usize> {
    lower
        .match_indices(name)
        .map(|(at, _)| at)
        .filter(|&at| {
            lower[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
                && lower[at + name.len()..].trim_start().starts_with('=')
        })
        .collect()
}

fn has_attribute(tag: &str, name: &str) -> bool {
    !attributes(tag, name).is_empty()
}

fn html_inline(path: &str, text: &str) -> Vec<Finding> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    for (at, _) in lower.match_indices("<script") {
        let end = lower[at..].find('>').map_or(lower.len(), |e| at + e);
        let tag = &lower[at..end];
        if !has_attribute(tag, "src") {
            out.push(finding(path, line_of(text, at), "a script without `src`"));
        } else if !lower[end..].starts_with("></script>") {
            out.push(finding(path, line_of(text, at), "a script with a body"));
        }
    }
    for (at, _) in lower.match_indices("<style") {
        out.push(finding(path, line_of(text, at), "a `style` element"));
    }
    for (at, _) in lower.match_indices("on") {
        let spaced = lower[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        if spaced && sets_handler(&lower[at + 2..]) {
            out.push(finding(path, line_of(text, at), "an inline event handler"));
        }
    }
    for at in attributes(&lower, "style") {
        out.push(finding(path, line_of(text, at), "an inline style"));
    }
    out
}

fn script_inline(path: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices(".on") {
        if sets_handler(&text[at + 3..]) {
            out.push(finding(
                path,
                line_of(text, at),
                "an event handler property",
            ));
        }
    }
    for (at, _) in text.match_indices("setAttribute(") {
        let name = text[at + 13..]
            .trim_start()
            .trim_start_matches(['"', '\'', '`']);
        if name.to_ascii_lowercase().starts_with("on") {
            out.push(finding(
                path,
                line_of(text, at),
                "an event handler attribute",
            ));
        }
    }
    out
}

/// What the Content-Security-Policy refuses: in an `.html` file, a script that is not exactly
/// `<script ... src=...></script>`, a `style` element or attribute, and an `on*=` handler; in a
/// `.js` file, an `.on*` handler property set and an `on*` attribute set. Other files: nothing.
pub fn inline_code(path: &str, text: &str) -> Vec<Finding> {
    if path.ends_with(".html") {
        html_inline(path, text)
    } else if path.ends_with(".js") {
        script_inline(path, text)
    } else {
        Vec::new()
    }
}

/// Every call of `fetch`.
pub fn fetches(path: &str, text: &str) -> Vec<Finding> {
    needles(path, text, &["fetch("])
}

#[cfg(test)]
mod tests;
