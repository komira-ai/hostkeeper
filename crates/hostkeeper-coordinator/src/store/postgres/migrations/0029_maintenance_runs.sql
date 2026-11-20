-- older-releases: may-run
-- The maintenance runs an operator scheduled (docs/design/maintenance-updates.md, sections 5.1 and
-- 5.7), part of the operations state: one row of maintenance_runs for each run, and one row of
-- maintenance_hosts for each host's turn of a run, by the run's id and the host's node id. A kind,
-- a step and a reason a host is left out are written by their names; a value a run leaves out
-- (no host group, no batch, no drain deadline, no cancel) is NULL. `position` keeps the order the
-- runs, and each run's hosts, are held in.
--
-- No release before reads or writes these tables, so it runs on a database that has them; it
-- serves no maintenance run. A shipped migration is never edited: its SHA-256 is pinned by a
-- test.
CREATE TABLE maintenance_runs (
  run_id              text PRIMARY KEY,
  kinds               text[] NOT NULL,
  host_ids            text[] NOT NULL,
  host_group          text,
  window_start_unix   bigint NOT NULL CHECK (window_start_unix >= 0),
  window_end_unix     bigint NOT NULL CHECK (window_end_unix >= 0),
  batch               bigint CHECK (batch >= 1),
  drain_deadline_secs bigint CHECK (drain_deadline_secs >= 0),
  take_last_host      boolean NOT NULL,
  reason              text NOT NULL,
  by_name             text NOT NULL,
  scheduled_unix      bigint NOT NULL CHECK (scheduled_unix >= 0),
  cancelled_unix      bigint CHECK (cancelled_unix >= 0),
  position            bigint NOT NULL);

CREATE TABLE maintenance_hosts (
  run_id          text,
  host_id         text,
  host_group      text,
  step            text NOT NULL CHECK (step IN ('waiting', 'staging', 'draining', 'applying',
                    'rebooting', 'checking', 'putting_back', 'done', 'deferred', 'halted',
                    'not_back', 'not_reached', 'skipped')),
  since_unix      bigint NOT NULL CHECK (since_unix >= 0),
  kinds           text[] NOT NULL,
  applied         text[] NOT NULL,
  skipped_because text,
  reason          text NOT NULL,
  position        bigint NOT NULL,
  PRIMARY KEY (run_id, host_id));
