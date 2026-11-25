-- older-releases: may-run
-- Edits as row transactions (docs/design/coordinator-multi-instance.md, sections 5.2 and 6), so
-- that coordinators on one database agree on every part of the state they cache.
--
-- `rollout_gate`: one row, never updated. A transaction that edits what a rollout decision reads
-- (the desired state, the operations state, the layout, the verdicts) takes it FOR UPDATE first;
-- a heartbeat asks for it with SKIP LOCKED and never waits. The row always exists, so SKIP LOCKED
-- finding no row means the gate is held.
--
-- `state_versions`: one row for each part of the state, the unit of lock, cache and version. An
-- edit takes the rows of the parts it writes FOR UPDATE, after the gate and in the order of the
-- rows below, and moves on the version of each part whose rows it wrote, in the same
-- transaction. A coordinator whose copy of a part carries an older version reads the part again.
--
-- `audit_counter`: the number of the next line of `audit`, taken by a transaction that writes
-- audit lines as the last of its locks and moved on before it commits, so the numbers follow the
-- commits with no gap. A start sets it past the newest line, which a release before this one
-- numbered from `max(seq)` without it.
--
-- A release before this one never reads these tables, so it runs on a database that has them.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.
CREATE TABLE rollout_gate (
  id smallint PRIMARY KEY CHECK (id = 1));
INSERT INTO rollout_gate (id) VALUES (1) ON CONFLICT DO NOTHING;

CREATE TABLE state_versions (
  part    text PRIMARY KEY,
  version bigint NOT NULL DEFAULT 0 CHECK (version >= 0));
INSERT INTO state_versions (part) VALUES
  ('desired'), ('ops'), ('accounts'), ('layout'), ('verdicts'), ('enrollment')
  ON CONFLICT DO NOTHING;

CREATE TABLE audit_counter (
  id   smallint PRIMARY KEY CHECK (id = 1),
  next bigint NOT NULL CHECK (next >= 1));
INSERT INTO audit_counter (id, next) SELECT 1, coalesce(max(seq), 0) + 1 FROM audit
  ON CONFLICT DO NOTHING;
