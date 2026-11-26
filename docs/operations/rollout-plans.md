# Rollout plans: stages, advance, pause and resume

How a service rolls a new version out, and what an operator can do to it. The design is
[rollout-update-plans.md](../design/rollout-update-plans.md). `hk` is [cli.md](../cli.md).
The plan of a package is the policy of each host group that lists it ([hostgroups.md](hostgroups.md)),
and each group rolls it over its own hosts: the section Host groups below says how.
Every `hk rollouts` call takes the operator token only: a CI token is refused with
`forbidden`, no token with `unauthorized`, and a refused call changes nothing (one exception: a
`Pause` stands without its audit line, see Pause and Resume below). Each call names a
service and gives a reason (1 to 256 bytes, no control character), and is an entry of the audit log
under the name of the token that made it.

## The plan

A package rolls out by an update plan: a list of stages, each the cumulative percent of the group's
eligible hosts that run the new version once the stage is done. The plan is a property of the host
group, not of the desired state CI sets: the group's policy
([hostgroups.md](hostgroups.md#policy)) holds its keys, and each package of the group rolls by
them.

```json
{ "hostGroup": { "name": "farm", "packages": ["hostkeeper", "hello"],
    "policy": { "bakeBeats": 2,
                "stages": [{ "percent": 1 }, { "percent": 10, "advance": "STAGE_ADVANCE_MANUAL" },
                           { "percent": 50 }, { "percent": 100 }] } } }
```

- A policy with no `stages` has `[50, 100]` (a rollout file's service, which has no group, keeps `[1, 10, 50, 100]`), every stage entered by itself (`auto`). A canary goes before them as before.
- The percents rise strictly and the last is 100; `Put` refuses any other list (`invalid_policy`).
  `advance` is auto or manual.
- `canary` with a `count` of `n` is a first stage of `n` hosts, entered by itself, before the
  stages. A release failure among the canary hosts before they have baked still rolls the version
  back at once and raises the `failed_canary` alert.
- A stage of percent `p` over `n` eligible hosts holds `max(1, ceil(p * n / 100))` hosts; stages
  that come out the same size are one stage. Six hosts with the default plan roll as 3 and 6, eight
  as 4 and 8. One host is one stage.

The hosts of a stage update in parallel, as each heartbeats, up to the policy's `max_updating` (the
whole stage if it names none; 0 holds the group), and never so that fewer live hosts of the group than its
`min_up` stay up. A stage is done when every host in it is up at the desired version and has baked
(`bake_beats` healthy heartbeats); only then does the next stage start. A group that wants one host
at a time sets `max_updating` of 1. A group whose `min_up` equals its host
count never has a host ordered.

A host is eligible for a plan when it is live, its group lists the package, it is neither
excluded nor held there, no standing force covers it, and the coordinator could order it the
package. A held, excluded or forced host is in no stage and does not stop one from being done.

A host in its turn of a maintenance run (from the `maintain` order that starts it until the turn
is done, deferred or put back, a halted turn included) is ordered no package update, install or
uninstall (`busy_elsewhere`), so a long drain must not hold a stage open, and it does not:

- A host that has not taken its place in the plan (it does not run the desired version) is in no
  stage until its turn ends: the stage is done once its other hosts are, and the host takes its
  place, and gets the update, after its turn.
- A host already at the desired version keeps its place and counts as done, though it is down or
  has not finished its bake while it drains and reboots; one that failed the version stays failed.
- `min_up` counts it as down.
- A run takes no place of `--live-rollouts`: a plan whose only hosts left to order are in their
  turn is not live, and a host in its turn bakes nothing that makes it live.

## Host groups

A package has one plan for each group that lists it, over that group's hosts alone:

- The plan's keys are the group's policy: `stages`, `canary`, `bake_beats`, `rollback`,
  `rollback_after_failed`, `failed_hosts`, `defer_backoff`, `max_updating`, `min_up` and
  `live_rollouts`
  ([hostgroups.md](hostgroups.md#policy)). The same policy applies to every package of the group.
- The role of a host is the name of its group. `max_updating` and `min_up` are the group's, counted
  over the group's hosts, and so are the hosts a stage's percent is computed over. Two groups never
  wait on each other for hosts, but they do wait for the cluster's one live rollout: see below.
- `--live-rollouts` counts services over every group, and a group's `live_rollouts` can only lower
  it: `rollout_elsewhere` names the services that hold the place.
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

`Releases.Status` (`hk releases status`) and the package page (Packages, then the package) show, per service, the
plan: its stages with their sizes and hosts, each host `waiting`, `updating`, `baking`, `done` or
`failed`; the current stage ("stage 2 of 3"); and why the plan waits:

| waits | meaning |
|---|---|
| `paused` | an operator paused the plan (by whom, and why, is shown) |
| `halted` | a halt holds every host of the package at its desired version (an operator's halt, or a host's halt reported at that version: [holds-and-force.md](holds-and-force.md#marks)), or the desired version is marked bad |
| `rollout_elsewhere` | another service's rollout is live (see `--live-rollouts` below) |
| `quiet` | the coordinator was started recently and has not heard the fleet, or (on the database store) the fleet's beats had a gap, or the database's link is lost |
| `manual_advance` | the next stage is `manual`: it waits for `Rollouts.Advance` |
| `failed_host` | failed hosts hold the plan: under `failed_hosts = FAILED_HOSTS_STOP` a host of the current stage failed the desired version; under `FAILED_HOSTS_CONTINUE` a canary host failed while the canary is the current stage, or as many hosts failed as the ceiling (see [A failed host](#a-failed-host)) |
| `below_min_up` | ordering the next host would leave fewer hosts of its group up than `min_up` |
| `updating` | the stage's hosts are updating |
| `deferring` | the hosts left to order declined the update and wait out their backoff |
| `baking` | the stage's hosts are updated and baking |
| `nothing_to_order` | every host the plan has left to order is held by a rule of its own host (it holds a halt of its own, its install waits `install_order`, it is `moving`, another update runs on it), or, under `FAILED_HOSTS_CONTINUE`, every host is done or failed: the plan takes no place of `--live-rollouts` (below), and each host's gate reason says which rule holds it |

A plan that takes no place of `--live-rollouts` (it is not live and nothing of it can be ordered
now: `failed_host`, `manual_advance`, `below_min_up`, `deferring`, `nothing_to_order`, a pause or a
halt) says its own reason, never `rollout_elsewhere`: what holds it is its own, not the services
that roll meanwhile.

A failed host shows as `failed` in its stage. Under `FAILED_HOSTS_CONTINUE` a plan whose every host
is done or failed has nothing left to order: it says `nothing_to_order` and takes no place.

Each host's gate reason names its stage. `paused`, `advance_wait`, `stage_failed`, `stage_wait`,
`below_min_up` and `rollout_elsewhere` are the reasons the plan adds; `no_slot` means the
group's `max_updating` is reached, and `baking` that the current
stage is baking.

The hosts of a service in `Releases.Status` are the hosts that report it and also every host its
group (or its role) places it on that does not run it yet, with no beat: `missing` while it is
to be installed there, or `excluded`, `down` or `unmanaged`, each with its gate reason. So a
package that no host runs yet still shows each host of its group and why it is not ordered. A
plan with no eligible host has no stage, and waits for nothing (`waiting` none, `eligible` 0):
the hosts' gate reasons say why. `no_build_for_host` there means that the desired version's
release has no entry the host can install; for example, a release whose only entry per platform
is `hostkeeper-archive`, which no host is ever picked
([packages-native.md](../design/packages-native.md)). Set a version whose release lists a native
entry (`apt`, `macos-pkg`) for the host's platform.

## Advance

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
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
| `unknown_service` | no group lists the service |
| `ambiguous_host_group` | the call names no `host_group` and more than one group lists the service |
| `not_in_host_group` | `host_group` names a group that does not list the service or that no group has the name of, or no group lists the service |
| `invalid_request` | the reason is missing, too long or has a control character, or `host_group` is no group name |
| `audit_unavailable` | the audit line could not be appended (a full disk, on the file store; the database store commits the line with the change and never answers this); nothing changed, ask again once the disk has room |
| `store_failed` | the position could not be written (on the database store, also while its session is lost); nothing changed (for `Pause`, which is written memory first, the pause stands) |

An `auto` stage needs no Advance: the coordinator enters it at the heartbeat that finds the stage
before it done, and writes the same `rollout_advance` entry under the name `coordinator`.

`Advance` names the plan's group in `host_group`; with none it acts on the one group
that lists the service. The entry names the group, and the answer is that group's plan.

## Pause and Resume

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    rollouts pause --service hello --reason "waiting for the storage team"
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    rollouts resume --service hello --reason "storage team is done"
```

`Rollouts.Pause` stops the plan ordering any host that has not started. The updates already
ordered or running finish and bake; nothing is undone. The pause belongs to the service, not to a
version: it stands across a new desired version, a coordinator restart and a new stage, until
`Rollouts.Resume`, which lets the plan go on from the same position. A paused plan still counts as
a live rollout, so with the default `--live-rollouts 1` it keeps the other services waiting.

A Pause is written memory first, like a halt: it stands even when its audit line cannot be
appended or its commit fails (the failure is logged as an error and the entry stays in the ring
`Releases.Status` shows; on the database store the change stands in memory, is committed again at
each heartbeat, and is listed in the file of `--halt-marker-file` until it lands). An Advance and
a Resume need their audit line.

| Call | Refused when |
|---|---|
| `Pause` | `already_paused`: the plan is paused already |
| `Resume` | `not_paused`: the plan is not paused |

Both take `host_group`. A `Pause` that names none pauses the service's plan in every
group that lists it, in one change, with one `rollout_pause` entry for each; a group whose plan is
paused already is left as it is, and `already_paused` is the answer only when every group was. A
`Resume` that names none acts on the one group that lists the service, and is refused
`ambiguous_host_group` when more do. A pause belongs to the plan of one group: it stops the hosts
of that group alone, and another group goes on.

Both also refuse `unknown_service`, `invalid_request`, `store_failed` and, for `Resume`,
`audit_unavailable`.

## A failed host

What a host that failed the desired version does to the rest of its plan is the group's policy key
`failed_hosts` ([hostgroups.md](hostgroups.md#policy)):

- `FAILED_HOSTS_CONTINUE`, the default (also what `FAILED_HOSTS_UNSPECIFIED` and a group put
  before the key read as): the failed host stays at the version it runs and is an alert. Its stage
  counts it as done, failed: a stage is done when as many of its hosts are done or failed as it
  holds. The plan goes on ordering the other hosts, `max_updating` at a time, and the failed host
  holds no slot of `max_updating`. The plan stays live, and takes the one place of
  `--live-rollouts`, while it has hosts to order.
- `FAILED_HOSTS_STOP`: the failed host holds its stage. The stage is not done (`failed_host`; the
  gate reason of its other hosts, once its places are taken, is `stage_failed`), so the next stage
  does not start until an operator resolves the host. A failed host that is not healthy (one halted
  after its switch) keeps a slot of `max_updating`. This is what every group did before the key; a
  group that relied on one failure stopping everything sets it, or a canary.

A service of the rollout file (no host group) keeps `FAILED_HOSTS_STOP`, and gets no automatic
retry.

Under either, a failure among the canary hosts, while the canary is the current stage, holds the
plan as `FAILED_HOSTS_STOP` does: nothing past the canary is ordered (and with `rollback =
"automatic"` the canary's release failure rolls the version back, as before).

**The ceiling.** Under `FAILED_HOSTS_CONTINUE`, `rollback_after_failed` is a ceiling: once as many
of the plan's eligible hosts have failed as the larger of `min_hosts` and `percent` of those hosts
(rounded up), the plan stops. A host whose failure it can clear by itself, which the coordinator
retries (below), is not counted: it says nothing of the release. Every host not yet ordered waits
`stage_failed`, the plan waits `failed_host` and takes no place; orders already given run to their
end. This is the one case where a healthy host is left at its version. With the defaults (25 percent, 2 hosts):

- a group of six hosts: the ceiling is 2 (25 percent of 6 is 1.5, rounded up to 2). One host
  fails, and the other five are updated. A second failure stops the plan: with `stages = [50,
  100]` and `max_updating = 1`, if the first two hosts ordered both fail, the other four are not
  updated.
- a group of four hosts: the ceiling is also 2 (25 percent of 4 is 1, and `min_hosts` is 2). One
  host fails, and the other three are updated; a second failure stops the plan.

Independently, with `rollback = "automatic"`, release failures that reach the same threshold (the
larger of `rollback_after_failed.percent` of the service's live hosts and its `min_hosts`) mark the
version bad, which halts the plan, and set the desired version back to the last good one; that is
checked at every heartbeat that brings a verdict, so it can be reached in the middle of a stage.

**Automatic retry.** In a host group, a failure whose reason the host can clear by itself
(`busy`, `verify_unavailable`, `fetch_failed`, `package_manager_busy`, `no_space`), that failed with
no phase or before the commit, is retried by the coordinator: once the group's `defer_backoff` has
passed since the host reported it, its report is set aside, as `Hosts.Retry` sets one aside, and the
host is ordered the version again at that heartbeat, at the next try. The n-th failure in a row of
one version waits `initial_secs * factor^(n-1)`, capped at `max_secs`: 300 s, 600 s, 1200 s and so
on up to 3600 s with the defaults. A run of failures ends when the host reports anything else: a
success, a failure it cannot clear by itself, or the version running. Any other reason (`manifest_invalid`,
`digest_mismatch`, `signature`, `config_missing` or another missing prerequisite, a halt) is not
retried: it stands until an operator resolves the host. The runs are kept in memory only: after a
restart the first report heard starts one, and the host is ordered again `initial_secs` after it.
A deferral (the host declined the update for its moment) backs off by the same key, as before.

An operator resolves a failed host by one of:

- `Services.Hold` or `Hosts.Exclude` of the host for the service: it is no longer eligible, and the
  stage takes another host in its place;
- `Releases.ClearHalt`, when its failure raised a halt: the failure is acknowledged and the host
  leaves the plan's count of failures;
- `Hosts.Retry` of the host and the service, to try the version on it again: its failed report is
  set aside, and it is ordered the next try when the stage has a place for it (see
  holds-and-force.md); a host whose failure was acknowledged is not ordered again without it,
  not even once it is included again;
- setting another desired version, by hand or by CI, which starts the plan over (a version set by an
  operator over a failure acknowledges the failures standing).

Every failed host is an alert of `Alerts.List` (`hk alerts list`), so the operator's tooling that
polls it tells a person at once: `failed_update`, critical, one for each host and service whose
last report is a failure of the group's desired version, with the version, the report's `reason`
and `detail` and its try (`tries`), first observed when this coordinator first saw it. A failure
the coordinator retries by itself stays listed through its tries, and its message says so ("the
host can clear it by itself (2 in a row): it is ordered 2.0 again after the group's
defer_backoff, longer after each failure"). It clears when the host reports another outcome (it
reaches the version, a failure it cannot clear by itself replaces it, or a new try an operator
asked for is under way: a report `Hosts.Retry` set aside is none), when another version is set, or
when the host is excluded or leaves its group. A host that holds a halt of its own is `host_halt`
instead ([holds-and-force.md](holds-and-force.md#marks)). A plan held by its failed hosts holds no
other service: it says `failed_host`, and the other services of every group roll meanwhile.

**A deferral that repeats.** A host that declines the group's desired version three times in a
row, each time it is offered the version again after its wait, for the same reason, is the
critical alert `repeated_deferral`: the reason may be one that does not clear by itself (a
prerequisite the system refuses to set up, say). With the default backoff the third comes about
15 minutes after the first (300 s, then 600 s). The alert names the host, the service, the version,
the reason and how many times in a row (`tries`). It clears when the host updates, gives another
reason, or another version is set. The host still waits out a longer backoff each time, and a
deferring host never holds another host of its plan or another service's rollout: a plan whose
hosts left to order all defer takes no place (`deferring`).

## Example: one host fails, under `continue`

A group `build` of six hosts, b1 to b6, with `stages = [50, 100]`, `max_updating = 1` and the
default `failed_hosts`, rolls `web` 2.0; a group `two` has `db`, whose 2.0 is set after `web`'s.
The coordinator runs at the default `--live-rollouts 1`.

1. b1 is ordered `web` 2.0 (stage 1 of 2 holds three hosts). Its update fails before the commit
   (`manifest_invalid`): b1 stays at 1.0, is `failed_here`, and `hk alerts list` answers one
   `failed_update` alert for it.
2. b1 holds no slot: b2 is ordered, and once it is done, b3. Stage 1 is then done (two done and
   one failed of its three), and stage 2 orders b4, b5 and b6, one at a time.
3. Meanwhile `db` waits `rollout_elsewhere` on `web`, which has hosts to order. Once b6 is done,
   `web` has none (five done, one failed): its plan says `nothing_to_order`, takes no place, and
   `db` is ordered.

Had b2 failed too, the plan would be at its ceiling (two of six): b3 to b6 wait `stage_failed`,
the plan waits `failed_host`, and `db` is ordered at once. Had b1's failure been `busy` instead,
it would be retried by itself: b1 is ordered 2.0 again 300 s after it reported the failure.

## Example: one host fails, under `stop`

A fleet of two groups, the coordinator at the default `--live-rollouts 1`: `build` of six Linux
build nodes listing `hostkeeper` and `tierstore`, with `stages = [50, 100]`, and `mac` of four
Macs listing `hostkeeper`, `tierstore-config` and `tierstore`, with `stages = [25, 100]` and
`failed_hosts = FAILED_HOSTS_STOP`. CI
sets `tierstore-config` 2.0 in `mac`, then `tierstore` 2.0 in `mac`, then `hostkeeper` 2.0 in both
groups.

1. The first Mac is ordered the install of `tierstore-config` (stage 1 of 2: one host). Its
   install fails before commit (`manifest_invalid`): the Mac is `failed_here`, the other three
   wait `stage_failed`, and the plan waits `failed_host`. `hk alerts list` answers one
   `failed_update` alert: that Mac, `tierstore-config`, 2.0, reason `manifest_invalid`, its
   `detail`, try 1.
2. `tierstore` is listed after `tierstore-config`: every Mac waits `install_order`, so its plan
   says `nothing_to_order` and takes no place in the queue, though its version was set before
   `hostkeeper`'s.
3. So `hostkeeper` is the one live rollout, in both groups at once, as each group's stages and
   `max_updating` let it: three build nodes and one Mac, then the rest. Nothing waits on
   `tierstore-config` or `tierstore`.
4. An operator reads the alert, fixes the package and sets `tierstore-config` 2.1. The failed
   report is not of the desired version any more, so the alert clears; the plan starts over,
   and its first install takes the next place once `hostkeeper` is no longer live. When the
   config is on a Mac, `tierstore` has a host it can order there and takes its place in the queue
   again.

Had a build node kept a halt of `hostkeeper` of its own instead (`halted_here`), with the five
others done, the plan would say `nothing_to_order` and hold no other service, and
`host_halt` would list the node with the halt's reason and when it wrote it, until an operator
clears it (`Releases.ClearHalt`).

## Across services: `--live-rollouts`

`hostkeeper-coordinator serve --live-rollouts N` (default 1, 1 to 16) is how many services may
have a live rollout at once in the whole cluster. A service is a package name: hostkeeper, and a
package that several groups list, are each one service. A group's `live_rollouts` can only lower
the number for its own packages, never raise it. The services that wait are queued in the order
their desired versions were set, and the coordinator orders them in that order: a later-set service
is held (`rollout_elsewhere`, the status names the services ahead as `QUEUED` and gives the
plan's `queue_position`) even when the earlier one has no host updating at the moment. A service
that nothing can order (halted, at a version marked bad, paused, held by its failed hosts, waiting for an Advance or for `min_up`, or whose
hosts that have not started all defer or are held by a rule of their own host) takes no place in
the queue. A host left to order is held by a rule of its own host when the decision would hold it
before the plan's gate however the plan stood: it holds a halt of the service of its own
(`halted_here`, `stale_halt_here`), its install waits for a package listed before it
(`install_order`), it was moved into its group and is not admitted (`moving`), another service
updates on it (`busy_elsewhere`), or its agent cannot take the order. Such a service gets its
place back, by when its version was set, at the first decision that finds one of its hosts
orderable again; a service that is live meanwhile is never held by it. On the database store the
order is the number each desired version was set under (`desired.set_seq` and
`group_desired.set_seq`, taken from the sequence `desired_seq` in the transaction that sets it,
`store/postgres/queue_seq.rs`), so every coordinator on the database queues the services alike and a
restart changes nothing. Over the files the order is kept in memory; after a coordinator restart the
services that were waiting queue by their group's last change, then by group and package name. A
rollout that is live is never held by the others. A rollout is live when one of its hosts runs an
update (updating, its promotion pending, or under an order) or bakes the desired version, or when
its plan has started a host and still has one it can order now (a paused plan and one waiting for an
Advance are live). A plan under way whose every host left is held by a rule of its own host is not
live: one host that keeps a halt of its own for hours holds no other service meanwhile. A failed
host does not make its service live. Under `FAILED_HOSTS_STOP` a failed host that is not healthy
still counts against its own group's `max_updating`; under `FAILED_HOSTS_CONTINUE` it counts against
none, and its plan stays live while it has other hosts to order. A halted service, a version marked
bad, a plan held by its failed hosts (see [A failed host](#a-failed-host)) and a service whose only
occupants are failed hosts order nothing until an operator acts, so they hold no other service. A
bake counts only for a host of the plan (live, in the group, neither excluded nor held, picked a
package of the version) that the coordinator saw update: a host the release picks no package for
(`no_build_for_host`), an excluded or held host, a host that stopped heartbeating, and a host the
coordinator first heard after it started already at the version (after a restart, it cannot tell
whether the host updated) bake nothing that holds another service. So a service whose every host
runs its desired version, with nothing updating or ordered, holds no other. A service whose rollout
is not live is told `rollout_elsewhere` while N others are, and its plan in `Releases.Status` names
them (`waiting_on`) and, for each, why it is live and on which host (`waiting_on_why`: `updating`,
`promotion_pending`, `ordered`, `baking` with the host, or `plan_under_way`); the web UI shows it as
`rollout_elsewhere on hello (baking on db-5)`. A service that is live is never held by the others.
The default keeps one service's rollout at a time in each group.

## What a restart keeps

The position of each plan (the desired version it is for, the stages entered, the hosts recorded
done, a pause) is kept for each (group, package) in the ops file, under `host_groups`
([coordinator-state.md](coordinator-state.md)), and read back at start: the plan goes on at the
stage it had entered, and a host recorded done is neither ordered again nor made to bake again. A
host of the current stage that had not finished its bake bakes again from zero; its bake holds
no other service's rollout (the coordinator did not see it update). A new desired
version starts the plan over. A plan that never goes past its first stage writes nothing.

On the database store ([coordinator-state.md](coordinator-state.md#the-database-store)) the
position is the row of the (group, package) in the table `group_rollouts` and the hosts recorded done are rows of
`group_rollout_done`, read back at start the same way. While the database's session is lost the
coordinator is quiet: no stage is entered and no host is ordered until the session is back.

## More than one coordinator on the database

On the database store several coordinators can answer heartbeats at once. They share one plan,
because the plan is rows and each heartbeat is decided over them
(`store/postgres/decide.rs`, `store/decision.rs`, `service_decide.rs`):

- A heartbeat is decided in one transaction under the rollout gate. It reads the desired state,
  the operations state and the fleet's rows (`fleet_hosts`, `fleet_bakes`, `fleet_orders`) afresh,
  decides over them, and writes its changes and the orders that differ from the rows read, then
  commits. Two coordinators never decide at the same time. A beat that finds the gate held is
  answered with no new order, and the host asks again at its next heartbeat.
- The stage a decision enters is first claimed by a compare-and-set on the plan's row
  (`rollouts.rs`): of two coordinators that reach the same stage, one enters it and writes the
  `rollout_advance` entry, and the other finds the stage entered and writes nothing.
- The queue of [Across services](#across-services---live-rollouts) is ordered by `set_seq`, the
  same on every coordinator.
- A halt, a version marked bad and the rollback to the last good version that a decision makes are
  written in that decision's transaction, so every coordinator reads the same halt at its next
  decision.
- The `quiet` rule is read from the rows. When the newest `seen_unix` of `fleet_hosts` is 45
  seconds old or older, or there is no row (a database restart, an outage, a first start),
  `cluster_quiet.until_unix` is raised to `--quiet-secs` past that beat, once for the whole
  cluster (`store/postgres/fleet_rows.rs`). Once a coordinator has read the rows it judges `quiet`
  by them and not by its own start.
- It fails closed. When the database cannot be reached, a change stands in memory and is not yet
  committed, or the decision's write or commit fails, the decision is rolled back and the host is
  answered with no new order; it keeps what it runs and asks again. No halt, rollback or stage is
  decided meanwhile, and `Alerts.List` shows `store_unreachable` while the link is lost.
- An operator's `Pause`, `Resume`, `Advance`, hold and force are changes of the same rows
  under the same gate (an edit waits for the gate), so they reach every coordinator: each reads the
  changed part before it next decides.

Over the files one process holds the lock and decides over its own memory, as before; none of the
above applies.

## Force, holds and abort

A force orders its pairs itself and passes the plan ([holds-and-force.md](holds-and-force.md#force)):
the pairs it orders take no place in a stage and no slot, and the new desired version it sets
starts the plan over for the other hosts. A held pair is in no stage. `Hosts.Abort` stops a running
update; the host reports a halt, which halts the service, and the plan keeps its position until
`Releases.ClearHalt` lets it go on from there.
