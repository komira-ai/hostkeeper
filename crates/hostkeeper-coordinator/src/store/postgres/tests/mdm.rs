//! The MDM tables of migration `0011` on Postgres: the rules the schema keeps by itself, a claim
//! that skips a row another transaction holds, what the store says of rows it cannot read or a
//! database it cannot reach, and the release before on a database holding `0011`, and this release
//! on the release before's.

use super::{SPENT_NOW, TestDatabase};
use crate::store::mdm::{
    AbmBatch, AbmDevice, AbmState, Answer, Challenge, Enqueued, Enrollment, Issuance, MdmError,
    MdmStore, NewCommand,
};
use crate::store::postgres::mdm::PgMdm;
use crate::store::postgres::{load_all, migrate};

use super::devices::with_session;

fn command(id: &str, enrollment_id: &str) -> NewCommand {
    NewCommand {
        id: id.to_owned(),
        enrollment_id: enrollment_id.to_owned(),
        command_type: "ProfileList".to_owned(),
        payload: b"plist".to_vec(),
        created_unix: 1,
    }
}

/// Proves: the schema keeps its own rules whatever the code does: a serial cannot have two live
/// enrollments, an enrollment, an identity and a command each hold only the states the code
/// spells, a command is of an enrollment that exists, and an identity holds a certificate whole or
/// not at all.
///
/// Catches: a migration that lets a second live enrollment of a serial in, or lets a command hang
/// on an enrollment that is not there, which the stores' rules would then be the only defence of.
#[test]
fn the_schema_refuses_what_the_rules_of_the_store_forbid() {
    let db = TestDatabase::new();
    let (store, _) = db.open("schema").expect("the store opens");
    let insert = |id: &str, serial: &str, state: &str| {
        db.run(&format!(
            "INSERT INTO mdm_enrollments (enrollment_id, serial, udid, state, created_unix) \
             VALUES ('{id}', '{serial}', 'u', '{state}', 1)"
        ))
    };
    insert("e1", "S1", "live").expect("a live enrollment");
    insert("e2", "S1", "replaced").expect("a replaced one of the same serial");
    assert!(insert("e3", "S1", "live").is_err(), "a second live one");
    assert!(insert("e4", "S2", "lost").is_err(), "a state nobody spells");
    assert!(
        insert("bad id", "S2", "live").is_err(),
        "an id the gate refuses"
    );
    assert!(
        db.run(
            "INSERT INTO mdm_commands (id, enrollment_id, command_type, payload, state, \
             created_unix) VALUES ('c1', 'nobody', 'X', ''::bytea, 'queued', 1)"
        )
        .is_err(),
        "a command of no enrollment"
    );
    assert!(
        db.run(
            "INSERT INTO mdm_commands (id, enrollment_id, command_type, payload, state, \
             created_unix) VALUES ('c1', 'e1', 'X', ''::bytea, 'half', 1)"
        )
        .is_err(),
        "a state nobody spells"
    );
    assert!(
        db.run(
            "INSERT INTO mdm_identities (challenge_sha256, serial, udid, expires_unix, state) \
             VALUES ('h', 'S', 'u', 1, 'issued')"
        )
        .is_err(),
        "an issued identity without a certificate"
    );
    assert!(
        db.run(
            "INSERT INTO mdm_identities (challenge_sha256, serial, udid, expires_unix, state, \
             cert_sha256) VALUES ('h', 'S', 'u', 1, 'issued', 'c')"
        )
        .is_err(),
        "a certificate hash without the certificate"
    );
    assert!(
        db.run("INSERT INTO abm_state (id, cursor) VALUES (2, '')")
            .is_err(),
        "a second sync state"
    );
    drop(store);
}

/// Proves: a claim skips a command another transaction holds locked and takes the next one, and
/// answers nothing when the only queued command is held, so two connections of one Mac cannot take
/// the same command.
///
/// Catches: a claim that waits on a locked row or takes it twice.
#[test]
fn a_claim_skips_a_command_another_transaction_holds() {
    let db = TestDatabase::new();
    let (store, _) = db.open("claim").expect("the store opens");
    let mdm = PgMdm::over(&store);
    mdm.replace_live(&Enrollment::new("e1", "S1", "u", 1))
        .expect("an enrollment");
    for id in ["a", "b"] {
        assert_eq!(mdm.enqueue(&command(id, "e1")), Ok(Enqueued::Queued));
    }
    let mut holder = db.client();
    holder
        .batch_execute(
            "SET search_path TO hostkeeper; BEGIN; \
             SELECT id FROM mdm_commands WHERE id = 'a' FOR UPDATE",
        )
        .expect("the row is held");
    let taken = mdm
        .claim("e1", &[], 5)
        .expect("a claim")
        .expect("one is taken");
    assert_eq!(taken.id, "b");
    assert_eq!(mdm.claim("e1", &[], 6), Ok(None));
    holder.batch_execute("COMMIT").expect("the row is let go");
    let taken = mdm
        .claim("e1", &[], 7)
        .expect("a claim")
        .expect("a is taken");
    assert_eq!(taken.id, "a");
}

/// Proves: a row that holds a state this release does not spell is an error naming the table and
/// the state, for an enrollment, a command and an identity, and a consume of such a challenge
/// changes nothing; and a table that cannot be read or written is an error, not an empty answer.
///
/// Catches: a row of a later release read as another state, or a failed read given back as "none".
#[test]
fn a_row_of_an_unknown_state_and_a_table_that_is_gone_are_errors() {
    let db = TestDatabase::new();
    let (store, _) = db.open("unknown").expect("the store opens");
    let mdm = PgMdm::over(&store);
    mdm.replace_live(&Enrollment::new("e1", "S1", "u", 1))
        .expect("an enrollment");
    mdm.enqueue(&command("c1", "e1")).expect("a command");
    mdm.issue_challenge(&Challenge {
        challenge_sha256: "h1".to_owned(),
        serial: "S1".to_owned(),
        udid: "u".to_owned(),
        expires_unix: 100,
        confirmed: false,
    })
    .expect("a challenge");
    db.run(
        "ALTER TABLE mdm_enrollments DROP CONSTRAINT mdm_enrollments_state; \
         ALTER TABLE mdm_commands DROP CONSTRAINT mdm_commands_state; \
         ALTER TABLE mdm_identities DROP CONSTRAINT mdm_identities_state; \
         ALTER TABLE mdm_identities DROP CONSTRAINT mdm_identities_issued; \
         UPDATE mdm_enrollments SET state = 'later'; \
         UPDATE mdm_commands SET state = 'later'; \
         UPDATE mdm_identities SET state = 'later'",
    )
    .expect("the rows are of a later release");
    let said = |e: Result<_, MdmError>, what: &str| {
        assert_eq!(
            e.map(|_: Option<()>| ()),
            Err(MdmError::Store(format!(
                "{what} holds a state this release does not know: later"
            )))
        );
    };
    said(mdm.enrollment("e1").map(|_| None), "mdm_enrollments");
    said(mdm.command("c1").map(|_| None), "mdm_commands");
    said(mdm.challenge("h1").map(|_| None), "mdm_identities");
    said(
        mdm.consume_challenge(
            "h1",
            "k",
            &Issuance {
                cert_sha256: "c".to_owned(),
                cert_der: Vec::new(),
                not_after_unix: 1,
            },
            1,
        )
        .map(|_| None),
        "mdm_identities",
    );
    db.run("DROP TABLE abm_state; DROP TABLE abm_devices")
        .expect("the tables are gone");
    assert!(matches!(mdm.abm_state(), Err(MdmError::Store(_))));
    assert!(matches!(mdm.abm_devices(), Err(MdmError::Store(_))));
    assert!(matches!(
        mdm.abm_apply(&AbmBatch {
            upserts: vec![AbmDevice {
                serial: "S".to_owned(),
                model: None,
                description: None,
                profile_status: "empty".to_owned(),
                profile_uuid: None,
                first_seen_unix: 1,
                synced_unix: 1,
                removed_unix: None,
            }],
            ..AbmBatch::default()
        }),
        Err(MdmError::Store(_))
    ));
}

/// Proves: with the tables gone, every call answers an error naming the failure, and none answers
/// an empty result or a refusal of its own.
///
/// Catches: a call that takes a failed read for "no rows", which a caller would act on as if the
/// serial had no enrollment.
#[test]
fn every_call_answers_an_error_when_the_tables_are_gone() {
    let db = TestDatabase::new();
    let (store, _) = db.open("gone").expect("the store opens");
    let mdm = PgMdm::over(&store);
    db.run("DROP TABLE mdm_commands, mdm_identities, mdm_enrollments, abm_devices, abm_state")
        .expect("the tables are gone");
    let issued = Issuance {
        cert_sha256: "c".to_owned(),
        cert_der: Vec::new(),
        not_after_unix: 1,
    };
    let challenge = Challenge {
        challenge_sha256: "h".to_owned(),
        serial: "S".to_owned(),
        udid: "u".to_owned(),
        expires_unix: 1,
        confirmed: false,
    };
    let failures = [
        mdm.replace_live(&Enrollment::new("e", "S", "u", 1)).err(),
        mdm.check_out("e", 1).err(),
        mdm.set_push("e", b"t", "m", "p").err(),
        mdm.record_push("e", 1).err(),
        mdm.record_contact("e", 1).err(),
        mdm.enrollment("e").err(),
        mdm.live_enrollment("S").err(),
        mdm.issue_challenge(&challenge).err(),
        mdm.consume_challenge("h", "k", &issued, 1).err(),
        mdm.challenge("h").err(),
        mdm.identity("c").err(),
        mdm.kill_identity("c").err(),
        mdm.enqueue(&command("c", "e")).err(),
        mdm.claim("e", &[], 1).err(),
        mdm.answer("e", "c", Answer::Error, None, 1).err(),
        mdm.requeue_timed_out(1).err(),
        mdm.command("c").err(),
        mdm.commands("e").err(),
        mdm.abm_apply(&AbmBatch {
            state: Some(AbmState {
                cursor: String::new(),
                profile_uuid: None,
                profile_digest: None,
                synced_unix: None,
            }),
            ..AbmBatch::default()
        })
        .err(),
        mdm.abm_device("S").err(),
        mdm.abm_devices().err(),
        mdm.abm_state().err(),
    ];
    for (n, failure) in failures.iter().enumerate() {
        assert!(
            matches!(failure, Some(MdmError::Store(_))),
            "call {n} answered {failure:?}"
        );
    }
}

/// Proves: `0011` does not stop the release before it (migrations `0001` to `0010`, whose start
/// never reads the MDM tables) on a database this release migrated and whose MDM tables hold an
/// enrollment: its runner takes the database and runs nothing, and the state reads as this release
/// loads it. And the other way: this release starts on a database the release before migrated, runs
/// `0011` and `0012`, and the
/// MDM tables are empty.
///
/// Catches: an MDM table that makes the previous coordinator refuse to start (the back-out of a
/// release would need the database restored), or a migration this release does not run on a
/// database of the release before.
#[test]
fn the_release_before_starts_on_a_database_with_0011_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    PgMdm::over(&store)
        .replace_live(&Enrollment::new("e1", "S1", "u", 1))
        .expect("an enrollment");
    store.stop();
    let (taken, before_loaded) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..10]).expect("the release before's set");
        let taken = migrate::apply(session, &before);
        (
            taken,
            load_all(session, SPENT_NOW).expect("the state reads"),
        )
    });
    assert!(
        matches!(&taken, Ok(ran) if ran.is_empty()),
        "0011 says older releases may run: {taken:?}"
    );
    assert_eq!(before_loaded, loaded);
    assert_eq!(
        db.rows("SELECT enrollment_id FROM mdm_enrollments").len(),
        1
    );

    let older = TestDatabase::new();
    let ran = with_session(&older, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..10]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, (1..=10).collect::<Vec<_>>());
    let (store, _) = older.open("this release").expect("this release starts");
    assert_eq!(PgMdm::over(&store).abm_devices(), Ok(Vec::new()));
    assert_eq!(PgMdm::over(&store).live_enrollment("S1"), Ok(None));
    store.stop();
    assert_eq!(
        older.rows("SELECT version FROM schema_version WHERE version >= 11 ORDER BY version"),
        vec![
            vec![Some("11".to_owned())],
            vec![Some("12".to_owned())],
            vec![Some("13".to_owned())],
            vec![Some("14".to_owned())],
            vec![Some("15".to_owned())],
            vec![Some("16".to_owned())]
        ]
    );
}
