-- older-releases: may-run
-- The operator's users and the API tokens made in the UI (docs/design/operator-login.md,
-- sections 6, 7 and 9): one row per user and one per token, the record as the accounts file holds
-- it. An older release may run on it: it never reads either table.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE users (
  name text PRIMARY KEY,
  -- The record as the accounts file holds it, without its name: the password's hash, whether the
  -- user is enabled, the initial credential's deadline, who made it and when.
  record jsonb NOT NULL);

ALTER TABLE users ADD CONSTRAINT users_name CHECK (name ~ '^[a-z0-9-]{1,32}$');

CREATE TABLE api_tokens (
  name text PRIMARY KEY,
  -- The record as the accounts file holds it, without its name: the SHA-256 of the token, who
  -- made it and when.
  record jsonb NOT NULL);

ALTER TABLE api_tokens ADD CONSTRAINT api_tokens_name CHECK (name ~ '^[a-z0-9-]{1,32}$');
