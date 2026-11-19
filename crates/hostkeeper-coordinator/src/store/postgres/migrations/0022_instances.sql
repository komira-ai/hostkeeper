-- older-releases: may-run
-- The coordinator instances that serve on this database (docs/design/coordinator-multi-instance.md,
-- section 4, "Instances"): one row for each, made at its start with an id of its own and its
-- `seen_unix` moved on every `--instance-beat-secs` while it runs. It is for an operator to see
-- which instances run and since when; no decision reads it. A new table only: an older release
-- never reads it.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.
CREATE TABLE instances (
  id           uuid PRIMARY KEY,
  started_unix bigint NOT NULL,
  seen_unix    bigint NOT NULL,
  release      text NOT NULL,
  listen       text NOT NULL);
