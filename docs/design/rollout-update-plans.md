# Rollout update plans

Status: design, for the maintainers' comments. It replaces the slot rule of `hostkeeper-design.md` section 5.4 ("One update is live in the whole farm at a time") and the canary section of `host-updates.md` 5.3. Where they disagree, this document wins.

## What changes for the operator

**Before.** The coordinator ordered one ordinary update at a time across every service and every host. A roll of one service over N hosts took N update-and-bake rounds, one after the other. `max_updating` and `min_up` were kept in the desired state and printed, and no decision read them.

**After.** Each service rolls out by an **update plan**: a list of stages, each the cumulative percent of the service's eligible hosts that run the new version once the stage is done. The default plan is `[1, 10, 50, 100]`. All the hosts a stage adds update in parallel, capped per role by the desired state's `max_updating`, and never below the desired state's `min_up`. Every host of a stage converges and bakes before the next stage starts. A stage can wait for an operator (`advance = "manual"`), and an operator can pause and resume a plan. Across services, one service's rollout is live at a time by default, as before; this is now a coordinator flag (`--live-rollouts`).

**What does not change.** The order of the rules before the plan (halt, membership, hold, quiet start, bad version, minimum version, one update at a time on a host, up to date, failed here, deferred); the bake (`bake_beats`, kept in memory and restarted by a coordinator restart); automatic rollback and its threshold; holds, force, abort, exclusion and assignment; the JSON routes, whose bytes are frozen. A service of one host rolls out exactly as before.

## 1. The plan

### 1.1 The rollout file

A service's `[[service]]` table takes `stages`, a list of 1 to 16 stages. A stage is a percent, or a table with a percent and how the plan enters it:

```toml
[[service]]
name = "hello"
roles = ["farm"]
bake_beats = 2
stages = [1, { percent = 10, advance = "manual" }, 50, 100]
```

- A percent is 1 to 100. The percents rise strictly, and the last is 100: a plan that stops short of every host would leave the service split for good. The rollout file refuses any other list, naming the service.
- `advance` is `"auto"` (the default) or `"manual"`. A stage table refuses any other key.
- A stage that comes out the same size as the one before it adds no host and is folded into it (section 1.2); when such a stage is `manual`, the next stage that adds hosts waits for the Advance instead, so an operator still approves before the plan goes wider.
- A service with no `stages` has the default plan, `[1, 10, 50, 100]`, every stage `auto`.
- `canary = { count = n }` is still read. It is the same thing as a first stage of `n` hosts, entered automatically, before the stages of the plan. `canary = { count = 3 }` on eight hosts with the default plan gives 3, 4 and 8; `stages = [30, 50, 100]` gives the same stages and the same orders. The rule that a release failure on a canary rolls back alone (section 4) stays with `canary`.

### 1.2 Eligible hosts and stage sizes

A host is **eligible** for a service when it is live (it heartbeat within the down limit, 45 seconds), the service is assigned to it, an operator has not excluded or held it there, no standing force covers the pair, it has no failure of the desired version that an operator acknowledged (section 1.5), and the coordinator could order it the service: it runs it, or it is to install it and its agent takes installs and the desired version has a package for its platform. Eligibility is computed at every decision from the fleet the heartbeats built; nothing about it is stored.

With `n` eligible hosts, a stage of percent `p` holds `max(1, ceil(p * n / 100))` hosts, never fewer than the stage before it and never more than `n`. A canary stage holds `min(count, n)`. Stages that come out the same size **collapse** into one: on 8 hosts, `[1, 10, 50, 100]` gives 1, 1, 4 and 8, which is three stages, 1, 4 and 8. On 100 hosts it gives 1, 10, 50 and 100. On one host every plan is one stage of one host. A collapsed stage is `manual` when its first stage is, or when a `manual` stage was folded into the one before it; it is numbered by its place among the collapsed stages, so the status says "stage 2 of 3".

### 1.3 Inside a stage

A host has **started** in the plan when it is updating, has an order outstanding, runs the desired version, or failed an update or install to it. The plan's hosts are counted against the current stage's size:

- The hosts that have started count first, whatever stage they started in.
- The free places of the stage go to the eligible hosts that have not started as they heartbeat, the first to ask first. A host that is deferring (it declined the update and its wait is not over) is not ordered, so it takes no place; nor does a host the coordinator cannot order (an agent that needs a package digest the version lacks). The status lists the hosts that wait by node id.
- All the hosts that hold a free place are ordered in parallel, as each heartbeats, under two limits, both per role and both read from the desired state:
  - `max_updating`: no more hosts of the role hold an update slot for the service at once than the state's number for the role. A role the state does not name has the default the state's format documents: 1, or 2 for `daemon` and `mac-daemon`. A `max_updating` of 0 holds the role. A host holds a slot as before: updating, promotion pending, failed until it beats healthy, or ordered until it reports or the order ages out. A held host that is updating still holds its slot.
  - `min_up`: a host is not ordered when that would leave fewer live hosts of the role up (healthy, not updating, not ordered) than the state's `min_up` for the role. With `min_up = 6` of 8 live hosts, at most two are updating or unhealthy at once.
- A host whose order is held by `max_updating` is told `no_slot`; by `min_up`, `below_min_up`.

A stage is **done** when it is full (as many eligible hosts have started as the stage holds) and every eligible host that started is up at the desired version and has baked: it beat healthy `bake_beats` times at the version, or it was recorded done at an earlier stage (section 3). Until then the next stage does not start.

### 1.4 Advancing

When a stage is done, the plan enters the next stage. An `auto` stage is entered as soon as the stage before it is done: the decision reads the position the plan reaches by itself from the one kept (its **effective** position), and the coordinator writes that position, with an audit entry (`rollout_advance`, by `coordinator`), at the same heartbeat that finds the stage done, before it answers any order. A write that fails is logged and tried again at the next heartbeat; the decision does not wait for it, since the position it records follows from the hosts' reports. Neither enters a stage while the plan is paused, the service is halted or its version is marked bad, or the coordinator is within its quiet start. A `manual` stage waits for an operator's `Rollouts.Advance`, which enters it with an audit entry under the operator's token name. A first stage that is `manual` waits for an Advance before any host is ordered.

When hosts already run the desired version (a rollback to a version most hosts still run, say), the coordinator enters every `auto` stage they already fill in one write: the plan goes straight to the first stage with hosts left to order, or stops at a `manual` stage.

`Rollouts.Pause` stops the plan: no host that has not started is ordered, while updates already ordered or running finish and bake. `Rollouts.Resume` lets it go on from the same position. A pause belongs to the service, not to a version: it stays until a Resume, across a new desired version.

### 1.5 Failure

The rollback threshold is unchanged: release-class failures (phase `after_commit` or `gate_release`, class not `environment`) of the desired version on live, unheld hosts, at least the larger of `rollback_after_failed.percent` of the live hosts of the service and its `min_hosts`. It is checked at every heartbeat that brings a verdict, so it is reached within a stage, as the failures arrive, not at the stage's end. Reaching it marks the version bad, which halts the plan (every host is told `bad`), and with `rollback = "automatic"` sets the desired version back to `last_good`, as before.

A failure below the threshold does not stop the stage: the stage's other hosts go on. But a stage with an eligible host that failed is not done, so the next stage does not start until an operator resolves the host: holds it or excludes it, clears the halt its failure raised, or sets a version by hand (which acknowledges the failures standing). A resolved host is no longer eligible, and the stage takes another host in its place. Setting another version starts the plan over.

### 1.6 Across services

`--live-rollouts N` (default 1, at most 16) is how many services may have a live rollout at once. A service's rollout is **live** when one of its hosts holds an update slot or bakes the desired version, or when its plan has started a host and still has a host to order (not counting hosts that are deferring). A paused plan, one waiting for an Advance and one held by a failed host are live: the operator is in the middle of it. A service whose rollout is not live is told `rollout_elsewhere` while `N` others are. A service that is live is never held by the others, so two live rollouts (after a restart, say) do not wait on each other. An uninstall counts towards its service's rollout the same way.

This keeps the rule of one service's rollout at a time across the fleet as the default, as a setting rather than a constant.

### 1.7 When the eligible set changes mid-plan

Stage sizes are recomputed from the eligible hosts at every decision, and the position is the stage, not a list of hosts, so:

- **A host joins** (newly assigned, back from silence, a hold released). It counts in `n`, so the stages may grow. It has not started, so it takes a free place of the current stage if the stage has one when it asks; otherwise it waits for a later stage. It never makes the plan go back.
- **A host leaves** (excluded, held, retired, unassigned, silent past the down limit). It no longer counts in `n` or in the stage. If it had started, the stage has a free place again and the next eligible host takes it. An update it was running still holds its slot (and so counts against `max_updating`) until its beats or its order's age free it, as before.
- **A host is held mid-update.** As leaving; its update finishes.
- **The stage shrinks below the hosts already started** (hosts left the plan). The stage is full; nothing is undone.
- **Much of the fleet goes silent mid-plan** (a partition). The stages shrink with the eligible hosts, so the plan can enter a later stage over the hosts that remain; when the others come back the plan stays where it went. `max_updating` and `min_up` still pace the hosts it then orders. After a coordinator restart the quiet start keeps this from happening while the fleet is heard again.

### 1.8 Holds, force, abort and halts inside a plan

- **Hold.** A held pair is not eligible: it is in no stage, takes no place, does not count towards a stage being done, and is told `held`, as before. Unhold makes it eligible at the next decision.
- **Force.** A force orders its pairs itself and takes no slot; a forced pair is not eligible for the plan, and no rule of the plan holds it: its plain update route is told the desired version, which the force set to the forced one. A force of another version than the desired one sets a new desired version, so the plan starts over for it (section 3).
- **Abort.** `Hosts.Abort` stops a running update; the host reports a halt, which halts the service (`halted`). The plan keeps its position, and `Releases.ClearHalt` lets it go on from there.
- **Halt.** A halted service orders nothing; the plan waits as `halted`.

## 2. The API

### 2.1 Rollouts

A new operator service, `hostkeeper.operator.v1.Rollouts`, with three calls. Each takes the operator token only (a CI token is refused `forbidden`, no token `unauthorized`), names a service and gives a reason (1 to 256 bytes, no control characters). A refused call changes nothing.

| Call | Does | Refused when |
|---|---|---|
| `Advance` | Enters the next stage of a plan that waits for a manual advance. | the plan is not waiting for one (`not_waiting`); the service has no desired state (`no_desired`) |
| `Pause` | Pauses the plan. | it is paused already (`already_paused`) |
| `Resume` | Resumes a paused plan. | it is not paused (`not_paused`) |

All three refuse `unknown_service`, `invalid_request` (a missing or bad reason), `store_failed` and `audit_unavailable`. Each answers the audit entry it wrote and the plan as it stands after the call. `hostkeeper ctl rollouts advance --service hello --reason '...'` and the gateway route `POST /hostkeeper.operator.v1.Rollouts/Advance` are generated from the proto, as for every other call.

### 2.2 Status

`Releases.Status` gains, per service, `plan`:

- the stages after collapsing, each with its percent, its size, its `advance` and its hosts, each host with its state (`waiting`, `updating`, `baking`, `done`, `failed`);
- the current stage (1-based; 0 before a manual first stage is entered) and the number of stages;
- the eligible host count, the version the plan is for, and whether it is paused, by whom and why;
- why it waits, naming the stage: `paused`, `halted` (a halt or a bad version), `rollout_elsewhere`, `quiet`, `manual_advance`, `failed_host`, `below_min_up`, `updating`, `deferring` (the hosts left to order in the stage declined the update and wait), `baking`, or none when it has a free place to order or every eligible host has rolled.

The hosts of each stage are the plan's: the hosts that have started fill the stages in order, done hosts first, then by node id; the rest follow in node-id order, the deferring ones last. The stage shown for a host that has not started is where it would fall if the hosts were ordered by node id; it takes the first free place when it asks. Each host's `HostService` gains `stage`, the stage it is in or waits for, so every gate reason shown for it names the stage.

The per-host gate reasons gain `paused`, `advance_wait` (the host is in a later stage than a done stage that waits for an Advance), `stage_failed` (a host of the current stage failed), `stage_wait` (the current stage is still filling or updating), `below_min_up` and `rollout_elsewhere`. `no_slot` now means the role's `max_updating` is reached, and `baking` that the current stage is baking. `canary_wait` is no longer given: a canary is the plan's first stage.

The JSON routes (`/v1/status` and the others) are frozen: they gain no plan field, and their `hold` spells the new reasons in snake case, as it does every gate.

### 2.3 The web UI

The Rollout page shows, per service, the plan (its stages and their sizes), "stage k of n", why it waits, and a table of the stages with their hosts and states; with buttons to Advance, Pause and Resume, each asking for a reason and a confirmation.

## 3. Restart and what is stored

The plan's position is kept per service in the ops store: the version it is for, the number of stages entered, the hosts recorded done, and the pause (who, why). It is written in the halts file beside the ops file (`<ops-file>.halts`, section 4.2 of `hostkeeper-design.md`), under a new top-level key `rollouts`, in the same atomic two-file commit as every other change: written whole, read back on start. The halts file's reader skips a key it does not know, so the release before reads a file with `rollouts` and loses only the position; the ops file itself is unchanged, and so is what its strict readers read.

- **Writes.** The coordinator writes the position when the plan enters a stage (with the hosts done so far): every stage it enters at one heartbeat is one write. An operator's Advance, Pause and Resume write it. A rollout that never goes past its first stage writes nothing. Each write is one audit entry.
- **A new desired version** starts the plan over: a position whose version is not the desired one is read as the first stage with no host done (a pause stands). Nothing is written until the plan enters its next stage.
- **A restart** keeps the position: the plan resumes at the stage it had entered, and a host recorded done counts as baked while it is up at the version, so it is neither ordered again nor made to bake again. A host of the current stage that had not finished its bake bakes again from zero, as before. The fleet, the bake counts, the deferrals and the outstanding orders are in memory and are rebuilt from heartbeats after the quiet start, as before.

Audit entries: `rollout_advance` (by `coordinator` or the operator; the reason says the service, version and stage entered, "hello 2.0: stage 2 of 3 (4 hosts)", followed by the operator's reason for a manual one), `rollout_pause` and `rollout_resume` (by the operator, with their reason). A pause is written memory first: it stands even when its audit line cannot be appended, as a halt does. An Advance and a Resume need their audit line, and are refused `audit_unavailable` without it.

## 4. Compatibility and failure modes

- **The rollout file.** `stages` is a new optional key; a file without it reads as before and gets the default plan. A release before this one refuses a file that has it, naming the key, as for every key it does not know.
- **The proto.** Added: the `Rollouts` service, its messages and `RolloutsRefusal`; `ServiceStatus.plan`; `HostService.stage`; six `GateReason` values. Nothing is renumbered or renamed. An older reader keeps the new fields as unknown fields.
- **State files.** The desired state and the ops file are unchanged. The halts file gains `rollouts`, which the release before skips.
- **A release before this one** on the same files orders one host at a time, as it did; it ignores the plan position, and this release finds it again on its next start (or starts the plan from the hosts that run the version, if the release before rewrote the halts file).
- **Behaviour change for every farm.** With the default plan, a service of more than one host updates more than one host at once from its second stage, up to `max_updating` per role. A role's default `max_updating` is 1 (2 for `daemon` and `mac-daemon`), so a stage updates one host of such a role at a time until CI sets a larger number for it; different roles of a stage update at the same time. A farm that wants one host at a time across the service sets `max_updating` of 1 for every role it runs and `stages = [100]`.
- **Canary failure.** A service with `canary` still rolls back on one release failure among the canary hosts before they have baked, and raises `failed_canary`.

## 5. Decisions

| Question | Decision | Why |
|---|---|---|
| Where the plan lives | The rollout file (`stages`), not the desired state. | The plan is a property of how a service is rolled, like `bake_beats` and `canary`; CI sets versions. |
| The per-stage cap | `max_updating` per role of the desired state, not a new key. | It is the limit the state already carries and names per role. |
| A role the state does not name | Its documented default: 1, or 2 for `daemon` and `mac-daemon`. | The desired-state format already says so; a farm that wants wider stages sets the role's number. |
| `min_up` | Per role, counting every live host of the role (held ones too) that is up. | `min_up` is about what stays serving. |
| Where the position is stored | In the halts file beside the ops file. | It is read by a lenient reader, so the release before keeps reading the ops store. |
| `advance` on a stage | It governs entering that stage. | A manual first stage then means "start the rollout by hand". |
| An unresolved failed host | Blocks the next stage until it is held, excluded or another version is set. | The brief's rule; holding or excluding it is the operator's resolution and is audited. |
| Pause | Per service, across versions; written memory first. | An operator who stops a service's rollout means the service. A pause raises protection. |
| A paused or manual-waiting plan | Live for `--live-rollouts`. | The operator is in the middle of that rollout. |
| Which hosts take a stage's free places | The first eligible hosts to ask. | A host that cannot be ordered now (deferring, an agent the version has no digest for) must not hold a place it will not take. |
| A deferring host | Takes no place, and is not counted as left to order for liveness. | It may decline for its whole backoff; it must not hold the plan or the fleet. |
| Entering an `auto` stage | Read at once by the decision; written by the coordinator at the same heartbeat. | The decision stays a pure function of what the hosts reported and the operator's marks; the write is the audit record and the restart checkpoint. |
| A plan that went on while hosts were silent | Stays where it went. | The position is a stage, not a host count; going back would re-hold hosts that already rolled. |
| A failure an operator acknowledged | Resolved: the host leaves the plan. | Clearing a host's halt, or setting a version by hand over its failure, is the operator saying the rollout may go on. |
| Canary failure rollback | Kept for services with `canary`. | It is behaviour farms rely on; `stages` without `canary` follow the threshold. |

## 6. Tests

The tests drive the decision over a fleet built from injected heartbeats at explicit times, and the coordinator over a clock the test sets; none sleeps or asserts timing. They are named in the pull request that builds this design.
