-- older-releases: may-run
-- The SSH user certificates the coordinator signed, their revocations and the hash-chained audit
-- of the SSH calls (docs/design/ssh-through-coordinator.md, section 5). Only new tables and a
-- sequence: an older release never reads any of them.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

-- The serial of a certificate: one sequence for every coordinator instance, so no two
-- certificates share a serial. A rolled-back write leaves a gap, which a serial allows.
CREATE SEQUENCE ssh_cert_serial START 1;

-- One row for each certificate signed, written in the transaction that writes its audit line and
-- before the certificate exists.
CREATE TABLE ssh_cert (
  serial          bigint PRIMARY KEY,
  operator        text NOT NULL,
  host_id         text NOT NULL,
  key_fingerprint text NOT NULL,
  issued_unix     bigint NOT NULL,
  expires_unix    bigint NOT NULL,
  reason          text NOT NULL DEFAULT '');
CREATE INDEX ssh_cert_operator ON ssh_cert (operator);

-- The certificates that were revoked: hosts are told these serials (a later change).
CREATE TABLE ssh_revocation (
  serial  bigint PRIMARY KEY REFERENCES ssh_cert (serial),
  by_name text NOT NULL,
  at_unix bigint NOT NULL);

-- The audit of the SSH calls: each row carries the SHA-256 of the row before it and its own
-- content, written under one advisory lock, so a row that is deleted or edited breaks the chain
-- after it. The first row's `prev_hash` is 32 zero bytes.
CREATE TABLE ssh_audit (
  seq       bigserial PRIMARY KEY,
  at_unix   bigint NOT NULL,
  by_name   text NOT NULL,
  action    text NOT NULL,
  subject   text,
  detail    text NOT NULL DEFAULT '',
  prev_hash bytea NOT NULL,
  hash      bytea NOT NULL);
