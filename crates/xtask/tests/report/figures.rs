//! The figures of the report: the problem figures, the methods any text names, the update
//! diagram and the mock-ups of the web UI.
//!
//! - A problem figure is a `<figure data-figure="problem">`; the report has
//!   [`MIN_PROBLEM_FIGURES`] to [`MAX_PROBLEM_FIGURES`] of them.
//! - Every `Service.Method` the page names, in prose, in code or in a diagram (its text and its
//!   `aria-label` values; `<style>`, `<script>` and comments are not read), is a method of a
//!   service of the protos.
//! - The update diagram is the one `<svg data-diagram="update">`. Every update step of the hook
//!   contract is the whole text of a `<text>` element of it, so a step that is only part of a
//!   longer label or a note does not count.
//! - A mock-up of the web UI is an element with `data-ui-page="<page>"`, one for each module
//!   `ui/pages/<page>.js` and none for a page that does not exist. Inside a mock-up every element
//!   whose class list holds `ui-t` is a string the UI prints: its text, white space collapsed, is
//!   a string literal of the page's module or of a shared one ([`Ui`]), so a mock-up cannot show a
//!   button, heading or column the UI does not have.

use std::collections::{BTreeMap, BTreeSet};

use crate::html::{Tag, attr, attributes, element_end, line_of, plain, tags};

/// The fewest and the most problem figures the report may hold.
pub const MIN_PROBLEM_FIGURES: usize = 2;
pub const MAX_PROBLEM_FIGURES: usize = 4;

/// One line when the number of `<figure data-figure="problem">` of `masked` is out of
/// [`MIN_PROBLEM_FIGURES`]..=[`MAX_PROBLEM_FIGURES`].
pub fn problem_figure_problems(masked: &str) -> Vec<String> {
    let n = tags(masked)
        .iter()
        .filter(|t| !t.closing && t.name.eq_ignore_ascii_case("figure"))
        .filter(|t| attr(t.attrs, "data-figure") == Some("problem"))
        .count();
    if (MIN_PROBLEM_FIGURES..=MAX_PROBLEM_FIGURES).contains(&n) {
        Vec::new()
    } else {
        vec![format!(
            "{n} problem figures, not between {MIN_PROBLEM_FIGURES} and {MAX_PROBLEM_FIGURES}"
        )]
    }
}

/// The `Service.Method` pairs of the `.proto` texts: a `service <Name> {` line opens a service,
/// and each `rpc <Method>(` inside it adds a pair.
pub fn rpcs<'a>(protos: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for text in protos {
        let mut service: Option<&str> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("service ") {
                service = rest.split_whitespace().next();
            } else if let Some(rest) = line.strip_prefix("rpc ") {
                let method = rest.split(['(', ' ']).next().unwrap_or("");
                if let Some(service) = service {
                    out.insert(format!("{service}.{method}"));
                }
            }
        }
    }
    out
}

/// The `Service.Method` names a text holds: two identifiers each starting with an upper-case
/// letter, joined by a point, not glued to a word character or a point before it nor followed by
/// a point and a word character (so `X.Y.Z` is none; a sentence's full stop after one is not read
/// as part of it).
pub fn method_names(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let starts = bytes[i].is_ascii_uppercase()
            && (i == 0 || !(ident(bytes[i - 1]) || bytes[i - 1] == b'.'));
        if !starts {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < bytes.len() && ident(bytes[j]) {
            j += 1;
        }
        let second =
            bytes.get(j) == Some(&b'.') && bytes.get(j + 1).is_some_and(u8::is_ascii_uppercase);
        if !second {
            i = j;
            continue;
        }
        let mut k = j + 1;
        while k < bytes.len() && ident(bytes[k]) {
            k += 1;
        }
        let longer = bytes.get(k) == Some(&b'.') && bytes.get(k + 1).copied().is_some_and(ident);
        if !longer {
            out.push(&text[i..k]);
        }
        i = k;
    }
    out
}

/// One line per `Service.Method` that `raw` (the page with `<style>`, `<script>` and comments
/// masked, code kept) names on a line, in its text (each tag read as a space, so two labels side
/// by side are not one word) or in an `aria-label`, and that no service of `rpcs` has.
pub fn method_problems(raw: &str, rpcs: &BTreeSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    for (n, line) in raw.lines().enumerate() {
        let mut bytes = line.as_bytes().to_vec();
        for tag in tags(line) {
            crate::page::blank(&mut bytes, tag.start, tag.end);
        }
        let text = String::from_utf8(bytes).expect("blanking keeps UTF-8");
        let mut texts = vec![crate::html::strip_entities(&text)];
        for tag in tags(line) {
            texts.extend(
                attributes(tag.attrs)
                    .into_iter()
                    .filter(|a| a.name.eq_ignore_ascii_case("aria-label"))
                    .filter_map(|a| match a.value {
                        crate::html::Value::Double(v) => Some(crate::html::strip_entities(v)),
                        _ => None,
                    }),
            );
        }
        for text in &texts {
            for name in method_names(text) {
                if !rpcs.contains(name) {
                    found.push(format!("line {}: {name} is no method of the protos", n + 1));
                }
            }
        }
    }
    found
}

/// The words of the update steps of the hook contract, in order, read from the source of
/// `hostkeeper_hook::Step`: the variants listed in `Step::ALL`, each spelled as `Step::name`
/// spells it (`Step::Prepare => "prepare"`). Empty when the source has neither.
pub fn step_words(source: &str) -> Vec<String> {
    let Some(all) = source.split("pub const ALL").nth(1) else {
        return Vec::new();
    };
    let all = &all[..all.find("];").unwrap_or(all.len())];
    all.split("Step::")
        .skip(1)
        .filter_map(|rest| {
            let variant: String = rest
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            let arm = format!("Step::{variant} => \"");
            let at = source.find(&arm)? + arm.len();
            source[at..].split('"').next().map(str::to_owned)
        })
        .collect()
}

/// The plain text of the element whose start tag is `all[k]`, white space collapsed; `None` when
/// it is never closed.
fn element_text(masked: &str, all: &[Tag<'_>], k: usize) -> Option<String> {
    let end = element_end(all, k)?;
    Some(collapse(&plain(&masked[all[k].end..end])))
}

/// `text` with each run of white space made one space, its ends kept.
fn squash(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if !c.is_whitespace() {
            out.push(c);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out
}

/// `text` trimmed, each run of white space made one space.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The indices of the tags of `all` from `k + 1` up to the end of the element `all[k]`.
fn inside(all: &[Tag<'_>], k: usize, end: usize) -> std::ops::Range<usize> {
    let last = all[k + 1..]
        .iter()
        .position(|t| t.start >= end)
        .map_or(all.len(), |p| k + 1 + p);
    k + 1..last
}

/// One line when `masked` has no `<svg data-diagram="update">` or more than one, or the one it has
/// is never closed, and one per step of `steps` that is not the whole text of a `<text>` element
/// of it.
pub fn update_problems(masked: &str, steps: &[String]) -> Vec<String> {
    let all = tags(masked);
    let marked: Vec<usize> = (0..all.len())
        .filter(|&k| {
            let t = &all[k];
            !t.closing
                && t.name.eq_ignore_ascii_case("svg")
                && attr(t.attrs, "data-diagram") == Some("update")
        })
        .collect();
    let k = match marked.as_slice() {
        [] => return vec!["no svg has data-diagram=\"update\"".to_owned()],
        [k] => *k,
        _ => {
            return vec![format!(
                "{} svgs have data-diagram=\"update\", not one",
                marked.len()
            )];
        }
    };
    let line = line_of(masked, all[k].start);
    let Some(end) = element_end(&all, k) else {
        return vec![format!("line {line}: the update diagram is never closed")];
    };
    let words: BTreeSet<String> = inside(&all, k, end)
        .filter(|&j| !all[j].closing && all[j].name.eq_ignore_ascii_case("text"))
        .filter_map(|j| element_text(masked, &all, j))
        .collect();
    steps
        .iter()
        .filter(|step| !words.contains(step.as_str()))
        .map(|step| format!("line {line}: the update diagram does not name the step {step}"))
        .collect()
}

/// A string literal of the UI: a whole string, or the static parts of a template literal around
/// its `${…}` holes.
#[derive(Debug, PartialEq)]
pub enum Literal {
    Plain(String),
    Template(Vec<String>),
}

impl Literal {
    /// Whether `label` (white space collapsed) is this literal: equal to a string, or a template
    /// with its holes filled, the first static part leading, the last ending it and the others in
    /// order between. A template whose static parts hold no letter or digit matches nothing.
    fn matches(&self, label: &str) -> bool {
        match self {
            Literal::Plain(s) => collapse(s) == label,
            Literal::Template(parts) => {
                if !parts.iter().any(|p| p.chars().any(char::is_alphanumeric)) {
                    return false;
                }
                let parts: Vec<String> = parts.iter().map(|p| squash(p)).collect();
                let (first, last) = (&parts[0], &parts[parts.len() - 1]);
                let Some(mut rest) = label.strip_prefix(first.trim_start()) else {
                    return false;
                };
                for middle in &parts[1..parts.len() - 1] {
                    match rest.find(middle.as_str()) {
                        Some(at) => rest = &rest[at + middle.len()..],
                        None => return false,
                    }
                }
                rest.ends_with(last.trim_end())
            }
        }
    }
}

/// The string literals of a JavaScript source: `'…'`, `"…"` and template literals, those inside a
/// template's holes too. `//` and `/* */` comments are skipped. A backslash escape is read as the
/// character it escapes (`\n` and `\t` as white space). A regular expression literal is not
/// recognised: one holding a quote, a backtick or `//` would be misread (the UI's hold none).
pub fn js_literals(source: &str) -> Vec<Literal> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    lex(&chars, 0, false, &mut out);
    out
}

/// The character an escape `\c` stands for.
fn unescape(c: char) -> char {
    match c {
        'n' | 't' | 'r' => ' ',
        c => c,
    }
}

/// Lexes `chars` from `i`, pushing literals onto `out`; inside a template hole (`hole`) it stops
/// after the `}` that closes the hole and returns the index after it.
fn lex(chars: &[char], mut i: usize, hole: bool, out: &mut Vec<Literal>) -> usize {
    let mut depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '/' if next == Some('/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if next == Some('*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
            }
            '\'' | '"' => {
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != c && chars[i] != '\n' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        s.push(unescape(chars[i]));
                    } else {
                        s.push(chars[i]);
                    }
                    i += 1;
                }
                out.push(Literal::Plain(s));
                i += 1;
            }
            '`' => {
                let mut parts = vec![String::new()];
                i += 1;
                while i < chars.len() && chars[i] != '`' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        if let Some(p) = parts.last_mut() {
                            p.push(unescape(chars[i]));
                        }
                        i += 1;
                    } else if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
                        i = lex(chars, i + 2, true, out);
                        parts.push(String::new());
                    } else {
                        if let Some(p) = parts.last_mut() {
                            p.push(chars[i]);
                        }
                        i += 1;
                    }
                }
                out.push(if parts.len() == 1 {
                    Literal::Plain(parts.remove(0))
                } else {
                    Literal::Template(parts)
                });
                i += 1;
            }
            '{' if hole => {
                depth += 1;
                i += 1;
            }
            '}' if hole => {
                if depth == 0 {
                    return i + 1;
                }
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    i
}

/// The texts of an HTML file of the UI (its `index.html`): each run of text between tags,
/// trimmed, as a plain literal.
pub fn html_literals(source: &str) -> Vec<Literal> {
    let all = tags(source);
    let mut out = Vec::new();
    let mut from = 0;
    for tag in all.iter().chain(std::iter::once(&Tag {
        name: "",
        attrs: "",
        start: source.len(),
        end: source.len(),
        closing: true,
        open: false,
    })) {
        let text = collapse(&source[from..tag.start]);
        if !text.is_empty() {
            out.push(Literal::Plain(text));
        }
        from = tag.end;
    }
    out
}

/// The strings of the web UI: the literals of each page module (`ui/pages/<page>.js`, by page)
/// and of the shared modules every page uses.
pub struct Ui {
    pub pages: BTreeMap<String, Vec<Literal>>,
    pub shared: Vec<Literal>,
}

/// One line per mock-up (`data-ui-page`) of a page the UI has no module for, per page with no
/// mock-up or with two, per `ui-t` element outside a mock-up or never closed, and per `ui-t`
/// element whose text is no literal of its page or of the shared modules.
pub fn ui_problems(masked: &str, ui: &Ui) -> Vec<String> {
    let all = tags(masked);
    let mut found = Vec::new();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut covered: Vec<(usize, usize)> = Vec::new();
    for (k, tag) in all.iter().enumerate() {
        if tag.closing {
            continue;
        }
        let Some(page) = attr(tag.attrs, "data-ui-page") else {
            continue;
        };
        let line = line_of(masked, tag.start);
        let Some(end) = element_end(&all, k) else {
            found.push(format!(
                "line {line}: the mock-up of {page} is never closed"
            ));
            continue;
        };
        covered.push((tag.start, end));
        if let Some(first) = seen.insert(page, line) {
            found.push(format!(
                "line {line}: a second mock-up of {page} (first on line {first})"
            ));
        }
        let Some(literals) = ui.pages.get(page) else {
            found.push(format!(
                "line {line}: a mock-up of {page}, which is no page of ui/pages"
            ));
            continue;
        };
        for j in inside(&all, k, end) {
            let t = &all[j];
            if t.closing || !is_ui_text(t) {
                continue;
            }
            let at = line_of(masked, t.start);
            let Some(label) = element_text(masked, &all, j) else {
                found.push(format!("line {at}: a ui-t element is never closed"));
                continue;
            };
            let known = literals.iter().chain(&ui.shared).any(|l| l.matches(&label));
            if !known {
                found.push(format!(
                    "line {at}: the mock-up of {page} shows {label:?}, which no literal of the UI holds"
                ));
            }
        }
    }
    for tag in all.iter().filter(|t| !t.closing && is_ui_text(t)) {
        if !covered
            .iter()
            .any(|(s, e)| *s < tag.start && tag.start < *e)
        {
            found.push(format!(
                "line {}: a ui-t element outside every mock-up",
                line_of(masked, tag.start)
            ));
        }
    }
    for page in ui.pages.keys() {
        if !seen.contains_key(page.as_str()) {
            found.push(format!("the page {page} of ui/pages has no mock-up"));
        }
    }
    found
}

/// Whether a tag's class list holds `ui-t`.
fn is_ui_text(tag: &Tag<'_>) -> bool {
    attr(tag.attrs, "class").is_some_and(|c| c.split_whitespace().any(|w| w == "ui-t"))
}
