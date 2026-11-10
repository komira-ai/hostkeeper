# Assignments, installs and uninstalls

Which services a host runs is the coordinator's to say: a host runs the services assigned to it,
and nothing else. Assigning a service to a role or a host is a coordinator change; nothing is
touched on the hosts. Every call below takes the operator token only: a CI token is refused with
`forbidden`, no token with `unauthorized`, and a refused call changes nothing. Every change is
an entry of the audit log, under the name of the token that made it.

This page is the placement of a coordinator that holds no host group. Once it holds one, the groups
place every package ([hostgroups.md](hostgroups.md)): `Assign` and `Unassign` are refused
`placed_by_host_group`, the rollout file's `roles` and `selector` and the entries below place
nothing, and a host runs the packages of its group. The uninstall intents of this page are the
ones a group's `Put` and `Move` write, and they are carried out as the Uninstall section says;
`SetMinVersion` of a role sets the minimum version of the group of that name.

## What a host is assigned

The rollout file is the base: a service is assigned to the hosts of the roles its `roles` list
(or, with a `[service.selector]`, to the hosts the selector selects). The operator's entries go
on top, and the first that applies decides:

1. an entry of the host itself (`Services.Assign` or `Services.Unassign` with `node_id`);
2. an entry of the host's role (the same calls with `role`), matched against the host's role
   each time the coordinator decides, so a host whose role changes follows the new one;
3. the rollout file.

`Assign` of a host writes the host's own entry, so the host keeps the service when its role is
later taken off it. `Unassign` of a scope the service is not assigned to is refused with
`not_assigned`.

An operator's enrollment approval (`Enrollment.Approve`) assigns the services it names to the
host it approves, by the same path as `Assign` of the host, with one `assign` entry per service
after the approval's own. If that write fails, the approval stands, the coordinator logs it, and
the operator assigns the services with `Services.Assign`. A Mac approved by its MDM nonce is not
assigned its allowlist entry's services this way: assign them with `Services.Assign`.

## Install

At the next heartbeat of a host that is assigned a service and does not run it (it does not
list it, or lists it `absent`: staged and never run), the coordinator orders `install` of the
service's desired version, with the package digest for the host's platform: the one CI set, or
the one the version's release index lists for that platform. The install is decided under
every rule of an update, in the same order: the halt, the exclusion, the hold, a version marked
bad, the minimum version, one update at a time on the host, a failure of this version on this
host, a deferral and the service's update plan ([rollout-plans.md](rollout-plans.md): a pause, the
other services' live rollouts, the stage the host is in, the role's `max_updating` and `min_up`).
An install takes an update slot and a place in a stage, and frees them as an update does.

An install goes only to a daemon that announces the `install-v1` feature and lists every
service it runs (`heartbeat-v2`), and only as a typed order: the JSON heartbeat reply carries
none. The rollout status says why a host is not told: `unsupported` (its daemon cannot take an
install, or, for hostkeeper itself, its daemon installed the desired release without switching
to it and waits for the next: [hostkeeper's self-update after a bootstrap](self-update-after-bootstrap.md)), `no_digest` (CI set the desired version without a digest or a release index) or
`no_package_for_platform` (the release index lists no package for the host's platform: the host
is ordered nothing for this version, and that is not a failed install). For a version set with
targets, the install names the entry picked for the host, and a host picked none is `no_target`
or `no_build_for_host` ([holds-and-force.md](holds-and-force.md)).

## Uninstall

`Unassign` with `uninstall` orders each host that stops having any assignment of the service,
and listed it at its last heartbeat, to uninstall it. Without `uninstall`, the service stays
installed there, unmanaged: it is never ordered there again. No other call orders an uninstall:
excluding a host, retiring it, an enrollment approval, a hold, a minimum version or a new
desired version never does.

The uninstall is decided under the halt, the hold, the quiet start, one update at a time on the
host and the other services' live rollouts (`rollout_elsewhere`, `--live-rollouts`), and takes an
update slot as an update does. It is not a stage of the service's update plan. It stands across a
coordinator restart, in the ops file (on the database store, the table `uninstall_intents`), and ends when the host reports the service gone from the
services it lists (audited as `uninstalled`, by the coordinator), or when the service is
assigned to the host again (`uninstall_cancelled`). An uninstall the host declined is ordered
again after its backoff; one that failed is left for the operator, and like a failed update it
holds the slot until the host heartbeats healthy (or an operator excludes it, which frees it).
An excluded host is ordered nothing at all.

## Status and convergence

`Releases.Status`, `Releases.Converge` (and `GET /v1/convergence`) and the Rollout page count
the hosts a service is assigned to, by the same rule the rollout gate orders by: the rollout
file's roles and the operator's entries alike. A role an operator assigned the service to is
counted after the rollout file's roles, by name, once one of its hosts reports the service. A host
that is assigned the service and does not list it is `missing`, and the service is not
`converged`, while the host's daemon could be ordered the install (it lists every service it runs
and takes installs, and the release index lists a package for its platform). A host or role an
`Unassign` takes the service off is no longer counted, whatever it still runs.

## Minimum versions

`Services.SetMinVersion` sets, for a role or a host, the version below which the rollout gate
orders the service there no update, install or automatic rollback. A host's own minimum stands
before its role's, and neither goes below the rollout file's `min_version`. A minimum above the
service's desired version is refused with `below_min_version`; an empty version removes the one
an operator set.

## Listing

`Services.List` shows, per service: its desired version, the roles it is assigned to, the
operator's entries and minimum versions, each host heard from since the coordinator started
that it is assigned to (with the version it reports installed, the minimum that applies to it
and the holds that hold it), the hosts ordered to uninstall it, and its holds.

## The ops file

The ops file gains an `assignments` key once an entry stands. A coordinator of an earlier
release refuses to start on such a file rather than ignore the assignments; a file that never
held one keeps the shape earlier releases read.

On the database store ([coordinator-state.md](coordinator-state.md#the-database-store)) the
entries are rows of the tables `assignments` and `min_versions`, and the uninstall intents rows of
`uninstall_intents`, each change with its audit row in one transaction.
