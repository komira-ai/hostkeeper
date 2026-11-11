# The coordinator's state: where it is, backing it up and restoring it

The coordinator keeps its durable state in one of two stores, chosen by the flags of `serve`:

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
the repository `Services.SetRepository` set for a service, uninstall intents, the inventory, the marks of last good and bad versions, the plan positions),
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
  with the file store); the enrollment store is the database's. The MDM gate runs on either store:
  its flags need enrollment as with the file store (`--ca-key-file`, and `--enrollment-dir` or
  `--database-dsn-file`), and on the database its device records are the rows of the table
  `mdm_devices`.
- **The start.** `serve` opens one session, takes the coordinator's advisory lock of the database
  without waiting, runs the migrations the database lacks (each in its own transaction, recorded in
  `schema_version`), raises the lease's epoch, and loads the state. Nothing is read or written
  before the lock is held. It refuses to start, naming why, when:
  - the database is not up (`the database is not up: ...`): nothing listens, or it is starting up.
    The supervisor starts the coordinator again; the code has no wait loop;
  - the server refuses (`the database refused the coordinator: ...`): a login, a database that is
    not there, or a setting. A database with `fsync`, `synchronous_commit` or `full_page_writes`
    off is refused, naming the setting: the coordinator acknowledges what is committed;
  - another session holds the lock: `another coordinator holds the lock of this database`;
  - the database holds a migration this release does not know, and that migration says older
    releases must not run on it: `the database's schema is newer than this coordinator (migration
    <n>, <name>)`;
  - a migration fails: it is rolled back whole, and the start names it.
- **One writer.** Every write goes through the session that holds the lock. Each transaction moves
  the lease's commit counter on, and the database refuses that from any session without the lock,
  so a coordinator that lost its session can commit nothing. Reads that are not decisions
  (`Audit.List`, the enrollment lists, the alerts' scans) run on a second, read-only session.
  Decisions read memory: a heartbeat in the steady state runs no query.
- **Generations and write ids.** The database keeps, with each service's desired state, its
  generation (one more for every accepted set, a set identical to the state held included, and a
  force), the write id of the last set (`hkctl releases set-desired --write-id`, 1 to
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
  and one that holds none orders every host nothing: there is no placement by roles. `0008` says older releases must not run on it:
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
- **Users and API tokens.** Migration `0010` adds the tables `users` and `api_tokens`, one row each
  per user and per token, keyed by name, holding the record as the accounts file holds it
  (`docs/design/operator-login.md`, komira-ai/hostkeeper#300, sections 6, 7 and 9). A database that holds
  no row holds no user and no token, which is how a coordinator of this release starts on a
  database the release before migrated. `0010` says older releases may run on it: the release
  before never reads either table, so going back below this release needs no restore for it.
  Nothing writes these rows yet: the coordinator holds the store, `export` writes them to the accounts file, and the login and the `Users` and
  `ApiTokens` services that use it land after.
- **MDM tables.** Migration `0011` adds the tables of the MDM server inside the coordinator
  (`mdm_enrollments`, `mdm_identities`, `mdm_commands`, `abm_devices`, `abm_state`). Nothing writes
  them yet. `0011` says older releases may run on it (the release before never reads these tables),
  so going back one release needs no restore.
- **MDM setup tables.** Migration `0012` adds `mdm_secrets` and `mdm_settings`
  (`docs/design/coordinator-mdm.md`, section 4.9). A row of `mdm_secrets` is one version of one kind of
  secret (the push key and certificate, the ABM key and token, the MDM CA key, the ACME account key,
  the DNS credential, the TLS key and certificate): its `sealed` column is a random nonce, then the
  ciphertext and its tag, AES-256-GCM under the key of `--mdm-secrets-key-file`, with the kind and the
  version as the associated data, so a row copied into another kind or version does not open. The
  database, a backup of it and the audit hold ciphertext, the SHA-256 fingerprint and the expiry, never
  a value. The newest version of a kind is the one in use; a replaced one is kept for the
  `secret_grace_days` setting (7) and deleted by the next upload. `mdm_settings` holds one row per
  setting that was set; a setting with no row has its default. `0012` says older releases may run on it
  (the release before never reads these tables). The file store keeps the same rows in the directory
  `<state-file>.mdm-setup/` (`secrets.json`, `settings.json`), made by the first change and not by a
  start; a file store's setup does not reach a database by any verb.
- **The push certificate.** Its records are in `mdm_secrets`: `push_pending_key` (the key of the request
  being made and its CSR, two PEM blocks), `push_encryption_key` (the key the reply of mdmcert.download is
  encrypted to), and `push_key` and `push_cert` (the pair in use, the newest version of each; the key is
  `BEGIN RSA PRIVATE KEY`, as NanoHUB keeps it, or PKCS#8). `Mdm.PutPushCertificate` stores the key and then the
  certificate, so a failure between the two leaves the new key beside the old certificate; the push
  identity is then the newest certificate with the newest held key that matches it, which is the key it
  replaced (held for `secret_grace_days`), until the upload is made again. A renewal keeps the topic of the certificate in use, which every enrollment carries. The settings
  `push_retry_s` (3600), `push_expiry_s` (86400), `push_priority` (10) and `push_timeout_s` (30) are those of the
  push, and `push_alert_days` (60,14) raises the alert `push_cert_near_expiry` before the certificate expires. The
  reply of mdmcert.download comes by email: the operator uploads the attachment (`Mdm.PutPushReply`).
- **The key of the MDM secrets.** `--mdm-secrets-key-file` names a file of 64 hex digits (the 256-bit
  key): a regular file of the coordinator's user, mode 0600 or 0400, which is not followed through a
  link. When it is not there and the store holds no sealed secret, the start makes it (mode 0600, never
  over a file that is there). The start is refused, naming the flag and making nothing, when the store
  holds a sealed secret and the file is not there or the flag is not given (a new key would orphan every
  secret), when the key opens none of the secrets of a kind the store holds, and when the file is not a
  key. The key is the only secret kept outside the store: back it up apart from the store, because a
  backup of the database is only ciphertext and losing the key loses the MDM CA (every Mac enrolls
  again), the push certificate and the ABM token, which are then uploaded again. Without the flag the
  coordinator keeps no MDM secret: `Mdm.PutSecret` is refused `secrets_unavailable`, and the settings
  still work.
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
  lock again, on a thread of its own. With the lock it reads the lease:
  - the same epoch: no other coordinator led meanwhile. A commit whose session was lost while it was
    in flight is settled by the lease's counter (when it landed, memory takes it and the log says
    so), the changes in memory are committed, and the coordinator orders again;
  - another epoch: another coordinator led the database meanwhile and this one's memory is stale.
    It logs every change it drops by its audit number, removes the marker, and exits non-zero
    (`another coordinator led the database while this one's session was lost ...`); its supervisor
    starts it again on the database as the other left it.
- **The marker** (`--halt-marker-file`, on a local disk that stays writable when the database is
  not) lists the audit number of every change in memory and not yet committed, with the epoch:
  `{"unwritten":[41,42],"epoch":3}`. It is written before such a change's commit is tried and
  removed once nothing is pending. A start that finds it removes it when the database is still at
  the epoch the marker names and every number it lists has its row in the `audit` table (each such
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
- The MDM tables (`mdm_*` and `abm_*`, the setup's sealed secrets and settings among them) are not
  exported: a file store's MDM setup is its own directory, and no verb moves one store's MDM data to the
  other. The sealed secrets of a database stay in it, and their key file is not copied.
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
| `<state-file>.accounts` | The operator's users and API tokens (`docs/design/operator-login.md`, komira-ai/hostkeeper#300, sections 6, 7 and 9): per user, the Argon2id hash of the password, whether the user is enabled, whether it must change its password, the deadline of an initial or reset password, who made the user and when, and when it last signed in; per token, the SHA-256 of the token, who made it and when. Written whole, mode 0600, when a change of a user or a token is made, and only then. A coordinator before the users does not read it; a key this release does not know is skipped, and a value it cannot read stops the start, naming the file. The writers are `--initial-admin-file` (below), a password change at sign-in, and the `Users` and `ApiTokens` calls ("Users and API tokens", below); the pages of the web app for them are a later part of the design. | No user and no API token made in the UI exists; the tokens of `--tokens-file` still work. |
| `<state-file>.mdm-setup/` | The MDM setup (`docs/design/coordinator-mdm.md`, section 4.9): `secrets.json`, each version of each secret as sealed bytes in hex with its fingerprint, expiry and times, and `settings.json`, the settings that were set. Written whole when a setup call changes it, and only then; a directory that is not there holds nothing. A field of a later release is kept and written back with its row. A file that does not read stops the start, naming it. `backup` and `restore` do not copy it in this release (nor the key file): copy it, stopped, with the key file kept apart from it. | No secret and no setting of the MDM setup: the secrets are uploaded again, and what was sealed under the key file can no longer be opened. |
| `<state-file>.mdm/` | The MDM tables (`docs/design/coordinator-mdm.md`, sections 4.2, 5.2 and 9.1): one `<family>.json` file per table (`enrollments`, `identities`, `commands`, `abm_devices`, `abm_state`), each `{"schema": 1, "rows": [...]}`, replaced whole when a change touches it; the commands before the enrollments. With `--mdm-listen` the coordinator writes it: an enrollment when a Mac sends `Authenticate`, its push token when `TokenUpdate` comes, a command when the gate queues it, and its answer when the Mac gives it. A directory that is not there holds nothing; a file that does not read stops the start. `backup`, `restore`, `export` and the database import do not carry it in this release (as for `<state-file>.mdm-setup/`): copy it, stopped. With the database the same rows are in `mdm_enrollments`, `mdm_identities`, `mdm_commands`, `abm_devices` and `abm_state`. | No Mac is enrolled in the coordinator's tables and no command is queued: the Macs' next `Authenticate` or `TokenUpdate` is refused `401` until they enroll again (the issued identities are lost with the table), and the gate's records say what they said. |
| `<state-file>.lock` | Nothing: the lock `serve` holds while it runs, so a second coordinator on the same files is refused. | Nothing. It is not backed up. |
| `--ops-file` | What operators changed: exclusions, labels, halts and their clears, holds, assignments, minimum versions, the `repository` key of a service (the repository `Services.SetRepository` set; written only while set; a coordinator before it passes over it), uninstall intents, the host inventory (each record's host group among it), the marks of last good and bad versions, and the audit ring; and, under `host_groups`, each (host group, package)'s minimum version, last good version, halt with its reports and generation, force, and plan position, all in this file (none in the halts file). `host_groups`, and a record's or an entry's `host_group`, are written only when there are any, and a coordinator before groups refuses the file then. | All of it. |
| `<ops-file>.audit` | Every change to the ops file, one JSON line each; a change of a host group says it in its `detail` as `host_group`, which a coordinator before groups passes over. | The audit history. |
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

Sessions are kept in memory: a restart signs everyone out. They end 30 minutes after their last use, or 12 hours after the
login. The flags, each with its default from the start:

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
listener as every operator call is, by gRPC and on the JSON gateway (`docs/gateway.md`), and by `hkctl users ...` and
`hkctl api-tokens ...` (`docs/cli.md`). Every operator is equal: a token of the tokens file, a stored API token and a signed-in user
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
  scanner find a leaked one), which the answer carries once; the store holds its SHA-256, the creator and the time. The token is
  an operator's bearer under its name from then on, and `ApiTokens.Revoke` ends that. A token cannot sign in: the login route
  reads passwords only. `ApiTokens.List` tells each token's name, whether it comes from the store or the tokens file, and for a
  stored one who made it and when; tokens of the tokens file are read-only, and removing one means editing the file, which
  `Revoke` answers `unknown_token` for.
- A bearer is looked up in the tokens file and in the store in one pass, each compared in constant time. Users and tokens share one
  name space, the tokens file's names in it: a name that is already a user's, a stored token's or the tokens file's is
  `name_taken` (`ALREADY_EXISTS`), and `coordinator`, `ci` and the other names the audit uses are `invalid_request`.
- Each call that changes something is audited by the caller, with the user or the token as the entry's `subject`: `user_create`,
  `user_disable`, `user_enable`, `user_delete`, `password_reset`, `api_token_create`, `api_token_revoke`. A call whose audit line
  cannot be written changes nothing (`audit_unavailable`). A call the store then cannot write is `store_failed` and changes
  nothing, and leaves the audit entry written before it. No password, hash, token or session id is ever in an audit entry.

A coordinator before this release has neither service: it answers the paths `UNIMPLEMENTED`, and never reads the accounts file.
