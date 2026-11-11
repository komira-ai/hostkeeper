-- older-releases: refuse
-- When an enrolled host first served (network install N11): the store's `Host.first_serving_unix`
-- (store.proto field 9), one column of `enrollment_hosts` as every other field of the message is.
-- An older release must not run on it: its write of a host row sets the columns of 0001 from
-- `EXCLUDED.*`, which now has one more, so the server would refuse every host row it writes.
ALTER TABLE enrollment_hosts ADD COLUMN first_serving_unix bigint;
