-- older-releases: refuse
-- Generations and write ids of the desired state (docs/design/coordinator-storage.md, section 3.2).
-- An older release must not run on it: it would write a desired state without raising its
-- generation, and a writer's if_generation would then pass against a state it never saw.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

-- One more for every accepted write, a write of the same version included.
ALTER TABLE desired ADD COLUMN generation bigint NOT NULL DEFAULT 0;
-- The last write's id, 1 to 128 visible ASCII characters.
ALTER TABLE desired ADD COLUMN write_id text;
-- The version and minimums the last change of version replaced.
ALTER TABLE desired ADD COLUMN replaced_version text;
ALTER TABLE desired ADD COLUMN replaced_min_up jsonb;

ALTER TABLE desired ADD CONSTRAINT desired_generation CHECK (generation >= 0);
ALTER TABLE desired ADD CONSTRAINT desired_write_id CHECK (write_id ~ '^[!-~]{1,128}$');
ALTER TABLE desired ADD CONSTRAINT desired_replaced
  CHECK ((replaced_version IS NULL) = (replaced_min_up IS NULL));
