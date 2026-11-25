//! What the web app's sources must never contain, each as a pure function of a file's path and
//! text that returns one [`Finding`] per offence.
//!
//! - [`token_keepers`]: a place a token could be kept past the tab or shown: browser storage,
//!   a cookie, the address (the location's hash and search, `history.`) and the console;
//! - [`network_and_code`]: a URL of another origin (`http://`, `https://`, protocol-relative
//!   `//`), a stylesheet import, code made from text (`eval(`, `Function(`, `import(`), markup
//!   made from text (`innerHTML` and its kin), and any other channel than `fetch` and the one
//!   socket (`WebSocket` is found in every file but [`SOCKET_OPENER`], `ws://` and `wss://` in every file);
//! - [`inline_code`]: inline code, which the page's Content-Security-Policy refuses for script
//!   and the app's own sources do not use for style either: in HTML a script that is not a `src`
//!   reference, a `style` element or attribute and an `on*=` handler; in JavaScript an `on*`
//!   handler property and an `on*` attribute;
//! - [`fetches`]: a call of `fetch`, which only the data layer and the entry module may make;
//! - [`sockets`]: a call of `WebSocket`, which only the entry module may make, once (it builds the
//!   one function, `openSocket`, that pages are given as `ctx.openSocket`, as they are given `fetch`),
//!   and [`opener`], what that one call must look like: its address is `socketAddress`'s, which is
//!   the page's own host and a path of a fixed shape (`ui/socket_address.js`);
//! - [`vendored_code`]: what a vendored library (a file under [`crate::vendor::VENDOR_DIR`]) is
//!   held to instead of all of the above: no inline script or handler made from text and no
//!   `WebSocket(` call. [`scan_file`] picks a file's rules by its directory.
//!
//! The one exception: `console.` is not found as the service of the generated client, `api.console.`
//! or `ctx.api.console.` (the operator API's Console service) and the `"function"` values of `calls.json`;
//! see [`is_client_service`].
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
            if called && !(word && joined(text, at)) && !is_client_service(path, stem, text, at) {
                out.push(finding(path, line_of(text, at), format!("`{needle}`")));
            }
        }
    }
    out.sort_by_key(|f| f.line);
    out
}

/// Whether the match of `stem` at `at` in the file `path` is the `console` service of the generated
/// API client: in a page, as in `ctx.api.console.power(`, where the client is named `api` (`calls.rs`
/// reads no other name), so the match is exempt only when `api.` stands right before it and no
/// letter, digit, `_` or `$` before that `api` makes it another name (`xapi.console.` is found, as
/// are `console.log(` and `window.console.`); and in `calls.json`, the derived list of the calls,
/// as the value of a `"function"` key (`"function": "console.power"`).
fn is_client_service(path: &str, stem: &str, text: &str, at: usize) -> bool {
    if stem != "console." {
        return false;
    }
    let before = &text[..at];
    if path == "calls.json" {
        return before.ends_with("\"function\": \"");
    }
    before.strip_suffix("api.").is_some_and(|name| {
        !name.ends_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
    })
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
    "ws://",
    "wss://",
    "EventSource",
    "sendBeacon",
];

/// The one address the app may name: the documentation as GitHub renders it, which a page links to
/// (`docLink` and `deployDocLink` in `view.js`, which refuse any `href` outside `docs/operations/`
/// and `docs/deploy/`). A link is a navigation the operator chooses, not a load; any other
/// `https://` is still found.
pub const DOCS_LINK: &str = "https://github.com/komira-ai/hostkeeper/blob/main/docs/";

/// The one file that may name a `WebSocket`: the entry module, which builds the function that opens
/// the Remote SSH terminal's socket on the page's own host and hands it to the pages. Every other
/// file is refused the word. No file may write a `ws://` or `wss://` address, this one included: it
/// builds the address from the page's own protocol and host.
pub const SOCKET_OPENER: &str = "main.js";

/// The needle of [`NETWORK_AND_CODE`] that [`SOCKET_OPENER`] may hold.
const SOCKET_NEEDLE: &str = "`WebSocket`";

/// The places [`NETWORK_AND_CODE`] names, in any file, but for [`DOCS_LINK`], and, in
/// [`SOCKET_OPENER`], for the `WebSocket` needle.
pub fn network_and_code(path: &str, text: &str) -> Vec<Finding> {
    let mut found = needles(path, text, NETWORK_AND_CODE);
    if path == SOCKET_OPENER {
        found.retain(|f| f.what != SOCKET_NEEDLE);
    }
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
    out.extend(handler_attributes(path, text));
    out
}

/// Every `setAttribute(` of an `on*` attribute: a handler made from text.
fn handler_attributes(path: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
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

/// Inline code: in an `.html` file, a script that is not exactly `<script ... src=...></script>`,
/// a `style` element or attribute, and an `on*=` handler; in a `.js` file, an `.on*` handler
/// property set and an `on*` attribute set. Other files: nothing. The policy refuses the inline
/// script and handlers; it allows inline style (a vendored terminal draws with it), which the
/// app's own sources still do not use.
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

/// Every call of `WebSocket` (`new WebSocket(address)`).
pub fn sockets(path: &str, text: &str) -> Vec<Finding> {
    needles(path, text, &["WebSocket("])
}

/// The first argument of [`SOCKET_OPENER`]'s `WebSocket(` call: the address
/// `ui/socket_address.js` builds from the page's own location and the path asked for, which it
/// refuses unless the path has one of its fixed shapes.
pub const SOCKET_ADDRESS: &str = "socketAddress(window.location, path)";

/// The only second argument that call may have: the subprotocols the page offers (a console
/// session's name and its CSRF offer), never an address.
pub const SOCKET_PROTOCOLS: &str = "protocols";

/// Whether `argument` (a call's arguments, trimmed) is [`SOCKET_ADDRESS`] alone or followed by
/// `, ` and [`SOCKET_PROTOCOLS`].
fn opener_arguments(argument: &str) -> bool {
    argument == SOCKET_ADDRESS
        || argument
            .strip_prefix(SOCKET_ADDRESS)
            .and_then(|rest| rest.trim_start().strip_prefix(','))
            .is_some_and(|rest| rest.trim() == SOCKET_PROTOCOLS)
}

/// The text between the `(` that opens at `open` in `text` and the `)` that closes it, trimmed,
/// or `None` when it is never closed.
fn argument(text: &str, open: usize) -> Option<&str> {
    let mut depth = 0usize;
    for (at, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[open + 1..open + at].trim());
                }
            }
            _ => {}
        }
    }
    None
}

/// What [`SOCKET_OPENER`] must hold: exactly one `WebSocket(` call, and that call's arguments are
/// [`SOCKET_ADDRESS`] (so the socket goes to the page's own host, at a path of a fixed shape),
/// alone or with [`SOCKET_PROTOCOLS`] after it. One finding for a file with no call, one for each
/// call past the first and one for each call with other arguments.
pub fn opener(path: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen = 0usize;
    for (at, _) in text.match_indices("WebSocket") {
        let rest = skip_blank(&text[at + "WebSocket".len()..]);
        if joined(text, at) || !rest.starts_with('(') {
            continue;
        }
        seen += 1;
        let line = line_of(text, at);
        if seen > 1 {
            out.push(finding(path, line, "a second `WebSocket(` call"));
        }
        if !argument(text, text.len() - rest.len()).is_some_and(opener_arguments) {
            out.push(finding(
                path,
                line,
                format!("a socket address not built by `{SOCKET_ADDRESS}`"),
            ));
        }
    }
    if seen == 0 {
        out.push(finding(path, 1, "no `WebSocket(` call"));
    }
    out
}

/// What a vendored file (under [`crate::vendor::VENDOR_DIR`]) is held to. It is exempt from the
/// needles ([`token_keepers`], [`network_and_code`], [`fetches`]: minified third-party code names
/// licence URLs and `console.`); it is still refused, in HTML, everything [`inline_code`] finds;
/// in JavaScript, an `on*` attribute set (a handler made from text, which the policy refuses; an
/// `.on*` property set to a function is not inline code, and the libraries set their sockets'
/// and emitters' handlers that way); and in any file, a call of `WebSocket` (the app's one socket
/// is opened by [`SOCKET_OPENER`] and handed to the libraries open).
pub fn vendored_code(path: &str, text: &str) -> Vec<Finding> {
    let mut out = if path.ends_with(".html") {
        html_inline(path, text)
    } else if path.ends_with(".js") {
        handler_attributes(path, text)
    } else {
        Vec::new()
    };
    out.extend(sockets(path, text));
    out
}

/// Every finding of the rules that hold for any one file, by its directory: for a vendored file,
/// [`vendored_code`]; for any other, [`token_keepers`], [`network_and_code`] and [`inline_code`].
/// The rules that count over the whole app ([`fetches`], [`sockets`] and [`opener`]) are not here.
pub fn scan_file(path: &str, text: &str) -> Vec<Finding> {
    if crate::vendor::is_vendored(path) {
        return vendored_code(path, text);
    }
    let mut out = token_keepers(path, text);
    out.extend(network_and_code(path, text));
    out.extend(inline_code(path, text));
    out
}

#[cfg(test)]
mod tests;
