# Coordinator storage: the state in Postgres

Status: design, for the maintainers' comments. It supersedes "plain files, no database" for the coordinator's state:
[hostkeeper-design.md](hostkeeper-design.md) section 4.3 ("There is no database") and the coordinator's rows of its
section 10, the halts-file storage of the plan position in [rollout-update-plans.md](rollout-update-plans.md) section 3,
and the device-record file of [hostkeeper-mdm.md](hostkeeper-mdm.md) section 6.1. Where they disagree with this document
on where the coordinator keeps its state, this document wins. C1 to C4 and C6 are built: the storage trait and its
fake (section 4.1), the enrollment crate's `Records` port, the Postgres store
(`crates/hostkeeper-coordinator/src/store/postgres/`, with its migrations `0001` to `0008`, sections 3 to 8),
`import` and `export` (section 11: `crates/hostkeeper-coordinator/src/transfer.rs` and
`crates/hostkeeper-coordinator/src/store/postgres/transfer.rs`), the generations and write ids (migration
`0003`, section 3.2), the releases of a set with targets (migration `0004`, section 3.2, from
[packages.md](packages.md) section 3.6), the MDM gate's device records (migration `0005`, the table `mdm_devices`,
sections 2 and 3.6: `crates/hostkeeper-coordinator/src/store/postgres/devices.rs`), what each host's daemon last said it runs (migration `0006`, the table `enrollment_host_running`, section 3.6), the registry of package repositories (migration `0007`, section 3.2, from [packages-repositories.md](packages-repositories.md) section 3), and C5, the compose deployment with its backups, restore and drill (sections 9 and
10: `docs/deploy/coordinator/`, and the `xtask db-backup` verb in `crates/xtask/src/db_backup.rs`). Every other
statement about new code is **Designed**, and section 13 lists the build items, C1 to C11, one pull request each. The tags are those of hostkeeper-design.md: **Built** names code on `main` with a path, **Designed** means
nothing implements it yet. The placement tables (`assignments`, the role scope of `min_versions`) and the per-service keying of `desired` and the plan's position are superseded by [hostgroups.md](hostgroups.md) section 8 (migration `0008`, whose tables are built; nothing reads them yet).

Ruling: the coordinator runs as one container and Postgres 16 as a second, on one machine for now, later on separate
small Linux servers. The coordinator's durable state moves from its files into that database. The file lock goes, and a
Postgres advisory lock keeps the single-writer guarantee. A standby coordinator is designed here (section 7); the lock
and its epoch are built by C3, and the standby itself is a later item that is not scheduled.

## 1. What changes for the operator

**Before.** `hostkeeper-coordinator serve` keeps its state in files named by `--state-file`, `--ops-file` and
`--enrollment-dir`, beside each other on one disk (**Built**,
[coordinator-state.md](../operations/coordinator-state.md)). A backup copies the files of a stopped coordinator; a
restore copies them back. A second coordinator on the same files is refused by a `flock` on `<state-file>.lock`, which
holds on one machine only.

**After.**

- The coordinator is started with `--database-dsn-file <file>` in place of `--state-file`, `--ops-file` and
  `--enrollment-dir`. The file holds the connection string with its password, owner-only. Nothing about the database is
  on the command line or in the environment. It also takes `--halt-marker-file`, a local file path that stays writable
  when the database is not (section 4.3).
- Everything the files held is in one Postgres database (section 3): the desired state, the operations store, the halts,
  the plan positions, the audit log, the spent CI token ids, the acknowledged verdicts and the enrollment store. A
  change that today rewrites several files is one transaction (section 4).
- The deployment is a compose file of two containers, the coordinator and `postgres:16` pinned by digest, with the
  database on a named volume and a health check (section 9).
- Backups are `pg_dump` on a timer, taken while the coordinator runs, with a restore drill (section 10). The
  coordinator's own `backup` and `restore` verbs stay for the file store only.
- The farm moves once: stop the file coordinator, run `hostkeeper-coordinator import`, start the compose stack. No host
  enrolls again (section 11). The way back is `hostkeeper-coordinator export` and the file coordinator.

**What does not change.** The rollout file, the operator tokens file, the OIDC allow-list, the TLS files, the CA key and
its certificate, the server certificate file, the Mac serial allowlist, the MDM pkg allowlist and the UI directory
stay files, read as today.
The fleet view (heartbeats, outstanding orders, bake, deferrals, tries, the quiet start) stays in memory and is rebuilt
from heartbeats after a restart. Every decision reads memory, never the database: a heartbeat in the steady state costs
no query. (Today it reads the enrollment store's host and deny files at every heartbeat; an in-memory copy of those rows
takes their place, section 4.4.) Every API answer, refusal and JSON route keeps its bytes, except `audit_unavailable`,
which the Postgres store no longer returns for an operations change (section 4.4), a status that is quiet while the
database is lost (section 5), the new alert `store_unreachable`, and C6, where an identical set raises the generation
(section 3.2). The gate and the API code keep their shape: they change state through the same calls (`OpsStore::change`
and its siblings, `DesiredStore::record`, `VerdictFile::record`, the enrollment store's methods), which now reach a
storage trait (section 4.1).

## 2. What is stored where

| Thing | Today | After |
|---|---|---|
| Desired state and the package digests of every version CI set | `--state-file` | tables `desired`, `desired_digests` |
| Spent CI token ids | `<state-file>.jti` | table `spent_tokens` |
| Acknowledged and counted verdicts | `<state-file>.verdicts` | tables `verdict_standing`, `verdict_counted` |
| Exclusions, labels, holds, assignments, minimums, uninstall intents, bad marks, last good, standing force, inventory | `--ops-file` | the tables of section 3.3 |
| Halts, halt reports, clear generations | `<ops-file>.halts` with the ops file | tables `halts`, `halt_reports`, column `services.halt_generation` |
| Plan positions (rollout-update-plans.md section 3) | `rollouts` of `<ops-file>.halts` | tables `rollouts`, `rollout_done` |
| The platform of each inventory record (`linux-x86_64`, `linux-arm64`) | `platforms` of `<ops-file>.halts`, by host id | column `inventory.platform` |
| The audit log | `<ops-file>.audit` and its sealed segments; the ring of 64 in the ops file | table `audit`; the ring is the newest 64 rows |
| Enrollment requests, issued certificates, hosts, deny rows, Mac nonces, the serial counter | `--enrollment-dir/*.pb` | the tables of section 3.6 |
| The MDM gate's device records | `--enrollment-dir/mdm-devices/<enrollment id>.pb` | table `mdm_devices` (section 3.6) |
| The single-writer lock | `flock` on `<state-file>.lock`; the enrollment store's own `flock` on `<enrollment-dir>/lock` | an advisory lock on one session, and the row `lease` (section 5); the enrollment store keeps its in-process mutex |
| Staged halts file, restore marker | `<ops-file>.halts.next`, `<state-file>.restore-unfinished` | gone: a transaction commits or does not |
| The unfinished-halt marker | `<ops-file>.halts.unfinished` | a file still, named by `--halt-marker-file` (section 4.3) |
| Rollout file, tokens, OIDC allow-list, TLS files, CA key and certificate, server certificate, Mac allowlist, MDM pkg allowlist, UI | files | files, unchanged |
| Fleet, orders, bake, deferrals, tries, poll nonces, the enrollment store's live-machine map | memory | memory, unchanged |

The MDM gate's device records (hostkeeper-mdm.md section 6.1, task M8) are kept where the coordinator keeps its other
state, so the gate runs on either store. On the file store they are a file per device,
`<--enrollment-dir>/mdm-devices/<enrollment id>.pb`; on the database they are the table `mdm_devices` of migration
`0005` (**Built**). Both are the gate's `DeviceStore`, and both pass the gate's one case list
(`crates/hostkeeper-mdm-gate/src/conformance.rs`). The import reads the files into the table and the export writes
them back (section 11).

## 3. The schema

### 3.1 Conventions

- One database per farm, owned by one role, `hostkeeper`. The tables live in the schema `hostkeeper`; migrations name no
  schema and run with `search_path` set to it, as the `js-state` crate of komira-ai/jobscheduler does (its Postgres
  store is the pattern this design copies where it says "jobscheduler").
- Names, versions, roles, host ids and actors are `text`. The grammars stay in Rust (`hostkeeper-wire`, the enrollment
  crate's `names`): a row is written only from a parsed value, and a load parses every value again and refuses the start
  on one that does not parse, naming the table and the key, as a strict file reader does today.
- Counters (`seq`, the halt, force and uninstall generations, the round, the serial) are `bigint`. The code's counters
  are `u64`; a value above `i64::MAX` is refused at the write with `store_failed`, and the import refuses it naming the
  field. No counter of a real farm comes near it.
- Times are unix seconds in `bigint`, taken from the coordinator's own clocks as today (the audit's `UnixClock`, the
  OIDC clock, the enrollment store's `now`). No column defaults to the database's `now()` except
  `schema_version.applied_unix`, which no decision reads. The wall-clock lint (`crates/xtask/tests/wallclock`) keeps
  holding: no new code reads the wall clock.
- A value that is JSON today (`UpdateResult`, `ForceRecord`, `IndexRecord`, role limits) is `jsonb`, written and read by
  the same serde code, so a load is exactly as strict or as lenient as the file reader is: `UpdateResult` keeps its
  tolerance of fields and values it does not know, and `ForceRecord` and `IndexRecord`, which refuse an unknown field
  (`deny_unknown_fields` in `force.rs` and `registry.rs`), refuse it at load as the file reader does at start. Hashes,
  keys and DER are `bytea`.
- A list the code keeps in order (exclusions, holds, halt reports, failure acks, counted verdicts, bad versions) has a
  `position bigint` column, so a load gives the list back in its order. A key the code keeps unique (one exclusion per
  service and host, one hold per service and scope, one report per host) is the primary key, so the database refuses a
  second one too.
- A per-service entry whose every field is at its default is not stored. The code reads a missing entry as the default
  (`ServiceOps`, `ServiceAssignments`, `Rollout`); build item C1 pins this with a test before any table relies on it,
  and the import compares states with such entries removed.

### 3.2 Desired state

```sql
desired(
  service           text PRIMARY KEY,
  version           text NOT NULL,
  max_updating      jsonb NOT NULL,      -- role -> count, the RoleLimits map
  min_up            jsonb NOT NULL,
  release_digest    text,                -- one package digest, or
  release_index     jsonb,               -- the IndexRecord: index digest and package per platform
  CHECK (release_digest IS NULL OR release_index IS NULL))

desired_digests(                         -- the Registry: every version CI set, at most 4096 a service
  service text, version text, release jsonb NOT NULL,   -- a digest string or an IndexRecord
  PRIMARY KEY (service, version))

releases(                                -- migration 0004: the release a version was set with targets
  service text, version text,
  targets jsonb NOT NULL,                -- per platform, its TargetList as proto3 JSON
  release_index_digest text,             -- the index set with the targets; NULL for none
  PRIMARY KEY (service, version))
```

A service's row and its registry rows change in one transaction, so they are never apart (today they are one file,
`desired.rs`). The registry's cap and its eviction (keep the desired version, `last_good` and the bad versions) stay in
Rust (`registry.rs`). A version set with targets is written as one set with neither a digest nor an index, and its
release in `releases`, in the same transaction; the releases of a service keep the registry's cap and eviction rule
(`releases.rs`), and are what the file store keeps in `<state-file>.releases`. `0004` is `may-run`: an older release
never reads the table. So is `0007`, the repository registry's (`repositories.rs`). `--legacy-service` keeps its
meaning for the JSON routes; the single-state file form it selects is a file encoding and has no table.

**Generations and write ids.** The operator API already defines them (`SetDesiredRequest.if_generation` and `write_id`,
`SetDesiredResponse.record`, the `WriteRecord` and `Replaced` messages, the refusal `generation_conflict`). The file
store keeps neither, so over it a set that names either field is refused `invalid_request` (`DesiredError::Unrecorded`
in `set_desired_by`, `src/service_ci.rs`, **Built**). The database keeps them: C6's migration `0003_generations` adds
to `desired`:

```sql
  generation        bigint NOT NULL DEFAULT 0,  -- one more for every accepted write, a write of the same version included
  write_id          text,                       -- the last write's id, 1 to 128 visible ASCII characters
  replaced_version  text,                       -- the version and minimums the last change of version replaced
  replaced_min_up   jsonb
```

and honours the two fields as the proto describes (**Built**: `crates/hostkeeper-coordinator/src/write_record.rs`, the
columns in `store/postgres/generations.rs`; a force raises the generation too, with no write id). C6's migration is
one an older release must not run on (section 6):
an older coordinator would write a desired state without raising its generation, and a writer's `if_generation` would
then pass against a state it never saw. C6 is a behaviour change of its own: over the database an identical set, which
the file store still answers `Recorded::Unchanged` without writing, raises the generation and is written.

### 3.3 The operations store

```sql
services(                                 -- the per-service marks of ServiceOps and ServiceAssignments
  service              text PRIMARY KEY,
  last_good            text,
  halt_generation      bigint NOT NULL DEFAULT 0,   -- clears; only grows; told to agents that announced halt-v1
  force_generation     bigint NOT NULL DEFAULT 0,   -- forces; the order id is force-<generation>
  uninstall_generation bigint NOT NULL DEFAULT 0,   -- the order id is uninstall-<generation>
  force                jsonb)                       -- the standing ForceRecord, or NULL

bad_versions(service text, version text, by_force boolean NOT NULL, position bigint NOT NULL,
  PRIMARY KEY (service, version))
failure_acks(service text, node_id text, report jsonb NOT NULL, position bigint NOT NULL,
  PRIMARY KEY (service, node_id))
host_labels(node_id text, key text, value text NOT NULL, PRIMARY KEY (node_id, key))
exclusions(service text, node_id text, reason text NOT NULL, position bigint NOT NULL,
  PRIMARY KEY (service, node_id))
holds(service text, scope text CHECK (scope IN ('host', 'role', 'fleet')), target text NOT NULL,  -- '' for fleet
  reason text NOT NULL, by_name text NOT NULL, at_unix bigint NOT NULL, position bigint NOT NULL,
  PRIMARY KEY (service, scope, target))
assignments(service text, scope text CHECK (scope IN ('host', 'role')), target text,
  entry text CHECK (entry IN ('include', 'exclude')), PRIMARY KEY (service, scope, target))
min_versions(service text, scope text CHECK (scope IN ('host', 'role')), target text, version text NOT NULL,
  PRIMARY KEY (service, scope, target))
uninstall_intents(service text, node_id text, generation bigint NOT NULL, PRIMARY KEY (service, node_id))
inventory(host_id text PRIMARY KEY, role text NOT NULL, data_macs text[] NOT NULL, bmc_mac text NOT NULL,
  serial text NOT NULL, notes text NOT NULL, platform text NOT NULL DEFAULT '', by_name text NOT NULL,
  at_unix bigint NOT NULL,
  position bigint NOT NULL,                -- the code keeps the records in the order they were added
  approve_until_unix bigint NOT NULL DEFAULT 0, used_request_id text, used_unix bigint,  -- the record's window
  CHECK ((used_request_id IS NULL) = (used_unix IS NULL)))
CREATE UNIQUE INDEX inventory_serial ON inventory (lower(serial)) WHERE serial <> '';
CREATE UNIQUE INDEX inventory_bmc ON inventory (lower(bmc_mac)) WHERE bmc_mac <> '';
```

`bad_versions` keys a version once per service. The code reads `bad` only by membership and adds a version only when it
is not there (`operator.rs`, `force.rs`), except the coordinator's own mark of a rollback (`reconcile.rs`), which pushes
without the check; a version listed twice reads the same as once, and the import folds such a pair into one row. The
inventory's uniqueness rules (no two records share a host id, a MAC or a serial, compared without case) stay in Rust
under the store's lock (`inventory.rs`). An empty `role`, `bmc_mac`, `serial`, `notes` or `platform` is the empty string, as the
code keeps it (`String`, left out of the file when empty; a platform that is none, left out of the halts file), and the two unique indexes skip it; they are a second guard
on the serial and the BMC MAC alone, and the rule across data and BMC MACs stays in Rust. The caps (`MAX_HOSTS`,
`MAX_EXCLUDED`, `MAX_HOLDS`, `assignment::MAX_ENTRIES`, `inventory::MAX_RECORDS`, `MAX_LABELS`, `MAX_REASON_BYTES`) stay
in the edits, checked under the store's lock as today.

### 3.4 Halts and plan positions

```sql
halts(service text PRIMARY KEY, node_id text NOT NULL, check_name text NOT NULL, target text NOT NULL,
  origin text NOT NULL CHECK (origin IN ('hook', 'updater', 'operator')), report jsonb)   -- the standing halt
halt_reports(service text, node_id text, kind text CHECK (kind IN ('acked', 'seen')), target text NOT NULL,
  report jsonb, position bigint NOT NULL, PRIMARY KEY (service, kind, node_id))

rollouts(service text PRIMARY KEY, version text, entered bigint NOT NULL DEFAULT 0,
  paused_by text, paused_reason text, CHECK ((paused_by IS NULL) = (paused_reason IS NULL)))
rollout_done(service text REFERENCES rollouts ON DELETE CASCADE, node_id text, PRIMARY KEY (service, node_id))
```

The halts file exists today only so that the release before the halt release can still read the ops file, and the digest
handshake between the two files exists only because two files cannot be replaced at once (`ops_file.rs`). In the
database both are rows of one transaction, so the split, the staged `.halts.next`, the digest and `settle` go. A clear
writes, in one transaction: the `halts` row removed, an `acked` report for the halt's host and each `seen` host, the
`seen` rows removed, `halt_generation` one more, and its audit row. A generation is therefore on disk with its clear or
not at all, and no agent is told one that is not, as today.

A plan position keeps the semantics of rollout-update-plans.md section 3: `entered` counts stages of the rollout file, a
position whose `version` is not the desired one reads as the first stage with no host done, a pause stands across
versions, and a stage entry rewrites `rollout_done` with the hosts done so far. Rows of services that are no longer
defined stay, as the halts file keeps them today; they are read only when the version matches.

### 3.5 The audit log

```sql
audit(
  seq      bigint PRIMARY KEY,          -- one more than the row before, across the whole history
  at_unix  bigint NOT NULL,
  by_name  text NOT NULL,               -- the token name, "coordinator", "ci" or "mdm-nonce"
  action   text NOT NULL,
  service  text,
  node_id  text,
  reason   text)
-- indexed by actor, action, service, host and time:
CREATE INDEX audit_by    ON audit (by_name, seq);
CREATE INDEX audit_action ON audit (action, seq);
CREATE INDEX audit_service ON audit (service, seq) WHERE service IS NOT NULL;
CREATE INDEX audit_node  ON audit (node_id, seq) WHERE node_id IS NOT NULL;
CREATE INDEX audit_time  ON audit (at_unix);
```

- **Append-only.** A trigger refuses `UPDATE` and `DELETE` of a row and `TRUNCATE` of the table. It guards against a bug
  and a hand-typed statement, not against the coordinator's own role, which owns the table and could drop the trigger.
  Taking that power away (a migration role that owns the tables, and a coordinator role granted only `INSERT` and
  `SELECT` on `audit`) is not designed here.
- **Numbering.** `seq` is assigned by the coordinator under the operations store's writers' lock at each change, from
  memory, as `audited_changes` in `ops.rs` does today: one more than the larger of the newest number in memory (pending
  rows included) and the newest committed, which a start reads from the table. A refused storage-first change leaves
  memory unchanged and so takes no number, except that a commit of unknown outcome (section 4.4) keeps its numbers
  reserved until the reconnect settles it, so no later change can take a number it may already hold; pending rows are
  committed in their order before any later row. The table is therefore in the order memory made it, and gap-free but
  for the numbers of a lost commit that turned out not to have landed. The single writer (section 5) makes a sequence
  object unnecessary, and keeps the numbers free of the gaps a sequence leaves on a rolled-back transaction. The import
  copies every line of the file history with its own `seq` and `at_unix`, so the numbers continue across the move.
- **The actor.** The actor is derived from `by_name` (`coordinator`, `ci`, otherwise an operator), as `actor_of` does
  today. `Audit.List`'s `actor` filter becomes a condition on `by_name`, served by `audit_by`; no second column can
  disagree with the first.
- **Reads.** `Audit.List` becomes one query: the filters it has today (`actor`, `by_name`, `action`, `service`,
  `node_id`), `seq > since_seq`, `ORDER BY seq` ascending, or descending for `newest` and turned back, `LIMIT` of at
  most 256. `next_seq` keeps its meaning. `Hosts.Retire`'s check of whether a retirement was audited (`retire_audited`)
  becomes `SELECT action FROM audit WHERE node_id = $1 AND action = ANY($2) ORDER BY seq DESC LIMIT 1`. The ring of 64
  that `Releases.Status` and `/v1/status` show is the newest 64 rows, loaded at start and kept in memory as today; it is
  not stored apart.
- **Retention.** Every row is kept. A row is about 200 bytes, and the file default of eight 16 MiB segments, about
  640,000 lines, would be some 130 MB of rows. `--audit-max-bytes` and `--audit-retained-segments` stay flags of the
  file store and are refused beside `--database-dsn-file`. The database prunes nothing, so `audit_pruned_before` is the
  import's: the time of the oldest line the files still held when they were imported (`audit_file::pruned_before`), kept
  in the `imports` row, or nothing pruned for a database that was never imported into. The enrollment store's start-time
  audit check (`audit_to`) compares every recorded approval and expiry newer than that with the table, by one query, so
  an import of a history whose oldest segments were deleted adds no audit rows at the first start.

### 3.6 Spent tokens, verdicts and enrollment

```sql
spent_tokens(jti text PRIMARY KEY, until_unix bigint NOT NULL)
CREATE INDEX spent_tokens_until ON spent_tokens (until_unix);

verdict_standing(service text, node_id text, report jsonb NOT NULL, PRIMARY KEY (service, node_id))
verdict_counted(service text, position bigint, node_id text NOT NULL, report jsonb NOT NULL,
  PRIMARY KEY (service, position))
```

- **Spent tokens.** A spend is one transaction: `DELETE` the ids whose `until_unix` is past by the OIDC clock, then
  `INSERT` the id. It commits before the verifier accepts the token, as the append and `sync_data` do today
  (`jti_file.rs`). A second spend of the same id fails on the primary key, which is a replay even if two processes ever
  raced. The verifier's memory cap of 10,000 stays. The load at start reads the ids not yet past.
- **Verdicts.** `VerdictFile::record` commits the changed `standing` rows and, per service it names, the whole `counted`
  list, in one transaction, before the heartbeat answer acknowledges any verdict (`verdict_file.rs`): an answer
  acknowledges only what is committed.

```sql
enrollment_requests(
  request_id bytea PRIMARY KEY,           -- SHA-256 of the CSR's public key, 32 bytes
  state smallint NOT NULL,                -- the proto numbers: 1 pending, 2 duplicate_claim, 3 approved, 4 issued, 5 denied
  expired_unix bigint NOT NULL DEFAULT 0, expired_after_secs bigint NOT NULL DEFAULT 0,
  posted_unix bigint NOT NULL, round bigint NOT NULL, serial bigint NOT NULL DEFAULT 0,
  renewal_of text NOT NULL DEFAULT '', image text NOT NULL, csr_der bytea NOT NULL, spki_der bytea NOT NULL,
  mac_nonce_presented boolean NOT NULL,
  machine_id text NOT NULL, dmi_serial text NOT NULL, dmi_product_uuid text NOT NULL, addresses text[] NOT NULL,
  mac_serial text NOT NULL, mac_platform_uuid text NOT NULL, bmc_mac text NOT NULL,      -- the claims
  approved_host_id text, approved_role text, approved_services text[], approved_replace boolean,
  approver text, approved_unix bigint, approved_reason text, approved_mac_nonce bytea)   -- the approval
CREATE INDEX enrollment_waiting ON enrollment_requests (posted_unix, request_id) WHERE state IN (1, 2);
CREATE INDEX enrollment_machine ON enrollment_requests (machine_id) WHERE machine_id <> '';

enrollment_issued(request_id bytea PRIMARY KEY REFERENCES enrollment_requests, round bigint NOT NULL,
  serial bigint NOT NULL, host_id text NOT NULL, not_after bigint NOT NULL, certificate_der bytea NOT NULL)
enrollment_hosts(host_id text PRIMARY KEY, key_hash bytea NOT NULL, machine_id text NOT NULL, role text NOT NULL,
  services text[] NOT NULL, duplicate boolean NOT NULL, first_heartbeat_unix bigint,
  first_serving_unix bigint)              -- first_serving_unix: added by migration 0002 (first_serving)
enrollment_deny(host_id text PRIMARY KEY, state smallint NOT NULL,  -- 1 retiring, 2 retired, 3 cleared
  cleared_by bytea)
enrollment_serial(singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton), next bigint NOT NULL)  -- starts at 2
mac_nonces(nonce_sha256 bytea PRIMARY KEY, serial text NOT NULL, issued_unix bigint NOT NULL, consumed_by bytea,
  request_id bytea)                       -- the request it was issued for; none on a nonce written before the field
```

The columns are the fields of the store's protobuf messages (`store.proto` of `hostkeeper-enrollment`), one for one, so
an export writes the same messages back. An expired request keeps the waiting state it expired in and a non-zero
`expired_unix`, as its file does, so the export gives back the files the release before reads. A request, an issued
certificate, a host, a deny row and a nonce are never deleted, as today. The rules that are not a key (the pending cap,
a machine id or MAC already bound, one certificate per request and round, a host id another key holds) stay in the
enrollment crate's code, under its store lock, as today.

```sql
mdm_devices(enrollment_id text PRIMARY KEY,  -- 1 to 64 ASCII letters, digits and '-', NanoHUB's id
  record bytea NOT NULL)                     -- the file's bytes: schema, record, the record's SHA-256
```

**Built** (migration `0005`, `may-run`: an older release never reads the table). A row holds the record as the
gate's file holds it, the protobuf message the gate's own writer makes, and a load reads it with the gate's own
reader, unlike the typed columns of the other tables (section 14). The record is the gate's: its fields are added
by the gate's own additive rule (fields 18, 19 and 20 were added to schema 1 so the release before reads them),
and the gate's reader already refuses a record that is not whole or is of a newer schema and skips a field it does
not know, so the row is exactly as strict as the file, and a record moves between a file and a row byte for byte.
As columns, every field the gate adds would be a migration of this store, and a second reader of the same record
that could disagree with the gate's. A record of another enrollment id than its key is refused, as a file of
another name is. A save is one transaction on the writing session, fenced as every commit is
(`crates/hostkeeper-coordinator/src/store/postgres/devices.rs`); a save that fails, or whose outcome is not known,
is an error, and the gate does not take the record.

What a host's daemon last said it runs (packages-native.md section 10.6) is the field `running` of the host's file
(`hosts/<host id>.pb`, field 10 of the enrollment store's `Host`). On the database it is not a column of
`enrollment_hosts`, which would make the migration `refuse` and stop a rollback of the coordinator:

```sql
enrollment_host_running(host_id text PRIMARY KEY,  -- the host's row in enrollment_hosts
  running bytea NOT NULL)                          -- the encoded message hostkeeper.enrollment.store.v1.Running
```

**Built** (migration `0006`, `may-run`: an older release never reads the table nor touches it). A read of a host
joins the two tables; a write of a host upserts the row, or deletes it for a host with no record (an approval), in one
transaction. A row that does not decode is `Corrupt` at the start and for an export; `export` writes it into the file.

### 3.7 Bookkeeping

```sql
schema_version(version integer PRIMARY KEY, name text NOT NULL,
  older_releases text NOT NULL CHECK (older_releases IN ('refuse', 'may-run')),
  applied_unix bigint NOT NULL DEFAULT extract(epoch FROM now()))
lease(singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton), epoch bigint NOT NULL,
  commits bigint NOT NULL, holder text NOT NULL, since_unix bigint NOT NULL)   -- section 5, section 4.4
imports(id bigint PRIMARY KEY, at_unix bigint NOT NULL, manifest jsonb NOT NULL,   -- section 11
  audit_pruned_before bigint)                                                        -- section 3.5
```

`0001` inserts the one row of `lease` (epoch 0) and of `enrollment_serial` (next 2). `schema_version` is made by
the migration runner before the first migration, so a database of any release can be asked which migrations it
holds. A trigger of `0001` on `lease` (`lease_fenced`) refuses an update of the row from a session that does not
hold the coordinator's advisory lock (section 5), and every transaction of the coordinator updates it first, so the
fence holds at the database, not only in the coordinator.

## 4. Transactions and write order

### 4.1 The storage trait

The three stores the gate and API code call (`DesiredStore`, `OpsStore`, `VerdictFile`) keep their methods. What they
call underneath becomes one crate-private trait of the coordinator, with three implementations: the files of today,
Postgres, and an in-memory fake for the in-process tests.

```rust
/// Where the coordinator's state is durable.
pub(crate) trait Backend: Send + Sync {
    /// Everything, read once at start.
    fn load(&self) -> Result<Loaded, BackendError>;
    /// One change, durable when this returns Ok, or not at all.
    fn commit(&self, change: &Commit<'_>) -> Result<(), BackendError>;
    fn audit_page(&self, query: &AuditQuery) -> Result<Vec<Logged>, BackendError>;
    fn spend(&self, jti: &str, until_unix: u64, now_unix: u64) -> Result<(), BackendError>;
}

/// The state before and after one change, and its audit lines.
pub(crate) struct Commit<'a> {
    pub desired: Option<(&'a Held, &'a Held)>,
    pub ops: Option<(&'a OpsState, &'a OpsState)>,
    pub verdicts: Option<(&'a verdict_file::Held, &'a verdict_file::Held)>,
    pub audit: &'a [AuditLine],
}
```

A change is made as today: an edit runs on a clone of the state, under the store's writers' lock. The backend gets the
state before and after. The file backend writes the after state whole, exactly as today. The Postgres backend compares
the two in memory and writes only the rows that differ, so a change costs what it changes, not the size of the whole
state as a file rewrite does. The fake keeps a durable copy and fails the commit it is told to, which the crash and
refusal tests use in place of `ops_hooks.rs` and `with_writer`.

The enrollment store gets the same cut one level down. Its `Fs` port (`crates/hostkeeper-enrollment/src/fs.rs`,
**Built**) is file-shaped: names, renames, directory syncs and a `flock`. The store's use of it is replaced by a port of
records, `Records`, a public trait of the enrollment crate: load a request, list every request, put several records
in one commit, take the next serial, set a host's first heartbeat time and its duplicate mark. Its implementations are
the files (the code of today behind it, still on `Fs`), Postgres (in the coordinator, which owns the database
connection), and a fake. `Fs` itself stays: the CA key file is read through it (`read_private`, the owner and mode check
of `Ca::open`), and the file `Records` writes through it. Under Postgres the store's lock is its in-process mutex alone;
the advisory lock of section 5 keeps other processes off. Adding the public trait to the enrollment crate is a change to
its public interface, made in its own pull request (C2). **Built** (C2):
`crates/hostkeeper-enrollment/src/enroll/records/`, with `FileRecords`, `MemoryRecords` and the conformance suite
(`records::conformance`). Its list gives every request, not the requests of one state: the store checks every request
a list gives it, as a file reader does today, so a list by state would leave the other requests unread and unchecked.

### 4.2 What is one transaction

Each of these is one transaction, where today it is several file writes:

- **An operations change** (every caller of `change`, `try_change`, `decide_change`, `decide_changes` and `escape`): the
  rows it changes, across what are today the ops file and the halts file, and its audit rows. Today the audit line is
  appended first and the ops file written after, so a line can name a change whose write then failed (`ops.rs`,
  `holds-and-force.md`); in one transaction the line and the change land together.
- **A desired state and its registry.**
- **A verdict acknowledgement:** the standing and counted verdicts of one heartbeat.
- **A force:** the forced desired state and the force's operations entries (`force`, `unmark_bad`, `mark_bad`,
  `clear_halt`). Today the desired state is written first and put back by hand when the audit cannot be appended
  (`force.rs`), and that put-back can itself fail. It is the one flow whose call site changes: it passes both parts in
  one `Commit`.
- **An approval** (`approve_held`): the deny row made `cleared`, the host row, the request row and the Mac nonce marked
  consumed. Today the nonce is marked after the request, and `reconcile_mac_nonces` repairs a crash between the two at
  the next open; in one transaction there is nothing to repair. The reconcile stays for the file store.
- **An issue**, in two: the serial counter and the request's reserved serial; then, after signing, the issued
  certificate and the request made `issued`. Signing stays between the two, so a crash after the first retries with the
  same serial and no certificate is ever signed twice for a request and round (`issue.rs`, **Built**).
- **An expiry sweep:** every request it expires.
- **A deny or a retire:** the deny row and the request marked denied.
- **A heartbeat's enrollment write**, each one row, each rare: a host's first heartbeat time (`note_heartbeat`, once
  after each approval), its first serving time (`note_serving`, once after each approval) and its duplicate mark
  (`report_live`, once). `report_live` keeps its map of live machine ids in
  memory, as today, and touches the database only to set the mark.

These stay ordered sequences of transactions, because each partial state they can leave is already the safe side and is
finished by the next call, as today:

- **`set_desired`:** the operator's audit row (and CI's, for a set with targets), the desired state and its release, the end of a standing force, the lift of an updater's
  halt. A failed lift leaves the service halted.
- **`Hosts.Retire`:** the deny row, then the assignments and the `host_retire` row, then the retire. A failed middle
  step leaves the host refused, and a repeat finishes it.
- **An enrollment change and its audit row.** The enrollment crate depends on no other crate of the workspace, so the
  audit row is the coordinator's second transaction, as its second write is today; `audit_to` at start audits one that
  did not land.

### 4.3 The write orders, kept

The three orders of `ops.rs` encode one rule: a change that raises protection stands even if storage fails, and a change
that lowers it needs storage first. They are kept, with storage now the database:

- **Storage first** (`Write::BeforeMemory`, every change that does not raise protection, clears among them): the
  transaction commits, then memory takes the change. A failed commit leaves memory as it was and refuses the call.
- **Memory first** (`Write::AfterMemory` and `Write::Halt`: a halt or halt report raised, the coordinator's `mark_bad`,
  a `Rollouts.Pause`; and `Hosts.Abort`, which today stands without its audit line, `escape` in `ops.rs`): memory takes
  the change, then the transaction is tried. A failed one leaves the change standing in memory, **pending**; an abort's
  order is queued all the same.
- **Pending changes.** The store keeps the state as last committed beside the state in memory. While they differ, every
  heartbeat tries one commit of the difference with the audit rows not yet written, as `retry_unwritten` does for a halt
  today; the next storage-first change commits the difference first, in its own transaction, and is refused if that
  fails, so no change is ever committed ahead of an earlier one. This widens today's retry from halts to every
  memory-first change: today a pause whose write failed is retried only by the next change that writes.
- **The marker.** A process that dies with a pending change would start again without it. Before it tries the commit of
  a memory-first change, the store writes the file `--halt-marker-file` names (owner-only, with a directory `fsync`):
  the epoch and the audit numbers of every pending change, rewritten when one is added. Every memory-first change has an
  audit row, committed in the same transaction as the change, so the rows' presence proves the commit. The store removes
  the file when nothing is pending. At start, a marker whose every listed number is in the `audit` table is a change
  that landed before the crash: `serve` logs it, removes the file and starts. Otherwise it refuses to start, naming the
  numbers that did not land, and its error says to look them up in the log, make the change again by hand if it still
  matters (a halt is raised again by the host's next report while the host is up), and remove the file, as
  `HaltWriteUnfinished` says today. That refusal is deliberate: under `restart: unless-stopped` the container is
  restarted with Docker's growing delay and refuses each time until a person settles it, which is what today's marker
  does to a file coordinator. It is a file because it must be writable when the database is not. The file store keeps
  its own marker, `<ops-file>.halts.unfinished`, widened the same way by C1.

### 4.4 Errors, refusals and the session

- **One writing session.** Every write goes through one connection, the one that holds the lock (section 5), in the
  order the coordinator makes them. With one writer, `READ COMMITTED` is enough: no transaction can conflict with
  another, so there are no serialisation retries.
- **Synchronous calls.** The store's calls stay synchronous, made under the fleet lock as today. The blocking `postgres`
  client (jobscheduler's choice) panics when it is driven from inside a tokio runtime, so it never is: the writing
  session belongs to a thread of the store's own, and a call hands its transaction to that thread and waits for the
  answer. A heartbeat waits for a commit as it waits for an `fsync` today. hostkeeper-design.md section 11.5 states the
  cost.
- **Bounded waits.** A stalled database must not freeze every heartbeat under the fleet lock. The session is opened with
  `connect_timeout`, `statement_timeout`, `lock_timeout`, client TCP keepalives and `tcp_user_timeout` (values set by
  C3, each a few seconds), and a timeout is a lost session. While the session is lost the store thread answers every
  call at once with "lost" and reconnects on its own, never under the fleet lock. So a heartbeat waits at most one
  statement timeout, once, and then not at all until the session is back.
- **Reads.** `Audit.List`, `Enrollment.List`, the inventory's enrolled-host reads and the alerts' scans of requests run
  on a second, read-only connection, so a long list never holds a commit. They see committed rows only, which is
  stricter than a scan of files that may change underneath it, and they show a commit once it has returned. The
  heartbeat's enrollment reads (`host_role_for`, `is_denied`, `is_retiring`, `is_duplicate`), which read files today,
  read an in-memory copy of the hosts and the deny rows, loaded at start and changed only after a commit returns Ok, so
  a heartbeat costs no query. After a commit of unknown outcome (below) the copy is loaded again from the tables before
  the next heartbeat is answered.
- **A commit of unknown outcome.** A session lost while `COMMIT` was in flight may or may not have committed. Every
  transaction also sets `lease.commits` to one more than before (a counter column of the `lease` row), so after the
  reconnect of section 5, with the epoch its own, the store reads the counter: equal to the number of the lost commit,
  it landed; one less, it did not. A storage-first change whose commit was lost is refused `store_failed` at once (the
  caller cannot wait for a reconnect); if the counter then shows it landed, memory takes it and the log says so, as
  `StoreError::NotSynced` of `desired.rs` does today for a file renamed into place whose directory sync failed. A repeat
  of the call is then a no-op, as it is today. Until the outcome is known, the audit numbers of the lost commit stay
  reserved (section 3.5). The last-committed copy of section 4.3 and the newest audit number are set from the outcome
  before anything pending is committed.
- **Refusals.** A failed commit of a storage-first change is refused `store_failed`, as a failed ops-file write is
  today. For an operations change, a desired state, a force and a verdict acknowledgement, the audit row is in the same
  transaction as the change, so `audit_unavailable` (which keeps its proto value and its meaning) is never returned by
  the Postgres store: there is no case where the change is written and its line is not. `set_desired` writes its audit
  row first, as today (an operator's, and CI's with targets), so it cannot leave a change without its line. An enrollment change's audit row is the second
  transaction (section 4.2): when it fails, the change stands, the failure is logged, and `audit_to` writes the row at
  the next start, as today; an approval is never refused `audit_unavailable`. The runbook rows that speak of the audit
  file's disk (`enrollment.md`'s `audit_unavailable` row and its "free the disk of the audit file";
  `holds-and-force.md`'s paragraphs on a change refused when its line cannot be appended and on the three calls that
  stand without it) are rewritten by C3.
- **What the database's own settings must be.** At connect the store reads `fsync`, `synchronous_commit` and
  `full_page_writes` and refuses a database where any of them is `off`, naming the setting: the coordinator acknowledges
  what is committed, so a commit must be on disk when it returns. jobscheduler's store refuses `synchronous_commit` off
  for the same reason.

## 5. The single writer: an advisory lock

- **The order at start:** take the lock; read `schema_version` and refuse a newer schema (section 6); run the migrations
  the database lacks; raise the epoch; load. Nothing is read or written before the lock is held.
- **Taking it.** At start, before anything else, `serve` opens the writing session and calls `pg_try_advisory_lock` with
  the store's fixed key (a constant of the code that never changes). If another session holds it, the start is refused:
  `another coordinator holds the lock of this database`, the advisory form of `StartError::Locked`. It never waits.
  Advisory locks are per database, so two farms on one server do not meet.
- **The epoch.** After the migrations, in the same session, it runs one transaction that adds one to `lease.epoch` and
  records the holder (its first `--server-name`, or `coordinator` without one) and the time. The epoch it wrote is the
  coordinator's own.
- **Every write on that session.** A transaction can commit only on the session that holds the lock. That is the fence:
  a coordinator that lost its session cannot commit anything, because the lock went with the session.
- **Letting go.** A coordinator that ends (its store stopped or dropped) runs `pg_advisory_unlock_all` on the writing
  session before it closes it, so the lock is free when the store is gone and a successor takes it at once. Closing the
  session alone returns before the server has ended it and freed the lock.
- **Losing the session.** When a commit or a check finds the session gone (the server restarted, the network dropped,
  the server ended it), the coordinator:
  1. orders nothing from then on, as in its quiet start, and its status says it is quiet; it refuses every storage-first
     change `store_failed`; memory-first changes stand, pending; heartbeats are still answered and the fleet view is
     still kept; an alert, `store_unreachable` (a new alert kind, additive), stands until the session is back;
  2. its store thread opens a new session and tries the lock again, on its own and never under the fleet lock;
  3. with the lock, reads `lease.epoch` and `lease.commits`. Equal epoch: no other coordinator held the database
     meanwhile; it settles a commit of unknown outcome (section 4.4), commits what is pending (section 4.3) and orders
     again. Different epoch: another coordinator led meanwhile and its memory is stale; it logs every pending change it
     drops, by audit number, removes the marker (another coordinator's state now governs, so they are dropped
     knowingly), and exits non-zero; its supervisor starts it again on the database as that one left it.
- **At start, a database that is not up** is a refusal to start, named `not_up` or `refused` by SQLSTATE and I/O kind as
  jobscheduler's `OpenError` does, never by message. The compose file's `depends_on` with the health check and the
  restart policy start it again. There is no wait loop in the code (rule B20).
- **What goes.** `src/lock.rs`'s file lock is not taken with `--database-dsn-file`, and `<state-file>.lock`, the
  enrollment store's `<enrollment-dir>/lock`, the staged halts file and the restore marker have no counterpart. A backup
  of the database needs no lock against a running coordinator, because `pg_dump` reads one consistent snapshot of a live
  database (section 10). The `backup` and `restore` verbs are for the file store only and keep their lock.

## 6. Migrations

- **Where.** Numbered SQL files under `crates/hostkeeper-coordinator/src/store/postgres/migrations/`, `0001_state.sql`
  first, embedded in the binary. A test pins the SHA-256 of each shipped file and the list of numbers and names, so a
  shipped migration is never edited (jobscheduler's `migration_identity.rs`).
- **Each file says what an older release may do.** Its first line is `-- older-releases: refuse` or `-- older-releases:
  may-run`. A migration that only adds what an older release can ignore without acting wrongly (an index; a table only
  the new release reads) says `may-run`. Any other says `refuse`, which is the default a lint holds every new file to
  unless the line says otherwise.
- **When they run.** `serve` runs them at start, after it holds the lock (section 5) and before it loads anything, each
  in its own transaction with its `schema_version` row, so a migration that fails is rolled back whole and the start is
  refused, naming it. A fresh database gets every migration; a database behind gets the ones it lacks.
- **A newer schema.** A database that holds a migration this binary does not know is refused at start when any such
  migration says `refuse`: `the database's schema is newer than this coordinator (migration <n>, <name>)`. When every
  unknown one says `may-run`, the binary starts. This is the rule of the ops file today, which refuses a newer `schema`
  rather than ignore what it holds, kept for the migrations that need it.
- **Only additions.** A migration adds tables, columns with a default, indexes and constraints that existing rows meet.
  A lint refuses `DROP`, `RENAME`, a column type change, `TRUNCATE`, `DELETE`, a `NOT NULL` column without a default,
  and statements that end the transaction (`COMMIT`, `ROLLBACK`, `SAVEPOINT`), as jobscheduler's `schema.rs` lint does.
  Taking something away is a later release's own reviewed change, and is not designed here.

## 7. A standby, designed and not built

The lock and the epoch are the base of a standby, and C3 builds them. Nothing else below is built by the items of
section 13; the standby is listed there as a later item that is not scheduled, and its `--standby` flag and the
daemon-config key of option (a) would be additions to public interfaces, flagged in its own pull request.

- **The standby.** A second coordinator, on a second machine, runs `serve --standby` with the same flags, files and
  database. It opens its session and waits in `pg_advisory_lock`. It binds no listener while it waits, so no daemon ever
  reaches a coordinator that is not the leader.
- **Takeover.** The leader's session ends: the process died, its machine went down, or the server ended a session its
  keepalives found dead. The server releases the lock with the session. The standby gets it, raises the epoch, loads the
  state, binds its listeners and starts its quiet start: it orders nothing until the quiet start has passed, and
  rebuilds the fleet from the heartbeats of the daemons that move to it.
- **The old leader.** It finds its session gone at its next commit or check, orders nothing from then on, cannot take
  the lock back, finds the epoch raised if it ever does, and exits (section 5). It must find the loss before the new
  leader's quiet start ends. So a leader checks its session before it answers with an order whenever its last successful
  use of the session is older than a bound, and the server's keepalives end a dead session within a second bound; the
  two together must stay under the quiet start (30 seconds by default, `QUIET_START` in `fleet.rs`). The values are set
  by the standby's build item and checked at start against `--quiet-secs`.
- **How daemons find the leader.** Two options:
  - (a) **A list of coordinator addresses** in the daemon config, tried in order, the next on a refused or failed
    connection. An additive key of `hostkeeper.toml`, and the server certificate of each coordinator names its own
    address.
  - (b) **One floating address**, moved to the leader by a VRRP daemon on both machines. No daemon change.

  Recommendation: (a). The hosts dial the coordinator over the tailnet (network-install.md section 7), and a tailnet has
  no floating address; (a) also needs no network equipment and is testable in process.
- **What it costs.** Both machines hold the CA key (each coordinator makes its own server certificate from it). That is
  a second copy of the farm's most sensitive file. An attacker on the standby's machine gets what one on the leader's
  gets: the CA key, the operator tokens, the database password.

## 8. Connection configuration

- **`--database-dsn-file <file>`.** The file holds one libpq connection string in keyword form, for example
  `host=127.0.0.1 port=5432 dbname=hostkeeper user=hostkeeper password=<secret>
  application_name=hostkeeper-coordinator`. It is read with `hostkeeper_wire::secret::read_secret` (**Built**): a
  regular file, not a link, owned by the coordinator's user (65532 in the image), mode 0600 or 0400 exactly, at most 4
  KiB. The password is never on argv and never in the environment (AGENTS.md rule S2). No error and no `Debug` shows the
  string or its path: an error names the flag, as the token file's do.
- **TLS.** The first build connects without TLS, and refuses a string with `sslmode` other than `disable`, so a
  `require` is never quietly downgraded (jobscheduler's rule). In the compose deployment the database listens on the
  machine's loopback only (section 9), so nothing crosses a network. Moving Postgres to another machine is build item
  C10: a TLS connector over the workspace's `rustls` and `tokio-rustls`, the server's CA named by a flag, and
  `sslmode=verify-full` required for any host that is not loopback.
- **Flags beside it.** `--database-dsn-file` refuses `--state-file`, `--ops-file`, `--enrollment-dir`,
  `--audit-max-bytes` and `--audit-retained-segments`, naming the flag. It requires `--halt-marker-file`. Enrollment is
  on when `--ca-key-file` is given, which keeps requiring `--server-cert-file` and `--server-name`; the store is the
  database's. The MDM gate's flags need enrollment, `--ca-key-file` with `--enrollment-dir` or
  `--database-dsn-file`, and with the database its device records are the table `mdm_devices` (section 2). The file store stays selectable by `--state-file` for one release after the farm moves (section 11).

## 9. Deployment: compose

`docs/deploy/coordinator/compose.yaml` (build item C5, **Built**). As built,
both images are pinned by version and digest, the `postgres` service is optional behind the compose profile
`postgres` (a machine with a Postgres server already names that server in the connection-string file, by loopback
or by its socket directory mounted into the coordinator), and the coordinator waits for it only when it runs
(`required: false`). The runbook is `docs/deploy/coordinator/README.md`. The sketch:

```yaml
services:
  postgres:
    image: postgres@sha256:<digest of postgres:16, pinned by C5>
    network_mode: host
    command: ["postgres", "-c", "listen_addresses=127.0.0.1", "-c", "port=5432"]
    secrets: [hostkeeper-db-password]
    volumes:
      - hostkeeper-db:/var/lib/postgresql/data
      - ./sql:/etc/hostkeeper-db:ro      # init.sql and password.sql, the setup below; read by psql, not by the image
    healthcheck:
      test: ["CMD", "pg_isready", "-h", "127.0.0.1", "-p", "5432", "-U", "hostkeeper", "-d", "hostkeeper"]
      interval: 5s
      timeout: 5s
      retries: 12
    restart: unless-stopped

  coordinator:
    image: hostkeeper-coordinator:<version>    # loaded from the release's image tar, its digest checked first
    network_mode: host
    depends_on:
      postgres: {condition: service_healthy}
    command:
      - serve
      - --database-dsn-file
      - /run/secrets/hostkeeper-db-dsn
      - --halt-marker-file
      - /var/lib/hostkeeper-coordinator/halt-unfinished
      - --rollout-file
      - /etc/hostkeeper-coordinator/rollout.toml
      # ... the listen, TLS, token, OIDC, CA and server certificate flags of today
    secrets: [hostkeeper-db-dsn]
    volumes:
      - hostkeeper-coordinator:/var/lib/hostkeeper-coordinator      # CA key and cert, server cert, marker
      - ./etc:/etc/hostkeeper-coordinator:ro                        # rollout, tokens, OIDC, TLS files
    restart: unless-stopped

secrets:
  hostkeeper-db-password: {file: ./secrets/db-password}
  hostkeeper-db-dsn: {file: ./secrets/db-dsn}

volumes:
  hostkeeper-db: {}
  hostkeeper-coordinator: {}
```

- **Host networking for both.** `--private-listen` and `--lan-listen` (network-install.md section 7) each refuse a
  wildcard address and take one interface's address. In a bridge network the container has no
  such address, so the coordinator uses the host's network and binds the addresses it binds today. Postgres then listens
  on the host's loopback only, and is not reachable from any other machine.
- **The image.** The release's image tar is checked against its attestation
  ([verify-a-release.md](../deploy/verify-a-release.md)) and loaded with `docker load`, which names it by the release's
  version. It sets no environment variable (the packaging test of `packaging/container/Dockerfile` holds it to that),
  and the compose file sets none for it.
- **No environment, and the database made by an explicit step.** The compose file sets no environment variable for
  either container. The Postgres image's own first-start setup, which reads `POSTGRES_*` variables, is not used: the
  data directory is made once by `initdb` with its settings on the command line, and the image's entrypoint then finds
  it made and only starts the server. The one-time setup of the C5 runbook:

  1. `docker compose run --rm --no-deps --user postgres --entrypoint initdb postgres --pgdata=/var/lib/postgresql/data
     --username=postgres --auth-local=trust --auth-host=scram-sha-256`. The superuser `postgres` gets no password, so
     with `scram-sha-256` on TCP it cannot log in over the network at all; it is reached only through the container's
     own socket, by `docker compose exec`, which needs root on the machine already.
  2. `docker compose up -d postgres`.
  3. `docker compose exec -T postgres psql --username=postgres --file=/etc/hostkeeper-db/init.sql`, where `init.sql`
     and `password.sql` (committed beside the compose file by C5, both mounted read-only) are:

     ```sql
     -- init.sql
     CREATE ROLE hostkeeper LOGIN;
     \ir password.sql
     CREATE DATABASE hostkeeper OWNER hostkeeper;
     -- password.sql
     \set password `cat /run/secrets/hostkeeper-db-password`
     ALTER ROLE hostkeeper PASSWORD :'password';
     ```

     psql reads the password from the mounted secret itself, so it is never on a command line or in an environment.
  4. `docker compose up -d coordinator`: its first start runs the migrations (section 6).
- **The local volume** holds the CA key and its certificate, the server certificate file and the marker. The image runs
  as user 65532, which must own the key file (`Ca::open` refuses a key another user owns or others can read). The
  runbook of C5 says how to place them.
- **The two secrets** are files on the machine, outside the repository and the compose directory's version control.
  Without swarm, compose mounts a secret as a bind mount that keeps the host file's owner and mode, so `db-dsn` is
  owned by 65532 with mode 0400 (the rule of `read_secret`), and `db-password`, read only by the `cat` of
  `password.sql` inside the container as its `postgres` user, is owned by that user's uid with mode 0400 (999 in the
  pinned image, checked by C5). The password is in both. To change it: write the new password into both
  files, run `password.sql` again through the same `docker compose exec ... psql --file`, and restart the coordinator.
  C5's acceptance is the setup above on a fresh machine, ending in a healthy database and a coordinator that holds the
  lock.
- **Health.** The database has a health check and the coordinator starts only once it passes. The coordinator's image
  has no shell or client to run one, so the coordinator has no compose `healthcheck` and no `HEALTHCHECK`; it exits on a
  refusal and the restart policy starts it again (section 5). Its health is a route a probe outside the container asks
  instead: `serve --health-listen <loopback address:port>` answers `GET /healthz` with `200 ok`, or `503` and one of
  `starting`, `store_unreachable`, `not_writer` and `draining` (from SIGTERM until it exits; two-programs.md
  section 2.7, [coordinator-health.md](../deploy/coordinator-health.md)).

## 10. Backups and the restore drill

- **Schedule.** `docs/deploy/coordinator/hostkeeper-db-backup.service` and `.timer` (C5, **Built**), the unit running the installed xtask binary's `db-backup` (which numbers each dump past the highest
  there, keeps only a dump whose command succeeded and wrote something, and only then removes the oldest): every six hours, `docker compose exec -T postgres pg_dump --format=custom --username=hostkeeper
  --dbname=hostkeeper` (over the container's own socket) into a backup directory on the host, owner-only, keeping the
  last 28 dumps (one week). The coordinator keeps running: a dump is one consistent snapshot. Copy the directory off the
  machine, as for today's snapshots.
- **What is not in it.** The CA key and its certificate stay a sealed offline copy
  ([ca-backup.md](../operations/ca-backup.md)). The files of the coordinator's local volume and its configuration are
  copied once, as today's move procedure says.
- **Restore.** Into an empty database only: `pg_restore --single-transaction --exit-on-error`, so a restore that fails
  leaves nothing behind (today's restore marker has no counterpart). Then one statement moves the serial counter on by
  1,048,576 (`UPDATE enrollment_serial SET next = next + 1048576`), so certificates issued after the restore never take
  a serial that a certificate issued after the dump already had (today's restore documents that hazard and leaves it).
  It is a step of the runbook, before the start; the hazard is closed only when it is run. Then start the coordinator.
- **What a restore loses** is what today's restore loses after the last backup (coordinator-state.md, "Hosts enrolled
  after the last backup"): enrollments, operator changes, desired versions and spent token ids after the dump. Dumps
  every six hours narrow the window; continuous WAL archiving would close it, and is not designed here.
- **The drill**, monthly and after any change to the schema: restore the newest dump into a scratch database on the same
  server (`createdb hostkeeper_drill`, then the restore above); write a second connection-string file that names
  `dbname=hostkeeper_drill`; run `hostkeeper-coordinator export` with it into an empty scratch directory, which reads
  every row through the same readers a start uses, writes the file set, and prints one line per table with its row
  count; compare those with the counts `export` prints when run against the live database into another scratch directory
  (it takes the lock, so this is done in a window the coordinator is stopped for, or the live counts are read with
  `SELECT count(*)` per table instead); drop the scratch database. After a drill that fails, the dumps are not trusted
  until one passes.

## 11. The move: import, cutover and back-out

**Built** (C4): `crates/hostkeeper-coordinator/src/transfer.rs`, the database's side in
`crates/hostkeeper-coordinator/src/store/postgres/transfer.rs`, the enrollment store read and
written whole by `FileRecords::read_snapshot` and `Snapshot::files`
(`crates/hostkeeper-enrollment/src/enroll/records/files.rs`); the runbook is
[coordinator-state.md](../operations/coordinator-state.md). Where C4 decided what this section leaves
open: the ops file's audit ring entries that its audit file holds no line of are not imported (they
have no time), and `state_imported` is numbered past them; the MDM gate's device records
(`<enrollment-dir>/mdm-devices/*.pb`) are read by the gate's own reader and written as rows of
`mdm_devices`, and `export` writes them back as the same files; and a history the import found pruned is exported
as the sealed segment `<ops-file>.audit.2`, so the file store gives back the same
`audit_pruned_before`. An import also refuses flags that name no file, and says how many audit
lines it could not read and so did not import; `export` runs the migrations a start runs before it
reads.

**`hostkeeper-coordinator import`** takes the file flags of `serve` (`--state-file`, `--ops-file`, `--enrollment-dir`,
`--legacy-service`) and `--database-dsn-file`. A desired-state file in the single-state form is read as the state of
`--legacy-service`, as `serve` reads it. Without `--apply` it reads the files and the database, prints what it would
import table by table, and writes nothing. With `--apply`:

1. It takes the file lock of `<state-file>.lock`, so a running file coordinator refuses it, and the database's lock.
2. It refuses a marker (`.halts.unfinished`, `.restore-unfinished`), as `serve` does.
3. It opens every file with the strict readers `serve` uses, and refuses whatever `serve` would refuse.
4. It runs the migrations and refuses a database that holds any state row.
5. In one transaction: it writes every table from the loaded state, copies the whole audit history (every sealed segment
   and the active file, each line with its `seq` and `at_unix`), records the manifest of the files it read (name, size,
   SHA-256, as `backup` writes) and the audit files' `pruned_before` in `imports`, appends the audit row
   `state_imported` by `coordinator`, then loads the state back through the Postgres store and compares it with the file
   load. A difference rolls the whole import back and names the first part that differs. Otherwise it commits.

It changes no file: the file coordinator can start again on the same files.

**`hostkeeper-coordinator export`** takes `--database-dsn-file` and the file flags (`--legacy-service` among them) with
`--into-empty`. It takes the database's lock (so a running coordinator refuses it), loads the state, and writes it with
the file store's own writers into targets that hold nothing, as `restore` does: the desired-state file is in the
single-state form exactly when the file store's writer would choose it (the legacy service is the only service). It
prints one line per table with its row count, and the audit's line count. It is the way back, and the drill's reader.

**The cutover** is an operator-run step with a plan mode and an apply mode, kept outside this repository with the farm's
other operator scripts. Plan: check the compose stack, the secrets and the local volume; run `import` without `--apply`
against the live files; save the coordinator's status for the comparison. Apply:

1. Take a file backup (`hostkeeper-coordinator backup`) and stop the file coordinator.
2. Start the compose `postgres` service only; run `import --apply` in a one-off coordinator container with the old state
   volume mounted.
3. Start the compose `coordinator` service with the CA key, server certificate and configuration of the file
   coordinator. The daemons dial the same address and present certificates of the same CA, and every host row moved with
   its key hash, so no host enrolls again.
4. Check: every host that was live heartbeats again after the quiet start; the status equals the saved one; `Audit.List`
   shows `state_imported`.

**Back-out**, at any time: stop the compose coordinator, run `export --into-empty` into new paths, and start the file
coordinator there. Changes made on the database are kept. If nothing changed since the import, the untouched original
files serve too. Before a second cutover, drop and create the database again (`dropdb` and `createdb` through `docker
compose exec`, or a new volume), because `import` refuses a database that holds state, and says so in its plan mode too.
The file store stays in the release for one release cycle after the move; build item C9 deletes it once the farm has run
on the database and the back-out window has closed.

**Compatibility (rule B8).**

- New release on old data: `import` reads every file the release before writes, the recorded fixtures of
  `crates/hostkeeper-coordinator/tests/fixtures/recorded` among them. Test:
  `import_reads_every_recorded_file_of_the_release_before`.
- Release before on new data: it cannot read a database, and is not asked to. `export` writes the files it reads, and
  the strict readers of the release before (`tests/support/readers.rs`, `strict-readers.json`, **Built**) read them.
  Test: `exported_files_are_read_by_the_strict_readers_of_the_release_before`. Running the binary of the release before
  in a test would fetch a release, which rule T12 forbids; these readers are its pinned copy, as `tests/state_files.rs`
  uses them today.
- Release N+1 on release N's database: migrations are additions (section 6). Release N on N+1's database: refused when a
  newer migration says `refuse`. Test: `a_migration_marked_refuse_stops_an_older_release_and_may_run_does_not`.

## 12. Tests

- **The in-process tests keep a fake** behind the same trait (section 4.1): every test that builds a coordinator with
  `OpsStore::volatile`, `with_writer`, `VerdictFile::volatile` or a scratch directory builds it on the fake, which fails
  the commit it is told to. No in-process test needs a server.
- **One conformance suite** over `Backend` and over `Records`, generic over the implementation, run against the fake and
  the files from C1 and C2, and against Postgres (its `Backend` and its `Records`) from C3: a commit is all or nothing;
  a load gives back exactly what was committed, list order and defaults included; a failed commit leaves the durable
  state as it was; audit numbering continues across restart; `audit_page` answers every filter, `since_seq` and `newest`
  alike; a spent id is refused again until it is past; approval and issue keep one certificate and one serial per
  request and round. Its negative cases run it against backends that are wrong in one way each (a commit that writes
  half, a load that drops order, a spend that forgets) and expect the failure of that clause, so the suite is shown to
  catch what it claims (rule T15).
- **Postgres in CI, as jobscheduler runs it.** The test jobs (now each `test-shard` of ci.yml) carry a `postgres` service container pinned by
  digest, with trust authentication on loopback and no password; no new job (rule B9). A GitHub service container
  takes no start command, so the trust setting is the image's own variable, `POSTGRES_HOST_AUTH_METHOD: trust`, in the
  workflow's `services:` block: a setting of a throwaway third-party container that lives for one job, as jobscheduler's
  CI sets it, and no configuration of hostkeeper's code. The deployment sets no such variable (section 9). The
  tests read the server's URL
  from one variable the job sets, `HOSTKEEPER_TEST_POSTGRES_URL`, through one function of the test support with an
  `#[expect(clippy::disallowed_methods, reason = ...)]`; no product code reads the environment. A test that needs the
  server fails when the variable is unset, with a message saying how to start one; it never skips. Each test creates its
  own database with a unique name and drops it, so tests running in parallel share nothing (rule T12). The workflow lint
  gains one rule, that a service image is pinned by digest, with its negative case.
- **No timing.** Expiry and the audit's times come from injected clocks. The loss of the session is made by
  `pg_terminate_backend` from a second session, a failed commit by a trigger the test installs, a refused setting by
  `ALTER DATABASE ... SET`; no test sleeps or bounds an elapsed time (rule T10).
- **Coverage.** The Postgres store is a module of the coordinator crate, measured by the same gate at 99% of lines and
  branches in the check `test`, over the test shards, each of which has the server. Nothing is excluded or feature-gated out of the measurement.
- **Named tests**, by build item:

| Test | Proves | Item |
|---|---|---|
| `an_entry_at_its_default_reads_as_absent` | the code reads a missing per-service entry as its default, so not storing it changes nothing | C1 |
| `a_failed_commit_leaves_memory_and_storage_as_they_were` | storage first: refused `store_failed`, nothing changed | C1 |
| `a_raised_halt_stands_when_its_commit_fails_and_lands_at_the_next_heartbeat` | memory first and the pending retry | C1 |
| `a_pending_change_is_committed_before_the_next_change` | no change is committed ahead of an earlier one | C1 |
| `a_pause_whose_commit_failed_is_retried_at_the_next_heartbeat` | the widened retry | C1 |
| `serve_refuses_to_start_while_the_halt_marker_is_there` | the marker | C1 |
| `a_force_lands_whole_or_not_at_all` | force's desired state and entries in one commit | C1 |
| `an_approval_and_its_nonce_land_together` | the approval's records in one commit | C2 |
| `the_records_conformance_suite_passes_on_the_files_and_the_fake` | the `Records` contract | C2 |
| `a_marker_whose_changes_all_landed_is_removed_at_start` | the marker settles itself when the audit rows prove the commit | C1 |
| `a_refused_change_takes_no_audit_number` | the numbering stays gap-free across a failed commit | C1 |
| `the_conformance_suite_passes_on_the_files_the_fake_and_postgres` | one contract, three backends | C1, C3 |
| `the_suite_fails_against_a_backend_that_breaks_each_clause` | the suite catches what it claims | C1 |
| `a_second_coordinator_on_the_database_is_refused` | the advisory lock | C3 |
| `a_lost_session_holds_orders_and_resumes_on_the_same_epoch` | section 5, the reconnect | C3 |
| `a_lost_session_exits_when_another_coordinator_led_meanwhile` | the epoch fence | C3 |
| `a_commit_on_a_session_without_the_lock_fails` | the fence holds at the database | C3 |
| `a_commit_that_landed_before_its_session_was_lost_is_taken_into_memory` | a commit of unknown outcome, settled by `lease.commits` | C3 |
| `a_commit_that_did_not_land_before_its_session_was_lost_is_not_taken` | the other outcome | C3 |
| `a_stalled_statement_is_a_lost_session` | the bounded wait | C3 |
| `the_different_epoch_exit_removes_the_marker_and_names_what_it_dropped` | the exit of section 5 | C3 |
| `store_unreachable_stands_while_the_session_is_lost` | the alert | C3 |
| `the_enrollment_copy_is_reloaded_after_a_commit_of_unknown_outcome` | the copy never disagrees with the tables | C3 |
| `the_first_heartbeat_and_the_duplicate_mark_are_written_once` | the heartbeat's enrollment writes, the first serving heartbeat among them | C3 |
| `a_fresh_database_gets_every_migration_in_order` | migrations at start | C3 |
| `a_migration_that_fails_is_rolled_back_whole_and_stops_the_start` | one transaction per migration | C3 |
| `a_migration_marked_refuse_stops_an_older_release_and_may_run_does_not` | the newer-schema rule | C3 |
| `the_shipped_migrations_are_unchanged` | the SHA-256 pins | C3 |
| `the_migration_lint_refuses_a_drop_and_a_rename` | only additions | C3 |
| `a_database_with_synchronous_commit_off_is_refused` | durable commits | C3 |
| `the_audit_table_refuses_update_delete_and_truncate` | append-only | C3 |
| `audit_list_answers_the_same_from_files_and_from_the_database` | the query equals today's reader | C3 |
| `a_dsn_file_others_can_read_is_refused` | the secret file's rule | C3 |
| `a_refused_connection_names_neither_the_password_nor_the_file` | the error text, by exact equality | C3 |
| `a_dsn_with_sslmode_require_is_refused` | no quiet downgrade | C3 |
| `the_database_flag_refuses_the_file_flags` | one store at a time | C3 |
| `a_heartbeat_in_the_steady_state_runs_no_query` | decisions read memory | C3 |
| `import_reads_every_recorded_file_of_the_release_before` | new release on old data | C4 |
| `import_copies_the_whole_audit_history_with_its_numbers` | sealed segments included | C4 |
| `import_refuses_a_database_that_holds_state` | one-time import | C4 |
| `import_refuses_while_a_file_coordinator_holds_the_lock` | the files are quiet | C4 |
| `import_rolls_back_when_the_read_back_differs` | the comparison | C4 |
| `import_without_apply_writes_nothing` | the dry run | C4 |
| `exported_files_are_read_by_the_strict_readers_of_the_release_before` | release before on new data | C4 |
| `export_refuses_targets_that_hold_state` | no file written over | C4 |
| `import_reads_a_single_state_desired_file_as_the_legacy_service` | the legacy form | C4 |
| `export_writes_the_single_state_form_only_for_a_lone_legacy_service` | both forms | C4 |
| `import_of_a_pruned_history_adds_no_audit_rows_at_first_start` | `audit_pruned_before` survives the move | C4 |
| `export_prints_the_count_of_each_table` | the drill's numbers | C4 |
| `an_accepted_write_raises_the_generation_even_for_an_identical_set` | the proto's generation | C6 |
| `a_stale_if_generation_is_refused_generation_conflict_and_changes_nothing` | the precondition | C6 |
| `the_write_id_and_the_replaced_state_come_back_in_the_record` | `SetDesiredResponse.record` | C6 |
| `an_older_release_is_refused_on_a_database_with_0003` | `0003` says `refuse` | C6 |
| `a_service_image_not_pinned_by_digest_is_refused` | the workflow lint's new rule | C3 |
| `the_device_store_cases_pass_on_postgres` | the gate's `DeviceStore` cases, the list the file store passes, on `mdm_devices` | M8 |
| `the_cases_fail_against_a_store_that_breaks_each_one` | the gate's case list catches what each case claims | M8 |
| `the_release_before_starts_on_a_database_with_0005_and_this_one_on_its` | `0005` says `may-run`, both ways | M8 |
| `the_gate_on_the_database_keeps_its_records_in_mdm_devices` | the gate on `--database-dsn-file`, across a restart | M8 |
| `a_device_row_the_gate_cannot_read_stops_the_start` | a row is as strict as a file | M8 |
| `import_writes_each_device_record_file_as_a_row_of_mdm_devices` | the import of `mdm-devices/*.pb` | M8 |
| `export_writes_the_device_records_back_as_the_files_they_were` | the back-out keeps the device records, byte for byte | M8 |

## 13. Build items, one pull request each, in this order

| # | Item | After | Size |
|---|---|---|---|
| C1 | The coordinator's `Backend` trait and its fake; the file store behind it with no change of behaviour; the in-process tests on the fake; the widened pending retry and the file store's marker (`<ops-file>.halts.unfinished`) for every memory-first change; force as one commit | none | L |
| C2 | The enrollment crate's `Records` port (a public trait) with its files and fake implementations, which the store uses in place of `Fs` (kept for the CA key and the file `Records`) | none | M |
| C3 | The Postgres store: migrations `0001`, the lock and the epoch, the writing thread and the read connection, the in-memory enrollment copy, `--database-dsn-file` and `--halt-marker-file`, the `store_unreachable` alert, the workspace dependency `postgres` (its licences checked by `cargo deny`), the service container in the `test` job and the workflow lint's rule; the operator docs that describe the store (coordinator-state.md, holds-and-force.md, assignments.md, rollout-plans.md, enrollment.md, enrollment-coordinator.md) | C1, C2 | L |
| C4 | `import` and `export` | C3 | M |
| C5 | `docs/deploy/coordinator/`: the compose file with both digests and no environment, `init.sql`, `password.sql` and the one-time setup, the backup timer, the runbook of the local volume, the secrets, the restore and the drill | C4 | S |
| C6 | Generations and write ids: migration `0003` (`refuse`), `if_generation` and `write_id` honoured, `SetDesiredResponse.record` filled | C3 | M |
| C7 | The cutover, run by the operator with its plan, apply and back-out, outside this repository | C5 | S |
| C8 | `docs/report` and the README's state sentences, once the farm runs on the database | C7 | S |
| C9 | Delete the file store, its flags and `backup`/`restore`, after the back-out window, with the supersession check | C7 | M |

Later, not scheduled: **C10**, TLS to a Postgres on another machine (section 8), when the database leaves the
coordinator's machine; **C11**, the standby of section 7, with its own tests and its interface additions. Each gets its
tests in its own pull request.

## 14. Decisions

| Question | Decision | Why |
|---|---|---|
| One database or one per store | One database, one schema. | The flows that cross stores (force, retire) can then commit together, and one lock covers everything. |
| What decisions read | Memory, as today; the database is read at start, by lists and by the import. | A heartbeat costs no query, and the gate's code does not change. |
| How a change reaches the database | The state before and after, compared in memory; only changed rows are written. | The stores' call sites and edits stay as they are; a change costs what it changes. |
| Typed columns or the records' encodings as blobs | Typed columns, with `jsonb` only for the values that are JSON today; the MDM gate's device records alone are their file's bytes (section 3.6). | The tables can be queried and constrained; each JSON value keeps its reader's strictness. The device record is the gate's own format, versioned by the gate's additive rule, and its reader is already as strict as a row must be. |
| Generations and write ids | Columns added by C6 with the API that honours them; not in `0001`. | They are what the operator API already describes; adding them before anything maintains them would store numbers that mean nothing. |
| The audit's numbers | Assigned by the coordinator under its writers' lock, not by a sequence. | One writer; no gaps from rolled-back transactions; the import keeps the file numbers. |
| Audit retention | Keep every row; the rotation flags stay with the file store. | The history is small in rows, and keeping it removes the "pruned before" cases. |
| The audit's actor | Derived from `by_name`, not stored twice. | One column cannot disagree with itself. |
| `audit_unavailable` | Kept in the proto, not returned by the Postgres store; an enrollment change's line still lands after it, repaired at start. | For every other change the line and the change are one transaction; there is no case to name. |
| The marker | A file, `--halt-marker-file`, listing the pending audit numbers; removed at start when they all landed. | It must be writable when the database is not, and a crash after the commit needs no person. |
| A commit of unknown outcome | Settled by the `lease.commits` counter after the reconnect. | One read tells whether the lost commit landed. |
| Retry of memory-first changes | Every heartbeat, for all of them, not only halts. | One rule; a pause that failed to write no longer waits for an unrelated change. |
| Losing the writing session | Order nothing, retry the lock at each heartbeat, resume on the same epoch, exit on another. | Safe whether or not a standby exists; a blip of the database does not need a person. |
| A database not up at start | Refuse to start; the supervisor starts it again. | No wait loop in the code (rule B20). |
| A newer schema | Refused when a newer migration says `refuse`, the default; started when all say `may-run`. | A release must not run on a schema whose newer parts it cannot work with, and must not be refused over a harmless addition such as an index. |
| Migrations | Run by `serve` at start, under the lock. | One writer, so no installer race; the farm has no installer step for the coordinator. |
| Approval and its audit row | Two transactions, with `audit_to` at start, as today. | The enrollment crate depends on no other crate of the workspace. |
| Client | The blocking `postgres` crate on the store's own thread. | The stores' calls are synchronous under the fleet lock; jobscheduler runs the same client. |
| How the database is made | `initdb` with explicit flags, then `init.sql` and `password.sql` through psql; the image's `POSTGRES_*` setup is not used. | Configuration is never passed by environment (AGENTS.md rule S2), and the password never leaves its file. |
| Networking of the containers | The host's network for both; Postgres on loopback. | The listeners bind one interface's address by rule; the database stays off the network. |
| Tests' server URL | One environment variable read by the test support only, failing when unset. | jobscheduler's pattern; product code reads no environment. |
| The file store after the move | Kept for one release cycle, then deleted (C9). | It is the back-out, and `export` writes through it. |
| Serial after a restore | Moved on by 1,048,576. | Today's restore can reuse serials; this one cannot. |
