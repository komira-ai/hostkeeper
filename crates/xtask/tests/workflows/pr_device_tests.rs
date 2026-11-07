//! The rules of `pr-device.yml` ([`super::pr_device`]) and the pull-request rules of
//! [`super::lint`] that it is the one exception to. Each rule is shown to refuse its defect by
//! putting it into the committed file, which passes, and reading the exact problems it reports.

use super::lint::{PRIVILEGED_WORKFLOW, entries, entry, lint};
use super::pr_device::{
    CONCURRENCY_PROBLEM, FILE, GATE_IF, GROUP, JOBS, LINUX_IF, MAC_IF, ON_PROBLEM, mirror_problems,
    same,
};
use super::yaml::{self, Node};
use super::{CLEAN, with, with_in, workflows};

/// The committed text of `file`.
fn committed(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{file} is in the repository"))
}

fn device() -> String {
    committed(FILE)
}

fn problems(text: &str) -> Vec<String> {
    lint(FILE, text)
}

/// The committed file with `from` (once in it) replaced by `to`.
fn device_with(from: &str, to: &str) -> String {
    with_in(&device(), from, to)
}

/// The start of the job `macos-test`, once in the file.
const MACOS_TEST: &str = "  macos-test:\n    needs: changes\n";
/// The start of the job `gate`, once in the file.
const GATE: &str = "  gate:\n    needs: [changes, macos-test, macos-device, privileged]\n";

#[test]
fn the_committed_pr_device_yml_passes_and_its_jobs_are_the_listed_ones() {
    // Catches: the workflow failing a rule; a job of it that is not in the lint's list (or one
    // of the list missing); the `if:` of a job reading an output `changes` does not set; and a
    // diff step that drops a renamed file's old path or reads paths split on newlines.
    let text = device();
    assert_eq!(problems(&text), Vec::<String>::new());
    assert_eq!(
        mirror_problems(&text, &committed(PRIVILEGED_WORKFLOW)),
        Vec::<String>::new()
    );
    let doc = yaml::load(&text).expect("pr-device.yml parses");
    assert_eq!(
        entry(&doc, "name").and_then(Node::as_str),
        Some("pr-device")
    );
    let jobs = entry(&doc, "jobs").map_or(&[][..], entries);
    let names: Vec<&str> = jobs.iter().filter_map(|(n, _)| n.as_str()).collect();
    let listed: Vec<&str> = JOBS.iter().map(|j| j.name).collect();
    assert_eq!(names, listed);
    let changes = entry(entry(&doc, "jobs").expect("jobs"), "changes").expect("changes");
    let outputs = entry(changes, "outputs").expect("outputs of changes");
    for (output, condition) in [("mac", MAC_IF), ("linux-privileged", LINUX_IF)] {
        assert_eq!(
            entry(outputs, output).and_then(Node::as_str),
            Some(format!("${{{{ steps.pr-jobs.outputs.{output} }}}}").as_str())
        );
        assert_eq!(
            condition,
            format!("${{{{ needs.changes.outputs.{output} == 'true' }}}}")
        );
    }
    for step in [
        "run: git diff --name-only --no-renames -z --output=\"$RUNNER_TEMP/changed\" HEAD^1 HEAD\n",
        "run: cargo xtask pr-jobs --changed \"$RUNNER_TEMP/changed\" --output \"$GITHUB_OUTPUT\"\n",
        "fetch-depth: 2\n",
        "FAILED: ${{ contains(needs.*.result, 'failure') || contains(needs.*.result, 'cancelled') }}\n",
        "run: test \"$FAILED\" = false\n",
    ] {
        assert_eq!(text.matches(step).count(), 1, "{step}");
    }
}

#[test]
fn a_macos_job_with_a_secret_on_a_pull_request_is_refused() {
    // Catches: a secret or the token handed to a macOS job of a pull request, as an expression in
    // its env, a step's `with:` or a run line, in any case, through `toJSON(secrets)`, or passed
    // with a `secrets:` key; and a secret set once for every job beside `jobs`.
    let cases: [(&str, &str); 6] = [
        (
            "    env:\n      TOKEN: ${{ secrets.DEPLOY_KEY }}\n",
            "job macos-test: a secret on pull_request (`${{ secrets.DEPLOY_KEY }}`)",
        ),
        (
            "    env:\n      TOKEN: ${{ github.token }}\n",
            "job macos-test: a secret on pull_request (`${{ github.token }}`)",
        ),
        (
            "    env:\n      TOKEN: ${{ SECRETS['DEPLOY_KEY'] }}\n",
            "job macos-test: a secret on pull_request (`${{ SECRETS['DEPLOY_KEY'] }}`)",
        ),
        (
            "    env:\n      ALL: ${{ toJSON(secrets) }}\n",
            "job macos-test: a secret on pull_request (`${{ toJSON(secrets) }}`)",
        ),
        (
            "    env:\n      TOKEN: a-${{ GitHub.Token }}-b\n",
            "job macos-test: a secret on pull_request (`a-${{ GitHub.Token }}-b`)",
        ),
        (
            "    secrets:\n      key: x\n",
            "job macos-test: a secret on pull_request (`secrets`)",
        ),
    ];
    for (added, want) in cases {
        let text = device_with(MACOS_TEST, &format!("{MACOS_TEST}{added}"));
        assert_eq!(problems(&text), [want], "{added}");
    }
    // In the device job, in a step's `with:`.
    let text = device_with(
        "          persist-credentials: false\n      # The system the tests run against",
        "          persist-credentials: false\n          token: ${{ secrets.GITHUB_TOKEN }}\n      # The system the tests run against",
    );
    assert_eq!(
        problems(&text),
        ["job macos-device: a secret on pull_request (`${{ secrets.GITHUB_TOKEN }}`)"]
    );
    // Beside `jobs`, for every job.
    let text = device_with(
        "  CARGO_TERM_COLOR: always\n",
        "  CARGO_TERM_COLOR: always\n  KEY: ${{ secrets.KEY }}\n",
    );
    assert_eq!(
        problems(&text),
        ["workflow: a secret on pull_request (`${{ secrets.KEY }}`)"]
    );
}

#[test]
fn the_secret_rule_holds_in_every_pull_request_workflow_and_reads_only_expressions() {
    // Catches: the rule held to pr-device.yml alone, so ci.yml could hand its jobs a secret; a
    // step name or text that says "secrets" outside an expression refused; and the rule holding
    // where no pull request runs (a push-only workflow may use the token).
    let step = "      - run: cargo test --workspace --locked\n";
    let secret = "      - run: echo ${{ secrets.X }}\n";
    assert_eq!(
        lint("ci.yml", &with(step, secret)),
        ["job test: a secret on pull_request (`echo ${{ secrets.X }}`)"]
    );
    assert_eq!(
        lint("ci.yml", &with_in(&super::push_only(), step, secret)),
        Vec::<String>::new()
    );
    for innocent in [
        "      - name: Hold no secrets here\n        run: cargo test\n",
        "      - run: cargo test secrets_are_redacted\n",
        "      - run: echo ${{ github.sha }} ${{ github.token_kind }}x\n",
        "      - run: echo ${{ inputs.secretsless }}\n",
    ] {
        assert_eq!(
            lint("ci.yml", &with(step, innocent)),
            Vec::<String>::new(),
            "{innocent}"
        );
    }
    // An expression left open runs to the end of the value, and still counts.
    assert_eq!(
        lint("ci.yml", &with(step, "      - run: echo ${{ secrets.X\n")),
        ["job test: a secret on pull_request (`echo ${{ secrets.X`)"]
    );
}

#[test]
fn a_write_scope_or_id_token_on_a_pr_device_job_is_refused() {
    // Catches: an OIDC token or a write scope granted to a job that runs pull-request code as
    // root on a macOS runner, which the exception for root and macOS must not let through.
    let read = "    runs-on: macos-15\n    permissions:\n      contents: read\n    # A bound on the whole job, against a test that hangs; no test depends on it.\n    timeout-minutes: 45\n";
    for (scope, wants) in [
        (
            "id-token",
            "job macos-device: `id-token: write` is not allowed in pr-device.yml",
        ),
        (
            "contents",
            "job macos-device: `contents: write` is not allowed in pr-device.yml",
        ),
    ] {
        let text = device_with(
            read,
            &read.replace("contents: read", &format!("{scope}: write")),
        );
        assert_eq!(
            problems(&text),
            [
                wants.to_owned(),
                format!("job macos-device: privileged on pull_request (`{scope}: write`)"),
            ]
        );
    }
}

#[test]
fn root_and_macos_on_a_pull_request_are_allowed_only_in_the_jobs_listed_for_them() {
    // Catches: the exception reaching a job it does not name: root in the macOS test job or the
    // gate, a macOS runner for the job that reads the paths, and a job named like a listed one in
    // another workflow (ci.yml), which is held to the rule in full.
    let sudo = "      - name: Run the workspace's tests\n        run: cargo test --workspace --locked --no-fail-fast\n";
    let text = device_with(sudo, &sudo.replace("run: cargo", "run: sudo cargo"));
    assert_eq!(
        problems(&text),
        [
            "job macos-test: privileged on pull_request (`sudo cargo test --workspace --locked --no-fail-fast`)"
        ]
    );
    let gate = "        run: test \"$FAILED\" = false\n";
    let text = device_with(gate, "        run: sudo test \"$FAILED\" = false\n");
    assert_eq!(
        problems(&text),
        ["job gate: privileged on pull_request (`sudo test \"$FAILED\" = false`)"]
    );
    let changes = "  changes:\n    runs-on: ubuntu-24.04\n";
    let text = device_with(changes, "  changes:\n    runs-on: macos-15\n");
    assert_eq!(
        problems(&text),
        [
            "job changes: macOS on pull_request (`macos-15`)",
            "job changes: runs on a runner outside [\"ubuntu-24.04\"]",
        ]
    );
    let in_ci = with(
        "  test:\n    runs-on: ubuntu-latest\n",
        "  macos-device:\n    runs-on: macos-15\n",
    );
    let in_ci = with_in(
        &in_ci,
        "      - run: cargo test --workspace --locked\n",
        "      - run: sudo cargo test\n",
    );
    assert_eq!(
        lint("ci.yml", &in_ci),
        [
            "job macos-device: privileged on pull_request (`sudo cargo test`)",
            "job macos-device: macOS on pull_request (`macos-15`)",
        ]
    );
    // The job names `needs` lists are not values of the job: the gate needs the macOS jobs.
    assert_eq!(problems(&device()), Vec::<String>::new());
    assert!(device().contains(GATE));
}

#[test]
fn the_trigger_of_pr_device_yml_is_pull_request_alone_with_no_filter() {
    // Catches: a paths or branches filter on `on`, which leaves the gate unreported (a required
    // check that never starts), a push trigger that would run the copies again after the merge,
    // and a `types` list that skips the pushes to an open pull request.
    let on = "on:\n  pull_request:\n\npermissions";
    for changed in [
        "on:\n  pull_request:\n    paths: ['crates/hostkeeper-launchd/**']\n\npermissions",
        "on:\n  pull_request:\n    branches: [main]\n\npermissions",
        "on:\n  pull_request:\n    types: [opened]\n\npermissions",
        "on:\n  pull_request:\n  push:\n    branches: [main]\n\npermissions",
        "on:\n  workflow_dispatch:\n\npermissions",
        "on:\n  pull_request: x\n\npermissions",
        "on:\n  pull_request: [x]\n\npermissions",
        "on: [pull_request, push]\n\npermissions",
    ] {
        let found = problems(&device_with(on, changed));
        assert!(
            found.contains(&ON_PROBLEM.to_owned()),
            "{changed}: {found:?}"
        );
    }
    for same in [
        "on: pull_request\n\npermissions",
        "on: [pull_request]\n\npermissions",
        "on:\n  pull_request: {}\n\npermissions",
    ] {
        assert_eq!(
            problems(&device_with(on, same)),
            Vec::<String>::new(),
            "{same}"
        );
    }
    let doc = yaml::load(&device()).expect("parses");
    assert!(entry(&doc, "on").is_some());
    // With no `on` it runs for no pull request, and its copies' root is refused as outside
    // privileged.yml.
    let none = problems(&device_with("on:\n  pull_request:\n\n", ""));
    assert_eq!(none.len(), 3, "{none:?}");
    assert!(none[0].starts_with("job macos-device: privileged outside privileged.yml"));
    assert!(none[1].starts_with("job privileged: privileged outside privileged.yml"));
    assert_eq!(none[2], ON_PROBLEM);
}

#[test]
fn the_concurrency_of_pr_device_yml_is_one_group_per_pull_request_that_cancels() {
    // Catches: superseded runs left running (and holding macOS runners) after a newer push, a
    // group shared by every pull request (which cancels another pull request's run), and a
    // workflow with no concurrency at all.
    let block = format!("concurrency:\n  group: {GROUP}\n  cancel-in-progress: true\n");
    assert!(device().contains(&block));
    for changed in [
        format!("concurrency:\n  group: {GROUP}\n  cancel-in-progress: false\n"),
        "concurrency:\n  group: pr-device\n  cancel-in-progress: true\n".to_owned(),
        format!("concurrency:\n  group: {GROUP}\n"),
        format!("concurrency:\n  group: {GROUP}\n  cancel-in-progress: true\n  x: y\n"),
        "concurrency: pr-device\n".to_owned(),
        String::new(),
    ] {
        assert_eq!(
            problems(&device_with(&block, &changed)),
            [CONCURRENCY_PROBLEM],
            "{changed}"
        );
    }
}

#[test]
fn each_job_of_pr_device_yml_has_its_needs_its_if_and_its_runner() {
    // Catches: a macOS job that runs for every pull request (no `if:`, or another one), one that
    // starts before `changes` has said, the gate not waiting for a job (so it passes before that
    // job fails) or skipped when one failed, a job the list does not name, one missing, and a job
    // on another runner.
    let mac_if = format!(
        "    if: {MAC_IF}\n    runs-on: macos-15\n    permissions:\n      contents: read\n    # A bound on the whole job, against a test that hangs; no test depends on it.\n    timeout-minutes: 60\n"
    );
    let cases: [(&str, String, Vec<String>); 9] = [
        (
            MACOS_TEST,
            "  macos-test:\n".to_owned(),
            vec!["job macos-test: its `needs` must be exactly {\"changes\"}".to_owned()],
        ),
        (
            &mac_if,
            mac_if.replace(&format!("    if: {MAC_IF}\n"), ""),
            vec![format!("job macos-test: its `if:` must be exactly `{MAC_IF}`")],
        ),
        (
            &mac_if,
            mac_if.replace(MAC_IF, LINUX_IF),
            vec![format!("job macos-test: its `if:` must be exactly `{MAC_IF}`")],
        ),
        (
            &mac_if,
            mac_if.replace("macos-15", "macos-14"),
            vec!["job macos-test: runs on a runner outside [\"macos-15\"]".to_owned()],
        ),
        (
            GATE,
            "  gate:\n    needs: [changes, macos-test, macos-device]\n".to_owned(),
            vec![
                "job gate: its `needs` must be exactly {\"changes\", \"macos-device\", \"macos-test\", \"privileged\"}"
                    .to_owned(),
            ],
        ),
        (
            &format!("    if: {GATE_IF}\n"),
            "    if: ${{ success() }}\n".to_owned(),
            vec![format!("job gate: its `if:` must be exactly `{GATE_IF}`")],
        ),
        (
            "  changes:\n    runs-on: ubuntu-24.04\n",
            "  changes:\n    if: ${{ github.actor != 'x' }}\n    needs: gate\n    runs-on: ubuntu-24.04\n"
                .to_owned(),
            vec![
                "job changes: has an `if:`, and runs for every pull request".to_owned(),
                "job changes: needs no job".to_owned(),
            ],
        ),
        (
            GATE,
            format!("  extra:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: cargo test\n{GATE}"),
            vec!["job extra is not a job of pr-device.yml".to_owned()],
        ),
        (
            "  changes:\n    runs-on: ubuntu-24.04\n",
            "  changes:\n    runs-on: [ubuntu-24.04]\n".to_owned(),
            vec!["job changes: runs on a runner outside [\"ubuntu-24.04\"]".to_owned()],
        ),
    ];
    for (from, to, want) in cases {
        assert_eq!(problems(&device_with(from, &to)), want, "{to}");
    }
    // A job of the list missing: the gate, removed with every line after its start.
    let text = device();
    let at = text.find(GATE).expect("the gate");
    assert_eq!(problems(&text[..at]), ["no job gate"]);
}

#[test]
fn a_copy_that_drifted_from_privileged_yml_is_refused() {
    // Catches: a pull request running other device tests than run after the merge: a step, a
    // runner, an env or a test target changed in the copy or in privileged.yml alone, a copy
    // missing, and a top-level env (RUSTFLAGS) that differs.
    let privileged = committed(PRIVILEGED_WORKFLOW);
    let drift = |job: &str| {
        format!("job {job}: not privileged.yml's job {job} with `needs` and `if:` added")
    };
    let device_mac = "--test device_mac --test device_mac_e2e --test device_mac_selfupdate --test unpack_mac -- --test-threads=1 --nocapture";
    let changed = device_with(
        device_mac,
        "--test device_mac -- --test-threads=1 --nocapture",
    );
    assert_eq!(
        mirror_problems(&changed, &privileged),
        [drift("macos-device")]
    );
    let linux = "    timeout-minutes: 60\n    env:\n      GH_VERSION: 2.102.1\n";
    let mut copy = device_with(linux, &linux.replace("2.102.1", "2.102.2"));
    assert_eq!(mirror_problems(&copy, &privileged), [drift("privileged")]);
    let original = with_in(&privileged, linux, &linux.replace("2.102.1", "2.102.2"));
    assert_eq!(mirror_problems(&copy, &original), Vec::<String>::new());
    copy = device_with("  RUSTFLAGS: -D warnings\n", "");
    assert_eq!(
        mirror_problems(&copy, &privileged),
        ["pr-device.yml: its top-level env is not privileged.yml's"]
    );
    let no_env = |text: &str| {
        let at = text.find("\nenv:\n").expect("env");
        let end = text[at + 1..].find("\n\n").expect("end of env") + at + 1;
        format!("{}{}", &text[..at + 1], &text[end + 1..])
    };
    assert_eq!(
        mirror_problems(&no_env(&device()), &no_env(&privileged)),
        Vec::<String>::new()
    );
    assert_eq!(
        mirror_problems(&no_env(&device()), &privileged),
        ["pr-device.yml: its top-level env is not privileged.yml's"]
    );
    let at = device()
        .find("  # privileged.yml's job privileged,")
        .expect("the copy");
    let end = device()
        .find("  # The one check of this workflow")
        .expect("the gate");
    let without = format!("{}{}", &device()[..at], &device()[end..]);
    assert_eq!(
        mirror_problems(&without, &privileged),
        [drift("privileged")]
    );
    assert_eq!(
        mirror_problems("on: [unclosed\n", &privileged).len(),
        1,
        "a file that does not parse is one problem"
    );
    assert_eq!(mirror_problems(&device(), "").len(), 1);
}

#[test]
fn same_compares_trees_not_where_they_were_written() {
    // Catches: a comparison that passes a quoted scalar for a plain one, a list for a mapping,
    // a shorter list or mapping, or one that fails two equal trees written on other lines.
    let load = |text: &str| yaml::load(text).expect("parses");
    assert!(same(&load("a: [1, 2]\n"), &load("\n\na:\n  - 1\n  - 2\n")));
    for (a, b) in [
        ("a: 1\n", "a: '1'\n"),
        ("a: [1]\n", "a: {b: 1}\n"),
        ("a: [1, 2]\n", "a: [1]\n"),
        ("a: 1\nb: 2\n", "a: 1\n"),
        ("a: 1\n", "b: 1\n"),
        ("a: 1\n", "a: 2\n"),
    ] {
        assert!(!same(&load(a), &load(b)), "{a} against {b}");
    }
    assert!(CLEAN.contains("pull_request"));
}
