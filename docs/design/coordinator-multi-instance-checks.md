# The coordinator as N instances: measurements, model check, compatibility and tests

Companion of [coordinator-multi-instance.md](coordinator-multi-instance.md) (the as-built choices of its section 4's M3, of
its section 5's M6 and of its section 6's M4, the measurements of its section 7, and its sections 14 and 15, moved here so that file stays under 1000 lines, R12). Section numbers are kept; "section n" in the text below refers
to that document.

## 4. The rest of M3, as built

The choices the design's section 4 leaves open, as the code makes them (`store/postgres/pool.rs`, `writer.rs`, `instance.rs`,
`migrate.rs`, `0022_instances.sql`, `config_database.rs`):

- A write takes the session that has been free longest, so the sessions take turns. Any session found lost makes the whole
  instance lost (every write refused at once, `/healthz` 503 `store_unreachable`) until the next heartbeat's revival, which runs
  on every session's thread: a session that is up answers one `SELECT 1` and is kept, one that is gone, or found gone so, is
  opened again with the shared lock and settles its own commit of unknown outcome. A writer whose thread is gone stops the
  store for good at that revival.
- The instance's row is made in the transaction that bumps the epoch at start (its id the database's `gen_random_uuid()`) and
  upserted every `--instance-beat-secs` by a thread that holds the pool, not the store, and that a drop of the store joins; a
  row that is gone is made again as it was. `listen` holds the heartbeat, private and LAN listeners' configured addresses. The
  status page does not list the rows yet.
- A migration rolled back by its lock timeout lets go of the runners' mutex for the pause, and the runner tries again from that
  migration; after the last attempt the start is refused naming it. `--migrate-ddl-lock-timeout-ms` is 1 to 4999, below the
  sessions' 5 s statement timeout, which would otherwise end the wait first as a failure the runner does not retry.
- The lint's rule refuses, on a hot table the migration did not create, any `ALTER TABLE` action but `ADD COLUMN` with a type,
  `NOT NULL`, a collation and a default with no function call or value function (a serial, generated, constrained or
  referencing column included), an index build, and `CLUSTER`, `REINDEX` and `VACUUM`; a quoted or schema-qualified name is read
  as the bare one, block comments are skipped, and the DDL of a dollar-quoted body is read where it starts.

## 5. M6, as built

The fleet's rows of the design's sections 5.1 and 5.2 as the code makes them (`0024_fleet.sql`, `store/postgres/fleet_rows.rs`,
`store/beat.rs`, `Writer::once` in `store/postgres/writer.rs`, the call in `Coordinator::answer`), where the text leaves a
choice open. No decision reads the rows: the heartbeat is still decided over the fleet in memory.

- `0024` makes `fleet_hosts` (logged, `fillfactor` 50, by M1), `fleet_bakes` (unlogged), `fleet_orders` (its primary key the
  host) and `cluster_quiet` (one row, 0). `fleet_hosts` has no `running` or `repo_report` yet: nothing writes them until a
  reader moves to the rows, and a column added then with no default is what the hot-table rule lets a migration do.
  `fleet_tries`, `fleet_canaries`, `fleet_queued_orders`, `fleet_found` and `fleet_defers` are made by M7, which first writes
  them; their columns are the decisions'. `fleet_orders` has no writer before M7, so until then the release of slots frees
  nothing in a running coordinator; its tests insert orders by hand.
- The statement runs for every heartbeat `Coordinator::answer` takes, before the fleet's lock, through
  `Backend::keep_beat` (a backend one process owns keeps nothing). A denied host's beat is refused before it, and a retiring
  host's is read for its final report only, so neither is kept. It runs on a writing session of the pool outside a transaction
  (`Writer::once`), with no row of `tx_ledger`: a beat whose session is lost mid-statement is never settled, and the host's
  next beat writes the row again. A failure that ends the session loses the store, as any write's does; a refused statement is
  logged (`a beat is not kept`) and the heartbeat answered all the same.
- Every time is the database's `now()` (principle 2); the coordinator's clock is not read. The gap is a newest `seen_unix`
  at or before `now() - DOWN_AFTER`, which is `is_live` false, and a beat dropped as older still runs the gap rule.
- `beat` holds the heartbeat as the coordinator read it, the JSON of `hostkeeper_wire` that both routes read (the gRPC route
  maps its message to it), not the message's bytes. The services kept are the legacy service's top level, then the beat's list;
  an entry of the list named as the legacy service is left out (the reader refuses a list that names a service twice).
- The bake counts a beat by `bake::counts`, the function `Bake::after` reads; `beats` is an `integer` that stops at its
  largest value, where `Bake::after` stops at `u32`'s (a definition bakes at most 100 beats). A slot is released for an
  update or install order (no `uninstall_id`) whose version the applied beat reports and not as an absent copy. The other ends
  of an order in `Fleet` (the beat shows it updating, a deferral, a failure, a refusal of the try) and the aged rows are the
  deciding transaction's (M7).
- The converged path is three round trips, not one: the request's read of the versions (M4, `Coordinator::refresh_caches`),
  the admission's read of the deny row (M5), and this statement. Folding the first two into it needs the heartbeat's admission
  moved after its message is read (the gRPC route admits a caller before the message is decoded), a change of the heartbeat's
  path M7 makes anyway; a round trip is 7 to 26 us (section 7).
- `--beat-reorder-secs` is 1 to 44, below `DOWN_AFTER`.

## 6. M4, as built

The edits, versions, gate and counter of the design's section 6 as the code makes them (`store/edit.rs`,
`store/postgres/edit.rs`, `store/postgres/parts.rs`, `0023_state_versions.sql`, and each store's edit: `ops_edit.rs`,
`desired_edit.rs`, `verdict_file.rs`, `accounts.rs`, `layout.rs`), where the text leaves a choice open:

- A store begins an edit of its scope under its writers' lock (`Backend::begin`); the scopes are constants, each with its lock
  plan (`Scope::locks`). The transaction is held open on a writer of the pool while the store makes its change on its own
  thread, so the stores' edit closures are unchanged and run on the parts read afresh under the locks. A part is read afresh
  only when its version is not the one this instance's copy carries; the store keeps those versions, and a copy carries a
  version from the moment the store takes it into memory.
- The counter's row is the last lock, taken when the edit begins (`SELECT next ... FOR UPDATE`) rather than by an `UPDATE` as
  the last write: a store numbers its entries before it commits (an entry's number is answered to the caller and names an
  abort's order). It is held to the commit, so the numbers are dense and follow the commits, and a rolled-back edit leaves no
  gap. Only an edit that writes audit lines takes it; a console line takes the counter alone (`Scope::AUDIT`).
- A part's version moves on when the commit writes its rows; the ops part's also for a line of the ring of changes (an audit
  line that is not audit-only), so the ring, and the hosts' facts generations, are read in the ops part's snapshot.
- `--operator-gate-wait-ms` bounds every lock wait of an edit (the gate, the parts' rows, the counter): `lock_timeout` is set
  to it, and `statement_timeout` to it plus a second, for those statements only. A wait that runs out refuses the edit
  `store_failed` and keeps the session; a statement's own bounded wait still loses it.
- A heartbeat's own write is the verdicts' (`Scope::VERDICTS`, `SKIP LOCKED`): refused at once while the gate is held or asked
  for, and made again at the host's next beat. What else a heartbeat writes through the ops store (a halt on a threshold, a
  stage entered, the last good version) waits for the gate as an operator's edit does until M7 moves it into the deciding
  transaction.
- A memory-first change (a halt, a bad mark, a pause, an abort) is made in an edit too while nothing is pending. One made while
  a change is pending, or while no edit can be begun (the database lost, the gate not free in time), stands in memory as before
  and lands at the next heartbeat's retry, its lines renumbered from the counter when another instance took their numbers. A
  change staged in a transaction (a force) is refused while a change is pending, and the ops part is not read again while one
  is. The marker file stays as it was.
- Invalidation: `Coordinator::refresh_caches` before every request (`grpc::route`, which heartbeats, API calls and UI requests
  all go through): one `SELECT` of `state_versions` on the read-only session, then each part that moved on in a `REPEATABLE
  READ, READ ONLY` transaction, version first. While a writing session is lost it reads nothing, and a store whose writers'
  lock a change holds (it may be waiting for the gate) is skipped, not waited for: the change reads the part afresh.
- The `Backend` contract's commit without an edit is, over Postgres, an edit of every part (`Scope::WHOLE`); no store calls it.
- The start sets the counter past the newest line after it loads and before it serves, in a statement of its own.
- `facts_generation` is the column `0008` added (nullable, no default: a host with none answers 0), raised to the line's
  number by the transaction of every audit line whose action changes a host's facts (`GREATEST`, so never lowered by it),
  and read with the ops part. A host record the enrollment store writes is checked against the row its decision read (M5),
  so one made before a raise is decided again on the raised row.
- `rollout_gate`, `state_versions` and `audit_counter` are bookkeeping: an export neither writes nor counts them, and the
  start after an import sets the counter past the imported lines.

## 7. The latency budget, measured (M1)

Two places, kept apart in every row. **Live** is the deployment's Postgres service, read only: `SELECT`s and `SHOW`s run with
`default_transaction_read_only = on`, `pgbench` with a script of `SELECT 1;` (no table), and `pg_test_fsync` writing its own
temporary file outside the data directory. Nothing was written to the live database and no setting was changed. **Scratch** is a
Postgres 17.10 container (the image of the same version and build) with the live server's non-default settings
(`max_connections` 200, `shared_buffers` 128MB, `synchronous_commit` on, no TCP listener), on a unix socket, with the schema
the coordinator's migrations make, including `0020_fleet.sql` of the stack (komira-ai/hostkeeper#403 and #404). A measurement
program drives that stack's own code: `fleet_rows::beat` (the beat statement of section 5.2) and `rollout::decide` with
`rollout::admit` (the gate with `SKIP LOCKED`, then the admission). It is an ignored test module run by hand and is not
committed. **Inferred** rows are arithmetic on measured rows, and they say which ones.

Each beat of the program does what a deciding beat does: the beat statement, then `decide(Wait::Beat, admit)` for the host's
next version. A placed order is taken by the host at once and settled by its next beat, so the stage never runs out of work. The
**50% stage** is `max_updating` set to half the hosts of the one role (`min_up` 0, `--live-rollouts` 1, `ORDER_AGE` 120 s). Two
instances with four pool sessions each are 8 sessions. An **operator edit** is `decide(Wait::Operator { 30 000 ms })`: the gate
`FOR UPDATE`, a reload of `desired`, `group_desired` and `halts`, then a halt raised or cleared on another service. That is the
shape of section 6, built since in M4 (`store/postgres/edit.rs`, section 6 above). The **operator's wait** is the time from its `BEGIN` to the moment the
gate is granted, so it includes the `BEGIN`, ledger and `set_config` round trips. The **hold** is the time from the grant to the
return of `COMMIT`, seen by the client. Times are means, with p99 and max where the tail matters. The scratch host was shared
(load average about 22 on 28 CPUs), so its tails carry that noise.

The scratch WAL is on tmpfs for every row unless the row says "local disk". That leaves out the commit's flush, and the live
disk's flush is then added from its own measurement. A cross-check on the local disk agrees: its flush is 4.9 ms, and the gate's
hold there is 4.7 ms against 0.54 ms on tmpfs.

| Quantity | Value | Where | Command or query |
|---|---|---|---|
| Postgres version | 17.10 (Debian 17.10-1.pgdg13+1); `transaction_timeout` exists (set to 0) | live | `SELECT version()`; `SHOW transaction_timeout` |
| `max_connections` | 200, `superuser_reserved_connections` 3; the coordinator's role has no connection limit (`rolconnlimit` -1) | live | `SHOW max_connections`; `SHOW superuser_reserved_connections`; `SELECT rolname, rolconnlimit FROM pg_roles` |
| Connections in use | 24 on the server, 2 of them the coordinator's (both idle) | live, one sample | `SELECT datname, usename, state, count(*) FROM pg_stat_activity GROUP BY 1, 2, 3` |
| Connections N instances need | 2 instances x (`--database-pool` 4 + 1 reader) = 10, against 173 free (200 - 3 reserved - 24 in use) | inferred from the two rows above | section 4, "Sessions" |
| Enrolled hosts (the real host count) | 11 (none denied) | live | `SELECT count(*) FROM hostkeeper.enrollment_hosts`; `SELECT count(*) FROM hostkeeper.enrollment_deny` |
| Round trip on the socket, `SELECT 1` | 7 to 9 us (simple and prepared protocol, 3 runs of 20 000 each) | live | `pgbench -n -M simple\|prepared -f <SELECT 1;> -t 20000 -c 1 -h <socket directory>` |
| Same, scratch | 9 to 10 us (`pgbench`); 17 to 26 us through the coordinator's client (`Session::query_one`, which parses and binds on every call) | scratch | as above; the program, 2000 calls |
| Commit flush (8 kB `fdatasync`, the server's `wal_sync_method`) | 0.84 ms | live disk | `pg_test_fsync -s 2` |
| Same, scratch | 4.9 ms | scratch, local disk | `pg_test_fsync -s 2` |
| Write load of the server today | 3.7 kB/s of WAL, 3.5 WAL flushes/s, 70 commits/s (0.22/s in the coordinator's database) | live, over 120 s | two samples of `pg_stat_wal` (`wal_bytes`, `wal_sync`) and `pg_stat_database` (`xact_commit`) |
| Beat statement, converged (no decision), `fleet_hosts` unlogged | 0.43 ms at 11 hosts, 0.34 ms at 500; 30 to 38 B of WAL per beat and no flush | scratch | the program, 1100 and 5000 beats, `pg_wal_lsn_diff` around them |
| Same, `fleet_hosts` and `fleet_bakes` logged | 0.35 ms (tmpfs) and 8.0 ms (local disk) at 11 hosts; 368 to 430 B of WAL and one flush per beat | scratch | the same after `ALTER TABLE ... SET LOGGED` |
| Same, logged, on the live disk | about 1.2 ms per beat (0.35 + 0.84) | inferred | the two rows above |
| `fleet_hosts` write cost at 4 beats per host per minute | 11 hosts: 0.73 beats/s, so logged is 270 B/s of WAL and 0.73 flushes/s (+7% WAL and +21% flushes on the server today), and unlogged is 22 B/s with no flush. 500 hosts: logged 14 kB/s and 33 flushes/s | inferred | beats/s (hosts x 4 / 60) x WAL per beat (368 B logged, 30 B unlogged at 11 hosts; 430 B logged at 500), against the live write load row |
| HOT updates and size | 100% of `fleet_hosts` updates were HOT, 98.9 to 99.4% of `fleet_bakes`; `fleet_hosts` 90 kB at 11 hosts, 0.9 to 1.1 MB at 500 | scratch | `pg_stat_user_tables` (`n_tup_upd`, `n_tup_hot_upd`), `pg_total_relation_size` |
| The gap rule's `max(seen_unix)` | 45 to 69 us at 11 hosts, 58 to 71 us at 500 (the whole round trip, unlogged and logged) | scratch | the program, 2000 runs of `SELECT coalesce(max(seen_unix), 0) FROM fleet_hosts` |
| Gate hold, 50% stage, real rate | 0.54 ms mean, p99 1.6 ms (11 hosts); 0.99 ms mean, p99 4.2 ms (500 hosts) | scratch | the program, every host beating every 15 s for 180 s, 2 instances x 4 sessions, an operator edit every 5 s |
| Same, on the live disk | about 1.4 ms mean at 11 hosts (0.54 + 0.84) | inferred | the rows above |
| Gate hold, saturated (8 sessions beating back to back) | 0.73 ms mean, p99 1.3 ms; 1230 decisions/s through the gate (11 hosts); 0.84 ms, p99 1.8 ms (500 hosts) | scratch | the program, 60 s |
| Gate capacity on the live disk, against the demand | about 700 decisions/s (1 / 1.4 ms) against 0.73 deciding beats/s at 11 hosts (0.1% busy) and 33/s at 500 (5%) | inferred | the rows above, if every beat took the deciding path |
| Beats that found the gate busy, 50% stage, real rate | 0 of 133 (11 hosts); 7 of 5985, 0.12% (500 hosts); 0 of 5985 at 500 hosts with no operator edits | scratch | the real-rate runs, with and without operator edits |
| Same, saturated | 89 to 90% with or without operator edits (11 and 500 hosts): the gate is the bottleneck at about 11 000 beats/s, 15 000 times the real 0.73/s at 11 hosts | scratch | the saturated runs |
| Operator edit's wait for the gate, 50% stage | real rate: 0.73 ms mean, max 1.6 ms (11 hosts); 0.74 ms, max 2.7 ms (500 hosts). Saturated: 0.77 ms, p99 2.1 ms (11); 0.96 ms, p99 3.9 ms, max 11.6 ms (500). On the local disk, saturated: 1.7 ms, p99 8.5 ms | scratch | the same runs; the default wait it is bounded by is 30 000 ms |
| Heartbeat's database time (beat statement + gate attempt), with and without operator edits | real rate, 11 hosts: 1.84 ms with and 2.06 ms without. Real rate, 500 hosts: 2.6 ms, p99 8.0 ms with, and 2.1 ms, p99 4.4 ms without; the beat statement, which never touches the gate, accounts for about half of the mean gap and most of the p99 gap (p99 4.4 against 1.6 ms), so the gap is the shared host's noise. Saturated: p99 1.72 against 1.73 ms (11 hosts) and 2.06 against 2.15 ms (500) | scratch | paired runs, `M1_OPS` 1 and 0 |
| DDL of a migration against a running instance (`ALTER TABLE ... ADD COLUMN` with `SET LOCAL lock_timeout = '3000ms'`, on `fleet_orders`, `fleet_hosts`, `rollout_gate`, 20 each) | lock wait and catalog change together: 0.45 to 1.5 ms mean, max 12.5 ms (tmpfs, saturated, 11 and 500 hosts); 6 ms mean, max 20 ms on the local disk. None of the 180 runs (60 per scenario: tmpfs at 11 and at 500 hosts, local disk at 11) reached the 3000 ms lock timeout | scratch | the program, a fourth session running the DDL every 250 ms under the saturated load |
| Beats stalled behind a waiting DDL | the beat statement's max rose from 5.4 ms to 38 ms (tmpfs) and the heartbeat's from 24 ms to 83 ms (local disk), for the moments a DDL waited; the heartbeat's p99 about doubled (1.7 to 3.7 ms on tmpfs, 2.9 to 6.1 ms on the local disk) | scratch | the DDL runs against the saturated runs |

**Verdict for section 19, decision 6.** No: the operator edit's wait under a 50% stage does not show in the heartbeat's latency.
A beat never waits for the gate (`SKIP LOCKED`). The paired runs with and without operator edits give the same heartbeat times when saturated, and at the real rate
the 11-host run with edits was the faster one. At 500 hosts most of the p99 gap is in the beat statement, which takes no gate.
So the gap is the noise of the shared host. An edit's only effect on beats is a
beat that finds the gate busy and decides 15 s later: 0 of 133 at the real host count, 7 of 5985 at 500 hosts. The one gate row
stays the granularity, and no per-service row is needed.

**Verdict for decision 4.** At the real host count the WAL cost of a logged `fleet_hosts` does not matter: 270 B/s and 0.73
flushes/s on a server that writes 3.7 kB/s and flushes 3.5 times a second. So by decision 4, `fleet_hosts` is logged. A logged
beat waits for one flush, about 0.84 ms on the live disk, which is noise against a 15 s heartbeat.

**Converged path against section 7's bound.** The beat statement alone is 0.34 to 0.45 ms on the scratch copy, at the edge of
"a few hundred microseconds". Most of it is the client's parse and plan of the statement on every call, which a prepared
statement would cut. At 0.73 beats/s that is not a load, so none of section 7's fallbacks is taken.

## 14. The model check, before code

The protocol to check is the deciding transaction, the conditional writes and the periodic claim, not the SQL. Two tools, one
idea each, as first designed; what was built (M2) is one of them, below.

**Built** (M2: `docs/design/models/coordinator-multi-instance.md`, the spec, and
`crates/hostkeeper-coordinator/tests/multi_instance_model/`, the test, `cargo test -p hostkeeper-coordinator --test
multi_instance_model`). The model is the Rust state-space test of the second item, split into twelve small models (`orders`,
`operator`, `steps`, `quiet`, `ledger`, `beats`, `reload`, `audit`, `periodic`, `migrate`, `locks`, `drain`), each searched over
every interleaving of its statements at N = 1, 2 and 3, with one test per invariant. The spec is not TLA+: it is a document whose
tables (every invariant with its model, every variant with the invariant it breaks and the smallest N at which it does, every
model's bound) the test reads and holds to the models in both directions, so the spec is checked by the same test run that CI
already runs, and no second tool (TLC needs Java, and B9 lets a pull request run only build and unit tests) is needed. What the
models found is in the spec's section "What the models found"; one of them changed section 11 of the design (panic mode at the
edge can engage for a few seconds during the drain delay, with no refused connection). The first item below is kept as it was
designed, for the record of what the models cover.

- **A model of the protocol** (TLA+/PlusCal, `docs/design/models/` in the PR that builds M2; run by an `xtask` verb outside
  the PR gate, since TLC needs Java and B9 allows only build and unit tests with coverage). The number of instances is a
  parameter: it is run at 1, 2 and 3, and a symmetry argument (no step names an instance, and the instances are
  interchangeable) is what carries the result to any N; the model has no leader, no lease and no election to include. State:
  instances, hosts `h1`, `h2`, services `s1`, `s2`, the shared rows (`orders`, `halts`, the plan stage, `desired.set_seq`,
  `cluster_quiet`, `periodic_jobs`, the gate), each instance's cache and version. Steps: a beat arrives on any instance at any
  time, and beats of one host may be applied in either order; a deciding transaction takes the gate, then its reads and its
  writes are separate steps inside the lock, so that a writer of an input that does not take the gate is *found* to break it;
  an operator edit of a decision input (halt, hold, force, retire, desired, layout, verdict) is a transaction that takes the
  gate or, in the negative variant, does not; a stage advance is the compare-and-set of section 5.3; a periodic tick claims a
  job by the due-time update; an instance may crash after `COMMIT` and before the reply, or before `COMMIT`; a commit may be
  of unknown outcome (the connection drops while the backend still finishes) and is retried with the same ledger id; the
  hung holder is a transaction the server aborts at any step (the idle timeout), a beat that finds the gate held is answered
  without a decision, an operator edit queues behind the holder; two starters run migrations; a part is reloaded in two
  reads or in one snapshot; the database may become unreachable (the model has every instance take no step, which is the fail-closed rule), restart and
  empty `fleet_hosts`, or the beats may stop for longer than `DOWN_AFTER`; the release before may run between two runs of the
  new one; a desired version changes. Invariants: live rollouts never exceed `--live-rollouts`; updating hosts of a stage
  never exceed `max_updating`; `min_up` holds; an order is never lost or duplicated for one `(host, service, target)` inside
  `ORDER_AGE`, and a host never has two orders at once; a stage is advanced once per version and each advance has one audit
  line; a halt raised twice is one halt generation and one audit line; a rollback is committed before any uninstall of that
  package is ordered; a halt, hold, force or desired change whose transaction committed before the gate of an order's
  transaction was granted is read by that decision (and is not read after: one lock order, no cycle); a transaction of
  unknown outcome lands exactly once; the stored `running` of a host never moves to an older beat; no order, stage advance, halt on a
  threshold, bad-version mark or rollback is made while `cluster_quiet` is in the future (an operator's halt is), and it is set after every gap of `DOWN_AFTER` in the beats; the audit counter and
  `set_seq` never reuse a number after the release before ran; queue order equals `set_seq` order on every instance; audit
  numbers follow commit order; a periodic job's effect happens once per due period, however many instances claim it, and a
  claimer that dies loses that run only; every migration is applied once however many instances start at once; a cache's
  label is never newer than its rows; no beat is granted the gate while an operator edit is queued for it. Liveness: a host behind its target gets an order if a slot is free, whatever the
  number of instances and whichever of them hears the beat.
- **An exhaustive state-space test in Rust** (the part CI runs, T11, T12): the pure decision functions over an in-memory
  store with transactions at statement granularity, all interleavings of N instances (N = 1, 2 and 3) over a bounded script,
  checking the same invariants. It is the test that the unlocked variants fail (T15: negative cases: an operator halt without
  the gate; a decision that reads a cache; a lookup-settle; an unguarded beat upsert; a stage advance without its
  compare-and-set and without the gate; a periodic claim that reads then writes; the quiet rule keyed on a missing sentinel; a migration runner with no mutex; a cache reloaded in two reads; a quiet rule that
  gates orders only; a beat that queues at the gate)
  and the locked one passes. The states are the reachable ones of a script of up to three instances, two hosts, two services
  and four operator and failure events; the bound is kept small and stated in the test.

Sanity run for this document, not the model: a throwaway explicit-state search of two instances each deciding one of two
services with a limit of one live rollout (script in the author's scratch, not committed) finds the double order when "read
the live count" and "insert the order" are two steps, and finds none when they are one step under the lock. Eight and five
states. It was run on the first draft's lock and not rerun for the gate row, which has the same effect. The real models are M2.

## 15. Compatibility (B8) and tests

All changes are additive: tables `fleet_*`, `cluster_quiet`, `rollout_gate`, `periodic_jobs`, `instances`, `sessions`, `login_failures`, `alerts`,
`state_versions`, `tx_ledger`, `audit_counter`, `fleet_bakes`, the sequence `desired_seq`, columns with defaults (`set_seq`,
`set_for`, `facts_generation`). No column renumbered, renamed or re-meant; no proto change. The fence trigger is untouched.
Migration `0019` (`tx_ledger`, built) and every new one from `0020` say `older-releases: may-run` (the migration lint's header rule; each adds only). Why that is safe, and
no `refuse`: while any new instance runs it holds the shared lock, so the release before cannot start (exclusive lock refused);
with none running the release before takes the exclusive lock and runs on its own tables, ignoring the new ones, which is
the back-out. What it forgets is the fleet (it never had one stored), as at every start of today.

Tests, named (each asserts the behaviour in its name; the PR bodies list them by name per P2):

- `release_before_is_refused_while_a_shared_holder_runs` and `shared_holder_is_refused_while_release_before_runs`: the
  exclusive and the shared `pg_try_advisory_lock` on one database; the old exclusive path against the new shared one.
- `release_before_runs_on_the_migrated_database_after_new_tasks_stop`: the previous release's store opened on a database
  written by the new release (the old release on new data); and `new_release_migrates_a_database_of_the_release_before`
  (the new release on old data), both against the service container of the `test` job.
- `exported_files_are_read_by_the_strict_readers_of_the_release_before` (exists, coordinator-storage.md section 12) stays green
  with the new tables exported or ignored by the importer.
- `n_instances_decide_one_rollout_at_a_time` (N = 1, 2, 3), `halt_before_commit_blocks_the_next_order`, `order_after_crash_is_returned_again`,
  `queue_follows_set_seq_on_both_tasks`, `audit_numbers_follow_commit_order`: the Rust state-space test of section 14.
  Added by the review of this document: `acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task`,
  `acknowledged_hold_and_desired_change_are_read_by_the_next_decision`, `unknown_outcome_commit_lands_once_by_retry`
  (including the old backend still committing), `stale_beat_does_not_overwrite_a_newer_one`,
  `bake_in_sql_matches_bake_after`, `order_slot_is_freed_when_the_applied_beat_reports_the_target` (these three **Built**
  (M6) in `store/postgres/tests/fleet_rows.rs`, with the gap rule's own test, the reorder window, two sessions writing one
  host's beats, and the B8 test of `0024` both ways),
  `no_order_after_a_gap_in_beats_until_quiet_ends` (clean database restart, outage over `DOWN_AFTER`, back-out then
  new cutover), `lock_order_has_no_cycle`.
  Added by the leaderless decision: `stage_advance_by_racing_instances_is_one_advance_and_one_audit_line`,
  `halt_raised_by_two_instances_is_one_generation_and_one_audit_line`, `rollback_is_committed_before_the_uninstall_is_ordered`,
  `one_host_never_has_two_orders` (the unique index, with the gate bypassed), `periodic_job_runs_once_per_period_with_n_claimers`,
  `claimer_death_loses_one_run_only`, `database_unreachable_orders_nothing_and_answers_unavailable`,
  `gate_busy_answers_the_beat_with_no_new_order`.
- Added by the second review (section 20, items 7 to 11): `two_starters_apply_each_migration_once` (two runners at once, a duplicate
  of the unlocked variant fails), `migration_ddl_gives_up_by_lock_timeout_instead_of_stalling_heartbeats` (a held gate
  transaction, a DDL with `lock_timeout`: no beat statement waits longer than the timeout, injected clock not wall time;
  **Built** (M3) in `store/postgres/tests/migrate_ddl.rs` with a held transaction on a table in place of the gate, which is M4's,
  and the beat shown let through while the holder is still open, no clock read),
  `migration_lint_refuses_a_rewrite_of_a_hot_table` (negative case, T15; **Built** (M3) in `store/postgres/migrate_tests.rs`), `cache_is_never_labelled_newer_than_its_content`,
  `partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing` (a failed-looking host set after a gap),
  `operator_halt_in_quiet_window_is_read_by_every_decision`, `old_task_accepts_until_the_drain_delay_ends` and
  `draining_task_answers_healthz_503_while_serving` (injected clock, T10), `drain_delay_config_is_refused_at_deploy_when_not_above_the_edge_check`,
  `stalled_holder_is_terminated_and_gate_is_released` (the server's idle-in-transaction timeout against the service container,
  set short for the test), `beat_never_overtakes_a_queued_operator_edit` (against real Postgres),
  `operator_halt_waits_for_one_holder_not_for_beats`.
- Return after a back-out (the old-release-on-new-data test extended with the new release again after it):
  `new_release_again_continues_audit_numbers_after_the_release_before_ran` and
  `set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order`.
- `denied_host_is_refused_by_the_other_task`, `one_request_is_approved_once` (**Built**, M5, in
  `store/postgres/tests/enrollment_races.rs`, with `one_certificate_is_issued_per_round_by_two_tasks`,
  `expiry_on_two_tasks_expires_each_request_once` and the others of that file), `session_is_valid_on_the_other_task`,
  `login_failures_count_across_tasks`, `expiry_audits_each_request_once`, `alert_is_logged_once`.
- `lost_session_orders_nothing_until_revived_and_settles_by_ledger` (replaces the `lease.commits` settle test).
- **Built** (M4), against the service container, in `store/postgres/tests/`: `lock_order_has_no_cycle` (`edits.rs`: every
  scope's lock plan is in the one order, and the transaction takes exactly that plan); `cache_is_never_labelled_newer_than_its_content`
  (`reload.rs`, a commit made between the two reads by the store's own hook); `audit_numbers_follow_commit_order` and
  `new_release_again_continues_audit_numbers_after_the_release_before_ran` (`numbers.rs`, two stores; the counter's re-seed,
  the re-stamp being M7's); `beat_never_overtakes_a_queued_operator_edit`, `operator_halt_waits_for_one_holder_not_for_beats`
  and `stalled_holder_is_terminated_and_gate_is_released` (`gate.rs`), with the operator's side of
  `halt_raised_by_two_instances_is_one_generation_and_one_audit_line` as `a_halt_raised_on_two_instances_is_one_halt_and_one_audit_line`.
- T5/T13: each test above has a failure case (the gate removed, the conditional write made unconditional).

Coverage is judged per crate as always (T1); the pool, the ledger and the conditional writes are covered by the
service-container tests (T17: no arm covered only by a different binary).
