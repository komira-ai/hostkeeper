-- older-releases: may-run
-- One column of a maintenance run: the SHA-256 of the Xcode archive a run of `probe` checks
-- (`Maintenance.Schedule`'s `xcode_sha256`: 64 lower-case hex characters, an archive of the Xcode
-- store; docs/operations/maintenance.md, "Probe: check an Xcode archive"), which the order of
-- each host's probe turn names so that its daemon checks the archive instead of making its reads.
-- NULL for a run that checks none, and for a row written before.
--
-- A release before reads and writes the rows of maintenance_runs by their other columns: it reads
-- this one as it finds none, so each probe turn of such a run it sends is a plain probe (the
-- daemon's reads, no archive checked), and when it rewrites a run's row it leaves the column as it
-- is (NULL in a row it adds). It never schedules a run that names an archive; one this release
-- scheduled is kept in the row for the release that reads it again.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.
ALTER TABLE maintenance_runs
  ADD COLUMN xcode_sha256 text NULL CHECK (xcode_sha256 ~ '^[0-9a-f]{64}$');
