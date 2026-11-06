//! The cell check of the technical report, `docs/report/hostkeeper.html`.
//!
//! The report states numbers only in named cells, and this test holds every cell to its source
//! (the kinds and their rules are in [`cells`]):
//!
//! - a measured cell (`data-result`) prints only values of the rows of its result file in
//!   `docs/report/results/` that name it, each in its row's unit;
//! - a cell of the code (`data-source` and `data-item`) prints one number that, in the unit
//!   word after it, is the value of the `const` or `static` it names, so a constant changed in
//!   the code fails here until the report follows;
//! - a cell of another tool's documentation (`data-cite`) names an `https:` page the report lists
//!   in its sources and prints a number; its value is not traced (the check has no network);
//! - an open cell (`data-open`) prints `open` and no digit: a measurement not taken is never
//!   written as a number. The open cells are allowed, and `the_open_cells_are_measurements` prints
//!   them (nextest shows its output on success, `.config/nextest.toml`);
//! - every measurement the report promises ([`MEASUREMENTS`]) is a cell, measured or open, and
//!   every open cell is one of them; at least one cell reads the code;
//! - no number stands in the prose outside a cell: text, and the value of every attribute that is
//!   not structure or geometry (`title`, `aria-label`; the list is [`page::NON_PROSE`]). Code is
//!   not prose (`<code>`, `<pre>`, `<style>`, `<script>` and comments: see [`page`]), and no
//!   number is written in other than ASCII digits;
//! - every link is an anchor of the page (an `id` it has), a link to a file or directory of this
//!   repository at `main` that exists (an anchor only into a Markdown file, naming one of its
//!   headings; a line of a source file moves with every edit, so the report names the item
//!   instead), or another `https:` or `mailto:` URL; a relative link is refused, since it breaks
//!   where the page is served. No `id` is used twice;
//! - the report has two to four problem figures; every `Service.Method` it names is a method of a
//!   service of the protos; the update diagram names every update step of the hook contract
//!   (`Step::ALL` in `crates/hostkeeper-hook/src/step.rs`) as a label of its own; and there is one
//!   mock-up for each page of the web UI (`ui/pages/*.js`), whose every UI string is a string
//!   literal of that page or of the shared modules (see [`figures`]).
//!
//! Each rule is shown refusing its defect on a small text in `check_tests.rs` and
//! `escape_tests.rs`, and on the committed report against the first farm canary's results in
//! `canary_tests.rs`.
//!
//! Catches: a number edited in the report without its source; a constant changed in the code
//! without the report; a measurement filled with a number no result holds; a promised
//! measurement dropped; a quotation of a page the sources do not list; a link to a file or
//! heading that was moved; a diagram naming a method that does not exist; an update diagram that
//! lost a step of the contract; a mock-up of a page, button or column the UI does not have.

mod canary_tests;
mod cells;
mod check_tests;
mod escape_tests;
mod figures;
mod html;
mod markdown;
mod page;
mod rust_value;
#[path = "../../src/testdir.rs"]
mod testdir;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cells::{
    Cell, cells, cite_problems, open_list, open_problems, required_problems, result_problems,
    source_problems, source_rule_problems, stray_open_problems,
};
use figures::{
    Ui, html_literals, js_literals, method_problems, problem_figure_problems, rpcs, step_words,
    ui_problems, update_problems,
};
use page::{
    glyph_problems, id_problems, link_problems, loose_numbers, mask_code, mask_raw, sources,
};

/// The report, as a path from the repository root.
const REPORT: &str = "docs/report/hostkeeper.html";
/// The directory of its result files.
const RESULTS: &str = "docs/report/results";
/// The directory whose `.proto` files define the services the report may name.
const PROTOS: &str = "crates/hostkeeper-proto";
/// The hook contract's steps, which the update diagram names.
const STEPS: &str = "crates/hostkeeper-hook/src/step.rs";
/// The page modules of the web UI, one per page, which the mock-ups draw.
const UI_PAGES: &str = "ui/pages";
/// The shared modules of the web UI whose strings any page may print.
const UI_SHARED_JS: &[&str] = &["ui/app.js", "ui/view.js"];
const UI_SHARED_HTML: &[&str] = &["ui/index.html"];

/// The measurements the report promises: each is a cell, filled from a result or open.
const MEASUREMENTS: &[&str] = &[
    "m-bootstrap",
    "m-enroll",
    "m-first-heartbeat",
    "m-converge-install",
    "m-converge-update",
    "m-converge-rollback",
    "m-tierstore-force-first",
    "m-coordinator-cpu",
    "m-coordinator-memory",
    "m-coordinator-memory-one-host",
    "m-daemon-rss",
    "m-channel-memory",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// What the check reads besides the report itself.
struct World<'a> {
    root: &'a Path,
    results: &'a Path,
    exists: &'a dyn Fn(&str) -> bool,
    rpcs: &'a BTreeSet<String>,
    steps: &'a [String],
    read: &'a dyn Fn(&str) -> Option<String>,
    ui: &'a Ui,
}

/// Every problem of the report `text`, and its cells.
fn check(text: &str, world: &World<'_>) -> (Vec<Cell>, Vec<String>) {
    let masked = mask_code(text);
    let (found, mut problems) = cells(&masked);
    problems.extend(result_problems(&found, world.results));
    problems.extend(source_problems(&found, world.root));
    problems.extend(open_problems(&found));
    problems.extend(required_problems(&found, MEASUREMENTS));
    problems.extend(cite_problems(&found, &sources(&masked)));
    problems.extend(loose_numbers(&masked, &found));
    problems.extend(link_problems(&masked, world.exists, world.read));
    problems.extend(id_problems(&masked));
    problems.extend(problem_figure_problems(&masked));
    problems.extend(method_problems(&mask_raw(text), world.rpcs));
    problems.extend(update_problems(&masked, world.steps));
    problems.extend(ui_problems(&masked, world.ui));
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

/// The strings of the web UI under `root`: the literals of each module of [`UI_PAGES`], by page
/// (its file stem), and of the shared files.
fn ui_texts(root: &Path) -> Ui {
    let mut pages = BTreeMap::new();
    for entry in std::fs::read_dir(root.join(UI_PAGES)).expect("read the UI pages") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|e| e == "js") {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("a UI page is named in UTF-8")
                .to_owned();
            let text = std::fs::read_to_string(&path).expect("read a UI page");
            pages.insert(stem, js_literals(&text));
        }
    }
    let read = |p: &&str| std::fs::read_to_string(root.join(p)).expect("read a shared UI file");
    let mut shared: Vec<_> = UI_SHARED_JS
        .iter()
        .map(read)
        .flat_map(|t| js_literals(&t))
        .collect();
    shared.extend(
        UI_SHARED_HTML
            .iter()
            .map(read)
            .flat_map(|t| html_literals(&t)),
    );
    Ui { pages, shared }
}

/// The report as committed.
fn report_text() -> String {
    std::fs::read_to_string(repo_root().join(REPORT)).expect("read the report")
}

/// The report checked against the repository.
fn check_report() -> (Vec<Cell>, Vec<String>) {
    check_report_text(&report_text(), &repo_root().join(RESULTS))
}

/// The report `text` checked against the repository, with its result files read from `results`.
fn check_report_text(text: &str, results: &Path) -> (Vec<Cell>, Vec<String>) {
    let root = repo_root();
    let protos = proto_texts(&root.join(PROTOS));
    let rpcs = rpcs(protos.iter().map(String::as_str));
    let steps = step_words(&std::fs::read_to_string(root.join(STEPS)).expect("read the steps"));
    assert!(!steps.is_empty(), "no update step read from {STEPS}");
    let exists_root = root.clone();
    let exists = move |p: &str| exists_root.join(p).exists();
    let read_root = root.clone();
    let read = move |p: &str| std::fs::read_to_string(read_root.join(p)).ok();
    let ui = ui_texts(&root);
    let world = World {
        root: &root,
        results,
        exists: &exists,
        rpcs: &rpcs,
        steps: &steps,
        read: &read,
        ui: &ui,
    };
    check(text, &world)
}

#[test]
fn the_report_holds_to_every_rule() {
    // Catches: any number, cell, link, figure or mock-up of the report that its source no longer
    // backs.
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
