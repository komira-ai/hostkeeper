//! A change whose audit line cannot be appended: the operator's verbs that steer the rollout
//! (`Releases.Force`, `Services.Hold`, `Releases.SetDesired`, and every other that changes the
//! ops state) are refused `audit_unavailable` and change nothing; `Hosts.Abort` and
//! `Releases.Halt`, the operator's way out, stand, logged, with their entries in the ring; and
//! `Audit.List` still answers.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde_json::json;

use crate::audit_file::{self, Appender, AuditLog, DEFAULT_ROTATION};
use crate::grpc::operator::set_refusal;
use crate::marks_support::{
    DB, NEW_DAEMON, WEB, ask, ask_typed, beat_of, digest, fleet_target, force_request, hold,
    operator, rig, runs, scope_fleet, scope_host, set, start, unhold,
};
use crate::ops::{AuditEntry, OpsError, OpsState, OpsStore, Write};
use crate::orders::Order;
use crate::service::SetError;
use crate::service::ci_calls::Setter;
use crate::testutil::{Log, Rig, ops_of, scratch, version};
use crate::write_record::WriteAsk;

/// The role of every host of these tests.
const DAEMON: &str = "daemon";

/// The files of the rig a refused change must leave as they were, by name.
const FILES: [&str; 3] = ["desired.json", "desired.json.ops", "desired.json.ops.audit"];

/// The bytes of [`FILES`], `None` for a file that is not there.
fn files(rig: &Rig) -> Vec<Option<Vec<u8>>> {
    FILES
        .iter()
        .map(|name| std::fs::read(rig.dir.path().join(name)).ok())
        .collect()
}

/// The ops file of the rig.
fn ops_path(rig: &Rig) -> PathBuf {
    rig.dir.path().join("desired.json.ops")
}

/// An appender that fails with a full disk while `down` is set, and otherwise appends to the
/// audit file of `ops` as the store's own would.
fn switchable(ops: &std::path::Path, down: &Arc<AtomicBool>) -> Appender {
    let log = AuditLog::open(audit_file::path_of(ops), DEFAULT_ROTATION).expect("opened");
    let down = Arc::clone(down);
    Box::new(move |line| {
        if down.load(Ordering::SeqCst) {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        log.append(line)
    })
}

/// The heartbeat of `d1`, a new daemon running `web` and `db` at 1.0.
fn d1_beat() -> hostkeeper_wire::wire::Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, "1.0"), runs(DB, "1.0")])
}

/// A rig whose coordinator cannot append to its audit file while the flag answered is set (it
/// starts set): `d1` heard, 1.0 and 2.0 of `web` registered and 2.0 desired, and one hold of
/// `db` on `d1` made, audited, before the disk filled.
async fn full_disk(test: &str) -> (Rig, Arc<AtomicBool>) {
    let mut rig = rig(test, "");
    ask(&rig, "d1", DAEMON, d1_beat());
    set(&rig, WEB, "1.0", 1).await;
    set(&rig, WEB, "2.0", 2).await;
    hold(&rig, DB, scope_host("d1")).expect("held");
    let down = Arc::new(AtomicBool::new(true));
    let ops = ops_of(&rig.dir).with_append(switchable(&ops_path(&rig), &down));
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(ops);
    ask(&rig, "d1", DAEMON, d1_beat());
    (rig, down)
}

/// What a test compares before and after a refused change: the files, the ops state and the
/// desired version of `web`.
fn snapshot(rig: &Rig) -> (Vec<Option<Vec<u8>>>, OpsState, Option<String>) {
    let desired = rig
        .coordinator
        .desired
        .get(WEB)
        .map(|desired| desired.version.to_string());
    (files(rig), rig.coordinator.ops().state(), desired)
}

/// The operator's set of `web` to `version`, with the digest registered for it.
async fn operator_set(rig: &Rig, version: &str, n: u8) -> Result<(), SetError> {
    let body = json!({"service": WEB, "version": version, "release_digest": digest(n)});
    rig.coordinator
        .set_desired_by(
            &Setter::Operator("operator"),
            body.to_string().as_bytes(),
            &WriteAsk::default(),
        )
        .await
        .map(|(set, _)| set)
        .map(drop)
}

/// Proves: with the audit file taking no lines, `Releases.Force`, `Services.Hold` and the
/// operator's `Releases.SetDesired` are each refused `audit_unavailable`, and the desired state, the ops state (holds, force, ring) and
/// the files on disk are byte for byte as they were; once the file takes lines again the same
/// three calls are made, so the refusal was the audit's.
///
/// Catches: a verb that applies its change and only logs the failed append, one that writes the
/// desired state before its audit (`set_desired`), or a refusal reported as `store_failed`.
#[tokio::test]
async fn force_hold_set_desired_refuse_when_append_fails() {
    let (rig, down) = full_disk("audit-refuse-verbs").await;
    let before = snapshot(&rig);

    let forced = rig.coordinator.force(
        &operator(),
        force_request(WEB, "1.0", fleet_target(WEB), false),
    );
    assert_eq!(forced, Err(proto::ReleasesRefusal::AuditUnavailable));
    assert_eq!(snapshot(&rig), before);

    assert_eq!(
        hold(&rig, WEB, scope_fleet()),
        Err(proto::ServicesRefusal::AuditUnavailable)
    );
    assert_eq!(snapshot(&rig), before);

    let refused = operator_set(&rig, "1.0", 1).await.expect_err("refused");
    assert!(
        matches!(&refused, SetError::Ops(OpsError::AuditUnavailable(_))),
        "{refused:?}"
    );
    assert_eq!(
        set_refusal(refused),
        proto::ReleasesRefusal::AuditUnavailable
    );
    assert_eq!(snapshot(&rig), before);

    down.store(false, Ordering::SeqCst);
    hold(&rig, WEB, scope_fleet()).expect("held once the audit takes lines");
    operator_set(&rig, "1.0", 1)
        .await
        .expect("set once the audit takes lines");
    rig.coordinator
        .force(
            &operator(),
            force_request(WEB, "2.0", fleet_target(WEB), false),
        )
        .expect("forced once the audit takes lines");
    let actions: Vec<String> = rig
        .coordinator
        .ops()
        .audit_log()
        .expect("the log")
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert_eq!(actions.first().map(String::as_str), Some("hold"));
    assert!(actions.contains(&"set_desired".to_owned()), "{actions:?}");
    assert!(actions.contains(&"force".to_owned()), "{actions:?}");
}

/// Proves: the other verbs that change the ops state are refused too, each with its service's
/// `audit_unavailable` and nothing changed: `Services.Unhold`, `Releases.MarkBad`,
/// `Hosts.Exclude` and `Enrollment.AddRecord`.
///
/// Catches: a verb outside force, hold and set-desired that still changes the state without its
/// audit line, or a refusal enum that maps the failure to `store_failed`.
#[tokio::test]
async fn every_other_changing_verb_refuses_when_append_fails() {
    let (rig, _down) = full_disk("audit-refuse-others").await;
    let before = snapshot(&rig);
    let c = &rig.coordinator;

    assert_eq!(
        unhold(&rig, DB, scope_host("d1")),
        Err(proto::ServicesRefusal::AuditUnavailable)
    );
    let mark = proto::MarkBadRequest {
        service: WEB.to_owned(),
        version: "1.0".to_owned(),
        reason: "bad build".to_owned(),
    };
    assert_eq!(
        c.mark_bad(&operator(), mark),
        Err(proto::ReleasesRefusal::AuditUnavailable)
    );
    let exclude = proto::ExcludeRequest {
        service: WEB.to_owned(),
        node_id: "d1".to_owned(),
        reason: "disk swap".to_owned(),
        abandon: false,
    };
    assert_eq!(
        c.exclude(&operator(), exclude),
        Err(proto::HostsRefusal::AuditUnavailable)
    );
    let add = proto::AddRecordRequest {
        host_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        ..proto::AddRecordRequest::default()
    };
    assert_eq!(
        c.add_record(&operator(), add),
        Err(proto::EnrollmentRefusal::AuditUnavailable)
    );
    assert_eq!(snapshot(&rig), before);
}

/// Proves: with the audit file taking no lines, `Hosts.Abort` and `Releases.Halt` succeed and
/// take effect: the abort is sent on `d1`'s next typed heartbeat and `web` is halted; the ring
/// holds both entries as answered, the audit file holds neither, both are pending, and each
/// failed commit is logged at error as a change that stands.
///
/// Catches: the operator's way out refused on a full disk, or an abort or a halt that stands
/// with no record anywhere.
#[tokio::test]
async fn abort_and_halt_proceed_when_append_fails() {
    let (rig, _down) = full_disk("audit-escape").await;
    let log = Log::default();
    let _guard = log.install();
    let audit_before = files(&rig)[2].clone();

    let abort = proto::AbortRequest {
        node_id: "d1".to_owned(),
        service: WEB.to_owned(),
        reason: "stuck step".to_owned(),
    };
    let aborted = rig.coordinator.abort(&operator(), abort).expect("queued");
    assert!(aborted.queued);
    let abort_entry = aborted.entry.expect("an entry");
    let halt = proto::HaltRequest {
        service: WEB.to_owned(),
        reason: "bad build".to_owned(),
    };
    let halted = rig.coordinator.halt(&operator(), halt).expect("halted");
    let halt_entry = halted.entry.expect("an entry");

    assert!(
        rig.coordinator.ops().marks(WEB).halt.is_some(),
        "web is halted"
    );
    let ring: Vec<(u64, String)> = rig
        .coordinator
        .ops()
        .state()
        .audit
        .into_iter()
        .map(|entry| (entry.seq, entry.action))
        .collect();
    assert!(
        ring.contains(&(abort_entry.seq, "abort".to_owned())),
        "{ring:?}"
    );
    assert!(
        ring.contains(&(halt_entry.seq, "halt".to_owned())),
        "{ring:?}"
    );
    assert_eq!(files(&rig)[2], audit_before, "the audit file took no line");
    assert!(rig.coordinator.ops().halt_unwritten(), "both are pending");
    let stands = "a change stands in memory and is not yet written";
    assert_eq!(log.text().matches(stands).count(), 2, "{}", log.text());

    rig.at(2_100);
    let (_, order) = ask_typed(&rig, "d1", DAEMON, d1_beat());
    assert_eq!(
        order,
        Some(Order::Abort {
            service: WEB.to_owned(),
            order_id: format!("abort-{}", abort_entry.seq),
        })
    );
}

/// Proves: with the audit file taking no lines, `Audit.List` still answers what the file holds
/// (the hold made before the disk filled), filters and all.
///
/// Catches: a read-only verb refused because appends fail.
#[tokio::test]
async fn audit_list_unaffected_by_append_failure() {
    let (rig, _down) = full_disk("audit-list-full").await;
    let listed = rig
        .coordinator
        .audit_list(&operator(), proto::ListAuditRequest::default())
        .expect("answered");
    let actions: Vec<&str> = listed.entries.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["hold"]);
    assert_eq!(listed.next_seq, listed.entries[0].seq);
    let by_service = proto::ListAuditRequest {
        service: Some(DB.to_owned()),
        ..proto::ListAuditRequest::default()
    };
    let filtered = rig
        .coordinator
        .audit_list(&operator(), by_service)
        .expect("answered");
    assert_eq!(filtered.entries, listed.entries);
}

/// Proves: in the ops store, a change written before memory is refused
/// [`OpsError::AuditUnavailable`] with the state, the ring, the numbering and the ops file as
/// they were (the ops file is never written); a change that raises protection, written after
/// memory, stands with its entry in the ring; and a store with no audit file never refuses.
///
/// Catches: a store that writes the ops file before it appends the audit line, or one that
/// refuses a halt or a bad mark for its audit.
#[test]
fn the_store_appends_the_audit_line_before_it_writes_the_ops_file() {
    let dir = scratch("audit-store-order");
    let file = dir.path().join("ops.json");
    let down = Arc::new(AtomicBool::new(false));
    let store = OpsStore::open(&file)
        .expect("opened")
        .with_append(switchable(&file, &down));
    let first = store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("alice", "include"),
            |_| {},
        )
        .expect("a change");
    let written = std::fs::read(&file).expect("the ops file");
    down.store(true, Ordering::SeqCst);

    let refused = store.change(
        Write::BeforeMemory,
        AuditEntry::new("alice", "exclude"),
        |s| {
            s.excluded.clear();
        },
    );
    let refused = refused.expect_err("refused");
    assert!(
        matches!(refused, OpsError::AuditUnavailable(_)),
        "{refused:?}"
    );
    assert!(
        refused
            .to_string()
            .starts_with("the audit file could not be appended to")
    );
    assert_eq!(store.state().audit, std::slice::from_ref(&first));
    assert_eq!(std::fs::read(&file).expect("the ops file"), written);

    let raised = store
        .change(
            Write::AfterMemory,
            AuditEntry::new("coordinator", "mark_bad"),
            |_| {},
        )
        .expect("a raise stands");
    assert_eq!(raised.seq, first.seq + 1);
    assert_eq!(store.state().audit, [first, raised]);

    let volatile = OpsStore::volatile();
    let entry = volatile
        .change(
            Write::BeforeMemory,
            AuditEntry::new("alice", "include"),
            |_| {},
        )
        .expect("a store with no audit file never refuses");
    assert_eq!(
        volatile.audit_after(0).expect("the ring"),
        std::slice::from_ref(&entry)
    );
    assert_eq!(volatile.audit_after(entry.seq).expect("the ring"), []);
    assert_eq!(volatile.audit_pruned_before().expect("none"), None);
}

/// Proves: a force refused for its audit whose desired state cannot be put back (the desired
/// file can no longer be replaced) is still `audit_unavailable`, with no force recorded and the
/// desired version in memory as it was, and the failed put-back is logged at error.
///
/// Catches: a put-back failure that goes unsaid, or one that turns the refusal into a success.
#[tokio::test]
async fn a_force_whose_desired_state_cannot_be_put_back_is_refused_and_logged() {
    let mut rig = rig("audit-force-put-back", "");
    ask(&rig, "d1", DAEMON, d1_beat());
    set(&rig, WEB, "1.0", 1).await;
    set(&rig, WEB, "2.0", 2).await;
    let blocker = rig.dir.path().join("desired.json.new");
    let append: Appender = Box::new(move |_| {
        std::fs::create_dir_all(blocker.join("in-the-way"))?;
        Err(io::Error::from(io::ErrorKind::StorageFull))
    });
    rig.coordinator =
        start(&rig.dir, &rig.clock, "").with_ops(ops_of(&rig.dir).with_append(append));
    let log = Log::default();
    let _guard = log.install();

    let forced = rig.coordinator.force(
        &operator(),
        force_request(WEB, "1.0", fleet_target(WEB), false),
    );
    assert_eq!(forced, Err(proto::ReleasesRefusal::AuditUnavailable));
    assert_eq!(rig.coordinator.ops().marks(WEB).force, None);
    assert_eq!(
        rig.coordinator.desired.get(WEB).map(|d| d.version),
        Some(version("2.0"))
    );
    let text = log.text();
    let error = "a change failed after its desired state was written, and the desired state could not be put back";
    assert_eq!(text.matches(error).count(), 1, "{text}");
}
