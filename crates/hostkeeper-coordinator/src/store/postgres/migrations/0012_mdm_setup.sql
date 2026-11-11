-- older-releases: may-run
-- The setup of the MDM server inside the coordinator (docs/design/coordinator-mdm.md, section
-- 4.9): the secrets the operator uploads or the coordinator makes, each sealed with AES-256-GCM
-- under the key of `--mdm-secrets-key-file` before it reaches the database, and the settings of
-- the setup page. An older release may run on it: it never reads either table.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

-- One row per version of each kind of secret. `sealed` is the nonce followed by the ciphertext and
-- its tag; the kind and the version are the associated data of the seal, so a row copied to
-- another kind or version does not open. The newest version of a kind is the one in use; an older
-- one is kept until its grace ends, so that a wrong upload is undone by uploading again
-- (`superseded_unix` says when a newer version took its place).
CREATE TABLE mdm_secrets (
  kind text NOT NULL,
  version bigint NOT NULL,
  sealed bytea NOT NULL,
  -- The SHA-256 of the plaintext in hex: what the status shows in place of the secret.
  fingerprint text NOT NULL,
  not_after_unix bigint,
  created_unix bigint NOT NULL,
  superseded_unix bigint,
  PRIMARY KEY (kind, version));

ALTER TABLE mdm_secrets ADD CONSTRAINT mdm_secrets_kind CHECK (kind ~ '^[a-z0-9_]{1,40}$');
ALTER TABLE mdm_secrets ADD CONSTRAINT mdm_secrets_version CHECK (version >= 1);
-- Nothing shorter than a nonce and a tag is a sealed value.
ALTER TABLE mdm_secrets ADD CONSTRAINT mdm_secrets_sealed CHECK (length(sealed) >= 28);
ALTER TABLE mdm_secrets ADD CONSTRAINT mdm_secrets_fingerprint
  CHECK (fingerprint ~ '^[0-9a-f]{64}$');

-- The settings of the setup page, one row per setting that was set: a setting with no row has
-- its default. The name is one this release knows; a later release adds names.
CREATE TABLE mdm_settings (
  name text PRIMARY KEY,
  value text NOT NULL);

ALTER TABLE mdm_settings ADD CONSTRAINT mdm_settings_name CHECK (name ~ '^[a-z0-9_]{1,40}$');
