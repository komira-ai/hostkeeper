# Rollout plans: stages, advance, pause and resume

How a service rolls a new version out, and what an operator can do to it. The design is
[rollout-update-plans.md](../design/rollout-update-plans.md). `hostkeeper ctl` is [ctl.md](../ctl.md).
Every `hostkeeper ctl rollouts` call takes the operator token only: a CI token is refused with
`forbidden`, no token with `unauthorized`, and a refused call changes nothing (one exception: a
`Pause` stands without its audit line, see Pause and Resume below). Each call names a
service and gives a reason (1 to 256 bytes, no control character), and is an entry of the audit log
under the name of the token that made it.

## The plan

A service rolls out by an update plan: a list of stages, each the cumulative percent of the
service's eligible hosts that run the new version once the stage is done. The plan is a property
of the service in the rollout file, not of the desired state CI sets:

```toml
[[service]]
name = "hello"
roles = ["farm"]
bake_beats = 2
stages = [1, { percent = 10, advance = "manual" }, 50, 100]
```

- A service with no `stages` has `[1, 10, 50, 100]`, every stage entered by itself (`auto`).
- The percents rise strictly and the last is 100; the coordinator refuses a rollout file with any
  other list at start, naming the service. `advance` is `"auto"` or `"manual"`.
- `canary = { count = n }` is a first stage of `n` hosts, entered by itself, before the stages. A
  release failure among the canary hosts before they have baked still rolls the version back at
  once and raises the `failed_canary` alert.
- A stage of percent `p` over `n` eligible hosts holds `max(1, ceil(p * n / 100))` hosts; stages
  that come out the same size are one stage. Eight hosts with the default plan roll as 1, 4 and 8.
  One host is one stage.

The hosts of a stage update in parallel, as each heartbeats, up to the desired state's
`max_updating` for the host's role (1 if the state does not name the role, 2 for `daemon` and
`mac-daemon`; 0 holds the role), and never so that fewer live hosts of the role than its `min_up`
stay up. A stage is done when every host in it is up at the desired version and has baked
(`bake_beats` healthy heartbeats); only then does the next stage start. A farm that wants one host
per role at a time sets `max_updating` of 1 for every role it runs, in the desired state, and
`stages = [100]`; the slot count is per role, so a service of several roles still updates one host
of each role at once. A role whose `min_up` equals its host count never has a host ordered.

A host is eligible for a plan when it is live, the service is assigned to it, it is neither
excluded nor held there, no standing force covers it, and the coordinator could order it the
service. A held, excluded or forced host is in no stage and does not stop one from being done.

## Reading a plan

`Releases.Status` (`hostkeeper ctl releases status`) and the Rollout page show, per service, the
plan: its stages with their sizes and hosts, each host `waiting`, `updating`, `baking`, `done` or
`failed`; the current stage ("stage 2 of 3"); and why the plan waits:

| waits | meaning |
|---|---|
| `paused` | an operator paused the plan (by whom, and why, is shown) |
| `halted` | the service is halted, or the desired version is marked bad |
| `rollout_elsewhere` | another service's rollout is live (see `--live-rollouts` below) |
| `quiet` | the coordinator was started recently and has not heard the fleet |
| `manual_advance` | the next stage is `manual`: it waits for `Rollouts.Advance` |
| `failed_host` | a host of the current stage failed the desired version |
| `below_min_up` | ordering the next host would leave fewer hosts of its role up than `min_up` |
| `updating` | the stage's hosts are updating |
| `deferring` | the hosts left to order declined the update and wait out their backoff |
| `baking` | the stage's hosts are updated and baking |

Each host's gate reason names its stage. `paused`, `advance_wait`, `stage_failed`, `stage_wait`,
`below_min_up` and `rollout_elsewhere` are the reasons the plan adds; `no_slot` now means the
role's `max_updating` is reached, and `baking` that the current stage is baking.

## Advance

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    rollouts advance --service hello --reason "stage 1 looks healthy"
```

`Rollouts.Advance` enters the next stage of a plan that waits for an operator: the stage after a
done stage, or the first stage when that one is `manual` (no host is ordered before it). The
answer is the audit entry (`rollout_advance`, whose reason names the stage entered: "hello 2.0:
stage 2 of 3 (4 hosts)", then yours) and the plan as it stands. It is refused:

| Refusal | When |
|---|---|
| `not_waiting` | the plan does not wait for an Advance: the stage before is not done, the next stage enters by itself, every stage is entered, the plan is paused or halted, or the coordinator is in its quiet start |
| `no_desired` | the service has no desired version |
| `unknown_service` | the coordinator does not manage the service |
| `invalid_request` | the reason is missing, too long or has a control character |
| `audit_unavailable` | the audit line could not be appended (a full disk); nothing changed, ask again once the disk has room |
| `store_failed` | the position could not be written; nothing changed (for `Pause`, which is written memory first, the pause stands) |

An `auto` stage needs no Advance: the coordinator enters it at the heartbeat that finds the stage
before it done, and writes the same `rollout_advance` entry under the name `coordinator`.

## Pause and Resume

```
hostkeeper ctl ... rollouts pause --service hello --reason "waiting for the storage team"
hostkeeper ctl ... rollouts resume --service hello --reason "storage team is done"
```

`Rollouts.Pause` stops the plan ordering any host that has not started. The updates already
ordered or running finish and bake; nothing is undone. The pause belongs to the service, not to a
version: it stands across a new desired version, a coordinator restart and a new stage, until
`Rollouts.Resume`, which lets the plan go on from the same position. A paused plan still counts as
a live rollout, so with the default `--live-rollouts 1` it keeps the other services waiting.

A Pause is written memory first, like a halt: it stands even when its audit line cannot be
appended (the failure is logged as an error and the entry stays in the ring `Releases.Status`
shows). An Advance and a Resume need their audit line.

| Call | Refused when |
|---|---|
| `Pause` | `already_paused`: the plan is paused already |
| `Resume` | `not_paused`: the plan is not paused |

Both also refuse `unknown_service`, `invalid_request`, `store_failed` and, for `Resume`,
`audit_unavailable`.

## A failed host

A release failure below the rollback threshold does not stop the stage: its other hosts go on. But
a stage with an eligible host that failed is not done (`failed_host`; the host's gate reason is
`stage_failed`), so the next stage does not start. The plan waits for an operator to resolve the
host, by one of:

- `Services.Hold` or `Hosts.Exclude` of the host for the service: it is no longer eligible, and the
  stage takes another host in its place;
- `Releases.ClearHalt`, when its failure raised a halt: the failure is acknowledged and the host
  leaves the plan's count of failures;
- setting another desired version, by hand or by CI, which starts the plan over (a version set by an
  operator over a failure acknowledges the failures standing).

Failures that reach the rollback threshold (the larger of `rollback_after_failed.percent` of the
service's live hosts and its `min_hosts`) mark the version bad, which halts the plan, and with
`rollback = "automatic"` set the desired version back to the last good one; that is checked at
every heartbeat that brings a verdict, so it can be reached in the middle of a stage.

## Across services: `--live-rollouts`

`hostkeeper-coordinator serve --live-rollouts N` (default 1, 1 to 16) is how many services may
have a live rollout at once. A rollout is live when one of its hosts holds an update slot or bakes
the desired version, or when its plan has started a host and still has one to order (a paused plan
and one waiting for an Advance are live). A service whose rollout is not live is told
`rollout_elsewhere` while N others are; a service that is live is never held by the others. The
default keeps one service's rollout at a time across the fleet, as the coordinator did before.

## What a restart keeps

The position of each plan (the desired version it is for, the stages entered, the hosts recorded
done, a pause) is kept in the halts file, `<ops-file>.halts`, under `rollouts`
([coordinator-state.md](coordinator-state.md)), and read back at start: the plan goes on at the
stage it had entered, and a host recorded done is neither ordered again nor made to bake again. A
host of the current stage that had not finished its bake bakes again from zero. A new desired
version starts the plan over. A plan that never goes past its first stage writes nothing.

A coordinator of the release before this one, started on the same files, reads the halts file
without the position and orders one host at a time, as it did; this release finds the position
again on its next start.

## Force, holds and abort

A force orders its pairs itself and passes the plan ([holds-and-force.md](holds-and-force.md#force)):
the pairs it orders take no place in a stage and no slot, and the new desired version it sets
starts the plan over for the other hosts. A held pair is in no stage. `Hosts.Abort` stops a running
update; the host reports a halt, which halts the service, and the plan keeps its position until
`Releases.ClearHalt` lets it go on from there.
