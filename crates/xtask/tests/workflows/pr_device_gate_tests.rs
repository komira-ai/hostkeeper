//! The conditions of `pr-device.yml`'s macOS jobs and of their gates, and the output of its job
//! `changes`, read from the committed file and evaluated ([`super::expr`]) against every outcome
//! of the jobs they name: a macOS job runs unless the pull request is known not to need it, and
//! its required check passes only when it passed or was not needed. Also: a push to main runs
//! every macOS job, and each required check name is the check of exactly one job.

use std::collections::BTreeMap;

use super::expr::{Context, holds};
use super::lint::{MACOS_DEVICE_JOB, PRIVILEGED_WORKFLOW, entries, entry, triggers};
use super::pr_device::{FILE, JOBS, Kind, MACOS_TEST_JOB, REQUIRED, gate_yaml, gated_name};
use super::pr_paths::NEEDED;
use super::workflows;
use super::yaml::{self, Node, Value};

/// The committed text of `file`.
fn committed(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{file} is in the repository"))
}

fn job<'a>(doc: &'a Node, name: &str) -> &'a Node {
    entry(doc, "jobs")
        .and_then(|jobs| entry(jobs, name))
        .unwrap_or_else(|| panic!("no job {name}"))
}

fn text_at<'a>(node: &'a Node, key: &str) -> &'a str {
    entry(node, key)
        .and_then(Node::as_str)
        .unwrap_or_else(|| panic!("no {key}"))
}

fn steps(node: &Node) -> &[Node] {
    match entry(node, "steps").map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps,
        _ => &[],
    }
}

/// The job of [`JOBS`] that is the gate of `gated`.
fn gate_of(gated: &str) -> &'static str {
    JOBS.iter()
        .find(|j| matches!(j.kind, Kind::Gate { of } if of == gated))
        .unwrap_or_else(|| panic!("no gate of {gated}"))
        .name
}

/// The conditions of the gated job `gated` of `text` and of its gate: `(the job's if, the gate's
/// if, the gate's skip-note step if, the gate's failing step if)`.
struct Conditions {
    job: String,
    gate: String,
    note: String,
    fail: String,
}

fn conditions(text: &str, gated: &str) -> Conditions {
    let doc = yaml::load(text).expect("load");
    let gate_name = gate_of(gated);
    let gate = job(&doc, gate_name);
    let [note, fail] = steps(gate) else {
        panic!("the gate of {gated} has two steps");
    };
    Conditions {
        job: text_at(job(&doc, gated), "if").to_owned(),
        gate: text_at(gate, "if").to_owned(),
        note: text_at(note, "if").to_owned(),
        fail: text_at(fail, "if").to_owned(),
    }
}

/// One outcome of the jobs before a gate.
#[derive(Debug, Clone, Copy)]
struct Outcome {
    cancelled: bool,
    changes: &'static str,
    /// The answer `changes` gave, `""` for none.
    answer: &'static str,
    /// What the macOS job ends as, if it runs.
    macos: &'static str,
}

const CHANGES_RESULTS: [&str; 4] = ["success", "failure", "cancelled", "skipped"];
const RESULTS: [&str; 3] = ["success", "failure", "cancelled"];
const ANSWERS: [&str; 5] = ["true", "false", "", "FALSE", "no"];

fn outcomes() -> Vec<Outcome> {
    let mut all = Vec::new();
    for cancelled in [false, true] {
        for changes in CHANGES_RESULTS {
            for answer in ANSWERS {
                for macos in RESULTS {
                    all.push(Outcome {
                        cancelled,
                        changes,
                        answer,
                        macos,
                    });
                }
            }
        }
    }
    all
}

/// Whether `o` is a pull request known not to need the macOS jobs.
fn not_needed(o: Outcome) -> bool {
    o.changes == "success" && o.answer.eq_ignore_ascii_case("false")
}

/// What one gated job and its gate do in `o`.
#[derive(Debug, PartialEq)]
struct Run {
    runs: bool,
    result: &'static str,
    notes_skip: bool,
    passes: bool,
}

/// What the gated job `gated` and its gate do in `o` under the conditions `c`.
fn run(c: &Conditions, gated: &str, o: Outcome) -> Run {
    let mut names = BTreeMap::from([
        ("needs.changes.result".to_owned(), o.changes.to_owned()),
        (
            "needs.changes.outputs.needed".to_owned(),
            o.answer.to_owned(),
        ),
    ]);
    let runs = holds(
        &c.job,
        &Context {
            names: names.clone(),
            cancelled: o.cancelled,
        },
    )
    .expect("the job's if");
    let result = if runs { o.macos } else { "skipped" };
    names.insert(format!("needs.{gated}.result"), result.to_owned());
    let context = Context {
        names,
        cancelled: o.cancelled,
    };
    assert!(
        holds(&c.gate, &context).expect("the gate's if"),
        "{o:?}: the gate is skipped"
    );
    let notes_skip = step_runs(&c.note, &context);
    let fails = step_runs(&c.fail, &context);
    Run {
        runs,
        result,
        notes_skip,
        passes: !fails,
    }
}

/// Whether a step under `condition` runs in `context`: a condition with no status function
/// (`always()`, `cancelled()`) is GitHub's `success() && (...)`, false in a cancelled run.
fn step_runs(condition: &str, context: &Context) -> bool {
    let status = condition.contains("always()") || condition.contains("cancelled()");
    (status || !context.cancelled) && holds(condition, context).expect("a step's if")
}

/// What `o` should give: the job runs unless the run is cancelled or `changes` passed and
/// answered `false`; the gate passes when the job passed or was skipped on that answer, and says
/// so in the second case.
fn wanted(o: Outcome) -> Run {
    let runs = !o.cancelled && !not_needed(o);
    let result = if runs { o.macos } else { "skipped" };
    let skipped_by_rule = result == "skipped" && not_needed(o);
    Run {
        runs,
        result,
        notes_skip: skipped_by_rule && !o.cancelled,
        passes: result == "success" || skipped_by_rule,
    }
}

#[test]
fn each_macos_job_runs_unless_changes_passed_and_answered_false_and_its_gate_follows_it() {
    // Catches: a macOS job skipped when changes failed, was cancelled or skipped, or answered
    // `true`, nothing or another word (the diff could not be judged), and one that runs in a
    // cancelled run; a gate that passes on a failed or cancelled macOS job, or on one skipped
    // without changes clearing it, that fails a job that passed or was rightly skipped, or that
    // says it was skipped by the path rule when it was not. Over every outcome, for both jobs.
    let text = committed(FILE);
    for gated in [MACOS_TEST_JOB, MACOS_DEVICE_JOB] {
        let c = conditions(&text, gated);
        let mut passes = [0, 0];
        for o in outcomes() {
            let got = run(&c, gated, o);
            assert_eq!(got, wanted(o), "{gated} {o:?}");
            if got.passes {
                passes[usize::from(got.runs)] += 1;
            }
        }
        // Both ways to pass are reached: skipped by the rule, and run and passed.
        assert!(passes[0] > 0 && passes[1] > 0, "{passes:?}");
        let docs_only = Outcome {
            cancelled: false,
            changes: "success",
            answer: "false",
            macos: "failure",
        };
        assert_eq!(
            run(&c, gated, docs_only),
            Run {
                runs: false,
                result: "skipped",
                notes_skip: true,
                passes: true
            }
        );
        let code = Outcome {
            answer: "true",
            ..docs_only
        };
        assert_eq!(
            run(&c, gated, code),
            Run {
                runs: true,
                result: "failure",
                notes_skip: false,
                passes: false
            }
        );
        let undecided = Outcome {
            changes: "failure",
            answer: "",
            macos: "success",
            ..docs_only
        };
        assert_eq!(
            run(&c, gated, undecided),
            Run {
                runs: true,
                result: "success",
                notes_skip: false,
                passes: true
            }
        );
    }
}

/// The value `changes` outputs as `needed` for an event `event` whose diff step ended `outcome`.
fn needed_output(event: &str, outcome: &str) -> String {
    let context = Context {
        names: BTreeMap::from([
            ("github.event_name".to_owned(), event.to_owned()),
            ("steps.diff.outcome".to_owned(), outcome.to_owned()),
        ]),
        cancelled: false,
    };
    holds(NEEDED, &context).expect("the output").to_string()
}

#[test]
fn a_failed_diff_answers_needed_and_runs_the_macos_jobs() {
    // Catches: an output that answers `false` when git could not compute the diff (a missing or
    // empty SHA, exit 128) or found a path off the skip list (exit 1), both of which fail the
    // step; or a step that never ran (outcome skipped, the checkout failed).
    let doc = yaml::load(&committed(FILE)).expect("load");
    let outputs = entry(job(&doc, "changes"), "outputs").expect("outputs");
    assert_eq!(text_at(outputs, "needed"), NEEDED);
    assert_eq!(needed_output("pull_request", "success"), "false");
    for outcome in ["failure", "skipped", "cancelled", ""] {
        assert_eq!(needed_output("pull_request", outcome), "true", "{outcome}");
    }
    let c = conditions(&committed(FILE), MACOS_TEST_JOB);
    let failed_diff = Outcome {
        cancelled: false,
        changes: "success",
        answer: "true",
        macos: "success",
    };
    assert!(run(&c, MACOS_TEST_JOB, failed_diff).runs);
}

#[test]
fn a_push_to_main_always_runs_every_macos_job() {
    // Catches: the path rule reaching a push: the changes output answering `false` on any event
    // but a pull request (the event has no base SHA there, so its diff would be of nothing); a
    // trigger other than pull_request on pr-device.yml; and the jobs a push to main runs on macOS
    // (privileged.yml's macos-device, artifacts.yml's macOS build) taking a condition or a paths
    // filter.
    for event in ["push", "workflow_dispatch", "schedule", ""] {
        assert_eq!(needed_output(event, "success"), "true", "{event}");
    }
    let doc = yaml::load(&committed(FILE)).expect("load");
    assert_eq!(triggers(entry(&doc, "on").expect("on")), ["pull_request"]);
    for (file, macos_job) in [
        (PRIVILEGED_WORKFLOW, MACOS_DEVICE_JOB),
        ("artifacts.yml", "build-macos"),
    ] {
        let doc = yaml::load(&committed(file)).expect("load");
        let on = entry(&doc, "on").expect("on");
        let push = entry(on, "push").expect("a push trigger");
        assert_eq!(
            entry(push, "branches").map(|b| {
                let mut values = Vec::new();
                super::lint::scalars(b, &mut values);
                values.join(",")
            }),
            Some("main".to_owned()),
            "{file}"
        );
        for filter in ["paths", "paths-ignore"] {
            assert!(entry(push, filter).is_none(), "{file}: {filter}");
        }
        let job = job(&doc, macos_job);
        assert!(entry(job, "if").is_none(), "{file} {macos_job}");
        assert!(
            text_at(job, "runs-on").starts_with("macos-"),
            "{file} {macos_job}"
        );
    }
}

/// The name of the check the job `id` (`node`) reports: its `name`, else its id.
fn check_name<'a>(id: &'a str, node: &'a Node) -> &'a str {
    entry(node, "name").and_then(Node::as_str).unwrap_or(id)
}

#[test]
fn each_required_check_name_is_the_check_of_one_job_and_the_gates_bear_the_real_job_names() {
    // Catches: a gate named otherwise than the job it reports on (the required check `macos-test`
    // or `macos-device` then never reports, or reports something else); a macOS job that keeps
    // its required name beside its gate (two checks of one name, one of them skipped, which
    // passes); and a required check left with no job.
    let doc = yaml::load(&committed(FILE)).expect("load");
    let jobs = entry(&doc, "jobs").map_or(&[][..], entries);
    let names: Vec<(&str, &str)> = jobs
        .iter()
        .map(|(id, node)| {
            let id = id.as_str().expect("an id");
            (id, check_name(id, node))
        })
        .collect();
    for required in REQUIRED {
        let reporting: Vec<&str> = names
            .iter()
            .filter(|(_, name)| *name == required)
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(reporting.len(), 1, "{required}: {reporting:?}");
    }
    for gated in [MACOS_TEST_JOB, MACOS_DEVICE_JOB] {
        let gate = gate_of(gated);
        assert_eq!(check_name(gate, job(&doc, gate)), gated);
        assert_eq!(check_name(gated, job(&doc, gated)), gated_name(gated));
        let want = yaml::load(&gate_yaml(gated)).expect("the gate parses");
        assert_eq!(text_at(&want, "name"), gated);
        assert_ne!(gated_name(gated), gated);
    }
    // Every check name of the file is distinct.
    let mut seen: Vec<&str> = names.iter().map(|(_, n)| *n).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), names.len(), "{names:?}");
}

#[test]
fn a_gate_or_gated_condition_that_lets_a_defect_through_is_caught() {
    // Catches: the tests above passing whatever the conditions say: each spoiled condition makes
    // one expectation of the outcome table false, and the lint refuses the spoiled file.
    let text = committed(FILE);
    for (replace, with) in [
        (
            "macos-test on macos-15\n    needs: changes\n    if: ${{ !cancelled() && !(needs.changes.result == 'success' && needs.changes.outputs.needed == 'false') }}",
            "macos-test on macos-15\n    needs: changes\n    if: ${{ !cancelled() && needs.changes.outputs.needed == 'true' }}",
        ),
        (
            "macos-test on macos-15\n    needs: changes\n    if: ${{ !cancelled() && !(needs.changes.result",
            "macos-test on macos-15\n    needs: changes\n    if: ${{ always() && !(needs.changes.result",
        ),
        (
            "macos-test on macos-15\n    needs: changes\n    if: ${{ !cancelled() && !(needs.changes.result == 'success' && ",
            "macos-test on macos-15\n    needs: changes\n    if: ${{ !cancelled() && !(",
        ),
        (
            "${{ always() && !(needs.macos-test.result == 'success' ||",
            "${{ !cancelled() && !(needs.macos-test.result == 'success' ||",
        ),
        (
            "!(needs.macos-test.result == 'success' ||",
            "!(needs.macos-test.result != 'failure' ||",
        ),
        (
            "(needs.macos-test.result == 'skipped' && needs.changes.result == 'success' && needs.changes.outputs.needed == 'false')",
            "needs.macos-test.result == 'skipped'",
        ),
        (
            "if: ${{ needs.macos-test.result == 'skipped' && needs.changes.result == 'success'",
            "if: ${{ always() && needs.macos-test.result == 'skipped' && needs.changes.result == 'success'",
        ),
        (
            "if: ${{ always() && !(needs.macos-test.result == 'success' ||",
            "if: ${{ !(needs.macos-test.result == 'success' ||",
        ),
    ] {
        assert_eq!(text.matches(replace).count(), 1, "{replace:?}");
        let spoiled = text.replacen(replace, with, 1);
        let c = conditions(&spoiled, MACOS_TEST_JOB);
        let wrong = outcomes()
            .into_iter()
            .any(|o| run(&c, MACOS_TEST_JOB, o) != wanted(o));
        assert!(wrong, "{with:?}");
        assert!(
            !super::lint::lint(FILE, &spoiled).is_empty(),
            "the lint passes {with:?}"
        );
    }
}

#[test]
fn the_expressions_are_read_with_githubs_precedence_and_refused_when_not_understood() {
    // Catches: an evaluator that reads `!a == b` as `!(a == b)`, `a || b && c` as `(a || b) && c`,
    // compares with regard to case, or reads what it does not understand as false.
    let context = Context {
        names: BTreeMap::from([("a.b".to_owned(), "X".to_owned())]),
        cancelled: true,
    };
    for (text, want) in [
        ("${{ a.b == 'x' }}", true),
        ("${{ a.b != 'x' }}", false),
        ("${{ unset == '' }}", true),
        ("${{ always() || always() && !always() }}", true),
        ("${{ (always() || always()) && !always() }}", false),
        ("${{ cancelled() }}", true),
        ("${{ !cancelled() }}", false),
        ("${{ a.b }}", true),
        ("${{ unset }}", false),
    ] {
        assert_eq!(holds(text, &context), Ok(want), "{text}");
    }
    for (text, why) in [
        ("a.b", "\"a.b\" is not `${{ … }}`"),
        ("${{ 'x }}", "an unclosed string in \" 'x \""),
        (
            "${{ a.b > 'x' }}",
            "'>' is not understood in \" a.b > 'x' \"",
        ),
        ("${{ (always() }}", "a `(` that is not closed"),
        ("${{ success() }}", "the function success is not understood"),
        ("${{ always('x') }}", "always( takes no arguments here"),
        (
            "${{ always() == 'x' }}",
            "Bool(true) compared with Text(\"x\"): only strings are compared here",
        ),
        // `!` binds tighter than `==`: `(!a.b) == 'x'`, a comparison of a boolean.
        (
            "${{ !a.b == 'x' }}",
            "Bool(false) compared with Text(\"x\"): only strings are compared here",
        ),
        ("${{ && }}", "Some(And) where a value was expected"),
        (
            "${{ always() always() }}",
            "[Name(\"always\"), Open, Close] left over in \"${{ always() always() }}\"",
        ),
    ] {
        assert_eq!(holds(text, &context), Err(why.to_owned()), "{text}");
    }
}
