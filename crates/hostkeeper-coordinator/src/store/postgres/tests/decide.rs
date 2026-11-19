//! A heartbeat's deciding transaction (`docs/design/coordinator-multi-instance.md`, sections 5.2
//! and 5.3), on the store: the gate a beat never waits for, every input read afresh under it,
//! the stores' changes made in it landing with it or not at all, the orders written back each on
//! the row as read with the host's key as the backstop, and the compare-and-set of a stage.
//!
//! Two coordinators are two stores opened on one database; the test's own sessions hold a lock
//! or write a row in between. Nothing sleeps and no time is measured.

use super::TestDatabase;
use super::coordinator::Holding;
use super::edits::{Instance, counter, instance, label, rows_of_labels, version};
use crate::ops::{AuditEntry, Write};
use crate::store::decision::{OrderRow, StageClaim, Writes};
use crate::store::{Backend, Decision, Opened, Part, Scope};

/// What a session of the test runs to hold the gate, as a decision or an operator's edit does.
const HOLD_GATE: &str = "BEGIN; SELECT 1 FROM hostkeeper.rollout_gate WHERE id = 1 FOR UPDATE";

/// The decision `instance` opens now, which the test expects to open.
fn deciding(instance: &Instance) -> Box<Decision> {
    match instance.store.decide() {
        Ok(Opened::Deciding(decision)) => decision,
        Ok(Opened::Busy) => panic!("the gate was held"),
        Ok(Opened::Alone) => panic!("a Postgres store decides in a transaction"),
        Err(why) => panic!("the decision did not open: {why}"),
    }
}

fn order(host: &str, service: &str, target: &str, at_unix: i64) -> OrderRow {
    OrderRow {
        host_id: host.to_owned(),
        service: service.to_owned(),
        target: target.to_owned(),
        uninstall_id: None,
        at_unix,
        order_try: 1,
    }
}

/// The orders the rows hold, as (host, service, target).
fn orders(db: &TestDatabase) -> Vec<(String, String, String)> {
    db.rows("SELECT host_id, service, target FROM fleet_orders ORDER BY host_id")
        .into_iter()
        .map(|row| {
            let text = |n: usize| row[n].clone().unwrap_or_default();
            (text(0), text(1), text(2))
        })
        .collect()
}

fn owned(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    rows.iter()
        .map(|(a, b, c)| ((*a).to_owned(), (*b).to_owned(), (*c).to_owned()))
        .collect()
}

/// Proves: a decision asks for the gate without waiting: held by another transaction, the
/// decision is refused at once and holds nothing; once it is free, the decision opens.
/// Catches: a beat that queues at the gate behind a decision or an operator's edit (every beat
/// of the fleet would wait for one hold), or one that decides without it.
#[test]
fn a_beat_finds_the_gate_held_and_decides_nothing() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    let holder = Holding::start(&db, HOLD_GATE, "COMMIT");
    assert!(matches!(a.store.decide(), Ok(Opened::Busy)));
    holder.release();
    drop(deciding(&a));
    assert!(matches!(a.store.decide(), Ok(Opened::Deciding(_))));
}

/// Proves: under the gate a decision reads afresh each part another coordinator changed since
/// this one's copy, and the fleet's rows: the database's clock, the quiet time, the orders,
/// bakes and tries, and the beats another instance heard (not those this one heard itself).
/// Catches: a decision that decides from a cache another coordinator's commit made stale (an
/// order past a halt it raised), or that misses a row of the fleet's.
#[test]
fn a_decision_reads_every_input_afresh_under_the_gate() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    label(&a.ops, "d1", "rack", "r1");
    let own = b.store.inner.instance.id.clone();
    db.run(&format!(
        "INSERT INTO fleet_hosts (host_id, role, beat, seen_unix, sent_unix, instance) VALUES \
           ('h1', 'daemon', '\\x7b7d', 10, 9, 'another'), \
           ('h2', 'daemon', '\\x7b7d', 10, 9, '{own}'); \
         INSERT INTO fleet_bakes (host_id, service, target, beats, at_unix) \
           VALUES ('h1', 'web', '2.0', 3, 10); \
         INSERT INTO fleet_orders (host_id, service, target, uninstall_id, at_unix, try) \
           VALUES ('h1', 'web', '2.0', NULL, 10, 4); \
         INSERT INTO fleet_tries (host_id, service, try) VALUES ('h1', 'web', 3); \
         UPDATE cluster_quiet SET until_unix = 99"
    ))
    .expect("the rows are written");
    let mut decision = deciding(&b);
    let (state, _) = decision
        .take_ops()
        .expect("the ops part moved on: read afresh");
    assert_eq!(state.hosts["d1"].labels["rack"], "r1");
    assert!(
        decision.take_desired().is_none(),
        "the desired part did not move on"
    );
    let rows = decision.rows().clone();
    assert!(rows.now_unix > 1_000_000_000, "the database's clock");
    assert_eq!(rows.quiet_until, 99);
    let hosts: Vec<&str> = rows.hosts.iter().map(|h| h.host_id.as_str()).collect();
    assert_eq!(hosts, ["h1"], "h2 was heard by the reader itself");
    assert_eq!(rows.bakes.len(), 1);
    assert_eq!(rows.orders[0].order_try, 4);
    assert_eq!(rows.tries[0].order_try, 3);
}

/// Proves: the changes a store makes on the deciding thread are written in the decision's
/// transaction, numbered on from the audit counter, and land with the orders at its end; a
/// decision dropped before its end lands none of them.
/// Catches: a store's change committed apart from the decision (a halt or a stage acknowledged
/// although the decision rolled back), or a decision whose orders land without its changes.
#[test]
fn the_changes_made_in_a_decision_land_with_it_or_not_at_all() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    let before = (counter(&db), version(&db, Part::Ops));
    let decision = deciding(&a);
    let line = label(&a.ops, "d1", "rack", "r1");
    assert_eq!(line.seq, u64::try_from(before.0).unwrap_or_default());
    drop(decision);
    assert!(
        rows_of_labels(&db).is_empty(),
        "a dropped decision lands nothing"
    );
    assert_eq!((counter(&db), version(&db, Part::Ops)), before);
    let b = instance(&db, "b");
    let decision = deciding(&b);
    let line = label(&b.ops, "d1", "rack", "r2");
    assert_eq!(line.seq, u64::try_from(before.0).unwrap_or_default());
    let writes = Writes {
        given: vec![order("h1", "web", "2.0", 10)],
        ..Writes::default()
    };
    decision.finish(&writes).expect("the decision lands");
    assert_eq!(
        rows_of_labels(&db),
        [("d1".to_owned(), "rack".to_owned(), "r2".to_owned())]
    );
    assert_eq!(orders(&db), owned(&[("h1", "web", "2.0")]));
    assert_eq!(counter(&db), before.0 + 1);
    assert_eq!(version(&db, Part::Ops), before.1 + 1);
}

/// Proves: the orders a decision writes back are each conditional on the row as it read it: an
/// order inserted by another writer after the read refuses the decision's order of the same host
/// (the host's key: one update at a time), and so does a row replaced since; an ended order
/// already gone is no refusal. A refused decision writes nothing.
/// Catches: a second order of one host, or an order written over one another writer changed.
#[test]
fn one_host_never_has_two_orders() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    let decision = deciding(&a);
    // Another writer that took no gate, which no coordinator is: the key is the backstop.
    db.run(
        "INSERT INTO fleet_orders (host_id, service, target, uninstall_id, at_unix, try) \
         VALUES ('h1', 'db', '5.0', NULL, 10, 1)",
    )
    .expect("the row is written");
    let given = Writes {
        given: vec![order("h1", "web", "2.0", 10)],
        ..Writes::default()
    };
    assert!(decision.finish(&given).is_err());
    assert_eq!(orders(&db), owned(&[("h1", "db", "5.0")]));
    let decision = deciding(&a);
    let replaced = Writes {
        replaced: vec![(order("h1", "db", "4.0", 10), order("h1", "db", "6.0", 20))],
        ..Writes::default()
    };
    assert!(
        decision.finish(&replaced).is_err(),
        "the row is not as read"
    );
    let decision = deciding(&a);
    let mut moved = order("h1", "db", "6.0", 20);
    moved.uninstall_id = Some("uninstall-1".to_owned());
    let writes = Writes {
        ended: vec![order("h2", "web", "2.0", 10)],
        replaced: vec![(order("h1", "db", "5.0", 10), moved)],
        ..Writes::default()
    };
    decision.finish(&writes).expect("as read: it lands");
    assert_eq!(orders(&db), owned(&[("h1", "db", "6.0")]));
    assert_eq!(
        db.rows("SELECT uninstall_id FROM fleet_orders"),
        [[Some("uninstall-1".to_owned())]]
    );
}

/// Proves: a decision keeps the try of an order that ended as the highest of its host and service,
/// never lowering one another decision kept.
/// Catches: a try lost when the order ends (the next order of another coordinator reuses it and
/// the host refuses it as stale), or one lowered.
#[test]
fn the_try_of_an_ended_order_is_kept_as_the_highest() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    let tries = |order_try| Writes {
        tries: vec![crate::store::decision::TryRow {
            host_id: "h1".to_owned(),
            service: "web".to_owned(),
            order_try,
        }],
        ..Writes::default()
    };
    deciding(&a).finish(&tries(5)).expect("it lands");
    deciding(&a).finish(&tries(3)).expect("it lands");
    assert_eq!(
        db.rows("SELECT try FROM fleet_tries"),
        [[Some("5".to_owned())]]
    );
}

/// Proves: a stage is entered by a compare-and-set on the plan's row: from the stage the decision
/// read it at only, or, when it read no plan at the version, only while there is none; for the
/// rollout file's plans and a host group's alike. Outside a decision a claim takes (the edit
/// that writes the stage holds the gate).
/// Catches: a stage entered twice by two decisions that both read it at the stage before (two
/// `rollout_advance` lines), or a stage entered over another decision's.
#[test]
fn a_stage_is_claimed_by_a_compare_and_set_on_the_plans_row() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    db.run(
        "INSERT INTO rollouts (service, version, entered) VALUES ('web', '2.0', 1); \
         INSERT INTO host_groups (name, generation, policy, by_name, at_unix) \
           VALUES ('g', 1, '{}', 'alice', 1); \
         INSERT INTO group_rollouts (host_group, service, version, entered) \
           VALUES ('g', 'web', '2.0', 1)",
    )
    .expect("the plans are written");
    let claim = |group, from, to| StageClaim {
        group,
        service: "web",
        version: "2.0",
        from,
        to,
    };
    assert_eq!(
        a.store.claim_stage(&claim(None, Some(5), 6)).ok(),
        Some(true),
        "outside a decision"
    );
    let decision = deciding(&a);
    for group in [None, Some("g")] {
        assert_eq!(
            a.store.claim_stage(&claim(group, Some(1), 2)).ok(),
            Some(true)
        );
        assert_eq!(
            a.store.claim_stage(&claim(group, Some(1), 2)).ok(),
            Some(false),
            "moved on"
        );
        assert_eq!(
            a.store.claim_stage(&claim(group, None, 1)).ok(),
            Some(false),
            "a plan at it"
        );
    }
    let other = StageClaim {
        version: "3.0",
        ..claim(Some("g"), None, 1)
    };
    assert_eq!(
        a.store.claim_stage(&other).ok(),
        Some(true),
        "no plan at 3.0 yet"
    );
    decision.finish(&Writes::default()).expect("it lands");
    let entered =
        db.rows("SELECT entered FROM rollouts UNION ALL SELECT entered FROM group_rollouts");
    assert_eq!(entered, [[Some("2".to_owned())], [Some("2".to_owned())]]);
}

/// Proves: a write of a decision that fails ends it: every later change of it, a claim and its
/// end are refused, and nothing it wrote lands.
/// Catches: a decision that answers an order after one of its changes was refused (the order
/// lands without the halt or the stage it was decided with).
#[test]
fn a_write_that_fails_ends_the_decision() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    db.run(
        "CREATE FUNCTION refuse() RETURNS trigger LANGUAGE plpgsql AS \
           $$BEGIN RAISE EXCEPTION 'refused by the test'; END$$; \
         CREATE TRIGGER refuse BEFORE INSERT ON audit FOR EACH ROW EXECUTE FUNCTION refuse()",
    )
    .expect("the trigger is made");
    let decision = deciding(&a);
    let change = |value: &str| {
        let mut entry = AuditEntry::new("alice", "set_labels");
        entry.node_id = Some("d1".to_owned());
        a.ops.change(Write::BeforeMemory, entry, |state| {
            state
                .hosts
                .entry("d1".to_owned())
                .or_default()
                .labels
                .insert("rack".to_owned(), value.to_owned());
        })
    };
    assert!(change("r1").is_err(), "its audit line is refused");
    db.run("DROP TRIGGER refuse ON audit").expect("dropped");
    assert!(change("r2").is_err(), "the decision is over");
    let claim = StageClaim {
        group: None,
        service: "web",
        version: "2.0",
        from: None,
        to: 1,
    };
    assert!(a.store.claim_stage(&claim).is_err());
    assert!(decision.finish(&Writes::default()).is_err());
    assert!(rows_of_labels(&db).is_empty());
    drop(deciding(&a));
}

/// Proves: an edit begun on the deciding thread of a part the decision does not hold (the
/// accounts) is refused rather than made outside it, and a failed claim ends the decision.
/// Catches: an edit that would wait for the gate its own thread holds, or a decision that goes
/// on after its claim failed.
#[test]
fn an_edit_outside_the_decisions_parts_and_a_failed_claim_are_refused() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    let decision = deciding(&a);
    assert!(a.store.begin(Scope::ACCOUNTS).is_err());
    db.run("ALTER TABLE group_rollouts RENAME TO group_rollouts_gone")
        .expect("renamed");
    let claim = StageClaim {
        group: Some("g"),
        service: "web",
        version: "2.0",
        from: Some(1),
        to: 2,
    };
    assert!(a.store.claim_stage(&claim).is_err());
    assert!(decision.finish(&Writes::default()).is_err());
}

/// Proves: a decision whose reads fail is refused and holds nothing: the gate is free after it,
/// and a store that is lost decides nothing.
/// Catches: a decision that holds the gate after it failed (every beat of the fleet would find
/// it busy), or one made on a lost session.
#[test]
fn a_decision_that_cannot_read_or_reach_the_database_holds_nothing() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    db.run("ALTER TABLE fleet_bakes RENAME TO fleet_bakes_gone")
        .expect("renamed");
    assert!(a.store.decide().is_err());
    let holder = Holding::start(&db, &format!("{HOLD_GATE} NOWAIT"), "COMMIT");
    holder.release();
    db.run("ALTER TABLE fleet_bakes_gone RENAME TO fleet_bakes")
        .expect("renamed back");
    db.end_sessions();
    assert!(
        a.ops
            .change(Write::BeforeMemory, AuditEntry::new("alice", "x"), |_| ())
            .is_err()
    );
    assert!(a.store.decide().is_err(), "the store is lost");
}

/// Proves: a desired change made in a decision (a rollback's, which writes no audit line) lands
/// with the decision and takes its queue number there; the coordinator's copy of the desired state
/// is then read again (its number is the rows'), while its ops copy, unchanged, is not.
/// Catches: a desired change of a decision committed apart from it, or a copy labelled current
/// although the commit stamped a number memory does not hold.
#[test]
fn a_desired_change_in_a_decision_lands_with_it_and_is_read_again() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    let decision = deciding(&a);
    a.desired
        .set("web", crate::testutil::desired("2.0", 1, 1))
        .expect("set in the decision");
    decision.finish(&Writes::default()).expect("it lands");
    assert_eq!(
        db.rows("SELECT version, set_seq IS NOT NULL FROM desired WHERE service = 'web'"),
        [[Some("2.0".to_owned()), Some("t".to_owned())]]
    );
    assert_eq!(a.store.stale(), [Part::Desired], "the copy is read again");
}

/// Proves: a second decision on one store waits for the decisions' session the first holds, and
/// opens once the first ends.
/// Catches: two decisions sharing one session (their writes in one transaction), or a second
/// that never wakes.
#[test]
fn a_second_decision_waits_for_the_decisions_session() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    let first = deciding(&a);
    std::thread::scope(|s| {
        let second = s.spawn(|| matches!(a.store.decide(), Ok(Opened::Deciding(_))));
        // A progress event, not a time: the second is counted as waiting.
        while a.store.waited() == 0 {
            std::thread::yield_now();
        }
        drop(first);
        assert_eq!(second.join().ok(), Some(true));
    });
}
