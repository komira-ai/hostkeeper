//! A coordinator over the Postgres store, end to end in process (`docs/design/coordinator-storage.md`,
//! sections 4.3 to 5 and 8): the desired state, the ops store and the verdicts over one store, a
//! session that is lost and made again, the epoch another coordinator raised meanwhile, a commit
//! whose outcome was not known, the alerts, and the enrollment records read fresh.
//!
//! Every wait is a deterministic event: a revival is posted to the writer's thread, and a job
//! queued after it runs after it; a server session that must not be taken is one a test client
//! holds. Nothing sleeps and no time is measured.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use hostkeeper_enrollment::enroll::records::{Record, Records, pb};
use hostkeeper_enrollment::enroll::{
    Claims, Decision, EnrollmentStore, NewRequest, StoreError as EnrollmentError,
};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::{HostId, Role};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceReply};

use super::{AT, TestDatabase, admin};
use crate::desired::DesiredStore;
use crate::enrollment::tests::support::{Files, FixedClock, HostKey, NOW};
use crate::enrollment::{Admission, Enrollment, StoreAt, os_random, setup};
use crate::marks_support::{
    DB, NEW_DAEMON, WEB, ask, ask_typed, beat_of, force_request, operator, runs, services_with,
    set, update,
};
use crate::ops::OpsStore;
use crate::orders::Order;
use crate::store::postgres::dsn::Timeouts;
use crate::store::postgres::writer::{Fault, LOCK_KEY};
use crate::store::{Backend, Health, Postgres, Shared};
use crate::testutil::Rig;
use crate::verdict_file::VerdictFile;

/// The role of every host of these tests.
pub(super) const DAEMON: &str = "daemon";

/// The message of the alert that says the session is lost.
const LOST_MESSAGE: &str = "the session with the database is lost: nothing is ordered, a change that lowers protection is refused, and one that raises it stands in memory until the session is back";

/// A client of the database that holds what a test tells it to, on a thread of its own, until it
/// is released: the lock of the coordinator that a revival must not find free, a table another
/// session has locked.
pub(super) struct Holding {
    release: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl Holding {
    /// Runs `hold` in a session of the database and waits until it has run (so, for a lock,
    /// until the lock is taken); `free` is run when the holding is released.
    pub(super) fn start(database: &TestDatabase, hold: &str, free: &str) -> Holding {
        let (name, hold, free) = (database.name().to_owned(), hold.to_owned(), free.to_owned());
        let (release, released) = mpsc::channel::<()>();
        let (ready_tx, ready) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let mut client = admin(Some(&name));
            client.batch_execute(&hold).expect("the hold is taken");
            ready_tx.send(()).expect("the test waits");
            released.recv().expect("the test releases");
            client.batch_execute(&free).expect("the hold is freed");
        });
        ready.recv().expect("the hold is taken");
        Holding { release, thread }
    }

    /// Holds the advisory lock exclusively: the session a revival finds taken.
    fn the_lock(database: &TestDatabase) -> Holding {
        Holding::start(
            database,
            &format!("SELECT pg_advisory_lock({LOCK_KEY})"),
            &format!("SELECT pg_advisory_unlock({LOCK_KEY})"),
        )
    }

    /// Frees what is held, and returns once it is.
    pub(super) fn release(self) {
        self.release.send(()).expect("the holder waits");
        self.thread.join().expect("the holder ends");
    }
}

/// A coordinator over one Postgres store, and what its tests reach beside it.
pub(super) struct Farm {
    pub(super) rig: Rig,
    pub(super) store: Postgres,
    pub(super) enrollment: Option<Arc<Enrollment>>,
    pub(super) database: TestDatabase,
}

/// The waits of the store's sessions that a test that does not tune them opens it with.
pub(super) fn waits() -> Timeouts {
    crate::store::postgres::TIMEOUTS
}

/// A coordinator running `web` and `db` over the stores of one Postgres backend, as `serve` has
/// them, and the marker `marker`: what [`crate::marks_support::start_on`] makes over
/// a fake.
pub(super) fn start_on_postgres(
    rig: &Rig,
    store: &Postgres,
    loaded: crate::store::Loaded,
    marker: &std::path::Path,
) -> crate::service::Coordinator {
    let list = format!(
        "{}services = [\"web\", \"db\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    let backend: Shared = Arc::new(store.clone());
    // The repository the tests' sets name, in the rows as an operator's add leaves it: a desired
    // state read again from the rows (a heartbeat's decision reads it afresh) still holds it.
    let mut seeded = loaded.desired.clone();
    if seeded.repositories == crate::repositories::Registry::default() {
        let kept = crate::repositories::Kept {
            repository: crate::repositories_support::komira_added(1),
            generation: 0,
        };
        seeded
            .repositories
            .entries
            .insert(kept.repository.name.clone(), kept);
        let change = crate::store::Commit {
            desired: Some((&loaded.desired, &seeded)),
            ..crate::store::Commit::default()
        };
        backend.commit(&change).expect("the repository is added");
    }
    Rig::start_over(
        DesiredStore::over(Arc::clone(&backend), seeded),
        &rig.clock,
        &list,
    )
    .with_services(services_with(""))
    .with_legacy_service(None)
    .with_ops(
        OpsStore::over(Arc::clone(&backend), loaded.ops)
            .with_marker(marker.to_owned())
            .with_unix(Arc::new(|| AT)),
    )
    .with_verdicts(VerdictFile::over(Arc::clone(&backend), loaded.verdicts))
    .with_accounts(crate::accounts::AccountStore::over(
        Arc::clone(&backend),
        loaded.accounts,
    ))
    .with_layout(crate::layout::LayoutStore::over(backend, loaded.layout))
}

impl Farm {
    /// A coordinator on a database of its own, its clock at 2000 (past the quiet start), over the
    /// store opened with `timeouts` and a pool of one writing session (beside the session of
    /// heartbeats' decisions; the pool has tests of its own); with `enrolled`, the enrollment
    /// store's records on the same store.
    fn start(test: &str, timeouts: Timeouts, enrolled: bool) -> Farm {
        Farm::start_over(test, timeouts, |store| {
            enrolled.then(|| {
                Arc::new(crate::store::postgres::PgRecords::over(store)) as Arc<dyn Records>
            })
        })
    }

    /// [`Farm::start`] with the enrollment store's records `records` makes over the store, when
    /// it makes any.
    pub(super) fn start_over(
        test: &str,
        timeouts: Timeouts,
        records: impl FnOnce(&Postgres) -> Option<Arc<dyn Records>>,
    ) -> Farm {
        let database = TestDatabase::new();
        let (store, loaded) = database
            .open_tuned(timeouts, &super::pooled(1), "coordinator")
            .expect("the store opens");
        let mut rig = Rig::new(test);
        let marker = rig.dir.path().join("halt.marker");
        let mut coordinator = start_on_postgres(&rig, &store, loaded, &marker);
        let mut enrollment = None;
        if let Some(records) = records(&store) {
            let files = Files::in_dir(&rig.dir);
            let started = setup(
                Arc::new(RealFs),
                &crate::enrollment::Setup {
                    store: StoreAt::Records(records),
                    ..files.setup(true)
                },
                FixedClock::at(NOW),
                os_random(),
            )
            .expect("enrollment starts over the database");
            let started = Arc::new(started);
            coordinator = coordinator.with_enrollment(Arc::clone(&started));
            enrollment = Some(started);
        }
        rig.coordinator = coordinator;
        rig.at(2_000);
        Farm {
            rig,
            store,
            enrollment,
            database,
        }
    }

    /// [`Farm::start`] with the standard waits and no enrollment.
    pub(super) fn plain(test: &str) -> Farm {
        Farm::start(test, waits(), false)
    }

    /// `web` and `db` desired at 2.0 (1.0 registered before), and `d1` heard at 1.0 at 2100.
    pub(super) async fn seeded(self) -> Farm {
        ask(&self.rig, "d1", DAEMON, daemon("1.0"));
        // The first beat found the fleet's rows empty, a gap: its quiet time ends, as time
        // passing would end it.
        self.database
            .run("UPDATE cluster_quiet SET until_unix = 0")
            .expect("the quiet time ends");
        for service in [WEB, DB] {
            set(&self.rig, service, "1.0", 1).await;
            set(&self.rig, service, "2.0", 2).await;
        }
        self.rig.at(2_100);
        self
    }

    /// Makes the session lost the way the world does: every session of the store is ended, and
    /// then the first write finds out (the exclusion of `d1` it refuses).
    pub(super) fn lose_session(&self) {
        self.database.end_sessions();
        assert_eq!(
            self.rig.coordinator.exclude(&operator(), exclude("d1")),
            Err(proto::HostsRefusal::StoreFailed)
        );
        assert_eq!(self.store.health(), Health::Lost);
    }

    /// Waits until the lock the ended sessions held is free: the server frees it when the backend
    /// has gone, which no client answer says.
    fn lock_is_free(&self) {
        Holding::the_lock(&self.database).release();
    }

    /// Asks for a revival and waits until the writer's thread has made it. A revival a heartbeat
    /// posted runs first: once the lock is free it may take it, and then the store is up already
    /// (and the lock not free for the barrier).
    fn revive(&self) {
        self.revival_ran();
        if self.store.health() == Health::Up {
            return;
        }
        // The sessions a loss left up (the decisions' one, which the test did not use) are ended
        // too, so the lock is free once every backend is gone; the revival opens each again.
        self.database.end_sessions();
        self.lock_is_free();
        self.store.revive();
        self.store.settle().expect("the writers run");
    }

    /// Waits until a revival the last heartbeat posted has run.
    fn revival_ran(&self) {
        self.store.settle().expect("the writers run");
    }

    fn lease(&self) -> (String, String) {
        let rows = self.database.rows("SELECT epoch, commits FROM lease");
        let row = &rows[0];
        (
            row[0].clone().expect("an epoch"),
            row[1].clone().expect("a count"),
        )
    }

    fn count(&self, table: &str) -> usize {
        self.database.rows(&format!("SELECT 1 FROM {table}")).len()
    }

    fn status_quiet(&self) -> bool {
        self.rig
            .coordinator
            .status(Some(crate::testutil::OPERATOR))
            .expect("the status")
            .quiet
    }
}

/// A daemon of this release running `web` and `db` at `at`.
pub(super) fn daemon(at: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, at), runs(DB, at)])
}

pub(super) fn exclude(node: &str) -> proto::ExcludeRequest {
    proto::ExcludeRequest {
        service: WEB.to_owned(),
        node_id: node.to_owned(),
        reason: "bad disk".to_owned(),
        abandon: false,
    }
}

fn abort(node: &str) -> proto::AbortRequest {
    proto::AbortRequest {
        service: WEB.to_owned(),
        node_id: node.to_owned(),
        reason: "stop it".to_owned(),
    }
}

/// Proves: when the session is lost the coordinator holds its orders and takes writes again on
/// the same epoch once the session is back. A storage-first change is refused `store_failed`;
/// the fleet is marked store-lost and the status quiet; the host is ordered nothing, and neither
/// is an abort the operator queued (a force is refused like any storage-first change). After the
/// revival the epoch of the store and the lease's are those they were, and the next heartbeat
/// orders the update.
///
/// Catches: a lost session that still orders updates, an operator order delivered while the
/// store cannot record what it did, a revival that raises the epoch (so it is read as another
/// coordinator's), or a coordinator that stays quiet after the session is back.
#[tokio::test]
async fn a_lost_session_holds_orders_and_resumes_on_the_same_epoch() {
    let farm = Farm::plain("pg-lost-resumes").seeded().await;
    let rig = &farm.rig;
    assert_eq!(ask(rig, "d1", DAEMON, daemon("1.0")), update(WEB, "2.0"));
    let epoch = farm.store.epoch().expect("an epoch");
    let leased = farm.lease();
    assert_eq!(leased.0, epoch.to_string());

    // Another session holds the lock, so a revival cannot succeed while the test looks.
    farm.database.end_sessions();
    let holding = Holding::the_lock(&farm.database);
    assert_eq!(
        rig.coordinator.exclude(&operator(), exclude("d1")),
        Err(proto::HostsRefusal::StoreFailed)
    );
    assert_eq!(farm.store.health(), Health::Lost);

    rig.coordinator
        .abort(&operator(), abort("d1"))
        .expect("an abort stands in memory");
    assert_eq!(
        rig.coordinator
            .force(
                &operator(),
                force_request(WEB, "1.0", crate::marks_support::fleet_target(WEB), false)
            )
            .map(drop),
        Err(proto::ReleasesRefusal::StoreFailed)
    );
    let (reply, order) = ask_typed(rig, "d1", DAEMON, daemon("1.0"));
    assert_eq!(
        (reply, format!("{order:?}")),
        (ServiceReply::Nothing, "None".to_owned())
    );
    farm.revival_ran();
    assert_eq!(farm.store.health(), Health::Lost, "the lock is held");
    assert!(farm.status_quiet(), "the status says it is quiet");
    assert_eq!(ask(rig, "d1", DAEMON, daemon("1.0")), ServiceReply::Nothing);

    holding.release();
    farm.revive();
    assert_eq!(farm.store.health(), Health::Up);
    assert_eq!(farm.store.epoch(), Some(epoch));
    assert_eq!(farm.lease().0, leased.0, "the lease's epoch is as it was");

    let (_, order) = ask_typed(rig, "d1", DAEMON, daemon("1.0"));
    assert!(
        matches!(order, Some(Order::Abort { .. })),
        "the operator's abort goes first: {order:?}"
    );
    assert!(!farm.status_quiet());
    assert_eq!(ask(rig, "d1", DAEMON, daemon("1.0")), update(WEB, "2.0"));
}

/// Proves: a commit whose session was lost after the server committed it is taken into memory
/// once the session is back: the exclusion is refused `store_failed` and memory does not hold it
/// while the table does; at the next heartbeat after the revival memory holds the exclusion and
/// its audit entry, and the same call made again succeeds and leaves one exclusion.
///
/// Catches: a change that landed and that memory never learns (so the operator's retry doubles
/// it or the coordinator orders a host it excluded), or a landing taken into memory twice.
#[tokio::test]
async fn a_commit_that_landed_before_its_session_was_lost_is_taken_into_memory() {
    let farm = Farm::plain("pg-landed").seeded().await;
    let before = farm.rig.coordinator.ops().state();
    farm.store.fault(Fault::LoseAfterCommit);

    assert_eq!(
        farm.rig.coordinator.exclude(&operator(), exclude("d1")),
        Err(proto::HostsRefusal::StoreFailed)
    );
    assert_eq!(farm.rig.coordinator.ops().state().excluded, before.excluded);
    assert_eq!(farm.count("exclusions"), 1, "the table holds it");
    let audited = farm.count("audit");

    farm.revive();
    ask(&farm.rig, "d1", DAEMON, daemon("1.0"));
    let state = farm.rig.coordinator.ops().state();
    assert_eq!(state.excluded.len(), 1, "memory holds the exclusion");
    assert_eq!(
        state.audit.last().map(|entry| entry.action.as_str()),
        Some("exclude")
    );
    assert_eq!(state.audit.len(), before.audit.len() + 1);
    assert_eq!(farm.count("audit"), audited);

    farm.rig
        .coordinator
        .exclude(&operator(), exclude("d1"))
        .expect("the call made again succeeds");
    assert_eq!(farm.rig.coordinator.ops().state().excluded.len(), 1);
    assert_eq!(farm.count("exclusions"), 1);
}

/// Proves: a change of the users and API tokens that landed before its session was lost is taken
/// into the coordinator's account store at the next heartbeat after the revival: the change is
/// refused, memory lacks the user while the table holds it, and afterwards memory holds the user
/// and the token.
///
/// Catches: an account store that never learns of a user the database holds, so an admin's retry
/// is refused as a name already taken while nobody can sign in as that user until a restart.
#[tokio::test]
async fn an_accounts_change_that_landed_before_its_session_was_lost_is_taken_into_memory() {
    let farm = Farm::plain("pg-accounts-landed").seeded().await;
    let held = crate::store::conformance_tests::rich_accounts();
    farm.store.fault(Fault::LoseAfterCommit);

    let refused = farm.rig.coordinator.accounts().change(|_| Ok(held.clone()));
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        farm.rig.coordinator.accounts().snapshot(),
        crate::accounts::Accounts::default()
    );
    assert_eq!(farm.count("users"), 2, "the table holds them");

    farm.revive();
    ask(&farm.rig, "d1", DAEMON, daemon("1.0"));
    assert_eq!(farm.rig.coordinator.accounts().snapshot(), held);
    assert_eq!(farm.count("api_tokens"), 2);
}

/// Proves: a commit whose session was lost before the server committed it is not taken: the
/// exclusion is refused, memory and the table both lack it after the revival, and the next
/// change takes the number the lost commit took (one past the newest row held): the audit
/// counter, which the lost commit's rolled-back transaction never moved on, numbers the lines
/// with no gap, and no audit number is used twice.
///
/// Catches: a change taken into memory that the database never held, an audit number used twice,
/// and a gap a lost commit leaves in the numbers.
#[tokio::test]
async fn a_commit_that_did_not_land_before_its_session_was_lost_is_not_taken() {
    let farm = Farm::plain("pg-not-landed").seeded().await;
    let before = farm.rig.coordinator.ops().state();
    let newest = before.audit.last().map_or(0, |entry| entry.seq);
    let numbers = |farm: &Farm| -> Vec<String> {
        farm.database
            .rows("SELECT seq FROM audit ORDER BY seq")
            .into_iter()
            .map(|row| row[0].clone().expect("a number"))
            .collect()
    };
    let held_before = numbers(&farm);
    farm.database
        .run(
            "CREATE FUNCTION test_end() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_terminate_backend(pg_backend_pid()); RETURN NULL; END $$; \
             CREATE CONSTRAINT TRIGGER test_end AFTER INSERT ON audit DEFERRABLE INITIALLY \
             DEFERRED FOR EACH ROW EXECUTE FUNCTION test_end()",
        )
        .expect("the trigger is set");

    assert_eq!(
        farm.rig.coordinator.exclude(&operator(), exclude("d1")),
        Err(proto::HostsRefusal::StoreFailed)
    );
    assert_eq!(farm.store.health(), Health::Lost);
    farm.database
        .run("DROP TRIGGER test_end ON audit; DROP FUNCTION test_end()")
        .expect("the trigger is dropped");

    farm.revive();
    ask(&farm.rig, "d1", DAEMON, daemon("1.0"));
    assert_eq!(farm.store.health(), Health::Up);
    assert_eq!(farm.rig.coordinator.ops().state().excluded, before.excluded);
    assert_eq!(farm.count("exclusions"), 0);
    assert_eq!(
        numbers(&farm),
        held_before,
        "the table holds nothing of the lost commit"
    );

    let done = farm
        .rig
        .coordinator
        .exclude(&operator(), exclude("d1"))
        .expect("the next change lands")
        .entry
        .expect("an entry");
    assert_eq!(
        done.seq,
        newest + 1,
        "the counter's number the lost commit took is free again"
    );
    let held = numbers(&farm);
    let mut unique = held.clone();
    unique.dedup();
    assert_eq!(held, unique, "no number is held twice");
    assert_eq!(held.last().cloned(), Some(done.seq.to_string()));
}

/// Proves: a statement whose wait for a lock runs out is refused and keeps the session: with the
/// table of the exclusions locked by another session, an operator's exclusion that writes it is
/// ended by the store's own bounded wait and refused `store_failed`, writing nothing, and the
/// store stays `Up`; once the lock is released the same exclusion is written at once, on the
/// same sessions, with no revival. (The reason the refusal is logged with is in
/// `waits::a_write_whose_lock_wait_runs_out_is_refused_with_what_it_waited_for`.)
///
/// Catches: a stalled database that freezes the coordinator behind a write that never returns, and
/// a lock timeout that drops the session, so every write and heartbeat is refused until a revival.
#[tokio::test]
async fn a_statement_whose_lock_wait_runs_out_is_refused_and_keeps_the_session() {
    let short = Timeouts {
        statement: Duration::from_secs(2),
        lock: Duration::from_secs(1),
        ..waits()
    };
    let farm = Farm::start("pg-stalled", short, false).seeded().await;
    let locked = Holding::start(
        &farm.database,
        "BEGIN; LOCK TABLE hostkeeper.exclusions IN ACCESS EXCLUSIVE MODE",
        "ROLLBACK",
    );

    assert_eq!(
        farm.rig.coordinator.exclude(&operator(), exclude("d1")),
        Err(proto::HostsRefusal::StoreFailed)
    );
    assert_eq!(farm.store.health(), Health::Up);

    locked.release();
    assert_eq!(
        farm.count("exclusions"),
        0,
        "the refused exclusion wrote nothing"
    );
    farm.rig
        .coordinator
        .exclude(&operator(), exclude("d1"))
        .expect("written once the lock is free");
    assert_eq!(farm.count("exclusions"), 1);
    assert_eq!(farm.store.health(), Health::Up);
}

/// Proves: `Alerts.List` holds exactly one `store_unreachable` alert, with its exact message,
/// while the session is lost, and none once the session is back.
///
/// Catches: a lost session no alert says, an alert listed twice, or one that stays after the
/// session is back.
#[tokio::test]
async fn store_unreachable_stands_while_the_session_is_lost() {
    let farm = Farm::plain("pg-alert").seeded().await;
    let unreachable = |farm: &Farm| -> Vec<String> {
        farm.rig
            .coordinator
            .list_alerts(&operator(), proto::ListAlertsRequest::default())
            .expect("the alerts")
            .alerts
            .into_iter()
            .filter(|alert| alert.kind == i32::from(proto::AlertKind::StoreUnreachable))
            .map(|alert| alert.message)
            .collect()
    };
    assert_eq!(unreachable(&farm), Vec::<String>::new());

    farm.lose_session();
    assert_eq!(unreachable(&farm), [LOST_MESSAGE.to_owned()]);

    farm.revive();
    assert_eq!(unreachable(&farm), Vec::<String>::new());
}

/// Proves: while the session is lost, an enrollment store that cannot be read (its table held
/// past the read's lock timeout) does not refuse the alerts' list: `store_unreachable` is listed;
/// with the session up, the same unreadable store refuses the list `store_failed`.
///
/// Catches: the alert that says the database is lost hidden by the very loss it reports.
#[tokio::test]
async fn store_unreachable_is_listed_when_the_enrollment_store_cannot_be_read() {
    let short = Timeouts {
        lock: Duration::from_millis(200),
        statement: Duration::from_secs(2),
        ..waits()
    };
    let farm = Farm::start("pg-alert-unread", short, true).seeded().await;
    let list = |farm: &Farm| {
        farm.rig
            .coordinator
            .list_alerts(&operator(), proto::ListAlertsRequest::default())
    };
    let hold = |farm: &Farm| {
        Holding::start(
            &farm.database,
            "BEGIN; LOCK TABLE \"hostkeeper\".enrollment_requests IN ACCESS EXCLUSIVE MODE",
            "ROLLBACK",
        )
    };
    let table = hold(&farm);
    assert_eq!(
        list(&farm).map(drop),
        Err(proto::AlertsRefusal::StoreFailed),
        "with the session up, an unreadable store refuses the list"
    );
    table.release();
    // Ending the coordinator's sessions ends every other one too, so the table is held again.
    farm.lose_session();
    let table = hold(&farm);
    let kinds: Vec<i32> = list(&farm)
        .expect("the alerts")
        .alerts
        .into_iter()
        .map(|alert| alert.kind)
        .collect();
    assert_eq!(kinds, [i32::from(proto::AlertKind::StoreUnreachable)]);
    table.release();
}

fn host_record(host: &str, role: &str) -> pb::Host {
    pb::Host {
        facts_generation: None,
        host_group: String::new(),
        moving_from: None,
        schema: 1,
        host_id: host.to_owned(),
        key_hash: vec![7; 32],
        machine_id: "m-1".to_owned(),
        role: role.to_owned(),
        services: Vec::new(),
        duplicate: false,
        first_heartbeat_unix: None,
        first_serving_unix: None,
        running: None,
    }
}

/// Proves: the enrollment records read a host as the table holds it at every read: a host row
/// changed by hand (as another coordinator would) is read changed at once, a commit of unknown
/// outcome does not stop a read while the writing session is lost, and the host that commit
/// wrote is read once it landed.
///
/// Catches: a host or deny row served from memory, which a write by another coordinator, or a
/// commit of unknown outcome, would leave behind the table (a denied host still admitted, an
/// approved one still refused).
#[test]
fn the_enrollment_records_read_each_host_as_the_table_holds_it() {
    let database = TestDatabase::new();
    // A pool of one writing session, the one the fault ends, beside the decisions' own.
    let (store, _) = database
        .open_options(&super::pooled(1), "records")
        .expect("the store opens");
    let records = crate::store::postgres::PgRecords::over(&store);
    let key = |host: &str| HostId::parse(host).expect("a host id");
    records
        .commit(&[], &[Record::Host(host_record("web-1", "web"))])
        .expect("committed");
    database
        .run("UPDATE enrollment_hosts SET role = 'by-hand' WHERE host_id = 'web-1'")
        .expect("changed by hand");
    let role = |records: &crate::store::postgres::PgRecords, host: &str| -> Option<String> {
        records
            .host(&key(host))
            .expect("read")
            .map(|host| host.role)
    };
    assert_eq!(
        role(&records, "web-1").as_deref(),
        Some("by-hand"),
        "a change by another writer is read at once"
    );

    store.fault(Fault::LoseAfterCommit);
    let lost = records.commit(&[], &[Record::Host(host_record("web-2", "storage"))]);
    assert!(lost.is_err(), "the commit's outcome is not known");
    assert_eq!(
        role(&records, "web-2").as_deref(),
        Some("storage"),
        "a read does not wait on the lost session, and reads what landed"
    );
    // The lost writer's backend is gone once only the decisions' session holds the lock: a
    // progress event the server shows, not a time.
    let holders = |database: &TestDatabase| {
        database
            .rows(&format!(
                "SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND objid = {LOCK_KEY}::bigint \
                 % 4294967296 AND granted AND database = \
                 (SELECT oid FROM pg_database WHERE datname = current_database())"
            ))
            .len()
    };
    while holders(&database) > 1 {
        std::thread::yield_now();
    }
    store.revive();
    store.settle().expect("the writers run");
    assert_eq!(store.health(), Health::Up);
    assert_eq!(role(&records, "web-1").as_deref(), Some("by-hand"));
}

/// An enrollment store over the records of `store`, with `host` approved.
pub(super) fn approved(store: &Postgres, host: &str) -> (EnrollmentStore, HostId) {
    let records = Arc::new(crate::store::postgres::PgRecords::over(store));
    let enrollment = EnrollmentStore::new(records);
    let id = HostId::parse(host).expect("a host id");
    let key = HostKey::new();
    let posted = enrollment
        .post(
            NewRequest {
                csr_der: key.csr(),
                claims: Claims {
                    machine_id: "m-1".to_owned(),
                    ..Claims::default()
                },
                image: "hostkeeper-image-1".to_owned(),
                renewal_of: None,
                mac_nonce_presented: false,
            },
            NOW,
        )
        .expect("posted");
    enrollment
        .approve(
            &posted.request_id,
            &key.key_hash(),
            &Decision {
                host_id: id.clone(),
                role: Role::parse("web").expect("a role"),
                services: Vec::new(),
                replace: false,
                host_group: None,
            },
        )
        .expect("approved");
    (enrollment, id)
}

/// Proves: the first heartbeat of an approved host is written once and the duplicate mark is
/// written once: a second heartbeat runs no statement and leaves the time of the first, a store
/// that knows nothing of the first reads it and writes nothing, a second machine id marks the
/// host duplicate in the table, and a third report of it writes nothing.
///
/// Catches: a heartbeat that rewrites the host's row each time it comes, a first-heartbeat time
/// replaced by a later one, or a duplicate mark written at every report.
#[test]
fn the_first_heartbeat_and_the_duplicate_mark_are_written_once() {
    let database = TestDatabase::new();
    let (store, _) = database.open("records").expect("the store opens");
    let (enrollment, id) = approved(&store, "web-1");
    let first = || database.rows("SELECT first_heartbeat_unix, duplicate FROM enrollment_hosts");

    enrollment.note_heartbeat(&id, NOW + 60).expect("noted");
    assert_eq!(
        first(),
        [vec![Some((NOW + 60).to_string()), Some("f".to_owned())]]
    );
    let other = EnrollmentStore::new(Arc::new(crate::store::postgres::PgRecords::over(&store)));
    let statements = store.statements();
    enrollment.note_heartbeat(&id, NOW + 90).expect("noted");
    assert_eq!(store.statements(), statements, "the second ran nothing");
    assert_eq!(first()[0][0], Some((NOW + 60).to_string()));

    let commits = database.commits();
    other.note_heartbeat(&id, NOW + 120).expect("noted");
    assert_eq!(database.commits(), commits, "a store that knew nothing");
    assert_eq!(first()[0][0], Some((NOW + 60).to_string()));

    assert!(!enrollment.report_live(&id, "m-1", NOW + 100).expect("live"));
    assert_eq!(database.commits(), commits, "one machine writes nothing");
    assert!(enrollment.report_live(&id, "m-2", NOW + 101).expect("live"));
    assert_eq!(first()[0][1], Some("t".to_owned()));
    assert_eq!(
        first()[0][0],
        Some((NOW + 60).to_string()),
        "the mark keeps the first heartbeat"
    );
    let marked = database.commits();
    assert_eq!(marked, commits + 1, "the mark was written");
    assert!(enrollment.report_live(&id, "m-2", NOW + 102).expect("live"));
    assert!(enrollment.report_live(&id, "m-1", NOW + 103).expect("live"));
    assert_eq!(database.commits(), marked, "written once");
}

/// Proves: a heartbeat in the steady state writes nothing: once a host's first heartbeat is
/// written, a coordinator over the database with enrollment answers a heartbeat of that host
/// with no write; and the host's deny row is read fresh: a deny row written by another
/// coordinator (here by hand) refuses the host's next heartbeat at once, with no restart and no
/// write of this one.
///
/// Catches: a heartbeat that writes the host's row each time it comes, and a deny row served from
/// a copy of what this coordinator wrote, which admits a host another coordinator retired.
#[tokio::test]
async fn a_heartbeat_in_the_steady_state_writes_nothing_and_reads_the_deny_row_fresh() {
    let farm = Farm::start("pg-steady", waits(), true).seeded().await;
    let enrollment = Arc::clone(farm.enrollment.as_ref().expect("enrollment"));
    let key = HostKey::new();
    let posted = enrollment
        .store()
        .post(
            NewRequest {
                csr_der: key.csr(),
                claims: Claims {
                    machine_id: "m-1".to_owned(),
                    ..Claims::default()
                },
                image: "hostkeeper-image-1".to_owned(),
                renewal_of: None,
                mac_nonce_presented: false,
            },
            NOW,
        )
        .expect("posted");
    let id = HostId::parse("d1").expect("a host id");
    enrollment
        .store()
        .approve(
            &posted.request_id,
            &key.key_hash(),
            &Decision {
                host_id: id.clone(),
                role: Role::parse(DAEMON).expect("a role"),
                services: Vec::new(),
                replace: false,
                host_group: None,
            },
        )
        .expect("approved");
    let who = crate::marks_support::who("d1", DAEMON);
    let beat = |farm: &Farm| {
        farm.rig
            .coordinator
            .heartbeat_from(who.clone(), daemon("1.0"), Some("m-1"))
            .expect("answered")
    };
    beat(&farm);
    assert!(
        enrollment
            .store()
            .enrolled_host(&id)
            .expect("read")
            .expect("enrolled")
            .first_heartbeat_unix
            .is_some(),
        "the first heartbeat is written"
    );

    let commits = farm.database.commits();
    beat(&farm);
    assert_eq!(
        farm.database.commits(),
        commits,
        "a heartbeat of a host already heard wrote nothing"
    );
    assert_eq!(farm.rig.coordinator.admission(&who), Admission::Admitted);
    farm.database
        .run("INSERT INTO enrollment_deny (host_id, state) VALUES ('d1', 2)")
        .expect("denied by another coordinator");
    assert_eq!(
        farm.rig.coordinator.admission(&who),
        Admission::Denied,
        "the deny row is read at the next heartbeat"
    );
    let store = enrollment.store();
    let reads: Result<(bool, bool, bool, bool), EnrollmentError> = (|| {
        Ok((
            store.is_denied(&id)?,
            store.is_retiring(&id)?,
            store.is_duplicate(&id)?,
            store.host_role(&id)?.is_some(),
        ))
    })();
    assert_eq!(reads.expect("the reads"), (true, false, false, true));
    assert_eq!(farm.database.commits(), commits, "and wrote nothing");
}
