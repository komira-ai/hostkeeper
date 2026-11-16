-- older-releases: may-run
-- The orders an operator queued for a host (docs/design/coordinator-multi-instance.md, section 5):
-- an `abort` (Hosts.Abort) and a `clear_record` (Hosts.ClearRecord), each waiting for the host's
-- next heartbeat, which any coordinator may hear. The coordinator that sends one deletes its row
-- in the transaction that decides the answer, so an order is sent once whichever coordinator took
-- the operator's call and whichever hears the beat, and a coordinator that stops forgets none.
-- A release before this one never reads the table.

-- One order of each kind per host and service: asking again while one waits queues nothing more,
-- and the first order id stands. Logged: an order the operator was told is queued is not lost
-- with the database's crash.
CREATE TABLE fleet_operator_orders (
  host_id  text NOT NULL,
  kind     text NOT NULL CHECK (kind IN ('abort', 'clear_record')),
  service  text NOT NULL,
  order_id text NOT NULL,
  PRIMARY KEY (host_id, kind, service));
