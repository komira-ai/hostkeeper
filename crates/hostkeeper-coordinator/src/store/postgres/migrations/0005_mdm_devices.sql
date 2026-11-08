-- older-releases: may-run
-- The MDM gate's device records (docs/design/hostkeeper-mdm.md, section 6.1;
-- docs/design/coordinator-storage.md, section 3.6): one row per MDM enrollment id, written by the
-- gate before it acts on an event. An older release may run on it: it never reads the table.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE mdm_devices (
  enrollment_id text PRIMARY KEY,
  -- The record as the gate keeps it: the message of its file `<enrollment id>.pb` of the file
  -- store (its schema, the record, the record's SHA-256), read by the gate's own reader.
  record bytea NOT NULL);

ALTER TABLE mdm_devices ADD CONSTRAINT mdm_devices_enrollment_id
  CHECK (enrollment_id ~ '^[A-Za-z0-9-]{1,64}$');
