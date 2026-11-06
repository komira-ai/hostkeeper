//! When an enrolled host was first heard: the coordinator keeps, with the host's enrollment, the
//! time by the enrollment's clock of the first heartbeat it accepted from the host since the
//! host's approval, and `Hosts.Get` and `Hosts.List` answer it as `first_heartbeat_unix`. Later
//! heartbeats and a restart of the coordinator leave it; a new approval of the host clears it
//! until the host is heard again; a heartbeat the coordinator refuses does not set it.

use std::sync::Arc;

use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::enrollment::os_random;
use crate::enrollment::tests::support::{HostKey, NOW, Rig as EnrollmentRig, enroll_request};
use crate::marks_support::{NEW_DAEMON, ask, beat_of, operator, start, who};
use crate::testutil::{Log, Rig, beat};

/// When the operator approves `db-1`, by the enrollment's clock.
const APPROVED: u64 = NOW + 1_000;
/// When `db-1` heartbeats first, then again.
const FIRST: u64 = NOW + 1_090;
const SECOND: u64 = NOW + 1_105;

/// A coordinator of `web` and `db`, past its quiet start, over its own enrollment.
struct Fixture {
    enrollment: EnrollmentRig,
    rig: Rig,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        Fixture::over(EnrollmentRig::new(&format!("{name}-enrollment")), name)
    }

    fn over(enrollment: EnrollmentRig, name: &str) -> Fixture {
        let mut rig = Rig::new(name);
        rig.coordinator =
            start(&rig.dir, &rig.clock, "").with_enrollment(Arc::clone(&enrollment.enrollment));
        rig.at(2_000);
        Fixture { enrollment, rig }
    }

    /// A new key that posted from machine `machine`, approved by the operator as `host` at
    /// `at` by the enrollment's clock, taking the id over when `replace`. Nothing is heard.
    fn approve(&self, machine: &str, host: &str, at: u64, replace: bool) {
        let key = HostKey::new();
        let posted = self
            .enrollment
            .enrollment
            .enroll(enroll_request(&key, machine))
            .expect("posted");
        assert_eq!(posted.state, EnrollState::Pending as i32);
        self.enrollment.clock.set(at);
        self.rig
            .coordinator
            .enrollment_approve(
                &operator(),
                proto::ApproveRequest {
                    request_id: key.key_hash().to_hex(),
                    key_sha256: key.key_hash().to_hex(),
                    host_id: host.to_owned(),
                    role: "storage".to_owned(),
                    reason: "racked".to_owned(),
                    replace,
                    ..proto::ApproveRequest::default()
                },
            )
            .expect("approved");
    }

    /// A heartbeat of `host` at `at` by the enrollment's clock, which the coordinator accepts.
    fn beat(&self, host: &str, at: u64) {
        self.enrollment.clock.set(at);
        ask(&self.rig, host, "storage", beat_of(NEW_DAEMON, Vec::new()));
    }

    fn get(&self, host: &str) -> proto::Host {
        self.rig
            .coordinator
            .get_host(
                &operator(),
                &proto::GetHostRequest {
                    node_id: host.to_owned(),
                },
            )
            .expect("answered")
            .host
            .expect("a host")
    }

    /// Each listed host's node id, approval time and first heartbeat time.
    fn listed(&self) -> Vec<(String, u64, Option<u64>)> {
        self.rig
            .coordinator
            .list_hosts(&operator(), proto::ListHostsRequest::default())
            .expect("listed")
            .hosts
            .into_iter()
            .map(|h| (h.node_id, h.enrolled_unix, h.first_heartbeat_unix))
            .collect()
    }
}

/// `host`'s approval time and first heartbeat time, as `Hosts.Get` answers them.
fn times(host: &proto::Host) -> (u64, Option<u64>) {
    (host.enrolled_unix, host.first_heartbeat_unix)
}

/// Proves: the first heartbeat the coordinator accepts from an enrolled host is answered, in
/// `Hosts.Get` and `Hosts.List`, at the time the enrollment's clock read when it arrived; a
/// second heartbeat leaves it; another host's heartbeat sets only that host's; a host heard
/// that is not enrolled answers none.
///
/// Catches: the time taken from the latest heartbeat instead of the first, from the approval,
/// from another clock or from another host, and a host that is not enrolled answered with one.
#[test]
fn the_first_accepted_heartbeat_is_kept_and_later_ones_leave_it() {
    let f = Fixture::new("first-beat");
    f.approve("m-1", "db-1", APPROVED, false);
    f.approve("m-2", "db-2", APPROVED + 5, false);
    f.beat("db-1", FIRST);
    assert_eq!(times(&f.get("db-1")), (APPROVED, Some(FIRST)));
    f.beat("db-1", SECOND);
    assert_eq!(
        times(&f.get("db-1")),
        (APPROVED, Some(FIRST)),
        "a later beat"
    );
    f.beat("db-2", SECOND + 1);
    f.beat("web-7", SECOND + 2);
    assert_eq!(
        f.listed(),
        [
            ("db-1".to_owned(), APPROVED, Some(FIRST)),
            ("db-2".to_owned(), APPROVED + 5, Some(SECOND + 1)),
            ("web-7".to_owned(), 0, None),
        ]
    );
}

/// Proves: the first heartbeat time survives a restart of the coordinator and of its
/// enrollment: it is read back from the enrollment store, and the first heartbeat after the
/// restart does not replace it.
///
/// Catches: the time kept in memory only, or set again by the first heartbeat a restarted
/// coordinator hears.
#[test]
fn a_restart_keeps_the_first_heartbeat_time() {
    let f = Fixture::new("first-beat-restart");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", FIRST);
    let restarted = f.enrollment.restart();
    let mut rig = Rig::new("first-beat-restart-again");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_enrollment(restarted);
    rig.at(2_000);
    let again = Fixture {
        enrollment: f.enrollment,
        rig,
    };
    again.beat("db-1", SECOND);
    assert_eq!(times(&again.get("db-1")), (APPROVED, Some(FIRST)));
}

/// Proves: a new key approved for the host id (`replace`) clears the first heartbeat time, so
/// the host answers none until its next accepted heartbeat, which then sets it.
///
/// Catches: a re-enrolled host answering the first heartbeat of its approval before, which
/// would put the first heartbeat before the approval it is answered with.
#[test]
fn a_new_approval_starts_the_first_heartbeat_over() {
    let f = Fixture::new("first-beat-replaced");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", FIRST);
    let again = SECOND + 500;
    f.approve("m-9", "db-1", again, true);
    assert_eq!(times(&f.get("db-1")), (again, None));
    f.beat("db-1", again + 30);
    assert_eq!(times(&f.get("db-1")), (again, Some(again + 30)));
}

/// Proves: a heartbeat the coordinator refuses (it lists no service, and no legacy service is
/// set) does not set the first heartbeat time; the next one it accepts does.
///
/// Catches: the time noted before the heartbeat is judged, so a refused heartbeat counts as
/// the host's first.
#[test]
fn a_refused_heartbeat_is_not_the_first() {
    let f = Fixture::new("first-beat-refused");
    f.approve("m-1", "db-1", APPROVED, false);
    f.enrollment.clock.set(FIRST);
    f.rig
        .coordinator
        .heartbeat(who("db-1", "storage"), beat("1.0"))
        .expect_err("a heartbeat with no service is refused");
    f.beat("db-1", SECOND);
    assert_eq!(times(&f.get("db-1")), (APPROVED, Some(SECOND)));
}

/// Proves: when the store cannot record the first heartbeat, the heartbeat is still answered,
/// the failure is logged, and the next accepted heartbeat records it.
///
/// Catches: a store failure that refuses the heartbeat, goes unlogged, or marks the host as
/// noted so the time is never recorded.
#[test]
fn a_first_heartbeat_the_store_cannot_write_is_recorded_by_the_next() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = EnrollmentRig::over("first-beat-fault-enrollment", fs.clone(), os_random());
    let f = Fixture::over(enrollment, "first-beat-fault");
    f.approve("m-1", "db-1", APPROVED, false);
    let log = Log::default();
    let guard = log.install();
    // Noting takes the store's lock (1), reads the host file (2), then writes it anew (3).
    fs.fail_at(fs.count() + 3);
    f.beat("db-1", FIRST);
    drop(guard);
    assert!(
        log.text().contains("a first heartbeat was not recorded"),
        "{}",
        log.text()
    );
    assert_eq!(times(&f.get("db-1")), (APPROVED, None));
    f.beat("db-1", SECOND);
    assert_eq!(times(&f.get("db-1")), (APPROVED, Some(SECOND)));
}
