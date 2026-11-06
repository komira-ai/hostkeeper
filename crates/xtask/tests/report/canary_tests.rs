//! The committed report against the committed results of the first farm canary: which cells the
//! canary fills and which stay open, and the check failing the real report when a cell and its
//! result file disagree, from either side.

use std::path::Path;

use crate::cells::{Cell, Kind, result_rows};
use crate::html::quantities;
use crate::testdir::TestDir;
use crate::{RESULTS, check_report, check_report_text, repo_root, report_text};

/// The result file of the first farm canary.
const CANARY: &str = "farm-canary-node-5.tsv";

/// The cells the canary fills, in the report's order.
const FILLED: &[&str] = &[
    "m-bootstrap",
    "m-enroll",
    "m-converge-install",
    "m-converge-update",
    "m-converge-rollback",
    "m-coordinator-memory-one-host",
    "m-daemon-rss",
    "m-channel-memory",
];

/// The measurements the canary could not take, in the report's order.
const STILL_OPEN: &[&str] = &[
    "m-first-heartbeat",
    "m-tierstore-force-first",
    "m-coordinator-cpu",
    "m-coordinator-memory",
];

fn canary_text() -> String {
    std::fs::read_to_string(repo_root().join(RESULTS).join(CANARY)).expect("read the canary")
}

/// The committed report's cells.
fn report_cells() -> Vec<Cell> {
    let (found, problems) = check_report();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    found
}

fn cell<'a>(found: &'a [Cell], id: &str) -> &'a Cell {
    found
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("the report has no cell {id}"))
}

/// The committed report with `from` replaced by `to` inside the element of cell `id` (once).
fn edit_cell(found: &[Cell], id: &str, from: &str, to: &str) -> String {
    let text = report_text();
    let (start, end) = cell(found, id).span;
    let element = &text[start..end];
    assert_eq!(element.matches(from).count(), 1, "{from:?} in {element}");
    format!(
        "{}{}{}",
        &text[..start],
        element.replacen(from, to, 1),
        &text[end..]
    )
}

fn scratch(name: &str) -> TestDir {
    TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
}

#[test]
fn the_canary_fills_its_cells_and_leaves_the_rest_open() {
    // Catches: a canary cell left open or filled from another file, and a measurement the
    // canary did not take (first heartbeat, the tierstore force, the six-node fleet) filled.
    let found = report_cells();
    let measured: Vec<&str> = found
        .iter()
        .filter(|c| c.kind == Kind::Result(CANARY.to_owned()))
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(measured, FILLED);
    let open: Vec<&str> = found
        .iter()
        .filter(|c| matches!(c.kind, Kind::Open(_)))
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(open, STILL_OPEN);
    let other_results: Vec<&str> = found
        .iter()
        .filter(|c| matches!(&c.kind, Kind::Result(name) if name != CANARY))
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(other_results, Vec::<&str>::new());
}

#[test]
fn every_row_of_the_canary_is_printed_by_exactly_the_cells_it_names() {
    // Catches: a row whose `cells` names a cell that does not print its value, or misses one
    // that does; then, for each row, its value changed under the report fails every cell that
    // prints it, and nothing else.
    let found = report_cells();
    let canary = canary_text();
    let rows = result_rows(&canary).expect("the canary parses");
    assert!(!rows.is_empty(), "the canary has no row");
    for row in &rows {
        let printing: Vec<&str> = found
            .iter()
            .filter(|c| c.kind == Kind::Result(CANARY.to_owned()))
            .filter(|c| {
                quantities(&c.text)
                    .iter()
                    .any(|(n, _)| (n - row.value).abs() < 1e-9)
            })
            .map(|c| c.id.as_str())
            .collect();
        let mut named: Vec<&str> = row.cells.iter().map(String::as_str).collect();
        named.sort_unstable();
        let mut printing_sorted = printing.clone();
        printing_sorted.sort_unstable();
        assert_eq!(printing_sorted, named, "row {}", row.id);

        let dir = scratch("report-canary-row");
        let changed: String = canary
            .lines()
            .map(|line| {
                let mut columns: Vec<&str> = line.split('\t').collect();
                if !line.starts_with('#') && columns.get(1) == Some(&row.id.as_str()) {
                    columns[2] = "9999";
                }
                format!("{}\n", columns.join("\t"))
            })
            .collect();
        std::fs::write(dir.join(CANARY), changed).expect("write");
        let (_, problems) = check_report_text(&report_text(), &dir);
        let expected: Vec<String> = found
            .iter()
            .filter(|c| row.cells.contains(&c.id))
            .flat_map(|c| {
                quantities(&c.text)
                    .into_iter()
                    .filter(|(n, _)| (n - row.value).abs() < 1e-9)
                    .map(|(n, _)| {
                        format!(
                            "line {}: {}: the number {n} is not a value of {CANARY}",
                            c.line, c.id
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(!expected.is_empty(), "row {} is printed nowhere", row.id);
        assert_eq!(problems, expected, "row {}", row.id);
    }
}

#[test]
fn a_report_cell_that_disagrees_with_the_canary_fails() {
    // Catches, on the committed report: a canary number edited without its row, a number printed
    // in another unit than its row's, a number taken from another cell's row, an open
    // measurement given a number, and a cell pointing at a result file that does not exist.
    let found = report_cells();
    let results = repo_root().join(RESULTS);
    let line = |id: &str| cell(&found, id).line;
    let cases = [
        (
            edit_cell(&found, "m-bootstrap", ">34 s", ">35 s"),
            format!(
                "line {}: m-bootstrap: the number 35 is not a value of {CANARY}",
                line("m-bootstrap")
            ),
        ),
        (
            edit_cell(&found, "m-daemon-rss", "15.4 MiB", "15.4 KiB"),
            format!(
                "line {}: m-daemon-rss: the number 15.4 is printed in KiB, its row daemon-rss is in MiB",
                line("m-daemon-rss")
            ),
        ),
        (
            edit_cell(
                &found,
                "m-coordinator-memory-one-host",
                "6.67 MiB",
                "15.4 MiB",
            ),
            format!(
                "line {}: m-coordinator-memory-one-host: the number 15.4 is not a value of {CANARY} for this cell; the rows holding it (daemon-rss) do not name m-coordinator-memory-one-host",
                line("m-coordinator-memory-one-host")
            ),
        ),
        (
            edit_cell(
                &found,
                "m-first-heartbeat",
                ">open: the coordinator does not record it yet<",
                ">open: 5 s<",
            ),
            format!(
                "line {}: m-first-heartbeat: an open cell prints a digit: \"open: 5 s\"",
                line("m-first-heartbeat")
            ),
        ),
        (
            edit_cell(
                &found,
                "m-enroll",
                "\"farm-canary-node-5.tsv\"",
                "\"farm-canary-node-6.tsv\"",
            ),
            format!(
                "line {}: m-enroll: result file farm-canary-node-6.tsv does not exist",
                line("m-enroll")
            ),
        ),
    ];
    for (text, expected) in cases {
        let (_, problems) = check_report_text(&text, &results);
        assert_eq!(problems, vec![expected]);
    }
}
