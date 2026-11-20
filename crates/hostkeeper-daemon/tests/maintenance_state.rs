//! The report's run state of a maintenance run record (`maintenance_state::run_state`): each step
//! of a turn in the wire's words, the kinds applied in order, the reason of a deferral or a halt,
//! cut to the wire's bound, and the ends of section 5.7 (docs/design/maintenance-updates.md).

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::maintenance_state::run_state;
use hostkeeper_proto::hostkeeper::agent::v1 as wire;
use hostkeeper_proto::maintenance::{MAX_HALTED_REASON, check_report};
use hostkeeper_steps::BootId;
use hostkeeper_steps::maintenance::{
    Action, Event, Kind, Launch, MaintenanceRecord, Outcome, next_action, transition,
};

fn record(kinds: &[Kind], services: &[&str]) -> MaintenanceRecord {
    let names = services
        .iter()
        .map(|name| ServiceName::new(name).expect("a service name"))
        .collect();
    MaintenanceRecord::new("run-1", "order-1", 3, kinds, names).expect("a valid record")
}

fn on(rec: &MaintenanceRecord, event: Event) -> MaintenanceRecord {
    transition(rec, event).expect("the event fits").0
}

fn boot(n: u32) -> BootId {
    BootId::new("a test", &format!("boot-{n}")).expect("a boot id")
}

/// Launches and ends the step `rec` stands at with `outcome`.
fn ended(rec: &MaintenanceRecord, outcome: Outcome) -> MaintenanceRecord {
    on(&on(rec, Event::Started), Event::Ended(outcome))
}

/// `rec` with every step it stands at ended well until it stands at `at`.
fn ok_until(mut rec: MaintenanceRecord, at: &Action) -> MaintenanceRecord {
    while next_action(&rec) != *at {
        rec = ended(&rec, Outcome::Ok);
    }
    rec
}

fn step_of(rec: &MaintenanceRecord) -> wire::MaintenanceStep {
    wire::MaintenanceStep::try_from(run_state(rec).step).expect("a known step")
}

fn reported(rec: &MaintenanceRecord) -> wire::MaintenanceReport {
    wire::MaintenanceReport {
        run: Some(run_state(rec)),
        ..wire::MaintenanceReport::default()
    }
}

/// Catches a step of the turn reported as another (a record not yet staged as staging, a reboot
/// as applying, a put back as done), and an id or a reason given where the wire wants none.
#[test]
fn each_step_of_a_turn_is_reported_in_the_wire_s_words() {
    let new = record(&[Kind::Linux, Kind::Reboot], &["js-agent"]);
    let staging = on(&new, Event::Started);
    let draining = ok_until(new.clone(), &drain("js-agent"));
    let applying = ok_until(new.clone(), &Action::Launch(Launch::Apply(Kind::Reboot)));
    let rebooting = on(
        &on(&applying, Event::Started),
        Event::RebootUnderWay(boot(1)),
    );
    let checking = on(&rebooting, Event::BootIdRead(boot(2)));
    let putting_back = ended(&checking, Outcome::Ok);
    let done = ended(&putting_back, Outcome::Ok);
    let steps = [
        (&new, wire::MaintenanceStep::Waiting),
        (&staging, wire::MaintenanceStep::Staging),
        (&draining, wire::MaintenanceStep::Draining),
        (&applying, wire::MaintenanceStep::Applying),
        (&rebooting, wire::MaintenanceStep::Rebooting),
        (&checking, wire::MaintenanceStep::Checking),
        (&putting_back, wire::MaintenanceStep::PuttingBack),
        (&done, wire::MaintenanceStep::Done),
    ];
    for (rec, step) in steps {
        let state = run_state(rec);
        assert_eq!(step_of(rec), step);
        assert_eq!(
            (
                state.run_id.as_str(),
                state.order_id.as_str(),
                state.halted_reason.as_str()
            ),
            ("run-1", "order-1", ""),
            "{step:?}"
        );
        assert_eq!(check_report(&reported(rec)), Ok(()), "{step:?}");
    }
}

fn drain(name: &str) -> Action {
    Action::Launch(Launch::Drain(
        ServiceName::new(name).expect("a service name"),
    ))
}

/// Catches the kinds applied reported out of the order applied, or a kind reported applied
/// before its reboot brought the host back.
#[test]
fn the_kinds_applied_are_reported_in_the_order_applied() {
    let rec = ok_until(
        record(&[Kind::Xcode, Kind::Macos], &[]),
        &Action::Launch(Launch::Apply(Kind::Macos)),
    );
    assert_eq!(run_state(&rec).applied, Vec::<i32>::new());
    let rebooting = on(&on(&rec, Event::Started), Event::RebootUnderWay(boot(1)));
    assert_eq!(run_state(&rebooting).applied, Vec::<i32>::new());
    let back = on(&rebooting, Event::BootIdRead(boot(2)));
    assert_eq!(
        run_state(&back).applied,
        [i32::from(wire::MaintenanceKind::Macos)]
    );
    let both = ended(&back, Outcome::Ok);
    assert_eq!(
        run_state(&both).applied,
        [
            i32::from(wire::MaintenanceKind::Macos),
            i32::from(wire::MaintenanceKind::Xcode)
        ]
    );
    let linux = ok_until(
        record(&[Kind::Reboot, Kind::Linux], &[]),
        &Action::Launch(Launch::Check),
    );
    assert_eq!(
        run_state(&linux).applied,
        [
            i32::from(wire::MaintenanceKind::Linux),
            i32::from(wire::MaintenanceKind::Reboot)
        ]
    );
}

/// Catches the ends of section 5.7 misreported: done, deferred (stage failed, or the drain
/// deadline passed, with the reason), halted (with the reason) and not back (still rebooting).
#[test]
fn the_ends_of_a_turn_are_reported_with_their_reasons() {
    let stage_failed = ended(
        &record(&[Kind::Linux], &[]),
        Outcome::Failed("exit 1".to_owned()),
    );
    let state = run_state(&stage_failed);
    assert_eq!(step_of(&stage_failed), wire::MaintenanceStep::Deferred);
    assert_eq!(state.halted_reason, "stage: exit 1");

    let past_deadline = ended(
        &ok_until(record(&[Kind::Linux], &["js-agent"]), &drain("js-agent")),
        Outcome::Deferred("the drain deadline passed".to_owned()),
    );
    assert_eq!(step_of(&past_deadline), wire::MaintenanceStep::PuttingBack);
    assert_eq!(run_state(&past_deadline).halted_reason, "");
    let deferred = ended(&past_deadline, Outcome::Ok);
    assert_eq!(step_of(&deferred), wire::MaintenanceStep::Deferred);
    assert_eq!(
        run_state(&deferred).halted_reason,
        "drain js-agent: the drain deadline passed"
    );

    let halted = ended(
        &ok_until(record(&[Kind::Linux], &["js-agent"]), &drain("js-agent")),
        Outcome::Failed("exit 1".to_owned()),
    );
    assert_eq!(step_of(&halted), wire::MaintenanceStep::Halted);
    assert_eq!(run_state(&halted).halted_reason, "drain js-agent: exit 1");

    let applying = ok_until(
        record(&[Kind::Linux], &["js-agent"]),
        &Action::Launch(Launch::Apply(Kind::Linux)),
    );
    let not_back = on(
        &on(&applying, Event::Started),
        Event::RebootUnderWay(boot(1)),
    );
    let still = on(&not_back, Event::BootIdRead(boot(1)));
    assert_eq!(step_of(&still), wire::MaintenanceStep::Rebooting);
}

/// Catches a reason longer than the wire's bound sent whole (so the whole report is refused),
/// or cut inside a character; a reason within the bound is sent as it is.
#[test]
fn a_reason_is_cut_to_the_wire_s_bound_at_a_character_boundary() {
    let halt = |detail: String| {
        ended(
            &ok_until(record(&[Kind::Linux], &[]), &Action::Launch(Launch::Check)),
            Outcome::Failed(detail),
        )
    };
    // "check: " is 7 bytes; then 248 bytes of `a` make 255, and `é` (2 bytes) would end at 257.
    let long = halt(format!("{}é and more", "a".repeat(MAX_HALTED_REASON - 8)));
    let reason = run_state(&long).halted_reason;
    assert_eq!(
        reason,
        format!("check: {}", "a".repeat(MAX_HALTED_REASON - 8))
    );
    assert_eq!(reason.len(), MAX_HALTED_REASON - 1);
    assert_eq!(check_report(&reported(&long)), Ok(()));

    let exact = halt("b".repeat(MAX_HALTED_REASON - 7));
    let reason = run_state(&exact).halted_reason;
    assert_eq!(
        reason,
        format!("check: {}", "b".repeat(MAX_HALTED_REASON - 7))
    );
    assert_eq!(reason.len(), MAX_HALTED_REASON);
}
