-- older-releases: may-run
-- The built-in host group `all` (docs/design/hostgroups.md, section 15): every enrolled host is in
-- it, so a package it lists runs on every host. It lists `hostkeeper` alone, so that no host gets
-- a package it did not have; an operator adds the rest. A database that holds it already (a
-- coordinator that held a group of this name) keeps it as it is. An older release may run on it:
-- it reads `all` as a group like any other that no host is in, so it places every host as it did,
-- and if it removes the group, this release holds it again at its next start.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

INSERT INTO host_groups (name, generation, policy, by_name, at_unix)
  VALUES ('all', 1, '{}'::jsonb, 'migration', 0)
  ON CONFLICT (name) DO NOTHING;
INSERT INTO host_group_packages (host_group, service, position)
  VALUES ('all', 'hostkeeper', 0)
  ON CONFLICT (host_group, service) DO NOTHING;
