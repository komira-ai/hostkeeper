//! The rule of [`super::xtask_calls`]: every `cargo xtask` call of a workflow is one the binary's
//! own command line accepts. Each case puts one call into [`super::CLEAN`] in place of its
//! `cargo test` step (line 16) and reads the exact problems the lint reports.

use super::downloads::collect_runs;
use super::xtask_calls::{commands, xtask_args};
use super::{assert_problems, with, workflows, yaml};

/// The step of [`super::CLEAN`] each case replaces.
const STEP: &str = "      - run: cargo test --workspace --locked\n";

/// The flags of `release nfpm-stage` but `--binaries`: the call net-install.yml made before it
/// passed the static programs.
const NFPM_STAGE: &str = "cargo xtask release nfpm-stage --arch amd64 --gh-version \"$GH_VERSION\" --gh-archive \"$RUNNER_TEMP/gh.tar.gz\" --gh-sha256 \"$GH_SHA256\" --trusted-root \"$RUNNER_TEMP/trusted_root.json\" --out target/nfpm";

/// [`super::CLEAN`] with its step's script replaced by `run` (one line).
fn run(run: &str) -> String {
    with(STEP, &format!("      - run: {run}\n"))
}

/// [`super::CLEAN`] with its step's script replaced by the block `lines`.
fn block(lines: &[&str]) -> String {
    let body: String = lines.iter().map(|l| format!("          {l}\n")).collect();
    with(STEP, &format!("      - run: |\n{body}"))
}

/// The problem of [`NFPM_STAGE`] in a script on `line`.
fn missing_binaries(line: usize) -> [String; 1] {
    [format!(
        "line {line}: `cargo xtask release nfpm-stage`: one or more required arguments were not provided: `--binaries <BINARIES>`"
    )]
}

#[test]
fn a_call_that_leaves_out_a_required_flag_is_refused() {
    // Catches: net-install.yml's `release nfpm-stage` with no `--binaries`, the flag the verb
    // has required since it stages the static programs, which failed only when the job ran.
    assert_problems(
        &run(NFPM_STAGE),
        &missing_binaries(16).each_ref().map(String::as_str),
    );
    let staged = format!("{NFPM_STAGE} --binaries target/x86_64-unknown-linux-musl/release");
    assert_problems(&run(&staged), &[]);
    // Every required flag left out is named, in the verb's order.
    assert_problems(
        &run("cargo xtask release nfpm-stage --arch amd64"),
        &[
            "line 16: `cargo xtask release nfpm-stage`: one or more required arguments were not provided: `--gh-version <GH_VERSION>`, `--gh-archive <GH_ARCHIVE>`, `--gh-sha256 <GH_SHA256>`, `--trusted-root <TRUSTED_ROOT>`, `--binaries <BINARIES>`, `--out <OUT>`",
        ],
    );
}

#[test]
fn a_call_that_leaves_out_a_required_argument_is_refused() {
    // Catches: a verb of positional arguments given too few, and the same verb given all.
    assert_problems(
        &run("cargo xtask release check-binary target/release/hostkeeper \"$RELEASE_VERSION\""),
        &[
            "line 16: `cargo xtask release check-binary`: one or more required arguments were not provided: `<COMMIT>`",
        ],
    );
    assert_problems(
        &run(
            "cargo xtask release check-binary target/release/hostkeeper \"$RELEASE_VERSION\" \"$GITHUB_SHA\"",
        ),
        &[],
    );
}

#[test]
fn a_flag_or_verb_the_binary_does_not_have_is_refused() {
    // Catches: a misspelt or removed flag, a flag given twice, a verb that does not exist, a
    // call that names no verb: each would fail only when the step ran; and the help passes.
    let cases: [(&str, &str); 6] = [
        (
            "cargo xtask release version --commit \"$GITHUB_SHA\" --version-file VERSION --output \"$GITHUB_OUTPUT\" --dry-run",
            "line 16: `cargo xtask release version`: unexpected argument found: `--dry-run`",
        ),
        (
            "cargo xtask release set-version --version 1.0.0 --version 1.0.1",
            "line 16: `cargo xtask release set-version`: an argument cannot be used with one or more of the other specified arguments: `--version <VERSION>`",
        ),
        (
            "cargo xtask release nfpm-stag --arch amd64",
            "line 16: `cargo xtask release`: unrecognized subcommand: `nfpm-stag`",
        ),
        (
            "cargo xtask relase version",
            "line 16: `cargo xtask`: unrecognized subcommand: `relase`",
        ),
        ("cargo xtask", "line 16: `cargo xtask` names no verb"),
        (
            "cargo xtask release",
            "line 16: `cargo xtask release` names no verb",
        ),
    ];
    for (call, want) in cases {
        assert_problems(&run(call), &[want]);
    }
    // The help, which compiles xtask and runs no verb, at the top and of a verb.
    for call in ["cargo xtask --help", "cargo xtask release index --help"] {
        assert_problems(&run(call), &[]);
    }
}

#[test]
fn a_literal_value_is_checked_by_the_verbs_parser_and_a_variable_is_not() {
    // Catches: an `--arch` that is not an architecture, a version or SHA-256 that is not one,
    // written in the workflow; and a value known only when the step runs refused for it.
    let arch = with_flag("--arch amd64", "--arch amd65");
    assert_problems(
        &run(&arch),
        &[
            "line 16: `cargo xtask release nfpm-stage`: one of the values isn't valid for an argument: `--arch <ARCH>`, `amd65`",
        ],
    );
    let version = with_flag("--gh-version \"$GH_VERSION\"", "--gh-version 2.x");
    assert_problems(
        &run(&version),
        &[
            "line 16: `cargo xtask release nfpm-stage`: invalid value for one of the arguments: `--gh-version <GH_VERSION>`, `2.x`",
        ],
    );
    let sha = with_flag("--gh-sha256 \"$GH_SHA256\"", "--gh-sha256 abc");
    assert_problems(
        &run(&sha),
        &[
            "line 16: `cargo xtask release nfpm-stage`: invalid value for one of the arguments: `--gh-sha256 <GH_SHA256>`, `abc`",
        ],
    );
    for value in ["\"$ARCH\"", "${{ matrix.arch }}", "\"${ARCH}\""] {
        assert_problems(
            &run(&with_flag("--arch amd64", &format!("--arch {value}"))),
            &[],
        );
    }
    // A literal value the parser accepts, and a flag written with `=`.
    assert_problems(&run(&with_flag("--arch amd64", "--arch=arm64")), &[]);
}

#[test]
fn an_alternate_value_expansion_is_checked_both_unset_and_set() {
    // Catches: `${FAIL_READY:+--fail-ready}` read as one unknown argument, and a flag that
    // only appears when its variable is set going unchecked.
    let call = "cargo xtask release targets --service hello ${FAIL_READY:+--fail-ready} --version \"$RELEASE\" --dir \"$RUNNER_TEMP/release\" --repo \"$GITHUB_REPOSITORY\" --output \"$GITHUB_OUTPUT\"";
    assert_problems(&run(call), &[]);
    assert_problems(
        &run(&call.replace(":+--fail-ready", "+--fail-red")),
        &[
            "line 16: `cargo xtask release targets` (with `${FAIL_READY+--fail-red}` as `--fail-red`): unexpected argument found: `--fail-red`",
        ],
    );
    // Set, the expansion's words are split, and a required flag left out is still found unset.
    let nfpm = format!("{NFPM_STAGE} ${{B:+--binaries target/static}}");
    assert_problems(
        &run(&nfpm),
        &missing_binaries(16).each_ref().map(String::as_str),
    );
    // A `${...}` that is not an alternate value is a value, as any word with `$` is.
    assert_problems(
        &run(&with_flag("--arch amd64", "--arch ${ARCH:-amd64}")),
        &[],
    );
    assert_problems(
        &run("cargo xtask release index ${1+x}"),
        &["line 16: `cargo xtask release index`: unexpected argument found: `${1+x}`"],
    );
}

/// The nfpm-stage call with `--binaries`, and the flag `from` written as `to`.
fn with_flag(from: &str, to: &str) -> String {
    let call = format!("{NFPM_STAGE} --binaries target/static");
    assert_eq!(call.matches(from).count(), 1, "{from}");
    call.replacen(from, to, 1)
}

#[test]
fn a_call_is_found_in_every_shape_a_script_can_hold_it() {
    // Catches: a call the lint does not see because it is the binary's path, follows another
    // command or `time`, has cargo's toolchain or options before the alias, is inside `$( )`, runs in the background, spans lines by `\`, has its output redirected, or is in a
    // block script; and a redirection's file read as an argument.
    let shapes: [(usize, String); 11] = [
        (
            16,
            run(&NFPM_STAGE.replace("cargo xtask", "target/debug/xtask")),
        ),
        (
            16,
            run(&NFPM_STAGE.replace("cargo xtask", "RUST_LOG=debug ./target/debug/xtask")),
        ),
        (16, run(&format!("mkdir -p target/nfpm && {NFPM_STAGE}"))),
        (16, run(&format!("true; {NFPM_STAGE} || exit 1"))),
        (16, run(&format!("staged=$({NFPM_STAGE})"))),
        (16, run(&format!("time {NFPM_STAGE} &"))),
        (
            16,
            run(&NFPM_STAGE.replace("cargo xtask", "cargo \"+$TOOLCHAIN\" --locked xtask")),
        ),
        (16, run(&format!("{NFPM_STAGE} > \"$GITHUB_OUTPUT\" 2>&1"))),
        (16, run(&format!("{NFPM_STAGE} 2> err.txt"))),
        (
            17,
            block(&["mkdir -p target/nfpm", NFPM_STAGE, "ls target/nfpm"]),
        ),
        (
            17,
            block(&[
                "cargo xtask release nfpm-stage --arch amd64 \\",
                "  --gh-version \"$GH_VERSION\" --gh-archive \"$RUNNER_TEMP/gh.tar.gz\" \\",
                "  --gh-sha256 \"$GH_SHA256\" --trusted-root \"$RUNNER_TEMP/trusted_root.json\" --out target/nfpm",
            ]),
        ),
    ];
    for (line, text) in shapes {
        let want = missing_binaries(line);
        assert_problems(&text, &want.each_ref().map(String::as_str));
    }
    // The redirection's file after a complete call is no argument of it.
    let staged = format!("{NFPM_STAGE} --binaries target/static > \"$GITHUB_OUTPUT\"");
    assert_problems(&run(&staged), &[]);
}

#[test]
fn text_that_only_names_xtask_is_not_a_call() {
    // Catches: a rule so loose it refuses a comment, a quoted message, a cargo command of the
    // crate, or a word that only ends in xtask.
    let cases = [
        "cargo test -p xtask --test workflows".to_owned(),
        "echo 'cargo xtask release nfpm-stage'".to_owned(),
        "cargo build -p xtask && ls crates/xtask".to_owned(),
        "./run-xtask release".to_owned(),
    ];
    for call in cases {
        assert_problems(&run(&call), &[]);
    }
    assert_problems(
        &block(&[
            "# cargo xtask release nfpm-stage, below with every flag",
            "true",
        ]),
        &[],
    );
    // A `#` inside a word starts no comment: the call after it is still read.
    assert_problems(
        &block(&["echo a#b; cargo xtask release nfpm-stag"]),
        &["line 17: `cargo xtask release`: unrecognized subcommand: `nfpm-stag`"],
    );
}

#[test]
fn quotes_and_backslashes_are_removed_as_the_shell_removes_them() {
    // Catches: a word split at a quoted space or inside `${ }`, or a quote or backslash kept in a word, so a
    // good call is refused or a call's verb is misread.
    let found = commands("a \"b c\" 'd e' f\\ g \"h\\\"i\" ''\nj\\\nk 3>x l");
    assert_eq!(
        found,
        [vec!["a", "b c", "d e", "f g", "h\"i", ""], vec!["jk", "l"],]
    );
    let found = commands("x ${{ matrix.a }} \"${{ b }}\" (y) z>&2 w");
    assert_eq!(
        found,
        [
            vec!["x", "${{ matrix.a }}", "${{ b }}"],
            vec!["y"],
            vec!["z", "w"]
        ]
    );
    // A parameter expansion is one word, its spaces and nested braces included.
    let found = commands("x ${A:+a b} ${B:-${C}} y");
    assert_eq!(found, [vec!["x", "${A:+a b}", "${B:-${C}}", "y"]]);
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        xtask_args(&words(&["X=1", "cargo", "xtask", "release"])),
        Some(&words(&["release"])[..])
    );
    assert_eq!(xtask_args(&words(&["cargo", "test", "-p", "xtask"])), None);
}

#[test]
fn every_xtask_call_in_a_run_of_the_repository_is_one_the_lint_reads() {
    // Catches: a call of a workflow of this repository that the lint skips, so the rule above
    // never judges it: each line of a `run:` that names `xtask` outside a comment is a call it
    // finds.
    let mut checked = 0;
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("the workflow parses");
        let mut runs = Vec::new();
        collect_runs(&doc, &mut runs);
        for (line, script) in runs {
            // Every word `xtask` or `.../xtask` outside a comment, but the name of the crate
            // after `-p`.
            let named = script
                .replace("\\\n", " ")
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .map(|l| {
                    let w: Vec<&str> = l
                        .split_whitespace()
                        .map(|w| w.trim_matches(['"', '\'']))
                        .collect();
                    (0..w.len())
                        .filter(|&i| w[i] == "xtask" || w[i].ends_with("/xtask"))
                        .filter(|&i| i == 0 || !matches!(w[i - 1], "-p" | "--package"))
                        .count()
                })
                .sum::<usize>();
            let found = commands(script)
                .iter()
                .filter(|c| xtask_args(c).is_some())
                .count();
            assert_eq!(named, found, "{file} line {line}: {script}");
            checked += found;
        }
    }
    assert!(checked > 0, "no workflow calls xtask");
}
