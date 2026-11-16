-- older-releases: may-run
-- The coordinator's browser sessions, the login limiter's counts and the due times of its
-- periodic work, in the database, so that any number of coordinators answer the same cookie,
-- count the same failures and run each periodic job once per period
-- (docs/design/coordinator-multi-instance.md, sections 8 and 9). A release before this one keeps
-- these in its memory and never reads the tables.

-- One row per signed-in browser, by the SHA-256 of the cookie's id (never the id itself).
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

-- The next time of each periodic job and who ran it last: a claim moves `next_run_unix` on, so
-- of the coordinators that find a job due one gets it.
CREATE TABLE periodic_jobs (
  name          text PRIMARY KEY,
  period_secs   bigint NOT NULL,
  next_run_unix bigint NOT NULL,
  last_run_unix bigint,
  last_instance text);
