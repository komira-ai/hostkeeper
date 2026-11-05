//! The hook as the daemon reads it: every step and outcome through the hook contract crate (the
//! exit code through `Outcome`, the last line through both readers, what the updater runs next
//! through `next`), over a fake runner and a fake probe, in a scratch record directory.

mod support;

use std::collections::BTreeMap;

use hello::hook::Units;
use hostkeeper_hook::{Class, Context, Next, Outcome, Phase, Step, Verdict};
use support::{FakeProbe, FakeRunner, Form, Ran, TARGET, Want, World, PORT, contract, ctx};

const START_A: &str = "/usr/bin/systemctl start hello@a.service";
const START_B: &str = "/usr/bin/systemctl start hello@b.service";
const STOP_A: &str = "/usr/bin/systemctl stop hello@a.service";
const STOP_B: &str = "/usr/bin/systemctl stop hello@b.service";

type Snapshot = BTreeMap<String, Option<String>>;

fn files(entries: &[(&str, Option<&str>)]) -> Snapshot {
    entries
        .iter()
        .map(|(name, content)| ((*name).to_owned(), content.map(str::to_owned)))
        .collect()
}

fn failed(phase: Phase, class: Class) -> Verdict {
    Verdict::Failed { phase, class }
}

fn rollback_then(phase: Phase, class: Class) -> Next {
    Next::Rollback {
        then: failed(phase, class),
    }
}

/// Runs a step that must succeed with nothing stated but a detail.
fn go(world: &World, step: Step, runner: &FakeRunner, probe: &FakeProbe) -> Ran {
    let ran = world.step(step, runner, probe);
    assert_eq!(ran.code, 0, "{step} gave {ran:?}");
    ran
}

/// A full update: the four steps, each succeeding.
fn update(world: &World, runner: &FakeRunner, probe: &FakeProbe) {
    for step in [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup] {
        go(world, step, runner, probe);
    }
}

/// Catches a first install that does not run prepare, cutover, verify and cleanup in the order
/// the updater runs them, with the argv, the markers, the record and the lines the design gives.
#[test]
fn a_first_install_runs_the_four_steps() {
    let world = World::systemd("first-install");
    let runner = FakeRunner::default();
    let probe = FakeProbe::ready(TARGET);

    let ran = world.step(Step::Prepare, &runner, &probe);
    let want = Want {
        detail: Some("prepared slot a"),
        ..Want::new(Outcome::Success)
    };
    assert_eq!(contract(Step::Prepare, &ran, &want, &ctx()), Next::Run(Step::Cutover));
    assert_eq!(ran.stdout, "{\"class\":\"release\",\"detail\":\"prepared slot a\"}\n");
    assert_eq!(world.snapshot(), files(&[("next", Some("a")), ("prepared", Some(""))]));
    assert!(runner.take_calls().is_empty(), "prepare changes nothing a client sees");
    assert!(probe.take_calls().is_empty());

    let ran = world.step(Step::Cutover, &runner, &probe);
    let want = Want {
        detail: Some("started slot a"),
        ..Want::new(Outcome::Success)
    };
    assert_eq!(contract(Step::Cutover, &ran, &want, &ctx()), Next::Run(Step::Verify));
    assert_eq!(runner.take_calls(), [START_A]);
    assert_eq!(
        world.snapshot(),
        files(&[
            ("next", Some("a")),
            ("prepared", Some("")),
            ("cutover", Some("")),
            ("slots", None),
            ("slots/a", Some("")),
        ])
    );

    let ran = world.step(Step::Verify, &runner, &probe);
    let next = contract(Step::Verify, &ran, &Want::new(Outcome::Success), &ctx());
    assert_eq!(next, Next::Run(Step::Cleanup));
    assert_eq!(ran.stdout, "{\"class\":\"release\",\"promotion_pending\":false}\n");
    assert_eq!(
        probe.take_calls(),
        [(PORT, "/readyz".to_owned()), (PORT, "/version".to_owned())]
    );
    assert!(runner.take_calls().is_empty(), "verify never changes the units");
    assert!(world.snapshot().contains_key("verified"));

    let ran = world.step(Step::Cleanup, &runner, &probe);
    let next = contract(Step::Cleanup, &ran, &Want::new(Outcome::Success), &ctx());
    assert_eq!(next, Next::Report(Verdict::Succeeded { promotion_pending: false }));
    assert_eq!(ran.stdout, "{\"class\":\"release\"}\n");
    assert_eq!(
        world.snapshot(),
        files(&[
            ("active", Some("a")),
            ("verified", Some("")),
            ("slots", None),
            ("slots/a", Some("")),
        ])
    );
    assert!(runner.take_calls().is_empty(), "cleanup has nothing to stop: cutover did");
}

/// Catches an update that starts the slot that is already serving, stops the wrong one, or does
/// not alternate between the two.
#[test]
fn an_update_alternates_between_the_slots() {
    let world = World::systemd("alternate");
    let runner = FakeRunner::default();
    let probe = FakeProbe::ready(TARGET);
    update(&world, &runner, &probe);
    runner.take_calls();

    let ran = go(&world, Step::Prepare, &runner, &probe);
    assert!(ran.stdout.contains("prepared slot b"), "{ran:?}");
    go(&world, Step::Cutover, &runner, &probe);
    assert_eq!(runner.take_calls(), [STOP_A, START_B]);
    assert_eq!(
        world.snapshot(),
        files(&[
            ("active", Some("a")),
            ("next", Some("b")),
            ("prepared", Some("")),
            ("cutover", Some("")),
            ("verified", Some("")).into_iter().next().map_or(("verified", None), |c| ("verified", Some(c))),
            ("slots", None),
            ("slots/b", Some("")),
        ])
        .into_iter()
        .filter(|(name, _)| name != "verified")
        .collect::<Snapshot>()
    );
    go(&world, Step::Verify, &runner, &probe);
    go(&world, Step::Cleanup, &runner, &probe);
    let record = world.snapshot();
    assert_eq!(record.get("active"), Some(&Some("b".to_owned())));
    assert!(!record.contains_key("next") && !record.contains_key("slots/a"));

    go(&world, Step::Prepare, &runner, &probe);
    go(&world, Step::Cutover, &runner, &probe);
    assert_eq!(runner.take_calls(), [STOP_B, START_A]);
}
