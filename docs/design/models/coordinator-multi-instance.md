# The coordinator as N instances: the models of the protocol

The spec of the model check of [coordinator-multi-instance.md](../coordinator-multi-instance.md) (section 14 there, and section 14
of [coordinator-multi-instance-checks.md](../coordinator-multi-instance-checks.md)): the protocol by which any number of coordinator
instances decide on one Postgres, split into small models, each searched over every interleaving of its statements at N = 1, 2
and 3 instances, with a variant for every lock and conditional write the protocol relies on.

**Built** (row M2 of section 17): this document and the test
`crates/hostkeeper-coordinator/tests/multi_instance_model/` (`cargo test -p hostkeeper-coordinator --test multi_instance_model`),
which runs in every pull request's test run like any other test. The test reads this file: the three tables below (Invariants,
Variants, Bounds) must state exactly what the models define, row for row, and the test
`the_spec_states_every_invariant_variant_and_bound_of_the_models` fails when they differ in either direction. The prose is for
the reader and is not read by the test.

## How a model is checked

A model is the protocol, not the SQL. Its state is the shared rows a step reads and writes (each as small as the property needs:
a flag, a counter, a holder) and every process's own state: where it is in its transaction and what it read. A step is one
statement of one process, or one event of the environment (a host, an operator, time, the database, the release before), so two
statements the design makes separate are two steps, and another process's step can fall between them. Locks are what the design
names: the gate row (`rollout_gate`, taken `FOR UPDATE SKIP LOCKED` by a beat and plain `FOR UPDATE`, queued in arrival order, by
an operator edit), the `state_versions` and `audit_counter` row locks, the unique index, the migration lock; a conditional write
is one step whose `WHERE` is its precondition.

The search (`explore.rs`) visits every state reachable from the model's initial states, breadth first, so a broken property comes
with a shortest trace. Two kinds of property:

- **Safety**: a predicate that must hold in every reachable state.
- **Liveness**: from every reachable state, a state where the goal holds can still be reached. The processes of a model can always
  take another step (a host beats again, an instance ticks again) and the state stays finite, so this says that nothing the
  processes do can get the system stuck short of the goal; it is checked on the whole graph by a search backwards from the goal.

The test of an invariant (one test per invariant, named as the invariant) searches its model's locked protocol at N = 1, 2 and 3,
which must keep every invariant of the model, then each variant listed against it at N = 1, 2 and 3: the variant must break the
invariant exactly from its stated N on and keep it below. A variant stated as breaking an invariant "never" is one that takes away
a lock the design does not rely on alone (a compare-and-set or a unique index is the backstop), and the test shows the invariant
still holds without it. Every invariant has at least one variant that breaks it, so no invariant is true merely by how the state
is written.

**Why N = 1, 2, 3, and why that carries to any N.** No step names an instance and the instances of a model are interchangeable:
each runs the same steps from the same kind of state, and every guard is a lock, a unique key or a compare-and-set on a row, which
treats every holder alike. So the design's argument is that a failure needs only as many instances as the race it is made of, and
the races of this protocol are two deciders between a read and a write (N = 2) and, for a limit of k concurrent holders, k + 1
deciders (here `max_updating` 1 broken at N = 2 and `min_up` at N = 3). The test runs N = 1, which has no race between instances
and shows what a single instance already gets wrong (the environment, an operator's session, a lost connection, is its own
process), and N = 2 and 3. It is the design's argument, stated here; the test does not prove it for N above 3.

## The models

### `orders`

The model `orders` is the deciding transaction of a heartbeat (section 5.2) placing orders under `--live-rollouts`, the queue,
`max_updating` and `min_up`. Rows: the order rows of each host (with the unique index on the host), whether the first service's
hosts defer (written by their own beats, without the gate, as every beat's rows are), and the gate row. Service `s1` (set first) runs
on hosts `h0`, `h1`, `h2` in one stage with `max_updating` 1 and `min_up` 1, and `s2` on `h3`; `--live-rollouts` is 1. At the start
`s1`'s hosts defer, so `s2` may go first, and they stop deferring at any moment, once. The decision restates the rules of
`queue.rs` and `live.rs`: a service set before another takes a place in the queue only while it waits and can be ordered, which is
why the race on the limit needs an input a beat writes without the gate. Each instance takes beats of any host, forever: ask for
the gate with `SKIP LOCKED` (busy: answered with the host's stored orders, nothing written), read every input, write the order
(`ON CONFLICT (host_id) DO NOTHING`), `COMMIT`, reply with the host's order row, new or found. An instance may crash once after
`COMMIT` and before the reply; one instance may hang once while it holds the gate, and the server ends its transaction (the idle
timeout), after which it takes beats again. Without the gate, B reads while `s1` defers and decides `s2`, `s1` stops deferring,
A orders `s1`, and both commit: two live rollouts (N = 2), two updating hosts of the stage (N = 2), and with a third instance no
host of the stage up (N = 3); the unique index alone still keeps one order per host, and without it two beats of one host order it
twice. An instance that orders the queue by its own stamps, as today's memory does, orders `s2` ahead of `s1`; a reply that carries
only a new order leaves a host whose reply was lost without its order; a hung holder the server never ends stops every decision.

### `operator`

The model operator checks an operator's edit of a decision input (raise the halt, set the hold, or change desired from 1 to 2, chosen at the start) against the deciding beats of N instances. The rows are the gate row with its holder and a first-in first-out queue of plain FOR UPDATE requests, one service with its halted and held flags and its desired version, and one order row per host (two hosts) holding the version it was placed for, inserted with ON CONFLICT DO NOTHING. Each instance takes beats of either host forever. A beat asks for the gate with SKIP LOCKED; when the gate is held it is answered with the stored orders and writes nothing, and when granted it reads the inputs and the host's order row in one statement, writes the order in a separate statement, and commits, which releases the gate. The gate is handed to the queue head at release, so a queued request implies a held gate and the refusal of a SKIP LOCKED request behind a queued one is not a separate step; the invariant about beats overtaking guards against a beat joining the queue, and the Postgres refusal rule is left to the real-database test. The operator queues with a plain FOR UPDATE, is granted when the gate is free and it heads the queue, writes, and commits, which is the acknowledgement. The database may go down once, rolling back every transaction in flight, releasing the gate, emptying the queue, and running no step while down (fail closed); a rolled-back edit may be tried again. Single-session aborts by a session timeout and the operator's lock timeout are not modelled, since each only ends a transaction without a commit and is covered by the outage's effects. Without the gate the operator's commit lands between a decision's read and its order commit, so an order outruns the acknowledged edit even beside one instance, because the operator is its own session. If decisions read an instance's cache, an instance other than the one serving the operator still holds the old inputs after the acknowledgement, so it fails from two instances. If beats queue at the gate, a beat ahead of the operator is granted while the operator is still queued, which needs a second instance to wait while the first holds. If a halt is acknowledged from memory while the database is down, another instance reads the rows when the database returns, before the halt is written back, and commits an order after the acknowledgement; one instance does not fail because its own decisions read its memory.

### `steps`

The model `steps` checks the rollout steps of section 5.3 whose race is decided by a conditional write, as three scenarios with one initial state each. An instance runs one deciding transaction at a time, so N counts concurrent deciders: with N = 1 nothing interleaves and "fails from 2" means two deciders at once. In the advance scenario one plan sits at stage 1 of 2, done and baked; the rows are the stage, the number of advances and the number of `rollout_advance` audit lines. A deciding beat takes the gate with `SKIP LOCKED` (busy means it is answered and writes nothing), reads the stage in one step, and in a second step runs `UPDATE plan SET stage = 2 WHERE stage = 1` with its audit line only when the row count is 1; once, an operator `Rollouts.Advance` with the stage it saw runs the same compare-and-set. In the halt scenario a failed verdict has crossed the threshold, and beats read the halt row then run `UPDATE halts SET generation = generation + 1 WHERE NOT halted`, auditing only on one row; the operator's halt is checked by the model `operator`. In the rollback scenario version 2 is rolling and the package is also being uninstalled; a failure report arrives once and is written as a verdict by an instance that takes the gate, and a deciding beat reads the verdict and the desired row, then rolls back first (desired back to `last_good`) and otherwise orders the uninstall, in the rule order of `decide.rs`. A write and its COMMIT are one step; a crash or abort before COMMIT only drops the write and releases the gate, and one after it changes no row, so neither is a step. Without the gate and without the compare-and-set, a beat that read stage 1 writes after the operator's advance (or another beat's) and makes a second advance and a second audit line; the same for a halt without `WHERE NOT halted`, which gives generation 2 and two audit lines. Without the gate but with the compare-and-set or the `WHERE NOT halted` guard the invariants still hold at every N, which is the backstop the design keeps on purpose. Without the gate on the beats and on the verdict's edit, a beat that read no failure writes its uninstall order after another instance committed the failure verdict and before any rollback, so an uninstall is ordered for a version being rolled back, from N = 2. The locked runs alone do not show that the writes are reachable; the variants do, since each breaks its invariant only by reaching the advance, the halt or the uninstall order.

### `quiet`

The model quiet checks the cluster's quiet rule after a gap in the beats. Its rows are fleet_hosts of two hosts (each absent, stale or fresh), cluster_quiet (in the future or not, with time abstract), the halts (an operator's halt and a halt raised on a threshold, which comes with a rollback), the orders, and a sentinel row that only one variant reads; ghost flags record what committed decisions did. Each of N instances takes beats of either host forever. A beat is one statement that reads max(seen_unix) as its snapshot has it before its own upsert, sets quiet when no row is fresh (the table empty or all rows old), and upserts the host fresh. The deciding transaction follows as separate statements: the gate with SKIP LOCKED (busy means no decision of its own), the read of quiet, the read of the halts and the fleet with the decision, and the write with COMMIT. While quiet was read as in the future a decision makes none of its own; otherwise, when a host is stale or absent, it may either order or raise the threshold halt and roll back, since the rollout's inputs are not modelled. An operator raises one halt under the gate, never held back by the quiet rule, and every decision reads it. The environment makes up to two gaps, each a database crash (the table is emptied and the sentinel is gone) or a clean restart or an outage longer than DOWN_AFTER (rows kept and stale, one step in the model); a gap can fire at any moment and aborts every transaction in flight, and a fresh host may also go silent at any moment. The ghost flag recording which hosts have beaten since the last gap is the model's abstraction of the quiet window: quiet ends only once every host has beaten, which is the assumption that --quiet-secs is longer than a beat period, and a threshold halt on a host that stays silent after that is allowed. Keying the rule on a missing sentinel fails because a clean restart or an outage leaves the sentinel, so a partial fleet orders or halts. Reading max(seen_unix) after the beat's own upsert fails because the beat always sees its own fresh row and never detects the gap. Gating orders only lets a partial fleet raise a threshold halt and a rollback, gating the operator's halt by quiet refuses it, and a decision that does not read the operator's halt orders after it. Two liveness checks hold in the locked protocol: quiet can always end, and a decision can always still wait on the operator's halt; they fail when nothing ends quiet and when decisions ignore the halt. All variants fail from N = 1, since none needs a second instance.

### `ledger`

The model ledger checks the settle of a commit of unknown outcome (section 4), as built: the outcome goes to the caller and the closure is not retried by the same id. Its rows are the committed rows of `tx_ledger` (the first id of each instance, with the unique index), a counter row and one audit line per instance. Each of N instances has one change, a closure that increments the counter and writes an audit line, run as a transaction whose first statement inserts the instance's own ledger id, then writes, then commits. The writes are invisible until commit and the increments commute, so the closure is a single step. Each instance's first `COMMIT` may come back of unknown outcome because the connection drops; a `COMMIT` that never reached the server is the case where the old backend rolls back. The old backend is then a separate environment process, not counted in N, that is still in progress and commits or rolls back at any moment. The instance revives and settles by inserting its id again with `ON CONFLICT DO NOTHING`, which waits for the in-progress backend: no row inserted means the first attempt landed, and an inserted row (which fences the id) means it did not. Only on "did not land" does the caller re-issue the change as a new request with a new id, whose commit is known; this re-issue is the caller's own, modelled so the exactly-once goal can be checked. The bound is one lost commit and at most one re-issue per instance, and a session lost during the settle or the re-issue is out of bound. The invariants are that the change lands exactly once and is never applied twice (a liveness goal that a doubled audit line makes unreachable), and that the outcome told to the caller equals what the rows hold and stays so. With `lookup_settle` the settle is a `SELECT` that cannot see the in-progress commit, so it answers "did not land", the old backend then commits and the re-issue applies the change a second time, which breaks both invariants from N = 1. With `retry_with_a_new_ledger_id` the revival retries at once with a fresh id and never settles, so if the old backend commits the change is applied twice, which breaks the first invariant from N = 1.

### `beats`

The model `beats` covers the ordered beat upsert of section 5.2. Its one row is the host's `fleet_hosts` row, reduced to the `sent_unix` it holds and whether it is older than `--beat-reorder-secs`; the row exists at the start with sent value 4. One host sends beats without end, each with the next value of its own clock (5, then 6, then 6 again, the same beat re-sent, which keeps the state finite). Its clock may step back once, after which it sends 1, then 2, then 2 again, below every earlier value. Each beat is handed to an idle instance, an instance has at most one beat in flight, and it applies the upsert as one statement at any later time, so beats reach the row in any order across instances. The guard is `sent >= stored OR the stored row is older than the window`; a dropped beat changes nothing, and an applied beat makes the row fresh. Time is an event that ages the row past the window; it is enabled only when no beat is in flight, which assumes that a beat is applied well within the window, so a beat stalled on a pool wait for longer than the window is not modelled and the 30 s default rests on beat latency being far below it. The clock step likewise happens with no beat in flight. That excludes a corner the guard cannot catch: with the row already older than the window, a beat with 5 in flight and the clock stepping back, a beat with 1 is applied first through the second arm and the delayed 5 then overwrites it, so the row is wrong until the next beat after the window. The first invariant uses ghost state, since sent values alone do not say which beat is newer after the step: each in-flight beat carries its rank in sending order and a flag that a later beat has been applied, and the invariant is that no applied flagged beat overwrites another sent value. Without the guard, two instances suffice for beat 6 to be applied before beat 5 so that the row goes back; with one instance beats are applied in the order sent. The second invariant is a liveness check that the stored row can always again hold the host's latest beat; without the reorder arm, a host whose clock stepped back is dropped forever, even with one instance. The statements that run only when the upsert applied (bake and slot release, in a `RETURNING` CTE) are not modelled, since they follow the row the upsert leaves.

### `reload`

The model `reload` checks the one-snapshot reload of a cache (section 6) and the converged path of the heartbeat (section 5.2). The rows are the part `desired`, abstractly the desired version of one host's service, and its `state_versions` number, which every operator edit bumps in the same commit as the rows; the host runs version 0 and is behind while the desired version is above it, and the host has at most one order row. A ghost keeps the rows of every version, so the check can compare a cache with the rows of the version in its label. Each instance holds a cache with a label and a content, and the operator, outside the N instances, makes two edits, each setting the desired version to 0 or 1 (an edit that changes nothing still bumps the version). Each instance takes beats of the host forever: one step reads the version vector. When it equals the label and the cache says the host is not behind, or an order at the cache's version exists, the beat replies on the converged path and decides nothing. When it equals the label and the cache says the host is behind with no such order, the beat goes to the deciding transaction without a reload; when it differs, the part is reloaded first. The deciding transaction reads the rows, not the cache, upserts the order at the desired version it reads, and is one step because the gate's interleavings belong to the model `orders`; it can also be skipped (gate busy or abort), leaving the order as it was, so the next beat decides. Locked, the reload is one step reading version and rows from one snapshot, so the cache equals the rows of its label. In the variant the rows and the version are two steps, so an edit can commit between them and the cache is labelled with the new version over the old rows; the first invariant fails even with one instance. The liveness check is a reachability goal that is met only after the last edit, so it bites there: in the variant the stale cache carries the current label, every later beat takes the converged path, and no order at the current version is ever reachable.

### `audit`

The model audit holds the audit numbers and the start-time re-seeds of design section 6 and section 16. Its rows are the audit lines (each is its number, kept in commit order), the audit_counter row with its row lock, the desired rows of services s1 and s2 (version, set_seq, set_for) and the sequence desired_seq; a ghost list keeps the order in which the services were last set, and only the invariants read it. Each of the N instances writes one audit line in a transaction: the counter UPDATE as the last write, which waits while another transaction holds the row lock and keeps it until COMMIT, then the COMMIT that makes the line visible. The transaction may also roll back after the UPDATE (a crash or a stop mid-transaction), which releases the lock and restores next, so a rollback leaves no gap. The release before runs once, as an environment process, only when every instance has stopped (it holds the exclusive fence lock): in one step it writes a line numbered max(seq)+1 without touching the counter and gives s1 a new version without touching set_seq or set_for. Then every instance starts, in two steps before it serves: the counter re-seed (greatest of next and max(seq)+1, waiting for the row lock) and the queue re-stamp under the gate, which gives fresh numbers from desired_seq to each row whose set_for is not its version or whose set_seq is missing. After its start an instance writes one more line and sets one desired version of s1 or s2 (set_seq from desired_seq), in either order; the desired change is modelled without its audit line, since the gate serializes it with the re-stamp. The primary key on the audit number is not modelled: a number used twice is itself the broken invariant. Only s1 can be changed by the release before, so the re-stamp has one row to rank, and the raise of desired_seq and the missing set_seq case are not exercised. The variant that takes the number from a sequence at statement time, with no row lock held to the commit, lets one instance take 1, a second take 2 and commit first, so a reader that has seen 2 would miss 1; it needs two instances. Without the counter re-seed the counter is stale after the release before wrote its line, so the next line of the new release reuses its number, which one instance shows. Without the re-stamp the changed row keeps its old set_seq, so the queue ranks s1 before s2 although s1 was set last, which one instance shows.

### `periodic`

The model periodic checks periodic work without a leader (design section 9). Its rows are the job's next_run (a period number), the alerts row (present or not), and ghost counters of the claims made in each period and of the alert= lines logged. Time is the current period, 0, 1 or 2, and the alert condition holds from the start, so every run of the scan finds it. Each of the N instances ticks without end: the claim is one statement that, when the job is due, advances next_run to the next period and returns the job, and the scan with its conditional insert (ON CONFLICT DO NOTHING RETURNING) is a second step in which only the inserting instance logs the line. The insert and the log line are one step, so a death between them that would lose the line is not modelled; the invariant is at most once. One instance may die once between its claim and its effect and then returns as a fresh instance. Time moves to the next period only after some instance has attempted a claim in the current one, and only while no instance is between the statements of a claim or between its claim and its effect; both assumptions say that instances tick far more often than the period and that a run is far shorter than a period. The locked protocol keeps all three invariants at N = 1, 2 and 3. A claim that reads the due row and then writes next_run unconditionally lets two instances both read the job as due before either writes, so it is claimed twice in one period from N = 2; the conditional insert still keeps the alert logged once at every N, which is why the design keeps the conditional write as the real guard and the claim as a saving of work. A claim held as a lease with no expiry is never released when its claimer dies, so every later period skips the job and the run is lost for good from N = 1, where the design's claim loses one run only. An unconditional insert and log makes each run log the alert again, so two runs in two periods log it twice from N = 1.

### `migrate`

The model migrate has three rows: the migrations table (whether m1 and m2 are each recorded), the schema objects (how many times each migration's DDL was applied) and the migration lock on MIGRATE_KEY (the session holding it, if any). N starters start at once; each takes the lock (a starter waits while another holds it), then for each migration reads whether it is recorded (one statement) and, if not, applies and records it in one transaction (one step), then releases the lock and goes on to serve. The runner runs on a session that already holds the shared fence lock, which excludes no one, so it is not a row of the model. A starter may die once in the whole run, at any step before it finished: Postgres releases its session lock and rolls back its open transaction (the apply-and-record step is atomic, so a death leaves the whole migration or none of it), and the dead starter is restarted and runs again from the start. One death suffices, since the lock is released on every death and a second death only returns a starter to the start. A wait that runs past lock_timeout is treated as waiting, since the restarted starter is in the same state. A second application of a DDL is counted as the defect (Postgres would fail on a duplicate object). The two invariants are that no migration is applied twice, and, as liveness, that from every state a state with every migration recorded is reachable. Without the mutex, two starters both read a migration as unrecorded before either applies it, and both apply it, from two starters on; one starter alone is correct. With the lock kept in a row that the starter sets and clears itself, a starter that dies while holding it leaves the row set for good, so every restarted starter waits forever and the migrations are never all recorded, already at one starter. The locked protocol holds at 1, 2 and 3 starters in 22, 62 and 156 states.

### `locks`

The model `locks` checks the one global lock order of design sections 4, 5.2 and 6 for a deadlock. Its locks are the gate row, the `state_versions` row of each of the six parts, the `audit_counter` row and the migration lock, each held by one transaction at a time until it commits, plus the shared fence lock, which never waits. Each of N instances is idle or runs one transaction of one of ten kinds, takes its locks one per step in the order of the locked protocol (waiting while another transaction holds the lock), then commits and releases all in one step; an idle instance may start any kind at any time, so every mix of N concurrent transactions is explored. The kinds are the deciding transaction of a beat, operator edits of desired, of ops, of layout and of verdicts, force or retire, an edit of accounts, an edit of a user with its token (accounts then enrollment), an enrollment approve, and the migration runner (fence lock, then migration lock, never the gate or a part row; it shares no lock with another kind, so that kind only checks the runner takes neither). A beat asks for the gate with SKIP LOCKED, so when the gate is held it gives up holding nothing; every other request waits. Other rows a transaction writes are assumed reachable only under a modelled lock, with the enrollment request and host rows and the DDL table locks left out; a waiter is queued behind the holder alone, crashes and aborts only release locks, and nothing ends a wait (no lock timeout, no server deadlock detector), so any cycle of waiting transactions is a deadlock that stays. The invariant lock_order_has_no_cycle holds when no reachable state has such a cycle. The variant edit_takes_the_gate_after_its_part_row fails from N = 2: an operator edit holding its part row (for example ops) waits for the gate that a beat holds while it waits for that row. The variant audit_counter_taken_before_a_part_row fails from N = 2: a user-with-token edit holds the audit counter and the accounts row and waits for the enrollment row that an enrollment approve holds while it waits for the audit counter; this rests on the reading that approve and a user-with-token edit take the enrollment part row.

### `drain`

The model `drain` is a start-first update of one task behind the edge, with the drain delay of section 11, item 2, on a clock the
model advances one second per step (every choice a second allows is a step of its own; no wall clock). The edge checks each task it
lists every 2 s, marks it healthy after 2 passes in a row and unhealthy after 2 failures, and sends connections to its healthy
tasks, or to every task it lists when fewer than half are healthy (panic mode). The new task becomes ready at some second, Docker's
healthcheck passes at some later second and Swarm sends the old task SIGTERM; from then the old task answers 503 and keeps accepting
for `--drain-delay-secs`, then refuses and exits. As the design assumes, DNS lists the new task no earlier than when it is ready and
at the latest one refresh after SIGTERM, and a task the edge has just learned is unhealthy until it passes its checks. The other
N - 1 tasks serve throughout. The locked protocol uses 8 s, the smallest delay the deploy check admits (more than 1 + 2 x 2 + 2);
an old task that stops accepting at SIGTERM, or a delay of 3 s, is refused a connection the edge still sends it, at every N.

## What the models found

- **A clock step with a beat in flight** (`beats`). The guard orders a host's beats by its own `sent_unix`. When the row is
  already older than the reorder window, a beat is still in flight, the host's clock steps back and a beat sent after the step is
  applied first (the second arm lets it in), the delayed beat then overwrites it, because its `sent_unix` is higher. The model
  leaves this out by stating that the clock steps and the row ages only with no beat in flight; the row is wrong until the host's
  next beat after the window, which is the cost the design accepts by ordering on `sent_unix` with no proto change (section 19,
  decision 4).
- **Panic mode at the edge** (`drain`). Under the design's own assumptions (the edge marks a task it has just learned unhealthy
  until it passes `healthy_threshold` checks, and DNS lists the new task at the latest one refresh after SIGTERM), the edge marks
  the old task unhealthy after `unhealthy_threshold` failed checks before it has marked the new one healthy, so for a few seconds
  no task is marked healthy and Envoy's panic mode sends connections to every task it knows. Both accept (the old one for the drain
  delay, the new one since it was ready), so no connection is refused, which is the invariant. Section 11 of the design said that
  panic mode never has to engage; it now says what the model shows.

## What is not modelled

The SQL itself (each statement is taken as doing what the design says it does; the tests against the service container in the
pull requests that build each part check that); the values of the timeouts (a hold is ended by the server, not by how long it
takes); `ORDER_AGE` (orders do not age out within any model's bound); the pool of sessions (a session of one instance is one more
process, the same as another instance, since no step names an instance); the decision's rules beyond what each property needs
(the decision of `orders` is the queue, the limit and the stage's `max_updating` and `min_up`, restated over the model's rows from
`queue.rs`, `live.rs` and `stages.rs`, not the crate's `decide`).

## Invariants

| Invariant | Model | Holds when |
|---|---|---|
| `n_instances_decide_one_rollout_at_a_time` | `orders` | at most `--live-rollouts` (1) services have an order row at once |
| `queue_follows_set_seq_on_both_tasks` | `orders` | no order is committed for a service while one set before it (by `set_seq`) waited and could be ordered, by the rows its decision read |
| `stage_never_has_more_updating_hosts_than_max_updating` | `orders` | the hosts of the stage with an order row never exceed `max_updating` (1) |
| `min_up_holds_in_every_stage` | `orders` | the hosts of the stage without an order row never fall below `min_up` (1) |
| `one_host_never_has_two_orders` | `orders` | no host has two order rows |
| `order_after_crash_is_returned_again` | `orders` | liveness: from every state, a state where every host with an order row has received it in a reply can be reached (an instance may crash after `COMMIT`, before the reply) |
| `a_host_behind_gets_an_order_whichever_instance_hears_it` | `orders` | liveness: from every state, a state where no host could be given an order (every free slot used) can be reached, whichever instance hears each beat |
| `acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task` | `operator` | no order commits after the operator's halt was acknowledged |
| `acknowledged_hold_and_desired_change_are_read_by_the_next_decision` | `operator` | no order commits after a hold was acknowledged, and no order for version 1 commits after desired 2 was acknowledged (tested through the committed order, not the read) |
| `beat_never_overtakes_a_queued_operator_edit` | `operator` | no beat is granted the gate while the operator's edit is queued for it (guards against a beat joining the queue) |
| `stage_advance_by_racing_instances_is_one_advance_and_one_audit_line` | `steps` | however beats and the operator call interleave, the plan is advanced at most once and at most one rollout_advance audit line exists |
| `halt_raised_by_two_instances_is_one_generation_and_one_audit_line` | `steps` | however beats interleave over a crossed threshold, the halt generation is at most 1 and at most one halt audit line exists |
| `rollback_is_committed_before_the_uninstall_is_ordered` | `steps` | no uninstall order is written while a failure verdict is committed and the rollback is not |
| `no_order_after_a_gap_in_beats_until_quiet_ends` | `quiet` | no order commits from a decision that read the fleet while some host had not beaten since the last gap |
| `partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing` | `quiet` | no threshold halt and rollback commits from a decision that read the fleet while some host had not beaten since the last gap |
| `operator_halt_in_quiet_window_is_read_by_every_decision` | `quiet` | the operator's halt is never refused by the quiet rule and no order commits after it |
| `quiet_ends_once_every_host_has_beaten` | `quiet` | from every reachable state cluster_quiet can still end (liveness: the quiet rule never sticks) |
| `a_decision_after_the_operator_halt_can_still_wait_on_it` | `quiet` | from every reachable state a decision can still read the operator's halt and commit without a decision of its own (liveness: the halt is read) |
| `unknown_outcome_commit_lands_once_by_retry` | `ledger` | from every reachable state the run can still end with every change applied exactly once (one audit line each, counter equal to N), so a change applied twice, which can never be undone, breaks it |
| `settled_outcome_is_what_landed` | `ledger` | in every state, the outcome told to a caller (landed or did not land) equals whether the first attempt's transaction has committed, so a did-not-land answer is never followed by that commit |
| `stale_beat_does_not_overwrite_a_newer_one` | `beats` | no applied beat overwrites the row of a beat sent after it with another sent value |
| `host_whose_clock_stepped_back_is_believed_again` | `beats` | from every reachable state, a state is reachable where the stored row holds the sent value of the host's latest beat |
| `cache_is_never_labelled_newer_than_its_content` | `reload` | every instance's cache content equals the rows of the version its label names |
| `host_behind_the_desired_version_is_ordered_after_a_reload` | `reload` | from every reachable state a state can still be reached where the last edit is made and the host is not behind or has an order at the current desired version |
| `audit_numbers_follow_commit_order` | `audit` | the numbers of the committed audit lines, read in commit order, are 1, 2, 3 and so on with no gap and no inversion |
| `new_release_again_continues_audit_numbers_after_the_release_before_ran` | `audit` | no audit number is carried by two committed lines and every line committed after the release before's line carries a number above it |
| `set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order` | `audit` | once an instance has finished its start, the queue order by set_seq equals the order in which the desired versions were last set |
| `periodic_job_runs_once_per_period_with_n_claimers` | `periodic` | the job is claimed at most once in every period, however many instances tick |
| `claimer_death_loses_one_run_only` | `periodic` | whenever time has left a period, the job ran in it or its claimer died in it, so the next period runs again |
| `alert_is_logged_once` | `periodic` | the alert= line is logged at most once, however many runs find the alert |
| `two_starters_apply_each_migration_once` | `migrate` | no migration's DDL is applied more than once, however many starters run at once |
| `every_migration_is_applied_once_a_starter_survives` | `migrate` | from every reachable state, a state where every migration is recorded can still be reached (liveness) |
| `lock_order_has_no_cycle` | `locks` | in no reachable state is there a cycle of transactions each waiting for a lock the next one holds; a beat's gate request never waits |
| `no_connection_is_refused_at_the_edge_during_an_update` | `drain` | every task the edge sends connections to (its healthy tasks, or every task it lists in panic mode) accepts them, at every second of a start-first update |

## Variants

| Variant | Model | Breaks | Fails from N |
|---|---|---|---|
| `decision_without_the_gate` | `orders` | `n_instances_decide_one_rollout_at_a_time` | 2 |
| `decision_without_the_gate` | `orders` | `stage_never_has_more_updating_hosts_than_max_updating` | 2 |
| `decision_without_the_gate` | `orders` | `min_up_holds_in_every_stage` | 3 |
| `decision_without_the_gate` | `orders` | `one_host_never_has_two_orders` | never |
| `order_insert_without_unique_index_and_without_the_gate` | `orders` | `one_host_never_has_two_orders` | 2 |
| `queue_stamped_per_instance` | `orders` | `queue_follows_set_seq_on_both_tasks` | 2 |
| `reply_carries_only_a_new_order` | `orders` | `order_after_crash_is_returned_again` | 1 |
| `hung_holder_is_never_terminated` | `orders` | `a_host_behind_gets_an_order_whichever_instance_hears_it` | 1 |
| `operator_edit_without_the_gate` | `operator` | `acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task` | 1 |
| `operator_edit_without_the_gate` | `operator` | `acknowledged_hold_and_desired_change_are_read_by_the_next_decision` | 1 |
| `decision_reads_a_cache` | `operator` | `acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task` | 2 |
| `decision_reads_a_cache` | `operator` | `acknowledged_hold_and_desired_change_are_read_by_the_next_decision` | 2 |
| `beat_queues_at_the_gate` | `operator` | `beat_never_overtakes_a_queued_operator_edit` | 2 |
| `halt_acknowledged_from_memory_while_the_database_is_down` | `operator` | `acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task` | 2 |
| `stage_advance_without_compare_and_set_and_without_the_gate` | `steps` | `stage_advance_by_racing_instances_is_one_advance_and_one_audit_line` | 2 |
| `stage_advance_without_the_gate` | `steps` | `stage_advance_by_racing_instances_is_one_advance_and_one_audit_line` | never |
| `threshold_halt_without_not_halted_guard_and_without_the_gate` | `steps` | `halt_raised_by_two_instances_is_one_generation_and_one_audit_line` | 2 |
| `threshold_halt_without_the_gate` | `steps` | `halt_raised_by_two_instances_is_one_generation_and_one_audit_line` | never |
| `rollback_and_uninstall_decided_and_verdict_edited_without_the_gate` | `steps` | `rollback_is_committed_before_the_uninstall_is_ordered` | 2 |
| `quiet_keyed_on_a_missing_sentinel` | `quiet` | `partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing` | 1 |
| `quiet_keyed_on_a_missing_sentinel` | `quiet` | `no_order_after_a_gap_in_beats_until_quiet_ends` | 1 |
| `gap_rule_reads_its_own_upsert` | `quiet` | `no_order_after_a_gap_in_beats_until_quiet_ends` | 1 |
| `gap_rule_reads_its_own_upsert` | `quiet` | `partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing` | 1 |
| `quiet_gates_orders_only` | `quiet` | `partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing` | 1 |
| `operator_halt_gated_by_the_quiet_rule` | `quiet` | `operator_halt_in_quiet_window_is_read_by_every_decision` | 1 |
| `decision_ignores_the_operator_halt` | `quiet` | `operator_halt_in_quiet_window_is_read_by_every_decision` | 1 |
| `decision_ignores_the_operator_halt` | `quiet` | `a_decision_after_the_operator_halt_can_still_wait_on_it` | 1 |
| `quiet_never_ends` | `quiet` | `quiet_ends_once_every_host_has_beaten` | 1 |
| `lookup_settle` | `ledger` | `unknown_outcome_commit_lands_once_by_retry` | 1 |
| `lookup_settle` | `ledger` | `settled_outcome_is_what_landed` | 1 |
| `retry_with_a_new_ledger_id` | `ledger` | `unknown_outcome_commit_lands_once_by_retry` | 1 |
| `unguarded_beat_upsert` | `beats` | `stale_beat_does_not_overwrite_a_newer_one` | 2 |
| `beat_guard_without_the_reorder_arm` | `beats` | `host_whose_clock_stepped_back_is_believed_again` | 1 |
| `cache_reloaded_in_two_reads` | `reload` | `cache_is_never_labelled_newer_than_its_content` | 1 |
| `cache_reloaded_in_two_reads` | `reload` | `host_behind_the_desired_version_is_ordered_after_a_reload` | 1 |
| `audit_number_from_a_sequence` | `audit` | `audit_numbers_follow_commit_order` | 2 |
| `no_audit_counter_reseed_at_start` | `audit` | `new_release_again_continues_audit_numbers_after_the_release_before_ran` | 1 |
| `no_set_seq_restamp_at_start` | `audit` | `set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order` | 1 |
| `claim_reads_then_writes` | `periodic` | `periodic_job_runs_once_per_period_with_n_claimers` | 2 |
| `claim_reads_then_writes` | `periodic` | `alert_is_logged_once` | never |
| `claim_held_until_the_run_ends` | `periodic` | `claimer_death_loses_one_run_only` | 1 |
| `alert_logged_on_every_run` | `periodic` | `alert_is_logged_once` | 1 |
| `migration_runner_without_the_mutex` | `migrate` | `two_starters_apply_each_migration_once` | 2 |
| `migration_lock_kept_in_a_row` | `migrate` | `every_migration_is_applied_once_a_starter_survives` | 1 |
| `edit_takes_the_gate_after_its_part_row` | `locks` | `lock_order_has_no_cycle` | 2 |
| `audit_counter_taken_before_a_part_row` | `locks` | `lock_order_has_no_cycle` | 2 |
| `old_task_stops_accepting_at_sigterm` | `drain` | `no_connection_is_refused_at_the_edge_during_an_update` | 1 |
| `drain_delay_shorter_than_the_edge_needs_to_mark_the_new_task_healthy` | `drain` | `no_connection_is_refused_at_the_edge_during_an_update` | 1 |

## Bounds

| Model | Bound |
|---|---|
| `orders` | N instances, each taking beats of any of 4 hosts without end; s1 on h0, h1, h2 (max_updating 1, min_up 1, deferring until it stops once), s2 on h3; --live-rollouts 1; one crash after COMMIT, one hung holder |
| `operator` | N instances, each taking beats of either of 2 hosts without end; one operator edit (halt, hold, or desired 1 to 2); one database outage |
| `steps` | N instances, each taking beats without end; three scenarios: a plan at stage 1 of 2 advanced by beats and by one operator call, a failed threshold halted by beats, a failure report arriving once while a package is both rolling back and being uninstalled |
| `quiet` | N instances, each taking beats of either of 2 hosts without end and deciding after each beat; one operator halt; up to 2 gaps in the beats, each a database crash or a restart or outage, which abort what is in flight; a fresh host may go silent |
| `ledger` | N instances, each with one change (increment a counter, write one audit line) and one lost commit; the old backend commits or rolls back at any moment; one re-issue by new request per instance |
| `beats` | N instances; one host sending beats without end, sent values 5 then 6 repeated, its clock stepping back once to 1 then 2 repeated; the row ages past the reorder window only with no beat in flight |
| `reload` | N instances, each taking beats of one host without end; one part with its version; an operator making two edits, each setting the desired version to 0 or 1 |
| `audit` | N instances each writing one audit line before and one after the release before ran, which writes one line and changes the version of s1 once; each instance then sets one desired version of s1 or s2; two services; the re-stamp has no row without set_seq and no desired_seq behind the rows |
| `periodic` | N instances ticking without end; one job with an alert condition that holds from the start; periods 0, 1 and 2, time moving on after the first claim attempt of a period; one claimer dies once after claiming |
| `migrate` | N starters running 2 migrations, all starting at once; one starter death in the whole run, at any step, after which it is restarted |
| `locks` | N instances, each running one transaction at a time of any of 10 kinds and then another, without end; 9 locks (gate, 6 part rows, audit counter, migration lock) and the shared fence lock; no lock timeout |
| `drain` | one task of N replaced start-first; edge check every 2 s, 2 passes to mark healthy, 2 failures to mark unhealthy, DNS refresh 1 s, panic below half healthy; --drain-delay-secs 8, the smallest the deploy check admits; every second of readiness, healthcheck, DNS and check phase searched |
