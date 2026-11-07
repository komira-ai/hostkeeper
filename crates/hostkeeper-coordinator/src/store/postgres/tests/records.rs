//! The enrollment store's records on Postgres, under the `Records` contract.

use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::enroll::records::Records;
use hostkeeper_enrollment::enroll::records::conformance::{self, Subject};

use super::TestDatabase;
use crate::store::postgres::{PgRecords, Postgres};

/// The records' suite's subject on a database of its own: each open makes the records again over
/// the one store, its copy of the hosts and the deny rows read anew from the tables, as a restart
/// does. (A store opened again would be refused the lock while the clause still holds records of
/// the first.)
struct OnPostgres {
    database: TestDatabase,
    live: Mutex<Option<Postgres>>,
}

impl Subject for OnPostgres {
    fn open(&self) -> Arc<dyn Records> {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        let store = live.get_or_insert_with(|| {
            self.database
                .open("records")
                .expect("the store opens")
                .0
        });
        Arc::new(PgRecords::over(store).expect("the records open"))
    }

    fn fail_next_commit(&self) {
        // A sequence moves on whatever the transaction does, so the trigger refuses one
        // transaction, the next, and no other.
        self.database
            .run(
                "DROP TRIGGER IF EXISTS test_once ON lease; DROP SEQUENCE IF EXISTS test_once; \
                 CREATE SEQUENCE test_once; \
                 CREATE OR REPLACE FUNCTION test_once() RETURNS trigger LANGUAGE plpgsql AS $$ \
                 BEGIN IF nextval('test_once') = 1 THEN \
                 RAISE EXCEPTION 'the test refuses this commit'; END IF; RETURN NEW; END $$; \
                 CREATE TRIGGER test_once BEFORE UPDATE ON lease FOR EACH ROW \
                 EXECUTE FUNCTION test_once()",
            )
            .expect("the refusal is set");
    }
}

/// Proves: the enrollment store's records on Postgres keep the whole `Records` contract: empty
/// records, a round trip of every kind, overwrites, the order of the lists, the serial, the
/// heartbeat's marks, a failed commit, an unkeyed record and the lock.
///
/// Catches: a column that drops or garbles a field of a message, a commit that writes half, a
/// list out of order, or a serial taken twice.
#[test]
fn the_records_conformance_suite_passes_on_postgres() {
    let failures = conformance::run(&|| {
        Box::new(OnPostgres {
            database: TestDatabase::new(),
            live: Mutex::new(None),
        })
    });
    let failures: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert_eq!(failures, Vec::<String>::new());
}
