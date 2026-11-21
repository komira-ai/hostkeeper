-- older-releases: may-run
-- Two columns of a host's turn of a maintenance run (docs/design/maintenance-updates.md, sections
-- 5.4 and 5.7): the act an operator queued for a halted host (`retry` or `put_back`, NULL for none),
-- which the coordinator sends to its daemon at each heartbeat until the daemon reports the turn at
-- another step, and the last try of the order of its turn the coordinator made, which each queued
-- act raises by one so that it reaches the daemon as a new try. NULL and 0 for a row written before.
--
-- A release before reads and writes the rows of maintenance_hosts by their other columns: it
-- reads these as it finds none, and when it rewrites a host's row it leaves them as they are (NULL
-- and 0 in a row it adds). It never queues an act, so a halted host stays halted until it is
-- cancelled; an act queued by this release is kept in the row for the release that reads it again.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.
ALTER TABLE maintenance_hosts
  ADD COLUMN queued text NULL CHECK (queued IN ('retry', 'put_back'));
ALTER TABLE maintenance_hosts
  ADD COLUMN order_try bigint NOT NULL DEFAULT 0 CHECK (order_try >= 0);
