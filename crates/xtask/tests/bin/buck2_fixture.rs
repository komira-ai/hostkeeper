//! The `buck2` the farm-build tests (tests/farm_build_cli.rs) hand `cargo xtask farm-build` with
//! `--buck2`, and the `buildrules` command line that buck2 names. A program of the tests, not
//! shipped; it builds nothing and reaches no network. Which of the two it is depends on its file
//! name: a copy named `buildrules` is the command line, any other is buck2.
//!
//! As buck2 it appends its arguments, joined by spaces, as one line to `calls` beside its own
//! file, then answers from files there:
//!
//! - `audit ...` prints `audit.stdout` (`{}`, buck2's answer for a key that is not set, when there
//!   is none) and `audit.stderr` (on standard error), and exits with the number in `audit.status`
//!   (0 when there is none);
//! - `build buildrules//cli:buildrules ...` answers the same way from `tool.*`; its standard output
//!   is, when there is no `tool.stdout`, the line `buildrules//cli:buildrules <dir>/buildrules`,
//!   naming the copy named `buildrules` beside it;
//! - any other `build ...`, the n-th of them (from 1), answers from `build.<n>.stdout`,
//!   `build.<n>.stderr` and `build.<n>.status` when there is such a file, else from `build.*`, so
//!   a test can say what each run of the build printed;
//! - `kill` exits 0.
//!
//! A `.status` file that says `abort` makes the call end by a signal (SIGABRT) instead of with a
//! status, after it printed its answer.
//!
//! Anything else exits 2.
//!
//! As `buildrules` it appends its arguments as one line to `buildrules.calls` beside its own file,
//! and works on a table of its own in its working directory: `buildrules.lock.bzl`, one line
//! `<file> <sha256>` for each of `Cargo.toml` and `Cargo.lock`, and
//! `third_party/buildrules/git_archives.tsv`, there only when `Cargo.toml` has a `git = `
//! dependency. `sync` writes both (and removes the second when there is no git dependency).
//! `check` exits 1, with one line in the real command line's form, when there is no table; else
//! with one `STALE:` line for each of the two files the table records another sha256 of, or when
//! they agree, for each generated file that is not what `sync` writes; else 0. The first line of a
//! file `defect` beside it asks for one failure: `sync-fails` (sync exits 1 with a line),
//! `sync-writes-nothing` (sync exits 0 and writes nothing), `check-usage` (check exits 2 with a
//! usage line).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sha2::Digest as _;

/// The target of the `buildrules` command line.
const TOOL: &str = "buildrules//cli:buildrules";

/// This program's own file.
fn me() -> PathBuf {
    std::env::current_exe().expect("the fixture's own path")
}

/// The text of `name` in `dir`, `None` when it cannot be read.
fn file(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

/// Appends `line` to `name` in `dir`.
fn record(dir: &Path, name: &str, line: &str) {
    let mut calls = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))
        .expect("the calls file");
    writeln!(calls, "{line}").expect("record the call");
}

/// Prints the answer of `step` (`<step>.stdout`, `.stderr`, `.status` in `dir`) and returns its
/// exit status.
fn answer(dir: &Path, step: &str, stdout_default: &str) -> ExitCode {
    let read = |suffix: &str, default: &str| {
        file(dir, &format!("{step}.{suffix}")).unwrap_or_else(|| default.to_owned())
    };
    print!("{}", read("stdout", stdout_default));
    eprint!("{}", read("stderr", ""));
    let status = read("status", "0");
    if status.trim() == "abort" {
        // Ended by a signal (SIGABRT), not with a status of its own.
        std::process::abort();
    }
    ExitCode::from(status.trim().parse::<u8>().expect("a status of 0 to 255"))
}

/// The fixture as buck2.
fn buck2(dir: &Path, args: &[String]) -> ExitCode {
    record(dir, "calls", &args.join(" "));
    match args.first().map(String::as_str) {
        Some("audit") => answer(dir, "audit", "{}"),
        Some("build") if args.iter().any(|a| a == TOOL) => {
            let tool = format!("{TOOL} {}\n", dir.join("buildrules").display());
            answer(dir, "tool", &tool)
        }
        Some("build") => {
            // This call is the n-th build of a target that is not the tool.
            let n = file(dir, "calls")
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with("build ") && !l.contains(TOOL))
                .count();
            let step = format!("build.{n}");
            let any = ["stdout", "stderr", "status"]
                .iter()
                .any(|s| dir.join(format!("{step}.{s}")).exists());
            answer(dir, if any { &step } else { "build" }, "")
        }
        Some("kill") => ExitCode::SUCCESS,
        _ => ExitCode::from(2),
    }
}

/// The table.
const TABLE: &str = "buildrules.lock.bzl";
/// The one generated file beside the table, there only for a git dependency.
const ARCHIVES: &str = "third_party/buildrules/git_archives.tsv";
/// The files the table records.
const INPUTS: [&str; 2] = ["Cargo.toml", "Cargo.lock"];
/// The fix the real command line ends each of its lines with.
const FIX: &str = "run `buildrules sync` and commit the result";

/// The sha256 of the file `path` of the working directory (of nothing, when it cannot be read).
fn sha256(path: &str) -> String {
    hex::encode(sha2::Sha256::digest(
        file(Path::new("."), path).unwrap_or_default(),
    ))
}

/// What a sync of the working directory writes: each generated file, and its text (`None`: the
/// file is removed).
fn synced() -> [(&'static str, Option<String>); 2] {
    let table = INPUTS
        .iter()
        .map(|input| format!("{input} {}\n", sha256(input)))
        .collect();
    let git = file(Path::new("."), "Cargo.toml").is_some_and(|m| m.contains("git = "));
    [
        (TABLE, Some(table)),
        (ARCHIVES, git.then(|| "github.com/o/r 0123abc\n".to_owned())),
    ]
}

/// The fixture as the `buildrules` command line.
fn buildrules(dir: &Path, args: &[String]) -> ExitCode {
    record(dir, "buildrules.calls", &args.join(" "));
    let defect = file(dir, "defect").unwrap_or_default();
    let defect = defect.lines().next().unwrap_or_default();
    let here = Path::new(".");
    if args.first().is_some_and(|verb| verb == "sync") {
        if defect == "sync-fails" {
            eprintln!("buildrules: sync: CargoFailed");
            return ExitCode::from(1);
        }
        if defect != "sync-writes-nothing" {
            for (path, text) in synced() {
                match text {
                    Some(text) => {
                        let path = Path::new(path);
                        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
                        std::fs::write(path, text).expect("write");
                    }
                    None => {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
        println!("buildrules: wrote {TABLE}");
        return ExitCode::SUCCESS;
    }
    if defect == "check-usage" {
        eprintln!("buildrules: check: unknown argument --nope");
        return ExitCode::from(2);
    }
    let Some(table) = file(here, TABLE) else {
        eprintln!("buildrules: {TABLE} is missing: {FIX}");
        return ExitCode::from(1);
    };
    let mut stale: Vec<String> = INPUTS
        .iter()
        .filter_map(|input| {
            let now = sha256(input);
            let recorded = table
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{input} ")))
                .unwrap_or("none");
            (recorded != now).then(|| {
                format!(
                    "{input} changed since `buildrules sync` (recorded sha256 {recorded}, now {now})"
                )
            })
        })
        .collect();
    if stale.is_empty() {
        stale = synced()
            .into_iter()
            .filter(|(path, text)| file(here, path) != *text)
            .map(|(path, _)| format!("{path} is not what sync writes"))
            .collect();
    }
    for line in &stale {
        eprintln!("buildrules: STALE: {line}: {FIX}");
    }
    ExitCode::from(u8::from(!stale.is_empty()))
}

fn main() -> ExitCode {
    let me = me();
    let dir = me.parent().expect("the fixture's directory").to_owned();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if me.file_name().is_some_and(|n| n == "buildrules") {
        buildrules(&dir, &args)
    } else {
        buck2(&dir, &args)
    }
}
