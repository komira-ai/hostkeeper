//! The MDM setup tables of migration `0012` on Postgres: the rules the schema keeps by itself, what
//! the database holds of a sealed secret, what the store says of a database it cannot read, and the
//! release before on a database holding `0012`, and this release on the release before's.

use std::sync::Arc;

use super::{SPENT_NOW, TestDatabase};
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};
use crate::mdm_setup::MdmSetup;
use crate::store::mdm_setup::{Sealed, SecretRow, SetupError, SetupStore};
use crate::store::postgres::mdm_setup::PgSetup;
use crate::store::postgres::{load_all, migrate};

use super::devices::with_session;

fn row(kind: &str, version: u32) -> SecretRow {
    SecretRow {
        kind: kind.to_owned(),
        version,
        sealed: Sealed(vec![9; 40]),
        fingerprint: "ab".repeat(32),
        not_after_unix: None,
        created_unix: 1,
        superseded_unix: None,
    }
}

/// Proves: the schema keeps its own rules whatever the code does: a kind holds lower-case letters,
/// digits and `_`, a version is 1 or more, a sealed value is at least a nonce and a tag, a
/// fingerprint is 64 hex digits, a version of a kind is there once, and a setting's name is as a
/// kind's.
///
/// Catches: a migration that lets a row in that the seal could never have made, which the code
/// would then be the only defence against.
#[test]
fn the_schema_refuses_what_the_rules_of_the_store_forbid() {
    let db = TestDatabase::new();
    let (store, _) = db.open("schema").expect("the store opens");
    let insert = |kind: &str, version: i64, sealed_len: usize, fingerprint: &str| {
        db.run(&format!(
            "INSERT INTO mdm_secrets (kind, version, sealed, fingerprint, created_unix) \
             VALUES ('{kind}', {version}, decode('{}', 'hex'), '{fingerprint}', 1)",
            "00".repeat(sealed_len)
        ))
    };
    let good = "cd".repeat(32);
    insert("tls_key", 1, 28, &good).expect("a row");
    assert!(insert("tls_key", 1, 28, &good).is_err(), "a version twice");
    assert!(insert("TLS", 1, 28, &good).is_err(), "upper case");
    assert!(insert("tls-key", 1, 28, &good).is_err(), "a dash");
    assert!(insert(&"k".repeat(41), 1, 28, &good).is_err(), "too long");
    assert!(insert("tls_cert", 0, 28, &good).is_err(), "version 0");
    assert!(
        insert("tls_cert", 1, 27, &good).is_err(),
        "too short to be sealed"
    );
    assert!(
        insert("tls_cert", 1, 28, "ABC").is_err(),
        "not a fingerprint"
    );
    assert!(
        db.run("INSERT INTO mdm_settings (name, value) VALUES ('Bad Name', '1')")
            .is_err()
    );
    db.run("INSERT INTO mdm_settings (name, value) VALUES ('tls_renew_days', '1')")
        .expect("a setting");
    assert!(
        db.run("INSERT INTO mdm_settings (name, value) VALUES ('tls_renew_days', '2')")
            .is_err(),
        "a name twice"
    );
    store.stop();
}

/// Proves: through the store of the database, a secret is a row whose `sealed` column holds the
/// nonce, the ciphertext and the tag and not a byte of the value, whose fingerprint is the SHA-256
/// of the value, and which opens to the value under the key and for its kind only; a restart reads
/// the same.
///
/// Catches: a secret that reaches the database as plaintext, or a row that opens as another kind.
#[test]
fn a_secret_in_the_database_is_sealed() {
    let db = TestDatabase::new();
    let (store, _) = db.open("sealed").expect("the store opens");
    let key = || MasterKey::from_bytes(&[3; 32]).expect("a key");
    let setup = MdmSetup::new(
        Arc::new(PgSetup::over(&store)),
        Some(key()),
        crate::enrollment::os_random(),
    );
    let value = Secret::new(b"RSA PRIVATE KEY plaintext marker".to_vec());
    let info = setup
        .put_secret(SecretKind::PushKey, &value, Some(2_000_000_000), 77)
        .expect("stored");
    let rows = db.rows("SELECT kind, version, encode(sealed, 'hex'), fingerprint, not_after_unix, created_unix, superseded_unix FROM mdm_secrets");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].as_deref(), Some("push_key"));
    assert_eq!(rows[0][1].as_deref(), Some("1"));
    let sealed = rows[0][2].clone().expect("sealed");
    assert!(!sealed.contains(&hex::encode(value.expose())), "{sealed}");
    assert_eq!(rows[0][3].as_deref(), Some(info.fingerprint.as_str()));
    assert_eq!(rows[0][4].as_deref(), Some("2000000000"));
    assert_eq!(rows[0][5].as_deref(), Some("77"));
    assert_eq!(rows[0][6], None);

    let again = MdmSetup::new(
        Arc::new(PgSetup::over(&store)),
        Some(key()),
        crate::enrollment::os_random(),
    );
    assert_eq!(again.open(SecretKind::PushKey).expect("opens"), Some(value));
    db.run("UPDATE mdm_secrets SET kind = 'tls_key'")
        .expect("moved to another kind");
    assert_eq!(again.unopened().expect("checked"), vec![SecretKind::TlsKey]);
    store.stop();
}

/// Proves: with the tables gone, or the session stopped, every call answers an error naming the
/// failure, and none answers an empty result: a read that failed is not "no secret".
///
/// Catches: a call that takes a failed read for no rows, which the start would take for a store
/// with no sealed secret and make a new key over the sealed ones.
#[test]
fn every_call_answers_an_error_when_the_tables_are_gone() {
    let db = TestDatabase::new();
    let (store, _) = db.open("gone").expect("the store opens");
    let setup = PgSetup::over(&store);
    db.run("DROP TABLE mdm_secrets; DROP TABLE mdm_settings")
        .expect("the tables are gone");
    assert!(matches!(setup.secrets(), Err(SetupError::Store(_))));
    assert!(matches!(setup.settings(), Err(SetupError::Store(_))));
    assert!(matches!(
        setup.put_secret(&row("tls_key", 1)),
        Err(SetupError::Store(_))
    ));
    assert!(matches!(
        setup.prune_superseded(1),
        Err(SetupError::Store(_))
    ));
    assert!(matches!(
        setup.set_setting("tls_renew_days", "1"),
        Err(SetupError::Store(_))
    ));
    assert!(MdmSetup::holds_secrets(&setup).is_err());

    let (other, _) = TestDatabase::new().open("stopped").expect("a store");
    let stopped = PgSetup::over(&other);
    other.stop();
    assert!(matches!(stopped.secrets(), Err(SetupError::Store(_))));
    assert!(matches!(
        stopped.put_secret(&row("tls_key", 1)),
        Err(SetupError::Store(_))
    ));
    store.stop();
}

/// Proves: a version outside what the code numbers (over 2^32) is an error naming the table, not a
/// wrapped number.
///
/// Catches: a hand-edited row that reads as another version and opens under the wrong associated
/// data, or not at all, with no word why.
#[test]
fn a_version_out_of_range_is_an_error() {
    let db = TestDatabase::new();
    let (store, _) = db.open("range").expect("the store opens");
    db.run(
        "INSERT INTO mdm_secrets (kind, version, sealed, fingerprint, created_unix) \
         VALUES ('tls_key', 9999999999, decode(repeat('00', 40), 'hex'), repeat('a', 64), 1)",
    )
    .expect("a row");
    assert_eq!(
        PgSetup::over(&store).secrets().expect_err("out of range"),
        SetupError::Store("mdm_secrets holds a version out of range: 9999999999".to_owned())
    );
    store.stop();
}

/// Proves: `0012` does not stop the release before it (migrations `0001` to `0011`, whose start
/// never reads the setup tables) on a database this release migrated and holding a sealed secret:
/// its runner takes the database and runs nothing, and the state reads as this release loads it.
/// And the other way: this release starts on a database the release before migrated, runs `0012`
/// alone, and holds no secret and no setting.
///
/// Catches: a setup table that makes the previous coordinator refuse to start (the back-out of a
/// release would need the database restored), or a migration this release does not run on a
/// database of the release before.
#[test]
fn the_release_before_starts_on_a_database_with_0012_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    PgSetup::over(&store)
        .put_secret(&row("tls_key", 1))
        .expect("a secret");
    store.stop();
    let (taken, before_loaded) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..11]).expect("the release before's set");
        let taken = migrate::apply(session, &before);
        (
            taken,
            load_all(session, SPENT_NOW).expect("the state reads"),
        )
    });
    assert!(
        matches!(&taken, Ok(ran) if ran.is_empty()),
        "0012 says older releases may run: {taken:?}"
    );
    assert_eq!(before_loaded, loaded);
    assert_eq!(db.rows("SELECT kind FROM mdm_secrets").len(), 1);

    let older = TestDatabase::new();
    let ran = with_session(&older, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..11]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, (1..=11).collect::<Vec<_>>());
    let (store, _) = older.open("this release").expect("this release starts");
    assert_eq!(PgSetup::over(&store).secrets(), Ok(Vec::new()));
    assert_eq!(PgSetup::over(&store).settings(), Ok(Default::default()));
    store.stop();
    assert_eq!(
        older.rows("SELECT version FROM schema_version WHERE version >= 12 ORDER BY version"),
        vec![
            vec![Some("12".to_owned())],
            vec![Some("13".to_owned())],
            vec![Some("14".to_owned())],
            vec![Some("15".to_owned())],
            vec![Some("16".to_owned())],
            vec![Some("17".to_owned())]
        ]
    );
}
