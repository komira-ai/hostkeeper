# Holds, marks and force

What an operator can do to stop or steer a rollout, and how the coordinator acts on it. The
stages of a rollout, and `Rollouts.Advance`, `Pause` and `Resume`, are in
[rollout-plans.md](rollout-plans.md). Every call below takes the operator token only: a CI token is refused with `forbidden`, no token with
`unauthorized`, and a refused call changes nothing. Every change is an entry of the audit log,
under the name of the token that made it; `Audit.List` reads the log back.

Where the coordinator holds host groups ([hostgroups.md](hostgroups.md)), a service is rolled out
once for each group that lists it, and the calls below act on a group or on each group that lists
the service. Each section says what it does there; what it says without a mention holds as it is.

## Holds

`Services.Hold` holds a service on one host, on every host of a role, or on the whole fleet,
with a reason; `Services.Unhold` releases it. A hold freezes the service where it is:

- The coordinator orders a held pair (a host and a service) nothing: no update, no install, no
  rollback, and no force unless the force sets `include_held`. The rollout status shows the
  pair as `held`.
- A hold carries no version. The host keeps running what it runs, and never learns of the hold.
- A role hold is matched against the host's role each time the coordinator decides, so a host
  that joins the role later is held, and one that leaves it is released. Where the coordinator
  holds host groups, a host's role is the name of its group: a hold of the role `web` holds the
  hosts of the group `web` (and of no other group), and a role that names no group is refused
  `unknown_host_group`. A hold of a host, or of the whole fleet, is as before.
- A held pair takes no update slot, is in no stage of the service's update plan (it takes no
  place in a stage and does not count towards one being done), never holds back a bake, and is
  left out of the convergence and of the hosts an automatic rollback is counted against, so it
  never blocks the others. An update the host was already running, or had been ordered, when the
  hold was set finishes, and holds the slot until it ends (`Hosts.Abort` stops it).
- Automatic rollback and a version marked bad do not reach a held pair. A held host that runs a
  version marked bad is an alert: `Alerts.List` lists it, an error-level log line with
  `alert=held_on_bad_version` is logged once when it appears, and the status flags the host.
- Holds are kept in the ops file (on the database store, the table `holds`) and survive a restart.
  `Services.List` shows them.
- Holding the same service in the same scope again replaces the reason. `Unhold` releases the
  pairs to the desired version at their next heartbeat, and they become eligible for the
  plan's stages again.

The ops file gains a `holds` key once a hold is set. A coordinator of an earlier release refuses
to start on such a file rather than ignore the holds; a file that never held one keeps the shape
earlier releases read.

In a host group a host is also told, as the status shows it, `install_order` (a package listed
before this one is not installed on the host yet), `uninstall_order` (a package whose uninstall
goes first is still on the host) or `moving` (the host was moved into its group and its plans have
not admitted it, or its uninstalls are not done); none of them is a failure, and
[hostgroups.md](hostgroups.md) says when each applies.

## A platform the release has no package for

A version set with a release index is ordered to each host as the package its platform has in the
index. A host whose platform the index does not list is ordered nothing for that version (no
update, no install, no automatic rollback, and no force: a force lists it as skipped with the
reason `no_package_for_platform`). The rollout status shows the host's hold as
`no_package_for_platform`, and the typed gate reason is `GATE_REASON_NO_PACKAGE_FOR_PLATFORM`.
This is not a failed update: the host is never counted as failed, and it never halts the service
or triggers an automatic rollback. It keeps what it runs, and is ordered again when a version
whose index lists its platform is set. A version set with one package digest is ordered to every
host, as before.

## A release with targets: the entry each host is picked

A version set with targets (`SetDesired` with `targets`, [packages.md](../design/packages.md)
section 3) is ordered to each host as one entry of its platform's list: the entry of the
installer the host runs the service with, wherever it stands in the list, else the first entry
whose installer the host's daemon lists. The daemon lists its installers in its heartbeats over
gRPC (`Hosts.Get` shows them as `installers`, and each service's in its heartbeat); a daemon that
does not announce `target-v1` is ordered nothing (`no_build_for_host`): no pick returns a
`hostkeeper-archive` entry. A host the release picks nothing for is ordered nothing for that
version, keeps what it runs, and is never counted as failed or missing:

- `no_target` (`GATE_REASON_NO_TARGET`): the release has no list for the host's platform;
- `no_build_for_host` (`GATE_REASON_NO_BUILD_FOR_HOST`): the host does not run the service and no
  entry is of an installer its daemon lists;
- `installer_dropped` (`GATE_REASON_INSTALLER_DROPPED`): the host runs the service with an
  installer the list has no entry of, or that its daemon no longer lists. A host changes a
  service's installer only by an uninstall and an install, or by the move to native packages
  (docs/design/packages-native.md section 10.6): a host running it from `hostkeeper-archive`,
  whose daemon still lists `hostkeeper-archive`, is picked the first native entry of the list its
  daemon lists. No pick returns a `hostkeeper-archive` entry: a host running it natively, going
  back to a version whose list has no native entry it lists, is `installer_dropped`.

None of them holds the convergence, but `Converge` counts the hosts picked an entry and those
picked none by why (`HostCounts.picked`, `no_target`, `no_build_for_host`,
`installer_dropped`), names the `installer_dropped` hosts (`Convergence.installer_dropped`), and
is not converged while no host was picked an entry (a misspelt installer, say). A host that does
not report the service shows its `no_target` or `no_build_for_host` in `Hosts.Get`. A daemon
announcing `target-v1` whose installers the coordinator has not read yet (its first heartbeat
after its start, or any after the coordinator's start, comes over JSON) is ordered nothing until
its next heartbeat over gRPC, and is held `not_a_member` meanwhile. Only an archive entry goes on
the JSON heartbeat route; any other waits for the next heartbeat over gRPC.

## Marks

- `Releases.MarkBad` marks a version of a service bad, any version the service's version scheme
  reads. No host is ordered it again, and CI cannot set it, until `UnmarkBad` or a force lifts
  the mark. A version marked bad halts the plan (every host is told `bad`). A second mark of the same version is refused with `already_bad`.
  The mark is the package's: in host groups a version marked bad is bad in every group, so a
  build that fails in one group is not rolled on to the next.
- `Releases.Halt` halts a service by hand, as a host's halt report does: no host of it is
  ordered until `ClearHalt` (or a force). It is an alert (`Alerts.List`, and an `alert=halt`
  line logged once when it appears). A service with
  no desired version has nothing to halt and is refused with `invalid_request`; a service that
  is halted already, with `already_halted`. In host groups a halt is a group's: the call names no
  group, and halts the service in every group that lists it and has a desired version of it, each
  at its own desired version, with one `halt` entry for each group that names it. A group that is
  halted already is left as it is, and `already_halted` is the answer only when every group was.
  `Releases.ClearHalt` clears the halt of every group that has one, one `clear_halt` entry each.
  The alert's message names the group; groups whose halt has the same service, host and version are
  one alert that names them all.
- `Hosts.Abort` queues an `abort` of a service for the host's next heartbeat. It takes no slot,
  is sent once, and only to a daemon that announced the `abort-v1` feature; an abort asked
  again while one is queued queues nothing more.
- `Hosts.ClearRecord` orders a host to set aside its step record of a service when that record
  does not read (`clear_record`), at the host's next heartbeat. It takes no slot and is sent once.
  It is accepted only for a host whose daemon announced the `clear-record-v1` feature in its last
  heartbeat; any other is refused `daemon_too_old` and nothing is ordered. It answers the order's
  id, `clear-record-<n>`, where `n` is the number of its `record_cleared` audit entry; asked again
  while the order waits, it answers the same id and audits nothing. A call whose audit line
  cannot be written is refused `store_failed` or `audit_unavailable`, and nothing is ordered.
  The order is kept in memory only: a restart of the coordinator before the host's next heartbeat
  forgets it, and the operator asks again. The host renames the record to
  `step-record.json.corrupt` only when it does not read, and reports the order's verdict as the
  service's last update, at the version the service runs: `succeeded` with the reason
  `record_cleared`, or failed with `record_reads` (the record reads, is absent, or is of a newer
  hostkeeper), `busy` (another order of the service holds it) or `store_failed`. The
  coordinator acknowledges that verdict and `Hosts.Get` shows it as the service's last update
  until the host reports a verdict of another order, but it is no word on a release: it never
  replaces the host's standing verdict, so a failure the host fell back from still keeps that
  version from being ordered to it, and a failed clear holds no update slot.
- `Hosts.Exclude` takes a host out of a service. Its update slot stays held while the host runs
  an update (it reports one `updating` or its promotion pending) or has an order outstanding,
  until the host's own heartbeats or its order's age free it; an excluded host whose last update
  failed and that is not healthy holds none, since nothing of an update runs there and the
  service no longer orders it. With `abandon`, for a host that is gone mid-update,
  the slot is freed at once; the audit entry is `exclude_abandon` with `possibly_mid_update` in
  its reason. It is an alert while the host stays excluded (`Alerts.List`, and an
  `alert=exclude_abandon` line logged once when it appears).

## Force

`Releases.Force` is the fleet-wide fix: one call orders a hookless `force` of a service to a
version at every targeted host's next heartbeat, past the update plan (its stages, the role
limits, a pause and the other services' rollouts), the canary, the bake, a deferral, a bad mark,
a halt and a host's failure of the version. The units stop
without a drain, so a force may cut running work.

The targets are the whole fleet (the call names the service a second time, `confirm_service`),
a role, or a list of hosts. The coordinator refuses, changing nothing:

| Refusal | When |
|---|---|
| `self_service` | the service is `hostkeeper`, which updates itself by its own path |
| `below_min_version` | the version is below the service's `min_version` (the floor itself is accepted) |
| `no_digest` | CI never registered a package digest or a release index for the version |
| `confirm_mismatch` | a fleet-wide force whose `confirm_service` is not the service |
| `unknown_target` | a role the service does not run on, or a host the coordinator has not heard from |
| `not_in_host_group` | in host groups: `host_group` names a group that does not list the service or that no group has the name of, or no group lists the service |

In host groups a force is a deployment to a group: it sets the group's desired version and the
force stands in that group's marks. The call names the group in `host_group`, which must list the
service (`not_in_host_group`), or names none:

- the whole fleet is the whole group, or each group that lists the service when none is named;
- a role names a group, and one that differs from `host_group` is `unknown_target`;
- hosts must each be in the group, else `unknown_target`; with no group named, the call is one
  force for each distinct group of the hosts, and a host the coordinator has not heard from, or in
  no group, refuses the whole call.

A force that reaches several groups is one change, with one `force` entry for each group (naming
it) and the `unmark_bad`, `mark_bad` and `clear_halt` entries of its group after it. A refusal for
any group (a version below the `min_version` of one, for example) changes nothing for any. The
force of a group ends when another version is set in that group, or another force replaces it;
the force of another group is not touched. The reply lists the hosts and entries of every group.

Otherwise it sets the desired version to the target, records the force, lifts the bad mark of
the target, marks bad each version a targeted host leaves on a downgrade (from the versions the
hosts last reported), clears a standing halt, and raises the service's halt generation so that
a host holding a halt of the service lifts it. The audit entries are written in that order, in
one write; the `force` entry names the token, the targets and the hosts holding an update slot
that the force passed.

The reply lists the hosts ordered (`targeted`), the held pairs it skipped (`held`), and the
other hosts it does not order with a reason: `daemon_too_old` for a daemon that did not
announce the `forced-v1` feature, `unknown_target` for a listed host that does not run the
service, `no_package_for_platform` for a host whose platform the version's release index lists
no package for, and, for a version set with targets, `no_target`, `no_build_for_host` or
`installer_dropped` for a host the release picks no entry for (each host forced is ordered the
entry the rollout gate would pick it). A held pair is ordered only when the call sets `include_held`, for that call only, and
no hold is removed.

A host that reports the force failed is not ordered again; a second force, to another version,
orders it again. A force whose targets are all held or too old orders no host and still makes
every change above. The force stands until CI or an operator sets another version or another force replaces it.
While it stands, the automatic rollback of the service is suspended, and the hosts it orders
take no update slot and no place in a stage. The new desired version the force sets starts the
service's update plan over for the hosts it does not order.

The `force`, `abort` and `clear_record` orders travel only on the typed heartbeat of
`hostkeeper.agent.v1`; the JSON heartbeat route carries none of them.

## The audit file

On the database store (`--database-dsn-file`, [coordinator-state.md](coordinator-state.md#the-database-store))
every change of the operations state, the desired state or the verdicts is one row of the table
`audit`, committed in the same transaction as the change itself, so there is no case where such a
change is written and its line is not: `audit_unavailable` is never returned for these calls there
(an enrollment change's row is the coordinator's second transaction, as on the files, and the next
start writes one that did not land: [enrollment-coordinator.md](../enrollment-coordinator.md#state-files)), and a change whose commit fails is refused `store_failed`
and changes nothing. Every row is kept (no rotation; `--audit-max-bytes` and
`--audit-retained-segments` are refused beside the database), and `Audit.List` is one query over
the table with the same filters, paging and `since_seq` cursor. The three calls below that stand
without their line stand on the database store while its session is lost: their change is in
memory, with its row, and both are committed together once the session is back
([coordinator-state.md](coordinator-state.md#the-database-store)). The rest of this section is the
file store.

Every change is one JSON line of the audit file `<ops-file>.audit`, appended and fsynced;
`Audit.List` reads it back. The file is rotated by size:

- When a line would take a file that is not empty past `--audit-max-bytes` (default 16 MiB), the
  file is sealed: renamed to `<ops-file>.audit.<n>`, `n` one past the newest segment's (1 for the
  first), and the line starts a fresh `<ops-file>.audit`. A line is never split across files,
  and a sealed segment is never written again. A line longer than the maximum is written whole,
  alone in its file.
- At each rotation the oldest segments beyond `--audit-retained-segments` (default 8) are
  deleted, so the audit holds at most the active file and that many segments. A segment that
  cannot be deleted is logged as a warning and tried again at the next rotation. Both flags
  refuse 0 at start.
- The entry numbers (`seq`) go on across files: the first line of a fresh file is the last
  number plus one. At start the coordinator reads the last number of the newest file that holds
  a line, so the numbers go on after a restart, also when the active file is empty or missing
  right after a rotation.
- `Audit.List` reads the active file and the segments kept, in number order, with the same
  filters, paging and `since_seq` cursor as over one file. A `since_seq` in a segment already
  deleted answers from the oldest entry kept; nothing in the answer says that older entries
  were deleted (the response has no field for it).
- `hostkeeper-coordinator backup` copies the active file and every segment kept, and `restore`
  writes them back under their numbers ([coordinator-state.md](coordinator-state.md)).

A change is not made without its audit line. When the line cannot be appended (a full disk, a
file that cannot be opened), the call is refused with `audit_unavailable` (gRPC `UNAVAILABLE`)
and changes nothing: `Releases.Force`, `Services.Hold`, `Releases.SetDesired` by an operator
or with targets, and every other call above that changes the ops state. Asking again once the disk has room is
safe. The line is appended before the ops file is written, so an audit line can name a change
whose ops-file write then failed (`store_failed`), or the first lines of a force refused at a
later one, never the other way round; a force, which
writes the desired state first, puts it back. On the JSON routes, whose answers are frozen, the
same refusal is `500 internal`.

Three calls still stand when their line cannot be appended, so that an operator can stop a rollout
on a full disk: `Hosts.Abort`, `Releases.Halt` and `Rollouts.Pause`. Each logs the failed append as an error and
keeps its entry in the ring of the ops file (`Releases.Status` shows the last 64); its line is
not in the audit file. The coordinator's own changes that raise protection (a halt a host
reports, a version marked bad by its failures) stand the same way. Read-only calls, `Audit.List`
included, are not affected.
