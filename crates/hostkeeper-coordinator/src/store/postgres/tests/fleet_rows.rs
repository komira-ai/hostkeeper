//! The fleet's rows over a server (`fleet_rows`): the ordered beat upsert, the bake counted in
//! SQL, the slot a report frees, and the quiet time a gap in the beats sets.
//!
//! The clock is a number each test passes in; nothing sleeps.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use super::TestDatabase;
use crate::bake::Bake;
use crate::fleet::DOWN_AFTER;
use crate::store::postgres::fleet_rows::{HostBeat, ServiceBeat, Timing, bake, beat, quiet_until};
use crate::store::postgres::session::Session;
use crate::store::postgres::{Postgres, TIMEOUTS};
use crate::testutil::{beat as heartbeat, unhealthy};

const DOWN: i64 = 45;

fn timing(now: i64) -> Timing {
    Timing {
        now,
        down_after: DOWN,
        reorder: 30,
        quiet: 30,
    }
}

/// A database with the schema, a store holding it open, and a session beside the store.
fn rig() -> (TestDatabase, Postgres, Session) {
    let db = TestDatabase::new();
    let (store, _) = db.open("fleet rows").expect("the store opens");
    let session = Session::connect(
        &db.dsn().config(TIMEOUTS),
        Arc::new(AtomicU64::new(0)),
        false,
    )
    .expect("a session");
    (db, store, session)
}

fn host_beat<'a>(sent_unix: i64, services: &'a [ServiceBeat<'a>]) -> HostBeat<'a> {
    HostBeat {
        host_id: "h1",
        role: "web",
        beat: b"beat",
        sent_unix,
        up: true,
        running: None,
        repo_report: None,
        instance: "a",
        services,
    }
}

const HEALTHY: ServiceBeat<'static> = ServiceBeat {
    service: "daemon",
    target: "1.0",
    counts: true,
};

fn stored(db: &TestDatabase) -> Vec<Option<String>> {
    db.rows(
        "SELECT convert_from(beat, 'UTF8'), seen_unix, sent_unix, up, instance, \
         convert_from(running, 'UTF8') FROM fleet_hosts WHERE host_id = 'h1'",
    )
    .remove(0)
}

/// Proves: a beat is stored as the host's row (its role, the message as received, the time it was
/// heard and the host's own, whether it is up, the instance that heard it); a later beat replaces
/// it; a beat that carries no `running` leaves the stored one, and one that carries it replaces it.
///
/// Catches: a beat that adds a second row, and an upsert that blanks what a lighter beat omits.
#[test]
fn a_beat_is_the_hosts_row_and_a_later_one_replaces_it() {
    let (db, _store, mut session) = rig();
    let mut first = host_beat(100, &[HEALTHY]);
    first.running = Some(b"r1");
    assert!(beat(&mut session, &first, &timing(1000)).expect("stored"));
    assert_eq!(
        stored(&db),
        vec![
            Some("beat".into()),
            Some("1000".into()),
            Some("100".into()),
            Some("t".into()),
            Some("a".into()),
            Some("r1".into())
        ]
    );
    let mut second = host_beat(115, &[HEALTHY]);
    second.beat = b"beat 2";
    second.instance = "b";
    second.up = false;
    assert!(beat(&mut session, &second, &timing(1015)).expect("stored"));
    assert_eq!(
        stored(&db),
        vec![
            Some("beat 2".into()),
            Some("1015".into()),
            Some("115".into()),
            Some("f".into()),
            Some("b".into()),
            Some("r1".into())
        ],
        "the running the beat did not carry stands"
    );
    let mut third = host_beat(130, &[HEALTHY]);
    third.running = Some(b"r2");
    third.repo_report = Some(b"repo");
    beat(&mut session, &third, &timing(1030)).expect("stored");
    assert_eq!(stored(&db)[5], Some("r2".into()));
    assert_eq!(
        db.rows("SELECT 1, convert_from(repo_report, 'UTF8') FROM fleet_hosts"),
        vec![vec![Some("1".into()), Some("repo".into())]]
    );
}

/// Proves: a beat older than the stored one (its own clock lower) is dropped whole: the row, the
/// bake and the slot stay as the newer beat left them, and the call says it was dropped; a beat
/// with the same clock is applied; an agent that sends no clock (0) is taken in arrival order.
///
/// Catches: beats of one host that reach two coordinators out of order, the older written last,
/// taking `running` and the bake backward.
#[test]
fn a_beat_older_than_the_stored_one_is_dropped_whole() {
    let (db, _store, mut session) = rig();
    db.run(
        "INSERT INTO fleet_orders (host_id, service, role, target, at_unix) \
         VALUES ('h1', 'daemon', 'web', '1.0', 990)",
    )
    .expect("an order");
    assert!(beat(&mut session, &host_beat(200, &[]), &timing(1000)).expect("first"));
    let mut late = host_beat(185, &[HEALTHY]);
    late.beat = b"late";
    assert!(
        !beat(&mut session, &late, &timing(1002)).expect("late"),
        "an older beat is dropped"
    );
    assert_eq!(stored(&db)[0], Some("beat".into()));
    assert_eq!(stored(&db)[1], Some("1000".into()));
    assert_eq!(bake(&mut session, "h1", "daemon").expect("bake"), None);
    assert_eq!(
        db.rows("SELECT count(*) FROM fleet_orders"),
        vec![vec![Some("1".into())]],
        "the dropped beat freed no slot"
    );
    assert!(
        beat(&mut session, &host_beat(200, &[]), &timing(1003)).expect("same"),
        "the same clock is the same beat sent again"
    );
    let mut old_agent = host_beat(0, &[]);
    old_agent.host_id = "h2";
    assert!(beat(&mut session, &old_agent, &timing(1004)).expect("no clock"));
    assert!(
        beat(&mut session, &old_agent, &timing(1005)).expect("no clock again"),
        "an agent with no clock is believed in arrival order"
    );
}

/// Proves: a beat whose clock is behind the stored one is believed once the stored row is older
/// than `reorder` (a host whose clock stepped back), and not before.
///
/// Catches: a host with a reset clock that is ignored for good.
#[test]
fn a_host_whose_clock_stepped_back_is_believed_after_the_reorder_window() {
    let (db, _store, mut session) = rig();
    beat(&mut session, &host_beat(5000, &[]), &timing(1000)).expect("first");
    assert!(!beat(&mut session, &host_beat(10, &[]), &timing(1029)).expect("inside"));
    assert!(beat(&mut session, &host_beat(10, &[]), &timing(1031)).expect("outside"));
    assert_eq!(stored(&db)[2], Some("10".into()));
}

/// Proves: the bake counted in SQL is the one `Bake::after` counts, on a run of beats that is
/// healthy, healthy again, at a new version, unhealthy (which ends it), healthy after that, and
/// healthy after a silence of `DOWN_AFTER`.
///
/// Catches: a `CASE` that drifts from the Rust reference (a bake that survives a silence, or
/// a version change).
#[test]
fn bake_in_sql_matches_bake_after() {
    let (_db, _store, mut session) = rig();
    let beats = [
        (heartbeat("1.0"), 1000_u64),
        (heartbeat("1.0"), 1015),
        (heartbeat("1.0"), 1030),
        (heartbeat("2.0"), 1045),
        (heartbeat("2.0"), 1060),
        (unhealthy(heartbeat("2.0")), 1075),
        (heartbeat("2.0"), 1090),
        (heartbeat("2.0"), 1105),
        (heartbeat("2.0"), 1105 + DOWN as u64),
        (heartbeat("2.0"), 1105 + DOWN as u64 + 44),
    ];
    let mut reference: Option<Bake> = None;
    for (n, (hb, at)) in beats.into_iter().enumerate() {
        reference = Bake::after(reference.as_ref(), &hb, Duration::from_secs(at));
        let version = hb.version.to_string();
        let counts = reference.is_some();
        let service = ServiceBeat {
            service: "daemon",
            target: &version,
            counts,
        };
        let clock = i64::try_from(n).expect("small") + 1;
        beat(
            &mut session,
            &host_beat(clock, &[service]),
            &timing(i64::try_from(at).expect("small")),
        )
        .expect("stored");
        let row = bake(&mut session, "h1", "daemon").expect("bake");
        assert_eq!(
            row,
            reference.as_ref().map(|b| (
                b.target.to_string(),
                i64::from(b.beats),
                i64::try_from(b.at.as_secs()).expect("small")
            )),
            "after beat {n}"
        );
    }
    assert_eq!(
        DOWN_AFTER,
        Duration::from_secs(45),
        "the constant the test mirrors"
    );
}

/// Proves: a beat that reports the version an order named frees that order's slot; a beat that
/// reports another version, an order of another host or service, and an uninstall order do not.
///
/// Catches: a slot held until it ages out after the host is done, and a slot freed by a host that
/// is still on the old version.
#[test]
fn a_report_of_the_ordered_version_frees_the_slot() {
    let (db, _store, mut session) = rig();
    db.run(
        "INSERT INTO fleet_orders (host_id, service, role, target, uninstall_id, at_unix) VALUES \
         ('h1', 'daemon', 'web', '2.0', NULL, 990), ('h2', 'daemon', 'web', '2.0', NULL, 990), \
         ('h3', 'web', 'web', '2.0', NULL, 990), ('h4', 'daemon', 'web', '1.0', 'u1', 990)",
    )
    .expect("orders");
    let orders = || db.rows("SELECT host_id FROM fleet_orders ORDER BY host_id");
    beat(&mut session, &host_beat(1, &[HEALTHY]), &timing(1000)).expect("old version");
    assert_eq!(orders().len(), 4, "the old version frees nothing");
    let new = ServiceBeat {
        target: "2.0",
        ..HEALTHY
    };
    beat(&mut session, &host_beat(2, &[new]), &timing(1001)).expect("new version");
    assert_eq!(
        orders(),
        vec![
            vec![Some("h2".into())],
            vec![Some("h3".into())],
            vec![Some("h4".into())]
        ]
    );
    let mut h4 = host_beat(
        3,
        &[ServiceBeat {
            target: "1.0",
            ..HEALTHY
        }],
    );
    h4.host_id = "h4";
    beat(&mut session, &h4, &timing(1002)).expect("the uninstall's version");
    assert_eq!(
        orders().len(),
        3,
        "an uninstall order is not freed by a report of its version"
    );
}

/// Proves: a beat several services long keeps one bake per service and ends the bake of a
/// service whose beat does not count while the others go on.
///
/// Catches: one bake for the host, or a service's bake ended by the host's other service.
#[test]
fn each_service_of_a_beat_has_its_own_bake() {
    let (_db, _store, mut session) = rig();
    let web = ServiceBeat {
        service: "web",
        target: "7",
        counts: true,
    };
    beat(&mut session, &host_beat(1, &[HEALTHY, web]), &timing(1000)).expect("first");
    let sick = ServiceBeat {
        counts: false,
        ..web
    };
    beat(&mut session, &host_beat(2, &[HEALTHY, sick]), &timing(1015)).expect("second");
    assert_eq!(
        bake(&mut session, "h1", "daemon").expect("bake"),
        Some(("1.0".into(), 2, 1015))
    );
    assert_eq!(bake(&mut session, "h1", "web").expect("bake"), None);
}

/// Proves: the first beat on an empty table sets the quiet time to now plus `quiet`; a beat while
/// the table is fresh does not move it; a beat after every row is older than `down_after` sets it
/// again, from that beat's time, and never lowers a later one.
///
/// Catches: a restart of Postgres, or a first start, in which a partial fleet is judged as if it
/// were the whole one.
#[test]
fn a_gap_in_the_beats_sets_the_quiet_time() {
    let (db, _store, mut session) = rig();
    assert_eq!(quiet_until(&mut session).expect("quiet"), 0);
    beat(&mut session, &host_beat(1, &[]), &timing(1000)).expect("first");
    assert_eq!(quiet_until(&mut session).expect("quiet"), 1030);
    beat(&mut session, &host_beat(2, &[]), &timing(1015)).expect("fresh");
    assert_eq!(quiet_until(&mut session).expect("quiet"), 1030, "no gap");
    beat(&mut session, &host_beat(3, &[]), &timing(1015 + DOWN - 1)).expect("just fresh");
    assert_eq!(quiet_until(&mut session).expect("quiet"), 1030);
    beat(&mut session, &host_beat(4, &[]), &timing(1059 + DOWN + 1)).expect("after a gap");
    assert_eq!(
        quiet_until(&mut session).expect("quiet"),
        1059 + DOWN + 1 + 30
    );
    db.run("UPDATE cluster_quiet SET until_unix = 9999")
        .expect("a later quiet");
    beat(&mut session, &host_beat(5, &[]), &timing(5000)).expect("another gap");
    assert_eq!(
        quiet_until(&mut session).expect("quiet"),
        9999,
        "the quiet time never goes back"
    );
}

/// The hosts a decision of `rows` reads under the gate, as (host, time heard, instance).
fn read_under_gate(rows: &crate::store::postgres::FleetRows) -> Vec<(String, i64, String)> {
    use crate::store::postgres::rollout::{Gated, Wait};
    match rows
        .gated(Wait::Beat, |gate| gate.snapshot())
        .expect("the gate is taken")
    {
        Gated::Decided(snapshot) => snapshot
            .hosts
            .into_iter()
            .map(|row| (row.host_id, row.seen_unix, row.instance))
            .collect(),
        Gated::Busy => panic!("nothing else holds the gate"),
    }
}

/// Proves: through a coordinator's session (`FleetRows`), a beat older than the one another
/// coordinator stored is not stored, and the newer row is the one the next decision reads, once:
/// the decision after it does not read it again; a
/// session the server ended fails the call it was ended under, and the next call makes a new one
/// and is stored.
///
/// Catches: a coordinator that keeps using a session the server ended (every later beat fails),
/// or that takes a beat it did not store as the row it holds.
#[test]
fn the_fleet_session_drops_a_stale_beat_and_is_made_again_once_ended() {
    let (db, store, _session) = rig();
    let a = store.fleet_rows("a");
    let b = store.fleet_rows("b");
    assert_eq!(a.beat(&host_beat(100, &[]), &timing(1000)), Ok(true));
    assert_eq!(b.beat(&host_beat(200, &[]), &timing(1001)), Ok(true));
    assert_eq!(
        a.beat(&host_beat(150, &[]), &timing(1002)),
        Ok(false),
        "the beat the other coordinator stored is newer"
    );
    assert_eq!(
        read_under_gate(&a),
        vec![("h1".to_owned(), 1001, "b".to_owned())]
    );
    assert_eq!(read_under_gate(&a), vec![], "taken in once");

    db.end_sessions();
    assert!(
        a.beat(&host_beat(300, &[]), &timing(1003)).is_err(),
        "the call the session ended under fails"
    );
    assert_eq!(a.beat(&host_beat(300, &[]), &timing(1004)), Ok(true));
    assert_eq!(stored(&db)[1], Some("1004".into()));
}
