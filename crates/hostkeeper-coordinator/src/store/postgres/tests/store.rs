//! The Postgres store under the [`Backend`] contract and its own promises.

use std::sync::{Arc, Mutex, PoisonError};

use super::TestDatabase;
use crate::store::conformance_tests::Subject;
use crate::store::{Postgres, Shared};

/// The conformance suite's subject on a database of its own: the live store, which a restart
/// stops so that the store started again takes the lock.
pub(crate) struct OnPostgres {
    database: TestDatabase,
    live: Mutex<Option<Postgres>>,
}

impl OnPostgres {
    pub(crate) fn new() -> OnPostgres {
        OnPostgres {
            database: TestDatabase::new(),
            live: Mutex::new(None),
        }
    }

    fn opened(&self) -> Postgres {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = live.take() {
            old.stop();
        }
        let (store, _) = self.database.open("conformance").expect("the store opens");
        *live = Some(store.clone());
        store
    }
}

impl Subject for OnPostgres {
    fn live(&self) -> Shared {
        let live = self
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Arc::new(live.unwrap_or_else(|| self.opened()))
    }

    fn restarted(&self) -> Shared {
        Arc::new(self.opened())
    }

    fn failing(&self, on: bool) {
        let sql = if on {
            "CREATE FUNCTION test_refuse() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'the test refuses this commit'; END $$; \
             CREATE TRIGGER test_refuse BEFORE UPDATE ON lease FOR EACH ROW \
             EXECUTE FUNCTION test_refuse()"
        } else {
            "DROP TRIGGER test_refuse ON lease; DROP FUNCTION test_refuse()"
        };
        self.database.run(sql).expect("the refusal is set");
    }
}
