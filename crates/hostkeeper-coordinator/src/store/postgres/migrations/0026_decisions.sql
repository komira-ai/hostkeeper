-- older-releases: may-run
-- What the decisions of many coordinators need from the database (docs/design/coordinator-multi-
-- instance.md, sections 5.1 and 5.2): one number that orders the services waiting for their turn
-- to roll, the same on every coordinator, and the highest try each host was given an order at.
-- A release before never reads any of it.
--
-- `desired_seq`: the sequence the numbers are taken from. A number is taken in the transaction
-- that sets a desired version, under the rollout gate, so the numbers follow the commits.
--
-- `desired.set_seq`, `desired.set_for` and the same two columns of `group_desired`: the number
-- taken when the row's desired version was last set, and the version it was taken for. A row
-- whose `set_for` is not its `version` was changed by a release before, which knows neither
-- column, and a row with no number was written by one; the next start gives each a new number,
-- in the queue's order of the release before. Both columns are null until then. They are written
-- by the statements that stamp them and by no other, so a change of a row's other columns keeps
-- them.
--
-- `fleet_tries`: the highest try of an order a coordinator gave a host for a service and no
-- longer counts as outstanding, so that the next order of any coordinator numbers past it.
-- Logged. A release before keeps its tries in memory.
--
-- A release before this one runs on a database that has them. A shipped migration is never
-- edited: its SHA-256 is pinned by a test.
CREATE SEQUENCE desired_seq;

ALTER TABLE desired ADD COLUMN set_seq bigint;
ALTER TABLE desired ADD COLUMN set_for text;
ALTER TABLE group_desired ADD COLUMN set_seq bigint;
ALTER TABLE group_desired ADD COLUMN set_for text;

CREATE TABLE fleet_tries (
  host_id text NOT NULL,
  service text NOT NULL,
  try     bigint NOT NULL CHECK (try >= 0),
  PRIMARY KEY (host_id, service));
