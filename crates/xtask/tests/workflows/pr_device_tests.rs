//! The rules of `pr-device.yml` ([`super::pr_device`]) and the pull-request rules of
//! [`super::lint`] that it is the one exception to (root, for the copies of privileged.yml's
//! jobs). No pull-request workflow has a macOS job: that rule is shown here, against the committed
//! workflows and against a macOS job added to each of them. Each rule is shown to refuse its
//! defect by putting it into the committed file, which passes, and reading the exact problems it
//! reports.

use super::lint::{
    PRIVILEGED_WORKFLOW, entries, entry, lint, runs_on_pull_requests, scalars, triggers,
};
use super::pr_device::{
    CONCURRENCY_PROBLEM, FILE, GROUP, JOBS, ON_PROBLEM, REQUIRED, TOP_LEVEL, mirror_problems, same,
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

/// The start of the job `privileged`, once in the file.
const PRIVILEGED: &str = "  privileged:\n    runs-on: ubuntu-24.04\n";

/// A job on a macOS runner, as one would be added to a workflow that runs for a pull request.
const MACOS_JOB: &str = "  macos-test:\n    runs-on: macos-15\n    permissions:\n      contents: read\n    steps:\n      - run: cargo test\n";

#[test]
fn the_committed_pr_device_yml_passes_and_its_jobs_are_the_listed_ones() {
    // Catches: the workflow failing a rule; a job of it that is not in the lint's list (or one of
    // the list missing); a copy that drifted from privileged.yml; and a required check that is
    // not the check of one of its jobs.
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
    assert_eq!(names, REQUIRED);
}

#[test]
fn no_workflow_that_runs_for_a_pull_request_runs_a_macos_job() {
    // Catches: a macOS runner, a darwin target or a macOS matrix value in any committed workflow
    // that a pull request starts (the shortage of macOS runners that starved a release): none
    // names macOS anywhere, and the release stage of the Mac's tests (release.yml) is not one.
    let mut pull_request_workflows = Vec::new();
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("a workflow parses");
        let on = entry(&doc, "on").map(triggers).unwrap_or_default();
        if !on.iter().any(|t| runs_on_pull_requests(t)) {
            continue;
        }
        pull_request_workflows.push(file.clone());
        let mut values = Vec::new();
        scalars(&doc, &mut values);
        let mac = values
            .iter()
            .flat_map(|v| v.lines())
            .find(|l| {
                let l = l.to_ascii_lowercase();
                l.contains("macos") || l.contains("darwin")
            })
            // The comments above the jobs are no value of the document.
            .map(str::to_owned);
        assert_eq!(mac, None, "{file} runs for a pull request and names macOS");
    }
    assert_eq!(pull_request_workflows, ["ci.yml", "pr-device.yml"]);
}

#[test]
fn a_macos_job_added_to_a_workflow_that_runs_for_a_pull_request_is_refused() {
    // Catches: a macOS job put back into pr-device.yml (where one used to run on every pull
    // request) or into ci.yml, as a job of its own, as the runner of an existing job or as a matrix
    // value: each is refused as macOS on pull_request, and the new job in pr-device.yml also as
    // a job the workflow does not list.
    let added = device_with(PRIVILEGED, &format!("{MACOS_JOB}\n{PRIVILEGED}"));
    let found = problems(&added);
    for want in [
        "job macos-test: macOS on pull_request (`macos-15`)",
        "job macos-test is not a job of pr-device.yml",
    ] {
        assert!(found.contains(&want.to_owned()), "{want}: {found:?}");
    }
    // The runner of a listed job.
    let swapped = device_with(PRIVILEGED, "  privileged:\n    runs-on: macos-15\n");
    assert_eq!(
        problems(&swapped),
        [
            "job privileged: macOS on pull_request (`macos-15`)",
            "job privileged: runs on a runner outside [\"ubuntu-24.04\"]",
        ]
    );
    // A matrix value and a darwin target, in pr-device.yml.
    let matrix = device_with(
        PRIVILEGED,
        "  privileged:\n    runs-on: ${{ matrix.os }}\n    strategy:\n      matrix:\n        os: [ubuntu-24.04, macos-15]\n",
    );
    assert!(
        problems(&matrix)
            .contains(&"job privileged: macOS on pull_request (`macos-15`)".to_owned()),
        "{:?}",
        problems(&matrix)
    );
    // ci.yml, the pull-request gate: the same.
    let in_ci = with(
        "  test:\n    runs-on: ubuntu-latest\n",
        "  test:\n    runs-on: macos-15\n",
    );
    assert_eq!(
        lint("ci.yml", &in_ci),
        ["job test: macOS on pull_request (`macos-15`)"]
    );
    let darwin = with_in(
        &in_ci,
        "    runs-on: macos-15\n",
        "    runs-on: ubuntu-latest\n",
    );
    let darwin = with_in(
        &darwin,
        "      - run: cargo test --workspace --locked\n",
        "      - run: cargo build --target aarch64-apple-darwin\n",
    );
    assert_eq!(
        lint("ci.yml", &darwin),
        ["job test: macOS on pull_request (`cargo build --target aarch64-apple-darwin`)"]
    );
}

#[test]
fn a_pull_request_job_on_linux_arm64_or_any_other_runner_is_refused() {
    // Catches: a job of a pull-request workflow on linux-arm64 (`ubuntu-24.04-arm`, 22.04-arm), in a
    // matrix, or on any label that is not Linux x86_64, which would hold the few arm and Mac
    // runners as macOS jobs once did; Linux x86_64 images pass. Every committed pull-request
    // workflow runs on Linux x86_64 alone.
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("a workflow parses");
        let on = entry(&doc, "on").map(triggers).unwrap_or_default();
        if !on.iter().any(|t| runs_on_pull_requests(t)) {
            continue;
        }
        let mut values = Vec::new();
        scalars(&doc, &mut values);
        assert!(
            !values
                .iter()
                .any(|v| v.ends_with("-arm") || v.contains("arm64")),
            "{file} runs on arm64 for a pull request"
        );
    }
    let hosted = "  test:\n    runs-on: ubuntu-latest\n";
    for label in ["ubuntu-24.04-arm", "ubuntu-22.04-arm"] {
        assert_eq!(
            lint(
                "ci.yml",
                &with(hosted, &format!("  test:\n    runs-on: {label}\n"))
            ),
            [format!(
                "job test: runner `{label}` on pull_request is not Linux x86_64 (ubuntu-*, not *-arm)"
            )]
        );
    }
    let matrix = "  test:\n    runs-on: ${{ matrix.os }}\n    strategy:\n      matrix:\n        os: [ubuntu-24.04, ubuntu-24.04-arm]\n";
    assert_eq!(
        lint("ci.yml", &with(hosted, matrix)),
        [
            "job test: runner `ubuntu-24.04-arm` on pull_request is not Linux x86_64 (ubuntu-*, not *-arm)"
        ]
    );
    for ok in ["ubuntu-24.04", "ubuntu-22.04", "ubuntu-latest"] {
        assert_eq!(
            lint(
                "ci.yml",
                &with(hosted, &format!("  test:\n    runs-on: {ok}\n"))
            ),
            Vec::<String>::new()
        );
    }
    // The same arm job is fine where no pull request runs (release.yml's check jobs).
    assert_eq!(
        lint(
            "ci.yml",
            &with_in(
                &super::push_only(),
                hosted,
                "  test:\n    runs-on: ubuntu-24.04-arm\n"
            )
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_macos_job_with_a_secret_on_a_pull_request_is_refused() {
    // Catches: a secret or the token handed to a job of a pull request, as an expression in
    // its env, a step's `with:` or a run line, in any case, through `toJSON(secrets)`, or passed
    // with a `secrets:` key; and a secret set once for every job beside `jobs`.
    let cases: [(&str, &str); 9] = [
        (
            "    env:\n      TOKEN: ${{ secrets.DEPLOY_KEY }}\n",
            "job privileged: a secret on pull_request (`${{ secrets.DEPLOY_KEY }}`)",
        ),
        (
            "    env:\n      TOKEN: ${{ github.token }}\n",
            "job privileged: a secret on pull_request (`${{ github.token }}`)",
        ),
        (
            "    env:\n      TOKEN: ${{ SECRETS['DEPLOY_KEY'] }}\n",
            "job privileged: a secret on pull_request (`${{ SECRETS['DEPLOY_KEY'] }}`)",
        ),
        (
            "    env:\n      ALL: ${{ toJSON(secrets) }}\n",
            "job privileged: a secret on pull_request (`${{ toJSON(secrets) }}`)",
        ),
        (
            "    env:\n      TOKEN: a-${{ GitHub.Token }}-b\n",
            "job privileged: a secret on pull_request (`a-${{ GitHub.Token }}-b`)",
        ),
        (
            "    env:\n      TOKEN: ${{ github['token'] }}\n",
            "job privileged: a secret on pull_request (`${{ github['token'] }}`)",
        ),
        (
            "    env:\n      TOKEN: ${{ github [ \"token\" ] }}\n",
            "job privileged: a secret on pull_request (`${{ github [ \"token\" ] }}`)",
        ),
        (
            "    env:\n      ALL: ${{ toJSON( github ) }}\n",
            "job privileged: a secret on pull_request (`${{ toJSON( github ) }}`)",
        ),
        (
            "    secrets:\n      key: x\n",
            "job privileged: a secret on pull_request (`secrets`)",
        ),
    ];
    for (added, want) in cases {
        let text = device_with(PRIVILEGED, &format!("{PRIVILEGED}{added}"));
        assert_eq!(problems(&text), [want], "{added}");
    }
    // In a step's `with:`.
    let text = device_with(
        "          persist-credentials: false\n      # The service manager the tests run against",
        "          persist-credentials: false\n          token: ${{ secrets.GITHUB_TOKEN }}\n      # The service manager the tests run against",
    );
    assert_eq!(
        problems(&text),
        ["job privileged: a secret on pull_request (`${{ secrets.GITHUB_TOKEN }}`)"]
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
fn a_write_scope_or_id_token_on_a_pr_device_job_is_refused() {
    // Catches: an OIDC token or a write scope granted to a job that runs pull-request code as
    // root, which the exception for root must not let through.
    let read = "    runs-on: ubuntu-24.04\n    permissions:\n      contents: read\n    # A bound on the whole job, against a test that hangs; no test depends on it.\n    timeout-minutes: 60\n";
    for (scope, wants) in [
        (
            "id-token",
            "job privileged: `id-token: write` is not allowed in pr-device.yml",
        ),
        (
            "contents",
            "job privileged: `contents: write` is not allowed in pr-device.yml",
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
                format!("job privileged: privileged on pull_request (`{scope}: write`)"),
            ]
        );
    }
}

#[test]
fn root_on_a_pull_request_is_allowed_only_in_the_jobs_listed_for_it() {
    // Catches: the exception reaching a job it does not name: root in a job of ci.yml named like a
    // listed one, which is held to the rule in full.
    let in_ci = with(
        "  test:\n    runs-on: ubuntu-latest\n",
        "  privileged:\n    runs-on: ubuntu-latest\n",
    );
    let in_ci = with_in(
        &in_ci,
        "      - run: cargo test --workspace --locked\n",
        "      - run: sudo cargo test\n",
    );
    assert_eq!(
        lint("ci.yml", &in_ci),
        ["job privileged: privileged on pull_request (`sudo cargo test`)"]
    );
}

#[test]
fn each_job_of_pr_device_yml_runs_on_every_pull_request_on_its_runner() {
    // Catches: a job under a condition, a name or after another job (a required check that then
    // passes, or never reports, without its tests having run); a job the list does not name, one
    // of the list missing, and a job on another runner.
    let cases: Vec<(&str, String, Vec<String>)> = vec![
        (
            PRIVILEGED,
            "  privileged:\n    if: ${{ github.event.pull_request.draft == false }}\n    runs-on: ubuntu-24.04\n".to_owned(),
            vec!["job privileged: has `if`, and must run and report on every pull request".to_owned()],
        ),
        (
            PRIVILEGED,
            "  privileged:\n    needs: openrc-device\n    runs-on: ubuntu-24.04\n".to_owned(),
            vec!["job privileged: has `needs`, and must run and report on every pull request".to_owned()],
        ),
        (
            PRIVILEGED,
            "  privileged:\n    name: privileged on linux\n    runs-on: ubuntu-24.04\n".to_owned(),
            vec!["job privileged: has `name`, and must run and report on every pull request".to_owned()],
        ),
        (
            PRIVILEGED,
            "  privileged:\n    runs-on: ubuntu-22.04\n".to_owned(),
            vec!["job privileged: runs on a runner outside [\"ubuntu-24.04\"]".to_owned()],
        ),
        (
            "\njobs:\n",
            "\njobs:\n  extra:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: cargo test\n".to_owned(),
            vec!["job extra is not a job of pr-device.yml".to_owned()],
        ),
    ];
    for (from, to, want) in cases {
        assert_eq!(problems(&device_with(from, &to)), want, "{to}");
    }
    // Jobs of the list missing: privileged and openrc-device, removed with every line after the
    // start of the first; and openrc-device alone.
    let text = device();
    let at = text
        .find("  # privileged.yml's job privileged;")
        .expect("the copy");
    assert_eq!(
        problems(&text[..at]),
        ["no job privileged", "no job openrc-device"]
    );
    let at = text
        .find("  # privileged.yml's job openrc-device;")
        .expect("the copy");
    assert_eq!(problems(&text[..at]), ["no job openrc-device"]);
}

#[test]
fn a_copy_that_drifted_from_privileged_yml_is_refused() {
    // Catches: a pull request running other device tests than run after the merge: a step, a
    // runner, an env or a test target changed in the copy or in privileged.yml alone, a copy
    // missing, and a top-level env (RUSTFLAGS) that differs.
    let privileged = committed(PRIVILEGED_WORKFLOW);
    let drift = |job: &str| format!("job {job}: not privileged.yml's job {job}, key for key");
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
        .find("  # privileged.yml's job privileged;")
        .expect("the copy");
    let without = device()[..at].to_owned();
    assert_eq!(
        mirror_problems(&without, &privileged),
        [drift("privileged"), drift("openrc-device")]
    );
    assert_eq!(
        mirror_problems("on: [unclosed\n", &privileged).len(),
        1,
        "a file that does not parse is one problem"
    );
    assert_eq!(mirror_problems(&device(), "").len(), 1);
    // A copy with an `if:` added is no copy either (and `problems` refuses the `if:` itself).
    let copy = device_with(
        PRIVILEGED,
        "  privileged:\n    if: ${{ always() }}\n    runs-on: ubuntu-24.04\n",
    );
    assert_eq!(mirror_problems(&copy, &privileged), [drift("privileged")]);
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
        "      - run: echo ${{ github['sha'] }} ${{ toJSON(github.event.pull_request.labels) }}\n",
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
fn the_trigger_of_pr_device_yml_is_pull_request_alone_with_no_filter() {
    // Catches: a paths or branches filter on `on`, which leaves the required checks unreported on
    // a pull request it filters out (a check that never starts), a push trigger that would run the copies again after the merge,
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
    assert!(
        none[0]
            .starts_with("job privileged: privileged outside privileged.yml and net-install.yml")
    );
    assert!(
        none[1].starts_with(
            "job openrc-device: privileged outside privileged.yml and net-install.yml"
        )
    );
    assert_eq!(none[2], ON_PROBLEM);
}

#[test]
fn the_concurrency_of_pr_device_yml_is_one_group_per_pull_request_that_cancels() {
    // Catches: superseded runs left running (and holding runners) after a newer push, a
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

#[test]
fn a_top_level_key_that_would_change_the_copies_is_refused() {
    // Catches: a top-level `defaults` (a shell or a working directory for every step), which
    // changes what the copies of privileged.yml's jobs run while their keys still compare equal.
    let at = "\npermissions: {}\n";
    for (added, key) in [
        (
            "defaults:\n  run:\n    working-directory: crates\n",
            "defaults",
        ),
        ("run-name: x\n", "run-name"),
    ] {
        assert_eq!(
            problems(&device_with(at, &format!("{at}{added}"))),
            [format!(
                "pr-device.yml: the top-level key `{key}` is not one of {TOP_LEVEL:?}"
            )],
            "{added}"
        );
    }
}
