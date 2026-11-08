-- older-releases: may-run
-- The releases a version was set with by SetDesired with targets (docs/design/packages.md,
-- section 3.6; docs/design/coordinator-storage.md, section 3.2): one row per service and
-- version, written once. An older release may run on it: it never reads the table, and reads a
-- version set with targets as one set with neither a package digest nor an index.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE releases (
  service text,
  version text,
  -- Each platform's TargetList, as proto3 canonical JSON, keyed by the platform's spelling.
  targets jsonb NOT NULL,
  -- The SHA-256 of the release index set with the targets; NULL when the set carried none.
  release_index_digest text,
  PRIMARY KEY (service, version));

ALTER TABLE releases ADD CONSTRAINT releases_index_digest
  CHECK (release_index_digest ~ '^[0-9a-f]{64}$');
