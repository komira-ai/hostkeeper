//! The intake of the `maintain` order (docs/design/maintenance-updates.md, sections 5.1 to 5.4):
//! an order is started once however often it is delivered, a second turn is refused while one
//! holds the host or a package step is unfinished, an order that breaks its grammar, a record that
//! does not read and a release that cannot be read are each refused with nothing launched.

mod support;
mod support_maintain;

use std::fs;
use std::path::PathBuf;

use hostkeeper_daemon::locks::Holder;
use hostkeeper_daemon::maintenance_hook::Kind as HookKind;
use hostkeeper_daemon::verb::{ACTION_RULE, KINDS_RULE, NO_TRY, ORDER_ID_RULE, RUN_ID_RULE};
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1::{MaintainAction, MaintenanceKind};
use hostkeeper_steps::RecordStore;
use hostkeeper_steps::maintenance::{MAINTENANCE_RECORD_FILE, MaintenanceStore};
use hostkeeper_steps::record::{HookSel, Progress, Record, State};
use support::{Rig, name};
use support_maintain::{
    ORDER, RUN, draining, launched, maintain, order_of, plain, reboot_on_cutover, record, terms,
};

/// The three steps of the maintenance hook, launched in this order for a turn of no services.
fn hook_steps() -> Vec<String> {
    ["prepare", "cutover", "verify"]
        .iter()
        .map(|step| format!("hostkeeper {step}"))
        .collect()
}

/// The reply of a turn that ran to its end with nothing to say.
fn done(resent: bool) -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent,
    }
}

fn refused(refusal: Refusal, detail: &str) -> Reply {
    Reply::Refused {
        refusal,
        detail: detail.to_owned(),
    }
}

/// Where the run record is on disk.
fn record_file(rig: &Rig) -> PathBuf {
    MaintenanceStore::beside(&rig.state)
        .dir()
        .join(MAINTENANCE_RECORD_FILE)
}

/// Catches an order delivered twice that runs twice: the second delivery of a turn that is done
/// answers the end of the first (`resent`), launches nothing more and leaves the record as it
/// was.
#[test]
fn the_same_order_delivered_twice_runs_once() {
    let rig = Rig::new("maintain-twice");
    let daemon = rig.daemon();
    let order = maintain(&[MaintenanceKind::Linux]);
    assert_eq!(daemon.order(&order), done(false));
    assert_eq!(launched(&rig), hook_steps());
    let held = record(&rig).expect("the run record");
    assert_eq!(daemon.order(&order), done(true));
    assert_eq!(launched(&rig), hook_steps());
    assert_eq!(record(&rig), Some(held));
}

/// Catches a retry of the order, under a higher try, that starts the turn again: the record holds
/// the order, so a higher try answers from it (`resent`), launches nothing and does not take the
/// new try into the record.
#[test]
fn a_higher_try_of_a_done_order_answers_from_the_record() {
    let rig = Rig::new("maintain-higher-try");
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        done(false)
    );
    let held = record(&rig).expect("the run record");
    let again = order_of(ORDER, 3, terms(&[MaintenanceKind::Linux], None));
    assert_eq!(daemon.order(&again), done(true));
    assert_eq!(launched(&rig), hook_steps());
    assert_eq!(record(&rig), Some(held));
}

/// Catches a delivery of an older try that is acted on: with try 2 in the record, the order of
/// try 1 is refused `stale_try` naming both tries, launches nothing and leaves the record.
#[test]
fn a_lower_try_than_the_record_holds_is_refused_stale() {
    let rig = Rig::new("maintain-stale-try");
    let daemon = rig.daemon();
    let second = order_of(ORDER, 2, terms(&[MaintenanceKind::Linux], None));
    assert_eq!(daemon.order(&second), done(false));
    let held = record(&rig).expect("the run record");
    assert_eq!(held.order_try, 2);
    let reply = daemon.order(&maintain(&[MaintenanceKind::Linux]));
    assert_eq!(
        reply,
        refused(
            Refusal::StaleTry,
            "order order-1 try 1 is older than try 2, which the maintenance record holds"
        )
    );
    assert_eq!(launched(&rig), hook_steps());
    assert_eq!(record(&rig), Some(held));
}

/// Catches a resend of an order whose turn a sequencer runs now that starts a second one: with
/// the host's sequencer held for that order, its resend answers `Running` and launches nothing.
#[test]
fn a_resend_while_the_turn_runs_answers_running() {
    let rig = Rig::new("maintain-resend-running");
    let daemon = rig.daemon();
    let _hold = daemon
        .ctx()
        .maintenance
        .hold(ORDER)
        .expect("the sequencer is free");
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        Reply::Running
    );
    assert!(launched(&rig).is_empty());
    assert_eq!(record(&rig), None);
}

/// Catches a second order that runs beside a turn: with the sequencer held for `other-order`,
/// the order is refused `busy` naming that order, launches nothing and writes no record.
#[test]
fn another_order_while_a_turn_runs_is_refused_busy() {
    let rig = Rig::new("maintain-other-running");
    let daemon = rig.daemon();
    let _hold = daemon
        .ctx()
        .maintenance
        .hold("other-order")
        .expect("the sequencer is free");
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        refused(
            Refusal::Busy,
            "the maintenance turn of order other-order runs on this host"
        )
    );
    assert!(launched(&rig).is_empty());
    assert_eq!(record(&rig), None);
}

/// Catches a turn started over a package step under way: `alpha`'s lock is held, so the order is
/// refused `busy` naming the service and who holds it, with nothing launched and no record.
#[test]
fn a_held_service_lock_refuses_the_turn() {
    let rig = Rig::new("maintain-locked");
    plain(&rig, "alpha", "1.0.0");
    let daemon = rig.daemon();
    let _lock = daemon
        .ctx()
        .locks
        .take_for(&name("alpha"), Holder::ClearRecord)
        .expect("the lock is free");
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        refused(Refusal::Busy, "alpha is locked by a `clear_record`")
    );
    assert!(launched(&rig).is_empty());
    assert_eq!(record(&rig), None);
}

/// Catches a turn started over an update that did not finish: `web` has a step record that is
/// still pending, so the order is refused `busy` naming the service, with nothing launched and no
/// record.
#[test]
fn an_unfinished_step_record_refuses_the_turn() {
    let rig = Rig::new("maintain-unfinished");
    let dir = rig.state.join("web");
    fs::create_dir_all(&dir).expect("the state directory of web");
    let pending = Record {
        schema: 1,
        service: "web".to_owned(),
        target_version: "2.0.0".to_owned(),
        artifact_digest: "cd".repeat(32),
        tarball_digest: "ab".repeat(32),
        previous_digest: None,
        generation: 1,
        params: dir.join("params.toml"),
        step: Step::Prepare,
        attempt: 1,
        state: State::Pending,
        hook: HookSel::New,
        progress: Progress::default(),
        order_try: Some(1),
        forced: false,
        force: None,
    };
    RecordStore::new(&dir)
        .save(&pending)
        .expect("the step record");
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        refused(Refusal::Busy, "a package step of web is unfinished")
    );
    assert!(launched(&rig).is_empty());
    assert_eq!(record(&rig), None);
}

/// Catches a second turn started while the first waits for its reboot: with the record of
/// `order-1` waiting for the reboot of `linux`, `order-2` is refused `busy` naming that order, its
/// run and what it waits for, and launches nothing more.
#[test]
fn another_order_while_a_turn_waits_for_a_reboot_is_refused_busy() {
    let rig = Rig::new("maintain-waiting");
    reboot_on_cutover(&rig, HookKind::Linux, MaintenanceKind::Linux);
    let daemon = rig.daemon();
    assert_eq!(
        daemon.order(&maintain(&[MaintenanceKind::Linux])),
        Reply::Running
    );
    let waiting = launched(&rig);
    assert_eq!(waiting, ["hostkeeper prepare", "hostkeeper cutover"]);
    let held = record(&rig).expect("the run record");
    let other = order_of("order-2", 1, terms(&[MaintenanceKind::Linux], None));
    assert_eq!(
        daemon.order(&other),
        refused(
            Refusal::Busy,
            &format!(
                "the maintenance turn of order {ORDER} (run {RUN}) stands at wait for the reboot of linux"
            )
        )
    );
    assert_eq!(launched(&rig), waiting);
    assert_eq!(record(&rig), Some(held));
}

/// Catches an order whose fields are not checked before anything runs: each is refused
/// `invalid_order` naming the field and its rule (the action that is not `RUN`, a run id that is
/// empty, kinds that are none, unspecified or repeated, an order id of 65 bytes, a try of 0), with
/// nothing launched and no record.
#[test]
fn an_order_that_breaks_its_grammar_is_refused_before_anything_runs() {
    let rig = Rig::new("maintain-grammar");
    let daemon = rig.daemon();
    let linux = MaintenanceKind::Linux;
    let with_action = |action: MaintainAction| {
        let mut terms = terms(&[linux], None);
        terms.action = action.into();
        order_of(ORDER, 1, terms)
    };
    let mut no_run_id = terms(&[linux], None);
    no_run_id.run_id = String::new();
    let long_id = "x".repeat(65);
    let cases = [
        (
            with_action(MaintainAction::Retry),
            format!("the order's maintain.action {ACTION_RULE}"),
        ),
        (
            with_action(MaintainAction::PutBack),
            format!("the order's maintain.action {ACTION_RULE}"),
        ),
        (
            order_of(ORDER, 1, no_run_id),
            format!("the order's maintain.run_id {RUN_ID_RULE}"),
        ),
        (
            maintain(&[]),
            format!("the order's maintain.kinds {KINDS_RULE}"),
        ),
        (
            maintain(&[MaintenanceKind::Unspecified]),
            format!("the order's maintain.kinds {KINDS_RULE}"),
        ),
        (
            maintain(&[linux, linux]),
            format!("the order's maintain.kinds {KINDS_RULE}"),
        ),
        (
            order_of(&long_id, 1, terms(&[linux], None)),
            format!("the order's order_id {ORDER_ID_RULE}"),
        ),
        (
            order_of(ORDER, 0, terms(&[linux], None)),
            format!("the order's try {NO_TRY}"),
        ),
    ];
    for (order, detail) in cases {
        assert_eq!(
            daemon.order(&order),
            refused(Refusal::InvalidOrder, &detail),
            "{detail}"
        );
    }
    assert!(launched(&rig).is_empty());
    assert_eq!(record(&rig), None);
}

/// Catches an order started over a record that does not read: garbage in the record file is
/// refused `record_unreadable`, and a record of schema 2 `record_newer`; neither is written over
/// and nothing is launched.
#[test]
fn a_record_that_does_not_read_refuses_the_order_and_is_left_as_it_is() {
    let rig = Rig::new("maintain-record-unreadable");
    let file = record_file(&rig);
    fs::create_dir_all(file.parent().expect("its directory")).expect("the directory");
    let daemon = rig.daemon();
    let order = maintain(&[MaintenanceKind::Linux]);

    fs::write(&file, "not json at all").expect("the garbage");
    let reply = daemon.order(&order);
    assert_eq!(reply.refusal(), Some(Refusal::RecordUnreadable));
    assert!(matches!(&reply, Reply::Refused { detail, .. }
        if detail.starts_with("the maintenance record is not readable: ")));
    assert_eq!(
        fs::read_to_string(&file).expect("the file"),
        "not json at all"
    );

    let newer = r#"{"schema":2,"run_id":"run-9"}"#;
    fs::write(&file, newer).expect("the newer record");
    assert_eq!(
        daemon.order(&order),
        refused(
            Refusal::RecordNewer,
            "the maintenance record has schema 2, of a newer hostkeeper; it is left as it is"
        )
    );
    assert_eq!(fs::read_to_string(&file).expect("the file"), newer);
    assert!(launched(&rig).is_empty());
}

/// Catches a draining service passed over when its release cannot be read: `alpha`'s tree no
/// longer matches its seal, so the order is refused `release_unreadable` naming `alpha`, with
/// nothing launched (no service is left serving while the host updates) and no record.
#[test]
fn a_release_that_no_longer_matches_its_seal_refuses_the_turn() {
    let rig = Rig::new("maintain-release-unreadable");
    draining(&rig, "alpha", "1.0.0");
    fs::write(
        rig.packages.join("alpha/1.0.0/tree/extra"),
        "added after the seal",
    )
    .expect("a file added to the tree");
    let daemon = rig.daemon();
    let reply = daemon.order(&maintain(&[MaintenanceKind::Linux]));
    assert_eq!(reply.refusal(), Some(Refusal::ReleaseUnreadable));
    assert!(
        matches!(&reply, Reply::Refused { detail, .. }
            if detail.starts_with("alpha: ") && detail.ends_with("is not the tree its seal names")),
        "{reply:?}"
    );
    assert!(launched(&rig).is_empty());
    assert_eq!(record(&rig), None);
}
