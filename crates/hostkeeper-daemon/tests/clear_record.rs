//! `clear_record` from a coordinator's order (two-programs.md section 3.2): only a step record
//! that does not read is set aside, as `step-record.json.corrupt` with its bytes, and nothing
//! else of the host changes; a record that reads, a held lock and a store that fails are refused
//! with their tokens. The order is taken whatever the record says, and the last test runs it end
//! to end: the real coordinator's `Hosts.ClearRecord`, the real heartbeat loop and the real
//! executor, on a host whose record of the service does not read.

mod support;

use std::fs;
use std::path::PathBuf;

use hostkeeper_config::names::Version;
use hostkeeper_coordinator::operator::Caller;
use hostkeeper_daemon::halt::HELD_HALT_FILE;
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::verbs::clear_record::{CORRUPT_FILE, REASON_RECORD_CLEARED};
use hostkeeper_daemon::{Refusal, Reply, Verb};
use hostkeeper_hook::Verdict;
use hostkeeper_link::{ANNOUNCED, Feature};
use hostkeeper_proto::hostkeeper::agent::v1::{self as agent, ClearRecord, Order, order};
use hostkeeper_proto::hostkeeper::operator::v1::ClearRecordRequest;
use hostkeeper_steps::record::{PARAMS_FILE, RECORD_FILE};
use support::coordinated::{Host, coordinator_of, on_host};
use support::{Rig, Scratch, d, name, snapshot, update};

/// A record of the step runner's committed fixtures of schema 1 (and one of schema 2).
fn fixture(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hostkeeper-steps/tests/fixtures/kbf-v0.0.64")
        .join(file);
    fs::read_to_string(path).expect("the fixture")
}

/// `web`'s state directory, made, with `record` as its step record.
fn with_record(rig: &Rig, record: &[u8]) -> PathBuf {
    let dir = rig.state.join("web");
    fs::create_dir_all(&dir).expect("the service's directory");
    fs::write(dir.join(RECORD_FILE), record).expect("the record");
    dir
}

/// The coordinator's `clear_record` order of `service`, as `Hosts.ClearRecord` queues it.
fn clear(service: &str) -> Order {
    Order {
        order_id: "clear-record-7".to_owned(),
        r#try: 1,
        verb: Some(order::Verb::ClearRecord(ClearRecord {
            service: service.to_owned(),
        })),
    }
}

/// The answer to an order that set the record aside.
fn cleared() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: Some(REASON_RECORD_CLEARED.to_owned()),
        resent: false,
    }
}

/// The update of `service` to 2.0.0, the lock an order of it holds.
fn update_lock() -> Package {
    Package {
        verb: HookedVerb::Update,
        version: Version::new("2.0.0").expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
    }
}

/// Catches a record that reads removed by the order: an absent record, one in each state
/// (`pending`, `running`, `done`, `finished`), and one of a newer schema are each refused
/// `record_reads` with their own text, and the state directory is byte for byte as it was.
#[test]
fn a_clear_record_order_refuses_a_record_that_reads_fine() {
    let rig = Rig::new("clear-reads");
    let daemon = rig.daemon();
    fs::create_dir_all(rig.state.join("web")).expect("the service's directory");
    let reads = |what: &str| {
        Reply::refused(
            Refusal::RecordReads,
            format!("the step record of web {what}; only a corrupt one is cleared"),
        )
    };
    let before = snapshot(&rig.state);
    assert_eq!(daemon.order(&clear("web")), reads("is absent"));
    assert_eq!(snapshot(&rig.state), before);
    let rows = [
        ("pending.json", "reads"),
        ("running.json", "reads"),
        ("done.json", "reads"),
        ("finished.json", "reads"),
        ("newer-schema.json", "has schema 2, of a newer hostkeeper"),
    ];
    for (file, what) in rows {
        with_record(&rig, fixture(file).as_bytes());
        let before = snapshot(&rig.state);
        assert_eq!(daemon.order(&clear("web")), reads(what), "{file}");
        assert_eq!(snapshot(&rig.state), before, "{file}");
    }
}

/// Catches a service name that breaks its rule reaching the state directory: the
/// `clear_record` order and the `abort` order of `a b` are each refused `invalid_order`, and the
/// state directory is byte for byte as it was.
#[test]
fn a_clear_record_or_abort_order_of_a_bad_service_name_is_invalid_order() {
    let rig = Rig::new("clear-bad-name");
    let daemon = rig.daemon();
    with_record(&rig, b"{");
    let before = snapshot(&rig.state);
    let abort = Order {
        order_id: "abort-bad".to_owned(),
        r#try: 1,
        verb: Some(order::Verb::Abort(agent::Abort {
            service: "a b".to_owned(),
        })),
    };
    for order in [clear("a b"), abort] {
        let reply = daemon.order(&order);
        assert_eq!(reply.refusal(), Some(Refusal::InvalidOrder), "{reply:?}");
    }
    assert_eq!(snapshot(&rig.state), before);
}

/// Catches an order that deletes anything beyond the record, or leaves the corrupt record in
/// place: an empty record, a truncated one and one with a field of the wrong type are each
/// renamed to `step-record.json.corrupt` with the same bytes (replacing the one before) and
/// answered `succeeded` with the reason `record_cleared`; the held halt, the params file, the
/// package directory with `current` and `previous`, and another service's state directory are
/// byte for byte as they were; and the next order of the service runs as generation 1.
#[test]
fn a_clear_record_order_clears_a_corrupt_record_and_nothing_else() {
    let rig = Rig::new("clear-corrupt");
    rig.running("web", "1.0.0");
    rig.stage("web", "0.9.0");
    rig.store.link("web", "previous", "0.9.0");
    rig.add("web", "2.0.0");
    rig.add("api", "2.0.0");
    let daemon = rig.daemon();
    let succeeded = Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    };
    assert_eq!(daemon.order(&update("api", "2.0.0", &d(), 1)), succeeded);
    let finished = fixture("finished.json");
    let truncated = &finished.as_bytes()[..finished.len() / 2];
    let wrong_type = finished.replace("\"generation\": 13", "\"generation\": \"thirteen\"");
    assert_ne!(wrong_type, finished, "the fixture has the field");
    let dir = with_record(&rig, b"");
    fs::write(dir.join(HELD_HALT_FILE), "3\n").expect("a held halt");
    fs::write(dir.join(PARAMS_FILE), "left = 1\n").expect("a params file");
    let kept = || {
        let web = [HELD_HALT_FILE, PARAMS_FILE].map(|file| fs::read(dir.join(file)).ok());
        (
            web,
            snapshot(&rig.packages),
            snapshot(&rig.state.join("api")),
        )
    };
    let before = kept();
    for corrupt in [&b""[..], truncated, wrong_type.as_bytes()] {
        with_record(&rig, corrupt);
        assert_eq!(daemon.order(&clear("web")), cleared());
        assert!(!dir.join(RECORD_FILE).exists());
        assert_eq!(
            fs::read(dir.join(CORRUPT_FILE)).expect("set aside"),
            corrupt
        );
        assert_eq!(kept(), before);
    }
    assert_eq!(daemon.order(&update("web", "2.0.0", &d(), 1)), succeeded);
    assert_eq!(rig.record("web").expect("the record").generation, 1);
}

/// Catches an order that ignores the service's lock: with the lock held, the order of a corrupt
/// record is refused `busy`, naming the holder, and the record stays where it is.
#[test]
fn a_clear_record_order_is_refused_while_the_service_lock_is_held() {
    let rig = Rig::new("clear-record-busy");
    let daemon = rig.daemon();
    let dir = with_record(&rig, b"{");
    let guard = daemon
        .ctx()
        .locks
        .take(&name("web"), update_lock())
        .expect("free");
    let detail = "web is locked by an order to update it to 2.0.0";
    assert_eq!(
        daemon.order(&clear("web")),
        Reply::refused(Refusal::Busy, detail)
    );
    assert_eq!(fs::read(dir.join(RECORD_FILE)).expect("in place"), b"{");
    drop(guard);
}

/// Catches the order still refused, or run only by one of its two paths: the wire's
/// `clear_record` case given to `order` and the verb given to `execute` each clear a corrupt
/// record and answer `record_cleared`, and neither leaves the service's lock held.
#[test]
fn a_clear_record_order_is_accepted_by_both_of_the_executors_paths() {
    let rig = Rig::new("clear-record-order");
    let daemon = rig.daemon();
    let dir = with_record(&rig, b"{");
    assert_eq!(daemon.order(&clear("web")), cleared());
    assert!(!dir.join(RECORD_FILE).exists());
    assert_eq!(daemon.ctx().locks.holder(&name("web")), None);
    with_record(&rig, b"[");
    assert_eq!(daemon.execute(Verb::ClearRecord(name("web"))), cleared());
    assert_eq!(fs::read(dir.join(CORRUPT_FILE)).expect("set aside"), b"[");
    assert_eq!(daemon.ctx().locks.holder(&name("web")), None);
}

/// Catches a record that cannot be read at all taken for a corrupt one: with a directory in the
/// record's place the order is `store_failed`, and nothing is renamed.
#[test]
fn a_clear_record_order_of_a_record_that_cannot_be_read_is_store_failed() {
    let rig = Rig::new("clear-record-dir");
    let daemon = rig.daemon();
    let dir = rig.state.join("web");
    fs::create_dir_all(dir.join(RECORD_FILE)).expect("a directory in the record's place");
    let reply = daemon.order(&clear("web"));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(matches!(reply, Reply::Refused { .. }), "{reply:?}");
    assert!(dir.join(RECORD_FILE).is_dir());
    assert!(!dir.join(CORRUPT_FILE).exists());
}

/// Catches a rename that fails reported as cleared: with a directory that holds a file in the
/// place of `step-record.json.corrupt`, the rename fails, the order is `store_failed`, and the
/// corrupt record is still where it was.
#[test]
fn a_clear_record_order_whose_rename_fails_is_store_failed_and_leaves_the_record() {
    let rig = Rig::new("clear-record-rename");
    let daemon = rig.daemon();
    let dir = with_record(&rig, b"{");
    fs::create_dir_all(dir.join(CORRUPT_FILE).join("in-the-way")).expect("a directory in the way");
    let reply = daemon.order(&clear("web"));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(matches!(reply, Reply::Refused { .. }), "{reply:?}");
    assert_eq!(fs::read(dir.join(RECORD_FILE)).expect("in place"), b"{");
}

/// Catches a daemon the coordinator never sends the order (it did not announce
/// `clear-record-v1`), a loop that drops the order or refuses it while the record does not read,
/// and a verdict that never reaches the coordinator: `web` runs 2.0.0, the version the
/// coordinator wants, and its step record does not read; an operator's `Hosts.ClearRecord` is
/// answered at the next heartbeat with the `clear_record` order; the real executor sets the
/// record aside; and the heartbeat after reports the order's verdict, `succeeded` at 2.0.0 with
/// the reason `record_cleared` under the order's id, which `Hosts.Get` shows.
#[test]
fn the_order_is_taken_by_a_daemon_whose_record_of_the_service_does_not_read() {
    const TARGET: &str = "2.0.0";
    let rig = Rig::new("clear-record-e2e");
    rig.running("web", TARGET);
    let dir = with_record(&rig, b"{ not a record");
    let scratch = Scratch::new("clear-record-e2e-coordinator");
    let coordinator = coordinator_of(scratch.path(), "web", TARGET);
    let mut host = Host::new(&rig, &coordinator);
    assert!(ANNOUNCED.contains(&Feature::ClearRecordV1));

    host.beat();
    assert_eq!(host.ordered(), [None]);
    let operator = Caller::Operator("operator".parse().expect("a token name"));
    let request = ClearRecordRequest {
        node_id: "host-1".to_owned(),
        service: "web".to_owned(),
    };
    let order_id = coordinator
        .clear_record(&operator, &request)
        .expect("ordered")
        .order_id;
    host.beat();
    let ordered = host.ordered()[1].clone().expect("an order");
    assert_eq!(ordered.id.as_str(), order_id);
    assert_eq!(
        ordered.verb,
        hostkeeper_link::Verb::ClearRecord(name("web"))
    );
    assert!(host.dispatch.run_next());
    assert!(!dir.join(RECORD_FILE).exists());
    assert_eq!(
        fs::read(dir.join(CORRUPT_FILE)).expect("set aside"),
        b"{ not a record"
    );

    host.beat();
    assert_eq!(host.ordered()[2], None);
    let reported = on_host(&coordinator, "web")
        .and_then(|service| service.last_update)
        .expect("the verdict reached the coordinator");
    assert_eq!(reported.order_id.as_deref(), Some(order_id.as_str()));
    assert_eq!(reported.order_try, Some(1));
    assert_eq!(reported.target, TARGET);
    assert_eq!(reported.outcome(), agent::Outcome::Succeeded);
    assert_eq!(reported.reason, REASON_RECORD_CLEARED);
}
