//! The fleet's rows over a server (`fleet_rows`, migration `0024`): the ordered beat upsert, the
//! bake counted in SQL against [`Bake::after`], the slot a report frees, the quiet time a gap in the
//! beats sets, two sessions writing one host's beats, and a coordinator that keeps each heartbeat
//! it answers.
//!
//! Every time is the database's. A test that needs a row of a given age sets it and runs the beat
//! in one transaction, so both read one `now()`; nothing sleeps and no time is measured. A session
//! that must wait for another's row lock is seen waiting in `pg_stat_activity` before the other
//! commits.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use hostkeeper_wire::identity::Identity;
use hostkeeper_wire::wire::Heartbeat;

use super::coordinator::{DAEMON, Farm, daemon};
use super::migrate_ddl::seen_waiting;
use super::{TestDatabase, logged, logging, pooled, with_session};
use crate::bake::Bake;
use crate::marks_support::ask;
use crate::store::beat::{HostBeat, ServiceSaid};
use crate::store::postgres::TIMEOUTS;
use crate::store::postgres::fleet_rows::{Timing, beat};
use crate::store::postgres::migrate;
use crate::store::postgres::session::Session;
use crate::store::postgres::writer::{WriteFail, try_lock};
use crate::store::{Backend, Health};
use crate::testutil::{beat as heartbeat, id, role, unhealthy, updating, version};

/// The database's clock in a statement, in unix seconds.
const NOW: &str = "extract(epoch FROM now())::bigint";

/// The times of the beats: down after 45 seconds, a reorder window of 30 and a quiet time of 30.
const TIMING: Timing = Timing {
    down_after: 45,
    reorder: 30,
    quiet: 30,
};

/// The instance the tests' beats are heard by.
const HEARD_BY: &str = "instance-a";

/// A database with the schema, and a session on it beside any store.
fn rig() -> (TestDatabase, Session) {
    let db = TestDatabase::new();
    drop(db.open("fleet rows").expect("the store opens and migrates"));
    let session = session(&db);
    (db, session)
}

fn session(db: &TestDatabase) -> Session {
    Session::connect(
        &db.dsn().config(TIMEOUTS),
        Arc::new(AtomicU64::new(0)),
        false,
    )
    .expect("a session")
}

fn said(service: &str, version: &str, counts: bool, runs: bool) -> ServiceSaid {
    ServiceSaid {
        service: service.to_owned(),
        version: version.to_owned(),
        counts,
        runs,
    }
}

/// A healthy beat of `service` at `version`.
fn healthy(service: &str, version: &str) -> ServiceSaid {
    said(service, version, true, true)
}

/// A beat of `host` with its clock at `sent_unix`, naming `services`.
fn host(host: &str, sent_unix: i64, services: Vec<ServiceSaid>) -> HostBeat {
    HostBeat {
        host_id: host.to_owned(),
        role: "web".to_owned(),
        beat: format!("beat {sent_unix}").into_bytes(),
        sent_unix,
        services,
    }
}

/// Runs `before`, then the beat statement for `b`, then `read`, in one transaction of `session`:
/// whether the beat was applied, and the row `read` answers, if any.
fn beat_in(
    session: &mut Session,
    before: &str,
    b: &HostBeat,
    read: &str,
) -> (bool, Option<postgres::Row>) {
    session.batch("BEGIN").expect("begin");
    if !before.is_empty() {
        session.batch(before).expect("the rows are set");
    }
    let applied = beat(session, b, HEARD_BY, &TIMING).expect("the beat statement runs");
    let row = if read.is_empty() {
        None
    } else {
        session.query_opt(read, &[]).expect("the rows are read")
    };
    session.batch("COMMIT").expect("commit");
    (applied, row)
}

/// Runs the beat statement for `b` on its own: whether it was applied.
fn beat_now(session: &mut Session, b: &HostBeat) -> bool {
    beat_in(session, "", b, "").0
}

/// The row of `host` in `fleet_hosts`: role, beat, sent_unix, instance.
fn stored(db: &TestDatabase, host: &str) -> Vec<Vec<Option<String>>> {
    db.rows(&format!(
        "SELECT role, convert_from(beat, 'UTF8'), sent_unix, instance FROM fleet_hosts \
         WHERE host_id = '{host}'"
    ))
}

fn text(values: &[&str]) -> Vec<Option<String>> {
    values.iter().map(|v| Some((*v).to_owned())).collect()
}

/// The bakes of `host`: service, target and beats, by service.
fn bakes(db: &TestDatabase, host: &str) -> Vec<Vec<Option<String>>> {
    db.rows(&format!(
        "SELECT service, target, beats FROM fleet_bakes WHERE host_id = '{host}' ORDER BY service"
    ))
}

/// The hosts that hold an order, in order.
fn ordered(db: &TestDatabase) -> Vec<String> {
    db.rows("SELECT host_id FROM fleet_orders ORDER BY host_id")
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect()
}

/// Proves: a beat is the host's row (its role, the beat as given, the host's clock, the instance
/// that heard it, and the time it was heard by the database's clock, between a read of the clock
/// before and one after); a later beat replaces every column, and the host still has one row.
///
/// Catches: an upsert that adds a second row for a host, keeps a column of the beat before, or
/// stamps the coordinator's clock instead of the database's.
#[test]
fn a_beat_is_the_hosts_row_and_a_later_one_replaces_it() {
    let (db, mut session) = rig();
    let before: i64 = session
        .query_one(&format!("SELECT {NOW}"), &[])
        .expect("the clock")
        .get(0);
    assert!(beat_now(&mut session, &host("h1", 100, Vec::new())));
    let after: i64 = session
        .query_one(&format!("SELECT {NOW}"), &[])
        .expect("the clock")
        .get(0);
    assert_eq!(
        stored(&db, "h1"),
        vec![text(&["web", "beat 100", "100", HEARD_BY])]
    );
    let seen: i64 = session
        .query_one("SELECT seen_unix FROM fleet_hosts", &[])
        .expect("the row")
        .get(0);
    assert!(
        (before..=after).contains(&seen),
        "{before} <= {seen} <= {after}"
    );

    let mut later = host("h1", 115, Vec::new());
    later.role = "storage".to_owned();
    assert!(beat(&mut session, &later, "instance-b", &TIMING).expect("the beat"));
    assert_eq!(
        stored(&db, "h1"),
        vec![text(&["storage", "beat 115", "115", "instance-b"])]
    );
}

/// Proves: a beat whose own clock is older than the stored one is dropped whole and answers so:
/// the row, the bake and the order stand as the newer beat left them; a beat with the same clock
/// is applied (a beat sent again), and frees the order it reports done; an agent that sends no
/// clock (0) is taken in the order its beats arrive.
///
/// Catches: beats of one host that reach the database out of order through two coordinators, the
/// older written last, taking the host's row, its bake and its order back.
#[test]
fn stale_beat_does_not_overwrite_a_newer_one() {
    let (db, mut session) = rig();
    db.run(&format!(
        "INSERT INTO fleet_orders (host_id, service, target, at_unix, try) \
         VALUES ('h1', 'daemon', '1.0', {NOW}, 1)"
    ))
    .expect("an order");
    assert!(beat_now(&mut session, &host("h1", 200, Vec::new())));
    assert!(
        !beat_now(
            &mut session,
            &host("h1", 185, vec![healthy("daemon", "1.0")])
        ),
        "an older beat is dropped"
    );
    assert_eq!(stored(&db, "h1")[0][1].as_deref(), Some("beat 200"));
    assert_eq!(bakes(&db, "h1"), Vec::<Vec<Option<String>>>::new());
    assert_eq!(ordered(&db), ["h1"], "a dropped beat frees no slot");

    let mut again = host("h1", 200, vec![healthy("daemon", "1.0")]);
    again.beat = b"beat 200 again".to_vec();
    assert!(beat_now(&mut session, &again), "the same clock is applied");
    assert_eq!(stored(&db, "h1")[0][1].as_deref(), Some("beat 200 again"));
    assert_eq!(bakes(&db, "h1"), vec![text(&["daemon", "1.0", "1"])]);
    assert_eq!(ordered(&db), Vec::<String>::new());

    assert!(beat_now(&mut session, &host("h2", 0, Vec::new())));
    let mut next = host("h2", 0, Vec::new());
    next.beat = b"the next".to_vec();
    assert!(beat_now(&mut session, &next), "no clock: arrival order");
    assert_eq!(stored(&db, "h2")[0][1].as_deref(), Some("the next"));
}

/// Proves: a beat older by its host's clock than the stored one is dropped while the stored row is
/// up to `--beat-reorder-secs` (30) old by the database's clock, exactly 30 included, and applied
/// once it is older (31): a host whose clock stepped back is believed again.
///
/// Catches: a host whose clock went back that is never heard again, and a window that believes an
/// older beat while the newer one is still fresh.
#[test]
fn a_host_whose_clock_stepped_back_is_believed_after_the_reorder_window() {
    let (db, mut session) = rig();
    assert!(beat_now(&mut session, &host("h1", 500, Vec::new())));
    let aged = |secs: i64| format!("UPDATE fleet_hosts SET seen_unix = {NOW} - {secs}");
    let back = host("h1", 400, Vec::new());
    assert!(!beat_in(&mut session, &aged(30), &back, "").0);
    assert_eq!(stored(&db, "h1")[0][2].as_deref(), Some("500"));
    assert!(beat_in(&mut session, &aged(31), &back, "").0);
    assert_eq!(stored(&db, "h1")[0][2].as_deref(), Some("400"));
}

/// The bake before a beat, in a case of [`bake_in_sql_matches_bake_after`]: its target, its beats
/// and how many seconds before the beat its last beat came.
type Before = Option<(&'static str, u32, u64)>;

/// Proves: the bake the beat statement writes is the one [`Bake::after`] gives for the same bake
/// before and the same heartbeat, in every case: no bake before; one more beat at the same version
/// 15 and 44 seconds after the last; one again after exactly `DOWN_AFTER` (45) of silence and at a
/// new version; no bake (the row deleted) for an unhealthy, updating or unpromoted beat, with or
/// without a bake before; and a long bake counted on. The bake's time is the beat's.
///
/// Catches: a SQL bake that drifts from the Rust one the decisions were written against: one that
/// counts across a version change or a silence, or keeps a bake through a beat that does not count.
#[test]
fn bake_in_sql_matches_bake_after() {
    let (_db, mut session) = rig();
    let mut pending = heartbeat("1.0");
    pending.promotion_pending = true;
    let cases: Vec<(Before, Heartbeat)> = vec![
        (None, heartbeat("1.0")),
        (Some(("1.0", 3, 15)), heartbeat("1.0")),
        (Some(("1.0", 3, 44)), heartbeat("1.0")),
        (Some(("1.0", 3, 45)), heartbeat("1.0")),
        (Some(("0.9", 3, 15)), heartbeat("1.0")),
        (Some(("1.0", 3, 15)), unhealthy(heartbeat("1.0"))),
        (Some(("1.0", 3, 15)), updating(heartbeat("1.0"), "1.1")),
        (Some(("1.0", 3, 15)), pending),
        (None, unhealthy(heartbeat("1.0"))),
        (Some(("1.0", 1000, 15)), heartbeat("1.0")),
    ];
    let now = Duration::from_secs(10_000);
    for (case, (before, beat)) in cases.into_iter().enumerate() {
        let who = Identity {
            host_id: id(&format!("h{case}")),
            role: role("web"),
        };
        let kept = HostBeat::of(&who, &beat, Some("daemon"));
        let set = before.map_or_else(String::new, |(target, beats, age)| {
            format!(
                "INSERT INTO fleet_bakes (host_id, service, target, beats, at_unix) \
                 VALUES ('h{case}', 'daemon', '{target}', {beats}, {NOW} - {age})"
            )
        });
        let (applied, row) = beat_in(
            &mut session,
            &set,
            &kept,
            &format!(
                "SELECT target, beats, at_unix = {NOW} FROM fleet_bakes \
                 WHERE host_id = 'h{case}' AND service = 'daemon'"
            ),
        );
        assert!(applied, "case {case}");
        let in_sql = row.map(|row| {
            let beats: i32 = row.get(1);
            let at_the_beat: bool = row.get(2);
            assert!(at_the_beat, "case {case}: the bake's time is the beat's");
            (
                row.get::<_, String>(0),
                u32::try_from(beats).expect("beats"),
            )
        });
        let reference = Bake::after(
            before
                .map(|(target, beats, age)| Bake {
                    target: version(target),
                    beats,
                    at: now - Duration::from_secs(age),
                })
                .as_ref(),
            &beat,
            now,
        )
        .map(|bake| (bake.target.to_string(), bake.beats));
        assert_eq!(in_sql, reference, "case {case}: {before:?}, {beat:?}");
    }
}

/// Proves: an applied beat frees the order of a service whose ordered version it reports running,
/// healthy or not; it frees no order at another version, none whose version it reports as an
/// absent copy, no uninstall (which ends at its verdict), none of another service, none of a host
/// that did not beat, and nothing when the beat is dropped as older.
///
/// Catches: a slot held after the host reached its target (the role's `max_updating` is then
/// spent until the order ages out), and one freed by a beat that did not reach it.
#[test]
fn order_slot_is_freed_when_the_applied_beat_reports_the_target() {
    let (db, mut session) = rig();
    db.run(&format!(
        "INSERT INTO fleet_orders (host_id, service, target, uninstall_id, at_unix, try) VALUES \
         ('h1', 'web', '2.0', NULL, {NOW}, 1), ('h2', 'web', '2.0', NULL, {NOW}, 1), \
         ('h3', 'web', '2.0', NULL, {NOW}, 1), ('h4', 'web', '2.0', 'u-1', {NOW}, 1), \
         ('h5', 'db', '2.0', NULL, {NOW}, 1), ('h6', 'web', '2.0', NULL, {NOW}, 1), \
         ('h7', 'web', '2.0', NULL, {NOW}, 1), ('h8', 'web', '2.0', NULL, {NOW}, 1)"
    ))
    .expect("the orders");
    assert!(beat_now(&mut session, &host("h7", 900, Vec::new())));
    for b in [
        host("h1", 100, vec![healthy("web", "2.0")]),
        host("h2", 100, vec![healthy("web", "1.0")]),
        host("h3", 100, vec![said("web", "2.0", false, false)]),
        host("h4", 100, vec![healthy("web", "2.0")]),
        host("h5", 100, vec![healthy("web", "2.0")]),
        host("h8", 100, vec![said("web", "2.0", false, true)]),
    ] {
        assert!(beat_now(&mut session, &b));
    }
    assert!(!beat_now(
        &mut session,
        &host("h7", 100, vec![healthy("web", "2.0")])
    ));
    assert_eq!(ordered(&db), ["h2", "h3", "h4", "h5", "h6", "h7"]);
}

/// Proves: each service a beat names has its own bake: a service whose beat counts is counted on,
/// one whose beat does not count loses its bake, and a service the beat does not name keeps its
/// own as it was.
///
/// Catches: a bake kept per host rather than per host and service, so one unhealthy service ends
/// the bakes of the others or one service's beats count for another's.
#[test]
fn each_service_of_a_beat_has_its_own_bake() {
    let (db, mut session) = rig();
    let first = host(
        "h1",
        100,
        vec![
            healthy("web", "1.0"),
            healthy("db", "3.0"),
            healthy("cache", "5.0"),
        ],
    );
    assert!(beat_now(&mut session, &first));
    let second = host(
        "h1",
        115,
        vec![healthy("web", "1.0"), said("db", "3.0", false, true)],
    );
    assert!(beat_now(&mut session, &second));
    assert_eq!(
        bakes(&db, "h1"),
        vec![text(&["cache", "5.0", "1"]), text(&["web", "1.0", "2"])]
    );
}

/// Proves: a beat that finds `fleet_hosts` empty, or its newest row exactly `DOWN_AFTER` (45)
/// seconds old, raises `cluster_quiet.until_unix` to the beat's moment plus the quiet time (30);
/// one that finds a row 44 seconds old leaves it; the time is never lowered, even by a gap whose
/// quiet ends sooner than the one stored.
///
/// Catches: a quiet rule that misses a first start or a gap (a partial fleet would then be judged
/// down host by host), one that quiets a fleet that is beating, and one that shortens a quiet time
/// another beat set.
#[test]
fn a_gap_in_the_beats_sets_the_quiet_time() {
    let (_db, mut session) = rig();
    let quiet_left = format!("SELECT until_unix - {NOW} FROM cluster_quiet");
    let left = |row: Option<postgres::Row>| row.expect("the quiet row").get::<_, i64>(0);
    let (_, row) = beat_in(&mut session, "", &host("h1", 100, Vec::new()), &quiet_left);
    assert_eq!(left(row), 30, "an empty table is a gap");
    for (age, quiet) in [(44, None), (45, Some(30))] {
        let set = format!(
            "UPDATE fleet_hosts SET seen_unix = {NOW} - {age}; \
             UPDATE cluster_quiet SET until_unix = 0"
        );
        let (_, row) = beat_in(
            &mut session,
            &set,
            &host("h1", 100, Vec::new()),
            &quiet_left,
        );
        let stored: i64 = left(row);
        match quiet {
            Some(quiet) => assert_eq!(stored, quiet, "newest {age} s old"),
            None => assert!(stored < 0, "newest {age} s old: no quiet ({stored})"),
        }
    }
    let set = format!(
        "UPDATE fleet_hosts SET seen_unix = {NOW} - 100; \
         UPDATE cluster_quiet SET until_unix = {NOW} + 1000"
    );
    let (_, row) = beat_in(
        &mut session,
        &set,
        &host("h1", 100, Vec::new()),
        &quiet_left,
    );
    assert_eq!(left(row), 1000, "never lowered");
}

/// Proves: two sessions writing beats of one host at once are applied one after the other, the
/// second judged against the first's committed row: a beat older than the one another session is
/// committing waits for it and is then dropped; a newer one waits and is then applied, and its bake
/// counts on from the first's, so no beat of the bake is lost.
///
/// Catches: an upsert whose guard reads the row as its own snapshot had it rather than as it is
/// once the other commits, which would let the older beat win, and a bake whose increments of two
/// coordinators overwrite each other.
#[test]
fn two_sessions_apply_one_hosts_beats_one_after_the_other() {
    let (db, mut first) = rig();
    assert!(beat_now(
        &mut first,
        &host("h1", 100, vec![healthy("web", "1.0")])
    ));
    for (held, waiting, applied, beats) in [(200, 150, false, "2"), (300, 315, true, "4")] {
        first.batch("BEGIN").expect("begin");
        assert!(
            beat(
                &mut first,
                &host("h1", held, vec![healthy("web", "1.0")]),
                HEARD_BY,
                &TIMING
            )
            .expect("the held beat")
        );
        let mut second = session(&db);
        std::thread::scope(|scope| {
            let other = scope.spawn(move || {
                beat(
                    &mut second,
                    &host("h1", waiting, vec![healthy("web", "1.0")]),
                    "instance-b",
                    &TIMING,
                )
                .expect("the waiting beat")
            });
            seen_waiting(&db, "fleet_hosts");
            first.batch("COMMIT").expect("commit");
            assert_eq!(other.join().expect("the other session"), applied);
        });
        let newer = if applied { waiting } else { held };
        assert_eq!(
            stored(&db, "h1")[0][2].as_deref(),
            Some(newer.to_string().as_str())
        );
        assert_eq!(bakes(&db, "h1"), vec![text(&["web", "1.0", beats])]);
    }
}

/// Proves: the store keeps a heartbeat in the fleet's rows on a writing session, under its own
/// instance's id; a beat older than the stored one changes nothing; a beat the server refuses is
/// logged with its host, writes nothing and loses no session; a beat whose session the server
/// ended loses the store, as any write does, and writes nothing; and a writer with no session
/// refuses the statement as lost.
///
/// Catches: a beat kept under another instance's id, a refused beat that loses the store or fails
/// silently, and one whose lost session leaves the store up.
#[test]
fn the_store_keeps_each_beat_and_says_what_it_cannot_write() {
    logging();
    let db = TestDatabase::new();
    let (store, _) = db
        .open_options(&pooled(1), "fleet rows")
        .expect("the store opens");
    let instance = store.instance().id.clone();
    store.keep_beat(host("h1", 200, vec![healthy("web", "1.0")]));
    assert_eq!(
        stored(&db, "h1"),
        vec![text(&["web", "beat 200", "200", &instance])]
    );
    store.keep_beat(host("h1", 100, Vec::new()));
    assert_eq!(stored(&db, "h1")[0][1].as_deref(), Some("beat 200"));

    let mut refused = host("refused-host", 300, Vec::new());
    refused.role = "a\0role".to_owned();
    store.keep_beat(refused);
    assert!(logged("a beat is not kept node_id=refused-host"));
    assert_eq!(
        stored(&db, "refused-host"),
        Vec::<Vec<Option<String>>>::new()
    );
    assert_eq!(
        store.health(),
        Health::Up,
        "a refused beat loses no session"
    );

    db.end_sessions();
    store.keep_beat(host("h1", 400, Vec::new()));
    assert_eq!(store.health(), Health::Lost);
    store.keep_beat(host("h1", 500, Vec::new()));
    assert_eq!(stored(&db, "h1")[0][1].as_deref(), Some("beat 200"));
    let sessionless = store
        .on_writer(|writer| writer.once(|_| Ok(())))
        .expect("the writer's thread runs");
    assert!(
        matches!(sessionless, Err(WriteFail::Lost(_))),
        "{sessionless:?}"
    );
}

/// Proves: a heartbeat a coordinator over the Postgres store answers is kept as the host's row,
/// the heartbeat as the coordinator read it, with a bake for each service it names; the next one
/// counts each bake on.
///
/// Catches: a coordinator that answers heartbeats and leaves the fleet's rows unwritten, so another
/// instance would find no host.
#[test]
fn a_heartbeat_the_coordinator_answers_is_kept_in_the_fleet_rows() {
    let farm = Farm::plain("fleet-rows-heartbeat");
    let beat = daemon("1.0");
    ask(&farm.rig, "d1", DAEMON, beat.clone());
    let row = farm
        .database
        .rows("SELECT role, convert_from(beat, 'UTF8') FROM fleet_hosts WHERE host_id = 'd1'");
    assert_eq!(row.len(), 1);
    assert_eq!(row[0][0].as_deref(), Some(DAEMON));
    let read: Heartbeat =
        serde_json::from_str(row[0][1].as_deref().expect("the beat")).expect("it reads");
    assert_eq!(read, beat);
    ask(&farm.rig, "d1", DAEMON, daemon("1.0"));
    assert_eq!(
        bakes(&farm.database, "d1"),
        vec![text(&["db", "1.0", "2"]), text(&["web", "1.0", "2"])]
    );
}

/// Proves (B8, both ways, for `0024`): the release before's runner (migrations `0001` to `0023`)
/// accepts a database this release migrated and kept beats in, runs nothing, and takes the
/// coordinator's lock exclusively once this release has stopped; this release, started on a
/// database the release before migrated, runs `0024`, and its first beat finds `fleet_hosts` empty,
/// keeps the host's row and quiets the fleet.
///
/// Catches: a `0024` marked `refuse`, which would turn a rollback into a restore, and a cutover
/// after a back-out that judges a fleet it has not heard from.
#[test]
fn the_release_before_runs_on_a_database_with_0024_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    store.keep_beat(host("h1", 100, vec![healthy("web", "1.0")]));
    store.stop();
    let (ran, locked) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..23]).expect("the release before's set");
        let ran = migrate::apply(session, &before).expect("the release before is accepted");
        (ran, try_lock(session).expect("a try"))
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert!(
        locked,
        "the release before has the lock once this release stopped"
    );

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..23]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    let (store, _) = old.open("this release").expect("this release migrates");
    store.keep_beat(host("h1", 100, Vec::new()));
    assert_eq!(stored(&old, "h1").len(), 1);
    let quiet = old.rows(&format!("SELECT until_unix > {NOW} FROM cluster_quiet"));
    assert_eq!(quiet, vec![text(&["t"])], "the first beat is a gap");
}
