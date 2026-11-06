//! The committed report against the committed results of the rollout to the other Linux nodes of
//! the lab after the canary: the two cells the rollout fills, each row printed by exactly the
//! cells it names, and each derived row (the least and the greatest time, the number of nodes)
//! equal to what the per-node rows give.

use std::collections::{BTreeMap, BTreeSet};

use crate::canary_tests::{cell, edit_cell, report_cells, rows_are_printed_by_exactly_their_cells};
use crate::cells::Kind;
use crate::{RESULTS, check_report_text, repo_root};

/// The result file of the rollout.
pub const ROLLOUT: &str = "farm-rollout-linux.tsv";

/// The cells the rollout fills, in the report's order.
pub const ROLLOUT_CELLS: &[&str] = &["m-rollout-bootstrap", "m-rollout-approve-to-ready"];

/// The Linux nodes of the rollout, as the report names them: the lab's six but node-5, the canary,
/// whose times are in its own file.
const NODES: &[&str] = &["node-1", "node-2", "node-3", "node-4", "node-6"];

fn rollout_text() -> String {
    std::fs::read_to_string(repo_root().join(RESULTS).join(ROLLOUT)).expect("read the rollout")
}

/// `value` to one decimal, as the derived rows print it.
fn tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// One line per derived row of the result file `text` that its raw rows do not give.
///
/// Every value is a finite number. A raw row `<measure>-node-<k>` is one node's time, `<k>` a
/// node of [`NODES`], once per measure; any other raw row names a cell that prints it, so no raw
/// row is left out of both checks. A derived row `<measure>-min` or `<measure>-max` is the least
/// or the greatest of the raw rows of its measure, to one decimal; a measure with such a row has
/// at least one raw row, and every measure has both. The derived row `nodes` is there, and is the
/// number of raw rows of every measure, and every measure covers the same nodes. Any other derived
/// row is refused, so a derived row is never left unchecked.
fn derived_problems(text: &str) -> Vec<String> {
    let mut raw: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    let mut derived: Vec<(String, f64)> = Vec::new();
    let mut problems = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let columns: Vec<&str> = line.split('\t').collect();
        let (Some(kind), Some(id), Some(value)) = (columns.first(), columns.get(1), columns.get(2))
        else {
            problems.push(format!("{line:?}: fewer than 3 columns"));
            continue;
        };
        let Some(value) = value.parse::<f64>().ok().filter(|v| v.is_finite()) else {
            problems.push(format!("{id}: value {value:?} is not a finite number"));
            continue;
        };
        match *kind {
            "raw" => {
                if let Some((measure, node)) = id.split_once("-node-") {
                    let node = format!("node-{node}");
                    if !NODES.contains(&node.as_str()) {
                        problems.push(format!("{id}: {node} is not a node of the rollout"));
                    }
                    let by_node = raw.entry(measure.to_owned()).or_default();
                    if by_node.insert(node, value).is_some() {
                        problems.push(format!("{id}: a second row of the same node"));
                    }
                } else if columns.get(5).is_none_or(|c| c.trim().is_empty()) {
                    problems.push(format!(
                        "{id}: a raw row that is no node's time and names no cell"
                    ));
                }
            }
            "derived" => derived.push(((*id).to_owned(), value)),
            other => problems.push(format!("{id}: kind {other:?} is neither raw nor derived")),
        }
    }
    let node_sets: BTreeMap<&String, BTreeSet<&String>> = raw
        .iter()
        .map(|(m, by_node)| (m, by_node.keys().collect()))
        .collect();
    let distinct: BTreeSet<&BTreeSet<&String>> = node_sets.values().collect();
    if distinct.len() > 1 {
        let listed: Vec<String> = node_sets
            .iter()
            .map(|(m, nodes)| {
                let nodes: Vec<&str> = nodes.iter().map(|n| n.as_str()).collect();
                format!("{m}: {}", nodes.join(" "))
            })
            .collect();
        problems.push(format!(
            "the measures cover different nodes ({})",
            listed.join("; ")
        ));
    }
    if !derived.iter().any(|(id, _)| id == "nodes") {
        problems.push("no derived row nodes".to_owned());
    }
    for measure in raw.keys() {
        for end in ["min", "max"] {
            if !derived
                .iter()
                .any(|(id, _)| *id == format!("{measure}-{end}"))
            {
                problems.push(format!("{measure}: no derived row {measure}-{end}"));
            }
        }
    }
    for (id, value) in &derived {
        if id == "nodes" {
            for (measure, by_node) in &raw {
                let count = by_node.len() as f64;
                if (count - value).abs() > 1e-9 {
                    problems.push(format!(
                        "nodes is {value}, {measure} has {count} per-node rows"
                    ));
                }
            }
            continue;
        }
        let (measure, least) = match (id.strip_suffix("-min"), id.strip_suffix("-max")) {
            (Some(m), _) => (m, true),
            (_, Some(m)) => (m, false),
            _ => {
                problems.push(format!("{id}: a derived row that is no min, max or nodes"));
                continue;
            }
        };
        let Some(by_node) = raw.get(measure) else {
            problems.push(format!("{id}: no {measure}-node-* row"));
            continue;
        };
        let values = by_node.values().copied();
        let (word, extreme) = if least {
            ("least", values.fold(f64::INFINITY, f64::min))
        } else {
            ("greatest", values.fold(f64::NEG_INFINITY, f64::max))
        };
        if (tenth(extreme) - value).abs() > 1e-9 {
            problems.push(format!(
                "{id} is {value}, the {word} {measure}-node-* is {extreme}"
            ));
        }
    }
    problems
}

/// The committed rollout with the value of row `id` set to `to`.
fn with_value(id: &str, to: &str) -> String {
    rollout_text()
        .lines()
        .map(|line| {
            let mut columns: Vec<&str> = line.split('\t').collect();
            if !line.starts_with('#') && columns.get(1) == Some(&id) {
                columns[2] = to;
            }
            format!("{}\n", columns.join("\t"))
        })
        .collect()
}

#[test]
fn the_rollout_fills_its_two_cells() {
    // Catches: a rollout cell left open, filled from the canary's file, or dropped.
    let found = report_cells();
    let measured: Vec<&str> = found
        .iter()
        .filter(|c| c.kind == Kind::Result(ROLLOUT.to_owned()))
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(measured, ROLLOUT_CELLS);
}

#[test]
fn every_row_of_the_rollout_is_printed_by_exactly_the_cells_it_names() {
    // Catches: a printed range or count whose row does not name its cell, a per-node row a cell
    // prints, and a row changed under the report that does not fail the cell printing it.
    rows_are_printed_by_exactly_their_cells(ROLLOUT);
}

#[test]
fn the_derived_rows_of_the_committed_rollout_are_what_its_node_rows_give() {
    // Catches: a least or greatest time, or the node count, that the committed per-node rows do
    // not give.
    let text = rollout_text();
    assert_eq!(derived_problems(&text), Vec::<String>::new());
    let raw = text.lines().filter(|l| l.starts_with("raw\t")).count();
    let derived = text.lines().filter(|l| l.starts_with("derived\t")).count();
    assert_eq!((raw, derived), (11, 5));
}

#[test]
fn a_derived_row_its_node_rows_do_not_give_fails() {
    // Catches: a node's time changed without the range, the range changed without a node's
    // time, a node dropped from one measure without the count, a node outside the rollout (one
    // outside the lab, and node-5, the canary), a node given twice, a raw row that is neither a
    // node's time nor printed, a value that is not a finite number, a missing count, min or max,
    // a derived row of a measure with no node rows, a derived row of no known shape, an unknown
    // kind, and a row of too few columns.
    let text = rollout_text();
    let dropped: String = text
        .lines()
        .filter(|l| !l.starts_with("raw\tbootstrap-apply-node-1\t"))
        .map(|l| format!("{l}\n"))
        .collect();
    let cases = [
        (
            with_value("bootstrap-apply-node-4", "21.0"),
            vec!["bootstrap-apply-min is 22.7, the least bootstrap-apply-node-* is 21".to_owned()],
        ),
        (
            with_value("approve-to-ready-node-3", "30.01"),
            vec![
                "approve-to-ready-max is 29.2, the greatest approve-to-ready-node-* is 30.01"
                    .to_owned(),
            ],
        ),
        (
            with_value("approve-to-ready-min", "27.3"),
            vec![
                "approve-to-ready-min is 27.3, the least approve-to-ready-node-* is 27.24"
                    .to_owned(),
            ],
        ),
        (
            dropped,
            vec![
                "the measures cover different nodes (approve-to-ready: node-1 node-2 node-3 node-4 node-6; bootstrap-apply: node-2 node-3 node-4 node-6)"
                    .to_owned(),
                "nodes is 5, bootstrap-apply has 4 per-node rows".to_owned(),
            ],
        ),
        (
            text.replace("bootstrap-apply-node-6", "bootstrap-apply-node-7"),
            vec![
                "bootstrap-apply-node-7: node-7 is not a node of the rollout".to_owned(),
                "the measures cover different nodes (approve-to-ready: node-1 node-2 node-3 node-4 node-6; bootstrap-apply: node-1 node-2 node-3 node-4 node-7)"
                    .to_owned(),
            ],
        ),
        (
            text.replace("bootstrap-apply-node-6", "bootstrap-apply-node-5"),
            vec![
                "bootstrap-apply-node-5: node-5 is not a node of the rollout".to_owned(),
                "the measures cover different nodes (approve-to-ready: node-1 node-2 node-3 node-4 node-6; bootstrap-apply: node-1 node-2 node-3 node-4 node-5)"
                    .to_owned(),
            ],
        ),
        (
            text.replace("bootstrap-apply-node-6", "bootstrap-apply-node-4"),
            vec![
                "bootstrap-apply-node-4: a second row of the same node".to_owned(),
                "the measures cover different nodes (approve-to-ready: node-1 node-2 node-3 node-4 node-6; bootstrap-apply: node-1 node-2 node-3 node-4)"
                    .to_owned(),
                "nodes is 5, bootstrap-apply has 4 per-node rows".to_owned(),
                // The second row replaced node-4's time, so the least is now node-3's.
                "bootstrap-apply-min is 22.7, the least bootstrap-apply-node-* is 23.925"
                    .to_owned(),
            ],
        ),
        (
            format!("{text}raw\tbootstrap-apply-nod-7\t30\ts\tm\n"),
            vec!["bootstrap-apply-nod-7: a raw row that is no node's time and names no cell".to_owned()],
        ),
        (
            format!("{text}raw\tx\t30\ts\tm\t\n"),
            vec!["x: a raw row that is no node's time and names no cell".to_owned()],
        ),
        (
            with_value("bootstrap-apply-node-1", "NaN"),
            vec![
                "bootstrap-apply-node-1: value \"NaN\" is not a finite number".to_owned(),
                // The refused row is left out, so node-1 is missing from the measure.
                "the measures cover different nodes (approve-to-ready: node-1 node-2 node-3 node-4 node-6; bootstrap-apply: node-2 node-3 node-4 node-6)"
                    .to_owned(),
                "nodes is 5, bootstrap-apply has 4 per-node rows".to_owned(),
            ],
        ),
        (
            with_value("bootstrap-apply-node-1", "inf"),
            vec![
                "bootstrap-apply-node-1: value \"inf\" is not a finite number".to_owned(),
                // The refused row is left out, so node-1 is missing from the measure.
                "the measures cover different nodes (approve-to-ready: node-1 node-2 node-3 node-4 node-6; bootstrap-apply: node-2 node-3 node-4 node-6)"
                    .to_owned(),
                "nodes is 5, bootstrap-apply has 4 per-node rows".to_owned(),
            ],
        ),
        (
            text.lines()
                .filter(|l| !l.starts_with("derived\tnodes\t"))
                .map(|l| format!("{l}\n"))
                .collect(),
            vec!["no derived row nodes".to_owned()],
        ),
        (
            text.lines()
                .filter(|l| !l.starts_with("derived\tapprove-to-ready-max\t"))
                .map(|l| format!("{l}\n"))
                .collect(),
            vec!["approve-to-ready: no derived row approve-to-ready-max".to_owned()],
        ),
        (
            format!("{text}derived\tenroll-max\t1\ts\tm\n"),
            vec!["enroll-max: no enroll-node-* row".to_owned()],
        ),
        (
            format!("{text}derived\tbootstrap-apply-mean\t24.4\ts\tm\n"),
            vec!["bootstrap-apply-mean: a derived row that is no min, max or nodes".to_owned()],
        ),
        (
            format!("{text}computed\tx\t1\ts\tm\n"),
            vec!["x: kind \"computed\" is neither raw nor derived".to_owned()],
        ),
        (
            format!("{text}raw\tx\tfast\ts\tm\n"),
            vec!["x: value \"fast\" is not a finite number".to_owned()],
        ),
        (
            format!("{text}raw\tx\n"),
            vec!["\"raw\\tx\": fewer than 3 columns".to_owned()],
        ),
    ];
    for (changed, expected) in cases {
        assert_eq!(derived_problems(&changed), expected);
    }
}

#[test]
fn a_node_time_inside_the_printed_tenth_passes_and_one_outside_fails() {
    // Catches: a derivation that compares the printed tenth with the exact time (refusing 25.64,
    // which prints as 25.6), or that truncates (accepting 25.66, which prints as 25.7).
    assert_eq!(
        derived_problems(&with_value("bootstrap-apply-node-6", "25.64")),
        Vec::<String>::new()
    );
    assert_eq!(
        derived_problems(&with_value("bootstrap-apply-node-6", "25.66")),
        vec![
            "bootstrap-apply-max is 25.6, the greatest bootstrap-apply-node-* is 25.66".to_owned()
        ]
    );
}

#[test]
fn a_rollout_cell_that_disagrees_with_its_rows_fails() {
    // Catches, on the committed report: a printed bound edited without its row, a node count
    // edited without its row, a rollout number printed from the canary's file, and the poll
    // interval printed in another unit than its row's.
    let found = report_cells();
    let results = repo_root().join(RESULTS);
    let line = |id: &str| cell(&found, id).line;
    let boot = "m-rollout-bootstrap";
    let ready = "m-rollout-approve-to-ready";
    let cases = [
        (
            edit_cell(&found, boot, "25.6 s over", "25.7 s over"),
            vec![format!(
                "line {}: {boot}: the number 25.7 is not a value of {ROLLOUT}",
                line(boot)
            )],
        ),
        (
            edit_cell(&found, ready, "over 5 nodes", "over 6 nodes"),
            vec![format!(
                "line {}: {ready}: the number 6 is not a value of {ROLLOUT}",
                line(ready)
            )],
        ),
        (
            edit_cell(
                &found,
                boot,
                "\"farm-rollout-linux.tsv\"",
                "\"farm-canary-node-5.tsv\"",
            ),
            [22.7, 25.6, 5.0]
                .iter()
                .map(|n| {
                    format!(
                        "line {}: {boot}: the number {n} is not a value of farm-canary-node-5.tsv",
                        line(boot)
                    )
                })
                .collect(),
        ),
        (
            edit_cell(&found, ready, "every 3 s", "every 3 ms"),
            vec![format!(
                "line {}: {ready}: the number 3 is printed in ms, its row approve-to-ready-poll is in s",
                line(ready)
            )],
        ),
    ];
    for (text, expected) in cases {
        let (_, problems) = check_report_text(&text, &results);
        assert_eq!(problems, expected);
    }
}
