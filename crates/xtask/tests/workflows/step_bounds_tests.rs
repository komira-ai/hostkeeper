//! The rule of [`super::step_bounds`]: in the workflows that reach the build farm, every job has
//! a `timeout-minutes` of its own and every step one under the job's. Each defect is shown
//! refused on a small workflow read by the rule itself, and through the whole lint on the
//! committed farm.yml and mac-farm.yml with the defect put in.

use super::lint::{entries, entry, lint};
use super::step_bounds::{FILES, problems};
use super::yaml::{self, Node, Value};
use super::{with_in, workflows};

/// The problems of the rule for the workflow `text`.
fn bounds(text: &str) -> Vec<String> {
    problems(&yaml::load(text).expect("the workflow parses"))
}

/// A job of three steps (named, a `run:` of two lines, and a `uses:`), bound at 30 minutes, its
/// steps at 10, 20 and 29.
const BOUND: &str = "\
jobs:
  build:
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    steps:
      - name: Check out
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        timeout-minutes: 10
      - run: |
          first line
          second line
        timeout-minutes: 20
      - uses: example/action@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v1.0.0
        timeout-minutes: 29
";

/// The committed text of the workflow `file`.
fn committed(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .expect("the workflow is in the repository")
        .1
}

/// The number (from 1) of the step named `name` of the job `job` of the workflow `text`.
fn step_number(text: &str, job: &str, name: &str) -> usize {
    let doc = yaml::load(text).expect("the workflow parses");
    let job: &Node = entry(&doc, "jobs")
        .and_then(|jobs| entry(jobs, job))
        .expect("the job");
    let Some(Value::Seq(steps)) = entry(job, "steps").map(|s| &s.value) else {
        panic!("the job has steps");
    };
    steps
        .iter()
        .position(|s| entry(s, "name").and_then(Node::as_str) == Some(name))
        .expect("the step")
        + 1
}

#[test]
fn the_committed_farm_workflows_give_every_step_a_bound_under_the_jobs() {
    // Catches: a step of farm.yml or mac-farm.yml without a bound of its own, or a bound that is
    // not under its job's, so that a step hung on a dead connection to the farm waits until the
    // job's limit; and a rule that holds no file of the repository.
    assert_eq!(FILES, ["farm.yml", "mac-farm.yml"]);
    for file in FILES {
        let text = committed(file);
        assert_eq!(bounds(&text), Vec::<String>::new(), "{file}");
        assert_eq!(lint(file, &text), Vec::<String>::new(), "{file}");
        let doc = yaml::load(&text).expect("the workflow parses");
        let jobs = entry(&doc, "jobs").map_or(&[][..], entries);
        assert!(!jobs.is_empty(), "{file}");
    }
}

#[test]
fn a_job_and_steps_with_their_bounds_under_it_pass() {
    // Catches: a rule that refuses what it was written for: a bound one minute under the job's,
    // a step with a `uses:` (whose bound holds the whole action), one with a `run:` of lines.
    assert_eq!(bounds(BOUND), Vec::<String>::new());
}

#[test]
fn a_step_without_a_bound_is_refused_and_named_by_its_name_run_or_uses() {
    // Catches: a step left to the job's limit, and a problem that does not say which step.
    let cases = [
        (
            "        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        timeout-minutes: 10\n",
            "        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n",
            "job build: step 1 (`Check out`) has no `timeout-minutes`, a whole number of minutes from 1",
        ),
        (
            "        timeout-minutes: 20\n",
            "",
            "job build: step 2 (`first line`) has no `timeout-minutes`, a whole number of minutes from 1",
        ),
        (
            "        timeout-minutes: 29\n",
            "",
            "job build: step 3 (`example/action@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`) has no `timeout-minutes`, a whole number of minutes from 1",
        ),
    ];
    for (from, to, want) in cases {
        assert_eq!(bounds(&with_in(BOUND, from, to)), [want], "{to:?}");
    }
}

#[test]
fn a_step_bound_that_is_not_under_the_jobs_is_refused() {
    // Catches: a step whose bound equals or passes the job's, which bounds nothing the job does
    // not already bound.
    for minutes in ["30", "31", "600"] {
        assert_eq!(
            bounds(&with_in(
                BOUND,
                "        timeout-minutes: 29\n",
                &format!("        timeout-minutes: {minutes}\n")
            )),
            [format!(
                "job build: step 3 (`example/action@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`): `timeout-minutes: {minutes}` is not under the job's 30"
            )],
            "{minutes}"
        );
    }
}

#[test]
fn a_bound_that_is_not_a_whole_number_of_minutes_from_1_is_no_bound() {
    // Catches: an expression (whose value the rule cannot know, and which may be the job's), a
    // fraction, 0 (no bound at all to GitHub) or text taken for a bound.
    for value in ["${{ inputs.minutes }}", "1.5", "0", "ten", "-1"] {
        assert_eq!(
            bounds(&with_in(
                BOUND,
                "        timeout-minutes: 20\n",
                &format!("        timeout-minutes: {value}\n")
            )),
            [
                "job build: step 2 (`first line`) has no `timeout-minutes`, a whole number of minutes from 1"
            ],
            "{value}"
        );
    }
}

#[test]
fn a_job_without_a_bound_of_its_own_is_refused() {
    // Catches: a job left to GitHub's default of six hours; its steps' bounds are then compared
    // with nothing, and only a step without one is refused beside it.
    for to in ["", "    timeout-minutes: ${{ inputs.minutes }}\n"] {
        let text = with_in(BOUND, "    timeout-minutes: 30\n", to);
        assert_eq!(
            bounds(&text),
            ["job build: no `timeout-minutes` of its own, a whole number of minutes from 1"],
            "{to:?}"
        );
        assert_eq!(
            bounds(&with_in(&text, "        timeout-minutes: 20\n", "")),
            [
                "job build: no `timeout-minutes` of its own, a whole number of minutes from 1",
                "job build: step 2 (`first line`) has no `timeout-minutes`, a whole number of minutes from 1",
            ],
            "{to:?}"
        );
    }
}

#[test]
fn the_lint_holds_farm_yml_and_mac_farm_yml_to_the_rule() {
    // Catches: the rule not called for the two workflows that reach the farm, so a bound removed
    // from the farm build, or raised to the job's, passes the lint.
    let farm = committed("farm.yml");
    let build = step_number(&farm, "build", "buck2 build //:ci on the farm, local 0");
    assert_eq!(
        lint(
            "farm.yml",
            &with_in(
                &farm,
                "        timeout-minutes: 150\n",
                "        timeout-minutes: 180\n"
            )
        ),
        [format!(
            "job build: step {build} (`buck2 build //:ci on the farm, local 0`): `timeout-minutes: 180` is not under the job's 180"
        )]
    );
    let mac = committed("mac-farm.yml");
    let compile = step_number(
        &mac,
        "compile",
        "Build hostkeeper, hk, hello and the bootstrap on a farm Mac",
    );
    let found = lint(
        "mac-farm.yml",
        &with_in(&mac, "        timeout-minutes: 50\n", ""),
    );
    assert!(
        found.contains(&format!(
            "job compile: step {compile} (`Build hostkeeper, hk, hello and the bootstrap on a farm Mac`) has no `timeout-minutes`, a whole number of minutes from 1"
        )),
        "{found:?}"
    );
}
