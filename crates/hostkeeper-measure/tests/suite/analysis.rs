//! The analysis and its output on records written by hand: which clock a time is read from,
//! what counts as missing, the statistics, and what the page and the file print for them.

use std::collections::BTreeMap;

use hostkeeper_measure::analysis::{self, AnalysisError, Clock, Value};
use hostkeeper_measure::record::{Record, RunInfo};
use hostkeeper_measure::render;

use crate::support;

/// A plan with every kind of measure, over steps that record what the records below need.
const PLAN: &str = r#"
id = "kinds"
title = "Every kind"
measures = "M."
records = "R."
repetitions = 3
states = [
    "first {first.min} {first.median} {first.max} n {first.n} missing {first.missing}; each {each.median}; none {none.median}; {plan} {run}",
]

[hosts]
says = "Hosts."
pick = "live"

[[step]]
id = "go"
do = "operator"
says = "Go."

[[step]]
id = "w"
do = "watch"
until = { what = "back" }
poll_secs = 1
timeout_secs = 1

[[measure]]
id = "first"
kind = "span"
says = "go to the first back"
unit = "s"
from = "go/done"
to = "w/back"
pick = "first"

[[measure]]
id = "last"
kind = "span"
says = "the first heartbeat to the last back"
unit = "s"
from = "w/heartbeat"
to = "w/back"
pick = "last"

[[measure]]
id = "each"
kind = "span"
says = "each host's heartbeat to its back"
unit = "s"
from = "w/heartbeat"
to = "w/back"
pick = "each"

[[measure]]
id = "fails"
kind = "count"
says = "failed polls"
unit = "requests"
of = "w/call-failed"

[[measure]]
id = "gap"
kind = "gap"
says = "the longest gap"
unit = "s"
of = "w/heartbeat"

[[measure]]
id = "none"
kind = "span"
says = "never recorded"
unit = "s"
from = "go/done"
to = "w/answered"
pick = "first"
"#;

fn rec(
    rep: u32,
    step: &str,
    event: &str,
    host: Option<&str>,
    runner_ms: u64,
    coordinator: Option<u64>,
) -> Record {
    Record {
        rep,
        step: step.to_owned(),
        event: event.to_owned(),
        host: host.map(str::to_owned),
        runner_ms,
        coordinator_unix: coordinator,
        detail: BTreeMap::new(),
    }
}

fn ended(rep: u32, why: Option<&str>) -> Record {
    let mut r = rec(rep, "repetition", "ended", None, 0, None);
    r.detail
        .insert("complete".to_owned(), why.is_none().to_string());
    if let Some(why) = why {
        r.detail.insert("why".to_owned(), why.to_owned());
    }
    r
}

fn info(plan: &str) -> RunInfo {
    RunInfo {
        plan: plan.to_owned(),
        run: "r1".to_owned(),
        repetitions: 3,
    }
}

/// Repetition 1: everything stamped by the coordinator. Repetition 2: a back seen only by the
/// runner, a host with one heartbeat, and a back of a host with no heartbeat. Repetition 3 did
/// not start.
fn records() -> Vec<Record> {
    vec![
        rec(1, "repetition", "started", None, 0, None),
        rec(1, "go", "done", None, 1_000, Some(100)),
        rec(1, "w", "heartbeat", Some("host-1"), 1_500, Some(101)),
        rec(1, "w", "heartbeat", Some("host-2"), 1_600, Some(102)),
        rec(1, "w", "call-failed", None, 2_000, None),
        rec(1, "w", "heartbeat", Some("host-1"), 9_000, Some(116)),
        rec(1, "w", "back", Some("host-2"), 9_500, Some(110)),
        rec(1, "w", "back", Some("host-1"), 9_000, Some(116)),
        ended(1, None),
        rec(2, "repetition", "started", None, 0, None),
        rec(2, "go", "done", None, 20_000, Some(200)),
        rec(2, "w", "heartbeat", Some("host-1"), 20_500, Some(201)),
        // A heartbeat of no host counts toward no host's gap.
        rec(2, "w", "heartbeat", None, 23_000, Some(203)),
        rec(2, "w", "back", Some("host-1"), 23_250, None),
        rec(2, "w", "back", Some("host-3"), 24_000, Some(205)),
        ended(2, Some("w timed out")),
    ]
}

#[test]
fn each_kind_of_measure_reads_one_clock_counts_what_is_missing_and_makes_up_nothing() {
    let plan = support::plan(PLAN);
    let a = analysis::analyze(&plan, &info("kinds"), &records()).expect("the analysis");
    assert_eq!((a.started, a.complete), (2, 1));
    assert_eq!(a.ended, vec![(2, "w timed out".to_owned())]);
    let by = |id: &str| {
        a.results
            .iter()
            .find(|m| m.measure.id == id)
            .expect("a measure")
    };
    let v = |rep, host: Option<&str>, value, clock| Value {
        rep,
        host: host.map(str::to_owned),
        value,
        clock,
    };
    // Both ends stamped: the coordinator. One end not: the runner, in milliseconds.
    assert_eq!(
        by("first").values,
        [
            v(1, None, 10.0, Some(Clock::Coordinator)),
            v(2, None, 3.25, Some(Clock::Runner))
        ]
    );
    assert_eq!(
        by("last").values,
        [
            v(1, None, 15.0, Some(Clock::Coordinator)),
            v(2, None, 3.5, Some(Clock::Runner))
        ]
    );
    // Each back from its own host's first heartbeat; host-3 has none, so it is missing.
    assert_eq!(
        by("each").values,
        [
            v(1, Some("host-2"), 8.0, Some(Clock::Coordinator)),
            v(1, Some("host-1"), 15.0, Some(Clock::Coordinator)),
            v(2, Some("host-1"), 2.75, Some(Clock::Runner)),
        ]
    );
    assert_eq!(by("each").missing, 1);
    // A count of none is a value: zero.
    assert_eq!(
        by("fails").values,
        [v(1, None, 1.0, None), v(2, None, 0.0, None)]
    );
    // The longest gap of each host with two heartbeats; repetition 2's one heartbeat gives none.
    assert_eq!(
        by("gap").values,
        [v(1, Some("host-1"), 15.0, Some(Clock::Coordinator))]
    );
    assert_eq!(by("gap").missing, 2);
    assert!(by("none").values.is_empty());
    assert_eq!(by("none").missing, 2);
    assert_eq!(by("none").stats(), None);
    assert_eq!(by("first").clocks(), [Clock::Coordinator, Clock::Runner]);
    let s = by("each").stats().expect("stats");
    assert_eq!((s.min, s.median, s.max), (2.75, 8.0, 15.0));
    let s = by("first").stats().expect("stats");
    assert_eq!((s.min, s.median, s.max), (3.25, 6.625, 10.0));
    assert_eq!(Clock::Runner.name(), "runner");
}

#[test]
fn the_page_and_the_file_print_every_value_with_its_plan_run_and_clock() {
    let plan = support::plan(PLAN);
    let a = analysis::analyze(&plan, &info("kinds"), &records()).expect("the analysis");
    assert_eq!(
        render::statement(&a, &plan.states[0]),
        "first 3.25 6.625 10 n 2 missing 0; each 8; none no value; kinds r1"
    );
    assert_eq!(
        render::statement(&a, "{nothing.max} {first.none}"),
        "{nothing.max} no value"
    );
    assert_eq!(render::statement(&a, "a {b"), "a {b");
    let md = render::markdown(&a);
    for want in [
        "## Every kind\n\nPlan [`kinds`](../../../crates/hostkeeper-measure/plans/kinds.toml), run `r1`: 2 of 3 repetitions started, 1 complete.\n\nM.\n\n**Hosts.** Hosts.\n\n**Recorded.** R.\n\n",
        "| `first` | go to the first back | s | 2 | 0 | 3.25 | 6.625 | 10 | coordinator, runner |\n",
        "| `fails` | failed polls | requests | 2 | 0 | 0 | 0.5 | 1 | - |\n",
        "| `none` | never recorded | s | 0 | 2 | - | - | - | - |\n",
        "**Repetitions that ended early.**\n\n- repetition 2: w timed out\n",
        "| `each` | 2 | host-1 | 2.75 | s | runner |\n",
        "| `fails` | 1 | - | 1 | requests | - |\n",
    ] {
        assert!(md.contains(want), "{want:?} not in\n{md}");
    }
    let tsv = render::tsv(&a);
    for want in [
        "# Plan kinds, run r1: Every kind. Written by `hostkeeper-measure analyze` from the run's records.\n",
        "\nraw\tfirst-rep-1\t10\ts\tplan kinds, run r1, repetition 1: go to the first back; coordinator clock\t\n",
        "\nraw\teach-rep-2-host-1\t2.75\ts\tplan kinds, run r1, repetition 2, host-1: each host's heartbeat to its back; runner clock\t\n",
        "\nraw\tfails-rep-2\t0\trequests\tplan kinds, run r1, repetition 2: failed polls\t\n",
        "\nderived\tfirst-n\t2\t\tplan kinds, run r1: the number of raw rows of first\t\n",
        "\nderived\tfirst-median\t6.625\ts\tplan kinds, run r1: the median raw value of first\t\n",
        "\nderived\tnone-missing\t2\t\tplan kinds, run r1: the values of none the repetitions did not record\t\n",
    ] {
        assert!(tsv.contains(want), "{want:?} not in\n{tsv}");
    }
    assert!(
        !tsv.contains("none-min"),
        "a measure with no value has no statistic rows"
    );
    // Every row of the file has the six columns of the results directory.
    for row in tsv.lines().filter(|l| !l.starts_with('#')) {
        assert_eq!(row.split('\t').count(), 6, "{row:?}");
    }
}

#[test]
fn numbers_print_with_at_most_three_decimals_and_no_trailing_zero() {
    for (v, want) in [
        (0.0, "0"),
        (-0.0, "0"),
        (-0.0001, "0"),
        (12.0, "12"),
        (12.5, "12.5"),
        (0.1234, "0.123"),
        (1.0005, "1.000"),
        (100.25, "100.25"),
        (-2.5, "-2.5"),
    ] {
        let want = want.trim_end_matches('0').trim_end_matches('.');
        let want = if want.is_empty() { "0" } else { want };
        assert_eq!(render::number(v), want, "{v}");
    }
}

#[test]
fn records_of_another_plan_or_out_of_the_plan_are_refused() {
    let plan = support::plan(PLAN);
    assert_eq!(
        analysis::analyze(&plan, &info("other"), &[]).expect_err("another plan"),
        AnalysisError::OtherPlan {
            run: "other".to_owned(),
            plan: "kinds".to_owned()
        }
    );
    let cases = [
        (
            rec(0, "go", "done", None, 0, None),
            "record 0: repetition 0 is not one of the run's 1 to 3",
        ),
        (
            rec(4, "go", "done", None, 0, None),
            "record 0: repetition 4 is not one of the run's 1 to 3",
        ),
        (
            rec(1, "go", "back", None, 0, None),
            "record 0: go/back is no event of the plan",
        ),
        (
            rec(1, "gone", "done", None, 0, None),
            "record 0: gone/done is no event of the plan",
        ),
    ];
    for (r, want) in cases {
        assert_eq!(
            analysis::analyze(&plan, &info("kinds"), &[r])
                .expect_err(want)
                .to_string(),
            want
        );
    }
    assert_eq!(
        AnalysisError::OtherPlan {
            run: "a".to_owned(),
            plan: "b".to_owned()
        }
        .to_string(),
        "the run is of plan \"a\", not \"b\""
    );
}

#[test]
fn a_span_whose_events_are_missing_in_a_repetition_counts_it_missing() {
    let plan = support::plan(PLAN);
    let records = vec![
        rec(1, "repetition", "started", None, 0, None),
        rec(1, "w", "back", Some("host-1"), 5_000, Some(105)),
        rec(2, "repetition", "started", None, 0, None),
        rec(2, "go", "done", None, 1_000, Some(100)),
    ];
    let a = analysis::analyze(&plan, &info("kinds"), &records).expect("the analysis");
    for id in ["first", "last"] {
        let m = a
            .results
            .iter()
            .find(|m| m.measure.id == id)
            .expect("a measure");
        assert!(m.values.is_empty(), "{id}");
        assert_eq!(m.missing, 2, "{id}");
    }
    assert_eq!(a.complete, 0);
    assert!(a.ended.is_empty());
    let md = render::markdown(&a);
    assert!(
        md.contains("**Repetitions that ended early.** None.\n"),
        "{md}"
    );
}
