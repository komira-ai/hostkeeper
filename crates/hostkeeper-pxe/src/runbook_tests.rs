//! `docs/operations/network-install.md`, the operator runbook, against this command line: every
//! line in a fenced block that begins `hostkeeper-pxe` parses with the program's own parser
//! ([`Cli`]), and the runbook has a line of each run it describes: a fetch, `serve` in proxy
//! mode, `serve` in router mode, and a `serve` with `--reinstall`. A line continued with a
//! trailing `\` is one line; the words are split at white space, and a line with a quote is
//! refused, since none of the runbook's lines needs one. A renamed or misspelled flag, a value a
//! flag does not take, or a required flag the runbook leaves out fails here.

use std::collections::BTreeSet;
use std::path::Path;

use clap::Parser;

use super::{Cli, ModeFlag, Verb};

/// The runbook, from this crate's directory.
const RUNBOOK: &str = "../../docs/operations/network-install.md";

/// The runs the runbook must show.
const RUNS: [&str; 4] = ["fetch", "serve proxy", "serve router", "serve --reinstall"];

/// The fenced lines that begin `hostkeeper-pxe`, each line ending in `\` joined to the next. A
/// line still continued where its block ends is kept with its `\`, which [`problems`] refuses.
fn command_lines(doc: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut fenced = false;
    let mut pending = String::new();
    for line in doc.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            if !pending.is_empty() {
                lines.push(format!("{}\\", std::mem::take(&mut pending)));
            }
            fenced = !fenced;
            continue;
        }
        if !fenced {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(head) => {
                pending.push_str(head.trim_end());
                pending.push(' ');
            }
            None => {
                pending.push_str(line);
                lines.push(std::mem::take(&mut pending));
            }
        }
    }
    lines
        .into_iter()
        .filter(|l| l.split_whitespace().next() == Some("hostkeeper-pxe"))
        .collect()
}

/// The run `line` is, as [`RUNS`] names it, or why it does not parse.
fn run_of(line: &str) -> Result<&'static str, String> {
    if line.ends_with('\\') || line.contains(['"', '\'']) {
        return Err(format!("{line}\na line cut at its \\ or quoted"));
    }
    let cli = Cli::try_parse_from(line.split_whitespace())
        .map_err(|e| format!("{line}\n{}", e.render()))?;
    Ok(match cli.verb {
        Verb::Fetch(_) => "fetch",
        Verb::Serve(flags) if !flags.reinstall.is_empty() => "serve --reinstall",
        Verb::Serve(flags) if flags.mode == ModeFlag::Proxy => "serve proxy",
        Verb::Serve(_) => "serve router",
    })
}

/// Every line of `doc` that does not parse, and every run of [`RUNS`] no line shows.
fn problems(doc: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut shown = BTreeSet::new();
    for line in command_lines(doc) {
        match run_of(&line) {
            Ok(run) => {
                shown.insert(run);
            }
            Err(why) => problems.push(why),
        }
    }
    for run in RUNS {
        if !shown.contains(run) {
            problems.push(format!("no command line shows `hostkeeper-pxe {run}`"));
        }
    }
    problems
}

/// A fenced block holding `line`.
fn fenced(line: &str) -> String {
    format!("text\n\n```\n{line}\n```\n")
}

/// The serve flags every run needs, after the mode.
const SERVE: &str = "--address 192.0.2.10 --dir d --daemon-config c --ssh-authorized-keys k \
     --coordinator https://c.example --ca-file a --token-file t --user-id 1 --group-id 1";

/// A line of each run.
fn run_lines() -> [String; 4] {
    [
        "hostkeeper-pxe fetch --release https://r.example/v1 --dir d --daemon-config c".to_owned(),
        format!("hostkeeper-pxe serve --mode proxy {SERVE}"),
        format!("hostkeeper-pxe serve --mode router {SERVE}"),
        format!("hostkeeper-pxe serve --mode proxy {SERVE} --reinstall web-17"),
    ]
}

/// A runbook with a line of each run, in a block each.
fn every_run() -> String {
    run_lines().iter().map(|line| fenced(line)).collect()
}

/// Catches a runbook command line that `hostkeeper-pxe` would refuse as a usage error, and a run
/// the runbook describes with no command line.
#[test]
fn the_runbook_command_lines_parse() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(RUNBOOK);
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {RUNBOOK}: {e}"));
    let problems = problems(&doc);
    assert!(problems.is_empty(), "{RUNBOOK}:\n{}", problems.join("\n"));
}

/// Catches the check passing a misspelled flag, a missing required flag, a value a flag does not
/// take, a quoted line, and a line cut at its `\`.
#[test]
fn a_line_the_parser_refuses_is_a_problem() {
    assert_eq!(problems(&every_run()), Vec::<String>::new());
    for line in [
        format!("hostkeeper-pxe serve --mode proxy {SERVE} --reinstal web-17"),
        format!("hostkeeper-pxe serve --mode both {SERVE}"),
        "hostkeeper-pxe serve --mode proxy --address 192.0.2.10".to_owned(),
        "hostkeeper-pxe fetch --release http://r.example --dir d --daemon-config c".to_owned(),
        "hostkeeper-pxe fetch --release https://r.example --dir \"d\" --daemon-config c".to_owned(),
        "hostkeeper-pxe fetch --release https://r.example \\".to_owned(),
    ] {
        let problems = problems(&(every_run() + &fenced(&line)));
        assert_eq!(problems.len(), 1, "{line}: {problems:?}");
        assert!(
            problems[0].starts_with(&format!("{line}\n")),
            "{problems:?}"
        );
    }
}

/// Catches a runbook that leaves out a run, and lines read from outside a fenced block or from a
/// line that does not begin with the program.
#[test]
fn every_run_has_a_command_line() {
    let without_router: String = run_lines()
        .iter()
        .filter(|line| !line.contains("--mode router"))
        .map(|line| fenced(line))
        .collect();
    let outside = format!(
        "Run `hostkeeper-pxe serve` and\nhostkeeper-pxe serve --bad\n{}",
        fenced("sudo hostkeeper-pxe serve --bad")
    );
    assert_eq!(
        problems(&(without_router + &outside)),
        ["no command line shows `hostkeeper-pxe serve router`"]
    );
    let continued = format!("hostkeeper-pxe serve --mode router \\\n    {SERVE}");
    assert_eq!(
        command_lines(&fenced(&continued)),
        [format!("hostkeeper-pxe serve --mode router {SERVE}")]
    );
}
