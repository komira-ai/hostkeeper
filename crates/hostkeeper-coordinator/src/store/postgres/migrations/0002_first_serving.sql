-- older-releases: refuse
-- When an enrolled host first served (network install N11): the store's `Host.first_serving_unix`
-- (store.proto field 9), one column of `enrollment_hosts` as every other field of the message is.
-- An older release must not run on it: its write of a host row lists the columns of 0001 against
-- `EXCLUDED.*`, which now has one more, and it would not keep the time.
ALTER TABLE enrollment_hosts ADD COLUMN first_serving_unix bigint;
