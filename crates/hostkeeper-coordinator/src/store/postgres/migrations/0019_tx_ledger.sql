-- older-releases: may-run
-- Several coordinators share one database (docs/design/coordinator-multi-instance.md, section 4):
-- the lock that kept a second one out is taken shared, so `lease.commits`, which every
-- transaction moved on and which would make all of them wait for one row, is no longer written.
-- A transaction names itself instead: its first statement inserts a row here, and a commit whose
-- outcome is not known is settled by inserting the same id again. A unique-index insert waits for
-- an in-progress insert of the same key and then does nothing if that one committed, so the
-- database, not a timing, says whether the first transaction landed. A release before this one
-- never reads the table.
CREATE TABLE tx_ledger (
  id      uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  at_unix bigint NOT NULL);
CREATE INDEX tx_ledger_at ON tx_ledger (at_unix);
