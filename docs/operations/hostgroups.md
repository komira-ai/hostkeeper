# Host groups: what each host runs

A host group is a name, the ordered list of the packages its hosts run, and one rollout policy.
Every enrolled host is in at most one group, and a host runs exactly the packages its group
lists, at the group's desired version of each. This page is what the coordinator does with groups.
The design is [hostgroups.md](../design/hostgroups.md). A fleet that was placed by roles and a
rollout file is converted once, offline, by [Migrating from roles](#migrating-from-roles).
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
| `packages` | 1 to 64 packages in order, none twice, each a service name (`A-Za-z0-9._-`, 1 to 255 bytes). `hostkeeper` is one of them. The order is the order of installs; uninstalls run in the reverse order. |
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
`--rollout-file` (`--rollout-file is not read by serve: convert it once with hostkeeper-coordinator
host-groups migrate (docs/operations/hostgroups.md)`) and has no rollout file, role or assignment
to place by. A coordinator that holds no group orders every host nothing. The coordinator decides
at each heartbeat:

- A host runs the packages its group lists and no others. `Services.Assign` and
  `Services.Unassign` are refused `placed_by_host_group`, before anything is read, and an approval
  that names services is refused `services_retired`.
- A host is put in its group when it is approved: by the group `Approve` names, the group of the
  inventory record that approves it, or the group of its Mac allowlist entry
  ([Enrolling a host into a group](#enrolling-a-host-into-a-group)). After that only `Move`
  changes it. An approval or a record that names a group the coordinator does not hold is refused
  `unknown_host_group`, a coordinator that holds none included.
- A host in no group is ordered nothing: no install, no update, no uninstall. It keeps running
  what it runs. A host is in none when it was approved before the coordinator held a group, until
  `Move` puts it in one; [the migration](#migrating-from-roles) puts every host of a role in a
  group.
- The role the coordinator's fleet records for a host is the name of its group (empty for a host in
  none). `Hosts.List` and `Hosts.Get` show a host in a group with the group's name as `role` and as
  `host_group` (an approval by group writes the group's name as the host's role, too); a host in
  none keeps the role it was approved for and an empty `host_group`. The slots of a package
  (`max_updating`), its `min_up` and its stages count the hosts of one group, so two groups never
  wait on each other; a hold of a role holds the group of that name.
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
none. The answer names the group the host was in. `unknown_host` is a host that is not enrolled
or is denied, and the checks run in the order of the table.

## Enrolling a host into a group

In group mode every approval puts its host in a group, which must be one the coordinator holds
(`unknown_host_group`) ([enrollment.md](enrollment.md)):

- `hkctl enrollment add-record --host-group <group>`: the record is the approval, and the
  coordinator puts the host it approves in the record's group. A record added before the
  coordinator held groups names its group by its role; one whose role no group has approves
  nothing, and its request waits for an operator.
- `hkctl enrollment approve --host-group <group>`: the operator's approval. It names no
  `--services` (`services_retired`).
- A Mac approved by its MDM nonce goes into the `host_group` of its entry of
  `--mac-serial-allowlist` ([enrollment-coordinator.md](../enrollment-coordinator.md#a-mac-approved-by-its-nonce)).

`--role` is read as the group's name where `--host-group` is left out, so scripts that name roles
keep working once the groups have the roles' names. The approval's audit entry names the group,
and the host's facts generation is raised by it. A renewal keeps the host's group, and an approval
with `--replace` keeps the group of the host it replaces: naming another group is
`host_group_differs`.

## Moving a host

A host in a group is changed to another by `Move`, and a host approved before the coordinator held
groups is put in one by `Move`.

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

## Migrating from roles

A coordinator that placed hosts by roles, a rollout file and assignments is converted once, by an
offline verb over its store. `serve` does not read the rollout file any more and refuses it, so
the conversion is the window in which a fleet changes from one to the other, with the hosts running
throughout and no service touched.

```
hostkeeper-coordinator host-groups migrate --rollout-file <file> \
    ( --state-file <file> [--ops-file <file>] --enrollment-dir <dir> | --database-dsn-file <file> ) \
    [--legacy-service <name>] [--live-rollouts <n>] [--mac-serial-allowlist <file>] [--apply]
```

- `--rollout-file` (required) is the file `serve` was given, read once with the reader `serve` used
  (a file it refused is refused here, `--rollout-file: ...`).
- The store is given by the flags `serve` names it by: exactly one of `--state-file` (with
  `--ops-file`, default the state file's name with `.ops` added, and `--enrollment-dir`) or
  `--database-dsn-file`; a file flag beside the database is refused, naming it. The enrollment store
  is required: without `--enrollment-dir` the verb is refused, since without the enrolled hosts
  there is no host to put in a group.
- `--legacy-service` reads a desired-state file in the single-state form, as `serve` does.
  `--live-rollouts` (1 to 16, default 1) is `serve`'s, and is used only for the note the plan
  prints. `--mac-serial-allowlist` is read so that the plan can say what each entry needs.
- **Without `--apply`** (the dry run) it reads the store and the rollout file and writes nothing: no
  file is made or changed, no lock taken, no migration run. It prints the plan and exits 0, or, when
  the plan holds an error, prints it to standard error and exits 1.
- A store that holds a host group already is refused (`the store already holds a host group: it is
  migrated already`), with `--apply` and without. A store that holds `<state-file>.migrate-unfinished`
  or the marker of a restore is refused as `serve` refuses it.

### What the plan is

For each enrolled host that is not denied, the set of packages placed on it today, decided as
the coordinator decided it (the rollout file's roles and selector, then the entries of the role and
of the host), plus `hostkeeper`. A host whose role is empty or is not a group name is an error
naming it. Then:

- **Groups.** The hosts of one role with the same set form one group named after the role. When the
  hosts of a role run different sets, the most common set takes the role's name (a tie goes to the
  set whose first host, by node id, sorts first) and each other set is `<role>-2`, `<role>-3`, in
  the order of the node id of its first host. A derived name that is over 32 bytes, or that is
  already a role's name (of a host, a record, an approval, the rollout file or the Mac allowlist),
  is an error naming the role. A role that a record, a pending approval or an allowlist entry names
  and no host has gets a group too, with the set the file places on a host of that role with no
  labels, and a role nothing names gets none.
- **Packages.** A group lists its set by the rollout file's `priority` (higher first), then by
  name; `hostkeeper` has priority 0. More than 64 packages is an error naming the group.
- **Policy.** The keys the rollout file's services of the group agree on become the group's
  policy. Where they differ, the most careful value of each key, and the plan prints each choice
  (`bake_beats 5: the highest of kbf 5, tierstore 0`): the `stages` and `canary` of the service
  whose first stage holds the fewest of the group's hosts (a tie goes to the first by name), the
  highest `bake_beats`, `manual` over `automatic` for `rollback`, the lowest
  `rollback_after_failed` percent and `min_hosts`, the longest `defer_backoff` of each field, and,
  from the per-role limits of the desired state over the group's packages that have one, the
  lowest `max_updating` and the highest `min_up`. `live_rollouts` is left unset, so it follows
  `serve --live-rollouts`; a group may now roll that many packages at once where the whole fleet
  did, and the plan says so. A policy that fails its checks is an error naming the group and key.
- **Desired versions and marks.** Each group's desired version of each package that has one (its
  generation kept on the database, 1 on the files), the package's `last_good`, halt, halt
  acknowledgements and force marks (a force of a role is rewritten to the group, one of hosts is
  cut to the group's hosts, and one that covers none of them is dropped), and the plan's position
  (stages entered, pause), with its done hosts split to the group each is in. The release registry
  and the versions marked bad stay per package. The package's own fields, the assignments, the
  plans and the per-role state are left as they are, unread afterwards.
- **Minimum versions.** What `Services.SetMinVersion` of a role writes: for each group that lists a
  package, the higher of the role's minimum and the rollout file's `min_version`. Host minimums,
  exclusions and holds of a host, and uninstall intents, stay as they are. A hold of a role is kept
  and copied to each other group of that role.
- **Repositories.** What `Services.SetRepository` writes: each service of the rollout file whose
  `repository` is not `komira`. A repository the registry does not hold is an error naming the
  service. Put the registry's entries in place first ([package-repositories.md](../deploy/package-repositories.md)).
- **Records.** Each enrolled host's `host_group`, each approved request not yet polled, and each
  inventory record of a role with no group, get the group of their role. A Mac allowlist entry
  cannot be rewritten by the verb: the plan prints the `host_group = "<role>"` it needs and the
  `services` that go, and an entry whose services are not all in its group's packages is an error
  naming its serial.
- **Audit.** One `host_group_put` entry per group (by `migration`, reason `migrated from role
  <role>: <n> hosts`), each followed by that group's `set_min_version` entries, then the
  `set_repository` entries (reason `repository <name>: from the rollout file`), numbered after the
  store's newest line.
- **A check.** For every host and package, the placement after the migration (its group's list)
  must equal the set it has today; a difference is an error naming host and package. The groups
  are built so there is none.

The report is the same with and without `--apply`: a header line (`hostkeeper-coordinator
host-groups migrate: the file store of <state-file> (dry run)`), per group `group <name> (from role
<role>): <n> hosts; packages a, b, c` with its non-default policy, merge choices, desired versions,
marks and minimums indented under it, the `hosts:` by group, the `repositories:`, the `records:`
(hosts, approvals, inventory records and allowlist entries), the `live_rollouts:` note, then
`no errors` or `errors:` with each one. The last line is `nothing written; --apply writes it in one
commit` for a dry run. With any error the report ends `nothing written; --apply refuses while there
is an error`, and `--apply` writes nothing and exits 1.

### What `--apply` writes

`--apply` writes the plan in one commit and prints it, ending `written in one commit (files); audit
entries <first>..<last>` or `written in one transaction (database); audit entries <first>..<last>`.

- **Database.** In one transaction: the changed rows of the desired state and the ops, the changed
  enrollment records, the audit rows. It takes the database's lock first, so a coordinator serving
  on it refuses the verb, and runs the migrations the database lacks. It reads everything back and compares it with the plan, and a difference
  rolls the transaction back. It does not raise the lease's epoch.
- **Files.** Under the lock of the state files, which a running coordinator holds (so it refuses
  with `a coordinator holds the lock of these state files`). It first writes the marker
  `<state-file>.migrate-unfinished`, then the changed enrollment records, the audit lines and the
  ops file, and the state file last: the state file is what introduces `host_groups`, and is the
  commit point. It removes the marker at the end.
- **The marker.** A write that fails (or a stop partway) leaves the marker and returns the error.
  `serve`, `backup`, `restore`, `import`, `export` and `migrate` then refuse the files:
  `<state-file>.migrate-unfinished: a host-groups migrate --apply stopped partway; restore the
  backup taken before it and run it again`. Restore the backup of the window, then run it again.

### The window

1. **Before the window**, on a copy of the store (`hostkeeper-coordinator export --into-empty` of
   a database into new files, or the backup of the files restored into new paths, both in
   [coordinator-state.md](coordinator-state.md)), or on the store itself (a dry run writes nothing
   and takes no lock), run the verb without `--apply`. On a database the dry run runs no
   migration: one whose schema is older than this release is refused by the read, and one that is
   newer is refused, so rehearse on a restored copy that a `serve` of this release has opened. Read the groups and policy choices, and fix
   every error: name or set the roles right, put the repositories in the registry, trim a
   package list.
2. **In the window.** Stop the coordinator. Take the backup
   ([coordinator-state.md](coordinator-state.md#backing-up) for the files, `pg_dump` for the
   database). Run the verb with `--apply`. Edit the coordinator's configuration: drop
   `--rollout-file` (or `rollout-file` of the configuration file), and in the Mac serial
   allowlist add the `host_group` and remove the `services` of each entry the plan listed (a start
   refuses an entry that names `services`, or a group the store does not hold). Start the new
   release.
3. **After the window.** Point CI at the groups (`host_group` of its release step,
   [github-actions.md](../deploy/github-actions.md)), put the config packages into their groups
   with `hkctl host-groups put`, and stop naming roles in scripts: `--role` is read as the group's
   name.

To go back, restore the backup and the old configuration with the release before: the migrated
files hold `host_groups`, which that release refuses. A coordinator holding groups has no way back
to roles but the backup.
