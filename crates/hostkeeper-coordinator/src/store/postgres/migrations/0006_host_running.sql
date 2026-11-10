-- older-releases: may-run
-- What a host's daemon last said it runs (docs/design/packages-native.md, section 10.6): one row
-- per approved host, written by the coordinator's heartbeat path with the host's enrollment and
-- removed whenever the host is written without one (an approval). An older release may run on
-- it: it never reads the table, and its writes of a host row (`enrollment_hosts`) do not touch
-- it, so a row it leaves behind is the last the newer release wrote and is read again, or written
-- over by the next heartbeat, when the newer release returns.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE enrollment_host_running (
  host_id text PRIMARY KEY,
  -- The record as the file store keeps it in the host file's field 10: the encoded message
  -- `hostkeeper.enrollment.store.v1.Running`.
  running bytea NOT NULL);
