use std::path::PathBuf;

use super::*;
use crate::testdir::TestDir;

fn cov(lf: u64, lh: u64, brf: u64, brh: u64) -> CrateCoverage {
    CrateCoverage {
        lines: Counts::new(lf, lh).expect("valid"),
        branches: Counts::new(brf, brh).expect("valid"),
    }
}

fn record(path: &str, coverage: CrateCoverage) -> FileRecord {
    FileRecord {
        path: PathBuf::from(path),
        coverage,
    }
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn row(measured: CrateCoverage) -> Row {
    Row {
        krate: "a".to_owned(),
        measured,
    }
}

/// Catches: a record attributed to the wrong crate, a crate with no record missing from the
/// result (it would then never be judged), and two files of one crate not summed.
#[test]
fn records_are_summed_per_crate_and_every_crate_is_listed() {
    let root = Path::new("/w");
    let files = [
        record("/w/crates/a/src/lib.rs", cov(10, 9, 2, 1)),
        record("/w/crates/a/tests/t.rs", cov(5, 5, 0, 0)),
        record("/w/crates/b/src/lib.rs", cov(4, 4, 1, 1)),
    ];
    let got = by_crate(root, &names(&["a", "b", "c"]), &files).expect("attributed");
    assert_eq!(got["a"], cov(15, 14, 2, 1));
    assert_eq!(got["b"], cov(4, 4, 1, 1));
    assert_eq!(got["c"], CrateCoverage::default());
}

/// Catches: a source outside every crate dropped (its lines would escape the gate), a file
/// directly under `crates/`, a path under a directory that is not a workspace crate, and a
/// path that starts in a crate and climbs out of it with `..`.
#[test]
fn a_record_outside_every_crate_is_an_error() {
    let root = Path::new("/w");
    for path in [
        "/elsewhere/src/lib.rs",
        "/w/crates/lib.rs",
        "/w/crates/ghost/src/lib.rs",
        "/w/src/lib.rs",
        "/w/crates/a/../../outside.rs",
        "/w/crates/a/src/../../b/src/lib.rs",
    ] {
        let files = [record(path, cov(1, 1, 0, 0))];
        assert_eq!(
            by_crate(root, &names(&["a"]), &files),
            Err(Error::Unattributed(PathBuf::from(path))),
            "{path}"
        );
    }
}

/// Catches: the threshold applied to the wrong metric, `>` for `>=`, and a crate with no
/// branches failing for want of them.
#[test]
fn a_crate_passes_at_99_percent_of_each_metric_and_fails_under_it() {
    let at = row(cov(100, 99, 100, 99));
    assert_eq!(at.status(), Status::Ok);
    let no_branches = row(cov(100, 100, 0, 0));
    assert_eq!(no_branches.status(), Status::Ok);
    let lines = row(cov(101, 99, 100, 100));
    assert_eq!(lines.status(), Status::Below(vec![Metric::Lines]));
    let branches = row(cov(100, 100, 101, 99));
    assert_eq!(branches.status(), Status::Below(vec![Metric::Branches]));
    let both = row(cov(100, 50, 100, 50));
    assert_eq!(
        both.status(),
        Status::Below(vec![Metric::Lines, Metric::Branches])
    );
}

/// Catches: a crate whose tests stopped being instrumented passing with zero lines.
#[test]
fn a_crate_with_no_instrumented_line_fails() {
    let nothing = row(CrateCoverage::default());
    assert_eq!(nothing.status(), Status::NoData);
    assert!(nothing.status().fails());
}

/// Catches: rows missing a workspace crate, or out of name order.
#[test]
fn rows_cover_the_workspace_in_name_order() {
    let measured: BTreeMap<String, CrateCoverage> = [
        ("b".to_owned(), cov(1, 1, 0, 0)),
        ("a".to_owned(), cov(2, 2, 0, 0)),
    ]
    .into();
    let got = rows(&measured);
    let summary: Vec<(&str, CrateCoverage)> =
        got.iter().map(|r| (r.krate.as_str(), r.measured)).collect();
    assert_eq!(summary, [("a", cov(2, 2, 0, 0)), ("b", cov(1, 1, 0, 0))]);
}

/// The H1 boundary, lines: a crate at 98.99% of its lines fails and one at 99.00% passes,
/// with every branch covered. Catches: a threshold of 98.99 or below, a comparison that
/// rounds 98.99 up, and `>` in place of `>=`.
#[test]
fn a_crate_at_98_99_percent_of_its_lines_fails_and_at_99_00_passes() {
    let under = row(cov(10_000, 9_899, 10, 10));
    assert_eq!(under.status(), Status::Below(vec![Metric::Lines]));
    assert!(under.status().fails());
    let at = row(cov(10_000, 9_900, 10, 10));
    assert_eq!(at.status(), Status::Ok);
    assert!(!at.status().fails());
}

/// The H1 boundary, branches: a crate at 98.99% of its branches fails and one at 99.00%
/// passes, with every line covered. Catches: the threshold read from the line counts only.
#[test]
fn a_crate_at_98_99_percent_of_its_branches_fails_and_at_99_00_passes() {
    let under = row(cov(10, 10, 10_000, 9_899));
    assert_eq!(under.status(), Status::Below(vec![Metric::Branches]));
    assert!(under.status().fails());
    let at = row(cov(10, 10, 10_000, 9_900));
    assert_eq!(at.status(), Status::Ok);
}

/// Catches: a table that drops the status, the workspace total or the verdict line.
#[test]
fn the_table_has_a_row_per_crate_a_total_and_a_verdict() {
    let measured: BTreeMap<String, CrateCoverage> = [
        ("a".to_owned(), cov(100, 100, 10, 10)),
        ("b".to_owned(), cov(100, 90, 0, 0)),
    ]
    .into();
    let report = render(&rows(&measured), total(&measured).expect("fits"));
    assert!(
        report.contains("| a | 100/100 | 100.00 | 10/10 | 100.00 | ok |"),
        "{report}"
    );
    assert!(
        report.contains("| b | 90/100 | 90.00 | - | - | BELOW THRESHOLD (lines) |"),
        "{report}"
    );
    assert!(
        report.contains("| **workspace** | 190/200 | 95.00 | 10/10 | 100.00 |  |"),
        "{report}"
    );
    assert!(
        report.ends_with("coverage gate: FAIL (1 crate(s) need attention; see status)\n"),
        "{report}"
    );
    let passing: BTreeMap<String, CrateCoverage> = [("a".to_owned(), cov(1, 1, 0, 0))].into();
    let report = render(&rows(&passing), total(&passing).expect("fits"));
    assert!(
        report.ends_with("coverage gate: PASS (threshold 99.00 of lines and branches)\n"),
        "{report}"
    );
}

/// Catches: the crate list taking files or directories without a manifest for crates.
#[test]
fn workspace_crates_are_the_directories_with_a_manifest() {
    let exe = std::env::current_exe().expect("test binary");
    let root = TestDir::under(
        &exe.parent().expect("deps").join("coverage-gate-unit"),
        "crates",
    );
    for dir in ["a", "b", "no-manifest"] {
        std::fs::create_dir_all(root.join("crates").join(dir)).expect("dir");
    }
    std::fs::write(root.join("crates/a/Cargo.toml"), b"").expect("manifest");
    std::fs::write(root.join("crates/b/Cargo.toml"), b"").expect("manifest");
    std::fs::write(root.join("crates/file"), b"").expect("file");
    assert_eq!(workspace_crates(&root).expect("list"), names(&["a", "b"]));
    assert!(workspace_crates(&root.join("absent")).is_err());
}

/// Catches: counts that overflow saturated into a passing crate. Two records each half
/// covered with `u64::MAX` lines would saturate to `u64::MAX` of `u64::MAX`, 100%, though
/// together they are about 50% covered; the sum is an error naming the crate, and a workspace
/// total that does not fit is an error too.
#[test]
fn a_sum_that_does_not_fit_is_an_error_naming_the_crate() {
    let root = Path::new("/w");
    let half = cov(u64::MAX, 1 << 63, 0, 0);
    let files = [
        record("/w/crates/a/src/lib.rs", half),
        record("/w/crates/a/src/other.rs", half),
    ];
    assert_eq!(
        by_crate(root, &names(&["a"]), &files),
        Err(Error::Overflow("a".to_owned()))
    );
    assert_eq!(
        Error::Overflow("a".to_owned()).to_string(),
        "the coverage counts of a do not fit in 64 bits"
    );
    let measured: BTreeMap<String, CrateCoverage> =
        [("a".to_owned(), half), ("b".to_owned(), half)].into();
    assert_eq!(
        total(&measured),
        Err(Error::Overflow("the workspace".to_owned()))
    );
}
