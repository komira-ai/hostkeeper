-- older-releases: refuse
-- Host groups (docs/design/hostgroups.md, section 8): the groups, their packages and their
-- desired versions; per (group, package) its minimum version, its marks and its plan position;
-- and the group of a host, an approval, an inventory record and an audit line. An older release
-- must not run on it: it would place services by roles over a fleet whose placement is its
-- groups, and its writes of a host or a request name every column of their tables
-- (`ROW(EXCLUDED.*)`), which this migration adds to.
-- The tables of placement by roles and of per-service state are left as they are.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

-- One row per group: its body but the packages, and the last change's author and time.
CREATE TABLE host_groups (
  name text PRIMARY KEY,
  -- Raised by every change of the group.
  generation bigint NOT NULL,
  -- The rollout policy, as the state file holds it: each key at its default left out.
  policy jsonb NOT NULL,
  by_name text NOT NULL,
  at_unix bigint NOT NULL);

ALTER TABLE host_groups ADD CONSTRAINT host_groups_name
  CHECK (name ~ '^[a-z]([a-z0-9-]{0,30}[a-z0-9])?$' AND name NOT LIKE '%--%');
ALTER TABLE host_groups ADD CONSTRAINT host_groups_generation CHECK (generation > 0);

-- The packages of a group, `position` their order in its list from 0.
CREATE TABLE host_group_packages (
  host_group text REFERENCES host_groups ON DELETE CASCADE,
  service text,
  position bigint NOT NULL,
  PRIMARY KEY (host_group, service));

-- A group's desired version of a package, and its write record.
CREATE TABLE group_desired (
  host_group text REFERENCES host_groups ON DELETE CASCADE,
  service text,
  version text NOT NULL,
  generation bigint NOT NULL,
  write_id text,
  replaced_version text,
  PRIMARY KEY (host_group, service));

ALTER TABLE group_desired ADD CONSTRAINT group_desired_generation CHECK (generation > 0);
ALTER TABLE group_desired ADD CONSTRAINT group_desired_write_id
  CHECK (write_id ~ '^[!-~]{1,128}$');

-- A group's minimum version of a package, written by Services.SetMinVersion alone.
CREATE TABLE group_min_versions (
  host_group text,
  service text,
  version text NOT NULL,
  PRIMARY KEY (host_group, service));

-- The marks of a package in a group: its last good version, its halt with the reports an
-- operator cleared and those seen while it stood, and the force that stands, each with its
-- generation. A pair whose every mark is at its default has no row.
CREATE TABLE group_services (
  host_group text,
  service text,
  last_good text,
  halt_generation bigint NOT NULL DEFAULT 0,
  force_generation bigint NOT NULL DEFAULT 0,
  force jsonb,
  halt jsonb,
  halt_acked jsonb,
  halt_seen jsonb,
  PRIMARY KEY (host_group, service));

-- Where a group's update plan of a package stands.
CREATE TABLE group_rollouts (
  host_group text,
  service text,
  version text,
  entered bigint NOT NULL DEFAULT 0,
  paused_by text,
  paused_reason text,
  PRIMARY KEY (host_group, service),
  CHECK ((paused_by IS NULL) = (paused_reason IS NULL)));

CREATE TABLE group_rollout_done (
  host_group text,
  service text,
  node_id text,
  PRIMARY KEY (host_group, service, node_id),
  FOREIGN KEY (host_group, service) REFERENCES group_rollouts ON DELETE CASCADE);

-- The group of an approved host; while a move's sequence runs, the group it moves from; and the
-- generation of its facts. NULL on a host written before.
ALTER TABLE enrollment_hosts ADD COLUMN host_group text;
ALTER TABLE enrollment_hosts ADD COLUMN moving_from text;
ALTER TABLE enrollment_hosts ADD COLUMN facts_generation bigint;
ALTER TABLE enrollment_hosts ADD CONSTRAINT enrollment_hosts_facts_generation
  CHECK (facts_generation >= 0);

-- The group an approval names; NULL on a request not approved, or approved before.
ALTER TABLE enrollment_requests ADD COLUMN approved_host_group text;

-- The group an inventory record names; NULL on a record written before.
ALTER TABLE inventory ADD COLUMN host_group text;

-- The group an audit line concerns; NULL when it concerns none.
ALTER TABLE audit ADD COLUMN host_group text;
CREATE INDEX audit_host_group ON audit (host_group, seq) WHERE host_group IS NOT NULL;
