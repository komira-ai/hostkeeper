//! The initial-admin file's effects as conditional writes on the user's row of `users`
//! (`docs/design/coordinator-multi-instance.md`, section 9, "Initial-admin file"): every
//! coordinator on the database looks at the same file, and the one whose write changed the row is
//! the one that audits, so a file is adopted once and its expiry voids once, however many look.
//! The row holds the user's record as `jsonb` (migration `0010`); the file's `id` is its
//! `initial_id`.

use super::Postgres;
use super::writer::{Tx, WriteFail};

fn written<R>(done: Result<R, WriteFail>) -> Result<R, String> {
    done.map_err(|fail| fail.why().to_owned())
}

/// Runs the write `body` of the users' rows under the row of the part `accounts` in
/// `state_versions` (`FOR UPDATE`, the one lock this transaction takes), and moves that part's
/// version on when `body` changed a row, so that every coordinator reads its copy of the accounts
/// again (section 6).
fn on_accounts(
    tx: &mut Tx<'_>,
    body: impl FnOnce(&mut Tx<'_>) -> Result<u64, postgres::Error>,
) -> Result<bool, postgres::Error> {
    tx.query_one(
        "SELECT version FROM state_versions WHERE part = 'accounts' FOR UPDATE",
        &[],
    )?;
    let changed = body(tx)? == 1;
    if changed {
        tx.execute(
            "UPDATE state_versions SET version = version + 1 WHERE part = 'accounts'",
            &[],
        )?;
    }
    Ok(changed)
}

/// The rows of the users a coordinator over a database adopts the initial-admin file into.
#[derive(Clone)]
pub(crate) struct PgAdmin {
    store: Postgres,
}

impl std::fmt::Debug for PgAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgAdmin").finish_non_exhaustive()
    }
}

impl PgAdmin {
    /// The users of `store`.
    pub(crate) fn new(store: Postgres) -> PgAdmin {
        PgAdmin { store }
    }

    /// Whether the row of `user` holds the file `id` already.
    ///
    /// # Errors
    /// Why it could not be read.
    pub(crate) fn adopted(&self, user: &str, id: &str) -> Result<bool, String> {
        let (user, id) = (user.to_owned(), id.to_owned());
        self.store.read(move |session| {
            Ok(session
                .query_opt(
                    "SELECT 1 FROM users WHERE name = $1 AND record->>'initial_id' = $2",
                    &[&user, &id],
                )?
                .is_some())
        })
    }

    /// Writes `record` as the row of `user` unless the row holds the file `id` already: whether
    /// this write made or changed the row (the adoption is this coordinator's to audit).
    ///
    /// # Errors
    /// Why it could not be written.
    pub(crate) fn adopt(
        &self,
        user: &str,
        record: serde_json::Value,
        id: &str,
    ) -> Result<bool, String> {
        let (user, id) = (user.to_owned(), id.to_owned());
        written(self.store.write(move |tx| {
            on_accounts(tx, |tx| {
                tx.execute(
                    "INSERT INTO users (name, record) VALUES ($1, $2) ON CONFLICT (name) DO UPDATE \
                     SET record = EXCLUDED.record \
                     WHERE users.record->>'initial_id' IS DISTINCT FROM $3",
                    &[&user, &record, &id],
                )
            })
        }))
    }

    /// Voids the initial credential of `user` if its row holds the file `id`, the password was not
    /// changed (an initial deadline is set) and it is not void yet: whether this write voided it
    /// (the expiry is this coordinator's to audit).
    ///
    /// # Errors
    /// Why it could not be written.
    pub(crate) fn void(&self, user: &str, id: &str) -> Result<bool, String> {
        let (user, id) = (user.to_owned(), id.to_owned());
        written(self.store.write(move |tx| {
            on_accounts(tx, |tx| {
                tx.execute(
                    "UPDATE users SET record = jsonb_set(record, '{initial_void}', 'true') \
                     WHERE name = $1 AND record->>'initial_id' = $2 \
                     AND record ? 'initial_expires_at' \
                     AND NOT coalesce((record->>'initial_void')::boolean, false)",
                    &[&user, &id],
                )
            })
        }))
    }
}
