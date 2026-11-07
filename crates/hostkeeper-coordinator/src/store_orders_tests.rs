//! The orders a change reaches storage and memory in (`docs/design/coordinator-storage.md`,
//! section 4.3), over the in-memory backend, which refuses the commits a test tells it to: a
//! storage-first change refused leaves memory and storage as they were and takes no audit number;
//! a memory-first change stands, pending, and is committed at the next heartbeat; a pending change
//! is committed before any later change; a force lands whole or not at all; and an entry at its
//! default reads as absent. Every time is explicit; nothing sleeps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::Heartbeat;

use crate::assignment::ServiceAssignments;
use crate::marks_support::{
    DB, NEW_DAEMON, WEB, ask, beat_of, fleet_target, force_request, operator, rig_on, runs, set,
    start, update,
};
use crate::ops::{HostOps, OpsError, OpsState, ServiceOps};
use crate::rollouts::Rollout;
use crate::store::{Fake, store_failure};
use crate::testutil::{Rig, version};

/// The role of every host of these tests.
const DAEMON: &str = "daemon";

/// A daemon of this release running `web` and `db` at `at`.
fn daemon(at: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, at), runs(DB, at)])
}

fn exclude(node: &str) -> proto::ExcludeRequest {
    proto::ExcludeRequest {
        service: WEB.to_owned(),
        node_id: node.to_owned(),
        reason: "bad disk".to_owned(),
        abandon: false,
    }
}

fn halt() -> proto::HaltRequest {
    proto::HaltRequest {
        service: WEB.to_owned(),
        reason: "look first".to_owned(),
    }
}

fn pause() -> proto::PauseRequest {
    proto::PauseRequest {
        service: WEB.to_owned(),
        reason: "look first".to_owned(),
    }
}

/// A rig on `fake` with `web` and `db` desired at 2.0 (1.0 registered before) and `d1` heard at
/// 1.0.
async fn farm(test: &str, fake: &Fake) -> Rig {
    let rig = rig_on(test, fake, "");
    ask(&rig, "d1", DAEMON, daemon("1.0"));
    for service in [WEB, DB] {
        set(&rig, service, "1.0", 1).await;
        set(&rig, service, "2.0", 2).await;
    }
    rig
}

/// What a commit the fake took held: whether `web` is halted, how many exclusions, and the audit
/// numbers of its lines.
type Taken = (bool, usize, Vec<u64>);

/// The audit numbers the fake holds.
fn committed_seqs(fake: &Fake) -> Vec<u64> {
    fake.audit().iter().map(|line| line.seq).collect()
}

/// Proves: a storage-first change whose commit fails is refused `store_failed` and changes
/// nothing: the exclusion is neither in memory nor in storage, the audit holds no line of it, and
/// CI's set of a new desired version is refused with the old one standing in both.
///
/// Catches: a change taken into memory before storage holds it, or a refused change that leaves
/// part of itself behind.
#[tokio::test]
async fn a_failed_commit_leaves_memory_and_storage_as_they_were() {
    let fake = Fake::new();
    let rig = farm("orders-failed-commit", &fake).await;
    let (memory, stored, lines) = (rig.coordinator.ops().state(), fake.ops(), fake.audit());
    fake.fail();

    assert_eq!(
        rig.coordinator.exclude(&operator(), exclude("d1")),
        Err(proto::HostsRefusal::StoreFailed)
    );
    let body = format!(r#"{{"service":"{WEB}","version":"3.0"}}"#);
    let set = rig
        .coordinator
        .set_desired(Some(&crate::testutil::ci_token()), body.as_bytes())
        .await;
    assert!(set.is_err(), "{set:?}");

    assert_eq!(rig.coordinator.ops().state(), memory);
    assert_eq!(fake.ops(), stored);
    assert_eq!(fake.audit(), lines);
    assert!(!rig.coordinator.ops().halt_unwritten());
    let wanted = |held: Option<crate::desired::Desired>| held.map(|d| d.version);
    assert_eq!(
        wanted(rig.coordinator.desired.get(WEB)),
        Some(version("2.0"))
    );
    assert_eq!(
        wanted(fake.desired().desired.get(WEB).cloned()),
        Some(version("2.0"))
    );
}

/// Proves: a halt whose commit fails stands: it is answered (`store_failed`, as a raise whose
/// write failed always was), held in memory and pending, and no host is ordered while it stands;
/// the next heartbeat commits it, with its audit line, and nothing is pending after.
///
/// Catches: a raise dropped when storage fails, a halt that orders nothing only once written, or
/// a pending halt left uncommitted until an unrelated change.
#[tokio::test]
async fn a_raised_halt_stands_when_its_commit_fails_and_lands_at_the_next_heartbeat() {
    let fake = Fake::new();
    let rig = farm("orders-halt-pending", &fake).await;
    fake.fail();
    assert_eq!(
        rig.coordinator.halt(&operator(), halt()).map(drop),
        Err(proto::ReleasesRefusal::StoreFailed)
    );
    let seq = rig
        .coordinator
        .ops()
        .state()
        .audit
        .last()
        .expect("the halt")
        .seq;
    assert!(rig.coordinator.ops().marks(WEB).halt.is_some(), "it stands");
    assert!(rig.coordinator.ops().halt_unwritten(), "it is pending");
    assert_eq!(
        fake.ops().services.get(WEB).and_then(|m| m.halt.clone()),
        None
    );
    rig.at(2_100);
    assert_ne!(
        ask(&rig, "d1", DAEMON, daemon("1.0")),
        update(WEB, "2.0"),
        "no update of a halted service"
    );

    fake.heal();
    rig.at(2_115);
    ask(&rig, "d1", DAEMON, daemon("1.0"));
    assert!(!rig.coordinator.ops().halt_unwritten());
    assert!(
        fake.ops()
            .services
            .get(WEB)
            .and_then(|m| m.halt.clone())
            .is_some(),
        "the heartbeat committed it"
    );
    let line = fake
        .audit()
        .into_iter()
        .find(|line| line.seq == seq)
        .expect("its line is committed");
    assert_eq!(line.action, "halt");
}

/// Proves: a change pending is committed before the next change, in a commit of its own, and a
/// storage-first change is refused while the pending one cannot be committed: storage takes the
/// halt alone first, then the exclusion, each with its own audit line, in their order.
///
/// Catches: a later change committed ahead of an earlier one, or one committed while an earlier
/// change still is not.
#[tokio::test]
async fn a_pending_change_is_committed_before_the_next_change() {
    let fake = Fake::new();
    let rig = farm("orders-pending-first", &fake).await;
    let failing = Arc::new(AtomicBool::new(true));
    let taken: Arc<Mutex<Vec<Taken>>> = Arc::default();
    let (gate, record) = (Arc::clone(&failing), Arc::clone(&taken));
    fake.refuse(move |change| {
        let (_, after) = change.ops?;
        // Only the halt's own commit fails: a store that went on past it would commit the
        // exclusion.
        if gate.load(Ordering::SeqCst) && after.excluded.is_empty() {
            return Some(store_failure());
        }
        let halted = after.services.get(WEB).is_some_and(|m| m.halt.is_some());
        let seqs = change.audit.iter().map(|line| line.seq).collect();
        let mut taken = record.lock().expect("the record");
        taken.push((halted, after.excluded.len(), seqs));
        None
    });
    let _ = rig.coordinator.halt(&operator(), halt());
    let halt_seq = rig
        .coordinator
        .ops()
        .state()
        .audit
        .last()
        .expect("the halt")
        .seq;
    assert_eq!(
        rig.coordinator.exclude(&operator(), exclude("d1")),
        Err(proto::HostsRefusal::StoreFailed),
        "refused while the halt cannot be committed"
    );
    assert!(rig.coordinator.ops().state().excluded.is_empty());

    failing.store(false, Ordering::SeqCst);
    let excluded = rig
        .coordinator
        .exclude(&operator(), exclude("d1"))
        .expect("excluded")
        .entry
        .expect("an entry");
    assert_eq!(excluded.seq, halt_seq + 1);
    assert_eq!(
        *taken.lock().expect("the record"),
        [(true, 0, vec![halt_seq]), (true, 1, vec![halt_seq + 1])]
    );
    assert!(!rig.coordinator.ops().halt_unwritten());
}

/// Proves: an operator's pause whose commit fails stands and is committed at the next
/// heartbeat, with no other change made: the retry of a memory-first change is every one's, not
/// only a halt's.
///
/// Catches: a pause that waits for an unrelated change to reach storage, so a restart before one
/// forgets it.
#[tokio::test]
async fn a_pause_whose_commit_failed_is_retried_at_the_next_heartbeat() {
    let fake = Fake::new();
    let rig = farm("orders-pause-retried", &fake).await;
    fake.fail();
    assert_eq!(
        rig.coordinator
            .rollout_pause(&operator(), pause())
            .map(drop),
        Err(proto::RolloutsRefusal::StoreFailed)
    );
    let paused = |state: &OpsState| state.rollouts.get(WEB).is_some_and(|r| r.paused.is_some());
    assert!(paused(&rig.coordinator.ops().state()), "it stands");
    assert!(!paused(&fake.ops()));

    fake.heal();
    rig.at(2_100);
    ask(&rig, "d1", DAEMON, daemon("1.0"));
    assert!(paused(&fake.ops()), "the heartbeat committed it");
    assert!(!rig.coordinator.ops().halt_unwritten());
    let restarted = start_on_fake(&fake, &rig);
    assert!(
        paused(&restarted.coordinator.ops().state()),
        "a restart keeps it"
    );
}

/// A coordinator started again on what `fake` holds, on the clock of `rig`.
fn start_on_fake(fake: &Fake, rig: &Rig) -> Rig {
    let mut again = Rig::new("orders-restarted");
    again.clock = Arc::clone(&rig.clock);
    again.coordinator = crate::marks_support::start_on(fake, &again.clock, "");
    again
}

/// Proves: a storage-first change refused takes no audit number: the next change is numbered one
/// past the last committed, and the audit storage holds every number once, without a gap.
///
/// Catches: a refused change that leaves a gap or a number used twice.
#[tokio::test]
async fn a_refused_change_takes_no_audit_number() {
    let fake = Fake::new();
    let rig = farm("orders-no-number", &fake).await;
    let last = rig
        .coordinator
        .ops()
        .state()
        .audit
        .last()
        .map_or(0, |e| e.seq);
    fake.fail();
    assert_eq!(
        rig.coordinator.exclude(&operator(), exclude("d1")),
        Err(proto::HostsRefusal::StoreFailed)
    );
    fake.heal();
    let entry = rig
        .coordinator
        .exclude(&operator(), exclude("d1"))
        .expect("excluded")
        .entry
        .expect("an entry");
    assert_eq!(entry.seq, last + 1);
    let seqs = committed_seqs(&fake);
    assert_eq!(seqs, (1..=last + 1).collect::<Vec<_>>());
}

/// Proves: a force is one commit of its desired state and its entries. Refused, it changes
/// nothing: the desired version, the force and the halt generation are as they were, in memory
/// and in storage. Taken, it is one commit holding both. Over the files, whose desired state is
/// written first, an ops file that cannot be written puts the desired file back as it was.
///
/// Catches: a force whose desired state lands without its force, or whose entries land without
/// its desired state.
#[tokio::test]
async fn a_force_lands_whole_or_not_at_all() {
    let fake = Fake::new();
    let rig = farm("orders-force-whole", &fake).await;
    let generation = rig.coordinator.ops().marks(WEB).halt_generation;
    let request = || force_request(WEB, "1.0", fleet_target(WEB), false);
    fake.fail();
    assert_eq!(
        rig.coordinator.force(&operator(), request()).map(drop),
        Err(proto::ReleasesRefusal::StoreFailed)
    );
    let version_of = |held: &crate::desired::Held| held.desired.get(WEB).map(|d| d.version.clone());
    assert_eq!(
        rig.coordinator.desired.get(WEB).map(|d| d.version),
        Some(version("2.0"))
    );
    assert_eq!(version_of(&fake.desired()), Some(version("2.0")));
    let marks = rig.coordinator.ops().marks(WEB);
    assert_eq!((marks.force, marks.halt_generation), (None, generation));
    assert_eq!(
        fake.ops().services.get(WEB).and_then(|m| m.force.clone()),
        None
    );

    fake.heal();
    let before = fake.commits();
    rig.coordinator
        .force(&operator(), request())
        .expect("forced");
    assert_eq!(fake.commits(), before + 1, "one commit");
    assert_eq!(version_of(&fake.desired()), Some(version("1.0")));
    assert!(
        fake.ops()
            .services
            .get(WEB)
            .and_then(|m| m.force.clone())
            .is_some()
    );

    let mut files = crate::marks_support::rig("orders-force-files", "");
    ask(&files, "d1", DAEMON, daemon("1.0"));
    set(&files, WEB, "1.0", 1).await;
    set(&files, WEB, "2.0", 2).await;
    let desired_file = files.dir.path().join("desired.json");
    let written = std::fs::read(&desired_file).expect("the desired file");
    files.coordinator = start(&files.dir, &files.clock, "").with_ops(
        crate::testutil::ops_of(&files.dir).with_write(Box::new(|_| {
            Err(OpsError::Io(crate::desired::StoreError::Io {
                path: "ops".into(),
                source: std::io::Error::other("the disk is full"),
            }))
        })),
    );
    assert_eq!(
        files.coordinator.force(&operator(), request()).map(drop),
        Err(proto::ReleasesRefusal::StoreFailed)
    );
    assert_eq!(
        std::fs::read(&desired_file).expect("the desired file"),
        written
    );
    assert_eq!(
        files.coordinator.desired.get(WEB).map(|d| d.version),
        Some(version("2.0"))
    );
}

/// Proves: the code reads a per-service entry at its default (`ServiceOps`, `ServiceAssignments`,
/// `Rollout`, and a host's labels) as the entry absent: a coordinator whose state holds them
/// answers every heartbeat, and shows the same status, as one whose state does not, through a
/// rollout and a refused order alike.
///
/// Catches: a reader that treats a stored default entry as something set, which a backend that
/// does not store such entries would change.
#[tokio::test]
async fn an_entry_at_its_default_reads_as_absent() {
    let mut with_defaults = OpsState::default();
    with_defaults
        .services
        .insert(WEB.to_owned(), ServiceOps::default());
    with_defaults
        .assignments
        .insert(WEB.to_owned(), ServiceAssignments::default());
    with_defaults
        .rollouts
        .insert(WEB.to_owned(), Rollout::default());
    with_defaults
        .hosts
        .insert("d1".to_owned(), HostOps::default());
    let plain = Fake::new();
    let seeded = Fake::holding(with_defaults);
    let a = farm("orders-defaults-plain", &plain).await;
    let b = farm("orders-defaults-seeded", &seeded).await;
    for (at, host, beat) in [
        (2_100, "d1", daemon("1.0")),
        (2_115, "d2", daemon("1.0")),
        (2_130, "d1", daemon("2.0")),
        (2_145, "d2", daemon("1.0")),
    ] {
        a.at(at);
        b.at(at);
        assert_eq!(
            ask(&a, host, DAEMON, beat.clone()),
            ask(&b, host, DAEMON, beat),
            "{host} at {at}"
        );
        let shown =
            |rig: &Rig| serde_json::to_value(crate::rollback_tests::status(rig)).expect("json");
        assert_eq!(shown(&a), shown(&b), "the status at {at}");
    }
    assert_eq!(
        a.coordinator.ops().marks(WEB),
        b.coordinator.ops().marks(WEB)
    );
}
