-- older-releases: may-run
-- The fleet in Postgres (docs/design/coordinator-multi-instance.md, section 5.1): what each host
-- last said, how many healthy beats in a row it has sent at one version of each service, the
-- orders that hold an update slot, and the quiet time a gap in the beats sets. Every coordinator
-- on the database writes a host's beat to the same rows, so none needs to have heard the beat
-- before. A heartbeat writes them in one statement; no decision reads them yet.
--
-- `fleet_hosts`: the last beat of each host (the heartbeat as the coordinator read it), when it
-- came by the database's clock (`seen_unix`), the host's own clock in it (`sent_unix`, 0 for an
-- agent that sends none) and the instance that heard it. A beat older by `sent_unix` than the
-- stored one is dropped unless the stored row is older than `--beat-reorder-secs`. Logged: its
-- write cost was measured and does not matter. Half-full pages and no index on a column a beat
-- updates, so the update stays on its page.
--
-- `fleet_bakes`: the healthy beats in a row of a host at one version of a service. Unlogged: a
-- crash that empties it only starts the bakes again.
--
-- `fleet_orders`: an order a host has not yet reported done, which holds an update slot. A host
-- takes one update at a time, which its primary key says.
--
-- `cluster_quiet`: one row. When a beat finds the newest row of `fleet_hosts` older than a host
-- may be silent, or finds none, the whole fleet is stale (a database restart, an outage, a first
-- start), and `until_unix` is raised to the quiet time past the beat's moment, never lowered.
--
-- A release before this one never reads these tables, so it runs on a database that has them.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.
CREATE TABLE fleet_hosts (
  host_id   text PRIMARY KEY,
  role      text NOT NULL,
  beat      bytea NOT NULL,
  seen_unix bigint NOT NULL,
  sent_unix bigint NOT NULL,
  instance  text NOT NULL) WITH (fillfactor = 50);

CREATE UNLOGGED TABLE fleet_bakes (
  host_id text NOT NULL,
  service text NOT NULL,
  target  text NOT NULL,
  beats   integer NOT NULL CHECK (beats > 0),
  at_unix bigint NOT NULL,
  PRIMARY KEY (host_id, service));

CREATE TABLE fleet_orders (
  host_id      text PRIMARY KEY,
  service      text NOT NULL,
  target       text NOT NULL,
  uninstall_id text,
  at_unix      bigint NOT NULL,
  try          bigint NOT NULL);

CREATE TABLE cluster_quiet (
  id         smallint PRIMARY KEY CHECK (id = 1),
  until_unix bigint NOT NULL);
INSERT INTO cluster_quiet (id, until_unix) VALUES (1, 0);
