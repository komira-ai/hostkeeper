-- older-releases: may-run
-- The lab's layout (docs/design/ui-shell-lab.md): the one cluster's name, the numbered rows of
-- numbered racks the web page's Home draws, and the placements of servers in them. One row per item, the record
-- as the layout file holds it, without its key. An older release may run on it: it never reads
-- any of the four tables.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE lab_cluster (
  id text PRIMARY KEY,
  -- The cluster's name as a JSON text; the only row is `name`.
  record jsonb NOT NULL);

ALTER TABLE lab_cluster ADD CONSTRAINT lab_cluster_id CHECK (id = 'name');

CREATE TABLE lab_rows (
  id text PRIMARY KEY,
  -- The record as the layout file holds it, without its id: the number.
  record jsonb NOT NULL);

ALTER TABLE lab_rows ADD CONSTRAINT lab_rows_id CHECK (id ~ '^row-[0-9]{1,9}$');

CREATE TABLE lab_racks (
  id text PRIMARY KEY,
  -- The record as the layout file holds it, without its id: the id of its row, its
  -- number in the row and its height in U.
  record jsonb NOT NULL);

ALTER TABLE lab_racks ADD CONSTRAINT lab_racks_id CHECK (id ~ '^rack-[0-9]{1,9}$');

CREATE TABLE lab_placements (
  subject text PRIMARY KEY,
  -- The record as the layout file holds it, without its subject: the id of the rack, the lowest U
  -- and the height in U.
  record jsonb NOT NULL);

ALTER TABLE lab_placements ADD CONSTRAINT lab_placements_subject CHECK (subject ~ '^(host|inventory):[!-~]{1,128}$');
