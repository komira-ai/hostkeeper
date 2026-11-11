//! A database of a test's own filled with a whole state ([`Moved`]) in one transaction, as a
//! coordinator that served on it would have left it: what the tests of `export` start from.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use super::TestDatabase;
use crate::accounts::Accounts;
use crate::desired;
use crate::ops::OpsState;
use crate::store::Commit;
use crate::store::postgres::devices::UPSERT;
use crate::store::postgres::rows::{Stmt, Val};
use crate::store::postgres::session::Session;
use crate::store::postgres::transfer::Moved;
use crate::store::postgres::{TIMEOUTS, migrate, records, state, statements};
use crate::verdict_file;

/// The statements that write `moved` into the empty tables of a database: the desired state, the
/// operations state, the verdicts and the accounts as a commit from nothing writes them, the whole audit history
/// with its numbers, CI's spent ids, the enrollment records, the MDM gate's device records, and,
/// when `moved` says its oldest audit lines were pruned, the import record of a release before
/// this one that says so.
fn writes(moved: &Moved) -> Vec<Stmt> {
    let (desired, ops, verdicts, held) = (
        desired::Held::default(),
        OpsState::default(),
        verdict_file::Held::default(),
        Accounts::default(),
    );
    let mut out = statements(&Commit {
        desired: Some((&desired, &moved.loaded.desired)),
        ops: Some((&ops, &moved.loaded.ops)),
        verdicts: Some((&verdicts, &moved.loaded.verdicts)),
        accounts: Some((&held, &moved.loaded.accounts)),
        audit: &moved.audit,
    })
    .expect("the state makes rows");
    for (jti, until) in &moved.loaded.spent {
        out.push(Stmt {
            sql: "INSERT INTO spent_tokens (jti, until_unix) VALUES ($1, $2)".to_owned(),
            params: vec![
                Val::Text(jti.clone()),
                Val::Int(state::int(*until).expect("a time")),
            ],
        });
    }
    out.extend(records::writes(&moved.records).expect("the records make rows"));
    out.extend(moved.devices.iter().map(|record| Stmt {
        sql: UPSERT.to_owned(),
        params: vec![
            Val::Text(record.enrollment_id.as_str().to_owned()),
            Val::Bytes(hostkeeper_mdm_gate::encode_record(record)),
        ],
    }));
    if let Some(at) = moved.pruned_before {
        out.push(Stmt {
            sql: "INSERT INTO imports (id, at_unix, manifest, audit_pruned_before) \
                  VALUES (1, 0, '{}', $1)"
                .to_owned(),
            params: vec![Val::Int(state::int(at).expect("a time"))],
        });
    }
    out
}

/// Brings `db` to this build's schema and writes `moved` into it, in one transaction.
pub(crate) fn seed(db: &TestDatabase, moved: &Moved) {
    let config = db.dsn().config(TIMEOUTS);
    let statements = writes(moved);
    super::off_runtime(move || {
        let mut session =
            Session::connect(&config, Arc::new(AtomicU64::new(0)), false).expect("a session");
        let set = migrate::checked(&migrate::SHIPPED).expect("the shipped migrations");
        migrate::apply(&mut session, &set).expect("the migrations run");
        session.batch("BEGIN").expect("a transaction");
        for statement in &statements {
            session
                .execute(&statement.sql, &statement.refs())
                .expect("a row of the state");
        }
        session.batch("COMMIT").expect("committed");
    });
}
