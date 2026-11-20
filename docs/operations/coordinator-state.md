# The coordinator's state: where it is, backing it up and restoring it

The coordinator keeps its durable state in one of two stores, chosen by the flags of `serve`. The
file store is for one process: its lock keeps a second coordinator off the same files. The database
store may be shared by several coordinators at once ([Several coordinators](#several-coordinators-on-one-database)
below):

- **The file store**: files named by three flags, `--state-file`, `--ops-file` (by default the state
  file's name with `.ops` added) and, when it is the hosts' CA, `--enrollment-dir`.
  `hostkeeper-coordinator backup` copies all of it into a snapshot directory, and
  `hostkeeper-coordinator restore` puts a snapshot back. Both take the same flags `serve` names the
  state by. Most of this page is about the file store.
- **The database store**: one Postgres database, named by `--database-dsn-file`, with
  `--halt-marker-file` beside it ([The database store](#the-database-store) below). `backup` and
  `restore` are for the file store only.

`hostkeeper-coordinator export` writes the database's state back into files
([Moving from the database to the files](#moving-from-the-database-to-the-files) below). There is
no verb that moves a file store into a database.

The CA's key is not part of the state in either store and never in a snapshot: back it up once,
sealed and offline, as [ca-backup.md](ca-backup.md) says.

## The database store

`serve --database-dsn-file <file> --halt-marker-file <file>` keeps everything the files of the file
store hold in the tables of one Postgres database (16 or newer), in the schema `hostkeeper`: the
desired state and its registry of digests, the spent CI token ids, the verdicts, the operator's users
(each with the Argon2id hash of its password, never the password) and the API tokens made in the UI
(each with the SHA-256 of the token, never the token), every operator
change (exclusions, labels, halts and their reports, holds, assignments, minimum versions,
the repository `Services.SetRepository` or `Services.Put` set for a service (a service with one is a registered package, [hostgroups.md](hostgroups.md#the-registry-of-packages)), uninstall intents, the inventory, the marks of last good and bad versions, the plan positions),
the host groups (each group's packages, policy, generation and desired versions, and each (group,
package)'s minimum version, marks and plan position), the whole audit log, and the enrollment store. The design is
[coordinator-storage.md](../design/coordinator-storage.md).

- **The connection string** is the one line of the file `--database-dsn-file` names, a libpq
  string in keyword form: `host=127.0.0.1 port=5432 dbname=hostkeeper user=hostkeeper
  password=<secret> sslmode=disable`. The file must be a regular file (not a link) owned by the
  coordinator's user, mode 0600 or 0400, at most 4 KiB. The password is never on the command line or
  in the environment, and no error shows the string or the file's path: an error names the flag.
  This release connects without TLS, so a string with `sslmode` `require`, `verify-ca` or
  `verify-full` is refused. The coordinator does not check where the host is: a host other than
  the machine's own loopback is connected to as well, with the password in the clear, so keep the
  database on the coordinator's machine, on loopback.
- **Flags beside it.** `--database-dsn-file` is refused beside `--state-file`, `--ops-file`,
  `--enrollment-dir`, `--audit-max-bytes` and `--audit-retained-segments`, each named
  (`--database-dsn-file and --ops-file are both given: ...`), and it requires `--halt-marker-file`.
  Enrollment is on when `--ca-key-file` is given (with `--server-cert-file` and `--server-name`, as
  with the file store); the enrollment store is the database's.
- **The start.** `serve` opens one session, takes the coordinator's advisory lock of the database
  shared and without waiting (several coordinators hold it together), runs the migrations the database lacks (each in its own transaction, recorded in
  `schema_version`), raises the lease's epoch, makes its row of the `instances` table, loads the
  state, and opens the rest of its writing sessions (`--database-pool`, below), each holding the
  lock shared too. Nothing is read or written before the lock is held. Each migration runs with
  `lock_timeout` set to `--migrate-ddl-lock-timeout-ms` (default 3000, 1 to 4999): a statement of it
  that waits longer for a lock, behind a running coordinator's transaction, rolls the whole migration
  back, so the running coordinator's statements queued behind it wait no longer than that. The start
  then lets go of the migrations' lock, waits `--migrate-retry-secs` (default 5) and tries again from
  that migration, `--migrate-attempts` times in all (default 6). It refuses to start, naming why,
  when:
  - the database is not up (`the database is not up: ...`): nothing listens, or it is starting up.
    The supervisor starts the coordinator again; the code has no wait loop;
  - the server refuses (`the database refused the coordinator: ...`): a login, a database that is
    not there, or a setting. A database with `fsync`, `synchronous_commit` or `full_page_writes`
    off is refused, naming the setting: the coordinator acknowledges what is committed;
  - a session holds the lock exclusively (the release before this one, an `import` or an `export`):
    `another coordinator holds the lock of this database`;
  - the database holds a migration this release does not know, and that migration says older
    releases must not run on it: `the database's schema is newer than this coordinator (migration
    <n>, <name>)`;
  - a migration fails: it is rolled back whole, and the start names it;
  - a migration could not have its locks at any of its attempts: `migration <n> (<name>) could not
    have its locks within its lock timeout in <attempts> attempts, each rolled back: ...`.
- **The writing sessions.** Every write goes through one of the coordinator's writing sessions
  (`--database-pool`, default 4, at least 1), each holding the lock: a write takes a free session,
  and waits only while every one is busy. A Postgres that serves N coordinators needs N x
  (`--database-pool` + 1) connections. Each transaction first inserts its row of the `tx_ledger`
  table, which names it if its commit is lost. A coordinator that lost any of its sessions commits
  nothing, on any of them, until the next heartbeat's revival has opened every lost one again with
  the lock. Reads that are not decisions
  (`Audit.List`, the enrollment lists, the alerts' scans) run on a second, read-only session.
  A heartbeat is decided in one transaction, and its host's enrollment and deny rows are read
  fresh ("A heartbeat's decision", below), so a deny another coordinator recorded is seen at once.
- **The fleet's rows.** Every heartbeat a coordinator answers is kept, in one statement on a writing
  session and outside a transaction, in the tables of migration `0024`: the host's row of
  `fleet_hosts` (the heartbeat as read, the database's time it was heard, the host's own clock in
  it and the instance that heard it), a row of `fleet_bakes` for each service the heartbeat names
  (the healthy beats in a row at one version, the bake of `docs/design/host-updates.md`), and the
  removal of the host's row of `fleet_orders` for a service whose ordered version it reports. A
  heartbeat older by its host's clock than the one stored is dropped unless the stored one is
  `--beat-reorder-secs` old (default 30, 1 to 44): two coordinators can receive one host's
  heartbeats out of order. When the newest row of `fleet_hosts` is 45 seconds old or older, or
  there is none (a database restart, an outage, a first start), the statement raises
  `cluster_quiet.until_unix` to `--quiet-secs` past its moment. A heartbeat whose row cannot be
  written is answered all the same (`a beat is not kept` in the log). The decision reads these
  rows (next bullet).
- **A heartbeat's decision.** Over the database a heartbeat is decided in one transaction
  (`store/postgres/decide.rs`, `store/decision.rs`). It takes the row of `rollout_gate` without
  waiting: a gate held, or asked for by an operator's edit that waits, leaves the host told
  nothing new until its next heartbeat. Under the gate it reads again every part another
  coordinator changed since this one read it, and the fleet's rows, so the decision is the same
  whichever coordinator hears the host. The halts, the stages entered, the marks and rollbacks
  it makes, and the orders it gives (each written against the row as read) are written in that
  transaction, and a decision that cannot be made or committed gives no order: the host keeps
  what it runs and asks again. The quiet time after a start is read from `cluster_quiet`, so it
  holds for the whole database and not for one process. The services waiting for their turn to
  roll are ordered by `desired.set_seq` and `group_desired.set_seq`, taken in the transaction
  that sets a version, so every coordinator orders them alike. Over the files there is one
  process, nothing to take, and the heartbeat is decided over the fleet in its memory.
- **Edits, and the copies in memory.** Each change of the state (an operator's, CI's, or one the
  coordinator makes itself) is one transaction that locks, after its row of `tx_ledger`, the row
  of `rollout_gate` when it changes what a rollout decision reads (the desired state, the
  operations state, the layout, the verdicts), then the row of each part of the state it changes
  in `state_versions`, then the row of `audit_counter` when it writes audit lines; it reads afresh
  any part another coordinator changed since this one read it, makes the change on that, writes the
  rows that differ, and moves each part's version on. An edit waits for the gate at most
  `--operator-gate-wait-ms` (default 30000), and is refused `store_failed` with nothing changed
  when the wait runs out; a heartbeat's own write (its verdicts) never waits, and is made at a
  later heartbeat when the gate is held. Audit lines are numbered by `audit_counter` in the order
  their transactions commit, with no gap; each start sets it past the newest line, which a release
  before this one numbered without it. Every request, a heartbeat among them, first reads the
  parts' versions (one query on the read-only session) and reads again, in one snapshot, each part
  whose version moved on; otherwise decisions and answers read memory. A host's facts generation
  is `enrollment_hosts.facts_generation`, raised to the audit number of the change that changes
  the host's facts, in its transaction.
- **The instances.** Each start makes a row of the `instances` table with an id of its own (random),
  the time it started and was last seen (the database's clock), its release, and the addresses of
  its listeners, and moves `seen_unix` on every `--instance-beat-secs` (default 5) while it runs,
  making the row again if it is gone. It says which coordinators serve on the database and since
  when (`SELECT * FROM hostkeeper.instances ORDER BY seen_unix DESC`); no decision reads it, and the
  row of a coordinator that stopped stays as it was last seen until the job `prune` deletes it, a
  day after it was last seen.
- **Periodic jobs.** Every `--job-tick-secs` (default 5) a coordinator claims the periodic jobs that
  are due, one statement over the `periodic_jobs` table (migration `0025`) that moves each claimed
  job's due time on by its period, so of all the coordinators on the database one runs a job in a
  period; a coordinator that stops after a claim loses that run, and the job is due again one
  period later for any of them. The periods are `--job-every NAME=SECS` (repeatable): `alerts`
  (default 15) observes the alerts, and the coordinator whose insert into the `alerts` table made
  an alert's row logs its `alert=` line, once (a cleared alert's row is deleted, so one that comes
  back is logged again; the `alert=` lines are in the logs of the service, not of one instance);
  `request-expiry` (default 15) expires the enrollment requests no operator approved in time, by
  a write that lands only on the requests as read, and audits each `request_expired` once; `prune` (default 3600) deletes
  the expired sessions, the failure counts the limiter forgot, the `instances` rows not seen
  for a day and the orders of `fleet_orders` that aged out (120 seconds after they were given). `xcode-feed` (default 86400, claimed only with `--xcode-feed-url`) reads the list of Xcode releases and replaces the table `xcode_releases` with it; a read that fails is a warning in the log, and the job is due again a day later. Over files the alerts are observed every 15 seconds and the requests expire at the
  `--tls-reload-secs` tick, as before. `SELECT * FROM hostkeeper.periodic_jobs` shows each job's
  next due time and the instance that ran it last.
- **Generations and write ids.** The database keeps, with each service's desired state, its
  generation (one more for every accepted set, a set identical to the state held included, and a
  force), the write id of the last set (`hk releases set-desired --write-id`, 1 to
  128 visible ASCII characters), and the version and minimums the last change of version replaced.
  `Releases.SetDesired` answers them as its `record` and `Releases.Converge` shows them, so a writer
  that lost an answer can look up whether its write landed. A set with `--if-generation <n>` is
  taken only while the service's generation is `n`; otherwise it is refused `generation_conflict`
  (gRPC `ABORTED`) and nothing changes, the operator's audit entry included. The JSON route
  `POST /v1/desired` takes neither, and raises the generation like any set. Migration `0003`
  adds them and says older releases must not run on it: a coordinator of the release before is
  refused on a database that has it (`the database's schema is newer than this coordinator
  (migration 3, generations)`). The file store keeps none of them: there a set that names
  `--if-generation` or `--write-id` is refused `invalid_request`, and a set identical to the
  state held writes nothing.
- **Host groups.** Migration `0008` adds the tables of the host groups (`host_groups`,
  `host_group_packages`, `group_desired`, `group_min_versions`, `group_services`,
  `group_rollouts`, `group_rollout_done`) and a `host_group` column to `enrollment_hosts` (with
  `moving_from` and `facts_generation`), `enrollment_requests` (`approved_host_group`), `inventory`
  and `audit`; a row written before has `NULL` there and is read as naming no group. While the
  database holds a group the coordinator places hosts by groups ([hostgroups.md](hostgroups.md)),
  and one that holds none places no host (there is no built-in group, migration `0018`, below):
  there is no placement by roles. `0008` says older releases must not run on it:
  a coordinator of the release before is refused on a database that has it (`the database's
  schema is newer than this coordinator (migration 8, host_groups)`), so going back below this
  release is restoring the dump taken before it. The tables of placement by roles stay as they
  are, and are not read once a group is held (moving a fleet to groups leaves them, as the ops file
  keeps its `assignments`).
- **A service's repository.** Migration `0009` adds the column `services.repository` (text, `NULL`
  for the default, `komira`): the repository `Services.SetRepository` set for the package
  ([hostgroups.md](hostgroups.md#where-a-packages-repository-and-minimum-version-come-from)). Older
  releases may run on it (`may-run`): they never read the column, and their writes of a service's
  row leave it as it is (a row an older release deletes takes the value with it).
- **A service's trusted publishers.** Migration `0014` adds the column `services.trusted_publishers`
  (jsonb, `NULL` until an operator's `Services.PutTrustedPublisher` or `Services.RemoveTrustedPublisher`
  first writes the package's list): the GitHub Actions runs that may set the package's desired
  version ([ci-client.md](../deploy/ci-client.md#trusted-publishers)). A package with `NULL` is
  trusted as the allow-list file says; a list the state holds, empty or not, wins over the file.
  Older releases may run on it (`may-run`): they never read the column, and their writes of a
  service's row leave it as it is, so a release that is rolled back reads CI's allow-list as the
  file says it. The file store writes the key `trusted_publishers` of the service in `--ops-file`
  only once a list is held, so a file of a coordinator that never edited one reads as it always
  did (a release before this one refuses the key); `backup` and `restore` carry it with the ops
  file, and the database export and import carry the column.
- **Users and API tokens.** Migration `0010` adds the tables `users` and `api_tokens`, one row each
  per user and per token, keyed by name, holding the record as the accounts file holds it
  (`docs/design/operator-login.md`, komira-ai/hostkeeper#300, sections 6, 7 and 9). A database that holds
  no row holds no user and no token, which is how a coordinator of this release starts on a
  database the release before migrated. `0010` says older releases may run on it: the release
  before never reads either table, so going back below this release needs no restore for it.
  The login, the initial-admin file and the `Users` and `ApiTokens` services write these rows; `export` writes them to the
  accounts file.
- **The lab's network devices.** Migration `0016` adds `lab_devices`, one row per switch, keyed by id (`net.<n>`), holding the
  record as the layout file holds it under `devices` (name, role, console host and device, management MAC, notes;
  `docs/design/network-gear.md`). A database that holds no row holds no device. `0016` says older releases may run on it
  (`may-run`): they never read the table. A device is placed by the inventory subject of its id (`inventory:net.<n>`) in
  `lab_placements`. The `Layout` calls `PutDevice` and `RemoveDevice` write the row; `export` writes the devices into the layout file.
- **The lab layout.** Migration `0014` adds `lab_cluster` (one row, `name`, the cluster's name as a JSON text), `lab_rows`,
  `lab_racks` and `lab_placements`, one row per row, rack and placement, keyed by id (`row-<n>`, `rack-<n>`) or by subject
  (`host:<id>` or `inventory:<id>`), holding the record as the layout file holds it
  (`docs/design/ui-shell-lab.md`). A database that holds no row holds no layout, which is how a coordinator of this release
  starts on a database the release before migrated. `0014` says older releases may run on it (`may-run`): they never read
  the tables. The `Layout` service and the approval of an enrollment with a placement write these rows; `export` writes them
  to the layout file.
- **The MDM's tables (unused).** Migrations `0005`, `0011` and `0012` made the tables of the
  coordinator's MDM, which was removed (2026-11-14, `docs/design/coordinator-mdm.md`): `mdm_devices`
  (`0005`), `mdm_enrollments`, `mdm_identities`, `mdm_commands`, `abm_devices` and `abm_state`
  (`0011`), and `mdm_secrets` and `mdm_settings` (`0012`). No code reads or writes them now, and they
  stay in place, with their migrations and their pinned SHA-256s, because a format changes by
  addition only and a release before the removal finds them where it left them (each of the three
  says older releases may run on it). A database that holds rows in them starts, serves and exports
  as one that holds none; `export` does not count or write them. The rows are the old MDM's: the
  sealed secrets among them (the push key, the ABM token, the MDM CA key, the DNS credential) are
  ciphertext under a key file (`--mdm-secrets-key-file`) that no release reads now; delete the file
  and the rows by hand when no older release is to run on the database again. A file store left the
  directories `<state-file>.mdm/` and `<state-file>.mdm-setup/` beside its files, made by the MDM's
  first change; nothing reads them either.
- **No built-in host group.** Migration `0013` once inserted a group `all` that every host was in;
  migration `0018` deletes it. `0018` deletes the group named `all` and every row keyed by it
  (`host_group_packages`, `group_desired`, `group_min_versions`, `group_services`, `group_rollouts`,
  `group_rollout_done`, and the halts and marks keyed by a group), and sets `enrollment_hosts.host_group`,
  `enrollment_hosts.moving_from`, `enrollment_requests.approved_host_group` and `inventory.host_group`
  to `NULL` where each is `all` (audit rows keep their history). After it `all` is an ordinary name. A host is in one group or is
  unassigned, and an unassigned host, which the `all` group used to give `hostkeeper`, is ordered no
  package until an operator moves it into a group ([hostgroups.md](hostgroups.md#there-is-no-built-in-group)); every
  group lists `hostkeeper`. `0018` says older releases may run on it: a release that holds `all` writes
  the group back at its start, listing `hostkeeper` alone, and places every host that is in no group
  in it again, so it orders such a host `hostkeeper` as it did, and no more; a release before `0013` reads
  the database as it did. A shipped migration is never edited: `0013` stays as it is and its
  SHA-256 stays pinned. The file store drops the group `all` and what is keyed by it from the
  state file and the ops file when it reads them, and writes the `host_groups` key without it.
- **Bounded waits.** Each session connects within 5 seconds, a statement runs at most 5 seconds and
  waits for a lock at most 3, and the connection's keepalives drop a peer that is gone. A wait
  that runs out loses the session. The same bounds hold at start, for each statement of a
  migration and of the load.
- **A lost session.** When a write finds the session gone (the server restarted, the network
  dropped, a wait ran out), the coordinator orders nothing (its status says it is quiet, and an
  operator's force or abort waits too), refuses every change that lowers protection
  `store_failed`, keeps a change that raises protection (a halt, a bad mark, a pause, an abort) in
  memory, still answers heartbeats, and raises the critical alert `store_unreachable`
  ([holds-and-force.md](holds-and-force.md)). At each heartbeat it opens a new session and takes the
  lock again (shared: another coordinator may have started meanwhile, which changes nothing), on a
  thread of its own. With the lock, a commit whose session was lost while it was in flight is
  settled by its row of the `tx_ledger` table (when it landed, memory takes it and the log says
  so), the changes in memory are committed, and the coordinator orders again.
- **The marker** (`--halt-marker-file`, on a local disk that stays writable when the database is
  not) lists the audit number of every change in memory and not yet committed, with the epoch:
  `{"unwritten":[41,42],"epoch":3}`. It is written before such a change's commit is tried and
  removed once nothing is pending. A start that finds it removes it when the database is still at
  the epoch the marker names (each start of any coordinator of the database raises the epoch by
  one) and every number it lists has its row in the `audit` table (each such
  change commits its audit row with it); otherwise it refuses to start, naming the numbers that did
  not land (every number, when another coordinator led the database since, as its rows can carry
  the same numbers): look each up in the log, make it again by
  hand if it still matters (a halt is raised again by the host's next report while the host is up),
  and remove the file. A marker this release cannot read refuses the start as one of the file store
  does.
- **The audit log** is the table `audit`, which refuses every `UPDATE`, `DELETE` and `TRUNCATE`
  (a guard against a bug or a hand-typed statement, not against the database's owner). Every row is
  kept; there is no rotation.
- **Backups** of the database are taken with the database's own tools while the coordinator runs:
  `pg_dump` reads one consistent snapshot. The compose deployment
  ([docs/deploy/coordinator](../deploy/coordinator/README.md)) dumps it every six hours and keeps a
  week of dumps; its runbook has the one-time setup, the restore (into an empty database, then the
  serial counter moved on) and the monthly restore drill.

## Several coordinators on one database

Any number of coordinators may serve on one database (`docs/design/coordinator-multi-instance.md`).
There is no leader: each takes the lock shared, each decides inside a transaction, and any of them
answers any host or operator.

- **What is shared.** Everything the database store holds ([above](#the-database-store)): the desired
  state, the operators' changes, the fleet's rows and the decisions over them, the enrollment
  records and the deny rows, the users, the sessions and the limiter's failure counts, the periodic
  jobs and the audit log. A change one coordinator commits is read by the others at their next
  request, by the versions of `state_versions`. What a coordinator keeps in memory is a copy that
  it reads again when that version moves, or a cache that can only refuse earlier (the spent CI
  token ids).
- **What a restart or a deploy does.** It signs no operator out (the sessions are rows), loses no
  halt, plan position or queue place, and leaves each periodic job to the other coordinators. The
  files of the file store are one process's: a restart there signs everyone out ("The sign-in",
  below) and the lock refuses a second coordinator.
- **What stays per coordinator.** Its listeners, its row of `instances`, its console and SSH
  tunnels, the counts `clock_skew_beats` and the heartbeats' skip sets of the enrollment store,
  and its server certificate. The listeners one coordinator serves are its own: a
  console stream or a tunnel lives on the coordinator that opened it.
- **The Xcode store is a directory, not rows.** `--xcode-dir` is not in the database: every instance
  mounts the same directory, and each reads and writes it as files ([xcode-releases.md](xcode-releases.md)).
  An upload is checked against the directory under the instance's own lock, so two uploads of one
  release that reach two instances at once are not serialised between them; push each release once.
  The refusal of a second build under a version (`xcode_version_taken`) is one instance's memory.
  Each instance names its upload files with a random token of its own, and a start removes only the
  `.upload-` files last written over 24 hours ago, so a starting instance does not remove an upload
  another instance has in progress.
- **What is not built.** The edge's health checks, the start-first update and the Mac listener
  through the edge are not in this repository: the deploy runs one coordinator and replaces it
  stop-first ([../deploy/coordinator-health.md](../deploy/coordinator-health.md), section 17 of the design).

### Stopping, and the two health routes

`--health-listen` answers two routes, `/healthz` (the process is alive) and `/readyz` (the
coordinator takes traffic now), and `--drain-delay-secs` (default 15) says how long a stopping
coordinator keeps serving after it is told to stop; both are the same over either store
([../deploy/coordinator-health.md](../deploy/coordinator-health.md)). From SIGTERM or SIGINT
`/readyz` answers 503 `draining` at once and `/healthz` stays 200, every listener keeps accepting
for the delay, and then the listeners accept no more, the open connections finish, held
`Releases.WaitConverged` calls are answered, and the process exits 0 once no connection is open.
`/readyz` also answers 503 `store_unreachable` while a database session is lost ("A lost session",
above), where `/healthz` stays 200: the process is alive, and a restart would not bring the database
back. The delay and the container's `stop_grace_period` are checked once at deploy, by the person
who deploys, against the edge's values; the coordinator does not read them at run time.

## Moving from the database to the files

`export` writes the state a database holds into files of the file store that hold none:

```
hostkeeper-coordinator export --database-dsn-file <file> --state-file <file> [--ops-file <file>] \
    [--enrollment-dir <dir>] [--legacy-service <name>] --into-empty
```

- It takes the database's lock, so a coordinator serving on the database refuses it, brings the
  schema up to this release as a start does (a newer schema is refused), and reads everything in
  one read-only snapshot.
- It writes as `restore` does: only into targets that hold no state, never over a file, with
  `<state-file>.restore-unfinished` while it writes, and what it wrote removed again when a write
  fails. A database that holds the enrollment store needs `--enrollment-dir`.
- The desired-state file is in the single-state form exactly when `--legacy-service` names the only
  service. The whole audit history is one audit file; a history the database holds as pruned is written
  as the sealed segment `<ops-file>.audit.2`, so the file store says the same time of its oldest
  line.
- The rows of `users` and `api_tokens` are written as `<state-file>.accounts` (mode 0600, the file
  the file store keeps), which a coordinator started on the files reads as the users and tokens the
  database held, hashes and all; a database that holds neither is exported with no such file. There
  is no import: a file store's accounts file does not reach a database by any verb, and a database
  is filled by serving on it.
- The rows of `lab_cluster`, `lab_rows`, `lab_racks`, `lab_placements` and `lab_devices` are written as `<state-file>.layout` (mode 0600, JSON,
  the file the file store keeps), which a coordinator started on the files reads as the layout the database held; a database
  that holds none is exported with no such file. There is no import, as for the accounts.
- The tables of the removed MDM (`mdm_*` and `abm_*`) are not exported: nothing reads them, and they
  stay in the database.
- It prints the files it wrote, the rows of every table, and the lines of the audit file.

To go back from the database to the files: stop the coordinator on the database, `export
--into-empty` into new paths, and start the file coordinator on them. Changes made on the
database are kept. CI's spent token ids that are past are not exported, as the file store drops
them.

The rest of this page is the file store.

## What each file holds (the file store)

| file | holds | without it |
|---|---|---|
| `--state-file` | The desired state of each service (version, limits, minimums) and the package digest of every version CI has set; and, under `host_groups`, each host group (its packages in order, its policy, its generation, who changed it last) with its desired version of each package. `host_groups` is written only while a group is held, and a coordinator before groups refuses the file then. | No service has a desired version until CI sets it again; no host group is held. |
| `<state-file>.releases` | The release each version was set with by a `SetDesired` with targets: per platform, its artifacts in the order set, and the digest of the release index set with them ([packages.md](../design/packages.md) section 3.6). Written once a version is set with targets. A coordinator before targets does not read it. | `Releases.GetRelease` answers `release_unknown` for every version, and a version can be set again with other targets. |
| `<state-file>.repositories` | The registry of package repositories ([packages-repositories.md](../design/packages-repositories.md) section 3): each repository with its keyring and the registry generation it was last put at, and the registry's generation. Written whole when an operator's `Repositories.Put` or `Repositories.Remove` changes it. A coordinator before the registry does not read it. | The registry is empty: no host is sent a repository until an operator puts each one again, and a `SetDesired` with a repository entry is refused `repository_unknown`. |
| `<state-file>.jti` | The ids of CI's OIDC tokens already spent, each with the second until which it is refused. | A CI token spent before the loss can be used again until it expires. |
| `<state-file>.verdicts` | Per service and host, the last release verdict the coordinator acknowledged, and the verdicts it counts toward an automatic rollback. | A host whose daemon stopped resending its verdict is no longer read as having failed it, so it can be ordered that version again; the count toward an automatic rollback starts again. |
| `<state-file>.accounts` | The operator's users and API tokens (`docs/design/operator-login.md`, komira-ai/hostkeeper#300, sections 6, 7 and 9): per user, the Argon2id hash of the password, whether the user is enabled, whether it must change its password, the deadline of an initial or reset password, who made the user and when, and when it last signed in; per token, the SHA-256 of the token, who made it and when. Written whole, mode 0600, when a change of a user or a token is made, and only then. A coordinator before the users does not read it; a key this release does not know is skipped, and a value it cannot read stops the start, naming the file. The writers are `--initial-admin-file` (below), a password change at sign-in, and the `Users` and `ApiTokens` calls ("Users and API tokens", below), by the Users and API tokens pages of the web app and by `hk`. | No user and no API token made in the UI exists; the tokens of `--tokens-file` still work. |
| `<state-file>.layout` | The lab's layout (`docs/design/ui-shell-lab.md`): the one cluster's name, its numbered rows, the numbered racks of each row with their height in U, and the placements of servers (an inventory entry, or a host no entry matches) in racks, each with its lowest U and its height in U. Written whole when a change of the layout commits, and only then; a file that is not there is an empty layout, and one that breaks a rule of it (a placement outside its rack, two on one U) stops the start, naming the file. Backed up and restored with the state file. | The Home page shows an empty lab and every server unplaced; rows, racks and placements are made again on the page. |
| `--xcode-dir` | The private Xcode store ([xcode-releases.md](xcode-releases.md)): per archive, `<sha256>.xip` (the bytes as Apple ships them) and `<sha256>.json` (its version, build, size, who pushed it and when), each mode 0600. Not written by `backup`, which copies the state files only; with the file store or the database, it is the directory the operator makes and mounts. | The archives are gone; an archive is pushed again, and the alert `xcode_release_waiting` stands until it is. |
| `<state-file>.lock` | Nothing: the lock `serve` holds while it runs, so a second coordinator on the same files is refused. | Nothing. It is not backed up. |
| `--ops-file` | What operators changed: exclusions, labels, halts and their clears, holds, assignments, minimum versions, the `repository` key of a service (the repository `Services.SetRepository` set; written only while set; a coordinator before it passes over it), the `trusted_publishers` key of a service (its trusted publishers, [ci-client.md](../deploy/ci-client.md#trusted-publishers); written only once an operator changed the package's list; a coordinator before it refuses the file then), uninstall intents, the host inventory (each record's host group among it), the marks of last good and bad versions, and the audit ring; and, under `host_groups`, each (host group, package)'s minimum version, last good version, halt with its reports and generation, force, and plan position, all in this file (none in the halts file). `host_groups`, and a record's or an entry's `host_group`, are written only when there are any, and a coordinator before groups refuses the file then. | All of it. |
| `<ops-file>.audit` | Every change to the ops file, one JSON line each, and the console's audit-only lines (`console_open`, `power_action` and the rest), numbered with the changes, which change nothing and never enter the ops file's ring; a change of a host group says it in its `detail` as `host_group`, which a coordinator before groups passes over. | The audit history. |
| `<ops-file>.audit.<n>` | The sealed segments of the audit file, the older changes, one JSON line each ([holds-and-force.md](holds-and-force.md#the-audit-file)). Written once the audit file has been rotated. | The audit history before the newest rotation. |
| `<ops-file>.halts` | The halt reports, who raised each halt, the reports kept while a halt stood, each service's clear generation, and each service's update plan position (`rollouts`: the version it is for, the stages entered, the hosts recorded done, an operator's pause; [rollout-plans.md](rollout-plans.md#what-a-restart-keeps)). Written once there is a halt or a plan that went past its first stage or was paused. | The halt reports and the clear generations: a halted service halts again after its clear. The plan positions: each plan starts at its first stage with no host recorded done, and an operator's pause is gone, so a paused plan goes on until it is paused again. |
| `<ops-file>.halts.next` | Only after a crash in the middle of a change: the halts file that change staged. The next start settles it. | The ops file holds that change and the halts file does not: what the change added to the halts file (a clear's generation, the reports of its acknowledgements) is lost. |
| `<ops-file>.halts.unfinished` | Only while a change that raises protection (a halt, a bad mark, a pause) or an operator's abort is in memory and not yet written, and after a crash in that window: the marker, listing the audit number of each such change (`{"unwritten":[41,42]}`). A start removes it when the ops file holds every change it lists; otherwise it refuses to start, naming the changes that did not land, until the operator makes them again by hand if they still matter and removes the marker. A marker the release before wrote (an empty file) refuses a start as it did. | A change it lists that did not land may be lost without notice. |
| `--enrollment-dir` | The enrollment store ([enrollment-coordinator.md](../enrollment-coordinator.md#state-files)): the requests, the certificates issued, the approved hosts (host id, key hash, role, services, host group, the group a move is from, the generation of its facts; a renewal keeps the last three), the deny rows, the Mac nonces, the next certificate serial, and, with the MDM gate, its record of each device (`mdm-devices/`). | No enrolled host's heartbeat or renewal is admitted. Each host has to enroll again and be approved again. Hosts that were denied are no longer denied. |

## Backing up

A backup copies the files of a stopped coordinator. It takes the lock `serve` holds, so with the
coordinator running it is refused (`a coordinator holds the lock of these state files`) and nothing
is written. Stop the coordinator, back up, and start it again:

```
hostkeeper-coordinator backup --state-file <file> [--ops-file <file>] [--enrollment-dir <dir>] \
    --out <new directory>
```

The flags are the ones `serve` runs with. Leave out `--ops-file` if `serve` runs without it. Give
`--enrollment-dir` whenever `serve` has it, or the enrolled hosts are not in the snapshot.

- The backup refuses an `--out` that is already there. It also refuses an `<out>.partial`, which is
  where the snapshot is written before it is renamed to `--out`. A backup that fails after making
  its `<out>.partial` removes it.
- It refuses when none of the state files is there, or the state file's directory is not, which
  means a flag names the wrong file. It names each file of the state that is not there and so
  not in the backup: a file that should be there and is named means a flag is wrong.
- It copies every file listed above that is there, except the lock. That is each sealed segment
  of the audit file, too: the manifest names segment `<n>` as `ops-audit.<n>`, with the number it
  has. From the enrollment directory it copies each file ending `.pb`, in that directory and the
  directories under it.
- It copies `<ops-file>.halts.next` and `<ops-file>.halts.unfinished` as they are, so a restored
  coordinator starts exactly as the old one would have. The next start settles a staged halts
  file. A marker refuses the start unless the ops file holds every change it lists.
- Anything else in the enrollment directory is not copied: a `.tmp` file a crash left, or a key or
  certificate file kept there. The backup names each such file it passes over, except the
  store's `lock`.
- The snapshot holds `manifest.json`, which names each file with its size and SHA-256. The
  directory and its files are owner-only (0700 and 0600).
- The output lists each file copied, with its size and SHA-256. Its last line says the CA's key
  and certificate are not in the backup.

Copy the snapshot off the machine yourself; the backup writes only to `--out`. With the release's
container image, run the same image with the coordinator's volume mounted. The image's entrypoint
is the binary, so `backup ...` is the container's arguments, as `serve ...` is.

## Restoring after a lost disk

1. Put the CA's key and its `.cert` back from the sealed copy, at the path `--ca-key-file` names
   ([ca-backup.md](ca-backup.md)). The server certificate file need not be restored: the start
   makes it when it is missing.
2. Restore the snapshot to the paths `serve` will run with:

   ```
   hostkeeper-coordinator restore --from <snapshot> --state-file <file> [--ops-file <file>] \
       [--enrollment-dir <dir>] --into-empty
   ```

3. Start `serve` with the same flags.

What the restore checks and refuses:

- `--into-empty` is required. The restore writes only into targets that hold no state.
- It reads the manifest and every file of the snapshot first. It refuses the snapshot, writing
  nothing, when:
  - a file's size or SHA-256 is not what the manifest says;
  - the snapshot holds a file the manifest does not name, or lacks one it names;
  - the manifest names a path that is no file of a snapshot (a segment's name is `ops-audit.`
    and a number with no leading zero, 1 or more);
  - the snapshot holds the enrollment store and `--enrollment-dir` is not given.
- It then takes the lock. It refuses while a coordinator holds it.
- It refuses when any target file is already there, a sealed segment of the audit file among
  them (any `<ops-file>.audit.<n>`, even when it is the only file there), or when the enrollment
  directory has anything in it. An enrollment directory that is there and empty, a freshly mounted volume for
  example, is fine.
- It makes the missing directories, owner-only. It writes each file to `<file>.restoring` and links
  it into place, which refuses a name that is already there, so no file is written over. Each
  sealed segment of the audit file is written back under its number, so `Audit.List` reads the
  restored audit as it read before the backup.
- Before its first file it writes `<state-file>.restore-unfinished`, and it removes the marker
  after its last. A restore that stopped partway (killed, or the machine lost) leaves the marker:
  `serve` refuses to start while it is there, and so does a second restore. A `<state-file>.migrate-unfinished` (left by a `host-groups migrate --apply` of release 0.1.253 that stopped partway) refuses `serve` and `backup`: `<state-file>.migrate-unfinished: a host-groups migrate --apply of release 0.1.253 stopped partway; restore the backup taken before it`; `restore` and `export` take it, in their targets, for state already there, and refuse as for any other file there. Remove the files the
  restore wrote and the marker, and restore again.
- A write that fails removes what the restore had written. The error names anything it could not
  remove; remove that before restoring again.

## Moving the coordinator to a new machine

1. Stop the coordinator on the old machine. Do not start it there again: the lock keeps a second
   coordinator off the same files on one machine only.
2. Back up there, and copy the snapshot to the new machine.
3. Copy the configuration `serve` reads and the backup does not: the OIDC
   allow-list, the tokens file or operator token file, the TLS files of `--tls-cert`, `--tls-key`
   and `--client-ca`, the Mac serial allowlist if any, the UI directory if any, and the CA's key
   and `.cert`.
4. Restore on the new machine, into the paths its `serve` will run with. They need not be the old
   paths: a snapshot names its files by what they are, not by where they were.
5. Point the names of `--server-name` at the new machine, and start `serve` there.

## Hosts enrolled after the last backup

A restore brings back the enrollment store as it was at the backup. Anything that changed after
the backup is lost, and every other file of the state is likewise as it was at the backup:

- A host approved after the backup has no host row in the restored store. Its heartbeats are
  refused even though its certificate is of the same CA, and it cannot renew. It has to enroll
  again: a new request, approved by an operator. If the request claims a machine id that another
  request in the store has, or a MAC bound to an enrolled host, it is a `duplicate_claim`, and the
  approval has to set `replace` (`Enrollment.Approve`,
  [enrollment-coordinator.md](../enrollment-coordinator.md)).
- A host whose key and host id are in the restored store is admitted as before.
- A host denied or retired after the backup is not denied in the restored store. Deny it again.
- The serial counter is the backup's. Certificates issued after the restore can take serial
  numbers that certificates issued after the backup already had.
- Desired versions CI set after the backup are lost; CI sets them again. Operator changes after
  the backup (holds, assignments, exclusions, halts) are lost; make them again, using the audit
  file of the lost machine if it survived. Ids of CI tokens spent after the backup are not
  refused until those tokens expire.

So back up after each change you would not want to make again, and before any move.

## The initial-admin file

`serve --initial-admin-file <path>` makes the first operator user (`docs/design/operator-login.md`, komira-ai/hostkeeper#300,
section 4). The file is TOML with four keys and no other, and is read under the rule of a secret file (a regular file owned by
the coordinator's user, mode 0600 or 0400, at most 4096 bytes):

```toml
id = "k3x9..."           # one for each file; 1 to 128 visible ASCII bytes
user = "admin"           # a name of 1 to 32 bytes of a-z, 0-9 and -
password = "..."         # plain text; at least --password-min-length characters
valid_until = 1800003600 # Unix seconds
```

The coordinator reads it at start and on every `--tls-reload-secs`, by its own wall clock, and:

- file absent: nothing;
- `valid_until` not in the future: deletes the file, marks the user's initial password void when this file's `id` made it and
  it was not yet changed, and audits `initial_password_expired` by `coordinator`;
- the user's record already holds this `id`: nothing, so a restart does not reset a password twice;
- otherwise hashes the password with Argon2id and makes the user (or, for a name that exists, gives it the new hash and enables
  it), with `must_change` on, the deadline `valid_until`, and the `id`; audits `user_bootstrap` by
  `coordinator`. The plain password is in the file only, never in the store, the audit or a log. The entry is written before
  the user is stored, so an audit that cannot be written leaves the user unmade and the next look does both; a store that
  cannot write after the entry repeats the entry at the next look. Expiry is in the same order (void, audit, delete), and a
  file already gone when it is deleted counts as deleted.

Over a database every coordinator reads the same file (the volume is mounted by each), and what it asks for is written once:
the user's row is written unless it holds the file's `id` already, and only the coordinator whose write changed the row audits
`user_bootstrap` (the row first, then the entry); a coordinator that finds the `id` in the row does nothing, without hashing.
At expiry the void is written unless the row is void already, and audited by the coordinator that voided it; a file whose `id`
the user's row does not hold is audited by the coordinator whose delete removed the file; a delete that finds the file gone, or
its volume read-only, counts as done.

A file that asks for a `valid_until` further than `--initial-password-secs` (default 3600, at most 3600) past the clock, a
password under the policy, a bad mode or a text this build cannot read is refused naming the flag: at start the coordinator does
not start, and on a tick it is logged as a warning and nothing changes. The file is deleted when it has expired, and when the
user it made changes the password (`POST /v1/password`, below), if it is still the file whose `id` the record holds; a file of
another `id` or user stays until it expires, and one that does not read is left for its expiry.

The start rule is: at least one of `--tokens-file`, `--operator-token-file` and `--initial-admin-file`, and not both token
flags. With `--initial-admin-file` alone no bearer is an operator's; tokens of `--tokens-file` keep working beside it. The
password hash is made under `--password-memory-kib` (19456), `--password-iterations` (2), `--password-parallelism` (1) and
`--password-min-length` (12), which are the OWASP minimum for Argon2id by default.

## The sign-in

A coordinator signs a person in with a user name and a password (`docs/design/operator-login.md`, sections 5 and 6.2). The
four routes are on the private listener only; the heartbeat and LAN listeners answer them as any path they do not serve.

- `POST /v1/login` takes `{"username", "password"}` as JSON (at most 4 KiB; the name is 1 to 32 bytes of a-z, 0-9 and -, the
  password 1 to 256 bytes; any other body is a 400 `bad_request`). It answers `{"username", "csrf", "must_change"}` and
  `Set-Cookie: __Host-hk_session=<id>; Secure; HttpOnly; SameSite=Strict; Path=/`, and audits `login` (the peer's address as its
  source). An unknown name, a wrong password, a disabled user and an initial password past its hour are all 401
  `invalid_credentials`, with the same bytes and the same one password verify. The first failure of a run is audited
  `login_failed` and the lock `login_locked`, both by `coordinator`.
- `GET /v1/session` answers the session of the cookie (`username`, `csrf`, `must_change`, and `must_change_until` while a
  password must be changed); no session is 401 `sign_in_required`.
- `POST /v1/logout` ends the session, answers `Set-Cookie: __Host-hk_session=; Max-Age=0` and audits `logout`.
- `POST /v1/password` takes `{"current", "new"}`: it verifies the current password like a login (a wrong one counts as a failed
  login), checks the policy (a refusal is 400 `password_policy` with the text of the rule), stores the new hash, clears
  `must_change`, ends the user's other sessions and keeps this one, deletes the initial-admin file that made the old password,
  and audits `password_changed`. An initial password is changed up to its deadline: after it, 401 `initial_password_expired`.

`logout` and `password` need the header `X-HK-CSRF` with the session's `csrf` and a body of type `application/json`; an `Origin`
header, when there is one, must be `https://` and the host asked, and `Sec-Fetch-Site`, when there is one, must be
`same-origin` (`login` takes the last two as well); a call that fails one is 403 `csrf` and changes nothing. A call that
carries an `Authorization` header never has its cookie read, and a gRPC call never does. On the JSON gateway
(`docs/gateway.md`) a call with the cookie and no `Authorization` header is made as the signed-in user, under the same checks.
A session that must change its password may call `/v1/password`, `/v1/logout` and `/v1/session` and nothing else.

A session also opens the UI's Remote SSH terminal, `GET /v1/ssh/terminal?host=<host id>` (a WebSocket upgrade, private listener only; `docs/operations/ssh-ca.md`, "Remote SSH from the UI"): the terminal's tunnels live as long as the session does, and what is typed in the terminal counts as use of it.

Over the files sessions are kept in memory: a restart signs everyone out. Over a database they are rows of the `sessions`
table (keyed by the SHA-256 of the cookie's id, never the id) and the failure counts rows of `login_failures` (migration `0025`):
every coordinator on the database answers the same cookie and counts the same failures, and a restart or a deploy signs no one
out; a session's last use is written at most every `--session-touch-secs` (default 30), so its idle deadline may count from up
to that long before its last use. When the database cannot be reached no session is made or found and every name is told
locked; a logout or a password change that cannot end its sessions answers 503 `store_unreachable`, and a user's disable,
removal or password reset `store_failed` (the change stands; making it again ends them). Sessions end 30 minutes after their last use, or 12 hours after the login. The flags, each with its default from the
start:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--session-idle-secs` | 1800 | idle deadline |
| `--session-max-secs` | 43200 | absolute deadline |
| `--session-max-count` | 256 | sessions held; a login past it ends the session least recently used |
| `--login-max-failures` | 5 | consecutive failures of a name before it is locked |
| `--login-lockout-secs` | 900 | how long the lock lasts, and after which a run of failures is forgotten |
| `--login-max-hashes` | 2 | password hashes at once; past it a login is 429 `try_later` and starts no hash |
| `--login-max-tracked` | 4096 | names the limiter counts failures of at once |

The password settings of the section above apply to every hash. A coordinator started from the flags always has a sign-in; the
routes exist even with no user, and no one can sign in until `--initial-admin-file` makes one, or an operator makes one ("Users and API tokens", below).

## Users and API tokens

The operator API has two services for the accounts of the sign-in (`docs/design/operator-login.md`, section 7), served on the private
listener as every operator call is, by gRPC and on the JSON gateway (`docs/gateway.md`), and by `hk users ...` and
`hk api-tokens ...` (`docs/cli.md`). Every operator is equal: a token of the tokens file, a stored API token and a signed-in user
call both, and the audit's `by` is the name of whichever called. A CI token is refused `forbidden`.

- `Users.Create` takes a name and makes the user with a temporary password, which the answer carries once and the coordinator
  keeps no copy of (the store holds its Argon2id hash). The password is 24 characters of letters and digits but `0`, `1`, `I`,
  `O` and `l`, drawn from the system's random bytes. The user must choose another at the first sign-in, and the temporary one works
  until `--initial-password-secs` (3600 by default) after it was made: the answer says the second. `Users.ResetPassword` does the
  same for a user that exists: a new temporary password, `must_change` on, the user's sessions ended, the credential made good
  again if it had been voided, and the initial-admin file that made the old password deleted if it is still there. An operator
  cannot reset their own password (`own_password`): a person changes their own on the sign-in page.
- `Users.Disable` and `Users.Delete` end the user's sessions at once; the last enabled user can be neither (`last_enabled`), so the
  farm is not locked out by its own admin. `Users.Enable` lets a disabled user sign in again with the password they had. A deleted
  user's audit rows stay. `Users.List` tells each user's name, whether it is enabled and must change its password, who made it and
  when, when it last signed in and until when a temporary password works; never a hash.
- `ApiTokens.Create` takes a name and makes a token of 32 random bytes, `hkt_` and 43 base64url characters (the prefix lets a secret
  scanner find a leaked one), which the answer carries once; the store holds its SHA-256, its owner, the creator and the time. The
  owner is the user `owner` names, or else the caller (a token of the tokens file must name one; a name that is no user's is
  `unknown_owner`). The token is an operator's bearer acting as its owner from then on: every check reads the owner's name, and
  the audit is by the owner with the token as `api_token`. `ApiTokens.Revoke` ends it, and so does disabling the owner (until the
  owner is enabled again) or deleting them, which removes their tokens. A token cannot sign in: the login route
  reads passwords only. `ApiTokens.List` tells each token's name, whether it comes from the store or the tokens file, and for a
  stored one its owner and who made it and when; tokens of the tokens file are read-only, and removing one means editing the file, which
  `Revoke` answers `unknown_token` for.
- A bearer is looked up in the tokens file and in the store in one pass, each compared in constant time. Users and tokens share one
  name space, the tokens file's names in it: a name that is already a user's, a stored token's or the tokens file's is
  `name_taken` (`ALREADY_EXISTS`), and `coordinator`, `ci` and the other names the audit uses are `invalid_request`.
- Each call that changes something is audited by the caller, with the user or the token as the entry's `subject`: `user_create`,
  `user_disable`, `user_enable`, `user_delete`, `password_reset`, `api_token_create`, `api_token_revoke`. A call whose audit line
  cannot be written changes nothing (`audit_unavailable`). A call the store then cannot write is `store_failed` and changes
  nothing, and leaves the audit entry written before it. No password, hash, token or session id is ever in an audit entry.

A coordinator before this release has neither service: it answers the paths `UNIMPLEMENTED`, and never reads the accounts file.

## The lab layout

The operator API has one service for the lab (`docs/design/ui-shell-lab.md`), `Layout`, which takes an operator only (a CI token is
`forbidden`) and which `hk layout ...` (`docs/cli.md`) and the Home page call. A hostkeeper runs one lab: the layout has one
cluster name, rows numbered 0, 1, 2, racks numbered within their row (each 1 to 60 U high, 42 by default) and placements. A
placement puts a server in a rack from its lowest U, so many U high; U 1 is the bottom. It belongs to the physical server, which
is its inventory entry when one matches the host (so a wipe and a new enrollment keep the place), else the host.

- `Layout.Get` changes nothing. `PutName` names the cluster; `AddRow` adds the next row number; `PutRack` adds a rack to a row (the
  next number of the row) or resizes or moves one (it takes the next number of the row it moves to); `RemoveRow` and `RemoveRack`
  refuse `in_use` while the row holds a rack or the rack a placement; `Place` puts or moves a server, `Unplace` takes it out.
- A placement that shares a U with another placement of the rack is refused `overlap`, one that is not within the rack's
  U 1 to its height, or a rack made shorter than a placement it holds, `out_of_rack`, a rack the layout lacks `unknown_rack`;
  the layout holds at most 64 rows, 512 racks and 4096 placements (`capacity`). A refusal changes nothing and writes no audit line.
- `Enrollment.Approve` takes an optional placement (`rack`, `placement_u`, `placement_height_u`, all three or none): the approval and
  the placement are judged first, so a placement that cannot be made refuses the approval (`placement_overlap`,
  `placement_out_of_rack`, `unknown_rack`, `placement_invalid`) and nothing changes; the audit holds `enroll_approve` and then
  `layout_place` with the reason `approval`. An approval with no placement places nothing, and the host is listed as unplaced.
- `PutDevice` adds a network device (a switch: name, role BMC or data network, console host and device, optional management MAC, notes) or replaces one by its id `net.<n>`; `RemoveDevice` removes it with its placement. A device is placed with `Place` under the inventory subject of its id.
- Each write is audited by the caller: `layout_name_put`, `layout_row_add`, `layout_row_remove`, `layout_rack_put`,
  `layout_rack_remove`, `layout_place`, `layout_unplace`, `layout_device_put`, `layout_device_remove`; a call whose audit line cannot be written changes nothing
  (`audit_unavailable`), and one the store then cannot write is `store_failed` and leaves the audit entry written before it.

A coordinator before this release has no `Layout` service (`UNIMPLEMENTED`), never reads the layout file or tables, and an
`ApproveRequest` that carries a placement is read by it with the placement dropped, as every unknown field is.
