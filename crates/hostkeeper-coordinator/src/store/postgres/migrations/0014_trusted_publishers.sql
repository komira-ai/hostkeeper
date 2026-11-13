-- older-releases: may-run
-- The trusted publishers of a service (docs/deploy/ci-client.md, "Trusted publishers"): the GitHub
-- Actions runs whose OIDC token may set its desired version, as a JSON array, NULL while the
-- service's entries were never held (the coordinator's start imports its allow-list's then). An
-- older release may run on it: it never reads the column, and its writes of a service's row name
-- their columns, so they leave it as it is; only a row it deletes, when every other mark of the
-- service it changed is back at its default, takes the value with it.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

ALTER TABLE services ADD COLUMN trusted_publishers jsonb;
