-- older-releases: may-run
-- An operator's approval of a host's key for service identity (docs/design/service-identity.md,
-- section 6): one row per host that has one, written by the coordinator with the host's enrollment
-- and removed whenever the host is written without one (an approval of the host). An older release
-- may run on it: it never reads the table, and its writes of a host row (`enrollment_hosts`) do not
-- touch it, so a row it leaves behind is read again by the newer release, which counts a row for
-- another key than the host's as absent.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.

CREATE TABLE enrollment_host_identity (
  host_id text PRIMARY KEY,
  -- The record as the file store keeps it in the host file's field 14: the encoded message
  -- `hostkeeper.enrollment.store.v1.IdentityApproval`.
  identity_approval bytea NOT NULL);
