# Halt scope: what one host's halt holds

Status: design, for a decision. Nothing of it is built. It answers komira-ai/hostkeeper#418: should a halt that a service's update
hook raises for one host stop the service in the whole host group, across new desired versions? **Built** marks what exists, with
its path. The halt is defined in [host-updates.md](host-updates.md) section 3.8 and its operator surface in
[holds-and-force.md](../operations/holds-and-force.md) (Marks).

## 1. Today

A halt is the outcome of an update that found a state "no version can repair": a hook's exit `76`, a `rollback` that fails before the
commit, an operator's abort (`aborted_by_operator`), or a staged release whose digest changed (`artifact_changed`) (host-updates.md
3.2, 3.8; `crates/hostkeeper-steps/src/runner.rs`). It has two halves, both **Built**:

- **The coordinator's halt, one per (group, package).** A host's report `Failed` with phase `halt` raises it
  (`crates/hostkeeper-coordinator/src/halt.rs`, `note` and `raise`). While it stands, every host of the group is told `halted`
  (`Gate::Halted`, checked before every other rule, `halt::decide`), the plan waits as `halted`, a waiting CI job ends `HALTED`
  (`wait.rs`), and a standing halt beats an automatic rollback (`rollback_tests.rs`, `a_halt_beats_a_rollback`). It is lifted by
  `Releases.ClearHalt`, by a force, and by the halting host reporting the service absent on the order's first try when no other host
  reported a halt while it stood (`clear_if_absent`). A new desired version other than the halt's target lifts it only when its
  origin is `Updater`: the reason `artifact_changed` (`UPDATER_HALT_REASONS`, `origin_of`), with no halt of another origin reported
  while it stood (`halt::lift`, called from `service_ci.rs` and `service_groups.rs`). Every other halt, a failed `rollback` and an
  abort included, is of origin `Hook` and stays across new versions. host-updates.md says so on purpose (3.8: "setting another desired
  version does not clear it"), and its reviewer notes (rev 4.2) accept it as a residual risk ("A `halt` stops a service's rollout
  fleet-wide, and only the operator token clears it").
- **The host's own hold.** The daemon that halted keeps the halt in its step record and a `held-halt` file beside it, and refuses
  every order of the service until a reply names a halt generation above the one it holds (`crates/hostkeeper-daemon/src/halt.rs`).
  It reports the hold (`ServiceBeat.local_halt`); with no coordinator halt standing, the coordinator holds that host alone
  (`Gate::HaltedHere`, `Gate::StaleHaltHere`) and lists it as the critical alert `host_halt` (`alerts_failures.rs`). The generation is
  per (group, package): every lift that counts it (`ClearHalt`, a force, the absent rule, the `Updater` lift) lifts every host's hold of that
  package in the group.

## 2. The case

js-agent's hook exited `76` on one host because the host carried a taint (host-updates.md 4.2: "a tainted agent refuses every
start"). The coordinator halted js-agent in the group. Every later js-agent release was stored as desired (CI's `set-desired` with
`wait` false answered `not_waited`, so each release job passed), and no host of the group was ordered it, the release whose agent
clears such taints itself (komira-ai/jobscheduler#894) included. One host's state kept every host of the group on an old agent until
an operator noticed the alert.

Two things were wrong, and only one is the halt's shape. The hook's check was wrong: a taint the new agent clears is not a state no
version can repair, and komira-ai/jobscheduler#894 removed that halt. But any other hook halt keeps the shape: the remedy of a host's
state is a person looking at that host, and while nobody does, the group gets no release at all, fixes and security releases
included.

## 3. The options

Each option ends with what it would have done in the case.

### A. Stay as designed

A hook's halt stops the package in the group until `Releases.ClearHalt`.

- For: one rule, built, tested and documented; nothing is ordered after the first halt, which is the safest answer when the release
  did cause it.
- Against: by the halt's own definition the release is not the cause (3.8: "the host, not the release, is in a state..."), yet every
  host pays, and the group gets no release, security fixes included, for as long as nobody acts. A release job that does not wait
  sees success.
- Cost: none here. A release job that sets with `wait` true ends at the halt and fails (`docs/deploy/ci-client.md`, Outputs); that is
  the service's configuration, not a change in this repository.
- The case: unchanged; the fixing release waits for an operator.

### B. Every halt holds only its host

The coordinator no longer raises a group halt from a host's report. The host's own hold gates it, and the group's plan treats it as a
failed host under the group's `failed_hosts` policy (under `FAILED_HOSTS_CONTINUE`, the group default, the plan stops at the ceiling
`rollback_after_failed`; under `FAILED_HOSTS_STOP`, and for a service of the rollout file, which has no group, one failed host holds its
stage).

- For: matches the halt's definition; no halt ever stops other hosts.
- Against: the first halt of a bad release no longer stops it. Where a canary or a small `max_updating` limits parallelism, up to
  the ceiling's count of hosts (by default the larger of 25 percent of the hosts and 2) halt before the plan stops, where today one does. In a group with the default plan
  (`stages = [50, 100]`, no canary, every host of a stage ordered at once) the first stage is exposed under A and B alike. With the
  group halt gone, a standing halt no longer beats an automatic rollback, and no waiting job ends `HALTED`.
- Cost: large. A per-host clear needs a `host` field on `ClearHaltRequest` (operator API, CLI, UI); a per-host generation stored in
  the file store and in the Postgres store (a migration, the multi-instance locks and their tests); a clear that writes the host's
  report into `halt_acked` with no halt standing (no code does that today: `operate.rs`, `past_held`, and holds-and-force.md say a
  hook's own halt report stands until `Hosts.Retry`); `install_order.rs` reading `local_halt`; and the status, alert and doc changes.
- The case: the group takes the fixing release; the tainted host waits for an operator.

### C. A halt with a scope

The hook says in its last line which a halt is (a new optional field, say `"halt_scope": "host" | "service"`), the updater carries it
on `last_update`, and the coordinator does B for `host` and A for `service`. A hook that says nothing gets `service`.

- For: a hook that knows more than the coordinator can say so; no hook's halt changes meaning until its owner opts in.
- Against: a halt caused by the release contradicts 3.8 (that is a `Failed` of class `release`, which the rollback threshold and the
  bad mark handle), so `service` has no concrete user, and the default keeps the case's shape for every hook whose owner does not act.
- Cost: B's cost, plus the hook contract (host-updates.md 3.2), a wire field on `UpdateResult` and each hook.
- The case: as A until js-agent's hook sends `host`, then as B.

### D. A new desired version lifts a hook's halt, as it lifts the updater's

Drop the origin check in `halt::lift`: a new desired version lifts any halt and counts the generation, so every held host lifts its
own hold and is ordered the new version.

- For: one condition; the case ends at the next release.
- Against: releases are automatic from `main` (AGENTS.md B12), so a new desired version can follow every merge, and a halt lasts
  until the next one. The halting host is then ordered each new version, and its hook runs again on a host it said must not be touched.
- Cost: one condition, tests, and the design text.
- The case: every host, the tainted one included, is ordered the fixing release.

### E. A halt stops its version; the host holds the rest (recommended)

The group's halt stands for the version it was raised at, as today. A new desired version other than the halt's target lifts the
group's halt of origin `Hook` too, **without counting the generation**: it acknowledges the halt's report and every report kept while
it stood (as `halt::lift` does now), so none is raised again, and the halting hosts keep their own holds (`halted_here`, the
`host_halt` alert) until `Releases.ClearHalt` or a force. (The absent rule acts only while the group's halt stands, so a host whose
first install halted and that reports the service absent after the lift waits for an operator, as a halt of a later try does today.)
A halt whose every report is of origin `Updater` keeps today's lift, which counts the generation; one with a `Hook` report kept
while it stood, which today is not lifted, is lifted without counting; an operator's `Releases.Halt` (origin `Operator`) stays until cleared.

- For: the first halt of a bad release still stops that release in the group before another host is ordered, exactly as today;
  the next release reaches every other host; the host the hook said must not be touched is not touched until a person clears it.
  It reuses the coordinator's acknowledgements, the per-host hold, its gate and its alert, all built. No wire, API, store or daemon
  change. A daemon that does not report `local_halt` still refuses the order itself from its step record (a `halted` refusal), so no
  hook runs on the halting host, but for an install onto a service the host reports absent, which a daemon runs over its halt
  (`verbs/update.rs`); section 5, item 4.
- Against: the group still waits for a new version (or an operator) after the first halt: a release that halts on one host for a
  host's reason waits for the next release. Two rules for one halt (group for its version, host after) is more to explain than A or B.
- Cost: small, in the coordinator; section 5.
- The case: the fixing release reaches every host but the tainted one, which waits for an operator.

## 4. Recommendation

**E.** It keeps what the group halt is good for (a bad release stops at its first halt) and removes what it is bad at (one host's
state stopping every later release), at the cost of one rule in `halt::lift` and the treatment of the held host in the plan and the
convergence (section 5). B gives a cleaner rule but costs an API field, a store migration and a weaker stop for a bad release; it can
follow E later if the stop of the halted version proves too strict. C is B behind an opt-in no hook needs. D runs the hook again on
the host that halted.

## 5. What E changes, if chosen

Coordinator only. Each item needs a behaviour test written with it (AGENTS.md T8).

1. **The lift.** `halt::lift` lifts a halt of origin `Hook` on a new desired version other than its target, whoever sets it (CI or
   an operator, with or without a group); it acknowledges the halt's report and those in `halt_seen`, writes the audit entry `lift_halt`, and
   does not count `halt_generation` unless every lifted report is of origin `Updater`. Tests: a hook halt on one host, a new version
   set: every other host is ordered it, the halting host is told `halted_here` and is not ordered; the halting host repeating its old
   report raises nothing; the same version set again lifts nothing.
2. **The held host in the plan.** Today a host that holds only its own halt is a member of the plan; for a version after the one it
   halted on, its state is `Waiting` (`stages.rs`, `state_of` reads `Failed` only for a failure of the desired version), it is never
   ordered, and its stage is never done, so the plan never finishes. Proposed: such a host leaves the plan, as an operator-held host
   does (`stages.rs`, `eligible`), and is shown with its `halted_here` reason. Test: after the lift, the plan finishes with the halting
   host left out and shown.
3. **The held host in the convergence.** Today it is counted and never at the desired version, so a CI job that waits ends at its
   deadline, `not_converged`, at every release until the host is cleared. Proposed: it is counted as an operator-held host is (left
   out of the convergence, shown), and the `host_halt` alert, which is critical, is the signal. Test: a wait for the new version
   converges with the halting host left out.
4. **Install order and the absent host.** With no group halt standing, `install_order.rs` sees no halt of the earlier package. Where
   the halted first install was rolled back (the host reports it absent), the later package is already held there, since the earlier
   one is the install the host is ordered next; where the halt left the earlier package installed, `holds_back` returns early and the
   later package would be ordered. Proposed: `holds_back` holds a later package on a host that holds a halt of the earlier one, checked
   before its `installed_on` return; and the coordinator orders no install of a package to a host that holds a halt of it (so an older
   daemon, which lifts its halt for an install onto an absent service, is not ordered one). Tests: a halt that leaves the earlier
   package installed on one host, a new version set: the later package is not ordered on that host and is ordered on the others; a
   host that holds a halt and reports the service absent is ordered no install until `ClearHalt`.
5. **Docs.** host-updates.md 3.8, Question 11 and its reviewer notes' residual risk; hostkeeper-design.md (Halt);
   holds-and-force.md (Marks) and rollout-plans.md (`halted`).

Compatibility: no persisted record changes; `lift_halt` and the acknowledgements exist. An older coordinator on the same files reads
an acknowledged report as acknowledged, as it does after an `Updater` lift. An older daemon holds as it does today and refuses the
order itself.

## 6. What the CEO decides

1. **The option:** A, B, C, D or E. Recommended: E.
2. **If E or B, the held host in the plan and the convergence** (section 5, items 2 and 3): left out and shown, as an operator-held
   host is, with `host_halt` as the signal (recommended), or counted, so that every waiting release job fails at its deadline until
   an operator clears the host.
3. **If E, which halts it covers:** every halt of origin `Hook` (recommended: a hook's `76`, a failed `rollback` before the commit, an
   operator's abort), or a hook's `76` only, leaving a failed `rollback` and an abort stopping the group across versions as today.
4. **Whether services whose hooks halt should set a canary** (`canary = { count = 1 }` in the group's plan), so that a bad release
   halts one host before a stage of them. This is configuration in each group, not a change here; recommended for js-agent under any
   option.
