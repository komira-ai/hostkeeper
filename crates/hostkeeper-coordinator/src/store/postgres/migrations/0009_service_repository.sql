-- older-releases: may-run
-- The repository a service is installed from, as `Services.SetRepository` set it
-- (docs/operations/hostgroups.md): the name of an entry of the repository registry, NULL for the
-- default (`komira`). An older release may run on it: it never reads the column, and its writes
-- of a service's row name their columns, so they leave it as it is; only a row it deletes, when
-- every other mark of the service it changed is back at its default, takes the value with it.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

ALTER TABLE services ADD COLUMN repository text;
