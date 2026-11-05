//! The binary end to end: a workspace directory with crates, an lcov tracefile, and the
//! exit status and output it gives.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use testdir::TestDir;

/// A workspace of its own under the target directory with the crates `names`, removed when it
/// drops.
fn workspace(name: &str, names: &[&str]) -> TestDir {
    let root = TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("coverage-gate-{name}"),
    );
    for krate in names {
        let dir = root.join("crates").join(krate);
        std::fs::create_dir_all(dir.join("src")).expect("crate dir");
        std::fs::write(dir.join("Cargo.toml"), b"").expect("manifest");
    }
    root
}

/// Adds a crate under `<root>/examples`.
fn example(root: &Path, name: &str) {
    let dir = root.join("examples").join(name);
    std::fs::create_dir_all(dir.join("src")).expect("example dir");
    std::fs::write(dir.join("Cargo.toml"), b"").expect("manifest");
}

/// A tracefile with one source per `(path, lf, lh, brf, brh)`.
fn lcov(root: &Path, files: &[(&str, u64, u64, u64, u64)]) -> PathBuf {
    let mut text = String::new();
    for (path, lf, lh, brf, brh) in files {
        text.push_str(&format!(
            "SF:{}\nLF:{lf}\nLH:{lh}\nBRF:{brf}\nBRH:{brh}\nend_of_record\n",
            root.join(path).display()
        ));
    }
    let file = root.join("lcov.info");
    std::fs::write(&file, text).expect("lcov");
    file
}

fn gate(root: &Path, lcov: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_coverage-gate"))
        .arg("--lcov")
        .arg(lcov)
        .arg("--root")
        .arg(root)
        .args(extra)
        .output()
        .expect("run coverage-gate")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn crates_at_99_percent_pass_and_the_table_is_printed() {
    // Catches: a gate that fails a good workspace, or prints nothing.
    let root = workspace("pass", &["a", "b"]);
    let tracefile = lcov(
        &root,
        &[
            ("crates/a/src/lib.rs", 100, 99, 10, 10),
            ("crates/b/src/lib.rs", 4, 4, 0, 0),
        ],
    );
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    assert!(out(&o).contains("coverage gate: PASS"), "{}", out(&o));
}

#[test]
fn a_crate_under_99_percent_fails_with_status_1() {
    // Catches: the threshold not enforced, or enforced with another exit status.
    let root = workspace("fail", &["a"]);
    let tracefile = lcov(&root, &[("crates/a/src/lib.rs", 100, 98, 0, 0)]);
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(out(&o).contains("BELOW THRESHOLD (lines)"), "{}", out(&o));
}

#[test]
fn the_gate_fails_a_crate_at_98_99_percent_of_lines_or_branches_and_passes_99_00() {
    // Catches: the H1 boundary moved either way, through the binary's exit status: a crate at
    // 98.99% of its lines, or of its branches, fails with status 1; at 99.00% of both, it
    // passes with status 0.
    let root = workspace("boundary", &["a"]);
    for (case, lh, brh, code, words) in [
        ("lines", 9_899, 10_000, 1, "BELOW THRESHOLD (lines)"),
        ("branches", 10_000, 9_899, 1, "BELOW THRESHOLD (branches)"),
        (
            "both at 99.00",
            9_900,
            9_900,
            0,
            "| a | 9900/10000 | 99.00 | 9900/10000 | 99.00 | ok |",
        ),
    ] {
        let tracefile = lcov(&root, &[("crates/a/src/lib.rs", 10_000, lh, 10_000, brh)]);
        let o = gate(&root, &tracefile, &[]);
        assert_eq!(o.status.code(), Some(code), "{case}: {}", out(&o));
        assert!(out(&o).contains(words), "{case}: {}", out(&o));
    }
}

#[test]
fn there_is_no_way_to_exclude_a_crate() {
    // Catches: an exclusion flag coming back, which would let a crate skip the gate.
    let root = workspace("exclude", &["a", "tool"]);
    let tracefile = lcov(
        &root,
        &[
            ("crates/a/src/lib.rs", 10, 10, 0, 0),
            ("crates/tool/src/lib.rs", 10, 1, 0, 0),
        ],
    );
    let o = gate(&root, &tracefile, &["--exclude", "tool"]);
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("unexpected argument '--exclude'"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(
        out(&o).contains("| tool | 1/10 | 10.00 | - | - | BELOW THRESHOLD (lines) |"),
        "{}",
        out(&o)
    );
}

#[test]
fn a_summary_file_gets_the_table_appended() {
    // Catches: the job summary overwritten or not written.
    let root = workspace("summary", &["a"]);
    let tracefile = lcov(&root, &[("crates/a/src/lib.rs", 10, 10, 0, 0)]);
    let summary = root.join("summary.md");
    std::fs::write(&summary, "# earlier\n").expect("summary");
    let o = gate(
        &root,
        &tracefile,
        &["--summary-file", summary.to_str().expect("utf8")],
    );
    assert_eq!(o.status.code(), Some(0));
    let text = std::fs::read_to_string(&summary).expect("read");
    assert!(text.starts_with("# earlier\n| crate |"), "{text}");
}

#[test]
fn unreadable_input_is_status_2() {
    // Catches: a missing or damaged tracefile read as a pass or as a gate failure.
    let root = workspace("input", &["a"]);
    let o = gate(&root, &root.join("absent.info"), &[]);
    assert_eq!(o.status.code(), Some(2));
    let empty = root.join("empty.info");
    std::fs::write(&empty, "").expect("empty");
    let o = gate(&root, &empty, &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no records"));
    let stray = lcov(&root, &[("elsewhere/src/lib.rs", 1, 1, 0, 0)]);
    let o = gate(&root, &stray, &[]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn a_crate_with_no_instrumented_line_and_one_under_on_branches_are_named() {
    // Catches: a crate that never ran passing for lack of data, and a branch shortfall
    // reported under the wrong metric.
    let root = workspace("nodata", &["a", "b"]);
    let tracefile = lcov(&root, &[("crates/a/src/lib.rs", 10, 10, 100, 90)]);
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(out(&o).contains("NOTHING MEASURED"), "{}", out(&o));
    assert!(
        out(&o).contains("BELOW THRESHOLD (branches)"),
        "{}",
        out(&o)
    );
}

#[test]
fn a_relative_root_is_resolved_from_the_working_directory() {
    // Catches: a relative `--root` compared as written with the tracefile's absolute paths.
    let root = workspace("relative", &["a"]);
    let tracefile = lcov(&root, &[("crates/a/src/lib.rs", 10, 10, 0, 0)]);
    let o = Command::new(env!("CARGO_BIN_EXE_coverage-gate"))
        .current_dir(&root)
        .args(["--lcov"])
        .arg(&tracefile)
        .args(["--root", "."])
        .output()
        .expect("run coverage-gate");
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    assert!(out(&o).contains("| a | 10/10 |"), "{}", out(&o));
    assert!(out(&o).contains("coverage gate: PASS"), "{}", out(&o));
}

#[test]
fn a_root_that_is_not_a_workspace_is_status_2() {
    // Catches: a workspace that cannot be listed read as an empty one, which passes.
    let root = workspace("no-crates", &["a"]);
    let tracefile = lcov(&root, &[("crates/a/src/lib.rs", 10, 10, 0, 0)]);
    let o = gate(&root.join("absent"), &tracefile, &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("list crates under"));
}

#[test]
fn a_summary_file_that_cannot_be_written_is_status_2() {
    // Catches: a failed summary write ignored, so the job summary silently goes missing.
    let root = workspace("summary-dir", &["a"]);
    let tracefile = lcov(&root, &[("crates/a/src/lib.rs", 10, 10, 0, 0)]);
    let o = gate(
        &root,
        &tracefile,
        &["--summary-file", root.to_str().expect("utf8")],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("append to"));
}

#[test]
fn a_record_missing_a_summary_is_status_2_not_a_pass() {
    // Catches: a fully covered file beside a record whose uncovered lines carry no summary
    // passing the gate, and a report made without branch coverage passing the branch gate.
    let root = workspace("missing-summary", &["a"]);
    let lib = root.join("crates/a/src/lib.rs");
    let other = root.join("crates/a/src/other.rs");
    let good = format!(
        "SF:{}\nLF:10\nLH:10\nBRF:2\nBRH:2\nend_of_record\n",
        lib.display()
    );
    for (case, record) in [
        (
            "lines",
            format!("SF:{}\nDA:1,0\nDA:2,0\nend_of_record\n", other.display()),
        ),
        (
            "branches",
            format!("SF:{}\nLF:3\nLH:3\nend_of_record\n", other.display()),
        ),
    ] {
        let file = root.join(format!("{case}.info"));
        std::fs::write(&file, format!("{good}{record}")).expect("lcov");
        let o = gate(&root, &file, &[]);
        assert_eq!(o.status.code(), Some(2), "{case}: {}", out(&o));
        let field = if case == "lines" { "LF" } else { "BRF" };
        assert!(
            String::from_utf8_lossy(&o.stderr).contains(&format!("has no {field}")),
            "{case}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

#[test]
fn counts_that_overflow_are_status_2_not_a_pass() {
    // Catches: two half-covered records of u64::MAX lines saturating into 100% and passing.
    let root = workspace("overflow", &["a"]);
    let half = 1u64 << 63;
    let tracefile = lcov(
        &root,
        &[
            ("crates/a/src/lib.rs", u64::MAX, half, 0, 0),
            ("crates/a/src/other.rs", u64::MAX, half, 0, 0),
        ],
    );
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "coverage-gate: the coverage counts of a do not fit in 64 bits\n"
    );
}

#[test]
fn a_source_that_climbs_out_of_its_crate_is_status_2() {
    // Catches: `crates/a/../../outside.rs`, which is not in crate `a`, counted for `a`.
    let root = workspace("climbs", &["a"]);
    let tracefile = lcov(
        &root,
        &[
            ("crates/a/src/lib.rs", 10, 10, 0, 0),
            ("crates/a/../../outside.rs", 10, 10, 0, 0),
        ],
    );
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("is not under <root>/crates/<crate>/"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn a_crate_under_examples_is_judged_and_named_by_its_directory() {
    // Catches: an examples crate whose record fails the run as unattributed, one left out of
    // the table, and one that is not held to 99%.
    let root = workspace("examples", &["a"]);
    example(&root, "hello");
    let tracefile = lcov(
        &root,
        &[
            ("crates/a/src/lib.rs", 10, 10, 0, 0),
            ("examples/hello/src/lib.rs", 100, 99, 10, 10),
        ],
    );
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    assert!(
        out(&o).contains("| hello | 99/100 | 99.00 | 10/10 | 100.00 | ok |"),
        "{}",
        out(&o)
    );
    let tracefile = lcov(
        &root,
        &[
            ("crates/a/src/lib.rs", 10, 10, 0, 0),
            ("examples/hello/src/lib.rs", 100, 90, 0, 0),
        ],
    );
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(
        out(&o).contains("| hello | 90/100 | 90.00 | - | - | BELOW THRESHOLD (lines) |"),
        "{}",
        out(&o)
    );
}

#[test]
fn a_workspace_without_an_examples_directory_is_judged_as_before() {
    // Catches: a missing `examples` directory failing the run (the workspace of every
    // repository without examples), or changing a byte of the table.
    let root = workspace("no-examples", &["a"]);
    assert!(!root.join("examples").exists());
    let tracefile = lcov(&root, &[("crates/a/src/lib.rs", 10, 10, 0, 0)]);
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    assert_eq!(
        out(&o),
        "| crate | lines | line % | branches | branch % | status |\n\
         |---|---:|---:|---:|---:|---|\n\
         | a | 10/10 | 100.00 | - | - | ok |\n\
         | **workspace** | 10/10 | 100.00 | - | - |  |\n\
         \n\
         coverage gate: PASS (threshold 99.00 of lines and branches)\n"
    );
}

#[test]
fn two_crates_with_one_directory_name_are_status_2() {
    // Catches: `crates/hello` and `examples/hello` merged into one row, which would let one
    // hide the other's shortfall.
    let root = workspace("same-name", &["hello"]);
    example(&root, "hello");
    let tracefile = lcov(
        &root,
        &[
            ("crates/hello/src/lib.rs", 10, 10, 0, 0),
            ("examples/hello/src/lib.rs", 10, 1, 0, 0),
        ],
    );
    let o = gate(&root, &tracefile, &[]);
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert_eq!(out(&o), "");
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "coverage-gate: list crates under {}: hello is the directory of both crates/hello \
             and examples/hello: crates are named by directory in the table, so rename one\n",
            root.display()
        )
    );
}
