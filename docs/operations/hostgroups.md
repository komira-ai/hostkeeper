# Host groups: what each host runs

A host group is a name, the ordered list of the packages its hosts run, and one rollout policy.
Every enrolled host is in at most one group, and once the coordinator holds a group a host runs
exactly the packages its group lists, at the group's desired version of each. This page is what
the coordinator does with groups today. The design, which goes further than what is built, is
[hostgroups.md](../design/hostgroups.md); the last section lists what is not built. `hkctl` is
[cli.md](../cli.md).

Every `HostGroups` call takes the operator token only: a CI token is refused with `forbidden`, no
token with `unauthorized`, and a refused call changes nothing. A call that changes something needs
a reason (1 to 256 bytes, no control character), is an entry of the audit log under the name of the
token that made it, and answers that entry. A request is at most 4 KiB, but `Put`'s, which is at
most 64 KiB; the gateway serves each call at `POST /hostkeeper.operator.v1.HostGroups/<Method>`
([gateway.md](../gateway.md)).

## What a group holds

| Field | Meaning |
|---|---|
| `name` | 1 to 32 bytes of `a-z`, `0-9` and `-`, a letter first, no `--`, not ending in `-`: the grammar of a role name. |
| `packages` | 1 to 64 packages in order, none twice, each a service name (`A-Za-z0-9._-`, 1 to 255 bytes). `hostkeeper` is one of them. The order is the order of installs; uninstalls run in the reverse order. |
| `policy` | How every package of the group rolls out (see Policy below). |
| `generation` | 1 for a new group, and one more for each `Put` that replaces it. |

A group names no host: a host is in a group because its enrollment record says so
(`HostGroups.Move`). A group lists packages, not versions: the versions are the group's desired
versions, set with `Releases.SetDesired` (Deployments below). A package can be listed by many
groups, and a host runs it at its own group's desired version. A package's minimum version in a
group is not part of the group: only `Services.SetMinVersion` writes it, and `Put` neither reads
nor resets it, so an incident's minimum is never undone by putting the group's file again.

## Group mode

The coordinator is in group mode while it holds at least one group: the first `Put` of a group
starts it, and it ends when the last group is removed. It is decided at each heartbeat, not at
start. In group mode:

- A host runs the packages its group lists and no others. What the rollout file's `roles`,
  `selector` and `priority` say, and the operator's assignments, place nothing any more.
  `Services.Assign` and `Services.Unassign` are refused `placed_by_host_group`, before anything is
  read. The services an approval names still write assignment entries, which place nothing.
- A host in no group is ordered nothing: no install, no update, no uninstall. It keeps running
  what it runs. A newly approved host is in no group until `Move` puts it in one.
- The role the coordinator's fleet records for a host is the name of its group (empty for a host in
  none). `Hosts.List` and `Hosts.Get` show a host in a group with the group's name as `role` and as
  `host_group`; a host in none keeps the role it was approved for and an empty `host_group`. The slots of a package
  (`max_updating`), its `min_up` and its stages count the hosts of one group, so two groups never
  wait on each other; a hold of a role holds the group of that name.
- The rollout file is still read at start. It gives a package what a package has alone: its
  version scheme, its repository and the floor of its `min_version`. Its `stages`, `canary`,
  `bake_beats`, `rollback`, `rollback_after_failed` and `defer_backoff` are not read, and neither
  are the desired state's `max_updating` and `min_up`: the group's policy holds them all. A
  package that a group lists and the rollout file does not define has the default version scheme
  and repository.
- A package a host runs that its group does not list, and no uninstall intent covers (it was
  installed by hand, or by an earlier placement), is never ordered. `Hosts.Get` still shows it.
- A private repository is served to a host under the name of its group, the one its record holds,
  not the role its certificate names: the suite a host is sent
  ([repository-keys.md](repository-keys.md)) and the paths it may read are of that group. A host
  in no group reads none.

Without any group, nothing on this page applies and the coordinator places by the rollout file and
the operator's assignments ([assignments.md](assignments.md)). The groups and their desired
versions are kept in the state file under the key `host_groups`, and each (group, package)'s
marks in the ops file under the same key (on the database store, the tables of migration `0008`:
[coordinator-state.md](coordinator-state.md)). A coordinator of the release before refuses both
files, and the database, once they hold a group.

## The calls

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups put --request web.json --reason "the web tier"
```

with `web.json` the group's body, in the canonical JSON of the proto
([gateway.md](../gateway.md)):

```json
{ "hostGroup": { "name": "web",
    "packages": ["hostkeeper", "hello", "hello-config-web"],
    "policy": { "stages": [{ "percent": 25, "advance": "STAGE_ADVANCE_MANUAL" }, { "percent": 100 }],
                "bakeBeats": 2, "maxUpdating": 2 } } }
```

| Call | Does | Refused (beyond `unauthorized`, `forbidden`, `invalid_request`, `store_failed`, `audit_unavailable`) |
|---|---|---|
| `List` | The groups by name, a page at a time (`limit` up to 256, 0 is 256; `page_token` is the `next_page_token` of the page before, the last group name of it), each with its packages, policy, generation, host count (hosts moving into the group included) and, per package, its desired version. | none |
| `Get` | One group as `List` says it, with, per package, its minimum version and its convergence (as `Releases.Converge` answers it for the group), and a page of its hosts by node id (`limit` up to 256; the token is the last node id of the page before). | `unknown_host_group` |
| `Put` | Creates a group, or replaces one whole. | `invalid_host_group` (the name), `invalid_package` (a name that breaks its rule, or a package twice), `invalid_policy`, `hostkeeper_not_listed` (an empty list included), `too_many_packages`, `generation_mismatch` |
| `Remove` | Removes a group, with its desired versions and its marks. | `unknown_host_group`, `not_empty` |
| `Move` | Moves a host into another group (below). | `unknown_host`, `unknown_host_group`, `same_host_group` |

A bad `limit` or `page_token`, a bad node id or a bad reason is `invalid_request`. The refusals
are tokens of the status message, as for every operator call.

**Put.** A new group names no `if_generation`. A replacement names the generation it read from
`List` or `Get`, and is refused `generation_mismatch` when it names none, when the group has moved
on, or when it names one for a group that does not exist. The group's desired version of each
package that stays listed is kept, with that package's marks (halt, last good version, force, plan
position); a package taken out loses them. A change to the policy applies at the next decision: a
plan keeps its position and its stages are recomputed. The refusal `invalid_policy` is logged with
the key that broke its range.

A package taken out of the list is uninstalled from the group's hosts that listed it at their last
heartbeat to this coordinator: one uninstall intent for each, written with the `Put`, in the
reverse of the old list's order, so a config package listed after its service goes first. A host
the coordinator has not heard from since it started has no intent. A package put back cancels
the group's hosts' intents for it, each an `uninstall_cancelled` entry after the `Put`'s. A package
added is installed once the group has a desired version for it.

The `host_group_put` entry is about the group, and its reason names what changed, then the
operator's reason: `new group`, `packages +<added> -<taken out>`, `reordered` (the packages that
stayed changed order), `policy <keys that changed>` or `no change`, joined by `; ` and followed by
`: ` and yours, for example `packages +hello-config-web; policy bake_beats: add the web config`.

**Remove.** A group that a host is in or moving into, or that an inventory record names, is
`not_empty`. Removing the last group ends group mode. The entry is `host_group_remove`.

**Move.** The entry is `host_move`, about the host (`node_id`) and the new group (`host_group`),
with the reason `from <old> to <new>: <yours>`, or `into <new>: <yours>` for a host that was in
none. The answer names the group the host was in. `unknown_host` is a host that is not enrolled
or is denied, and the checks run in the order of the table.

## Moving a host

`Approve` and `AddRecord` do not name a group yet: `--host-group` on either is refused
`unknown_host_group`. So a host gets into a group by `Move`, and a host in a group is changed to
another by `Move`.

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups move --node-id web-17 --host-group web --reason "racked for the web tier"
```

The host is in the new group from the call: its record is written first, and the old group's plans
stop counting it. The same commit writes the audit entry, an uninstall intent for each package
of the old group that the host listed and the new group does not (in the reverse of the old list's
order), and the cancelling of the host's intents for packages the new group lists. If that commit
fails, the host's old record is written back, and the call changes nothing. The audit entry also
raises the host's facts generation (see the facts below), once it is committed, so a call that
fails raises nothing.

The host is then moving until its packages match its new group, at the host's heartbeats:

1. **Admission.** For every package of the new group that the host must install or change (the
   group has a desired version for it, the host can be ordered that version, and the host does not
   run it), the package's plan must have rolled every other host or have a free place in its
   current stage, and be neither paused, halted nor at a version marked bad. Until every plan
   admits it, the host is ordered nothing and is told `moving` for every package. A coordinator in
   its quiet start admits no host. When the host is admitted it takes a place in each of those
   plans, as a host that joins does. The admission is kept in memory: after a restart the
   coordinator makes it again.
2. **Facts.** The move raised the host's facts generation, and the next answer to the host's
   heartbeat over gRPC carries its facts, which name the new group. A host whose daemon announces
   `facts-v1` is ordered nothing, and is told `moving`, until its heartbeat reports that
   generation: the daemon has written the facts file by then, so a package installed after reads
   the new group. The coordinator decides this at each heartbeat, so a change of the host's facts
   during the move (`hkctl hosts set-labels`) holds the sequence again until the host reports
   the new generation. A host whose daemon does not announce `facts-v1`, or that heartbeats over
   the JSON route, is sent no facts and is not held for them; so is every host of a coordinator
   that does not advertise the feature.
3. **Uninstalls.** The host's uninstall intents run, one order at a time, under the halt, the
   hold and the one update at a time on a host; the admitted host is not asked the plan's gate
   (`rollout_elsewhere`, `no_slot`) for them. The installs and updates wait for them (`moving`).
4. **Installs and updates.** The packages of the new group are ordered in the order of its list,
   back to back. An admitted host is not asked the plan's gate (stage, pause, `no_slot`,
   `rollout_elsewhere`) for the package and version it was admitted to; every other rule holds. A
   package the host runs at another version is updated, a downgrade included.

The move ends when no uninstall to run is left and every package of the group is at its desired
version or cannot be ordered to the host: the record's old group is cleared. After that the host
is an ordinary host of its group: a package whose desired version is set later rolls to it under
the plan's gate.

- A pair held on the host, a package it is excluded from and a package whose desired version is
  below the host's minimum version are the host's own, and the move goes on around them.
- A move does not cut an update that is running on the host: it finishes and reports, and the
  sequence waits for it.
- A host the coordinator has not heard from is recorded as moving, and the sequence runs at its
  first heartbeat. A second `Move` while one is under way replaces the target: the sequence starts
  again from admission into the newest group, and the group it was in before becomes the one it
  moves from.
- The installs wait for the uninstalls (`moving`). An uninstall the host declined is ordered again
  after its backoff; one that failed is left for the operator, as
  [assignments.md](assignments.md#uninstall) says, and the installs wait for it.
- A package the host cannot be ordered (the release has no target or no digest for it, or the
  daemon cannot take an install) does not hold the move: it ends without that package. The
  move also ends, with the intents standing, for an uninstall intent of a package the host does not
  list.

A host's holds, exclusions and minimum version stay with it. A standing force of the old group no
longer covers it.

### The facts

A host's facts (its id, its group, its labels) are sent in the answer to its heartbeat over gRPC,
whole, until the host reports their generation. The generation is the number of the newest audit
entry that changed the facts of the host: `host_move`, `set_labels`, an approval or a replacing
approval, or, if none since the coordinator opened its audit log, the number of its last entry.
The group in the facts is the one the host's record holds, or, for a host in none, the role it
was approved for. Nothing is stored for the generation: it follows from the audit log, so a
restart resends every host its facts once and the daemon, which writes the file only on a change,
writes nothing.

## Installs and uninstalls follow the list

A host is offered the install of a package only when every package listed before it in the group
is installed on the host, at any version, or cannot be: the group has no desired version for it,
or the release has no target, no package for the host's platform or no build for the host
([holds-and-force.md](holds-and-force.md)). Until then the later package is told `install_order`
and takes no place in its plan, whichever stage each plan is in. So a config package listed after
its service is never ordered to a host that does not run the service. A package that is not
installed because a halt, a hold, an exclusion, a version marked bad, the quiet start or a gate of
its plan holds it also holds the later ones back. Updates are not ordered by the list.

The uninstall of a package waits, told `uninstall_order`, while a package whose intent was
written before it (which the group listed after it) is still on the host, whatever holds that
one; on a moving host, a pair an operator holds does not hold the others. An uninstall is paced
as an update: under the halt, the hold, the quiet start, the one update at a time on a host and
the group's other live rollouts, it takes a slot of the group's `max_updating`, and it is not a
stage of any plan.

The gate reasons the status shows for a host in a group, besides those of
[rollout-plans.md](rollout-plans.md):

| Hold | Gate reason | Meaning |
|---|---|---|
| `install_order` | `GATE_REASON_INSTALL_ORDER` | a package listed before this one is not installed on the host yet and can be |
| `uninstall_order` | `GATE_REASON_UNINSTALL_ORDER` | a package whose uninstall goes first is still on the host |
| `moving` | `GATE_REASON_MOVING` | the host is moving and its plans have not admitted it, or its uninstalls are not done |

## Deployments: a version is set in a group

A desired version is a (group, package, version), rolled over that group's hosts by that group's
policy. The group, its package list and its hosts decide what is deployed; CI and operators set
versions.

| Call | With `host_group` | Without it |
|---|---|---|
| `Releases.SetDesired` | One deployment to that group, one `set_desired` entry naming it. The group must list the package (`not_in_host_group`). | One deployment to each group that lists the package, in one change, one entry per group. A package no group lists is `not_in_host_group`; `if_generation` is taken only when one group lists the package (`ambiguous_host_group`). |
| `Releases.Converge`, `WaitConverged` | The group, which must list the package. | Every group that lists the package and has a desired version of it, converged when all are. |
| `Releases.GetRelease` | The group must list the package. | As before. |
| `Releases.Force`, `Rollouts.Pause`, `Resume`, `Advance`, `Releases.Halt`, `ClearHalt` | See [holds-and-force.md](holds-and-force.md) and [rollout-plans.md](rollout-plans.md). | |

The frozen routes `POST /v1/desired` and `GET /v1/convergence` name no group, so they read as
the column on the right. A set refused for any group writes nothing for any. A `SetDesired` whose
`desired` carries `max_updating` or `min_up` is refused `limits_in_host_group`: the limits are the
group's policy. A group with no hosts is converged. A package a group lists that has no desired
version in that group orders nothing, and the status shows the pair `no_desired`; when no group
has a desired version, `WaitConverged` answers `no_desired` at once.

`Releases.Status` answers one entry for each (group, package), with its `host_group`. A version
marked bad is bad in every group (`Releases.MarkBad` is the package's), so a build that fails in
a canary group is not rolled on to the next one.

Promotion from one group to the next is a sequence of deployments the caller makes: set the
canary group, `WaitConverged` on it, then set the next.

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    releases set-desired --service hello --host-group web-canary --request desired.json
```

with `desired.json` the request as [hello.md](../hello.md) builds it (`desired` with the
version, and the `targets` of the release).

## Policy

| Key | Default | Range, and where it is described |
|---|---|---|
| `stages` | `[1, 10, 50, 100]` | 1 to 16 stages whose percents rise strictly to 100, each `auto` or `manual` ([rollout-plans.md](rollout-plans.md#the-plan)) |
| `canary` | none | `count` 1 to 10000 |
| `bake_beats` | 0 | 0 to 100 |
| `rollback` | manual | `ROLLBACK_MODE_MANUAL` or `ROLLBACK_MODE_AUTOMATIC`; automatic sets the group's desired version back to the group's last good one |
| `rollback_after_failed` | 25 percent, 2 hosts | `percent` 1 to 100, `min_hosts` 1 to 10000 |
| `defer_backoff` | 300 s, factor 2, 3600 s | `initial_secs` at least 1, `factor` 1 to 16, `max_secs` from `initial_secs` to 604800 |
| `max_updating` | 1 | 0 to 10000; 0 holds the group |
| `min_up` | 0 | 0 to 10000 |
| `live_rollouts` | the coordinator's `--live-rollouts` | 1 to 16 |

A key left out takes its default. Each package of the group rolls by this one policy: its stages
are computed over the group's eligible hosts, `max_updating` caps the group's hosts updating that
package at once (uninstalls count), `min_up` counts the group's live hosts up for it, and
`live_rollouts` is how many of the group's packages may roll at once. `rollout_elsewhere` names
the group's other packages that hold the place. The plan, the canary, the bake, the failed-host
rules and `--live-rollouts` are otherwise as [rollout-plans.md](rollout-plans.md) says.

## Incidents are per host and per group

Hold, exclude and abort stay per host. A hold of a role names a group (a role that is no group is
refused `unknown_host_group`); a force, a halt, a pause and a minimum version of a role act on a
group, or on each group that lists the package when the call names none. Those are in
[holds-and-force.md](holds-and-force.md) and [rollout-plans.md](rollout-plans.md). `Services.List`
shows, per package, the groups that list it (`host_groups`), their hosts and minimum versions, and
leaves `roles` and `assignments` empty. `Alerts.List` derives a `halt` alert from each group's
halt (groups with the same halt of a package, host and version are one alert that names them
all), and the failed-canary, held-on-bad-version and abandoned-host alerts cover the packages
that only a group lists.

## The audit log

A change made in a group is an entry that names it (`AuditEntry.host_group`), and `Audit.List`
with `host_group` lists those entries. The actions `host_group_put`, `host_group_remove` and
`host_move` are new; `set_desired`, `force`, `halt`, `clear_halt`, `set_min_version`,
`rollout_advance`, `rollout_pause`, `rollout_resume` and the rollback entries name the group of
the plan they are about.

## Not built yet

- Approval by group: `Approve` and `AddRecord` name no group, the services an approval names are
  not refused, and the Mac allowlist names no group. A new host is in no group until `Move`.
- The CI allow-list's `host_groups`: a CI token may deploy to any group that lists its service.
- The migration of roles, the rollout file and assignments into groups, and the refusal of
  `--rollout-file`. Until then, putting the first group is the whole fleet changing from placement
  by roles to placement by groups: every host is in no group, and runs what it runs but is ordered
  nothing, until it is moved.

Each is a later build item of the design ([hostgroups.md](../design/hostgroups.md), section 12).
