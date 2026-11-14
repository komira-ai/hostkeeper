-- older-releases: may-run
-- Removes the built-in host group `all` (docs/design/hostgroups.md, "Removing the built-in group
-- all"): a host is in one group or in none, and every group lists `hostkeeper`. Deletes the group
-- named `all` and every row keyed by it (its packages, desired versions, minimum versions, marks,
-- rollout positions and the hosts those recorded done); a host recorded in it is in no group
-- (`host_group` NULL). Then each group that does not list `hostkeeper` gets it, first in its list
-- and at the version `all` had for it when the group has none of its own, so that the hosts of
-- such a group, which ran `hostkeeper` under `all`, keep their agent's updates.
-- An older release may run on it: it holds `all` again in memory, listing `hostkeeper` alone with
-- no desired version, and reads every host as in it, which orders the hosts no package they were
-- not already ordered; every group it reads lists `hostkeeper`, as it accepts.
-- deletes-rows: the built-in group `all` and every row keyed by it, which the owner removed
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

INSERT INTO group_desired (host_group, service, version, generation, write_id, replaced_version)
  SELECT g.name, d.service, d.version, d.generation, d.write_id, d.replaced_version
  FROM host_groups g, group_desired d
  WHERE d.host_group = 'all' AND d.service = 'hostkeeper' AND g.name <> 'all'
    AND NOT EXISTS (
      SELECT 1 FROM host_group_packages p WHERE p.host_group = g.name AND p.service = 'hostkeeper')
    AND NOT EXISTS (
      SELECT 1 FROM group_desired o WHERE o.host_group = g.name AND o.service = 'hostkeeper');

UPDATE host_group_packages SET position = position + 1
  WHERE host_group <> 'all'
    AND host_group NOT IN (SELECT host_group FROM host_group_packages WHERE service = 'hostkeeper');

INSERT INTO host_group_packages (host_group, service, position)
  SELECT name, 'hostkeeper', 0 FROM host_groups
  WHERE name <> 'all'
    AND name NOT IN (SELECT host_group FROM host_group_packages WHERE service = 'hostkeeper');

DELETE FROM group_rollout_done WHERE host_group = 'all';
DELETE FROM group_rollouts WHERE host_group = 'all';
DELETE FROM group_services WHERE host_group = 'all';
DELETE FROM group_min_versions WHERE host_group = 'all';
DELETE FROM group_desired WHERE host_group = 'all';
DELETE FROM host_group_packages WHERE host_group = 'all';
DELETE FROM host_groups WHERE name = 'all';

UPDATE enrollment_hosts SET host_group = NULL WHERE host_group = 'all';
