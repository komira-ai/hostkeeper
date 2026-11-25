-- older-releases: may-run
-- Two counts of a host's turn of a maintenance run (docs/design/maintenance-updates.md, sections
-- 5.3, 5.6 and 5.7): how many times it was offered its turn again after a deferral, which names the
-- order of its turn, and, once done, the healthy heartbeats in a row it has sent toward its group's
-- `bake_beats`, until which it holds its batch place. Both are 0 for a row written before.
--
-- A release before reads and writes the rows of maintenance_hosts by their other columns and
-- leaves these as they are (0 in a row it adds): it never offers a deferred host again, and it
-- frees a done host's batch place without waiting for its bake.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.
ALTER TABLE maintenance_hosts
  ADD COLUMN offered_again bigint NOT NULL DEFAULT 0 CHECK (offered_again >= 0);
ALTER TABLE maintenance_hosts
  ADD COLUMN baked_beats bigint NOT NULL DEFAULT 0 CHECK (baked_beats >= 0);
