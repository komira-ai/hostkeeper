-- older-releases: may-run
-- The lab's sleds (docs/design/rack-sleds.md): a sled is the one thing that occupies U in a rack.
-- `lab_sleds` holds one row per sled and `lab_sled_items` one row per subject standing on a
-- slot of a sled, each the record as the layout file holds it, without its key. An older release
-- may run on it: it never reads either table.
--
-- The placements of the release before (`lab_placements`: rack, lowest U, height) become sleds
-- here, once: one implicit sled of that rack, U and height for each, and the subject on its slot 0.
-- The sled ids count from 1 in the order of rack id, U and subject key, as the file's reader
-- numbers them. `lab_placements` is left as it is, and no later release reads it: a rollback to
-- the release before finds the placements as they were at this migration.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE lab_sleds (
  id text PRIMARY KEY,
  -- The record as the layout file holds it, without its id: the id of its rack, its lowest U, its
  -- height in U, its label and whether it is implicit (made by a placement, gone when it is empty).
  record jsonb NOT NULL);

ALTER TABLE lab_sleds ADD CONSTRAINT lab_sleds_id CHECK (id ~ '^sled-[0-9]{1,9}$');

CREATE TABLE lab_sled_items (
  subject text PRIMARY KEY,
  -- The record as the layout file holds it, without its subject: the id of the sled and the
  -- slot, from 0.
  record jsonb NOT NULL);

ALTER TABLE lab_sled_items ADD CONSTRAINT lab_sled_items_subject CHECK (subject ~ '^(host|inventory):[!-~]{1,128}$');

WITH numbered AS (
  SELECT subject, record,
    row_number() OVER (
      ORDER BY (record->>'rack') COLLATE "C", (record->>'u')::integer, subject COLLATE "C") AS n
  FROM lab_placements),
made AS (
  INSERT INTO lab_sleds (id, record)
  SELECT 'sled-' || n,
    jsonb_build_object(
      'rack', record->'rack', 'u', record->'u', 'height_u', record->'height_u', 'implicit', true)
  FROM numbered
  RETURNING id)
INSERT INTO lab_sled_items (subject, record)
SELECT subject, jsonb_build_object('sled', 'sled-' || n, 'slot', 0)
FROM numbered;
