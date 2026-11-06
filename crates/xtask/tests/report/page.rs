//! The HTML of the report: what is code and what is prose, its numbers, its ids and its links.
//!
//! Code is never read as prose. Before the cells are found and the prose is scanned, the page is
//! masked: the contents of every `<style>` and `<script>` element, then every HTML comment, then
//! the contents of every `<code>` and `<pre>` element are blanked (each byte but a newline made a
//! space, so byte offsets and line numbers stay those of the file). An element is matched by its
//! name in any letter case, up to the end tag that closes it (the same name nested inside counts);
//! an element never closed, and a `<!--` with no `-->`, hide nothing and stay text. A code element
//! quotes an identifier, a path or a literal of the code; a number a reader should take as a fact
//! is written in a cell instead.

use std::collections::BTreeSet;

use crate::cells::{CELL_ATTRS, Cell};
use crate::html::{Value, attr, attributes, element_end, line_of, strip_entities, tags, tokens};

/// The prefix of a link to a file of this repository at `main`, and of one to a directory.
pub const REPO_BLOB: &str = "https://github.com/komira-ai/hostkeeper/blob/main/";
pub const REPO_TREE: &str = "https://github.com/komira-ai/hostkeeper/tree/main/";
/// Links into this repository that name some ref start with one of these.
const REPO_REFS: &[&str] = &[
    "https://github.com/komira-ai/hostkeeper/blob/",
    "https://github.com/komira-ai/hostkeeper/tree/",
];

/// The attributes whose values are not prose: structure, styling hooks, link targets (the link
/// rule reads `href`) and SVG geometry. Every other attribute (`title`, `alt`, `aria-label`,
/// `content`, `placeholder`, and any not listed) is prose, so a number in it is refused.
pub const NON_PROSE: &[&str] = &[
    "href",
    "src",
    "id",
    "class",
    "lang",
    "charset",
    "name",
    "rel",
    "role",
    "scope",
    "colspan",
    "rowspan",
    "headers",
    "for",
    "open",
    "hidden",
    "xmlns",
    "viewBox",
    "preserveAspectRatio",
    "d",
    "points",
    "transform",
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "width",
    "height",
    "dx",
    "dy",
    "refX",
    "refY",
    "markerWidth",
    "markerHeight",
    "markerUnits",
    "orient",
    "offset",
    "text-anchor",
    "dominant-baseline",
    "font-size",
    "font-weight",
    "stroke-width",
    "stroke-dasharray",
    "stroke-linecap",
    "stroke-linejoin",
    "marker-start",
    "marker-end",
    "fill",
    "stroke",
    "opacity",
    "fill-opacity",
    "stroke-opacity",
    "aria-hidden",
    "aria-labelledby",
    "aria-describedby",
    "focusable",
    "data-figure",
    "data-diagram",
    "data-ui-page",
];

/// `bytes[from..to]` made spaces, its newlines kept.
pub fn blank(bytes: &mut [u8], from: usize, to: usize) {
    for b in &mut bytes[from..to] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

/// `text` with the contents of every element named in `names` blanked: from the end of its start
/// tag to the start of its end tag. An element never closed is left as it is.
fn mask_elements(text: &str, names: &[&str]) -> String {
    let all = tags(text);
    let mut bytes = text.as_bytes().to_vec();
    let mut covered = 0;
    for (k, tag) in all.iter().enumerate() {
        let wanted = names.iter().any(|n| tag.name.eq_ignore_ascii_case(n));
        if tag.closing || !tag.open || !wanted || tag.start < covered {
            continue;
        }
        if let Some(end) = element_end(&all, k) {
            blank(&mut bytes, tag.end, end);
            covered = end;
        }
    }
    String::from_utf8(bytes).expect("blanking whole ASCII-delimited ranges keeps UTF-8")
}

/// `text` with every HTML comment blanked; a `<!--` with no `-->` after it is text.
fn mask_comments(text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();
    let mut from = 0;
    while let Some(at) = text[from..].find("<!--").map(|at| at + from) {
        match text[at + 4..].find("-->") {
            Some(len) => {
                let end = at + 4 + len + 3;
                blank(&mut bytes, at, end);
                from = end;
            }
            None => break,
        }
    }
    String::from_utf8(bytes).expect("blanking whole ASCII-delimited ranges keeps UTF-8")
}

/// `text` with its `<style>` and `<script>` contents and its comments blanked: what is not text
/// of the page at all.
pub fn mask_raw(text: &str) -> String {
    mask_comments(&mask_elements(text, &["style", "script"]))
}

/// `text` as the cell and number rules read it: [`mask_raw`], then the contents of every `<code>`
/// and `<pre>` blanked.
pub fn mask_code(text: &str) -> String {
    mask_elements(&mask_raw(text), &["code", "pre"])
}

/// Whether attribute `name` is prose.
fn is_prose(name: &str) -> bool {
    !NON_PROSE.iter().any(|n| n.eq_ignore_ascii_case(name))
        && !CELL_ATTRS.iter().any(|c| c.eq_ignore_ascii_case(name))
}

/// One line per number printed in the prose of `masked`: outside every cell and every tag, and in
/// the value of every attribute that is prose ([`NON_PROSE`]) of a tag outside a cell.
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
                if let (true, Value::Double(value)) = (is_prose(a.name), a.value) {
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
pub const ENTITIES: &[&str] = &[
    "amp", "lt", "gt", "quot", "apos", "nbsp", "mdash", "ndash", "rsquo", "lsquo", "rdquo",
    "ldquo", "hellip", "middot", "rarr", "larr", "uarr", "darr", "times", "dagger",
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

/// The `id` values of `masked`, each with its line, in order.
fn ids(masked: &str) -> Vec<(&str, usize)> {
    tags(masked)
        .into_iter()
        .filter(|t| !t.closing)
        .filter_map(|t| attr(t.attrs, "id").map(|id| (id, line_of(masked, t.start))))
        .collect()
}

/// One line per `id` used a second time.
pub fn id_problems(masked: &str) -> Vec<String> {
    let mut seen: Vec<(&str, usize)> = Vec::new();
    let mut found = Vec::new();
    for (id, line) in ids(masked) {
        match seen.iter().find(|(s, _)| *s == id) {
            Some((_, first)) => found.push(format!(
                "line {line}: the id {id} is used again (first on line {first})"
            )),
            None => seen.push((id, line)),
        }
    }
    found
}

/// The `href` values of `masked`, each with its line, in order.
pub fn hrefs(masked: &str) -> Vec<(&str, usize)> {
    tags(masked)
        .into_iter()
        .filter(|t| !t.closing)
        .filter_map(|t| attr(t.attrs, "href").map(|h| (h, line_of(masked, t.start))))
        .collect()
}

/// The `href` of every `<a>` inside the element with `id="sources"`: the pages the report cites.
/// Empty when no element has that id or it is never closed.
pub fn sources(masked: &str) -> Vec<String> {
    let all = tags(masked);
    let Some(k) = all
        .iter()
        .position(|t| !t.closing && attr(t.attrs, "id") == Some("sources"))
    else {
        return Vec::new();
    };
    let Some(end) = element_end(&all, k) else {
        return Vec::new();
    };
    all[k + 1..]
        .iter()
        .take_while(|t| t.start < end)
        .filter(|t| !t.closing && t.name.eq_ignore_ascii_case("a"))
        .filter_map(|t| attr(t.attrs, "href").map(str::to_owned))
        .collect()
}

/// Whether `path` is a plain relative path: no empty, `.` or `..` part and no `\`.
fn plain_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// One line per link of `masked` that leads nowhere: an anchor `#x` that no `id` of the page has;
/// a link to this repository at `main` ([`REPO_BLOB`], [`REPO_TREE`]) whose path is not plain or
/// does not exist (`exists`, given a path from the repository root), or whose fragment is not a
/// heading of the Markdown file it names (`read` reads it) or is on a file that is not Markdown
/// (a line of a source file moves with every edit); a link into this repository at a ref other
/// than `main`; and any link that is neither an `https:` or `mailto:` URL nor an anchor, since a
/// relative link breaks wherever the page is served. Other URLs are not checked.
pub fn link_problems(
    masked: &str,
    exists: &dyn Fn(&str) -> bool,
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let own: BTreeSet<&str> = ids(masked).into_iter().map(|(id, _)| id).collect();
    let mut found = Vec::new();
    for (target, line) in hrefs(masked) {
        if let Some(fragment) = target.strip_prefix('#') {
            if !own.contains(fragment) {
                found.push(format!(
                    "line {line}: the link {target} names no id of the page"
                ));
            }
            continue;
        }
        let in_repo = target
            .strip_prefix(REPO_BLOB)
            .or_else(|| target.strip_prefix(REPO_TREE));
        let Some(rest) = in_repo else {
            if REPO_REFS.iter().any(|p| target.starts_with(p)) {
                found.push(format!(
                    "line {line}: the link {target} names a ref other than main"
                ));
            } else if !(target.starts_with("https://") || target.starts_with("mailto:")) {
                let why = if target.contains(':') {
                    "only https and mailto links are allowed"
                } else {
                    "a relative link breaks where the page is served, so link the repository URL"
                };
                found.push(format!(
                    "line {line}: the link {target} is not an https URL; {why}"
                ));
            }
            continue;
        };
        let (path, fragment) = match rest.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment)),
            None => (rest, None),
        };
        let path = path.trim_end_matches('/');
        if !plain_path(path) {
            found.push(format!(
                "line {line}: the link {target} is not a plain path in the repository"
            ));
            continue;
        }
        if !exists(path) {
            found.push(format!("line {line}: the link {target} leads to no file"));
            continue;
        }
        let Some(fragment) = fragment else {
            continue;
        };
        if !path.ends_with(".md") {
            found.push(format!(
                "line {line}: the link {target} points at a line of a source file; name the item instead"
            ));
            continue;
        }
        let known = read(path)
            .as_deref()
            .map(crate::markdown::anchors)
            .unwrap_or_default();
        if !known.iter().any(|a| a == fragment) {
            found.push(format!(
                "line {line}: the link {target} names no heading of {path}"
            ));
        }
    }
    found
}
