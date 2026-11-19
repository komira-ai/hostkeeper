//! The row of this instance in `instances` (migration `0022`;
//! `docs/design/coordinator-multi-instance.md`, section 4, "Instances"): made at the start with a
//! random id, in the transaction that raises the epoch, and upserted every `--instance-beat-secs`
//! from then on, its `seen_unix` the database's clock. It tells an operator which instances serve
//! on the database and since when; no decision reads it. A row of an instance that stopped stays
//! as it was last seen.

use std::sync::Weak;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use super::session::Session;
use super::writer::WriteFail;
use super::{Inner, Postgres};

/// What an instance says of itself in its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Instance {
    /// Its id, random at its start.
    pub(crate) id: String,
    /// When it started, by the database's clock.
    pub(crate) started_unix: i64,
    /// The release it runs.
    pub(crate) release: String,
    /// The addresses it listens on.
    pub(crate) listen: String,
}

/// Makes the row of a new instance running `release` and listening on `listen`, in the
/// transaction open on `session`: its id is the database's `gen_random_uuid()`, and it started and
/// was seen at the transaction's `now()`.
///
/// # Errors
/// The client's error.
pub(crate) fn start(
    session: &mut Session,
    release: &str,
    listen: &str,
) -> Result<Instance, postgres::Error> {
    let row = session.query_one(
        "INSERT INTO instances (id, started_unix, seen_unix, release, listen) \
         VALUES (gen_random_uuid(), extract(epoch FROM now())::bigint, \
                 extract(epoch FROM now())::bigint, $1, $2) \
         RETURNING id::text, started_unix",
        &[&release, &listen],
    )?;
    Ok(Instance {
        id: row.get(0),
        started_unix: row.get(1),
        release: release.to_owned(),
        listen: listen.to_owned(),
    })
}

impl Postgres {
    /// This instance's row.
    #[cfg(test)]
    pub(crate) fn instance(&self) -> &Instance {
        &self.inner.instance
    }

    /// Upserts this instance's row: `seen_unix` moved to the database's `now()`, and the row made
    /// again as it was at the start when it is gone.
    ///
    /// # Errors
    /// [`WriteFail`]: it is not written, or its outcome is not known.
    pub(crate) fn beat_instance(&self) -> Result<(), WriteFail> {
        let Instance {
            id,
            started_unix,
            release,
            listen,
        } = self.inner.instance.clone();
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO instances (id, started_unix, seen_unix, release, listen) \
                 VALUES ($1::text::uuid, $2, extract(epoch FROM now())::bigint, $3, $4) \
                 ON CONFLICT (id) DO UPDATE SET seen_unix = EXCLUDED.seen_unix",
                &[&id, &started_unix, &release, &listen],
            )
            .map(drop)
        })
    }
}

/// One beat of the store `inner` while it is there: false once it is gone. A beat that is not
/// written (the store lost) is the store's alert, not this one's: the next beat writes it.
pub(super) fn beat_weak(inner: &Weak<Inner>) -> bool {
    let Some(inner) = inner.upgrade() else {
        return false;
    };
    if let Err(fail) = (Postgres { inner }).beat_instance() {
        tracing::debug!(why = fail.why(), "the instance's row was not upserted");
    }
    true
}

/// Calls `beat` every `every`, until `stop` is told to stop or is dropped, or `beat` answers
/// false.
pub(super) fn beat_every(stop: &Receiver<()>, every: Duration, mut beat: impl FnMut() -> bool) {
    while stop.recv_timeout(every) == Err(RecvTimeoutError::Timeout) && beat() {}
}
