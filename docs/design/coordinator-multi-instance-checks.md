# The coordinator as N instances: model check, compatibility and tests

Companion of [coordinator-multi-instance.md](coordinator-multi-instance.md) (sections 14 and 15 of it, moved here so that file
stays under 1000 lines, R12). Section numbers are kept; "section n" in the text below refers to that document.

## 14. The model check, before code

The protocol to check is the deciding transaction, the conditional writes and the periodic claim, not the SQL. Two tools, one
idea each:

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
  `bake_in_sql_matches_bake_after`, `order_slot_is_freed_when_the_applied_beat_reports_the_target`,
  `no_order_after_a_gap_in_beats_until_quiet_ends` (clean database restart, outage over `DOWN_AFTER`, back-out then
  new cutover), `lock_order_has_no_cycle`.
  Added by the leaderless decision: `stage_advance_by_racing_instances_is_one_advance_and_one_audit_line`,
  `halt_raised_by_two_instances_is_one_generation_and_one_audit_line`, `rollback_is_committed_before_the_uninstall_is_ordered`,
  `one_host_never_has_two_orders` (the unique index, with the gate bypassed), `periodic_job_runs_once_per_period_with_n_claimers`,
  `claimer_death_loses_one_run_only`, `database_unreachable_orders_nothing_and_answers_unavailable`,
  `gate_busy_answers_the_beat_with_no_new_order`.
- Added by the second review (section 20, items 7 to 11): `two_starters_apply_each_migration_once` (two runners at once, a duplicate
  of the unlocked variant fails), `migration_ddl_gives_up_by_lock_timeout_instead_of_stalling_heartbeats` (a held gate
  transaction, a DDL with `lock_timeout`: no beat statement waits longer than the timeout, injected clock not wall time),
  `migration_lint_refuses_a_rewrite_of_a_hot_table` (negative case, T15), `cache_is_never_labelled_newer_than_its_content`,
  `partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing` (a failed-looking host set after a gap),
  `operator_halt_in_quiet_window_is_read_by_every_decision`, `old_task_accepts_until_the_drain_delay_ends` and
  `draining_task_answers_healthz_503_while_serving` (injected clock, T10), `drain_delay_config_is_refused_at_deploy_when_not_above_the_edge_check`,
  `stalled_holder_is_terminated_and_gate_is_released` (the server's idle-in-transaction timeout against the service container,
  set short for the test), `beat_never_overtakes_a_queued_operator_edit` (against real Postgres),
  `operator_halt_waits_for_one_holder_not_for_beats`.
- Return after a back-out (the old-release-on-new-data test extended with the new release again after it):
  `new_release_again_continues_audit_numbers_after_the_release_before_ran` and
  `set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order`.
- `denied_host_is_refused_by_the_other_task`, `one_request_is_approved_once`, `session_is_valid_on_the_other_task`,
  `login_failures_count_across_tasks`, `expiry_audits_each_request_once`, `alert_is_logged_once`.
- `lost_session_orders_nothing_until_revived_and_settles_by_ledger` (replaces the `lease.commits` settle test).
- T5/T13: each test above has a failure case (the gate removed, the conditional write made unconditional).

Coverage is judged per crate as always (T1); the pool, the ledger and the conditional writes are covered by the
service-container tests (T17: no arm covered only by a different binary).
