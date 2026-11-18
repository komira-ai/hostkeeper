-- older-releases: may-run
-- The SSH tunnels through the coordinator (docs/design/ssh-through-coordinator.md, sections 7
-- and 8): where each host was last heard from and the host keys it reports, the host keys an
-- operator pinned, and the tunnels open on every coordinator instance. Only new tables: an older
-- release never reads any of them.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

-- The address a host's last heartbeat came from (no port), when, and the host keys it reported.
CREATE TABLE ssh_host (
  host_id   text PRIMARY KEY,
  address   text NOT NULL,
  seen_unix bigint NOT NULL,
  reported  text[] NOT NULL DEFAULT '{}');

-- The host keys an operator pinned for a host.
CREATE TABLE ssh_host_key_pin (
  host_id      text PRIMARY KEY,
  fingerprints text[] NOT NULL,
  by_name      text NOT NULL,
  at_unix      bigint NOT NULL);

-- One row for each tunnel that is open. A row is deleted when its tunnel closes (the audit keeps
-- the history), so the unique index is one open tunnel per operator per host across instances.
CREATE TABLE ssh_tunnel (
  id             bigserial PRIMARY KEY,
  operator       text NOT NULL,
  host_id        text NOT NULL,
  cert_serial    bigint NOT NULL,
  instance       text NOT NULL,
  address        text NOT NULL,
  opened_unix    bigint NOT NULL,
  last_seen_unix bigint NOT NULL);
CREATE UNIQUE INDEX ssh_tunnel_one ON ssh_tunnel (operator, host_id);
CREATE INDEX ssh_tunnel_instance ON ssh_tunnel (instance);
