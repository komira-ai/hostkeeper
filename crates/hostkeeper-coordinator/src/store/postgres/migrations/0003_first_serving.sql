-- older-releases: refuse
-- When an enrolled host first served (docs/design/coordinator-storage.md, section 3.6): the first
-- heartbeat since its approval that reported a service it manages healthy with no update under
-- way. An older release must not run on it: its write of a host row names the columns of 0001
-- only, so it would fail on this table or keep a time an approval clears.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

ALTER TABLE enrollment_hosts ADD COLUMN first_serving_unix bigint;
