-- older-releases: may-run
-- The registry of package repositories (docs/design/packages-repositories.md, section 3;
-- docs/design/coordinator-storage.md, section 3.2): one row per repository, and the registry's
-- generation in a row of its own. An older release may run on it: it never reads either table.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE repositories (
  name text PRIMARY KEY,
  -- The record as the repositories file holds it: its generation and the repository, the keyring
  -- as base64.
  record jsonb NOT NULL);

ALTER TABLE repositories ADD CONSTRAINT repositories_name
  CHECK (name ~ '^[a-z0-9-]{1,32}$');

-- The registry's generation: the only row is id 1, and a registry never put has none.
CREATE TABLE repository_generation (
  id bigint PRIMARY KEY,
  generation bigint NOT NULL);

ALTER TABLE repository_generation ADD CONSTRAINT repository_generation_id CHECK (id = 1);
ALTER TABLE repository_generation ADD CONSTRAINT repository_generation_positive
  CHECK (generation > 0);
