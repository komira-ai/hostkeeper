//! The rule of [`super::release_ref`]: the repository's workflows pass; the job `channel` of
//! `release.yml` as it was, which ran `release channel-host` from the checkout of a release
//! older than the verb, is refused; and each defect is shown refused, by the exact problem, in
//! a small workflow of one job.

// clap's command line, named apart from `std::process::Command` (which no test here starts).
use clap::{Arg, ArgAction, Command as Cli};

use super::release_ref::{FLOOR, SURFACE_FILE, floor_problem, problems, surface};
use super::{workflows, yaml};

/// A checkout of the commit of the release a run publishes.
const RELEASE_CHECKOUT: &str = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          ref: ${{ needs.newest.outputs.commit }}\n";

/// A checkout of the workflow's own commit.
const OWN_CHECKOUT: &str =
    "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n";

/// A call of a verb release 0.1.208 does not have.
const NEW_VERB: &str = "cargo xtask release channel-host --channel \"$C\" --output \"$O\"";

/// The problems of a workflow of one job `j` whose steps are `steps`.
fn job_problems(steps: &str) -> Vec<String> {
    let text = format!("jobs:\n  j:\n    runs-on: ubuntu-24.04\n    steps:\n{steps}");
    let doc = yaml::load(&text).expect("the workflow parses");
    problems("t.yml", &doc)
}

/// A step that runs `command`.
fn run(command: &str) -> String {
    format!("      - run: {command}\n")
}

/// The problem of a call of step `n` on `line` under [`RELEASE_CHECKOUT`].
fn refused(n: usize, line: usize, why: &str) -> String {
    format!(
        "t.yml: job j step {n} (line {line}) runs the xtask of `${{{{ needs.newest.outputs.commit }}}}`, which may be as old as release {}: {why}",
        FLOOR.0
    )
}

/// What [`floor_problem`] says of the words of `command` after `cargo xtask`.
fn floor(command: &str) -> Option<String> {
    let args: Vec<String> = command.split(' ').map(str::to_owned).collect();
    floor_problem(SURFACE_FILE, &args)
}

/// The repository's `release.yml`.
fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// `text` (the repository's `release.yml`) with its job `channel` as it was before its host step
/// ran from a checkout of the workflow's own commit: no such checkout, and the host step after
/// the release's checkout and download, right before the join.
pub fn before_the_fix(text: &str) -> String {
    let own = text
        .find("      # Only when HOSTKEEPER_TAILNET_CLIENT_ID is set: the channel's host, by the xtask")
        .expect("the own checkout");
    let release = text
        .find("      # The xtask of the release's own commit")
        .expect("the release checkout");
    let host = &text[own..release];
    let step = &host[host
        .find("      - name: The release channel's host\n")
        .expect("the host step")..];
    let join = "      - name: Join the tailnet\n";
    let without = format!("{}{}", &text[..own], &text[release..]);
    let moved = without.replacen(join, &format!("{step}{join}"), 1);
    assert_ne!(moved, without, "the join follows");
    moved
}

#[test]
fn the_repository_workflows_call_from_a_release_checkout_only_what_the_floor_has() {
    // Catches: a job of a workflow that runs, from the checkout of a release's commit, a verb or
    // flag the oldest commit a release run can check out does not have.
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("every workflow parses");
        assert_eq!(problems(&file, &doc), Vec::<String>::new(), "{file}");
    }
}

#[test]
fn the_channel_job_that_ran_channel_host_from_the_release_checkout_is_refused() {
    // Catches: the channel job as it ran, failing with `unrecognized subcommand 'channel-host'`
    // when the run released a commit older than the verb; the fix runs the verb from a
    // checkout of the workflow's own commit.
    let text = before_the_fix(&release_yml());
    let doc = yaml::load(&text).expect("release.yml parses");
    let line = text
        .lines()
        .position(|l| l.contains("run: cargo xtask release channel-host"))
        .map(|at| at + 1)
        .expect("the host step");
    assert_eq!(
        problems("release.yml", &doc),
        [format!(
            "release.yml: job channel step 6 (line {line}) runs the xtask of `${{{{ steps.newest.outputs.commit }}}}`, which may be as old as release {}: `cargo xtask release channel-host` is not a verb of release {}",
            FLOOR.0, FLOOR.0
        )]
    );
}

#[test]
fn a_verb_newer_than_the_floor_runs_only_from_the_workflows_own_commit() {
    // Catches: a call of a verb the floor lacks, under the checkout of a release's commit; and
    // the rule refusing the same call from the workflow's own commit, by no `ref`, by
    // `ref: ${{ github.sha }}`, or before any checkout (no xtask of another commit runs there).
    assert_eq!(
        job_problems(&format!("{RELEASE_CHECKOUT}{}", run(NEW_VERB))),
        [refused(
            2,
            8,
            "`cargo xtask release channel-host` is not a verb of release 0.1.208"
        )]
    );
    let sha = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          ref: ${{ github.sha }}\n";
    for steps in [
        format!("{OWN_CHECKOUT}{}", run(NEW_VERB)),
        format!("{sha}{}", run(NEW_VERB)),
        run(NEW_VERB),
    ] {
        assert_eq!(job_problems(&steps), Vec::<String>::new(), "{steps}");
    }
}

#[test]
fn the_last_checkout_before_a_call_decides_which_xtask_runs_it() {
    // Catches: a rule that reads only the job's first checkout, or any checkout of the job.
    assert_eq!(
        job_problems(&format!(
            "{RELEASE_CHECKOUT}{OWN_CHECKOUT}{}",
            run(NEW_VERB)
        )),
        Vec::<String>::new()
    );
    assert_eq!(
        job_problems(&format!(
            "{OWN_CHECKOUT}{}{RELEASE_CHECKOUT}{}",
            run(NEW_VERB),
            run(NEW_VERB)
        )),
        [refused(
            4,
            10,
            "`cargo xtask release channel-host` is not a verb of release 0.1.208"
        )]
    );
}

#[test]
fn a_call_the_floors_command_line_would_refuse_is_refused() {
    // Catches: a flag added after the floor, a required flag of the floor left out, and too many
    // or too few arguments for the floor's verb, each refused by name.
    let cases = [
        (
            "release targets --version v --dir d --repo r --new-flag",
            "`cargo xtask release targets` of release 0.1.208 has no flag `--new-flag`",
        ),
        (
            "release check dist --commit c",
            "`cargo xtask release check` of release 0.1.208 requires `--version`",
        ),
        (
            "release check a b --version v --commit c",
            "`cargo xtask release check` of release 0.1.208 takes 1 to 1 arguments, not 2",
        ),
        (
            "release check-binary a b",
            "`cargo xtask release check-binary` of release 0.1.208 takes 3 to 3 arguments, not 2",
        ),
        (
            "db-backup --into x --keep 3",
            "`cargo xtask db-backup` of release 0.1.208 takes 1 to any number of arguments, not 0",
        ),
        (
            "release",
            "`cargo xtask release` is not a verb of release 0.1.208",
        ),
        (
            "release no-such --flag",
            "`cargo xtask release no-such` is not a verb of release 0.1.208",
        ),
    ];
    for (call, why) in cases {
        assert_eq!(floor(call).as_deref(), Some(why), "{call}");
    }
    assert_eq!(
        job_problems(&format!(
            "{RELEASE_CHECKOUT}{}",
            run("cargo xtask release check-binary a b")
        )),
        [refused(
            2,
            8,
            "`cargo xtask release check-binary` of release 0.1.208 takes 3 to 3 arguments, not 2"
        )]
    );
}

#[test]
fn a_call_the_floor_accepts_passes() {
    // Catches: a rule that counts a flag's value as an argument, refuses `--flag=value`, a `-`
    // argument, a verb of many arguments, a call that only prints the help, or a verb of a
    // nested group.
    for call in [
        "release check dist --version v --commit c",
        "release check --version=v --commit=c dist",
        "release check - --version v --commit c",
        "release targets --service hello --fail-ready --version v --dir d --repo r --output o",
        "db-backup --into x --keep 3 a b c",
        "release channel-host --help",
        "release channel-host -h",
        "channels apt read --bucket b --out o",
        "--help",
    ] {
        assert_eq!(floor(call), None, "{call}");
    }
}

#[test]
fn each_way_an_alternate_value_expansion_reaches_the_floor_is_checked() {
    // Catches: a flag newer than the floor hidden in `${NAME:+--flag}`, which reaches the binary
    // only when the variable is set.
    let base = "release targets --version v --dir d --repo r";
    assert_eq!(
        floor(&format!("{base} ${{FAIL_READY:+--fail-ready}}")),
        None
    );
    assert_eq!(
        floor(&format!("{base} ${{NEW:+--new-flag}}")).as_deref(),
        Some("`cargo xtask release targets` of release 0.1.208 has no flag `--new-flag`")
    );
}

#[test]
fn every_form_of_a_call_of_xtask_is_held_to_the_floor() {
    // Catches: a call of the binary the checkout built by its path, or in a container that
    // mounts it, left out of the rule.
    for command in [
        "target/debug/xtask release channel-host --channel c",
        "docker run --rm --volume \"$PWD/target/debug/xtask:/usr/local/bin/xtask:ro\" ubuntu@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852 xtask release channel-host --channel c",
        "echo start && cargo xtask release channel-host --channel c",
    ] {
        assert_eq!(
            job_problems(&format!("{RELEASE_CHECKOUT}{}", run(command))),
            [refused(
                2,
                8,
                "`cargo xtask release channel-host` is not a verb of release 0.1.208"
            )],
            "{command}"
        );
    }
}

#[test]
fn a_checkout_the_rule_cannot_read_in_a_job_that_calls_xtask_is_refused() {
    // Catches: a checkout of another repository or into a path, after which a call's xtask is
    // not the checkout's the rule reads; and the same checkout refused in a job with no call.
    let other = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          repository: komira-ai/tierstore\n";
    let path = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          path: tool\n";
    for checkout in [other, path] {
        assert_eq!(
            job_problems(&format!("{RELEASE_CHECKOUT}{checkout}{}", run(NEW_VERB))),
            [
                "t.yml: job j step 2 checks out another repository or into a path, in a job that calls xtask"
            ],
            "{checkout}"
        );
        assert_eq!(
            job_problems(&format!("{checkout}{}", run("echo no xtask"))),
            Vec::<String>::new(),
            "{checkout}"
        );
    }
}

#[test]
fn a_workflow_without_jobs_or_a_job_without_steps_has_nothing_to_hold() {
    // Catches: the rule failing on a reusable workflow's caller job, which has no steps.
    for text in [
        "on: push\n",
        "jobs:\n  j:\n    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@2c767d1842a88edb7120073d2dff3b4bfccc834c # v0.1.184\n",
    ] {
        let doc = yaml::load(text).expect("parses");
        assert_eq!(problems("t.yml", &doc), Vec::<String>::new(), "{text}");
    }
}

#[test]
fn surface_writes_each_verb_with_its_flags_and_arguments() {
    // Catches: a surface that leaves out a verb of a nested group, a short flag, whether a flag
    // takes a value or is required, or a positional argument and how many words it takes; and
    // one that lists `help`, which every verb has.
    let cmd = Cli::new("x")
        .subcommand(
            Cli::new("a")
                .arg(Arg::new("req").long("req").required(true))
                .arg(Arg::new("opt").long("opt").action(ArgAction::SetTrue))
                .arg(Arg::new("s").short('s'))
                .arg(Arg::new("p").required(true))
                .arg(Arg::new("rest").num_args(1..)),
        )
        .subcommand(Cli::new("g").subcommand(Cli::new("b")));
    assert_eq!(surface(&cmd), "a --opt --req=! -s= <p>! <rest>...\ng b\n");
    assert_eq!(surface(&Cli::new("x")), "");
    // A short flag of a surface is read as the verb takes it.
    assert_eq!(
        floor_problem(
            "a -s= <p>!\n",
            &["a".into(), "-s".into(), "1".into(), "p".into()]
        ),
        None
    );
}

#[test]
fn the_floors_command_line_is_the_published_release_it_names() {
    // Catches: the recorded command line and its release drifting apart (the floor moved
    // without the file, or the file without the floor), and a file not in `surface`'s order.
    let mut lines = SURFACE_FILE.lines();
    assert_eq!(
        lines.next(),
        Some(
            format!(
                "# The command line of xtask at release {} (commit {}), as release_ref::surface writes it.",
                FLOOR.0, FLOOR.1
            )
            .as_str()
        )
    );
    let body: Vec<&str> = lines.collect();
    let mut sorted = body.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(body, sorted);
    // The verbs the release jobs run are there; the one the floor predates is not.
    assert!(body.iter().any(|l| l.starts_with("release targets ")));
    assert!(!body.iter().any(|l| l.starts_with("release channel-host")));
}
