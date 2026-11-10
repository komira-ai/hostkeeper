# Rollout plans: stages, advance, pause and resume

How a service rolls a new version out, and what an operator can do to it. The design is
[rollout-update-plans.md](../design/rollout-update-plans.md). `hkctl` is [cli.md](../cli.md).
Where the coordinator holds host groups ([hostgroups.md](hostgroups.md)), the plan of a package
is the policy of each group that lists it, and each group rolls it over its own hosts: the section
Host groups below says what differs, and the rest of this page holds there as it is written.
Every `hkctl rollouts` call takes the operator token only: a CI token is refused with
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

## Host groups

Where the coordinator holds at least one host group, a package has one plan for each group that
lists it, over that group's hosts alone, and the rollout file's plan keys are not read:

- The plan's keys are the group's policy: `stages`, `canary`, `bake_beats`, `rollback`,
  `rollback_after_failed`, `defer_backoff`, `max_updating`, `min_up` and `live_rollouts`
  ([hostgroups.md](hostgroups.md#policy)). The same policy applies to every package of the group.
- The role of a host is the name of its group. `max_updating` (1 if the policy names none; 0
  holds the group) and `min_up` are the group's, counted over the group's hosts, and so are the
  hosts a stage's percent is computed over. Two groups never wait on each other.
- `--live-rollouts` is the default of a group's `live_rollouts`, and counts the group's other
  packages: `rollout_elsewhere` names those.
- The position of a plan, its pause and its done hosts, the halt, the last good version and a
  force are kept for each (group, package), in the ops file under `host_groups`. A version set in
  one group starts the plan of that group over, and no other.
- `Releases.Status` answers one plan for each (group, package), with its `host_group`.
- An install waits, and takes no place in its plan, until every package listed before it in the
  group is installed (`install_order`,
  [hostgroups.md](hostgroups.md#installs-and-uninstalls-follow-the-list)). A host moved into a
  group is admitted to the plans of the packages it must install or change before it is ordered
  any of them (`moving`, [hostgroups.md](hostgroups.md#moving-a-host)).

## Reading a plan

`Releases.Status` (`hkctl releases status`) and the Rollout page show, per service, the
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
role's `max_updating` is reached (in a host group, the group's), and `baking` that the current
stage is baking.

## Advance

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
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
| `ambiguous_host_group` | in host groups: the call names no `host_group` and more than one group lists the service |
| `not_in_host_group` | in host groups: `host_group` names a group that does not list the service or that no group has the name of, or no group lists the service; without host groups, any `host_group` |
| `invalid_request` | the reason is missing, too long or has a control character, or `host_group` is no group name |
| `audit_unavailable` | the audit line could not be appended (a full disk, on the file store; the database store commits the line with the change and never answers this); nothing changed, ask again once the disk has room |
| `store_failed` | the position could not be written (on the database store, also while its session is lost); nothing changed (for `Pause`, which is written memory first, the pause stands) |

An `auto` stage needs no Advance: the coordinator enters it at the heartbeat that finds the stage
before it done, and writes the same `rollout_advance` entry under the name `coordinator`.

In host groups `Advance` names the plan's group in `host_group`; with none it acts on the one group
that lists the service. The entry names the group, and the answer is that group's plan.

## Pause and Resume

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    rollouts pause --service hello --reason "waiting for the storage team"
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    rollouts resume --service hello --reason "storage team is done"
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

In host groups both take `host_group`. A `Pause` that names none pauses the service's plan in every
group that lists it, in one change, with one `rollout_pause` entry for each; a group whose plan is
paused already is left as it is, and `already_paused` is the answer only when every group was. A
`Resume` that names none acts on the one group that lists the service, and is refused
`ambiguous_host_group` when more do. A pause belongs to the plan of one group: it stops the hosts
of that group alone, and another group goes on.

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
have a live rollout at once. A rollout is live when one of its hosts runs an update (updating,
its promotion pending, or under an order) or bakes the desired version, or when its plan has
started a host and still has one to order (a paused plan and one waiting for an Advance are live).
A failed host does not make its service live: it still counts against its own role's
`max_updating`, but a halted service, a version marked bad, a plan held by a failed host and a
service whose only occupants are failed hosts order nothing until an operator acts, so they hold
no other service. A bake counts only for a host of the plan (live, assigned, neither excluded nor
held, picked a package of the version) that the coordinator saw update: a host the release picks
no package for (`no_build_for_host`), an excluded or held host, a host that stopped heartbeating,
and a host the coordinator first heard after it started already at the version (after a restart,
it cannot tell whether the host updated) bake nothing that holds another service. So a service
whose every host runs its desired version, with nothing updating or ordered, holds no other. A
service whose rollout is not live is told `rollout_elsewhere` while N others are, and its plan in
`Releases.Status` names them (`waiting_on`) and, for each, why it is live and on which host
(`waiting_on_why`: `updating`, `promotion_pending`, `ordered`, `baking` with the host, or
`plan_under_way`); the web UI shows it as `rollout_elsewhere on hello (baking on node-05)`. A
service that is live is never held by the others. The default keeps one service's rollout at a time across the fleet, as the
coordinator did before.

## What a restart keeps

The position of each plan (the desired version it is for, the stages entered, the hosts recorded
done, a pause) is kept in the halts file, `<ops-file>.halts`, under `rollouts`
([coordinator-state.md](coordinator-state.md)), and read back at start (the position of a plan in
a host group is kept in the ops file, under `host_groups`): the plan goes on at the
stage it had entered, and a host recorded done is neither ordered again nor made to bake again. A
host of the current stage that had not finished its bake bakes again from zero; its bake holds
no other service's rollout (the coordinator did not see it update). A new desired
version starts the plan over. A plan that never goes past its first stage writes nothing.

A coordinator of the release before this one, started on the same files, reads the halts file
without the position and orders one host at a time, as it did; this release finds the position
again on its next start.

On the database store ([coordinator-state.md](coordinator-state.md#the-database-store)) the
position is the row of the service in the table `rollouts` and the hosts recorded done are rows of
`rollout_done`, read back at start the same way. While the database's session is lost the
coordinator is quiet: no stage is entered and no host is ordered until the session is back.

## Force, holds and abort

A force orders its pairs itself and passes the plan ([holds-and-force.md](holds-and-force.md#force)):
the pairs it orders take no place in a stage and no slot, and the new desired version it sets
starts the plan over for the other hosts. A held pair is in no stage. `Hosts.Abort` stops a running
update; the host reports a halt, which halts the service, and the plan keeps its position until
`Releases.ClearHalt` lets it go on from there.
