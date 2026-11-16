//! The decisions of a rollout over a server (`rollout`), with two coordinators deciding at once:
//! the order of a host, the advance of a stage, a halt, and the queue, each against the rows.
//!
//! Every wait is an event: a barrier starts both deciders together, a channel says the gate is
//! held. Nothing sleeps and no time is measured; the clock is a number each test passes in.

use std::sync::atomic::AtomicU64;
use std::sync::mpsc::channel;
use std::sync::{Arc, Barrier};

use super::TestDatabase;
use crate::store::postgres::rollout::{
    Admission, Ask, Gated, Limits, Uninstalling, Wait, admit, decide, gate_timed_out,
    place_uninstall, raise_halt, stage_is_at,
};
use crate::store::postgres::session::Session;
use crate::store::postgres::{Postgres, TIMEOUTS};

const NOW: i64 = 10_000;

const LIMITS: Limits = Limits {
    max_updating: 3,
    min_up: 0,
    live_rollouts: 1,
    order_age: 120,
    down_after: 45,
    queue_idle: 3600,
};

/// A database with its schema and, beside one store that holds it open, a session for each of
/// `n` coordinators.
fn rig(n: usize) -> (TestDatabase, Postgres, Vec<Session>) {
    let db = TestDatabase::new();
    let (store, _) = db.open("rollout").expect("the store opens");
    let sessions = (0..n)
        .map(|_| {
            Session::connect(
                &db.dsn().config(TIMEOUTS),
                Arc::new(AtomicU64::new(0)),
                false,
            )
            .expect("a session")
        })
        .collect();
    (db, store, sessions)
}

fn ask<'a>(host_id: &'a str, service: &'a str) -> Ask<'a> {
    Ask {
        host_id,
        role: "web",
        service,
        target: "2.0",
        now: NOW,
        order_try: 1,
    }
}

fn answer(session: &mut Session, ask: &Ask<'_>, limits: &Limits) -> Admission {
    match decide(session, Wait::Beat, |s| admit(s, ask, limits)).expect("the decision runs") {
        Gated::Decided(admission) => admission,
        Gated::Busy => panic!("nothing else holds the gate"),
    }
}

fn count(db: &TestDatabase, sql: &str) -> String {
    db.rows(sql).remove(0).remove(0).expect("a count")
}

/// Proves: two coordinators asked at once for the hosts of one role, more hosts than
/// `max_updating`, place exactly `max_updating` orders between them and no host twice; a host
/// asked on both gets one order, and the one that finds the gate busy asks again at its next beat.
///
/// Catches: two coordinators that each count the slots before the other's order is written and
/// place `max_updating` each.
#[test]
fn two_coordinators_never_exceed_max_updating() {
    for round in 0..8 {
        let (db, _store, sessions) = rig(2);
        let hosts: Vec<String> = (0..12).map(|n| format!("h{n}")).collect();
        let start = Barrier::new(2);
        let placed: Vec<Vec<String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = sessions
                .into_iter()
                .enumerate()
                .map(|(who, mut session)| {
                    let (hosts, start) = (&hosts, &start);
                    scope.spawn(move || {
                        start.wait();
                        let mut mine = Vec::new();
                        let order: Vec<&String> = if (who + round) % 2 == 0 {
                            hosts.iter().collect()
                        } else {
                            hosts.iter().rev().collect()
                        };
                        for host in order {
                            // A busy gate answers no order: the host's next beat asks again.
                            let admission = loop {
                                let asked = ask(host, "daemon");
                                match decide(&mut session, Wait::Beat, |s| {
                                    admit(s, &asked, &LIMITS)
                                })
                                .expect("runs")
                                {
                                    Gated::Decided(a) => break a,
                                    Gated::Busy => std::thread::yield_now(),
                                }
                            };
                            if admission == Admission::Placed {
                                mine.push(host.clone());
                            }
                        }
                        mine
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("joins"))
                .collect()
        });
        let mut all: Vec<&String> = placed.iter().flatten().collect();
        all.sort();
        assert_eq!(all.len(), 3, "round {round}: placed {placed:?}");
        all.dedup();
        assert_eq!(all.len(), 3, "no host twice");
        assert_eq!(count(&db, "SELECT count(*) FROM fleet_orders"), "3");
    }
}

/// Proves: with the gate held by one decision, a beat is answered at once as busy and reads and
/// writes nothing; an operator's edit waits, and gives up with the database's lock error when its
/// wait runs out; once the holder commits, both get the gate.
///
/// Catches: a beat that queues behind a decision, and an operator's wait with no bound.
#[test]
fn a_beat_never_waits_for_the_gate_and_an_operator_waits_a_bounded_time() {
    let (db, _store, mut sessions) = rig(3);
    let (mut third, mut second, mut first) = (
        sessions.pop().expect("a session"),
        sessions.pop().expect("a session"),
        sessions.pop().expect("a session"),
    );
    let (held_tx, held_rx) = channel();
    let (release_tx, release_rx) = channel::<()>();
    std::thread::scope(|scope| {
        let holder = scope.spawn(move || {
            decide(&mut first, Wait::Beat, |s| {
                held_tx.send(()).expect("told");
                release_rx.recv().expect("released");
                s.execute(
                    "INSERT INTO fleet_orders (host_id, service, role, target, at_unix) \
                     VALUES ('held', 'daemon', 'web', '2.0', 1)",
                    &[],
                )
            })
        });
        held_rx.recv().expect("the gate is held");
        let busy = decide(&mut second, Wait::Beat, |s| {
            admit(s, &ask("h1", "daemon"), &LIMITS)
        })
        .expect("runs");
        assert_eq!(busy, Gated::Busy);
        assert_eq!(count(&db, "SELECT count(*) FROM fleet_orders"), "0");
        let refused = decide(&mut third, Wait::Operator { timeout_ms: 50 }, |_| Ok(()))
            .expect_err("the wait runs out");
        assert!(gate_timed_out(&refused), "{refused}");
        release_tx.send(()).expect("released");
        assert_eq!(
            holder.join().expect("joins").expect("decides"),
            Gated::Decided(1)
        );
    });
    assert_eq!(
        decide(&mut second, Wait::Beat, |s| admit(
            s,
            &ask("h1", "daemon"),
            &LIMITS
        ))
        .expect("runs"),
        Gated::Decided(Admission::Placed),
        "the gate is free once the holder commits, and sees its order"
    );
    assert_eq!(
        decide(&mut third, Wait::Operator { timeout_ms: 1000 }, |_| Ok(
            "edit"
        ))
        .expect("runs"),
        Gated::Decided("edit")
    );
}

/// Proves: a decision whose body fails is rolled back with everything it wrote, and the gate is
/// free for the next; a decision whose session is ended by the server while it holds the gate
/// frees it too, so another coordinator decides at once.
///
/// Catches: a gate held by a coordinator that died or failed in the middle of a decision.
#[test]
fn a_failed_or_lost_decision_holds_nothing() {
    let (db, _store, mut sessions) = rig(2);
    let mut other = sessions.pop().expect("a session");
    let mut session = sessions.pop().expect("a session");
    let failed = decide(&mut session, Wait::Beat, |s| {
        admit(s, &ask("h1", "daemon"), &LIMITS)?;
        s.execute("INSERT INTO no_such_table VALUES (1)", &[])
    });
    assert!(failed.is_err());
    assert_eq!(
        count(&db, "SELECT count(*) FROM fleet_orders"),
        "0",
        "rolled back"
    );

    let pid = session.backend_pid().expect("pid");
    let (held_tx, held_rx) = channel();
    let (done_tx, done_rx) = channel::<()>();
    std::thread::scope(|scope| {
        let doomed = scope.spawn(move || {
            decide(&mut session, Wait::Beat, |_| {
                held_tx.send(()).expect("told");
                done_rx.recv().expect("told to go on");
                Ok(())
            })
        });
        held_rx.recv().expect("the gate is held");
        assert_eq!(
            decide(&mut other, Wait::Beat, |_| Ok(())).expect("runs"),
            Gated::Busy
        );
        db.rows(&format!("SELECT pg_terminate_backend({pid})"));
        done_tx.send(()).expect("go on");
        assert!(
            doomed.join().expect("joins").is_err(),
            "the lost session fails"
        );
    });
    assert_eq!(
        decide(&mut other, Wait::Beat, |_| Ok(())).expect("runs"),
        Gated::Decided(()),
        "the server freed the gate with the session"
    );
}

/// Proves: each way an order is refused or kept: the host's own order is sent again, an order for
/// another service or an uninstall leaves the host busy, a quiet fleet, a halted service, a full
/// role and too few hosts staying up are refused, an order older than `order_age` frees its slot,
/// and only hosts that are live, up and without an order count as staying.
///
/// Catches: an order placed while the fleet is quiet or the service halted, a slot held for ever
/// by an order nobody reported, and `min_up` counted over hosts that are down or already updating.
#[test]
fn an_order_is_refused_or_kept_by_the_rows() {
    let (db, _store, mut sessions) = rig(1);
    let s = &mut sessions[0];
    assert_eq!(answer(s, &ask("h1", "daemon"), &LIMITS), Admission::Placed);
    assert_eq!(
        answer(s, &ask("h1", "daemon"), &LIMITS),
        Admission::Existing
    );
    assert_eq!(answer(s, &ask("h1", "web"), &LIMITS), Admission::HostBusy);
    let other_version = Ask {
        target: "3.0",
        ..ask("h1", "daemon")
    };
    assert_eq!(answer(s, &other_version, &LIMITS), Admission::HostBusy);
    db.run("INSERT INTO fleet_orders (host_id, service, role, target, uninstall_id, at_unix) VALUES ('h9', 'daemon', 'web', '1.0', 'u', 10000)")
        .expect("an uninstall order");
    assert_eq!(
        answer(s, &ask("h9", "daemon"), &LIMITS),
        Admission::HostBusy
    );

    db.run("UPDATE cluster_quiet SET until_unix = 10001")
        .expect("quiet");
    assert_eq!(answer(s, &ask("h2", "daemon"), &LIMITS), Admission::Quiet);
    assert_eq!(
        answer(s, &ask("h1", "daemon"), &LIMITS),
        Admission::Existing,
        "an order given stays the host's in the quiet time"
    );
    db.run("UPDATE cluster_quiet SET until_unix = 10000")
        .expect("not quiet");

    raise_halt_in(&db, "daemon");
    assert_eq!(answer(s, &ask("h2", "daemon"), &LIMITS), Admission::Halted);
    db.run("DELETE FROM halts").expect("cleared");

    // h1 and h9 hold two of three slots.
    assert_eq!(answer(s, &ask("h2", "daemon"), &LIMITS), Admission::Placed);
    assert_eq!(answer(s, &ask("h3", "daemon"), &LIMITS), Admission::Full);

    // Aged orders free their slots.
    let later = Limits {
        order_age: 120,
        ..LIMITS
    };
    let aged = Ask {
        now: NOW + 120,
        ..ask("h3", "daemon")
    };
    assert_eq!(answer(s, &aged, &later), Admission::Placed);
    assert_eq!(count(&db, "SELECT count(*) FROM fleet_orders"), "1");

    // min_up: live, up hosts of the role without an order, not counting the asker.
    db.run(
        "INSERT INTO fleet_hosts (host_id, role, beat, seen_unix, sent_unix, up, instance) VALUES \
         ('u1', 'web', ''::bytea, 10120, 1, true, 'a'), ('u2', 'web', ''::bytea, 10120, 1, true, 'a'), \
         ('d1', 'web', ''::bytea, 10120, 1, false, 'a'), ('old', 'web', ''::bytea, 10000, 1, true, 'a'), \
         ('x1', 'db', ''::bytea, 10120, 1, true, 'a')",
    )
    .expect("hosts");
    let many = Limits {
        max_updating: 9,
        ..LIMITS
    };
    let wants_two = Limits { min_up: 2, ..many };
    let at = |host| Ask {
        now: NOW + 120,
        ..ask(host, "daemon")
    };
    assert_eq!(
        answer(s, &at("u1"), &wants_two),
        Admission::Short,
        "only u2 would stay"
    );
    let wants_one = Limits { min_up: 1, ..many };
    assert_eq!(
        answer(s, &at("u1"), &wants_one),
        Admission::Placed,
        "u2 stays"
    );
    assert_eq!(
        answer(s, &at("u2"), &wants_one),
        Admission::Short,
        "u1 is updating now, and d1, old and x1 do not count"
    );
    assert_eq!(answer(s, &at("u2"), &many), Admission::Placed);
}

/// Puts `service` in the queue as of `NOW`, keeping its place when it has one; its place: what the
/// commit of a desired version does (`queued`).
fn enqueue(session: &mut Session, service: &str) -> Result<i64, postgres::Error> {
    session.execute(
        "INSERT INTO rollout_queue (service, asked_unix) VALUES ($1, $2) \
         ON CONFLICT (service) DO UPDATE SET asked_unix = excluded.asked_unix",
        &[&service, &NOW],
    )?;
    Ok(session
        .query_one(
            "SELECT seq FROM rollout_queue WHERE service = $1",
            &[&service],
        )?
        .get(0))
}

/// Takes `service` out of the queue: its rollout is over. True when it was there.
fn finish(session: &mut Session, service: &str) -> Result<bool, postgres::Error> {
    Ok(super::super::rollout::finish(session, &[service])? == 1)
}

fn raise_halt_in(db: &TestDatabase, service: &str) {
    db.run(&format!(
        "INSERT INTO halts (service, node_id, check_name, target, origin) \
         VALUES ('{service}', 'h1', 'check', '2.0', 'updater')"
    ))
    .expect("a halt");
}

/// Proves: one service rolls at a time (`live_rollouts` 1) in the order it was asked for: with
/// s1 asked before s2 and s3, s2 and s3 wait behind it (s2 and s3 as `Behind` while nothing
/// rolls, `Elsewhere` once s1 does), s1's other hosts go on, and when s1's rollout is over and
/// finished s2 is next, then s3; with two places both of the first two go and the third waits.
///
/// Catches: a queue that lets the service whose host beat first roll out of turn, and two
/// coordinators that each start a different service.
#[test]
fn one_service_rolls_at_a_time_in_the_order_it_was_asked_for() {
    let (db, _store, mut sessions) = rig(1);
    let s = &mut sessions[0];
    let place = |s: &mut Session, service: &str| {
        decide(s, Wait::Beat, |s| enqueue(s, service)).expect("runs")
    };
    let (Gated::Decided(a), Gated::Decided(b), Gated::Decided(c)) =
        (place(s, "s1"), place(s, "s2"), place(s, "s3"))
    else {
        panic!("the gate is free")
    };
    assert!(a < b && b < c, "the queue is in the order asked");
    assert_eq!(
        decide(s, Wait::Beat, |s| enqueue(s, "s1")).expect("runs"),
        Gated::Decided(a),
        "asking again keeps the place"
    );

    assert_eq!(answer(s, &ask("h2", "s2"), &LIMITS), Admission::Behind);
    assert_eq!(answer(s, &ask("h1", "s1"), &LIMITS), Admission::Placed);
    assert_eq!(
        answer(s, &ask("h3", "s1"), &LIMITS),
        Admission::Placed,
        "s1 goes on"
    );
    assert_eq!(answer(s, &ask("h2", "s2"), &LIMITS), Admission::Elsewhere);

    db.run("DELETE FROM fleet_orders")
        .expect("s1's hosts reported");
    assert_eq!(
        decide(s, Wait::Beat, |s| finish(s, "s1")).expect("runs"),
        Gated::Decided(true)
    );
    assert_eq!(
        decide(s, Wait::Beat, |s| finish(s, "s1")).expect("runs"),
        Gated::Decided(false)
    );
    assert_eq!(answer(s, &ask("h4", "s3"), &LIMITS), Admission::Behind);
    assert_eq!(answer(s, &ask("h2", "s2"), &LIMITS), Admission::Placed);

    db.run("DELETE FROM fleet_orders; DELETE FROM rollout_queue")
        .expect("reset");
    let two = Limits {
        live_rollouts: 2,
        ..LIMITS
    };
    for service in ["s1", "s2", "s3"] {
        decide(s, Wait::Beat, |s| enqueue(s, service)).expect("runs");
    }
    assert_eq!(
        answer(s, &ask("h3", "s3"), &two),
        Admission::Behind,
        "s1 and s2 are before it"
    );
    assert_eq!(
        answer(s, &ask("h2", "s2"), &two),
        Admission::Placed,
        "the second of two places"
    );
    assert_eq!(
        answer(s, &ask("h3", "s3"), &two),
        Admission::Behind,
        "the one place left is s1's, asked before s3"
    );
    assert_eq!(answer(s, &ask("h1", "s1"), &two), Admission::Placed);
    assert_eq!(answer(s, &ask("h3", "s3"), &two), Admission::Elsewhere);
}

/// Proves: two coordinators, each with a service waiting, asked at once with one place free, start
/// one service between them, the one asked first.
///
/// Catches: two coordinators that each find nothing rolling and each start a service.
#[test]
fn two_coordinators_start_one_service_between_them() {
    for round in 0..8 {
        let (db, _store, mut sessions) = rig(3);
        let mut setup = sessions.pop().expect("a session");
        for service in ["s1", "s2"] {
            decide(&mut setup, Wait::Beat, |s| enqueue(s, service)).expect("runs");
        }
        let start = Barrier::new(2);
        std::thread::scope(|scope| {
            for (mut session, (host, service)) in
                sessions.into_iter().zip([("h1", "s1"), ("h2", "s2")])
            {
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    loop {
                        let asked = ask(host, service);
                        match decide(&mut session, Wait::Beat, |s| admit(s, &asked, &LIMITS))
                            .expect("runs")
                        {
                            Gated::Busy => std::thread::yield_now(),
                            Gated::Decided(_) => break,
                        }
                    }
                });
            }
        });
        assert_eq!(
            db.rows("SELECT service FROM fleet_orders"),
            vec![vec![Some("s1".to_owned())]],
            "round {round}: the service asked first rolls"
        );
    }
}

/// Proves: a decider asks whether the plan still stands at the stage it read it at, before it
/// writes the next: a plan at that stage for that version (a group's too) answers yes, and a plan
/// at another stage, kept for another version (which counts as the first stage), or with no row
/// (the first stage) answers as its position says; two deciders under the gate that ask in turn,
/// the first writing the stage it asked about, find the second told it has moved.
///
/// Catches: a stage entered twice, with two audit lines, by two coordinators that read the same
/// position.
#[test]
fn a_stage_is_entered_once_whoever_asks_first() {
    let (db, _store, mut sessions) = rig(1);
    db.run(
        "INSERT INTO rollouts (service, version, entered) VALUES ('s1', '2.0', 1); \
         INSERT INTO group_rollouts (host_group, service, version, entered) \
         VALUES ('g', 's1', '2.0', 1)",
    )
    .expect("plans");
    let s = &mut sessions[0];
    assert!(stage_is_at(s, None, "s1", "2.0", 1, 1).expect("runs"));
    assert!(stage_is_at(s, Some("g"), "s1", "2.0", 1, 1).expect("runs"));
    assert!(
        !stage_is_at(s, None, "s1", "2.0", 2, 1).expect("runs"),
        "not at that stage"
    );
    // Another version counts as the plan's first stage, whatever the row says.
    assert!(stage_is_at(s, None, "s1", "3.0", 1, 1).expect("runs"));
    assert!(!stage_is_at(s, None, "s1", "3.0", 2, 1).expect("runs"));
    // No plan is at its first stage.
    assert!(stage_is_at(s, None, "s9", "2.0", 0, 0).expect("runs"));
    assert!(!stage_is_at(s, Some("other"), "s1", "2.0", 1, 0).expect("runs"));
    // The first of two deciders writes the stage; the second, asking after it, is told it moved.
    db.run("UPDATE rollouts SET entered = 2 WHERE service = 's1'")
        .expect("entered");
    assert!(!stage_is_at(s, None, "s1", "2.0", 1, 1).expect("runs"));
    assert!(stage_is_at(s, None, "s1", "2.0", 2, 1).expect("runs"));
}

/// Proves: of two coordinators that see the failure threshold crossed, one raises the halt and
/// the other finds it raised; the row holds the first's report.
///
/// Catches: two halts, with two audit lines and the second's report over the first's.
#[test]
fn a_halt_is_raised_once() {
    let (db, _store, mut sessions) = rig(1);
    let s = &mut sessions[0];
    assert!(raise_halt(s, "s1", "h1", "health", "2.0", "updater").expect("runs"));
    assert!(!raise_halt(s, "s1", "h2", "other", "2.0", "operator").expect("runs"));
    assert_eq!(
        db.rows("SELECT node_id, check_name, origin FROM halts"),
        vec![vec![
            Some("h1".into()),
            Some("health".into()),
            Some("updater".into())
        ]]
    );
}

/// Proves: a service at the head of the queue that cannot roll does not hold the others back. A
/// halted head is passed over while it is halted and takes the head again, with its place kept,
/// once the halt clears; a head that stopped asking for `queue_idle` seconds (its rollout over,
/// its hosts gone) leaves the queue, and the service behind it rolls; a head that goes on asking
/// keeps its place however long its rollout runs.
///
/// Catches: a queue that makes every later service wait for ever behind a halted, finished or
/// abandoned service, with `live_rollouts` 1.
#[test]
fn a_service_that_cannot_roll_does_not_hold_the_queue() {
    let (db, _store, mut sessions) = rig(1);
    let s = &mut sessions[0];

    // s1 asks first, rolls, is over and is then halted; s2 asks after it.
    assert_eq!(answer(s, &ask("h1", "s1"), &LIMITS), Admission::Placed);
    db.run("DELETE FROM fleet_orders").expect("h1 reported");
    raise_halt_in(&db, "s1");
    assert_eq!(
        answer(s, &ask("h2", "s2"), &LIMITS),
        Admission::Placed,
        "a halted service holds no place"
    );
    assert_eq!(
        db.rows("SELECT service FROM rollout_queue ORDER BY seq"),
        vec![vec![Some("s1".to_owned())], vec![Some("s2".to_owned())]],
        "a halted service keeps its row, and so its turn"
    );
    // The halt clears: s1 is the head again, and s2 waits behind it.
    db.run("DELETE FROM halts; DELETE FROM fleet_orders")
        .expect("the halt cleared, h2 reported");
    assert_eq!(answer(s, &ask("h2", "s2"), &LIMITS), Admission::Behind);

    // Idle: s1 and s2 asked at NOW and ask no more. An hour later s4 rolls at once, and the
    // queue holds s4 alone.
    let at = |seconds, host, service| Ask {
        now: NOW + seconds,
        ..ask(host, service)
    };
    assert_eq!(answer(s, &at(3600, "h4", "s4"), &LIMITS), Admission::Placed);
    assert_eq!(
        db.rows("SELECT service FROM rollout_queue ORDER BY seq"),
        vec![vec![Some("s4".to_owned())]]
    );

    // A service that goes on asking keeps its place, and its idle time counts from its last ask.
    assert_eq!(answer(s, &at(6600, "h5", "s4"), &LIMITS), Admission::Placed);
    assert_eq!(
        count(
            &db,
            "SELECT asked_unix FROM rollout_queue WHERE service = 's4'"
        ),
        (NOW + 6600).to_string()
    );
    assert_eq!(
        answer(s, &at(6600, "h6", "s5"), &LIMITS),
        Admission::Elsewhere
    );
    assert_eq!(
        answer(s, &at(6600 + 3599, "h6", "s5"), &LIMITS),
        Admission::Behind,
        "s4 asked 3599 seconds ago: it still holds its place"
    );
    assert_eq!(
        answer(s, &at(6600 + 3600, "h6", "s5"), &LIMITS),
        Admission::Placed,
        "and an hour after its last ask it does not"
    );
}

/// Proves: an uninstall order is written for a host that holds no order; asked again with the
/// same order id it is the host's order already and is sent again; any other order of the host
/// (another uninstall of the service, an uninstall of another service, an update) keeps it from
/// being written, and nothing more is written.
///
/// Catches: a second order for a host, which a host would run beside the first, or an uninstall
/// sent again that is refused as if the host were busy.
#[test]
fn an_uninstall_is_placed_once_and_never_beside_another_order() {
    let (db, _store, mut sessions) = rig(1);
    let s = &mut sessions[0];
    let uninstall = |host| Ask {
        target: "1.0",
        ..ask(host, "s1")
    };
    let place =
        |s: &mut Session, ask: &Ask<'_>, order_id: &str| match decide(s, Wait::Beat, |held| {
            place_uninstall(held, ask, order_id)
        })
        .expect("the decision runs")
        {
            Gated::Decided(placed) => placed,
            Gated::Busy => panic!("nothing else holds the gate"),
        };
    assert_eq!(
        place(s, &uninstall("h1"), "uninstall-1"),
        Uninstalling::Placed
    );
    assert_eq!(
        place(s, &uninstall("h1"), "uninstall-1"),
        Uninstalling::Existing
    );
    assert_eq!(
        place(s, &uninstall("h1"), "uninstall-2"),
        Uninstalling::HostBusy
    );
    let other = Ask {
        target: "1.0",
        ..ask("h1", "s2")
    };
    assert_eq!(place(s, &other, "uninstall-1"), Uninstalling::HostBusy);
    assert_eq!(answer(s, &ask("h2", "s1"), &LIMITS), Admission::Placed);
    assert_eq!(
        place(s, &uninstall("h2"), "uninstall-3"),
        Uninstalling::HostBusy
    );
    assert_eq!(
        db.rows("SELECT host_id, service, target, uninstall_id FROM fleet_orders ORDER BY host_id"),
        vec![
            vec![
                Some("h1".into()),
                Some("s1".into()),
                Some("1.0".into()),
                Some("uninstall-1".into())
            ],
            vec![
                Some("h2".into()),
                Some("s1".into()),
                Some("2.0".into()),
                None
            ],
        ]
    );
}
