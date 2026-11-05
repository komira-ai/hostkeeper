//! The cell check of the technical report, `docs/report/hostkeeper.md`.
//!
//! The report states numbers only in named cells, and this test holds every cell to its source
//! (the kinds and their rules are in [`cells`]):
//!
//! - a measured cell (`data-result`) prints only values of the rows of its result file in
//!   `docs/report/results/` that name it, each in its row's unit;
//! - a cell of the code (`data-source` and `data-item`) prints one number that, in the unit
//!   word after it, is the value of the `const` or `static` it names, so a constant changed in
//!   the code fails here until the report follows;
//! - an open cell (`data-open`) prints `open` and no digit: a measurement not taken is never
//!   written as a number. The open cells are allowed, and `the_open_cells_are_measurements` prints
//!   them (nextest shows its output on success, `.config/nextest.toml`);
//! - every measurement the report promises ([`MEASUREMENTS`]) is a cell, measured or open, and
//!   every open cell is one of them; at least one cell reads the code;
//! - no number stands in the prose outside a cell, link text and attributes included (code is
//!   not prose: see [`markdown`]), and no number is written in other than ASCII digits;
//! - every relative link leads to a file or directory of the repository, an anchor into a
//!   Markdown file names one of its headings, and no link points at a line of a source file (a line number moves with every
//!   edit; the report names the item instead);
//! - the report has at most three diagrams; every `Service.Method` a diagram names is a method of
//!   a service of the protos, and the update diagram sends every update step of the hook
//!   contract (`Step::ALL` in `crates/hostkeeper-hook/src/step.rs`) as a message of its own.
//!
//! Each rule is shown refusing its defect on a small text in `check_tests.rs`.
//!
//! Catches: a number edited in the report without its source; a constant changed in the code
//! without the report; a measurement filled with a number no result holds; a promised
//! measurement dropped; a citation to a file or heading that was moved; a diagram naming a
//! method that does not exist, or an update diagram that lost a step of the contract.

mod cells;
mod check_tests;
mod escape_tests;
mod html;
mod markdown;
mod rust_value;
#[path = "../../src/testdir.rs"]
mod testdir;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cells::{
    Cell, cells, open_list, open_problems, required_problems, result_problems, source_problems,
    source_rule_problems, stray_open_problems,
};
use markdown::{anchors, blocks, glyph_problems, link_targets, loose_numbers, mask_code};

/// The report, as a path from the repository root.
const REPORT: &str = "docs/report/hostkeeper.md";
/// The directory of its result files.
const RESULTS: &str = "docs/report/results";
/// The most diagrams the report may hold.
const MAX_DIAGRAMS: usize = 3;
/// The languages of a fenced block that is a diagram.
const DIAGRAM_FENCES: &[&str] = &["mermaid", "dot", "graphviz", "plantuml", "svg"];
/// The directory whose `.proto` files define the services a diagram may name.
const PROTOS: &str = "crates/hostkeeper-proto";
/// The hook contract's steps, which the update diagram names.
const STEPS: &str = "crates/hostkeeper-hook/src/step.rs";
/// The line (a Mermaid comment) that marks the update diagram.
const UPDATE_MARK: &str = "%% update";

/// The measurements the report promises: each is a cell, filled from a result or open.
const MEASUREMENTS: &[&str] = &[
    "m-enroll",
    "m-converge-install",
    "m-converge-update",
    "m-converge-rollback",
    "m-tierstore-force-first",
    "m-coordinator-cpu",
    "m-coordinator-memory",
    "m-daemon-rss",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `target` resolved against the directory of `from`, with `.` and `..` applied; `None` when it
/// climbs above the root.
fn resolve(from: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// One line per relative link of `masked` (the report at `path`, code blanked out) that leads to
/// no file or directory (`exists`, given a path from the repository root), names an anchor no
/// heading of its Markdown file has, or points at a line of a file that is not Markdown. `own`
/// is the report's own text, for its anchors; `read` reads a Markdown file for its anchors.
fn link_problems(
    path: &str,
    masked: &str,
    own: &str,
    exists: &dyn Fn(&str) -> bool,
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut found = Vec::new();
    for target in link_targets(masked) {
        if target.contains(':') {
            continue;
        }
        let (file, fragment) = match target.split_once('#') {
            Some((file, fragment)) => (file, Some(fragment)),
            None => (target, None),
        };
        let resolved = if file.is_empty() {
            path.to_owned()
        } else {
            match resolve(path, file) {
                Some(resolved) => resolved,
                None => {
                    found.push(format!("the link {target} climbs out of the repository"));
                    continue;
                }
            }
        };
        if !exists(&resolved) {
            found.push(format!("the link {target} leads to no file"));
            continue;
        }
        let Some(fragment) = fragment else {
            continue;
        };
        if !resolved.ends_with(".md") {
            found.push(format!(
                "the link {target} points at a line of a source file; name the item instead"
            ));
            continue;
        }
        let text = if resolved == path {
            Some(own.to_owned())
        } else {
            read(&resolved)
        };
        let known = text.as_deref().map(anchors).unwrap_or_default();
        if !known.iter().any(|a| a == fragment) {
            found.push(format!("the link {target} names no heading of {resolved}"));
        }
    }
    found
}

/// The `Service.Method` pairs of the `.proto` texts: a `service <Name> {` line opens a service,
/// and each `rpc <Method>(` inside it adds a pair.
fn rpcs<'a>(protos: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
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

/// The `Service.Method` names a diagram's text holds: two identifiers each starting with an
/// upper-case letter, joined by a point, not glued to a word character or a point before it nor
/// followed by a point and a word character (so `X.Y.Z` is none; a sentence's full stop after
/// one is not read as part of it).
fn method_names(text: &str) -> Vec<&str> {
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

/// The words of the update steps of the hook contract, in order, read from the source of
/// `hostkeeper_hook::Step`: the variants listed in `Step::ALL`, each spelled as `Step::name`
/// spells it (`Step::Prepare => "prepare"`). Empty when the source has neither.
fn step_words(source: &str) -> Vec<String> {
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

/// One line per diagram over [`MAX_DIAGRAMS`] (a `mermaid` block or an inline `<svg`), per
/// `Service.Method` in a mermaid block that no service of `rpcs` has, and per step of `steps`
/// the update diagram (the first mermaid block with a line that is [`UPDATE_MARK`], a Mermaid
/// comment) does not send as a message of its own (a line `A->>B: <step>`); one line when there
/// is no update diagram. A diagram is a fenced block of a language of [`DIAGRAM_FENCES`], or an
/// `<svg`, `<img`, `<picture` or `![` image outside code.
fn diagram_problems(
    text: &str,
    masked: &str,
    rpcs: &BTreeSet<String>,
    steps: &[String],
) -> Vec<String> {
    let all = blocks(text);
    let mermaid: Vec<_> = all.iter().filter(|b| b.info == "mermaid").collect();
    let fenced = all
        .iter()
        .filter(|b| DIAGRAM_FENCES.contains(&b.info))
        .count();
    let pictures = ["<svg", "<img", "![", "<picture"]
        .iter()
        .map(|p| masked.matches(p).count())
        .sum::<usize>();
    let mut found = Vec::new();
    if fenced + pictures > MAX_DIAGRAMS {
        found.push(format!(
            "{} diagrams, more than {MAX_DIAGRAMS}",
            fenced + pictures
        ));
    }
    for block in &mermaid {
        for name in method_names(block.body) {
            if !rpcs.contains(name) {
                found.push(format!(
                    "the diagram on line {}: {name} is no method of the protos",
                    block.line
                ));
            }
        }
    }
    let update = mermaid
        .iter()
        .find(|b| b.body.lines().any(|line| line.trim() == UPDATE_MARK));
    match update {
        None => found.push(format!("no mermaid diagram has the line {UPDATE_MARK:?}")),
        Some(block) => {
            let messages: BTreeSet<&str> = block
                .body
                .lines()
                .filter(|line| line.contains("->"))
                .filter_map(|line| line.split_once(':').map(|(_, said)| said.trim()))
                .collect();
            for step in steps {
                if !messages.contains(step.as_str()) {
                    found.push(format!(
                        "the update diagram on line {} does not name the step {step}",
                        block.line
                    ));
                }
            }
        }
    }
    found
}

/// What the check reads besides the report itself.
struct World<'a> {
    root: &'a Path,
    results: &'a Path,
    exists: &'a dyn Fn(&str) -> bool,
    rpcs: &'a BTreeSet<String>,
    steps: &'a [String],
    read: &'a dyn Fn(&str) -> Option<String>,
}

/// Every problem of the report `text` at `path`, and its cells.
fn check(path: &str, text: &str, world: &World<'_>) -> (Vec<Cell>, Vec<String>) {
    let masked = mask_code(text);
    let (found, mut problems) = cells(&masked);
    problems.extend(result_problems(&found, world.results));
    problems.extend(source_problems(&found, world.root));
    problems.extend(open_problems(&found));
    problems.extend(required_problems(&found, MEASUREMENTS));
    problems.extend(loose_numbers(&masked, &found));
    problems.extend(link_problems(path, &masked, text, world.exists, world.read));
    problems.extend(diagram_problems(text, &masked, world.rpcs, world.steps));
    problems.extend(glyph_problems(&masked));
    problems.extend(stray_open_problems(&found, MEASUREMENTS));
    problems.extend(source_rule_problems(&found));
    (found, problems)
}

/// The `.proto` texts under `dir`, recursively.
fn proto_texts(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read a proto directory") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "proto") {
                out.push(std::fs::read_to_string(&path).expect("read a proto"));
            }
        }
    }
    out
}

/// The report checked against the repository.
fn check_report() -> (Vec<Cell>, Vec<String>) {
    let root = repo_root();
    let text = std::fs::read_to_string(root.join(REPORT)).expect("read the report");
    let protos = proto_texts(&root.join(PROTOS));
    let rpcs = rpcs(protos.iter().map(String::as_str));
    let steps = step_words(&std::fs::read_to_string(root.join(STEPS)).expect("read the steps"));
    assert!(!steps.is_empty(), "no update step read from {STEPS}");
    let exists_root = root.clone();
    let exists = move |p: &str| exists_root.join(p).exists();
    let read_root = root.clone();
    let read = move |p: &str| std::fs::read_to_string(read_root.join(p)).ok();
    let world = World {
        root: &root,
        results: &root.join(RESULTS),
        exists: &exists,
        rpcs: &rpcs,
        steps: &steps,
        read: &read,
    };
    check(REPORT, &text, &world)
}

#[test]
fn the_report_holds_to_every_rule() {
    // Catches: any number, cell, link or diagram of the report that its source no longer backs.
    let (found, problems) = check_report();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(!found.is_empty(), "the report has no cell");
}

#[test]
fn the_open_cells_are_measurements() {
    // Catches: a claim other than a measurement left open (an open cell is only ever a number
    // the farm run has not produced yet). Prints the open cells, which are allowed.
    let (found, _) = check_report();
    let open = open_list(&found);
    println!("{} open cells in {REPORT}:", open.len());
    for line in &open {
        println!("  {line}");
    }
    let stray = stray_open_problems(&found, MEASUREMENTS);
    assert!(stray.is_empty(), "{}", stray.join("\n"));
}
