//! The Xcode releases the feed last named as rows of `xcode_releases` (migration `0028`;
//! `docs/design/maintenance-updates.md`, section 1.1, job `xcode-feed`): the job replaces the
//! table whole, in one transaction, after each read of the feed that succeeds, and a coordinator
//! reads it to tell whether a release is waiting. A release is its version, its build and its
//! channel, with the macOS it requires, as text.

use super::Postgres;
use crate::xcode_feed::{KeptRelease, KnownReleases, XcodeRelease};

/// The Xcode release rows of a coordinator over a database.
#[derive(Clone)]
pub(crate) struct PgReleases {
    store: Postgres,
}

impl std::fmt::Debug for PgReleases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgReleases").finish_non_exhaustive()
    }
}

impl PgReleases {
    /// The Xcode release rows of `store`.
    pub(crate) fn new(store: Postgres) -> PgReleases {
        PgReleases { store }
    }
}

impl KnownReleases for PgReleases {
    /// Deletes every row and inserts `releases`, in one transaction: a failure leaves the rows
    /// as they were. A release listed twice is one row.
    fn keep(&self, releases: &[XcodeRelease]) -> Result<(), String> {
        let kept: Vec<KeptRelease> = releases.iter().map(KeptRelease::from).collect();
        let versions: Vec<String> = kept.iter().map(|r| r.version.clone()).collect();
        let builds: Vec<String> = kept.iter().map(|r| r.build.clone()).collect();
        let channels: Vec<String> = kept.iter().map(|r| r.channel.clone()).collect();
        let macs: Vec<String> = kept.iter().map(|r| r.requires_macos.clone()).collect();
        self.store
            .write(move |tx| {
                tx.execute("DELETE FROM xcode_releases", &[])?;
                tx.execute(
                    "INSERT INTO xcode_releases (version, build, channel, requires_macos) \
                     SELECT version, build, channel, requires_macos \
                     FROM unnest($1::text[], $2::text[], $3::text[], $4::text[]) \
                       AS u(version, build, channel, requires_macos) \
                     ON CONFLICT DO NOTHING",
                    &[&versions, &builds, &channels, &macs],
                )?;
                Ok(())
            })
            .map_err(|fail| fail.why().to_owned())
    }

    /// The rows, read on the read-only session, oldest first (by version number, build and
    /// channel: the text order of the version column is not the number order).
    fn known(&self) -> Result<Vec<XcodeRelease>, String> {
        let rows = self.store.read(|session| {
            Ok(session
                .query(
                    "SELECT version, build, channel, requires_macos FROM xcode_releases",
                    &[],
                )?
                .iter()
                .map(|row| KeptRelease {
                    version: row.get(0),
                    build: row.get(1),
                    channel: row.get(2),
                    requires_macos: row.get(3),
                })
                .collect::<Vec<KeptRelease>>())
        })?;
        let mut releases = rows
            .iter()
            .map(KeptRelease::release)
            .collect::<Result<Vec<XcodeRelease>, String>>()?;
        releases.sort();
        Ok(releases)
    }
}
