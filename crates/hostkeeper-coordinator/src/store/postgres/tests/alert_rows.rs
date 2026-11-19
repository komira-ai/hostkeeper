//! The alerts' rows (migration `0023`; `docs/design/coordinator-multi-instance.md`, section 9, job
//! `alerts`): of any number of coordinators that observe the same alerts, the one whose insert
//! made an alert's row logs it, and a row that no observation finds is forgotten.
//!
//! The alerts of these tests are halts of two services at a version of the test's own, so that
//! the log lines of the test (the log is the process's, shared by the tests that run side by
//! side) are told from every other test's by `version=`. Nothing sleeps; the observations that
//! race wait for one another at a barrier.

use std::sync::{Arc, Barrier, PoisonError};

use super::coordinator::Farm;
use super::{AT, LOG, TestDatabase, logging, options};
use crate::alerts::{Book, Observed};
use crate::marks_support::{DB, WEB, operator, set};
use crate::ops::OpsStore;
use crate::service::Coordinator;
use crate::store::Postgres;
use crate::store::postgres::alert_rows::{Key, PgAlerts};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

/// The number of lines of the process's log that are an alert of `version`: each one names it as
/// `version=<version> `.
fn logged_alerts(version: &str) -> usize {
    let log =
        String::from_utf8_lossy(&LOG.lock().unwrap_or_else(PoisonError::into_inner)).into_owned();
    let marker = format!("version={version} ");
    log.lines()
        .filter(|line| line.contains(r#"alert="halt""#) && line.contains(&marker))
        .count()
}

/// A coordinator over a new store of the database of `farm`, reading what the database holds now,
/// whose ops store says `unix`, and its alerts noted in the rows.
fn second_over(farm: &Farm, unix: u64) -> (Postgres, Coordinator) {
    let (store, loaded) = farm
        .database
        .open("second")
        .expect("the second store opens");
    let marker = farm.rig.dir.path().join("second-halt.marker");
    let ops = loaded.ops.clone();
    let backend: Arc<dyn crate::store::Backend> = Arc::new(store.clone());
    let mut coordinator = super::coordinator::start_on_postgres(&farm.rig, &store, loaded, &marker)
        .with_ops(
            OpsStore::over(backend, ops)
                .with_marker(marker)
                .with_unix(Arc::new(move || unix)),
        );
    coordinator.alerts = Book::default().over(PgAlerts::new(store.clone()));
    (store, coordinator)
}

/// Two coordinators on one database, each over a store of its own and with its alerts noted in
/// the database's rows, that both hold the halts of `web` and `db` at `version`.
struct Pair {
    farm: Farm,
    second: Coordinator,
    second_store: Postgres,
    version: String,
}

impl Pair {
    /// The second coordinator reads its state from the database after the halts, and its clock of
    /// the ops store says `second_unix` (the first one's says [`AT`]).
    async fn start(test: &str, version: &str, second_unix: u64) -> Pair {
        logging();
        let mut farm = Farm::plain(test);
        for service in [WEB, DB] {
            set(&farm.rig, service, version, 1).await;
            farm.rig
                .coordinator
                .halt(
                    &operator(),
                    proto::HaltRequest {
                        service: service.to_owned(),
                        reason: "a bad disk".to_owned(),
                    },
                )
                .expect("halted");
        }
        let (second_store, second) = second_over(&farm, second_unix);
        farm.rig.coordinator.alerts = Book::default().over(PgAlerts::new(farm.store.clone()));
        Pair {
            farm,
            second,
            second_store,
            version: version.to_owned(),
        }
    }

    fn first(&self) -> &Coordinator {
        &self.farm.rig.coordinator
    }

    fn logged(&self) -> usize {
        logged_alerts(&self.version)
    }

    fn rows(&self) -> Vec<Vec<Option<String>>> {
        self.farm.database.rows(&format!(
            "SELECT kind, service, first_unix FROM alerts WHERE version = '{}' ORDER BY service",
            self.version
        ))
    }

    fn stop(self) {
        self.second_store.stop();
        self.farm.store.stop();
    }
}

/// `coordinator`'s observation, made on a thread of its own: the log of the process is read from
/// there, and not from a test's thread, where the rig of the coordinator keeps a log of its own.
fn observe(coordinator: &Coordinator) -> Vec<Observed> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| coordinator.observe_alerts().expect("observed"))
            .join()
            .expect("joined")
    })
}

/// The halts of `observed`, by service and first observed second.
fn halts(observed: &[Observed]) -> Vec<(String, u64)> {
    observed
        .iter()
        .filter(|one| one.alert.subject.kind == crate::alerts::Kind::Halt)
        .map(|one| (one.alert.subject.service.clone(), one.first_unix))
        .collect()
}

/// Proves: two coordinators over one database that observe the same two halts at the same moment
/// (a barrier, forty rounds, the rows emptied between the rounds) log each alert exactly once
/// between them (two lines a round), and both answer the same first-observed second for each.
///
/// Catches: an alert logged by every coordinator that observes it, a log lost when the two
/// inserts race, and a first-observed second that differs between the coordinators.
#[tokio::test]
async fn alert_is_logged_once_when_two_coordinators_observe_at_once() {
    let pair = Pair::start("alerts-once-at-once", "9.4271.1", AT + 500).await;
    assert_eq!(pair.logged(), 0);
    let barrier = Barrier::new(2);
    for round in 1..=40_usize {
        pair.farm
            .database
            .run("DELETE FROM alerts")
            .expect("the rows are emptied");
        let (one, other) = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                observe(pair.first())
            });
            let second = scope.spawn(|| {
                barrier.wait();
                observe(&pair.second)
            });
            (
                first.join().expect("joined"),
                second.join().expect("joined"),
            )
        });
        assert_eq!(pair.logged(), 2 * round, "round {round}");
        let (one, other) = (halts(&one), halts(&other));
        assert_eq!(one.len(), 2);
        assert_eq!(one, other, "round {round}");
    }
    pair.stop();
}

/// Proves: a coordinator that observes the two halts after another one has, and the other way
/// round, logs none of them again, and answers the first-observed second the rows hold (the first
/// observer's, whichever clock the later one reads); the rows are one per alert with that second.
///
/// Catches: a coordinator that logs what the database already holds, and a first-observed second
/// that the later observation overwrites.
#[tokio::test]
async fn alert_is_logged_once_when_two_coordinators_observe_in_turn() {
    let pair = Pair::start("alerts-once-in-turn", "9.4271.2", AT + 500).await;
    let first = observe(pair.first());
    assert_eq!(pair.logged(), 2);
    assert_eq!(
        halts(&first),
        [("db".to_owned(), AT), ("web".to_owned(), AT)]
    );
    let second = observe(&pair.second);
    assert_eq!(pair.logged(), 2, "the second finds the rows made");
    assert_eq!(halts(&second), halts(&first));
    assert_eq!(
        pair.rows(),
        [
            vec![
                Some("halt".to_owned()),
                Some("db".to_owned()),
                Some(AT.to_string())
            ],
            vec![
                Some("halt".to_owned()),
                Some("web".to_owned()),
                Some(AT.to_string())
            ],
        ]
    );
    assert_eq!(halts(&observe(pair.first())), halts(&first));
    assert_eq!(pair.logged(), 2);

    // The other way round: the second's clock is the one the rows hold.
    pair.farm
        .database
        .run("DELETE FROM alerts")
        .expect("the rows are emptied");
    let second = observe(&pair.second);
    assert_eq!(
        halts(&second),
        [("db".to_owned(), AT + 500), ("web".to_owned(), AT + 500)]
    );
    assert_eq!(pair.logged(), 4);
    assert_eq!(halts(&observe(pair.first())), halts(&second));
    assert_eq!(pair.logged(), 4);
    pair.stop();
}

/// Proves: an alert whose condition ends is deleted from the rows by the next observation (the
/// other one's row stays), and when the condition returns it is logged again and first observed
/// anew, by whichever coordinator observes it.
///
/// Catches: a cleared alert whose row stays, so that it is never logged again when it returns,
/// and the clear of one alert that deletes the rows of the others.
#[tokio::test]
async fn a_cleared_alert_is_forgotten_and_logged_again_when_it_returns() {
    let mut pair = Pair::start("alerts-cleared", "9.4271.3", AT + 500).await;
    observe(pair.first());
    assert_eq!(pair.logged(), 2);

    let body = format!(r#"{{"action":"clear_halt","service":"{WEB}","reason":"disk replaced"}}"#);
    pair.first()
        .operate(Some(crate::testutil::OPERATOR), body.as_bytes())
        .expect("the halt is cleared");
    let observed = observe(pair.first());
    assert_eq!(halts(&observed), [("db".to_owned(), AT)]);
    assert_eq!(pair.logged(), 2, "a clear logs nothing");
    assert_eq!(
        pair.rows(),
        [vec![
            Some("halt".to_owned()),
            Some("db".to_owned()),
            Some(AT.to_string())
        ]],
        "the row of the alert that cleared is deleted, the other's stays"
    );

    // The halt returns, raised on a restarted second coordinator (which reads the clear from the
    // database), whose clock is later.
    pair.second_store.stop();
    let (store, second) = second_over(&pair.farm, AT + 500);
    pair.second = second;
    pair.second_store = store;
    pair.second
        .halt(
            &operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "a bad disk again".to_owned(),
            },
        )
        .expect("halted");
    let again = observe(&pair.second);
    assert_eq!(
        halts(&again),
        [("db".to_owned(), AT), ("web".to_owned(), AT + 500)]
    );
    assert_eq!(pair.logged(), 3, "the returned alert is logged once more");
    pair.stop();
}

/// Proves: a coordinator whose rows cannot be written (its sessions are gone) still logs each
/// alert it finds, once, from its memory, answers them with its own clock, and does not log them
/// again at the next observation; nothing it found is in the rows.
///
/// Catches: an alert never logged while the database is away, an alert logged at every
/// observation because the memory is not kept, and a row written by a store that was lost.
#[tokio::test]
async fn an_observation_over_a_lost_store_falls_back_to_memory() {
    let pair = Pair::start("alerts-lost", "9.4271.4", AT + 500).await;
    pair.farm.database.end_sessions();
    let first = observe(pair.first());
    assert_eq!(
        halts(&first),
        [("db".to_owned(), AT), ("web".to_owned(), AT)]
    );
    assert_eq!(pair.logged(), 2);
    let again = observe(pair.first());
    assert_eq!(halts(&again), halts(&first));
    assert_eq!(pair.logged(), 2, "not logged again");
    assert_eq!(pair.rows(), Vec::<Vec<Option<String>>>::new());
    pair.stop();
}

/// The key of an alert of kind `kind` about `service` and `host`, the rest empty.
fn key(kind: &str, service: &str, host: &str) -> Key {
    [
        kind.to_owned(),
        service.to_owned(),
        host.to_owned(),
        String::new(),
        String::new(),
        String::new(),
    ]
}

/// The rows of `alerts` of `db`, each as its six texts and its first second.
fn held(db: &TestDatabase) -> Vec<Vec<String>> {
    db.rows(
        "SELECT kind, service, host, version, request, other_host, first_unix FROM alerts \
         ORDER BY kind, service, host, version, request, other_host",
    )
    .into_iter()
    .map(|row| row.into_iter().map(Option::unwrap_or_default).collect())
    .collect()
}

/// Proves: noting a found set answers each alert's first second and whether this note made its
/// row: new ones with the second of the note and `true`, held ones with the held second and
/// `false`, in the order of the found set; an alert that differs in one text only (the other
/// host) is another row; one subject twice in a set is one row with one second; the rows of the
/// alerts not found are deleted in the same note; an empty set deletes every row; a second past
/// what the column holds is kept at its largest.
///
/// Catches: a held alert reported as new (logged again), a new one reported as held (never
/// logged), a first second that a later note overwrites, a duplicate that makes two rows or
/// fails the note, and rows of cleared alerts that stay.
#[test]
fn note_returns_first_unix_of_held_rows_and_marks_only_new_ones() {
    let db = TestDatabase::new();
    let (store, _) = db
        .open_options(&options(), "alerts")
        .expect("the store opens");
    let alerts = PgAlerts::new(store.clone());
    let (a, b, c) = (
        key("halt", "web", "n1"),
        key("silent_host", "", "n2"),
        key("halt", "db", ""),
    );

    assert_eq!(alerts.note(vec![], 50), Ok(vec![]));
    assert_eq!(
        alerts.note(vec![a.clone(), b.clone()], 100),
        Ok(vec![(100, true), (100, true)])
    );
    assert_eq!(
        alerts.note(vec![c.clone(), a.clone(), b.clone()], 200),
        Ok(vec![(200, true), (100, false), (100, false)])
    );
    assert_eq!(held(&db).len(), 3);

    // Not found: deleted; found again later: new, with the later second.
    assert_eq!(alerts.note(vec![b.clone()], 300), Ok(vec![(100, false)]));
    assert_eq!(
        held(&db),
        [["silent_host", "", "n2", "", "", "", "100"]
            .map(str::to_owned)
            .to_vec()]
    );
    assert_eq!(
        alerts.note(vec![a.clone(), b.clone()], 400),
        Ok(vec![(400, true), (100, false)])
    );

    // The same subject twice is one row, with one second.
    let mut other_host = a.clone();
    other_host[5] = "n9".to_owned();
    let twice = alerts
        .note(vec![c.clone(), c.clone(), other_host.clone()], 500)
        .expect("noted");
    assert_eq!(
        twice.iter().map(|noted| noted.0).collect::<Vec<_>>(),
        [500, 500, 500]
    );
    assert!(twice[0].1 && twice[2].1, "{twice:?}");
    assert_eq!(held(&db).len(), 2, "c once and the host-qualified a");
    assert_eq!(
        alerts.note(vec![c.clone(), c.clone(), other_host], 600),
        Ok(vec![(500, false), (500, false), (500, false)])
    );

    // A second that does not fit the column is its largest.
    assert_eq!(
        alerts.note(vec![a.clone()], u64::MAX),
        Ok(vec![(i64::MAX as u64, true)])
    );

    // An empty set clears every row.
    assert_eq!(alerts.note(vec![], 700), Ok(vec![]));
    assert_eq!(held(&db), Vec::<Vec<String>>::new());
    store.stop();
}

/// What `coordinator` answers `Alerts.List` with, as halts by service and first observed second,
/// asked on a thread of its own: the blocking client does not run on a test's runtime thread.
fn listed(coordinator: &Coordinator) -> Vec<(String, u64)> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                coordinator
                    .list_alerts(&operator(), proto::ListAlertsRequest {})
                    .expect("listed")
                    .alerts
                    .into_iter()
                    .filter(|alert| alert.kind == crate::alerts::Kind::Halt.proto() as i32)
                    .map(|alert| (alert.service, alert.first_observed_unix))
                    .collect()
            })
            .join()
            .expect("joined")
    })
}

/// Proves: over rows, `Alerts.List` writes and logs nothing: an alert the job has not noted is
/// answered with the second of the list, no row is made and no `alert=` line is logged; once the
/// job's observation has noted them, a list on another coordinator answers the rows' first second
/// and not its own clock's; and a list on a coordinator that derives fewer alerts (one halt
/// cleared) deletes no row.
///
/// Catches: a list that notes the alerts (a row made, or logged, by a read), a list that answers
/// its own clock for an alert the database holds, and a list that forgets the rows of alerts it
/// does not derive.
#[tokio::test]
async fn alerts_list_over_rows_writes_and_logs_nothing() {
    let pair = Pair::start("alerts-list-rows", "9.4271.5", AT + 500).await;
    let second = AT + 500;
    assert_eq!(
        listed(&pair.second),
        [("db".to_owned(), second), ("web".to_owned(), second)],
        "not noted yet: the list's own second"
    );
    assert_eq!(pair.rows(), Vec::<Vec<Option<String>>>::new());
    assert_eq!(pair.logged(), 0);

    observe(pair.first());
    assert_eq!(pair.logged(), 2);
    assert_eq!(
        listed(&pair.second),
        [("db".to_owned(), AT), ("web".to_owned(), AT)],
        "the first observer's second, from the rows"
    );
    assert_eq!(pair.logged(), 2, "a list logs nothing");

    let body = format!(r#"{{"action":"clear_halt","service":"{WEB}","reason":"disk replaced"}}"#);
    pair.first()
        .operate(Some(crate::testutil::OPERATOR), body.as_bytes())
        .expect("the halt is cleared");
    assert_eq!(listed(pair.first()), [("db".to_owned(), AT)]);
    assert_eq!(pair.rows().len(), 2, "a list deletes no row");
    assert_eq!(pair.logged(), 2);
    pair.stop();
}

/// Proves: when the alert rows cannot be read (the reader's sessions are gone), `Alerts.List`
/// still answers every alert, from an observation made by this coordinator with its own clock,
/// and the rows it could not read are still not written.
///
/// Catches: a list that fails, or answers nothing, while the database is away, and a list that
/// answers an alert it derived with no first second.
#[tokio::test]
async fn alerts_list_falls_back_to_an_observation_when_the_rows_cannot_be_read() {
    let pair = Pair::start("alerts-list-lost", "9.4271.6", AT + 500).await;
    pair.farm.database.end_sessions();
    assert_eq!(
        listed(pair.first()),
        [("db".to_owned(), AT), ("web".to_owned(), AT)]
    );
    assert_eq!(pair.rows(), Vec::<Vec<Option<String>>>::new());
    pair.stop();
}
