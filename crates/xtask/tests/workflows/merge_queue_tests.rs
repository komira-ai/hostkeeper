//! The rules of the merge queue ([`super::merge_queue`]): the committed ci.yml and pr-device.yml
//! run and report every check `main` requires for a merge group as for a pull request, and
//! CONTRIBUTING.md's ruleset requires exactly those checks. Each rule is shown to refuse its defect
//! by putting it into the committed file, which passes, and reading the exact problems it reports;
//! and every condition of the two files is evaluated ([`super::expr`]) for a pull request and for
//! a merge group.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::ci;
use super::expr::{Context, holds};
use super::lint::{entries, entry};
use super::merge_queue::{
    ALWAYS, CANCEL, FILES, KEY, PUSH_ONLY, QUEUE, REQUIRED, SQUASH, checks_problems,
    concurrency_problem, doc_problems, group, on_problem, problems, queue, squash,
};
use super::pr_device;
use super::yaml::{self, Node, Value};
use super::{with_in, workflows};

/// The committed text of `file`.
fn committed(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{file} is in the repository"))
}

/// The committed CONTRIBUTING.md.
fn contributing() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../CONTRIBUTING.md");
    std::fs::read_to_string(path).expect("read CONTRIBUTING.md")
}

/// The queue's problems of `text` as `file`.
fn queue_problems(file: &str, text: &str) -> Vec<String> {
    problems(file, &yaml::load(text).expect("the workflow parses"))
}

/// The committed `file` with `from` (once in it) replaced by `to`.
fn edited(file: &str, from: &str, to: &str) -> String {
    with_in(&committed(file), from, to)
}

/// Every workflow, with `file`'s text replaced by `text`.
fn workflows_with(file: &str, text: &str) -> Vec<(String, String)> {
    workflows()
        .into_iter()
        .map(|(name, old)| {
            let new = if name == file { text.to_owned() } else { old };
            (name, new)
        })
        .collect()
}

/// The event-read refusal of `line` in `file`.
fn read_problem(file: &str, line: &str) -> String {
    format!(
        "{file}: `{line}` reads the event; only an if: that starts `{PUSH_ONLY}` may, so a merge group runs what a pull request runs"
    )
}

/// The job-condition refusal of `job` in `file`.
fn if_problem(file: &str, job: &str) -> String {
    format!(
        "{file}: job {job} has an if: other than {ALWAYS}, and could be skipped for a merge group, which counts as passed"
    )
}

/// The `on` of the committed ci.yml, up to the key after it.
const CI_ON: &str = "on:\n  pull_request:\n  merge_group:\n    types: [checks_requested]\n    branches: [main]\n  push:\n    branches: [main]\n\npermissions";

/// The start of ci.yml's job `lint`, once in the file.
const LINT: &str = "  lint:\n    runs-on: ubuntu-latest\n";

/// The format step of ci.yml's job `lint`, once in the file.
const FMT: &str = "      - run: cargo fmt --all --check\n";

#[test]
fn the_committed_workflows_and_contributing_md_keep_every_rule_of_the_queue() {
    // Catches: a rule that refuses the committed files, so every case below would fail for that
    // reason instead of the defect it adds; and the list of required checks drifting from the
    // workflows' jobs.
    for file in FILES {
        assert_eq!(
            queue_problems(file, &committed(file)),
            Vec::<String>::new(),
            "{file}"
        );
    }
    assert_eq!(checks_problems(&workflows()), Vec::<String>::new());
    assert_eq!(doc_problems(&contributing()), Vec::<String>::new());
    let names: Vec<&str> = REQUIRED.iter().map(|(_, check)| *check).collect();
    assert_eq!(names, ["lint", "test", "privileged", "openrc-device"]);
    assert_eq!(FILES, [ci::FILE, pr_device::FILE]);
}

/// Every `if:` of the jobs of `doc` and of their steps, each as `${{ … }}`, with where it is.
fn conditions(doc: &Node) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (name, job) in entry(doc, "jobs").map_or(&[][..], entries) {
        let name = name.as_str().unwrap_or_default();
        let mut add = |at: String, node: Option<&Node>| {
            if let Some(c) = node.and_then(Node::as_str) {
                let c = if c.trim_start().starts_with("${{") {
                    c.to_owned()
                } else {
                    format!("${{{{ {c} }}}}")
                };
                found.push((at, c));
            }
        };
        add(format!("job {name}"), entry(job, "if"));
        if let Some(Value::Seq(steps)) = entry(job, "steps").map(|s| &s.value) {
            for (k, step) in steps.iter().enumerate() {
                add(format!("job {name} step {k}"), entry(step, "if"));
            }
        }
    }
    found
}

/// Whether `condition` holds for a run of `event`, with every other name unset, when an earlier
/// step or a needed job `failed`.
fn holds_for(condition: &str, event: &str, failed: bool) -> bool {
    let context = Context {
        names: BTreeMap::from([("github.event_name".to_owned(), event.to_owned())]),
        cancelled: false,
        failed,
    };
    holds(condition, &context).unwrap_or_else(|e| panic!("{condition}: {e}"))
}

#[test]
fn every_condition_of_the_two_workflows_is_the_same_for_a_merge_group_as_for_a_pull_request() {
    // Proves, by evaluating them: no job or step of ci.yml or pr-device.yml runs for a pull request
    // and is skipped for a merge group (or the reverse), after a failure or not; every job runs for
    // a merge group; and the steps that read the event, the cache saves, run for a push alone.
    let mut reading = 0;
    for file in FILES {
        let doc = yaml::load(&committed(file)).expect("parses");
        let found = conditions(&doc);
        assert!(!found.is_empty(), "{file} has conditions");
        for (at, condition) in found {
            for failed in [false, true] {
                assert_eq!(
                    holds_for(&condition, "pull_request", failed),
                    holds_for(&condition, "merge_group", failed),
                    "{file} {at}: {condition}"
                );
            }
            if !at.contains(" step ") {
                assert!(holds_for(&condition, "merge_group", false), "{file} {at}");
            }
            if condition.contains("github.event_name") {
                reading += 1;
                assert!(!holds_for(&condition, "merge_group", false), "{file} {at}");
                assert!(holds_for(&condition, "push", false), "{file} {at}");
            }
        }
    }
    // ci.yml's three cache saves and pr-device.yml's two.
    assert_eq!(reading, 5);
}

#[test]
fn a_pull_requests_run_is_cancelled_by_its_newer_one_and_no_merge_groups_run_is() {
    // Proves: cancel-in-progress holds for a pull request alone, and each group is the pull
    // request's ref or else the run's own id, never the pull request number (empty for a merge
    // group, which would put every merge group in one group).
    let names = |event: &str| Context {
        names: BTreeMap::from([("github.event_name".to_owned(), event.to_owned())]),
        cancelled: false,
        failed: false,
    };
    assert!(holds(CANCEL, &names("pull_request")).expect("evaluates"));
    for event in ["merge_group", "push"] {
        assert!(!holds(CANCEL, &names(event)).expect("evaluates"), "{event}");
    }
    assert!(!KEY.contains("number"));
    assert_eq!(group(ci::FILE), format!("ci-{KEY}"));
    assert_eq!(group(pr_device::FILE), format!("pr-device-{KEY}"));
    for file in FILES {
        let block = format!(
            "concurrency:\n  group: {}\n  cancel-in-progress: {CANCEL}\n",
            group(file)
        );
        assert!(committed(file).contains(&block), "{file}");
    }
}

#[test]
fn a_trigger_of_ci_yml_that_misses_a_merge_group_or_a_pull_request_is_refused() {
    // Catches: no merge_group (the queue waits for lint and test until it times out); another
    // type or branch, a paths filter, or no filter at all; a filtered pull request; a push to every
    // branch (which runs again on the queue's own branches); a trigger added; and the list form,
    // which carries no filter.
    for changed in [
        "on:\n  pull_request:\n  push:\n    branches: [main]\n\npermissions",
        "on:\n  pull_request:\n  merge_group:\n  push:\n    branches: [main]\n\npermissions",
        "on:\n  pull_request:\n  merge_group:\n    types: [destroyed]\n    branches: [main]\n  push:\n    branches: [main]\n\npermissions",
        "on:\n  pull_request:\n  merge_group:\n    types: [checks_requested]\n    branches: ['**']\n  push:\n    branches: [main]\n\npermissions",
        "on:\n  pull_request:\n  merge_group:\n    types: [checks_requested]\n    branches: [main]\n    paths: [crates/**]\n  push:\n    branches: [main]\n\npermissions",
        "on:\n  pull_request:\n    paths: [crates/**]\n  merge_group:\n    types: [checks_requested]\n    branches: [main]\n  push:\n    branches: [main]\n\npermissions",
        "on:\n  pull_request:\n  merge_group:\n    types: [checks_requested]\n    branches: [main]\n  push:\n\npermissions",
        "on:\n  pull_request:\n  merge_group:\n    types: [checks_requested]\n    branches: [main]\n  push:\n    branches: [main]\n  workflow_dispatch:\n\npermissions",
        "on:\n  merge_group:\n    types: [checks_requested]\n    branches: [main]\n  push:\n    branches: [main]\n\npermissions",
        "on: [pull_request, merge_group, push]\n\npermissions",
    ] {
        assert_eq!(
            queue_problems(ci::FILE, &edited(ci::FILE, CI_ON, changed)),
            [on_problem(ci::FILE)],
            "{changed}"
        );
    }
    assert_eq!(
        on_problem(ci::FILE),
        "ci.yml: on must be exactly pull_request with no filter, merge_group of types [checks_requested] on branches [main], push to [main]"
    );
    // The same triggers in another order, and a filter as a scalar, are the same triggers.
    let same = "on:\n  push:\n    branches: main\n  merge_group:\n    branches: [main]\n    types: [checks_requested]\n  pull_request: {}\n\npermissions";
    assert_eq!(
        queue_problems(ci::FILE, &edited(ci::FILE, CI_ON, same)),
        Vec::<String>::new()
    );
    // ci::problems runs the queue's rules on ci.yml.
    assert_eq!(
        ci::problems(&edited(
            ci::FILE,
            CI_ON,
            "on:\n  pull_request:\n\npermissions"
        )),
        [on_problem(ci::FILE)]
    );
}

#[test]
fn a_concurrency_of_ci_yml_that_lets_runs_of_the_queue_cancel_each_other_is_refused() {
    // Catches: the pull request number as the key (every merge group in one group, each
    // cancelling the one before), a merge group's run cancelled at all, another workflow's
    // prefix (pr-device.yml's runs would cancel ci.yml's), an extra key, and no concurrency.
    let block = format!(
        "concurrency:\n  group: {}\n  cancel-in-progress: {CANCEL}\n",
        group(ci::FILE)
    );
    for changed in [
        format!(
            "concurrency:\n  group: ci-${{{{ github.event.pull_request.number }}}}\n  cancel-in-progress: {CANCEL}\n"
        ),
        format!(
            "concurrency:\n  group: {}\n  cancel-in-progress: true\n",
            group(ci::FILE)
        ),
        format!(
            "concurrency:\n  group: {}\n  cancel-in-progress: {CANCEL}\n",
            group(pr_device::FILE)
        ),
        format!(
            "concurrency:\n  group: {}\n  cancel-in-progress: {CANCEL}\n  x: y\n",
            group(ci::FILE)
        ),
        format!("concurrency:\n  group: {}\n  group: {0}\n", group(ci::FILE)),
        String::new(),
    ] {
        let text = edited(ci::FILE, &block, &changed);
        let Ok(doc) = yaml::load(&text) else {
            // A repeated key is no YAML the lint reads; GitHub refuses it too.
            continue;
        };
        assert_eq!(
            problems(ci::FILE, &doc),
            [concurrency_problem(ci::FILE)],
            "{changed}"
        );
    }
    assert_eq!(
        concurrency_problem(ci::FILE),
        format!(
            "ci.yml: concurrency must be exactly the group ci-{KEY} with cancel-in-progress: {CANCEL}"
        )
    );
}

#[test]
fn a_job_condition_that_could_skip_a_required_check_for_a_merge_group_is_refused() {
    // Catches: a job skipped for a merge group (a skipped job counts as a passed check, so the
    // queue would merge what nothing tested), by the event or by any other condition; and
    // allows always(), the gate's, which runs whatever the jobs before it did.
    let event = "    if: ${{ github.event_name == 'pull_request' }}\n";
    assert_eq!(
        queue_problems(ci::FILE, &edited(ci::FILE, LINT, &format!("{LINT}{event}"))),
        [
            if_problem(ci::FILE, "lint"),
            read_problem(ci::FILE, "${{ github.event_name == 'pull_request' }}"),
        ]
    );
    for other in [
        "    if: ${{ !cancelled() }}\n",
        "    if: false\n",
        "    if: success()\n",
    ] {
        assert_eq!(
            queue_problems(ci::FILE, &edited(ci::FILE, LINT, &format!("{LINT}{other}"))),
            [if_problem(ci::FILE, "lint")],
            "{other}"
        );
    }
    assert_eq!(
        queue_problems(
            ci::FILE,
            &edited(ci::FILE, LINT, &format!("{LINT}    if: {ALWAYS}\n"))
        ),
        Vec::<String>::new()
    );
    // In pr-device.yml too, beside its own rule of no `if:` at all.
    let privileged = "  privileged:\n    runs-on: ubuntu-24.04\n";
    assert_eq!(
        queue_problems(
            pr_device::FILE,
            &edited(
                pr_device::FILE,
                privileged,
                &format!("{privileged}    if: false\n")
            )
        ),
        [if_problem(pr_device::FILE, "privileged")]
    );
}

#[test]
fn a_step_that_reads_the_event_otherwise_than_as_a_push_is_refused() {
    // Catches: a step skipped for a merge group by its event, its ref or the pull request it does
    // not have (a step that passes without running), however the condition is written; a command
    // that reads the pull request's base, head, ref or event, in an expression or from the
    // runner's variables, and so runs otherwise for a merge group; and the same at the top of
    // the workflow. Allows the cache saves' form and any spacing of it.
    let cases: [(&str, &str); 15] = [
        (
            "      - if: ${{ github.event_name == 'pull_request' }}\n        run: cargo fmt --all --check\n",
            "${{ github.event_name == 'pull_request' }}",
        ),
        (
            "      - if: github.event_name != 'merge_group'\n        run: cargo fmt --all --check\n",
            "github.event_name != 'merge_group'",
        ),
        (
            "      - if: ${{ github.event_name == 'push' || github.event_name == 'pull_request' }}\n        run: cargo fmt --all --check\n",
            "${{ github.event_name == 'push' || github.event_name == 'pull_request' }}",
        ),
        (
            "      - if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' || github.event_name == 'pull_request' }}\n        run: cargo fmt --all --check\n",
            "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' || github.event_name == 'pull_request' }}",
        ),
        (
            "      - if: ${{ !github.event.pull_request.draft }}\n        run: cargo fmt --all --check\n",
            "${{ !github.event.pull_request.draft }}",
        ),
        (
            "      - if: ${{ startsWith(github.ref, 'refs/pull/') }}\n        run: cargo fmt --all --check\n",
            "${{ startsWith(github.ref, 'refs/pull/') }}",
        ),
        (
            "      - if: ${{ github['event_name'] == 'pull_request' }}\n        run: cargo fmt --all --check\n",
            "${{ github['event_name'] == 'pull_request' }}",
        ),
        (
            "      - run: echo ${{ github [ \"head_ref\" ] }}\n",
            "echo ${{ github [ \"head_ref\" ] }}",
        ),
        (
            "      - run: git diff ${{ github.event.pull_request.base.sha }}\n",
            "git diff ${{ github.event.pull_request.base.sha }}",
        ),
        (
            "      - run: echo ${{ github.head_ref }} ${{ github.base_ref }}\n",
            "echo ${{ github.head_ref }} ${{ github.base_ref }}",
        ),
        (
            "      - run: echo ${{ github.ref_name }}\n",
            "echo ${{ github.ref_name }}",
        ),
        (
            "      - run: test \"$GITHUB_EVENT_NAME\" = pull_request\n",
            "test \"$GITHUB_EVENT_NAME\" = pull_request",
        ),
        (
            "      - run: jq .pull_request.base.sha \"$GITHUB_EVENT_PATH\"\n",
            "jq .pull_request.base.sha \"$GITHUB_EVENT_PATH\"",
        ),
        (
            "      - run: git diff \"origin/$GITHUB_BASE_REF\"\n",
            "git diff \"origin/$GITHUB_BASE_REF\"",
        ),
        (
            "      - name: Check ${{ github.event.pull_request.number }}\n        run: cargo fmt --all --check\n",
            "Check ${{ github.event.pull_request.number }}",
        ),
    ];
    for (step, line) in cases {
        assert_eq!(
            queue_problems(ci::FILE, &edited(ci::FILE, FMT, step)),
            [read_problem(ci::FILE, line)],
            "{step}"
        );
    }
    // A multi-line command is named by its first line.
    assert_eq!(
        queue_problems(
            ci::FILE,
            &edited(
                ci::FILE,
                FMT,
                "      - run: |\n          cargo fmt --all --check\n          echo \"$GITHUB_REF\"\n"
            )
        ),
        [read_problem(ci::FILE, "cargo fmt --all --check")]
    );
    // The top-level env applies to every job.
    let env = "env:\n  CARGO_TERM_COLOR: always\n";
    assert_eq!(
        queue_problems(
            ci::FILE,
            &edited(
                ci::FILE,
                env,
                &format!("{env}  BASE: ${{{{ github.event.merge_group.base_sha }}}}\n")
            )
        ),
        [read_problem(
            ci::FILE,
            "${{ github.event.merge_group.base_sha }}"
        )]
    );
    for innocent in [
        "      - if: ${{ github.event_name == 'push' && steps.cargo-cache.outputs.cache-hit != 'true' }}\n        run: cargo fmt --all --check\n",
        "      - if: ${{github.event_name=='push'&&matrix.shard == 1}}\n        run: cargo fmt --all --check\n",
        "      - if: github.event_name == 'push' && !cancelled()\n        run: cargo fmt --all --check\n",
        "      - run: echo ${{ github.sha }} ${{ github.repository }} \"$GITHUB_WORKSPACE\" \"$GITHUB_STEP_SUMMARY\"\n",
        "      - run: echo ${{ github['sha'] }} ${{ github[ \"run_id\" ] }}\n",
    ] {
        assert_eq!(
            queue_problems(ci::FILE, &edited(ci::FILE, FMT, innocent)),
            Vec::<String>::new(),
            "{innocent}"
        );
    }
    // Two expressions in one if: is no if: the lint reads as the push's.
    let two = "      - if: ${{ github.event_name == 'push' && x }} ${{ github.event_name }}\n        run: cargo fmt --all --check\n";
    assert_eq!(
        queue_problems(ci::FILE, &edited(ci::FILE, FMT, two)),
        [read_problem(
            ci::FILE,
            "${{ github.event_name == 'push' && x }} ${{ github.event_name }}"
        )]
    );
    // The same rule holds in pr-device.yml.
    let save = "        if: ${{ github.event_name == 'push' && steps.cargo-cache.outputs.cache-hit != 'true' }}\n";
    let text = committed(pr_device::FILE);
    let at = text.find(save).expect("a cache save of pr-device.yml");
    let mut changed = text.clone();
    changed.replace_range(
        at..at + save.len(),
        "        if: ${{ github.event_name == 'pull_request' && steps.cargo-cache.outputs.cache-hit != 'true' }}\n",
    );
    assert_eq!(
        queue_problems(pr_device::FILE, &changed),
        [read_problem(
            pr_device::FILE,
            "${{ github.event_name == 'pull_request' && steps.cargo-cache.outputs.cache-hit != 'true' }}"
        )]
    );
}

#[test]
fn a_required_check_without_exactly_one_job_of_its_workflow_is_refused() {
    // Catches: a required check renamed or removed (the queue waits for it until it times out),
    // reported by a second job (either one's result could stand for the check), or by a job of
    // another workflow; a workflow outside the two that runs for merge groups; and a workflow
    // that does not parse. A workflow that runs for no pull request and no merge group
    // (privileged.yml, with jobs of the same names) reports nothing here.
    let check = |file: &str, check: &str, jobs: &str| {
        format!(
            "the required check {check} is reported by jobs of {jobs}, not by exactly one job of {file}"
        )
    };
    let renamed = edited(
        ci::FILE,
        LINT,
        "  lint:\n    name: Lint\n    runs-on: ubuntu-latest\n",
    );
    assert_eq!(
        checks_problems(&workflows_with(ci::FILE, &renamed)),
        [check(ci::FILE, "lint", "[]")]
    );
    let gate = edited(ci::FILE, "    name: test\n", "    name: tests\n");
    assert_eq!(
        checks_problems(&workflows_with(ci::FILE, &gate)),
        [check(ci::FILE, "test", "[]")]
    );
    let second = edited(
        ci::FILE,
        LINT,
        "  lint:\n    name: test\n    runs-on: ubuntu-latest\n",
    );
    assert_eq!(
        checks_problems(&workflows_with(ci::FILE, &second)),
        [
            check(ci::FILE, "lint", "[]"),
            check(ci::FILE, "test", "[\"ci.yml\", \"ci.yml\"]"),
        ]
    );
    let privileged = "  privileged:\n    runs-on: ubuntu-24.04\n";
    let moved = edited(
        pr_device::FILE,
        privileged,
        "  privileged:\n    name: openrc-device\n    runs-on: ubuntu-24.04\n",
    );
    assert_eq!(
        checks_problems(&workflows_with(pr_device::FILE, &moved)),
        [
            check(pr_device::FILE, "privileged", "[]"),
            check(
                pr_device::FILE,
                "openrc-device",
                "[\"pr-device.yml\", \"pr-device.yml\"]"
            ),
        ]
    );
    // A job `lint` in another workflow: on pull requests alone, and on merge groups too.
    let other = "name: other\non:\n  pull_request:\npermissions: {}\njobs:\n  lint:\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n";
    let mut all = workflows();
    all.push(("other.yml".to_owned(), other.to_owned()));
    assert_eq!(
        checks_problems(&all),
        [check(ci::FILE, "lint", "[\"ci.yml\", \"other.yml\"]")]
    );
    let other_queued = other.replace("  pull_request:\n", "  pull_request:\n  merge_group:\n");
    let mut all = workflows();
    all.push(("other.yml".to_owned(), other_queued));
    assert_eq!(
        checks_problems(&all),
        [
            "other.yml: runs for a merge group; only [\"ci.yml\", \"pr-device.yml\"] do, held to the queue's rules".to_owned(),
            check(ci::FILE, "lint", "[\"ci.yml\", \"other.yml\"]"),
        ]
    );
    // On a push alone, a job of a required check's name reports nothing for the queue.
    let mut all = workflows();
    all.push((
        "other.yml".to_owned(),
        other.replace("  pull_request:\n", "  push:\n"),
    ));
    assert_eq!(checks_problems(&all), Vec::<String>::new());
    // A file that does not parse is named, and its checks are missing.
    let broken = workflows_with(pr_device::FILE, "jobs: [\n");
    let found = checks_problems(&broken);
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found[0].starts_with("pr-device.yml: "), "{found:?}");
    assert_eq!(found[1], check(pr_device::FILE, "privileged", "[]"));
    assert_eq!(found[2], check(pr_device::FILE, "openrc-device", "[]"));
}

/// The committed CONTRIBUTING.md with `from` (once in it) replaced by `to`.
fn doc_with(from: &str, to: &str) -> String {
    with_in(&contributing(), from, to)
}

const CHECKS_PROBLEM: &str = "CONTRIBUTING.md: the ruleset must have one required_status_checks rule of exactly [\"lint\", \"test\", \"privileged\", \"openrc-device\"] from GitHub Actions (15368), with strict_required_status_checks_policy false";
const RULESET_PROBLEM: &str =
    "CONTRIBUTING.md: the ruleset is not an active branch ruleset of refs/heads/main alone";

fn queue_problem() -> String {
    format!(
        "CONTRIBUTING.md: the ruleset must have one merge_queue rule with {QUEUE} {}",
        queue()
    )
}

fn squash_problem() -> String {
    format!("CONTRIBUTING.md: {SQUASH} must be exactly {}", squash())
}

#[test]
fn a_ruleset_in_contributing_md_other_than_the_workflows_checks_and_the_queues_settings_is_refused()
{
    // Catches: the ruleset the documented calls create requiring a check no job reports (the
    // queue would wait for it), or leaving out one the workflows run; a check of another app; a
    // branch that must be up to date; a ruleset of another branch or not enforced; another merge
    // method, grouping, size or timeout of the queue, a second queue rule or none; and squash
    // settings that copy the description or the branch commits into main's history.
    let openrc = "          { \"context\": \"openrc-device\", \"integration_id\": 15368 }\n";
    let cases: Vec<(String, String, String)> = vec![
        (
            ",\n".to_owned() + openrc,
            "\n".to_owned(),
            CHECKS_PROBLEM.to_owned(),
        ),
        (
            openrc.to_owned(),
            openrc.to_owned().replace('\n', ",\n")
                + "          { \"context\": \"test-shard (1)\", \"integration_id\": 15368 }\n",
            CHECKS_PROBLEM.to_owned(),
        ),
        (
            "{ \"context\": \"lint\", \"integration_id\": 15368 }".to_owned(),
            "{ \"context\": \"lint\", \"integration_id\": 1 }".to_owned(),
            CHECKS_PROBLEM.to_owned(),
        ),
        (
            "{ \"context\": \"lint\", \"integration_id\": 15368 },\n          { \"context\": \"test\", \"integration_id\": 15368 }".to_owned(),
            "{ \"context\": \"test\", \"integration_id\": 15368 },\n          { \"context\": \"lint\", \"integration_id\": 15368 }".to_owned(),
            CHECKS_PROBLEM.to_owned(),
        ),
        (
            "\"strict_required_status_checks_policy\": false".to_owned(),
            "\"strict_required_status_checks_policy\": true".to_owned(),
            CHECKS_PROBLEM.to_owned(),
        ),
        (
            "\"type\": \"required_status_checks\"".to_owned(),
            "\"type\": \"required_status_check\"".to_owned(),
            CHECKS_PROBLEM.to_owned(),
        ),
        (
            "\"refs/heads/main\"".to_owned(),
            "\"refs/heads/release\"".to_owned(),
            RULESET_PROBLEM.to_owned(),
        ),
        (
            "\"enforcement\": \"active\"".to_owned(),
            "\"enforcement\": \"evaluate\"".to_owned(),
            RULESET_PROBLEM.to_owned(),
        ),
        (
            "\"target\": \"branch\"".to_owned(),
            "\"target\": \"tag\"".to_owned(),
            RULESET_PROBLEM.to_owned(),
        ),
        (
            "\"merge_method\": \"SQUASH\"".to_owned(),
            "\"merge_method\": \"MERGE\"".to_owned(),
            queue_problem(),
        ),
        (
            "\"grouping_strategy\": \"ALLGREEN\"".to_owned(),
            "\"grouping_strategy\": \"HEADGREEN\"".to_owned(),
            queue_problem(),
        ),
        (
            "\"max_entries_to_build\": 5".to_owned(),
            "\"max_entries_to_build\": 1".to_owned(),
            queue_problem(),
        ),
        (
            "\"check_response_timeout_minutes\": 360".to_owned(),
            "\"check_response_timeout_minutes\": 30".to_owned(),
            queue_problem(),
        ),
        (
            "\"check_response_timeout_minutes\": 360\n".to_owned(),
            "\"check_response_timeout_minutes\": 360,\n        \"extra\": 1\n".to_owned(),
            queue_problem(),
        ),
        (
            "\"type\": \"merge_queue\"".to_owned(),
            "\"type\": \"merge-queue\"".to_owned(),
            queue_problem(),
        ),
        (
            "    { \"type\": \"deletion\" },\n".to_owned(),
            "    { \"type\": \"deletion\" },\n    { \"type\": \"merge_queue\", \"parameters\": {} },\n".to_owned(),
            queue_problem(),
        ),
        (
            "\"squash_merge_commit_message\": \"BLANK\"".to_owned(),
            "\"squash_merge_commit_message\": \"COMMIT_MESSAGES\"".to_owned(),
            squash_problem(),
        ),
        (
            "\"squash_merge_commit_message\": \"BLANK\"".to_owned(),
            "\"squash_merge_commit_message\": \"PR_BODY\"".to_owned(),
            squash_problem(),
        ),
        (
            "\"squash_merge_commit_title\": \"PR_TITLE\"".to_owned(),
            "\"squash_merge_commit_title\": \"COMMIT_OR_PR_TITLE\"".to_owned(),
            squash_problem(),
        ),
    ];
    for (from, to, want) in cases {
        assert_eq!(doc_problems(&doc_with(&from, &to)), [want], "{to}");
    }
}

#[test]
fn a_contributing_md_whose_queue_section_cannot_be_read_is_refused() {
    // Catches: the section renamed or gone, a JSON block more or fewer than the two, a block that
    // is not JSON, and one left open: each is named, rather than passed as having no problem.
    let heading = "\n## The merge queue\n";
    assert_eq!(
        doc_problems(&doc_with(heading, "\n## The queue\n")),
        ["CONTRIBUTING.md: no section \"## The merge queue\""]
    );
    let settings = "```json\n{ \"squash_merge_commit_title\"";
    assert_eq!(
        doc_problems(&doc_with(settings, "```\n{ \"squash_merge_commit_title\"")),
        [
            "CONTRIBUTING.md: \"## The merge queue\" has 1 JSON blocks, not the ruleset and the squash settings"
        ]
    );
    let why = "\nWhy each:\n";
    assert_eq!(
        doc_problems(&doc_with(why, "\n```json\n{}\n```\n\nWhy each:\n")),
        [
            "CONTRIBUTING.md: \"## The merge queue\" has 3 JSON blocks, not the ruleset and the squash settings"
        ]
    );
    let bad = "{ \"squash_merge_commit_title\": \"PR_TITLE\", \"squash_merge_commit_message\": \"BLANK\" }";
    let err = serde_json::from_str::<serde_json::Value>("{ \"squash_merge_commit_title\": }\n")
        .expect_err("not JSON");
    assert_eq!(
        doc_problems(&doc_with(bad, "{ \"squash_merge_commit_title\": }")),
        [format!("CONTRIBUTING.md: a ```json block: {err}")]
    );
    // A block left open at the end of the section.
    let tail =
        "{\"squash_merge_commit_title\": \"PR_TITLE\", \"squash_merge_commit_message\": \"BLANK\"}";
    let open = format!("# x\n\n## The merge queue\n\n```json\n{tail}\n```\n\n```json\n{tail}\n");
    assert_eq!(
        doc_problems(&open),
        ["CONTRIBUTING.md: a ```json block that is not closed"]
    );
    // A section is read to the next heading of its level, not past it.
    let next = format!(
        "# x\n\n## The merge queue\n\n```json\n{tail}\n```\n\n## Next\n\n```json\n{tail}\n```\n"
    );
    assert_eq!(
        doc_problems(&next),
        [
            "CONTRIBUTING.md: \"## The merge queue\" has 1 JSON blocks, not the ruleset and the squash settings"
        ]
    );
}
