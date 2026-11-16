# The coordinator as N instances on one Postgres

Status: design, for the owner's decision. A living document: it is updated in the pull requests that build it. Tags as in
[hostkeeper-design.md](hostkeeper-design.md): **Built** names code on `main` with a path, **Designed** means nothing implements
it yet. Nothing in this document is built.

It changes one decision of [coordinator-storage.md](coordinator-storage.md): that decision's section 5 ("the single writer: an
advisory lock") and the sentence of its section 14 that a heartbeat costs no query. Section 7 of that document designs an
active/standby pair; this document supersedes it (the supersession check is section 18 here). The rest of that document
stands: the schema, the migrations, import/export, the backups.

Direction (owner, 2026-11-15): the coordinator can run as several instances at once, with Postgres as the store. The same
principle as the existing frontend: a deploy never leaves zero serving instances, and no state lives only in one task's disk or
memory unless it is a cache that can be rebuilt from the database.

Decision (owner, 2026-11-15, second round, binding): **Postgres is the single source of truth, and any number of instances
just work.** There is no leader, no leader lease and no election. Every decision (placement, admission, a rollout step, a
halt, the one-deploy-at-a-time queue, periodic work) is made inside a Postgres transaction whose correctness does not depend
on which instance runs it or how many run it at once. The database is accepted as the central dependency, and when it is
unreachable instances decide nothing (principle 4). The deploy shape is one instance in steady state and two only while an
update overlaps; correctness is argued and tested for any N.

## 1. What changes, and what does not

**Before (Built).** One Swarm task, `replicas: 1`, `order: stop-first`.
The instance takes a Postgres session advisory lock at start (`store/postgres/writer.rs`, `LOCK_KEY`, `try_lock`) and a second
instance that finds it held exits (`DatabaseError::Locked`). Everything the coordinator knows is in that process's memory: the
desired state, the operations state, the accounts and the enrollment records (each loaded once at start and written back as a
diff), and, never stored, the whole live picture of the fleet (`Coordinator.fleet`, `fleet.rs`), the sessions (`session.rs`)
and the rollout queue's order (`queue.rs`). A deploy stops the old instance, then starts the new one: the coordinator is away for
the stop, the start and the 30 s quiet start of `QUIET_START`, and hosts retry their heartbeat meanwhile.

**After (Designed).**

- One instance serves in steady state (`replicas: 1`). During an update two serve at once: the new one starts first and is
  healthy before the old one drains, for up to the stop grace period. Any instance answers any heartbeat, any UI page, any API
  call, behind the edge that already spreads connections to `tasks.hostkeeper-coordinator_server`. Nothing in the design counts
  instances, so a third (a manual `docker service scale`, a stuck drain) is as correct as a second.
- A deploy is `start-first, parallelism 1`. At no moment are there zero serving instances.
- Every fact that two instances must agree on is a row in Postgres. An instance's memory holds only caches that it refreshes
  when the database says they are stale.
- A host sees no difference: same ports, same protocol, same certificate chain, same orders. An operator sees no difference
  except a new "instances" list on the status page. Sessions survive a deploy (today a restart signs everyone out).

**What does not change.** Rollout rules (stages, `min_up`, `max_updating`, one service at a time, FIFO, halts, rollback first),
the proto, the hosts, the file store (still one process, still its file lock, until C9 of coordinator-storage.md deletes it),
and the pure decision code (`stages.rs`, `live.rs`, `halt.rs`): the decision becomes a transaction around the same functions.

**What it is not.** The Swarm has one node today, and so does Postgres. Two instances give a deploy with
no gap and the loss of one instance without an outage; they do not survive the loss of the node or of the database, and the
database is now the one thing every decision depends on. The same holds for the existing frontend.

## 2. Why this is more than a flag: what two instances get wrong today

Spot-checked against `main` (1ed6efd). Each row is the failure if a second instance simply started.

| State | Where it is now | Failure with two instances |
|---|---|---|
| Store lock and epoch | advisory lock + `lease` row (`writer.rs:30`, `:220`; `0001_state.sql` trigger `lease_fenced`) | the second instance exits; so the lock must stop being exclusive (section 4) |
| Desired, ops, accounts, layout, verdicts | one in-memory copy per store, written as before/after diff (`store/mod.rs`, `rows.rs`); writers serialize in a process mutex (`ops.rs:534`) | A's change never reaches B; a diff from a stale copy overwrites A's change; two read-modify-writes run unserialized |
| Audit numbers | `last_seq + 1` from an in-memory atomic (`postgres/mod.rs:238`, `ops.rs:884`) | both number the same entry; one commit fails on the primary key; facts generations (`ops.rs:633`, `facts.rs:100`) differ per instance |
| **Fleet**: last beat of each host, orders, bake counts, tries, canaries, queued orders | `Coordinator.fleet: Mutex<Fleet>`, memory (`service.rs:233`, `fleet.rs:1-23`) | an agent dials a new connection per heartbeat (`hostkeeper-link/src/grpc.rs:157-173`), so each instance sees about 1/N of a host's beats: `is_live` (45 s) flaps, bake resets, an order of A is invisible to B, `min_up` counts a partial fleet |
| One service rolling, FIFO, stage slots | computed per heartbeat from the local fleet; queue stamps in memory (`queue.rs:58`) | two instances each find "nothing live" and order two services (`--live-rollouts 1` broken); queue order differs per instance; one slot ordered twice |
| Halts (memory-first, marker file) | `halt.rs`, `ops.rs:500-512`, `--halt-marker-file` | a halt raised on A is in A's memory until its commit lands; B keeps ordering; A's pending halts are lost if A dies |
| Enrollment hosts, deny rows, requests | Postgres, plus an in-memory `Copy` and an in-process mutex (`records.rs:67`, `enroll/store.rs:124`) | a deny on A is not seen by B; two instances approve one request or issue two certificates for one round |
| Sessions, CSRF, login limiter | memory (`session.rs:163`, `:357`) | a login on A is unknown to B ("signed out" every other request); the failure cap is N times too loose |
| Spent CI token ids | Postgres `ON CONFLICT DO NOTHING` (`postgres/mod.rs:726`) plus an in-memory set | safe in SQL; the in-memory set must not be what decides |
| CA key, server certificate | files on the local volume `hostkeeper-coordinator-state` | each instance needs the CA key; two instances must not write one file |
| Alerts, request expiry, initial admin file | loops in each process (`alerts.rs:838`, `listen.rs:402`, `initial_admin.rs:480`) | duplicated log lines and audit rows; a race on the file's delete |

There is no incremental way to run two instances with the current `Fleet`: it is the one in-memory state every order decision
reads. So the fleet moves into the database first (section 5), and the rest follows.

## 3. Principles

1. **Postgres decides; an instance's memory only caches.** Every write is a short transaction. Every decision that places an
   order, raises a halt or moves a plan is a transaction that reads the rows it decides from.
2. **Time is the database's.** Stored times are unix seconds from `now()` in the transaction; no instance clock orders anything.
   `Fleet` today takes `now: Duration` since process start (`fleet.rs`, `config.rs` `origin.elapsed()`); the clock stays
   injected so the decision replays in tests, but its value comes from the database's `now()` fetched in the same statement.
3. **No leader.** There is no election, no lease, no instance with a role, and no background rollout loop (decisions are per
   heartbeat, `service.rs:595`). Three tools carry all coordination, and nothing else: a **row lock taken inside the deciding
   transaction** (the gate, section 5.2, and the part rows, section 6); a **conditional write** (an `UPDATE` or `INSERT ... ON
   CONFLICT` whose `WHERE` names the state the decision saw, a compare-and-set on a version, a stage or a state column, with
   a unique index as the backstop, and the row count says who won); and a **claim by advancing a due time** for periodic work
   (section 9). An instance that dies at any statement leaves nothing that another must clear by a timeout: its transaction
   committed or it did not, and the next heartbeat or tick, on any instance, continues from the rows. An instance that hangs
   instead of dying is cut off by Postgres's own timeouts on its sessions (section 5.2, "Bounds on a hold"), which release its
   locks; no instance's clock does it.
4. **The database is the central dependency, and an instance that cannot use it decides nothing (fail closed).** When Postgres
   is unreachable, or a decision's transaction fails, the instance places no order, raises no halt, moves no plan, answers
   `/healthz` 503 and answers a heartbeat with "unavailable" so the host retries. There is no memory-first fallback and no
   halt or order held in memory until the database is back. Hosts keep running what they run; nothing is rolled out or halted
   until a database is reachable again, and after any gap in the beats the quiet rule of section 5.1 applies. This is
   `Health::Lost` today (`health.rs`), now the rule for every decision.
5. **Additive formats (B8).** New tables and columns only; a name, a number or a meaning that shipped does not change.
6. **No runtime machinery for config mistakes (B20).** Anything a deploy gets wrong is checked once at deploy, not by a gate.

**Why no leader.** The owner decided on 2026-11-15 that Postgres is the single source of truth and that n instances should
just work. A leader would add a second truth (what the leader believes) and a clock-driven rule for replacing it; without one
there is one truth, and a lost instance is only a lost connection.

## 4. The store: from one exclusive lock to a shared one (a release fence, not a lease)

Today's lock is exclusive: one session may hold `pg_try_advisory_lock(LOCK_KEY)`. The trigger `lease_fenced` lets a session
update `lease` only if `pg_locks` shows it holds that key; it does not look at the lock's mode (`0001_state.sql:260-273`).

**Designed.** The lock is only a fence between releases: every new instance holds it at once, it elects nobody and no decision
reads it, and it exists because the release before takes the key exclusively. An instance takes the same key in **shared** mode, `pg_try_advisory_lock_shared(LOCK_KEY)`, on each of its sessions.
Postgres itself then gives:

- any number of new instances hold the key together;
- the release before this one calls `pg_try_advisory_lock` (exclusive) and is refused while any new instance is up
  (`DatabaseError::Locked`), and a new instance is refused while an old one runs. Old and new never write together, with no
  change to the trigger and no new migration for the fence;
- going back works: stop every new instance, the old release takes the exclusive lock and runs on the same database.

Two consequences, both changes of code:

- **`lease.commits` goes.** Every transaction today updates the one row `lease` (`writer.rs:318`), which would serialize all
  writers of all instances on one row lock. Its job, settling a commit of unknown outcome after a lost session (`writer.rs:374`),
  moves to a ledger, and the settle is by **idempotent retry, not by lookup**. Why a lookup is unsound: today a new session can
  settle only after the old backend is dead (it must take the exclusive lock, and the `lease` update is in the same
  transaction); a shared lock excludes nobody, so a `SELECT` of the ledger can run while the old backend is still finishing
  its `COMMIT`, find nothing, and conclude "did not land". The rule instead: every writing transaction inserts
  `tx_ledger(id uuid primary key, at_unix)` as its **first** statement, with an id the instance made before `BEGIN`. After a
  lost session the instance retries the same closure, in a new transaction, with the **same id**, again as the first statement.
  A unique-index insert waits for an in-progress transaction that inserted the same key and then fails if it committed, so
  the retry is decided by Postgres, not by timing: a unique violation means the first attempt landed (the retry rolls back
  and the caller gets the first attempt's outcome, re-read from the rows it wrote, for an order the order row of that host);
  success means it did not, and the retry's own writes are the only ones. A duplicate audit line, a double increment or a
  second order cannot happen. The retry window is bounded (a few seconds, a config key, section 12), far inside the hour a
  ledger row lives. Any instance prunes rows older than an hour in its reload tick (`DELETE ... WHERE at_unix < $1`,
  idempotent). The `lease` row is left as it is, written once by the old release only.
- **"Deposed" goes.** Another instance's epoch is normal now (`config.rs:798`, `ops.rs:551`). `Lost`/`Reviving` stay: an instance whose
  session is gone orders nothing until it is revived (`writer.rs`), and answers 503 meanwhile.

**Sessions.** The single writing thread becomes a small pool of sessions per instance (a config key, section 12), each holding the
shared lock, so heartbeats of one task do not queue behind each other on one session. The pool plus one reader session is
how many connections an instance needs; `max_connections` of the Postgres service must cover `instances x (pool + 1)` with room (not
checked, section 13).

**Instances.** Each instance has an `instance_id` (random at start) and upserts `instances(id, started_unix, seen_unix, release,
listen)` every few seconds. It is for the status page and for the start rule of section 5; correctness never reads it.

**Migrations (serialized, and short).** A shared fence excludes nobody, so it cannot serialize two instances that start at the
same moment (a start-first update, or a third task scaled up by hand): both would read the migrations table, both would run
migration `0020`, and one would fail on a duplicate object. The runner therefore takes a mutex of its own:

- One transaction: `pg_advisory_xact_lock(MIGRATE_KEY)` first, a key of its own and not `LOCK_KEY`, then it reads the
  migrations table and applies every missing migration, recording each in the same transaction, then `COMMIT`. A second
  starter blocks on the lock (bounded by `--migrate-wait-secs`), then reads the table, finds everything applied and does
  nothing. Postgres releases the lock at commit, rollback or the death of the backend, so a starter that dies mid-migration
  leaves nothing to clear. This is a mutex around a few seconds of start-up, not a leader: it elects nobody, no decision
  reads it, and it is gone when the transaction ends.
- Lock order: the migration lock is taken and released **before** the shared fence lock, and the runner never takes the gate
  or a `state_versions` row, so it joins no cycle (`lock_order_has_no_cycle` covers it).
- A migration's DDL can need `ACCESS EXCLUSIVE` on a table the running instance uses (`fleet_orders`, `state_versions`).
  Postgres queues every later statement on that table behind a waiting DDL, so an unbounded wait behind the old instance's open
  gate transaction would stall its heartbeats for the whole overlap. Each migration transaction therefore sets `SET LOCAL
  lock_timeout` to `--migrate-ddl-lock-timeout-ms` (default 3000): a DDL that cannot get its lock in that time rolls the whole
  migration transaction back (nothing is half applied), and the runner retries after `--migrate-retry-secs` up to
  `--migrate-attempts` times, then exits so the failure is visible. The stall is bounded by the lock timeout, not by the old
  instance. The migration lint (the header rule that exists for `older-releases`) gains one rule so the retry is rare: a
  migration on an existing table is `ADD COLUMN` with no default or a constant default (a catalog change, held for
  milliseconds), never a rewrite or an index build on `fleet_orders`, `fleet_hosts`, `state_versions` or `rollout_gate`; a new
  table needs no lock on a running table. This is the B20 shape: the config mistake is caught once, in the lint, not by a
  runtime gate.

## 5. The fleet moves into Postgres

### 5.1 Tables (all new, additive; migration `0019`, `-- older-releases: may-run`)

| Table | Holds | Logged |
|---|---|---|
| `fleet_hosts(host_id pk, role, beat bytea, seen_unix, sent_unix, running bytea, repo_report bytea, instance)` | the last heartbeat of each host (the proto, as received), the host's own `sent_unix` of it, what it runs and its repository report (today `running.rs:43`, `repository_push.rs:119`) | unlogged, `fillfactor` 50, no index on updated columns (HOT) |
| `fleet_bakes(host_id, service, target, beats int, at_unix)` | the count of healthy beats in a row at one version, per service and host (today `Bake`, `bake.rs:33`) | unlogged |
| `fleet_orders(host_id, service, target, uninstall_id, at_unix, try)` | an outstanding order that holds a slot (today `orders`, `ORDER_AGE` 120 s); a unique index on `host_id` (one update at a time on a host) backs the gate | logged |
| `fleet_tries`, `fleet_canaries`, `fleet_queued_orders(host_id, service, kind, order_id)`, `fleet_found`, `fleet_defers` | the other maps of `Fleet` (`fleet.rs:180-210`) | logged |
| `cluster_quiet(id, until_unix)` | the start rule below | logged |
| `rollout_gate(id)` | one row, never updated, only locked `FOR UPDATE` by every transaction that decides or edits a decision input (section 5.2) | logged |
| `periodic_jobs(name, next_run_unix, last_run_unix, last_instance)` | the due time of each periodic job, advanced by whoever claims it (section 9) | logged |
| `desired_seq` (sequence) and columns `set_seq`, `set_for` on `desired`/`group_desired` | the FIFO order of the queue (today `queue.rs:58`, per instance), and the version the `set_seq` was taken for (section 16, back-out) | logged |

`fleet_hosts` is unlogged because it is rebuilt by the next beat of each host within 15 s (`heartbeater.rs:124`); a database
crash empties it. Orders, tries and canaries are decisions and stay logged. A row older than `DOWN_AFTER` (45 s) means down,
as `is_live` says today. Whether unlogged pays for itself is measured before it is chosen (section 7).

**The start rule becomes the cluster's.** Today a restarted instance forgets its outstanding orders and waits `QUIET_START` for the
hosts to report. With orders durable that wait is not needed for them; it is needed whenever the fleet table is **empty or
stale as a whole**: a database crash (the unlogged table is truncated), a clean Postgres restart, any outage longer
than `DOWN_AFTER`, the first start, and a back-out followed by a new cutover (the release before wrote no `fleet_hosts`
rows, so the old ones are old). In all of them the rows still there carry an old `seen_unix`, so judging "down" by `seen_unix`
alone would count most of the fleet as down while the first hosts beat, and `min_up`, the stages and the halt logic (a host
that did not come back) would act on a partial fleet. The rule keys on the signal itself, a gap in the beats:

- The heartbeat's one statement reads `max(seen_unix)` of `fleet_hosts` **as the statement's snapshot has it, before its own
  upsert** (a data-modifying CTE does not see its own rows), and in the same statement, when that value is older than
  `DOWN_AFTER` or the table is empty, sets `cluster_quiet.until_unix = greatest(until_unix, now() + quiet)`. No sentinel row
  and no epoch are needed: an empty table is a gap too. The table holds tens to hundreds of rows (not measured, section 7), so the
  `max` is a scan of a few pages with no index on the updated column (M1 measures it; the fallback is one more row,
  `cluster_quiet.last_beat_unix`, written at most once per second).
- Every deciding transaction reads `cluster_quiet` after it holds the gate and takes **no decision of its own** while
  `until_unix` is in the future. What is held back is every step the fleet's state would drive: no order, no stage advance, no
  halt raised on a threshold ("a host did not come back"), no version marked bad, no rollback to `last_good`. A partial fleet
  must not halt and revert a healthy rollout, which is the harm the rule exists to prevent, so verdicts are gated as much as
  orders. What still applies is what an *operator* did, because it only stops things: an operator's halt, hold, force or retire is a
  row transaction (section 6) and is read by every decision. The clock of "did not come back" also restarts: a host's deadline
  counts from the later of its order time and `until_unix`, so a host that was merely unreported during the gap has the whole
  window after it to report (`effective_at = greatest(order.at_unix, cluster_quiet.until_unix)` passed to `halt.rs`). It is evaluated in each deciding transaction, so an instance that started a moment ago and one that
  has run for a week decide alike.
- A deploy of one task changes neither the beats nor the gap, so a deploy no longer quiets the fleet: that is a gain over
  today. Two beats at once that both see the gap both write the same `greatest`: idempotent.

### 5.2 The heartbeat, and where decisions run

The decision code stays the pure functions of `stages.rs`, `live.rs`, `halt.rs`, `queue.rs`, fed rows instead of the `Fleet`
mutex's maps.

Two paths:

- **Converged path** (the common one: the host runs what is wanted, nothing is queued for it, no rollout concerns it). One
  statement, with these parts in one round trip:
  1. the **ordered beat upsert** of the host's `fleet_hosts` row (below);
  2. the host's `fleet_bakes` rows, advanced in SQL from the row's old values under its row lock, in the same statement and
     only when the beat was applied: `beats` is `old_beats + 1` when the beat counts (health `Healthy`, not updating, no
     promotion pending: a function of the beat alone) and the old row has the same `target` and a `at_unix` younger than
     `DOWN_AFTER`, `1` when it counts and the old row does not match, and the row is deleted when it does not count. This is
     `Bake::after` (`bake.rs:42`) written as a `CASE`; the Rust function stays as the reference and a test runs both on the
     same beats (`bake_in_sql_matches_bake_after`). The health that matters is the one the agent sent in the beat, not a
     decision against the desired state, so nothing in it needs the deciding path;
  3. the **release of slots**: `DELETE FROM fleet_orders WHERE host_id = $h AND service = $s AND target = <version the
     applied beat reports>` (the host reached the target; today `Fleet` drops the order on that beat), also only when the
     beat was applied. A slot is also freed by age: `ORDER_AGE` is read as a filter by every reader, and the deciding
     transaction, under the gate, deletes the aged rows. Deleting a row without the gate only frees a slot, so a decision
     that reads the row a moment before the delete is conservative (one fewer order), never wrong;
  4. the gap rule above, the state-version vector (section 6), the host's deny row and the host's queued orders.
  A cache is reloaded only by the rule of section 6 ("Invalidation": rows and version read in one snapshot, so a cache is
  never labelled newer than its content).
  If all are as the instance's caches say, reply. No gate, one round trip. **This path never places an order, a halt or a plan
  step**, so a version vector read here, before an operator's commit, can only delay an order by one beat; it cannot let one
  through (the next paragraph is the path that can). A beat takes a host over from whichever instance heard the one before:
  the row is keyed by the host, so there is no per-instance ownership of a host to hand over.
- **Ordered beat upsert.** An agent dials a new connection per beat and instances vary in latency, so beats of one host reach
  Postgres out of order (beat n stalls on a pool wait in B, beat n+1 is written by A, then B writes n: `running` goes back
  and the bake and orders are wrong). The upsert is `INSERT ... ON CONFLICT (host_id) DO UPDATE ... WHERE
  excluded.sent_unix >= fleet_hosts.sent_unix OR fleet_hosts.seen_unix < now() - $reorder`, and the bake and slot statements
  above run only when it updated or inserted a row (a `RETURNING` CTE). `sent_unix` is the host's own clock in the heartbeat
  (`agent.proto:173`, optional, in seconds; beats are 15 s apart, so a tie means the same beat re-sent and applying it is
  idempotent). The second arm is for a host whose clock stepped back: a beat lower than the stored one is dropped, unless the stored row is
  itself older than `--beat-reorder-secs` (default 30, a config key, below `DOWN_AFTER`), after which the host is believed
  again. An agent that predates the field sends none: its `sent_unix` is stored as 0, so its order is the arrival order, as
  today. A dropped beat still gets its reply (the decision of the current row). Presence is written on every applied beat, about
  4 writes per host per minute; the owner accepts a write per host about every 5 s, so no thinning of this write is designed
  (the fallback of section 7 stays a fallback).
- **Deciding path** (an order, a halt, a plan step may follow). One transaction, READ COMMITTED:

  ```
  BEGIN
    INSERT INTO tx_ledger(id, ...)                          -- first statement, section 4
    SELECT 1 FROM rollout_gate WHERE id = 1 FOR UPDATE SKIP LOCKED  -- a beat; no row back = the gate is busy, ROLLBACK, answer without a new order
                                                            -- (an operator edit: plain FOR UPDATE with its own lock_timeout, section 6)
    one pipelined read of every input, after the lock is granted
    the pure decision; the writes it names, each conditional (section 5.3)
    UPDATE state_versions ... for the parts it wrote (section 6); audit counter row last
  COMMIT
  ```

  `rollout_gate` is a table of one row (`id smallint primary key check (id = 1)`; the row always exists, so no row back from
  `SKIP LOCKED` means "locked"). Its row lock replaces the advisory
  transaction lock the first draft of this document used: the lock is released with the transaction, by Postgres, whatever
  happens to the instance, and it names no instance. After the lock is granted the transaction **re-reads every input of the
  decision from the rows, never from an instance's cache**: the fleet rows (`fleet_hosts`, `fleet_bakes`, `fleet_orders`,
  tries, canaries, queued orders), `cluster_quiet`, the halts, holds, force and retire marks, the desired versions with their
  `set_seq`, the layout, the verdicts and the hosts' deny rows (the parts `desired`, `ops`, `layout`, `verdicts` and the fleet;
  one pipelined batch, each statement taking its own snapshot after the lock, which is what READ COMMITTED gives). It runs the
  pure decision, inserts the order rows, the audit and the plan progress and halts the decision itself raises (rows of `ops`),
  bumps `state_versions` for `ops` when it wrote any, and commits. The caches of section 6 serve the converged path and the
  UI only; they never feed a decision. `READ COMMITTED` is enough **because every writer of an input of a decision takes the
  gate too** (section 6, the lock order): an operator's halt, hold, force, retire, desired change, layout or verdict commit
  either completes before the decision's lock is granted, and then every read of the decision sees it, or it waits for the
  decision's commit and is ordered after it. The deny row is the exception, read under `READ COMMITTED` without the gate: a
  deny that commits after the decision's read cannot be seen by it, which is the same as today's order reaching a host a
  moment before its deny (the host is refused at its next beat).

**What waits, and what fails.** A **beat never queues at the gate**: it asks with `FOR UPDATE SKIP LOCKED`, and when the gate
is held (or an operator edit is queued for it) the beat is answered with the orders already stored for the host and no new one;
the next beat decides. So a busy gate delays an order by at most a beat and never refuses a host, and no beat is ever ahead of an
operator in the lock's queue. (A conditional lock request is also refused while a conflicting request is queued, which
is what keeps a beat from overtaking a waiting operator; the test `beat_never_overtakes_a_queued_operator_edit` runs it against
the service container instead of trusting this sentence.) An **operator edit** of a decision input queues at the gate with a plain
`FOR UPDATE` under its own `SET LOCAL lock_timeout = --operator-gate-wait-ms` (default 30000, far longer than a hold). The only
requests ahead of it are the one holder and earlier operator edits, so a halt raised at a stage start, while hundreds of hosts
beat, waits for one decision, not for the beats. An edit whose wait still times out is refused as `store_failed`, nothing
changed. A deadlock (`40P01`) or serialization failure (`40001`) rolls the transaction back and it is retried in a new
transaction with the same ledger id, within `--ledger-retry-secs` (section 4); the global lock order below means a deadlock
cannot come from this design and would be a bug, but the retry costs nothing.

**Bounds on a hold.** Only a bounded hold keeps "no instance clears anything by a timeout" true for an instance that hangs
rather than dies (a runtime pause, a blocked executor, a CPU-starved Argon2 burst between two statements). Postgres enforces the
bound, on its own clock, on every pool session: `idle_in_transaction_session_timeout = --tx-idle-timeout-ms` (default 5000) and
`statement_timeout = --statement-timeout-ms` (default 5000), set when the session is opened. A holder idle between statements is
terminated by the server after the first, a running statement is cancelled after the second, and either rolls the transaction back
and releases the gate and every row lock. The stalled process finds a dead session when it resumes (`Lost`, then `Reviving`,
section 4), and its ledger-id retry lands nothing twice. The longest the gate can be held by an instance that has gone silent is
therefore the idle timeout; every edit and decision waits at most that long for it. (`transaction_timeout` of newer Postgres
would bound the whole transaction; it is not relied on, because the version of the Postgres service was not checked, section 13.)

**Isolation.** No transaction of this design needs `SERIALIZABLE`. The deciding transaction would pay for it and gain nothing:
it reads most of the fleet rows, and under serializable snapshot isolation every concurrent beat upsert would be a read-write
conflict with it, so decisions would abort and retry under normal load, where the gate gives the same guarantee by exclusion.
Everywhere else a decision is one conditional write whose `WHERE` is its precondition. `SERIALIZABLE` with a retry loop is the
fallback for a future decision that cannot name its precondition; none does today.

**One row lock is still the right granularity** (question 2). The gate protects the one-service-at-a-time rule and the FIFO,
which read every service's rows, and `--live-rollouts N` (at most 16) counts live services over every group. Only the live
services step, so a lock per service would let at most N decisions run side by side, at the price of a second lock and a
second order for the admission decision, which needs the cluster-wide row anyway. The deciding path runs only while something
rolls, its transaction is a few statements, and only operator edits queue at the gate (a beat that finds it busy is answered and decides at its next beat), which
does not depend on how many instances there are: more instances spread the same decisions, they do not add any. A finer lock
is an optimisation to add if section 7 shows the wait (B20: no speculative machinery).

```mermaid
sequenceDiagram
    participant H as Host agent
    participant E as Edge (envoy)
    participant A as Instance A
    participant B as Instance B
    participant P as Postgres
    H->>E: heartbeat (new connection)
    E->>A: tcp, TLS untouched
    A->>P: upsert fleet_hosts + read versions, deny, queued (1 statement)
    P-->>A: versions unchanged, not denied, nothing queued
    A-->>H: reply, no order
    Note over H,E: 15 s later
    H->>E: heartbeat (new connection)
    E->>B: tcp, TLS untouched
    B->>P: upsert + read (1 statement)
    P-->>B: desired version changed, host is behind
    B->>P: BEGIN; ledger; SELECT rollout_gate FOR UPDATE
    B->>P: SELECT fleet rows, quiet, halts, holds, desired, layout, verdicts (no cache)
    B->>B: pure decision (stages, live, queue)
    B->>P: INSERT fleet_orders, plan/halt rows, state_versions(ops), audit; COMMIT
    B-->>H: reply with order
    Note over A,B: an instance that crashes after COMMIT before the reply: the host beats again,<br/>the next instance finds the order row inside ORDER_AGE and returns the same order
```

**One service at a time, FIFO.** `set_seq` comes from the sequence `desired_seq`, taken in the transaction that sets a desired
version, so the queue is a total order on every instance. The existing tie-breaks (`queue.rs:10-12`) remain for rows with no
`set_seq` (written by the release before). The one-live-rollout check reads `fleet_orders` and the plan rows inside the
gate's transaction, so two instances cannot each see "none live".

**Halts are storage-first.** A halt is raised by the deciding transaction that sees the failure, or by an operator call (a
row transaction, section 6). An operator halt takes the gate (section 6), so it commits entirely before or entirely after a
deciding transaction. Every deciding transaction reads the halt rows after it holds the gate, so a halt is global from its
commit and no order whose transaction takes the lock after it is placed against it; an order that committed before the
halt's lock was granted was placed before the halt was acknowledged. The marker file and the `pending` retry queue (`halt.rs:15-22`,
`ops.rs:500-512`, `ops_pending.rs`) are not used with Postgres; the file store keeps them until C9. The guarantee memory-first
gave ("a halt stops the service even with the database slow or down") is kept in the other way: with the database down no
instance orders (principle 4). With it slow, the halt waits behind a deciding transaction for milliseconds, and no order can pass it.

**Other heartbeat caches** (what each host runs, repository reports, facts heard, alerts' `seen`): columns of `fleet_hosts`
or the `alerts` table (section 9), written by the same upsert only when they changed. Counters that are only diagnostics
(`extras_dropped`, `service.rs:257`) and held `WaitConverged` readers (`wait.rs:66`) stay per instance; `WaitConverged` answers
from shared rows by polling the version vector.

### 5.3 Rollout steps without a leader

Every step below is one transaction of the deciding path. Steps the fleet's state drives (an order, a stage advance, a halt on a
threshold, a version marked bad, a rollback) run only when `cluster_quiet` is not in the future (section 5.1); a halt, hold, force or
retire that an operator asks for is not gated by it. The gate serializes the decisions; the conditional write is what
keeps the step correct even if two instances reached it with the same inputs, and what says which of them did it, so the
audit line and the order are written once. "Any N" holds because nothing below names an instance.

| Step | Transaction (after the gate, from fresh rows) | Racing instances are resolved by | Written once by |
|---|---|---|---|
| **Choose the next hosts of a stage** (an order for the beating host) | count live services, the stage's updating hosts, `max_updating` and `min_up` from `fleet_orders`, `fleet_hosts`, the plan rows; if the host may be ordered, `INSERT INTO fleet_orders` | the gate (the second decider reads the first's order and its slot); unique index on `fleet_orders(host_id)` (one update at a time on a host) as backstop; a beat that finds its own order row inside `ORDER_AGE` is answered with that order | the order row |
| **Advance a stage** (50% to 100%, or the first stage) | outside the quiet window; the beat finds the stage done (every host up at the version and baked); `UPDATE plan SET stage = $n + 1 WHERE group = $g AND service = $s AND version = $v AND stage = $n` | compare-and-set on the stage number: zero rows means another beat advanced it, and this one re-decides from the new stage. `Rollouts.Advance` is the same update with the stage the operator saw; zero rows is `not_waiting` | the `rollout_advance` audit line, inserted only when the row count is 1 (name `coordinator`, or the operator's) |
| **Halt** (the failure threshold, only outside the quiet window; or an operator, always) | read the verdicts and the live hosts; `UPDATE halts SET ... generation = generation + 1 WHERE (group, service) AND NOT halted` | the gate for the operator's call (it takes the gate, section 6); the `WHERE NOT halted` for two deciders that both see the threshold crossed: one row, one generation (`halt-v1`), one audit line | the row count of the update |
| **Roll back first, then delete** | only outside the quiet window; in the transaction that marks a version bad: `desired` is set back to `last_good` with a new `set_seq`, the plan restarts. An uninstall (a package removed from a group, a `Move`) is decided from rows read after the gate, so it sees the rollback committed or waits behind it; and the rule order of `decide.rs` (halt and bad version before the uninstall rules) is unchanged | the gate: the rollback's transaction and the uninstall's cannot interleave, so no instance orders a delete of what another is rolling back | the rollback's own audit line, written with the desired change |
| **One service at a time, FIFO** | the queue is not state: the head is derived in the transaction from `set_seq` of the desired rows and from the live count, `--live-rollouts` over every group (`live.rs`) | the gate, and `desired_seq`, a sequence taken in the transaction that sets a desired version | none needed: nothing is written until an order is |
| **Cluster quiet** | the gap rule of section 5.1 | `greatest(until_unix, ...)`, idempotent | the row |
| **Release polling, certificate issuance** | not coordinator loops (section 9) | not applicable | not applicable |

The host's queued orders (`fleet_queued_orders`) are delivered by the beat of that host and re-sent until the host reports
the order done, as today; a reply lost with its instance is the same case as a crash after `COMMIT` in the diagram below.

```mermaid
sequenceDiagram
    participant H1 as Host h1 (service s1)
    participant H2 as Host h2 (service s2)
    participant A as Instance A
    participant B as Instance B
    participant P as Postgres
    Note over P: --live-rollouts 1, nothing live, s1 set before s2
    H1->>A: heartbeat
    H2->>B: heartbeat (same moment)
    A->>P: BEGIN; ledger; SELECT rollout_gate FOR UPDATE
    B->>P: BEGIN; ledger; SELECT rollout_gate FOR UPDATE SKIP LOCKED
    P-->>A: lock granted
    Note over P,B: B asks FOR UPDATE SKIP LOCKED: no row back, the gate is busy
    B->>P: ROLLBACK
    B-->>H2: stored orders only, no new order (s2 decides at its next beat)
    A->>P: read every input; decide: s1 is the head, order h1
    A->>P: INSERT fleet_orders(h1, s1); audit; COMMIT
    P-->>A: committed (lock released)
    A-->>H1: order for s1
    Note over H2,B: 15 s later, the next beat of h2 reaches any instance
    H2->>B: heartbeat
    B->>P: BEGIN; ledger; gate FOR UPDATE SKIP LOCKED: granted
    B->>P: read every input (sees A's order: s1 is live)
    B->>B: decide: s2 waits, rollout_elsewhere
    B->>P: COMMIT (no writes)
    B-->>H2: no order
```

```mermaid
sequenceDiagram
    participant A as Instance A (beat of h3)
    participant B as Instance B (beat of h4)
    participant P as Postgres
    Note over P: stage 1 of 2 is done and baked, stage 2 is auto
    A->>P: BEGIN; gate FOR UPDATE; read; stage 1 is done
    B->>P: BEGIN; gate FOR UPDATE SKIP LOCKED: no row, busy; ROLLBACK
    A->>P: UPDATE plan SET stage = 2 WHERE stage = 1 -> 1 row
    A->>P: audit rollout_advance (coordinator); order h3; COMMIT
    Note over B: B answers h4 with no new order; at h4's next beat B decides again
    B->>P: BEGIN; gate granted; read; the plan is at stage 2, h4 is in it
    B->>P: order h4 (no advance needed); COMMIT
    Note over A,B: if the gate were bypassed, B's update would match 0 rows: no second audit line, no second advance
```

```mermaid
sequenceDiagram
    participant A as Instance A (beat with a failed verdict)
    participant B as Instance B (beat of h7)
    participant O as Operator
    participant P as Postgres
    A->>P: BEGIN; gate FOR UPDATE SKIP LOCKED: granted; quiet is not in the future; read verdicts: threshold crossed
    B->>P: BEGIN; gate FOR UPDATE SKIP LOCKED: no row, busy; ROLLBACK
    O->>P: Halt.Raise: BEGIN; gate FOR UPDATE (queues behind A; no beat can pass it)
    A->>P: version marked bad; halt row (generation + 1); desired = last_good, new set_seq; audit; COMMIT
    P-->>O: lock granted after A commits; the operator's halt reads A's halt and is ordered after it
    O->>P: write (already halted: no second generation); COMMIT
    Note over B: h7 beats again; B decides after the operator
    B->>P: BEGIN; gate granted; read: halted, desired is last_good
    B->>B: decide: no order for the bad version; h7 gets what last_good says
```

When the database is unreachable no step runs anywhere (principle 4):

```mermaid
sequenceDiagram
    participant H as Host agent
    participant A as Instance A
    participant P as Postgres
    H->>A: heartbeat
    A->>P: upsert / BEGIN
    P--xA: connection refused or timeout
    A-->>H: unavailable (no order, no halt, no plan step); /healthz 503
    Note over H: the host keeps what it runs and retries its heartbeat
    Note over A,P: when Postgres answers again, the gap rule (section 5.1) quiets every decision of its own for --quiet-secs
```

The same gap seen by a deciding beat: the partial fleet raises no halt.

```mermaid
sequenceDiagram
    participant H as Host h1 (first to beat after the gap)
    participant A as Instance A
    participant P as Postgres
    Note over P: Postgres was away 60 s; fleet_hosts is stale (or truncated)
    H->>A: heartbeat
    A->>P: upsert; max(seen_unix) is older than DOWN_AFTER: cluster_quiet.until = now + quiet
    A->>P: BEGIN; gate (SKIP LOCKED) granted; read cluster_quiet: in the future
    A->>A: partial fleet: hosts 'not back' are not judged; no order, no advance, no halt, no rollback
    A->>P: COMMIT (no writes)
    A-->>H: stored orders only
    Note over A,P: an operator's halt in this window is still taken and read by every decision (it only stops things)
```

## 6. Everything else: row transactions and cache versions

**Edits (desired, ops, accounts, layout, verdicts).** The in-memory copy becomes a read cache, and a write is:

```
BEGIN
  INSERT INTO tx_ledger(id, ...)                              -- first statement, section 4
  SELECT 1 FROM rollout_gate WHERE id = 1 FOR UPDATE          -- the gate, only for the parts a decision reads (desired, ops, layout, verdicts);
                                                              -- queues, under SET LOCAL lock_timeout = --operator-gate-wait-ms
  SELECT version FROM state_versions WHERE part = $1 FOR UPDATE  -- the part's row lock; a fixed order when an edit spans parts
  reload the part from its rows                               -- fresh, under the lock
  apply the existing edit closure to the fresh copy
  diff before/after, write the changed rows (the code of rows.rs), each conditional on what the closure read
  UPDATE state_versions SET version = version + 1 WHERE part = $1
  audit, if any (the audit counter row, last)
COMMIT
```

This is the `writing` mutex of today moved to Postgres, and the closure and the diff are the code that exists, so the call
sites do not change. Every lock here is a row lock released by Postgres at the end of the transaction, so an instance that
dies mid-edit holds nothing. A part is the unit of lock, cache and version; the parts are `desired`, `ops`, `accounts`,
`layout`, `verdicts`, `enrollment`. **The one lock order, global:** the gate row, then the `state_versions` rows of the parts
in the order of that list, then the `audit_counter` row last (section 6, audit numbers). Every transaction that takes more than
one of them takes them in this order and none takes a row twice or in another order, so there is no cycle. An edit of
`desired`, `ops`, `layout` or `verdicts` (set desired, hold, halt raise and clear, force, retire, layout, verdict) takes the
gate first: those are the inputs of a decision (section 5.2), and this is what makes a halt acknowledged to an operator
outrun-proof. An edit of `accounts` or `enrollment` does not (no decision reads them under the gate). A flow across parts
(force, retire) takes their rows in the fixed order of that list. A reload per edit costs one part's rows (operator edits are
rare; measured in section 7). `if_generation` on `SetDesired` (migration `0003`) is unchanged and still the caller's
optimistic check.

**Invalidation.** `state_versions(part pk, version bigint)` is read by the heartbeat's statement (section 5.2) and, for the UI
and the API, by one `SELECT` per request; a changed version reloads that part, an unchanged one reads from memory.

**Reload rule (a cache is never newer than its content).** A part's rows and its version are read in **one snapshot**, with
the version read first: one statement (its CTEs share a snapshot) or one `REPEATABLE READ, READ ONLY` transaction that selects
`state_versions.version` and then the part's rows, and the cache is labelled with *that* version. Every writer bumps the
version in the transaction that writes the rows (a rule above), so a snapshot that holds version `v` holds exactly the rows
of `v`, and a snapshot older than a commit holds neither the new rows nor the new version. The bad order is two reads: rows
of `v`, then a commit of `v+1`, then a read of the version that labels the old rows `v+1`; every later beat would see the
version equal and take the converged path, and a host behind the new desired version would not be ordered until some other
edit bumped the part. With one snapshot that cache is next compared with `v+1` in the database, differs, and reloads. The
converged path never decides, and a deciding transaction re-reads rows and so was never exposed; the rule closes the one place
where staleness would otherwise have no bound. Test: `cache_is_never_labelled_newer_than_its_content` (a commit between the
two reads, injected). No
`LISTEN/NOTIFY`: it needs a dedicated connection and a poll fallback anyway (B20). The bound on staleness is zero for the
decision (it reads rows, not caches), at most one beat for the converged path (which cannot order), and one request for the
UI. Every transaction that writes rows of a part bumps its `state_versions` row, the deciding transaction included (the
`ops` rows it writes: halts, plan progress).

**Enrollment.** `RecordsLock::in_process` (`records.rs`, `enroll/store.rs:124`) goes. Approve, reject, renew and expire become
conditional writes in a transaction: `UPDATE enrollment_requests SET state='approved' WHERE request_id=$1 AND
state='pending'` and zero rows means the other instance won; a host or request row is read `FOR UPDATE` when a check precedes the
write (`store.rs:203, :252, :290, :466, :496`). `take_serial` is already one SQL statement (`records.rs:675`). The `Copy` of
hosts and deny rows becomes a cache under the `enrollment` version. **Deny rows are read fresh on every heartbeat**, in the
heartbeat's one statement (a join on the host id, an indexed read): a denied host is refused by every instance from the commit,
at no extra round trip (decided, section 19).

**Audit numbers.** The audit's `seq` must be dense and in commit order, because `Audit.List` pages by `seq > $1` and a reader
that has seen 11 must not miss a 10 that commits later. A sequence does not give that (a transaction takes 10, another takes
11 and commits first). So the number is taken by `UPDATE audit_counter SET next = next + 1 RETURNING next - 1` as the last
write before `COMMIT`: the row lock is held until the commit, so numbers follow commit order, and a rollback leaves no gap.
Only transactions that write audit lines take it (operator edits, orders, halts, not the converged heartbeat). This keeps
the decision of coordinator-storage.md section 14 ("no gaps"), which only needed its lock replaced. The column and primary key
are unchanged; the release before reads `max(seq)` at start and continues from it. **Return after a back-out:** the release
before ignores `audit_counter` and writes `max(seq) + 1` on, so the counter is stale when the new release comes back. At
start, after taking the shared lock (no writer of the release before can run then) and before serving, an instance runs
`UPDATE audit_counter SET next = greatest(next, (SELECT coalesce(max(seq), 0) + 1 FROM audit))`: idempotent, monotonic, and
serialized with other instances' audit writers by the row lock. The same start step re-stamps the queue (section 16). The facts generation of a host (today the
audit number at open plus an in-memory raise, `facts.rs:100-115`) becomes a column `enrollment_hosts.facts_generation`
(default 0) raised in the transaction that changes the host's labels or group, taking at least the audit number as the
release before does, so every instance answers the same number and a flip does not refetch facts.

**Spent CI tokens.** The insert's result (`ON CONFLICT DO NOTHING`, rows recorded) decides; the in-memory set is a cache that
can only refuse earlier (`store/mod.rs:296`). No new table.

## 7. Latency budget

Counts are round trips on the unix socket (the Postgres socket directory), the rows locked and how long they are held. **They are design
counts (inferred from the flows above), not measurements.** The measurement is part of PR M1 (section 17) on the deployment: round-trip
time of the statements, the wait on the gate under a 50% stage of the fleet, WAL and vacuum of `fleet_hosts` at 4 upserts per
host per minute for the real host count (not read in this work). The hold time of the gate is the span from its grant to
`COMMIT`: the read batch, the write batch and one WAL flush; M1 states it in milliseconds.

| Operation | Round trips | Rows locked | Lock hold | Today |
|---|---|---|---|---|
| Heartbeat, converged | 1 (upsert + versions + deny + queued) | the host's own `fleet_hosts` row, its bake and order rows | one statement | 0 |
| Heartbeat, a cache is stale | 1 + 1 reload of the changed part | as above | as above | 0 |
| Heartbeat, deciding (order, stage advance, halt, plan step) | 1 for the beat, then `BEGIN` + ledger + gate (1), one pipelined read of every input (1), writes + versions + audit + `COMMIT` (1): 4 | the gate row; the order and plan rows it writes; `state_versions('ops')` when it writes `ops` rows; the `audit_counter` row for the last statement | gate: grant to commit, about 2 round trips and one commit flush | 0 |
| Heartbeat that finds the gate busy | 1 + `BEGIN`/ledger/gate attempt/`ROLLBACK` (pipelined, 1) | none held (`SKIP LOCKED`) | none, never waits | 0 |
| Operator edit of desired, ops, layout, verdicts (hold, halt raise/clear, force, retire) | `BEGIN`, ledger, gate, part row, reload part, write, version, audit, `COMMIT`: 3 to 4 (pipelined) | the gate row, the part's `state_versions` row, `audit_counter` | gate: waits for the one holder (at most `--tx-idle-timeout-ms`, normally milliseconds) and earlier operator edits, never for a beat; then held to commit | 1 write session |
| Operator edit of accounts, enrollment (user, token) | as above without the gate | the part's `state_versions` row, `audit_counter` | to commit | 1 write session |
| UI or API read | 1 (versions) + 1 reload only when changed | none | none | 0 |
| UI login / request | 1 read of the session row, + 1 write at most every 30 s | the session row | one statement | 0 |
| Enrollment approve / renew | 2 to 3 (`FOR UPDATE`, conditional update, audit) | the request or host row | to commit | 2 transactions |
| Periodic job claim (section 9), per instance per tick | 1 | the due `periodic_jobs` rows (skip locked), usually none | one statement | 0 |
| Instance start | migration lock + migrations (one transaction, idempotent, `lock_timeout` on DDL), shared lock, load all parts | the migration advisory lock (seconds, released at commit); the shared advisory lock | migration lock: the migration transaction; shared lock: the instance's life | same plus exclusive lock |

The gate is held by one decision at a time, and what queues behind it is the hosts that beat at that moment and the operator
edits, whatever the number of instances. If the heartbeat's converged path is more than a few hundred microseconds, the
fallbacks are in this order: the narrow row and `fillfactor` (done by design), a beat that changes nothing but `seen_unix`
skips the write when the stored `seen_unix` is under a few seconds old (a config key), then a per-instance read cache of
`nodes` with a 1 to 2 s TTL (config key). A hot gate row is measured (operators queue on it too), and only then a finer lock
considered (question 2).

## 8. Sessions and the login limiter

`session.rs` says "a restart ends every session, because nothing is written anywhere". Designed: a table `sessions(id_sha256
bytea pk, user, csrf, created_unix, last_used_unix, serial)` (never the raw cookie id: the key is the SHA-256 the memory table
uses now, `session.rs:163`), and `login_failures(name pk, failures, last_failure_unix, locked_until_unix)`. The idle and
absolute deadlines and the cap on the number of sessions keep their rules, in SQL; `last_used_unix` is written at most once
per 30 s per session. The cookie, its flags and the CSRF token do not change, so nothing browser-visible changes, and a
deploy no longer signs anyone out. Argon2id hashing stays per instance (`password.rs:320`, a CPU bound).

```mermaid
sequenceDiagram
    participant U as Operator browser
    participant A as Instance A
    participant B as Instance B
    participant P as Postgres
    U->>A: POST /login (name, password)
    A->>P: read login_failures(name); check lock
    A->>A: Argon2id verify (this instance's CPU)
    A->>P: INSERT sessions(sha256(id), csrf, ...); clear failures
    A-->>U: Set-Cookie (unchanged format)
    U->>B: GET /ui/ (cookie)
    B->>P: SELECT sessions WHERE id_sha256 = sha256(cookie)
    P-->>B: row (idle/absolute deadlines checked)
    B-->>U: page
```

## 9. Periodic work without a leader

Two kinds. **Effects** (an audit line, an alert line, an expiry, an adoption) are made exactly once by a conditional write, on
any number of instances. **Scans** (the work that finds those effects) are claimed per job by advancing a due time, so that
N instances do not all scan at once. Correctness rests on the conditional write; the claim only saves work, and a lost claim
costs one period.

**The claim.** One table, `periodic_jobs(name text primary key, next_run_unix bigint not null, last_run_unix bigint,
last_instance text)`, one row per job, seeded by `INSERT ... ON CONFLICT DO NOTHING` at start. Every instance ticks (a short
config key) and claims what is due in one statement:

```
WITH due AS (SELECT name FROM periodic_jobs WHERE next_run_unix <= extract(epoch FROM now())
             ORDER BY next_run_unix FOR UPDATE SKIP LOCKED)
UPDATE periodic_jobs SET next_run_unix = extract(epoch FROM now()) + $period, last_run_unix = extract(epoch FROM now()),
       last_instance = $me
WHERE name IN (SELECT name FROM due) RETURNING name
```

`SKIP LOCKED` makes a second claimer skip a row the first holds, and a claimer that waits instead (a plain `UPDATE ... WHERE
next_run_unix <= now()` on one row) re-checks the `WHERE` after the first commits and updates nothing: either way one instance
gets the job for the period. This is not a lease: the row has no expiry and no holder to be replaced. An instance that dies
after claiming loses that run, and the job runs again one period later from any instance; every job below is level-triggered
(it derives its work from the rows) and idempotent, so a lost or repeated run changes nothing but timing. `last_instance` is
for the status page.

| Work | Now | Designed (every instance ticks; the claim picks one; the effect is conditional) |
|---|---|---|
| Alert observation, every 15 s (`alerts.rs:838`) | logs `alert=` once per first sight, `Book.seen` in memory | job `alerts`. Derive from the shared rows; `INSERT INTO alerts(subject, first_unix) ... ON CONFLICT DO NOTHING RETURNING`: the instance that inserts logs the line, once; a cleared alert is `DELETE ... RETURNING`. `Alerts.List` reads the table. The watcher of `alert=` lines must read the service's logs, not one instance's |
| Request expiry (`expiry.rs:112`) | audits `request_expired` | job `request-expiry`. `UPDATE ... SET state='expired' WHERE state='pending' AND posted_unix < $1 RETURNING request_id`, audit the returned rows in the same transaction: exactly once |
| Pruning: `tx_ledger` older than an hour, expired `sessions`, old `login_failures`, `instances` not seen for a day, aged `fleet_orders` | none or per process | jobs `prune-*`: `DELETE ... WHERE ...`, idempotent |
| TLS, tokens file, client CA reload (`listen.rs:402-441`) | per process | per instance, unchanged (the files are the same Swarm secrets); not claimed, because each instance must reload its own |
| Server certificate renewal (`enrollment/mod.rs:220`) | per process | per instance, on its own tmpfs (section 10); not claimed |
| Host certificate issuance (Enroll, Poll, Renew) | request-driven | not a loop: a transaction, `UPDATE enrollment_requests SET state='approved' WHERE request_id=$1 AND state='pending'`, serial from `take_serial` (one SQL statement); zero rows means another instance won and the caller reads its result |
| Initial-admin file (`initial_admin.rs:480`) | adopts the file, audits `user_bootstrap`, deletes the file when expired | adoption is a conditional upsert keyed by the file's `id` (the "record already holds this id" rule exists today) with the audit row only when the upsert changed a row; a failed delete (`ENOENT` or read-only) counts as done. The volume stays mounted by every instance for this file alone (decided: keep it for now) |
| Rollout driver, queue, halt evaluation | per heartbeat, in the fleet lock | per heartbeat, in the transaction of section 5.2 and the steps of 5.3; no loop. A stage that completes while no host beats is not advanced today either (`service.rs:595`); if a periodic advance is later wanted it is a job here that runs the same compare-and-set advance |
| GitHub release polling | does not exist in the coordinator: CI pushes `Releases.SetDesired` with a GitHub OIDC token (`api.github.com` is read only by the `xtask` update tooling, not by the service) | nothing to run. If a poller is ever added to the coordinator it is one more `periodic_jobs` row whose effect is a conditional `SetDesired` |

```mermaid
sequenceDiagram
    participant A as Instance A
    participant B as Instance B
    participant C as Instance C
    participant P as Postgres
    Note over P: job alerts is due (next_run <= now)
    A->>P: claim due jobs (FOR UPDATE SKIP LOCKED, advance next_run)
    B->>P: claim due jobs
    C->>P: claim due jobs
    P-->>A: alerts claimed
    P-->>B: nothing due (row locked, skipped)
    P-->>C: nothing due (next_run advanced)
    A->>P: scan; INSERT alerts ... ON CONFLICT DO NOTHING RETURNING
    P-->>A: new subject: log alert= once
    Note over A,P: A dies after the claim: the run is lost, the next one is due one period later on any instance
```

## 10. The CA key, the server certificate, secrets

- **CA key and cert.** Today files on the local volume (`--ca-key-file`, and `<ca-key-file>.cert` beside it). Only `--ca-init`
  writes them, and it refuses when either exists (`ca/key.rs:54, :100-123`); `serve` reads them. The key check is mode
  `& 0o077 == 0` and owner uid (`ca/key.rs:42`), which a Swarm secret with `mode: 0400` and the task's uid passes. Designed:
  a Swarm secret `hk-ca-key-v1` at `/run/secrets/hk-ca-key-v1` and the certificate (not secret) as a Swarm config mounted with `target: /run/secrets/hk-ca-key-v1.cert`, so
  `--ca-key-file=/run/secrets/hk-ca-key-v1` finds its pair; `--ca-init` is not
  passed. Created by one reversible step of the infrastructure repository's `cp-edge` flow that copies the existing bytes,
  with the SHA-256 recorded before and after and never printed; the volume's copy stays until the soak ends.
- **Server certificate.** Each instance makes its own, with its own new key, from the shared CA, on a per-instance tmpfs (`/run/hk`):
  no two instances write one file. Hosts pin the CA hash, not the server key (`hostkeeper-link/src/grpc.rs:162`), and every
  certificate names the same `--server-name`s, so the edge's SNI match is unchanged. Serials are unique because they come
  from `enrollment_serial` in SQL.
- **Static config** (`hk-oidc-v4`, `hk-tokens-v1`, `hk-tls-cert-v1`, `hk-tls-key-v1`, `hk-client-ca-v1`, `hk-dsn-v1`) is per
  service, identical for every instance by construction. One deploy check (B20: fix at the source): a credential-file path that
  a repository row names (`repository_serve.rs` reads it at every request) is a Swarm secret of the stack. Whether any live
  repository uses one was not checked (needs an operator token).
- **The state volume** after this: `initial-admin.toml` only. `halt-unfinished` has no use under Postgres, and the MDM files
  go with the MDM (the retired `--mdm-*` flags are still passed and ignored by `main`; they and the host-mode 443 publish
  leave the stack with that removal, not here).

## 11. Routing, health, deploy

Built today (stack file and the edge's `cds.yaml`/`lds.yaml`, as read, not changed here): agents dial the edge on the
tailnet name at 8443 (heartbeat, mutual TLS) and operators and CI at 8445 (private surface and UI); the listeners match the
SNI and `tcp_proxy` to clusters `hk-heartbeat`, `hk-private`, `STRICT_DNS` on `tasks.hostkeeper-coordinator_server`,
`dns_refresh_rate` 1 s, `LEAST_REQUEST`, with **no active health check** ("there is one task"). TLS is untouched, so the
coordinator sees the client certificate. So 8443 and 8445 already spread over every task Swarm lists.

Needed:

1. **An active health check** on both clusters, as the existing frontend's clusters have (`/readyz`, `close_connections_on_host_health_failure:
   false`). The coordinator's `/healthz` is on `127.0.0.1:8446` inside the task (`health.rs`): a new additive flag
   `--health-listen` serves the same path on an address of the overlay `cp-hk`. 200 only when the store is `Up`, the
   listeners are bound and the task is not draining.
2. **Update shape:** `replicas: 1`, `order: start-first`, `parallelism: 1`: one instance in steady state, two only while the
   old one drains. The Docker healthcheck says when the new one is healthy in its own container (as now: 5 s
   interval, `start_period` 120 s), and Swarm then sends the old task SIGTERM. That is not the moment the *edge* can use the new
   task: it learns of it through `STRICT_DNS` (1 s refresh) and then needs its own active check to pass `healthy_threshold`
   times at `interval`. "At least one task answers 200" is a claim about tasks; what matters is that the edge has a healthy
   upstream that accepts. So the old task gets a **drain delay**: from SIGTERM it answers `/healthz` 503 (so the edge stops
   choosing it once it has the new one) but **keeps its listeners open and keeps serving new connections for
   `--drain-delay-secs`**; only then does it stop accepting, finish in-flight calls and exit, all inside `stop_grace_period`.
   Long calls (`WaitConverged`) end at the end of the delay and callers retry, as they do at a deploy today.
   The deploy checks, once (B20), that `--drain-delay-secs > dns_refresh_rate + healthy_threshold x interval + one interval` of
   the edge's new check and that `stop_grace_period > --drain-delay-secs + the longest in-flight call`. Proposed values: check
   `interval` 2 s, `healthy_threshold` 2, `unhealthy_threshold` 2 (the edge's check does not exist yet; these are mine, to be
   confirmed with the edge's owner), DNS 1 s, so the new task is usable in about 1 + 4 + 2 = 7 s, and `--drain-delay-secs` 15,
   `stop_grace_period` 60. During the delay both tasks accept, so there is no instant when a host or an operator is refused or
   the edge routes to nothing healthy (Envoy's panic mode never has to engage). The Mac listener (item 4) is the one port the
   old task still holds through the delay, which is why it is open.
3. **No stickiness.** An agent opens a new connection per heartbeat, so a host's beats alternate between instances. Source-IP
   affinity would not help: `min_up` and one-live-rollout are cluster-wide. The shared fleet (section 5) is what makes this
   correct. Beyond TCP keep-alive there is no stickiness, and a connection ends at a server-side maximum age
   (`--connection-max-age-secs`), so none outlives a drain by long.
4. **The Mac listener (open).** The LAN listener (host-mode publish of 8446 to task port 8447, for Macs) cannot be bound by two
   tasks on one node, so it also stops a start-first update from starting the new instance while the old one holds the port.
   Options: (a) the edge serves it as one more SNI chain and cluster, `tcp_proxy` with TLS untouched, so
   the certificate survives; (b) the ingress routing mesh on the service VIP (TCP, TLS untouched, so the certificate should
   survive; not verified); (c) a second one-task service for it, which is a singleton again. Recommendation (a), the edge
   already is the front door (open question 1).
5. **Postgres placement.** The database is reached over the unix socket bind mount of the Postgres socket directory, so every instance runs
   on the one-node Swarm (the placement constraint already holds).
6. **The first cutover is stop-first.** The release before holds the exclusive lock; a start-first instance would be refused. Every
   later update is start-first, which requires that every migration a release adds is `may-run` (the release before must run
   on the new schema), or that update is stop-first.

```mermaid
sequenceDiagram
    participant S as Swarm
    participant O as Old task (release n)
    participant N as New task (release n+1)
    participant E as Edge health check
    participant P as Postgres
    S->>N: start (start-first)
    N->>P: migration lock (xact advisory); migrations (may-run, lock_timeout on DDL); commit
    N->>P: shared lock, load
    N-->>E: /healthz 503 starting
    N-->>E: /healthz 200
    Note over S,N: Docker healthcheck of N passes
    S->>O: SIGTERM
    O-->>E: /healthz 503 draining (listeners stay open and serving)
    Note over E,N: STRICT_DNS (1 s) sees N; active check passes healthy_threshold times
    E->>N: begin sending connections
    E->>O: stop choosing O (503, unhealthy_threshold)
    Note over O: --drain-delay-secs elapsed (greater than DNS + healthy_threshold x interval + one interval)
    O->>O: stop accepting, finish in-flight, exit within stop_grace_period
    Note over S,P: the edge has a healthy, accepting upstream at every moment
```

## 12. Configuration (every number is a key with a default)

| Key | Default | Meaning |
|---|---|---|
| `--database-pool` | 4 | write/decision sessions per instance |
| `--instance-beat-secs` | 5 | `instances.seen_unix` |
| `--health-listen` | none | readiness address on `cp-hk` |
| `--cache-nodes-ttl-ms` | 1000 | per-instance cache of read-only parts of the fleet, if section 7 needs it |
| `--session-touch-secs` | 30 | write of `last_used_unix` at most this often |
| `--beat-reorder-secs` | 30 | a beat older (by the host's `sent_unix`) than the stored one is dropped unless the stored row is older than this |
| `--ledger-retry-secs` | 10 | how long an instance retries a transaction of unknown outcome, a deadlock or a serialization failure before it answers 503 |
| `--operator-gate-wait-ms` | 30000 | how long an operator edit waits for the gate row (a heartbeat never waits: `SKIP LOCKED`); then it is refused `store_failed` |
| `--tx-idle-timeout-ms` | 5000 | `idle_in_transaction_session_timeout` of every pool session: the longest a silent instance holds the gate or a row lock |
| `--statement-timeout-ms` | 5000 | `statement_timeout` of every pool session |
| `--drain-delay-secs` | 15 | from SIGTERM, the old task answers 503 on `/healthz` but keeps accepting for this long; deploy-checked against the edge's DNS refresh and check interval x thresholds |
| `--migrate-wait-secs` | 60 | how long a starter waits for the migration lock |
| `--migrate-ddl-lock-timeout-ms` | 3000 | `lock_timeout` of a migration transaction: a DDL that cannot get its lock rolls the migration back |
| `--migrate-retry-secs`, `--migrate-attempts` | 5, 6 | the wait between and number of attempts of a rolled-back migration before the instance exits |
| `--job-tick-secs` | 5 | how often an instance claims due periodic jobs (section 9) |
| `--job-every name=secs` (repeatable) | `alerts=15`, `request-expiry=15`, `prune=3600` | the period of each periodic job |
| `--connection-max-age-secs` | 300 | a server-side maximum age of an agent connection, besides TCP keep-alive |
| existing: `--quiet-secs` (floor in `config.rs:512`), `--tls-reload-secs` | unchanged | the cluster's quiet rule uses `--quiet-secs` |

The flags are additive; no flag is removed or renamed. `--halt-marker-file` is accepted and ignored with Postgres.

## 13. js-server, and what was not checked

**js-server** (a Swarm service, one replica, stop-first, state in a `--state-file`
on its volume and a `--store` URL secret) is a different codebase and was only seen by its flags. The
owner's direction covers it, and so does the leaderless decision: Postgres the single source of truth, n schedulers that just
work, no leader or lease of its own. It gets its own design in its repository, from this document's checklist: what is only in
memory or on the instance's disk, what two instances decide together (here a lease in the name of its state file is the likely
one, and under this decision it becomes rows and conditional writes), what a deploy does, how callers find it. This pull
request does not design it. Decisions already taken for it, carried here so they are not lost: no agent stickiness beyond TCP
keep-alive plus a server-side connection max age; the job-store cutover is a short pause with a dry-run copy and a
Postgres-to-SQLite export tool for rollback; a hello with the same credential and agent instance takes the host over at once;
losing one unflushed 5-minute usage buffer when an instance dies is accepted (writing usage per event is better if it is
cheap).

Not checked, because access was denied or needs a secret, and to be confirmed by the owner or engineering before the cutover:
the files of the state volume (names and sizes are from the service's flags); the Postgres socket directory;
`max_connections` of the Postgres service and the role's connection limit (needs the DSN); live table contents (including the
number of enrolled hosts, which sizes section 7); whether any registry row names a credential file; the version of
the Postgres service (whether `transaction_timeout` exists); the edge's check interval and thresholds (no active check exists yet, the
values of section 11 are proposed).

## 14. The model check, before code

Moved to [coordinator-multi-instance-checks.md](coordinator-multi-instance-checks.md), section 14. In one paragraph: the concurrency
protocol (the gate with `SKIP LOCKED` for beats, the conditional writes, the migration mutex, the one-snapshot cache reload, the
quiet rule, the periodic claim, the drain delay) is modelled in TLA+/PlusCal at N = 1, 2 and 3 and as an exhaustive interleaving
test in Rust, before any of the code (PR M2), with a negative variant for every missing lock or conditional write.

## 15. Compatibility (B8) and tests

Moved to [coordinator-multi-instance-checks.md](coordinator-multi-instance-checks.md), section 15. In one paragraph: every change
is additive (new tables, a sequence, columns with defaults, flags; no proto change; the fence trigger untouched), migration `0019`
is `may-run`, the release before is refused while a new instance runs and runs on the migrated database after they stop, and
each behaviour has a named test with a failure case.

## 16. Cutover and back-out (reversible)

1. Ship the release (PRs M1 to M9 landed, section 17) with Postgres and `replicas: 1`, still `stop-first`: the first
   start migrates and takes the shared lock. Operationally today's shape, with the new code. Soak.
2. Move the CA key to the secret and config (section 10), and `server.pem` to a tmpfs; one reversible edge step each, bytes
   hashed before and after, never printed. Still one replica.
3. Add the health check to the edge's clusters, the readiness flag and the drain delay (checked against the edge's values,
   section 11); deploy (stop-first once, the last).
4. Resolve the Mac listener (open question 1).
5. `order: start-first`, `replicas: 1`: the first update that overlaps two instances. Watch: `instances` rows (two only during
   the drain), no `alert=` twice, the heartbeat latency (M1's numbers), the operator edits' gate wait, no refused connection at the edge during the drain
   delay, and that both tasks starting together apply each migration once.
6. Soak through a real rollout of each plan kind (single 100%, 50% then 100%), with a halt raised mid-rollout, and update
   the coordinator itself while a rollout is live, so two instances decide through the overlap.

**Back-out.** `order: stop-first` (replicas stay 1) is a service update, no downgrade (the same release). To go back to the release
before: `docker service update --replicas 1 --update-order stop-first --image <release n>`; it takes the exclusive lock once
the new instances are stopped, runs its single-writer code on the same database (the audit counter and the sequences are ignored;
its next audit number is `max(seq) + 1`), forgets the fleet and quiets 30 s. **Coming back to the new release** after that:
its start (under the shared lock, so no release before is running) runs the two re-seeds: `audit_counter.next =
greatest(next, max(seq) + 1)` (section 6), and the queue's. The release before changes a desired version without touching
`set_seq` or `set_for`, so a row whose `set_for` is not its current version, or whose `set_seq` is NULL, is *changed or
unstamped*; those rows are given fresh numbers from `desired_seq` (first raised to `greatest(last, max(set_seq))`) in the
order of the release before's own queue rule (`queue.rs:10-12` tie-breaks), under the gate. A NULL `set_seq` ranks before
every stamped row for the few moments until this runs, so no instance decides on it differently; and because `fleet_hosts` is stale
by then, the gap rule of section 5.1 quiets the fleet. The CA secret and tmpfs steps are reverted by
the reverse edge step; the volume's copy is why they are reversible. Fix forward is the normal course (the rollout policy of
the farm): the back-out is a stopgap while a fix ships.

## 17. Implementation pull requests, in landing order

| # | Item | After | Size |
|---|---|---|---|
| M1 | Measure on the deployment: round trip on the socket, the gate's hold and the operator edit's wait under a 50% stage, how often a beat finds the gate busy, DDL lock wait of a migration against a running instance, `fleet_hosts` write cost at the real host count, `max_connections`, the Postgres version; a table in this document | none | S |
| M2 | The models of section 14 (spec and Rust state-space test at N = 1, 2, 3), failing on the unlocked variants | M1 | M |
| M3 | Shared fence lock and pool: `pg_try_advisory_lock_shared`, the `tx_ledger` settle, no `lease.commits` write, no deposed exit, `instances`; the migration mutex, DDL `lock_timeout` and the migration lint rule; the session timeouts; `0019` part 1. Still one instance | M2 | L |
| M4 | Row-transaction edits and `state_versions` invalidation for desired, ops, accounts, layout, verdicts, with the row-lock order of section 6 and the one-snapshot reload rule; `rollout_gate` with `SKIP LOCKED` for beats and the operator wait; `audit_counter` and its start-time re-seed; `facts_generation` | M3 | L |
| M5 | Enrollment: conditional writes, `FOR UPDATE`, fresh deny read; spent-token cache | M3 | M |
| M6 | The fleet in Postgres, without decisions: `fleet_*`, `cluster_quiet` and the gap rule, the ordered beat upsert, the in-SQL bake, the converged path | M4, M5 | M |
| M7 | The deciding transaction and the steps of 5.3: gate, re-read of every input, `set_seq`/`set_for` and the start-time re-stamp, orders with the unique host index, compare-and-set stage advance, storage-first halts, rollback before delete, fail closed; the quiet rule gating verdicts and rollbacks as well as orders | M6 | L |
| M8 | Sessions and login limiter; `periodic_jobs` and the claim; alerts, expiry, pruning and initial-admin made exactly-once | M3 | M |
| M9 | `--health-listen` and `--drain-delay-secs` (the old task keeps accepting while it answers 503); docs (`coordinator-state.md`, `rollout-plans.md`, `holds-and-force.md`, `enrollment.md`) updated; this document's tags turned to Built | M7, M8 | S |
| M10 | Infrastructure (outside this repository): CA secret and config, tmpfs, edge health checks and Mac listener, `start-first` update at `replicas: 1`. The cutover of section 16 | M9 | M |
| M11 | js-server's design and PRs in its repository, to the same leaderless rule | none | not sized |

M3 to M8 are each safe at one instance (the same code with one replica); only M10 lets two overlap.

## 18. Supersession check (B22)

This supersedes section 7 of coordinator-storage.md (the standby: its takeover, its quiet-start timing bound and its
list-of-addresses option are not needed when every instance serves), the second half of section 5 ("the single writer": the lock is
shared, `lease.commits` and the epoch's exit are gone), and these rows of section 14 "What decisions read" (a heartbeat reads
the database), "The audit's numbers" (still gap-free, by a counter row, not by the lock) and "A commit of unknown outcome"
(the ledger). M9 edits those sections to point here. Nothing is deleted from the file store (C9's business).

## 19. Decided, and open questions for the owner

**Decided by the owner** (kept here so the document does not reopen them):

1. Postgres is the single source of truth; no leader, no lease, no election; any N works (sections 3 and 5).
2. The database is the central dependency; with it unreachable instances decide nothing, and there is no memory-first
   fallback. This is also the answer to the earlier question of what guarantees a halt: no order is placed by an instance that
   cannot reach Postgres, so a halt that is not yet stored cannot be outrun.
3. Deploy shape: one instance in steady state, two only while an update overlaps (`replicas: 1`, start-first, the old one
   drains up to the stop grace period).
4. Zero staleness for a host's deny row and for revoked API tokens (the deny row is read in the heartbeat's one statement,
   tokens by the version vector, one `SELECT` per request); keep the initial-admin file on the shared volume for now (idempotent,
   section 9; a one-shot `bootstrap-admin` that writes the database is the later retirement); `fleet_hosts` unlogged only if M1
   shows the WAL cost matters (its loss only re-quiets the fleet); `may-run` migrations so a release rollback is a service
   update, with a facts generation that never goes backward accepted (the host refetches facts once if it does); heartbeat
   order by the host's `sent_unix` with the guard of section 5.2 and no proto change (an optional `beat_seq` in
   `HeartbeatRequest` is additive, B8, and not needed); presence written about every 5 s per host is fine.

**Open.**

1. **The Mac listener (the owner's open item).** The LAN listener (host-mode publish of 8446 to task port 8447, for Macs)
   cannot be bound by two instances on one node, so it also stops a start-first update from starting the new instance on that port
   while the old one holds it. Options: (a) the edge serves it as one more SNI chain and cluster, `tcp_proxy` with TLS
   untouched, so the certificate survives and the listener is no longer an instance port; (b) the ingress routing mesh on the
   service VIP (TCP, TLS untouched, so the certificate should survive; not verified); (c) one task for the Mac listener, a
   separate one-replica service or the whole coordinator back to stop-first, which is a gap in every update for the Macs.
   Recommendation: (a), the edge already is the front door; (b) only if verified to keep the client certificate; not (c).
2. **Is one row lock (the gate) still the right granularity?** Recommendation: yes. Only the live services step, the
   one-service-at-a-time and FIFO rules read every service, the hold is a few statements, and only operator edits queue at the gate, and
   the load on it does not grow with the number of instances (section 5.2). M1 measures the wait under a 50% stage; split by service (a row per
   service taken after the gate) only if the wait shows in the heartbeat's latency.

## 20. Discussion: the review of this document

Findings of the first review and what was done, including where a finding was taken only in part.

1. **A halt, hold or desired change can be outrun** (blocker). Right, and the cause was as stated: the deciding transaction
   took the one global advisory lock (`K_rollout` in that draft, now the gate row), operator edits took only their part lock, and the doc named only the fleet and the halts as read in
   the transaction. Changed: the deciding transaction re-reads every input from the rows (section 5.2); the edits of the
   parts a decision reads take the gate first, with one stated lock order (sections 5.2, 6); the deciding transaction bumps
   `state_versions` for the `ops` rows it writes; the invariant and the negative variants are in section 14. One point of the
   finding is taken in part: the converged path's cache validated by the earlier statement is *not* a defect, because that
   path never orders; at worst it delays an order by one beat. The deny row is left out of the lock on purpose (section 5.2).
2. **The ledger lookup is unsound with a shared lock** (major). Right: the old release's exclusive lock was what made the
   lookup safe. Changed to retry by the same id as the first statement of the retried transaction, which Postgres decides by
   the unique index (section 4), instead of a `pg_stat_activity` wait, which is racy by pid reuse and needs a privilege the
   coordinator's role does not need otherwise.
3. **The quiet rule keys on the wrong signal** (major). Right: the sentinel covered the crash only. Replaced by the gap rule
   (section 5.1), which covers a crash, a clean restart, an outage over `DOWN_AFTER`, the first start and a back-out followed by a
   cutover; the sentinel and `fleet_epoch` are gone. Not claimed: that the cost of the `max` is small at the real host count
   (not read; M1 measures it).
4. **The heartbeat upsert can be overwritten** (major). Right on the ordering and on the unstated owners of the bake and of the
   slots. Changed: the guarded upsert and who frees a slot (section 5.2). **Not right** on one point: the bake *can* be computed
   in SQL, because `Bake::after` reads only the beat (health, updating, promotion pending, version) and the previous row, not
   the desired state; the health is the agent's word in the beat. It is a `CASE` in the same statement, with a test against the
   Rust function. The finding's fix of a per-host beat sequence is offered as an optional proto field (section 19, decided against for now) and
   not taken first, because `sent_unix` is already sent and no proto change is needed.
5. **Back-out then roll-forward breaks audit numbering** (major). Right. Changed: the start-time re-seed of `audit_counter`,
   the `set_for` column to detect a row the release before changed, and the queue position of NULL rows (sections 6, 16), with
   two tests (section 15).
6. **The owner's leaderless decision (2026-11-15, second round).** This revision. What it changed: (a) the global advisory
   transaction lock `K_rollout` became the row lock of the one-row `rollout_gate`, taken inside the deciding transaction, and the
   per-part advisory locks became `state_versions` row locks, so every coordination is a row lock, a conditional write or a
   due-time claim, released by Postgres, with no instance named anywhere (sections 5.2, 5.3, 6); (b) each rollout step (an order,
   a stage advance, a halt, rollback before delete, the queue) states the write that decides its race (section 5.3); (c)
   periodic work is claimed per row (section 9); (d) fail closed is the stated rule and the question about the halt guarantee is
   closed (principle 4, section 19); (e) steady state is one instance with a start-first update, not two (sections 1, 11, 16);
   (f) the model runs at N = 1, 2, 3 (section 14); (g) the open questions are cut to the Mac listener and the gate's
   granularity. The earlier text had no election; the only lease in it was the old release's `lease` row, which stays written
   by the old release alone (section 4).

7. **Migrations are not serialized** (major). Right: the shared lock excludes nobody, and a DDL waiting for `ACCESS EXCLUSIVE`
   queues every later statement on the table behind it. Changed: a mutex of its own, an exclusive transaction advisory lock on
   `MIGRATE_KEY` around the whole migration transaction, and a `lock_timeout` on DDL with a bounded retry (section 4). It is a
   mutex held for seconds, not a leader. The migration lint gains a rule against rewrites of hot tables.
8. **A cache can be labelled newer than its content** (major). Right as a hazard, and it would have been unbounded staleness for
   the converged path. Changed: rows and version are read in one snapshot, version first, and the cache takes the version of that
   snapshot (section 6). Not a defect as the finding itself notes: a deciding transaction re-reads rows.
9. **The quiet rule suppressed only orders** (major). Right. Changed: in the quiet window no decision of its own is taken (no
   order, stage advance, threshold halt, bad-version mark or rollback), and the deadline of "did not come back" counts from the
   end of the window (sections 5.1, 5.3). An operator's halt, hold, force or retire is still applied: it only stops things, and
   it is not driven by the fleet's state.
10. **No healthy upstream in the start-first window** (major). Right: a task answering 200 is not an edge that has marked it healthy.
    Changed: `--drain-delay-secs`, during which the old task answers 503 but keeps accepting, with a deploy check against the
    edge's DNS refresh and check interval and thresholds (section 11). The edge's check values are proposed and not yet
    agreed. The Mac listener stays open question 1: the old task holds that port through the delay.
11. **The gate had no hold bound and halts no priority** (major). Right on both. Changed: Postgres's own
    `idle_in_transaction_session_timeout` and `statement_timeout` on every pool session bound a hold by a hung instance; beats
    use `SKIP LOCKED` and never queue, so an operator edit waits only for the holder and earlier operator edits, with its own
    longer `lock_timeout` (section 5.2). The cost is that a beat that finds the gate busy decides one beat later (15 s); M1
    measures how often. The finding's suggestion of a longer operator wait alone was not enough, since the FIFO queue would still
    put the operator behind the beats.

Not changed by the review and still open: what was not checked (section 13), and the measurements of M1.
