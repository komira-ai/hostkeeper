-- older-releases: may-run
-- The fleet in Postgres (docs/design/coordinator-multi-instance.md, section 5): what each host
-- last said, how many healthy beats it has sent in a row, the orders that hold an update slot,
-- the start rule's quiet time, the gate every decision takes first and the queue that makes one
-- service roll at a time in the order it was asked for. A release before this one never reads
-- these tables, so it runs on a database that has them.

-- One row, never updated: a transaction that decides, or edits what a decision reads, takes it
-- FOR UPDATE first, so decisions and edits run one after the other whatever instance makes them.
CREATE TABLE rollout_gate (
  id smallint PRIMARY KEY CHECK (id = 1));
INSERT INTO rollout_gate (id) VALUES (1);

-- The last heartbeat of each host. Unlogged: the next beat of each host rebuilds a row within
-- one heartbeat period, and a crash that empties the table is a gap the quiet rule sees.
CREATE UNLOGGED TABLE fleet_hosts (
  host_id     text PRIMARY KEY,
  role        text NOT NULL,
  beat        bytea NOT NULL,
  seen_unix   bigint NOT NULL,
  sent_unix   bigint NOT NULL,
  up          boolean NOT NULL,
  running     bytea,
  repo_report bytea,
  instance    text NOT NULL) WITH (fillfactor = 50);

-- The healthy beats in a row of a host at one version of a service.
CREATE UNLOGGED TABLE fleet_bakes (
  host_id text NOT NULL,
  service text NOT NULL,
  target  text NOT NULL,
  beats   bigint NOT NULL,
  at_unix bigint NOT NULL,
  PRIMARY KEY (host_id, service));

-- An order a host has not yet reported done: it holds a slot of the service's role. A host takes
-- one update at a time, which the primary key says.
CREATE TABLE fleet_orders (
  host_id      text PRIMARY KEY,
  service      text NOT NULL,
  role         text NOT NULL,
  target       text NOT NULL,
  uninstall_id text,
  at_unix      bigint NOT NULL,
  try          bigint NOT NULL DEFAULT 0);
CREATE INDEX fleet_orders_service ON fleet_orders (service, role);

-- When the fleet's rows are not to be judged: the whole table is stale or empty (a database
-- restart, an outage, a first start), so a host not heard from yet is not a host that is down.
CREATE TABLE cluster_quiet (
  id         smallint PRIMARY KEY CHECK (id = 1),
  until_unix bigint NOT NULL);
INSERT INTO cluster_quiet (id, until_unix) VALUES (1, 0);

-- The order services were asked to roll in: a service takes its place when it first needs a
-- rollout and keeps it until the rollout is over.
CREATE SEQUENCE desired_seq;
CREATE TABLE rollout_queue (
  service text PRIMARY KEY,
  seq     bigint NOT NULL DEFAULT nextval('desired_seq'));
CREATE UNIQUE INDEX rollout_queue_seq ON rollout_queue (seq);
