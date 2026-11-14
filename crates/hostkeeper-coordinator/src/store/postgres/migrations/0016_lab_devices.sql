-- older-releases: may-run
-- The lab's network devices (docs/design/network-gear.md): the switches of the racks, one row each,
-- the record as the layout file holds it, without its id. A device is placed like an inventory
-- entry (`lab_placements`, subject `inventory:<id>`). An older release may run on it: it never
-- reads the table.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE lab_devices (
  id text PRIMARY KEY,
  -- The record as the layout file holds it, without its id: name, role, console, management MAC
  -- and notes.
  record jsonb NOT NULL);

ALTER TABLE lab_devices ADD CONSTRAINT lab_devices_id CHECK (id ~ '^net\.[0-9]{1,9}$');
