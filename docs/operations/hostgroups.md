# Host groups: what each host runs

A host group is a name, the ordered list of the packages its hosts run, and one rollout policy.
Every enrolled host is in at most one group, and a host runs exactly the packages its group
lists, at the group's desired version of each. This page is what the coordinator does with groups.
The design is [hostgroups.md](../design/hostgroups.md). A fleet that was placed by roles and a
rollout file moves through the operator API, by [Moving a fleet from roles to host groups](#moving-a-fleet-from-roles-to-host-groups).
`hkctl` is [cli.md](../cli.md).

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
| `packages` | 0 to 64 packages in order (1 to 64 for `all`), none twice, each a service name (`A-Za-z0-9._-`, 1 to 255 bytes). `all` lists `hostkeeper`; a specific group need not. The order is the order of installs; uninstalls run in the reverse order. |
| `policy` | How every package of the group rolls out (see Policy below). |
| `generation` | 1 for a new group, and one more for each `Put` that replaces it. |

A group names no host: a host is in a group because its enrollment record says so, put there by
its approval ([Enrolling a host into a group](#enrolling-a-host-into-a-group)) and changed later
only by `HostGroups.Move`. A group lists packages, not versions: the versions are the group's desired
versions, set with `Releases.SetDesired` (Deployments below). A package can be listed by many
groups, and a host runs it at its own group's desired version. A package's minimum version in a
group is not part of the group: only `Services.SetMinVersion` writes it, and `Put` neither reads
nor resets it, so an incident's minimum is never undone by putting the group's file again.

## How the coordinator places hosts

`serve` always places by groups, whether or not the coordinator holds a group yet: it refuses
`--rollout-file` (`--rollout-file is not read by serve: start serve without it and place hosts with
the operator API: HostGroups.Put each group, HostGroups.Move each host, Services.SetRepository and
Services.SetMinVersion where needed, Releases.SetDesired per group (docs/operations/hostgroups.md)`)
and has no rollout file, role or assignment to place by. A coordinator holds the built-in group `all` from its first start
([below](#the-built-in-group-all)), so every host runs at least what `all` lists. The coordinator
decides at each heartbeat:

- A host runs the packages `all` lists, then those of its specific group, if it has one, that
  `all` does not list, and no others. `Services.Assign` and
  `Services.Unassign` are refused `placed_by_host_group`, before anything is read, and an approval
  that names services is refused `services_retired`.
- A host is put in its group when it is approved: by the group `Approve` names, the group of the
  inventory record that approves it, or the group of its Mac allowlist entry
  ([Enrolling a host into a group](#enrolling-a-host-into-a-group)). After that only `Move`
  changes it. An approval or a record that names a group the coordinator does not hold is refused
  `unknown_host_group`, a coordinator that holds none included.
- A host in no specific group runs the packages of `all` alone. A host is in none when it was
  approved naming no group (`Approve` with no `host_group` and no role), or before the coordinator
  held a group, or was moved back to none, until `Move` puts it in one; [moving a fleet from roles](#moving-a-fleet-from-roles-to-host-groups)
  puts every host of a role in the group of that name.
- The role the coordinator's fleet records for a host is the name of its group (empty for a host in
  none). `Hosts.List` and `Hosts.Get` show a host in a group with the group's name as `role` and as
  `host_group` (an approval by group writes the group's name as the host's role, too); a host in
  none shows the role it was approved for (none, for a host approved with no group) and an empty `host_group`. The slots of a package
  (`max_updating`), its `min_up` and its stages count the hosts of one group, so two groups never
  wait on each other; a hold of a role holds the group of that name.
- A package of `all` that no specific group lists is counted over every host, as one group named
  `all`: its slots, `min_up` and stages are `all`'s. A host whose own group lists the package is
  counted in that group instead, and not in `all`'s.
- A package's own settings are not the group's: its version scheme is the default, its minimum
  version is the one `Services.SetMinVersion` set for the group (see below), and its repository
  is the one `Services.SetRepository` set. Everything else that rolls it out is the group's
  policy.
- A package a host runs that its group does not list, and no uninstall intent covers (it was
  installed by hand, or by an earlier placement), is never ordered. `Hosts.Get` still shows it.
- A private repository is served to a host under the name of its group, the one its record holds,
  never a role from its certificate (a host certificate names the fixed role `host`, which means
  nothing, [enrollment-coordinator.md](../enrollment-coordinator.md)): the suite a host is sent
  ([repository-keys.md](repository-keys.md)) and the paths it may read are of that group. A host
  in no group reads none.

The groups and their desired versions are kept in the state file under the key `host_groups`, and
each (group, package)'s marks in the ops file under the same key (on the database store, the tables
of migration `0008`: [coordinator-state.md](coordinator-state.md)). A coordinator of the release
before refuses both files, and the database, once they hold a group.

### Where a package's repository and minimum version come from

A package belongs to no group, so its repository is not a field of a group: it is set for the
package, whichever groups list it, by `Services.SetRepository`. It is kept in the coordinator's
state (the ops file's `repository` key of the service; on the database, the column
`services.repository` of migration `0009`, which older releases may run on), and every change is
an audit entry, `set_repository`, naming the service, with the reason `repository <name>: <yours>`
(or `repository komira (the default): <yours>` when cleared).

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    services set-repository --service hello-config-web --repository acme-private \
    --reason "the web config comes from the private repository"
```

- A package whose repository was never set comes from `komira`, the default. An empty
  `--repository` clears the setting and puts the package back on it.
- The service must be a package some group lists (`unknown_service`), the name must be a
  repository name (`invalid_request`) and the registry must hold it (`repository_unknown`). A call
  that sets what is already set still writes its entry. The call takes the operator token only, like
  every call of this page.
- `Services.List` shows a package's repository as set (or `komira`). A release's `SetDesired` is
  accepted only when its repository entries name channels of that repository
  (`repository_unknown` otherwise), so set a config package's repository before its first release.
- `Repositories.Remove` of a repository is refused (`in_use`) while a package names it, and
  `komira` itself while any group lists a package on the default; clear or move those packages
  first, then remove it ([package-repositories.md](../deploy/package-repositories.md)).

A package's minimum version in a group is the one `Services.SetMinVersion` set for it (with the
group's name as the `role` of the minimum): nothing else floors a package, since there is no
rollout file. A host's own minimum stands before it.

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

**Remove.** A group that a host is in or moving into, that an inventory record names (by its
group, or by its role for a record added before the coordinator held groups), or that an entry of
the Mac allowlist names, is `not_empty`; an approved host is in its group from its approval, before
it first polls. The check runs under the lock an approval and `AddRecord` check their group
under, so neither lands in a group being removed. Once the last group is removed the coordinator holds
none, and orders no host anything. The entry is `host_group_remove`.

**Move.** The entry is `host_move`, about the host (`node_id`) and the new group (`host_group`),
with the reason `from <old> to <new>: <yours>`, or `into <new>: <yours>` for a host that was in
none; a move out of a group into none names no group and has the reason `from <old> to no group:
<yours>`. The answer names the group the host was in. `unknown_host` is a host that is not enrolled
or is denied, and the checks run in the order of the table.

## The built-in group `all`

Every enrolled host is in `all`, and may be in one specific group besides: there is no stored
membership of `all`, a host is in it by being enrolled. `all` is how an operator targets every host:
a package it lists runs on every host, and a call that names a group (`Releases.SetDesired`,
`Releases.Force`, the rollout calls, `Services.SetMinVersion`, alerts and holds by group) may name
`all` and then reaches every host that runs the package as `all` says. The console shows it,
marked built in, on the Host groups page, for editing its packages, and offers it nowhere else.

- It always exists. A coordinator that holds none makes it at its first start (the database's
  migration `0013` makes it; a state file that lacks it gets it in memory, and its `host_groups`
  key is left out while `all` is as made, so the release before reads the file). It cannot be
  removed or renamed, and no group of any other path may take its name: `Remove` of `all`, `Put` that
  would create it, and `Move` into it are refused `reserved_host_group`; an approval or an
  inventory record that names it is refused `unknown_host_group`. A `Put` that replaces it is
  taken, over its generation like any other.
- The made one lists `hostkeeper` alone, so a coordinator that held groups before gets no
  package it did not run: hosts that were in a group keep it, and now also run what `all` lists
  (`hostkeeper`, which they already had). An operator adds the rest with `Put`.
- `all` must list `hostkeeper` (`hostkeeper_not_listed`), so that every host keeps its daemon
  whatever its own group lists. A specific group may list nothing, or leave `hostkeeper` out.
- What a host runs is `all`'s packages in `all`'s order, then the packages of its own group that
  `all` does not list, in that group's order. A package both list is run once, at the place `all`
  lists it, and as the host's own group says: its desired version, its rollout policy, its
  marks and its halts are the group's. A package only `all` lists is run as `all` says: the
  desired version set for `all`, its policy, its marks. A host whose own group lists a package
  that `all` has a desired version for and the group has none is not ordered it.
- A change of a list that stops a host running a package uninstalls it; one that leaves the
  package run by the other list does not. `Put` of `all` that drops a package uninstalls it from
  every host whose own group does not list it, and `Move` keeps the packages of `all`.
- `HostGroups.List` counts every enrolled host in `all`, and `Get` of `all` pages them all.
- A host approved with no group runs the packages of `all` alone until it is moved
  ([Enrolling a host into a group](#enrolling-a-host-into-a-group)).

## Enrolling a host into a group

In group mode an approval puts its host in the group it names, which must be one the coordinator
holds (`unknown_host_group`) and is not `all`; an operator's approval may name none, and the host is
then in `all` alone until `Move` ([enrollment.md](enrollment.md)):

- `hkctl enrollment add-record --host-group <group>`: the record is the approval, and the
  coordinator puts the host it approves in the record's group. A record added before the
  coordinator held groups names its group by its role; one whose role no group has approves
  nothing, and its request waits for an operator.
- `hkctl enrollment approve --host-group <group>`: the operator's approval. It names no
  `--services` (`services_retired`), and the group is optional (so is `--reason`).
- A Mac approved by its MDM nonce goes into the `host_group` of its entry of
  `--mac-serial-allowlist` ([enrollment-coordinator.md](../enrollment-coordinator.md#a-mac-approved-by-its-nonce)).

`--role` is read as the group's name where `--host-group` is left out, so scripts that name roles
keep working once the groups have the roles' names. The approval's audit entry names the group,
and the host's facts generation is raised by it. A renewal keeps the host's group, and an approval
with `--replace` keeps the group of the host it replaces: naming another group is
`host_group_differs`.

## Moving a host

A host in a group is changed to another by `Move`, and a host approved in none, or before the
coordinator held groups, is put in one by `Move`. A host in a group is moved back to none by a
`Move` with an empty `host_group` (`hkctl host-groups move --host-group ""`, or **Remove from
group** beside the host in a group's detail on the console's Host groups page): it then runs the
packages of `all` alone, what its old group listed and `all` does not is uninstalled, and nothing
is installed, so the host does not move; a host already in none is refused `same_host_group`. A
host cannot be moved into `all` (`reserved_host_group`), which it is in already.

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups move --node-id web-17 --host-group web --reason "racked for the web tier"
```

Out of its group, into none:

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups move --node-id web-17 --host-group "" --reason "no longer a web server"
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
  after its backoff; one that failed is left for the operator
  ([Uninstalls](#uninstalls)), and the installs wait for it.
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
The group in the facts is the one the host's record holds (an approval by group writes it), or,
for a host in none, the role it was approved for. Nothing is stored for the generation: it follows from the audit log, so a
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

## Uninstalls

An uninstall is ordered to a host in two ways only: a `Put` that takes a package out of a group's
list, and a `Move` to a group that does not list a package the old one did. Each writes one
uninstall intent for every host concerned that listed the package at its last heartbeat to this
coordinator (above). No other call orders one: excluding a host, retiring it, an enrollment
approval, a hold, a minimum version or a new desired version never does. A package that is not
listed and has no intent stays installed on the host, unmanaged: it is never ordered there again.

The uninstall is decided under the halt, the hold, the quiet start, one update at a time on the
host and the group's other live rollouts (`rollout_elsewhere`, `--live-rollouts`), and takes an
update slot of the group's `max_updating` as an update does. It is not a stage of any plan. It
stands across a coordinator restart, in the ops file (on the database store, the table
`uninstall_intents`), and ends when the host reports the package gone from the services it lists
(audited `uninstalled`, by the coordinator), or when the package is listed for the host again
(`uninstall_cancelled`). An uninstall the host declined is ordered again after its backoff; one
that failed is left for the operator, and like a failed update it holds the slot until the host
heartbeats healthy (or an operator excludes it, which frees it). An excluded host is ordered
nothing at all.

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

An entry of the CI allow-list may name `host_groups` ([ci-client.md](../deploy/ci-client.md)), a
list of host group names. Without the key, a token of the entry deploys its services to any group
that lists them. With it, the token deploys to those groups alone, and `host_groups = []` names
none. A set that names a group outside the list is refused `forbidden` (gRPC `PermissionDenied`,
HTTP 403), before the group is looked up, so it does not say whether the group exists. A set that
names no group is one deployment to each of the token's groups that lists the package, one entry
each; it is `not_in_host_group` when no group lists the package and `forbidden` when some do but
none of them is the token's, and `if_generation` is `ambiguous_host_group` when more than one of
the token's groups lists it. A coordinator that holds no host group refuses such a token every
set with `forbidden`. The list limits deployments only: `Converge`, `WaitConverged` and
`GetRelease` answer a token of any admitted entry for any group.

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

## Moving a fleet from roles to host groups

A coordinator that placed hosts by roles, a rollout file and assignments moves to groups through
the operator API, with the hosts running throughout and no service touched. `serve` does not read
the rollout file any more and refuses it, and a coordinator that holds no group orders every host
nothing (see [How the coordinator places hosts](#how-the-coordinator-places-hosts)), so the order
below is safe at every step.

1. **Start this release without the rollout file.** Stop the coordinator and take the backup
   ([coordinator-state.md](coordinator-state.md#backing-up) for the files, `pg_dump` for the
   database). Drop `--rollout-file` (or `rollout-file` of the configuration file). Also leave out
   `--mac-serial-allowlist` for now: a start refuses an entry that names `services`, and one that
   names a group the store does not hold, which at this point is every group. Start the release.
   Every host keeps running what it runs and is ordered nothing.
2. **Put each group.** `hkctl host-groups put` with the group's packages in install order and its
   policy ([cli.md](../cli.md#hkctl-host-groups-put), [The calls](#the-calls)). Name a group after
   the role whose hosts it takes.
3. **Move each enrolled host into its group.** `hkctl host-groups move` for each host, the hosts
   of a role into the group of that name ([cli.md](../cli.md#hkctl-host-groups-move),
   [Moving a host](#moving-a-host)).
4. **Set what the rollout file held per package.** `hkctl services set-repository` for each
   package whose repository is not `komira` (put the registry's entries in place first,
   [package-repositories.md](../deploy/package-repositories.md)), and `hkctl services
   set-min-version` for each minimum a group needs
   ([Where a package's repository and minimum version come from](#where-a-packages-repository-and-minimum-version-come-from)).
5. **Set the desired versions.** `hkctl releases set-desired --host-group <group>` for each
   package of each group ([Deployments](#deployments-a-version-is-set-in-a-group)), and point CI at
   the groups with the `host_group` of its release step
   ([github-actions.md](../deploy/github-actions.md)).
6. **Bring the Mac allowlist back.** Once the groups exist, give each entry of
   `--mac-serial-allowlist` its `host_group` and remove its `services`, and start the coordinator
   with the file again.

A host is ordered nothing until both its group exists and has a desired version, so no service is
touched while the move runs; a host's packages change only when step 5 gives its group a version
that differs from what it runs. Scripts that pass `--role` to `hkctl enrollment approve` or
`enrollment add-record`, or name a `role` in a `releases force` request, keep working: it is read as
the group's name. Scripts that call `services assign` or `unassign`, or approve naming services, are
refused (`placed_by_host_group`, `services_retired`) and must change.

To go back, restore the backup and the old configuration with the release before: the store now
holds `host_groups`, which that release refuses. A coordinator holding groups has no way back to
roles but the backup.
