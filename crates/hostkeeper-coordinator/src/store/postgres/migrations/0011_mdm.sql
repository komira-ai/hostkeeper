-- older-releases: may-run
-- The MDM server inside the coordinator (docs/design/coordinator-mdm.md, sections 4.2, 4.5, 4.6,
-- 5.2 and 9.1): the Mac enrollments of a serial and their push state, the identities (the SCEP
-- challenges and the certificates issued under them), the commands queued for an enrollment, the
-- ABM device list and its sync state. An older release may run on it: it never reads these
-- tables. `mdm_secrets` and `mdm_settings` are added by the migration of their own task (CM20).
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

-- One row per MDM enrollment (one per `Authenticate`). A serial has at most one live row; a new
-- `Authenticate` marks the old one `replaced` in the transaction that inserts the new one.
CREATE TABLE mdm_enrollments (
  enrollment_id text PRIMARY KEY,
  serial text NOT NULL,
  udid text NOT NULL,
  -- live, replaced (a newer enrollment of the serial took its place) or checked_out.
  state text NOT NULL,
  -- The SHA-256 of the identity certificate the enrollment presents, in hex.
  cert_sha256 text,
  -- What a push needs: the token and magic of the last `TokenUpdate` and the topic.
  push_token bytea,
  push_magic text,
  topic text,
  -- A push was sent and no request from this enrollment has reached `/mdm` since.
  push_outstanding boolean NOT NULL DEFAULT false,
  last_push_unix bigint,
  last_seen_unix bigint,
  created_unix bigint NOT NULL);

ALTER TABLE mdm_enrollments ADD CONSTRAINT mdm_enrollments_id
  CHECK (enrollment_id ~ '^[A-Za-z0-9-]{1,64}$');
ALTER TABLE mdm_enrollments ADD CONSTRAINT mdm_enrollments_state
  CHECK (state IN ('live', 'replaced', 'checked_out'));
-- A serial has one live enrollment.
CREATE UNIQUE INDEX mdm_enrollments_one_live ON mdm_enrollments (serial) WHERE state = 'live';
CREATE INDEX mdm_enrollments_serial ON mdm_enrollments (serial);

-- One row per SCEP challenge, by the SHA-256 of the challenge in hex (the challenge itself is
-- never kept): bound to a serial and the blob's UDID, valid until `expires_unix`. A row becomes
-- `issued` when the certificate is recorded, which consumes the challenge; a repeat with the same
-- key hash gets the stored certificate.
CREATE TABLE mdm_identities (
  challenge_sha256 text PRIMARY KEY,
  serial text NOT NULL,
  udid text NOT NULL,
  expires_unix bigint NOT NULL,
  -- An operator confirmed the request this challenge was issued for (4.2).
  confirmed boolean NOT NULL DEFAULT false,
  -- challenged, issued, retired (its enrollment was replaced or checked out) or dead (refused
  -- for good).
  state text NOT NULL,
  key_sha256 text,
  cert_sha256 text,
  cert_der bytea,
  not_after_unix bigint,
  issued_unix bigint);

ALTER TABLE mdm_identities ADD CONSTRAINT mdm_identities_state
  CHECK (state IN ('challenged', 'issued', 'retired', 'dead'));
-- A challenged row has no certificate; every other row has all three of what was issued.
ALTER TABLE mdm_identities ADD CONSTRAINT mdm_identities_issued
  CHECK ((state = 'challenged') = (cert_sha256 IS NULL)
    AND (cert_sha256 IS NULL) = (cert_der IS NULL)
    AND (cert_sha256 IS NULL) = (not_after_unix IS NULL));
CREATE UNIQUE INDEX mdm_identities_cert ON mdm_identities (cert_sha256);
CREATE INDEX mdm_identities_serial ON mdm_identities (serial);

-- The commands of an enrollment, in the order they were enqueued (`seq`). `id` is the command's
-- UUID, which the gate chose: enqueuing an id that exists changes nothing.
CREATE TABLE mdm_commands (
  id text PRIMARY KEY,
  seq bigint GENERATED ALWAYS AS IDENTITY,
  enrollment_id text NOT NULL REFERENCES mdm_enrollments,
  command_type text NOT NULL,
  payload bytea NOT NULL,
  -- queued, sent, acked, errored or cancelled.
  state text NOT NULL,
  -- The last accepted answer was NotNow and the command was not sent since.
  deferred boolean NOT NULL DEFAULT false,
  deliveries bigint NOT NULL DEFAULT 0,
  created_unix bigint NOT NULL,
  sent_unix bigint,
  done_unix bigint,
  response bytea);

ALTER TABLE mdm_commands ADD CONSTRAINT mdm_commands_state
  CHECK (state IN ('queued', 'sent', 'acked', 'errored', 'cancelled'));
ALTER TABLE mdm_commands ADD CONSTRAINT mdm_commands_id CHECK (id ~ '^[A-Za-z0-9-]{1,64}$');
CREATE INDEX mdm_commands_claim ON mdm_commands (enrollment_id, seq) WHERE state = 'queued';
CREATE INDEX mdm_commands_sent ON mdm_commands (sent_unix) WHERE state = 'sent';

-- The devices of the org's ABM, by serial (`serial` is Apple's, upper case).
CREATE TABLE abm_devices (
  serial text PRIMARY KEY,
  model text,
  description text,
  -- ABM's profile_status: empty, assigned, pushed or removed.
  profile_status text NOT NULL,
  profile_uuid text,
  first_seen_unix bigint NOT NULL,
  synced_unix bigint NOT NULL,
  -- Set when ABM reports the device removed (disowned); NULL while it is ours.
  removed_unix bigint);

-- One row, id 1: where the ABM sync stands.
CREATE TABLE abm_state (
  id bigint PRIMARY KEY CHECK (id = 1),
  -- The cursor of the last sync, empty before the first.
  cursor text NOT NULL,
  -- The UUID of the ADE profile this coordinator defined, and the digest of what it rendered.
  profile_uuid text,
  profile_digest text,
  synced_unix bigint);
