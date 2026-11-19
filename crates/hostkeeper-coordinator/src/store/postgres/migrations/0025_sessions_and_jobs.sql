-- older-releases: may-run
-- The coordinator's browser sessions and failed-login counts, the due times of its periodic work
-- and the alerts it has logged, in the database, so that any number of coordinators answer the
-- same cookie, count the same failures, run each periodic job once per period and log each alert
-- once (docs/design/coordinator-multi-instance.md, sections 8 and 9). New tables only: a release
-- before this one keeps these in its memory and never reads them.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

-- One row per signed-in browser, keyed by the SHA-256 of the cookie's id (never the id itself).
CREATE TABLE sessions (
  id_sha256         bytea PRIMARY KEY,
  username          text NOT NULL,
  csrf              text NOT NULL,
  must_change       boolean NOT NULL,
  must_change_until bigint,
  created_unix      bigint NOT NULL,
  last_used_unix    bigint NOT NULL,
  serial            bigint GENERATED ALWAYS AS IDENTITY);
CREATE INDEX sessions_username ON sessions (username);

-- The consecutive failed logins of a user name and, once they reach the limit, until when the
-- name is locked.
CREATE TABLE login_failures (
  name              text PRIMARY KEY,
  failures          bigint NOT NULL,
  last_failure_unix bigint NOT NULL,
  locked_until_unix bigint);

-- Rows never updated, only locked FOR UPDATE: 'sessions' by the making of a session (the cap
-- counts every row), 'login_failures' by the counting of a failed login (the cap on names).
CREATE TABLE login_gate (
  name text PRIMARY KEY);
INSERT INTO login_gate (name) VALUES ('sessions'), ('login_failures') ON CONFLICT DO NOTHING;

-- The next due time of each periodic job and who claimed it last: a claim moves
-- `next_run_unix` on by the job's period, so of the coordinators that find it due one gets it.
CREATE TABLE periodic_jobs (
  name          text PRIMARY KEY,
  next_run_unix bigint NOT NULL,
  last_run_unix bigint,
  last_instance text);

-- The alerts observed now, each with when it was first observed: the coordinator whose insert
-- made the row logs the alert, once; a cleared alert's row is deleted.
CREATE TABLE alerts (
  kind       text NOT NULL,
  service    text NOT NULL,
  host       text NOT NULL,
  version    text NOT NULL,
  request    text NOT NULL,
  other_host text NOT NULL,
  first_unix bigint NOT NULL,
  PRIMARY KEY (kind, service, host, version, request, other_host));
