# Maintenance updates on Linux: the notice, Update, Schedule and a halted host

Every Linux host's daemon reports, in its heartbeat, the security and kernel updates its host has
not taken and whether it needs a restart. The web UI shows that as a notice; one **Update** button,
or a **Schedule** with choices, then takes each host out of service, updates it, restarts it if it
needs to, checks it and puts it back, one batch of each host group at a time. A failure stops the
run in that group and leaves the failed host out of service until an operator acts. This page is
how to do each of those things, and what to do when one goes wrong. The design is
[maintenance-updates.md](../design/maintenance-updates.md) (sections 3, 4.2 and 5); the group's
settings are also in [hostgroups.md](hostgroups.md#maintenance); `hk` is [cli.md](../cli.md). A Mac's
macOS and Xcode updates are not on this page.

Every `Maintenance` call takes the operator's credential only (a token of the tokens file, a stored
API token, or a signed-in session in the UI): a CI token is refused with `forbidden`, no token with
`unauthorized`, and a refused call changes nothing and orders no host. A call that changes
something needs a reason (1 to 256 bytes, no control character), is an entry of the audit log under
the name of the caller, with the reason followed by `run <id>`, and answers that entry. The
commands below reach the coordinator's private listener: `https://<private-listener>` is its
address, `<ca.pem>` the CA its certificate chains to, and `<token-file>` a file holding an operator
token. A refusal prints `refused: <token>` on stderr; the tokens are in [Refusals](#refusals). The
gateway serves each call at `POST /hostkeeper.operator.v1.Maintenance/<Method>`
([gateway.md](../gateway.md)).

## What the notice means

**Where it comes from.** The coordinator discovers no update itself. Each host's daemon looks at
its own host after it starts, then every hour, and again as soon as a maintenance turn of it ends,
and sends what it found in each heartbeat (the feature `maintenance-v1`). It reads
`/etc/os-release`, `uname -r`, the kernels under `/lib/modules`, and asks the package manager it
finds (`apt-get`, else `dnf`, else `apk`) what it would upgrade. Two kinds can be due on Linux:

- `linux`: a package update. With the daemon's `--maintenance-linux-scope security` (its default),
  only security updates (apt: an origin ending `-security`; dnf: listed by `dnf updateinfo list
  --security`) and new kernels (apk names no security origin, so there only kernels); with `all`,
  every update. A new kernel package also needs a restart.
- `reboot`: a restart the host needs and has not had, with nothing to install: a newer kernel
  installed than the one running (`kernel`), `/run/reboot-required` (`reboot-required`),
  `needrestart -b` saying the kernel is stale (`needrestart`), or `needs-restarting -r`
  (`needs-restarting`).

The packages hostkeeper installs itself (`hostkeeper` and every package the daemon placed) are
never reported due, and a run never moves them: apt holds them (`apt-mark hold`), dnf runs with
`--exclude=<name>`, and apk pins each in `/etc/apk/world` at the version installed.

The scope of the notice is the daemon's flag, a host setting. The scope of what a run installs is
the group's `linux_scope` ([The group's settings](#the-groups-settings)), which the coordinator
sends in the order. Keep the two the same: a group set to `LINUX_SCOPE_ALL` whose daemons run with
the default shows only security updates and kernels as due, and a run then installs every update.

**Where it shows.**

- **Home**, above the alerts: a strip headed "Updates due", one line per host group with due hosts,
  in the coordinator's words ("Updates due on 4 hosts of build"), each with an **Update** button,
  one Reason field for the strip, and **Open Maintenance**; the line for the hosts in no host group
  has no button (update those from the Maintenance page). It is the warning alert `updates_due`
  ([Alerts](#alerts)), and it stands while something is due; it is a notice, never a page.
- **Host page**: under its name, "Due" and a short phrase per item: "kernel update, restart needed"
  (a kernel package, or a newer kernel installed and not running), "restart needed" (any other
  restart it needs), "12 security updates", "3 other updates", and "and 4 more" for items the report
  left out (it lists at most 12). Its **Updates** tab shows the operating system, the running and
  the newest installed kernel, when it booted, the last look and how old it is, and a table of the
  due items (Kind, From, To, Restart, Security, Label); below it, **Update** and the **Schedule**
  form for this host.
- **Maintenance** (in the Manage menu): the fleet's count ("Updates due on 7 hosts in 2 groups"),
  the runs, and a card per host group with its hosts, the due ones first, each with what is due and
  its last look.

**When to distrust it.** A look older than two hours, or one more than five minutes ahead of the
coordinator's clock, is stale: the host page says "stale look" with its age, and an empty list then
does not mean nothing is due. A failed look keeps the last good report and names why
(`look_failed` in the report). A host that shows "no report" runs a daemon without
`maintenance-v1`, or has not sent a heartbeat since the coordinator started; it is never ordered.
The same report, read with `hk`:

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts get --node-id build-03
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    alerts list
```

The host's answer holds `maintenance` (the report: `host`, `due`, `dueLeftOut`, `lookedUnix`,
`lookFailed`, `run`), `maintenanceLookAgeSecs` and `maintenanceStale`.

## What one host's turn does

A run takes its hosts one batch of each group at a time ([Who is taken, and when](#who-is-taken-and-when)).
Each host's turn is the same sequence, run by the host's daemon, which keeps where it is in a run
record (`/var/lib/updater/maintenance/maintenance-record.json`) written before and after each
step, so a crash, a reboot or a newer daemon picks it up where it stopped:

| # | Step | What runs | If it fails |
|---|---|---|---|
| 1 | stage | `hostkeeper maintenance-hook prepare`: refreshes the package lists, takes the package manager's own estimate of the download, checks there are at least twice as many bytes free where it downloads, and downloads only (`apt-get --download-only`, `dnf upgrade --downloadonly`, `apk fetch`). Nothing is installed; the host still serves. The kind `reboot` stages nothing (a restart has nothing to download), so a run of `reboot` alone passes this step without running a program. | deferred: nothing was touched |
| 2 | drain | each installed service whose package names `[hook] drain` and `[hook] resume`, in the order of their names: its `drain` takes it out of service and returns when it has no work (js-agent's cordons the host in the lease server and waits for its leases to end) | exit 75, or the run's drain deadline passing: deferred, each drained service resumed. Anything else: halted |
| 3 | apply | `maintenance-hook cutover`: the upgrade of the order's scope (apt `install --only-upgrade` of the security packages and kernels, or `dist-upgrade` under `all`, keeping existing configuration files; dnf `upgrade --security` then the kernel by name, or `upgrade`; apk the kernel alone, or `upgrade`), then, when the host needs a restart (a newer kernel, `/run/reboot-required`, `needrestart`, `needs-restarting`), `systemctl reboot` (OpenRC's `reboot`). After the restart the daemon sees the new boot id and goes on | halted |
| 4 | check | `maintenance-hook verify`: the host booted again if it had to, runs the newest installed kernel and needs no restart, has nothing of the order's scope still due, and every installed service reports itself healthy | halted |
| 5 | put back | each drained service's `resume`, in the reverse order (js-agent's uncordons) | halted |
| 6 | done | the record is closed; the daemon looks again at once, so the notice clears | |

No step runs `rollback`: an OS update cannot be rolled back. A host whose packages name no `drain`
step is not taken out of service at all: it is staged, updated, restarted if needed and checked
while it serves. A package drains when its manifest names both `[hook] drain` and `[hook] resume`.

While a host is in its turn (from the order that starts it until it is done, deferred or put
back), the coordinator gives it no package update (`busy_elsewhere`); it gets them after its turn.
A host with a package update under way is not started. A run does not count against
`--live-rollouts`. A host in its turn cannot be moved to another group (`HostGroups.Move` is
refused `host_in_maintenance`).

## Update: the one button

**Update** is everything due, now, with the group's batch and drain deadline. It is on the
Home strip (for a group), on a host's Updates tab (for that host), and on the Maintenance page (on
each due host's row, and **Update the group** on each group's card but Unassigned's). Type a
reason, press **Update**, and confirm "Update the due hosts of build? It drains, updates, restarts
if needed, puts back." The page answers "Scheduled run run-1796529600-1: 3 hosts waiting", and
names any host it left out, with why.

It is one `Maintenance.Schedule` with no kinds, no window, no batch and no drain deadline. The same
from the command line, for a group and for one host:

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance schedule --host-group build --confirm true --reason "kernel security update"
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance schedule --host-ids build-03 --confirm true --reason "kernel security update"
```

The answer is the run (its `id` is the one every later call names) and its hosts, each `waiting` or
`skipped`.

**Update never takes a group's last host.** A run that would take every host of a group (the one
host of a group of one among them) is refused `last_host`. Update the group with every host due is
therefore refused: use **Schedule** with "Take the group's last host" ticked
(`--take-last-host true`), or update the hosts a few at a time. The group's `min_serving` still
holds: with it at 1, a group of three takes one host while two others serve.

## Schedule: now or a window

**Schedule** is the same run with choices, on a host's Updates tab and on each group's card
(**Schedule...**). Its fields:

- **What**: the kinds due among its hosts, all ticked ("Linux updates", "restart"). With every one
  ticked, the run takes whatever is due on each host when it is stored; untick one to leave it out.
- **Where**: this host, every host of the group, or the hosts ticked (at most 32).
- **When**: now, or in a window, a start and an end typed in your own time zone, which the form
  shows in UTC under them. Before the start no host starts; after the end no host starts, and a
  host already in its turn finishes: a window never cuts a drain.
- **Batch**: how many hosts of each group at once (empty: the group's `batch`).
- **Drain deadline, seconds**: empty is the group's `drain_deadline_secs`; 0 is no deadline.
- **Take the group's last host**: needed to take every host of a group.
- **Reason**, then **Check**, which asks the coordinator without storing anything and shows each
  host it would take and each it would leave out and why, and **Schedule**, which stores the run
  after one confirmation.

From the command line, a run without `--confirm true` is the Check: nothing is stored, no host is
ordered, the answer has no run id, and it is refused exactly as the stored run would be. A window
is in Unix seconds:

```
date -u -d '2026-12-05 22:00' +%s
date -u -d '2026-12-06 04:00' +%s
```

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance schedule --host-group build --kinds linux \
    --window-start-unix 1796508000 --window-end-unix 1796529600 \
    --batch 2 --drain-deadline-secs 7200
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance schedule --host-group build --kinds linux \
    --window-start-unix 1796508000 --window-end-unix 1796529600 \
    --batch 2 --drain-deadline-secs 7200 --confirm true --reason "Saturday night patch window"
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance schedule --host-group lab --take-last-host true --confirm true \
    --reason "the lab's one host"
```

A window start needs an end, and the end must come after it; an end alone (the start 0, now) is
taken. A window whose end has passed is refused `window_ended`. A host in a turn of another run, or
still waiting or deferred in another run that has not ended, is refused `host_in_maintenance`:
[Cancel](#cancel) that run first, or leave the host out.

### Who is taken, and when

A run holds the hosts named, or every host of the group, that have something due of its kinds.
A host with nothing due is not in it; a run in which no host would wait is refused `nothing_due`.
A host is listed `skipped`, and never ordered, when its daemon does not announce `maintenance-v1`
("older daemon"), a hold stands on it for a package (`Services.Hold`), or it is excluded from a
package (`Hosts.Exclude`). The others wait, and at each of its heartbeats the coordinator starts a
waiting host only when all of these hold:

- the coordinator is past its quiet start (`--quiet-secs`, 30 seconds by default);
- the run's window is open;
- no host of the same group is halted or not back in the run (other groups go on);
- the host is live, is not held or excluded, is still in the group it was taken in, and has no
  package update under way;
- fewer than the batch (the run's, else the group's `batch`) of the group's hosts are in their turn,
  counted over every run; a host whose turn is done keeps its place until it has sent the group's
  `bake_beats` healthy heartbeats in a row;
- it is next: waiting hosts by node id, then deferred ones, the fewest offers first;
- at least `min_serving` + 1 of the group's hosts are serving (live, no service unhealthy, not in a
  turn), counting the host about to start; with `take_last_host`, never more than the hosts of the
  group the coordinator has heard.

Groups go on in parallel: a run over three groups with batch 2 has up to six hosts in their turn.

## How a drain waits

A drain has no deadline unless the run or the group sets one: it waits until each service's
`drain` says it has no work, however long that takes, and nothing stops a job to finish an
update. While a host drains, the run shows it "draining since" the time it started.

With a deadline (the Schedule's drain deadline, or the group's `drain_deadline_secs`), a drain that
has not ended when the deadline passes is ended, every service drained so far is resumed, and the
host is back in service with nothing changed: its turn is `deferred`. The run goes on to the next
host, and offers the deferred one again after the group's other hosts, while the window is open,
once the group's `defer_backoff` has passed since it deferred (300 seconds by default, doubling with
each deferral of its turn, at most 3600). A stage that fails defers the same way. A run with no
window end keeps offering a deferred host; Cancel it to stop.

## Reading a run

The Maintenance page's **Runs** lists the last ten runs, read again every 10 seconds: each with its
id, state, where, window, batch, reason and who scheduled it, "halted in: build" when a group of it
is halted, and a row per host with its group, step and since when. From the command line:

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance list --states running --states halted
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance get --run-id run-1796529600-1
```

`List` answers the newest runs first, without their hosts; `Get` one run with each host's `step`,
`sinceUnix`, the kinds of its turn (`kinds`), the kinds applied so far (`applied`), why it was
skipped (`skippedBecause`) and, for a halted or deferred host, its daemon's `reason`. The
coordinator keeps the newest 256 runs; the audit log keeps every entry (`hk audit list --action
maintenance_step` and the run's `maintenance_schedule`, `maintenance_start`, `maintenance_cancel`,
`maintenance_retry_host` and `maintenance_put_back`).

### A run's states

| State | Means |
|---|---|
| `scheduled` | its window has not opened; no host has started |
| `running` | hosts are being taken, a batch of each group at a time |
| `halted` | a host of at least one group (`haltedGroups`) is halted or not back: no further host of that group starts, and the run goes on in its other groups |
| `done` | every host's turn has ended |
| `cancelled` | an operator cancelled it |
| `window_ended` | its window ended with hosts not reached |

### A host's steps

| Step | Means |
|---|---|
| `waiting` | in the run, not started: its batch place, the window or the serving count holds it |
| `staging` | downloading the update while it still serves |
| `draining` | its services' `drain` steps run |
| `applying` | the update is being installed |
| `rebooting` | it is restarting, for a kind that needs it |
| `checking` | the hook's `verify` runs |
| `putting_back` | the drained services' `resume` steps run |
| `done` | back in service, updated, or put back by an operator |
| `deferred` | staging failed or the drain deadline passed; back in service with nothing changed, offered again later |
| `halted` | a step from drain on failed; it stays out of service until an operator acts ([A halted host](#a-halted-host)) |
| `not_back` | it was restarting and stayed silent past the group's `return_secs` |
| `not_reached` | not started before the run was cancelled or its window ended |
| `skipped` | left out of the run, with why; never ordered |

A halted or deferred host's reason is its daemon's: the step, the exit, and the hook's reason when
it gave one, such as `check: exit 1, kernel_not_newest: the host runs kernel 6.8.0-45-generic, and
6.8.0-49-generic is installed` or `stage: exit 75, free_space_short: /var/cache/apt/archives has
41943040 bytes free; the download of 73400320 bytes needs 146800640`. The step is `stage`,
`drain <service>`, `apply <kind>`, `check` or `resume <service>`. The hook's reasons:

- stage (always deferred): `free_space_short`, `package_manager_busy` (the package manager's lock
  was held for 300 seconds), `stage_listing_failed`, `stage_download_failed`, `stage_dir_failed`,
  `no_package_manager`, `own_packages_unreadable`, `linux_scope_invalid`, `unsupported_kind`;
- apply: `install_failed` (names the program, its exit and its first line of error output),
  `reboot_check_failed`, `kernel_unreadable`, `reboot_failed`, `boot_id_unreadable`,
  `run_record_failed`, and the stage's `no_package_manager`, `own_packages_unreadable` and
  `linux_scope_invalid` should the host change between the two;
- check: `kernel_not_newest`, `reboot_needed`, `update_due`, `service_unhealthy`, `not_applied`,
  `not_rebooted`, `facts_unreadable`, `health_unreadable`, `boot_id_unreadable`,
  `run_record_failed`;
- a service's own `drain` or `resume`: its exit code alone.

## A halted host

A step from drain on that fails (other than a drain that defers) halts the host. Nothing after it runs: no `resume` runs, so **every
service drained so far stays out of service**, and nothing is rolled back, so whatever the apply
installed stays installed. That is on purpose: the host failed in the middle of a change, and
putting it back to work is a person's call. js-agent's cordon lasts across the host's restarts and
daemon restarts until its `resume` runs (js-agent keeps it with a file in its own state directory,
komira-ai/jobscheduler). The halted record is not resumed when the daemon restarts either.

The run halts in that group: the host keeps its batch place, no further host of the group starts,
and the critical alert `maintenance_halted` stands ([Alerts](#alerts)). The run goes on in its
other groups.

**Read why.** The run's row says "halted: " and the reason; the alert carries it too. Fix the cause
on the host if it is one you can fix, then choose:

### Retry

Runs the turn again from the step that failed: the same drain, the same apply, the check, or the
same resume, then on to the end. In the UI, type the run's Reason and press **Retry** on the host's
row ("Retry build-03 from the step that failed? It is drained again if needed and the update goes
on.").

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance retry-host --run-id run-1796529600-1 --host-id build-03 \
    --reason "freed /var, the install can go on"
```

Use it when the cause is gone: a package mirror that was down (`install_failed`), a service that
was still starting when the check ran (`service_unhealthy`: the check reads each service's health
once, right after the restart, so wait until `hk hosts get` shows it healthy), a drain whose
service has been fixed. Retry is refused `run_ended` once the run has ended: put the host back
instead.

### Put back

Runs every drained service's `resume`, in the reverse order, and ends the host's turn as done,
leaving the update as far as it got (the run's `applied` says which kinds were). The run goes on in
the group. In the UI, **Put back** ("Put build-03 back? Its drained services' resume runs, the
update stays as far as it got, and the run goes on."):

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance put-back --run-id run-1796529600-1 --host-id build-03 \
    --reason "back to work; the kernel update waits for next week"
```

Use it when the host should serve again before the cause is understood, or when a Retry cannot
succeed (a check that fails `update_due` for a package the mirror will not have until later). Put
back is taken in a run that has ended, too, for a host still halted in it. The host's due items
stay due, and a later run updates it.

Neither call reaches the host at once. Each stores the act, and the coordinator sends it at each of
the host's heartbeats until the daemon reports the turn at another step; the answer still shows
the host `halted`, and the alert adds "a retry is sent to its daemon" (or "a put back"). Watch the
run with `hk maintenance get`; the alert clears once the daemon reports the new step. Calling again
while the host is still halted replaces the act. Both are refused `not_halted` for a host that is
not halted (a host not back among them) and `not_in_run` for a host the run does not hold.

### Cancel

Stops the run: no host starts from now on, a host in its turn finishes it, and the hosts that had
not started end as not reached. A halted host stays halted, and out of service, until it is put
back; Cancel does not release it. In the UI, **Cancel** on the run ("Cancel run run-1796529600-1?
No new host starts; a host in its turn finishes.").

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance cancel --run-id run-1796529600-1 --reason "change freeze until Monday"
```

There is no pause: a pause is a Cancel and, later, a new Schedule. A run that has ended is refused
`run_ended`.

### A host that is not back

A host the coordinator stores `rebooting` and that stays silent for its group's `return_secs` (1200
seconds for a Linux host by default) is stored `not_back`, and the critical alert
`maintenance_not_back` stands. It is still out of service (no `resume` has run), and no further host
of its group starts. There is no Retry or Put back for it: its turn goes on by itself when its
daemon reports again. Look at the machine: open its Console (Serial over LAN) with the **Console**
button of its server's row in Home's List view, and see whether it is stuck at the firmware, the boot loader or a kernel that
does not boot. Once it reports, the turn goes on (to the check), and a failure there halts it as
above. While a host is stored rebooting or not back, the coordinator raises no `silent_host` for
it.

## Alerts

All three are in `Alerts.List` (`hk alerts list`), logged once (`alert=<kind>`) when they first
appear, and clear by themselves when their cause does.

| Alert | Level | Stands while | What to do |
|---|---|---|---|
| `updates_due` | warning | a host of the group (one alert per group, `host_group` set, and one for the hosts in no group) reports something due, stale reports included | Update the group or its hosts, or Schedule them in a window; or leave it: it never pages |
| `maintenance_halted` | critical | a host's turn is stored halted (one per host); it names the host, the run, its group and its daemon's reason, and adds that a retry or a put back is sent once one is queued | [A halted host](#a-halted-host): read the reason, then Retry or Put back |
| `maintenance_not_back` | critical | a host stored rebooting stayed silent past its group's `return_secs` (one per host) | [A host that is not back](#a-host-that-is-not-back): look at its console; it goes on when it reports |

## The group's settings

A host group's policy holds a `maintenance` table. A key left out takes its default; `Get` and
`List` answer every key, defaults included (`returnSecs` shows 0 for the platform default). The
three Mac keys do nothing on a Linux host.

| Key | Default | Range, and what it does |
|---|---|---|
| `xcode_keep` | 0 | 0 to 4: the newest Xcode releases a Mac keeps; Mac only |
| `xcode_default` | `latest` | the Xcode `xcode-select` points at; Mac only |
| `macos_major` | false | whether a new major macOS counts as due; Mac only |
| `linux_scope` | `LINUX_SCOPE_SECURITY` | what a run installs: `LINUX_SCOPE_SECURITY` (security updates and kernels) or `LINUX_SCOPE_ALL` (every update); keep it the same as the daemons' `--maintenance-linux-scope` |
| `batch` | 1 | 1 to 10000: hosts of the group in their turn at once, unless a run names its own |
| `min_serving` | 1 | 0 to 10000: hosts of the group that must stay serving while one is taken; no run overrides it |
| `drain_deadline_secs` | 0 (none) | seconds a drain may wait before the host is put back as it was and deferred, unless a run names its own |
| `return_secs` | 0 (1200 for a Linux host) | 0, or 60 to 86400: seconds a restarting host may be silent before `maintenance_not_back` |

A run also reads two keys of the group's policy itself: `bake_beats` (a host whose turn is done
holds its batch place until it has sent that many healthy heartbeats in a row; default 0) and
`defer_backoff` (how long a deferred host waits before it is offered again). A host in no group
takes every default.

To change them, read the group, then put it back whole with the new table: a `Put` replaces the
group, its packages and the rest of its policy included, and one that leaves `maintenance` out sets
every key back to its default. A value out of range is refused `invalid_policy`, and the log names
the key.

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups get --host-group build
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups put --if-generation 7 --request build.json \
    --reason "two build hosts at a time, at most four hours of drain"
```

with `build.json` the group as `get` answered it, its `maintenance` changed:

```json
{ "hostGroup": { "name": "build",
    "packages": ["hostkeeper", "js-agent"],
    "policy": { "bakeBeats": 2,
                "maintenance": { "linuxScope": "LINUX_SCOPE_SECURITY", "batch": 2,
                                 "minServing": 2, "drainDeadlineSecs": "14400" } } } }
```

## Refusals

| Refusal | When |
|---|---|
| `unauthorized` | no credential, or not an operator's |
| `forbidden` | a CI token |
| `invalid_request` | a field breaks its rule: a host id, a group name, a run id; a reason missing from a call that changes something, over 256 bytes or with a control character; a kind named twice; both or neither of `--host-ids` and `--host-group`; more than 32 hosts or one twice; a batch of 0 or above 10000; a window start without an end, or an end not after its start |
| `unknown_group` | `Schedule` names a group the coordinator does not hold |
| `unknown_host` | `Schedule` names a host that is not enrolled |
| `unknown_run` | no run has the id |
| `nothing_due` | no host of the `Schedule` has anything due of its kinds that could be ordered (every one with something due was skipped, or none has sent a report since the coordinator started) |
| `window_ended` | the window's end is at or before the coordinator's clock |
| `last_host` | the run would take every host of a group, without `--take-last-host true` |
| `host_in_maintenance` | a host of the `Schedule` holds a turn of another run that has not ended |
| `run_ended` | `Cancel` or `RetryHost` of a run that is done, cancelled or past its window |
| `not_in_run` | `RetryHost` or `PutBack` of a host the run does not hold |
| `not_halted` | `RetryHost` or `PutBack` of a host whose turn is not halted |
| `store_failed` | the run could not be read or written; nothing changed, and asking again is safe |
| `audit_unavailable` | the audit entry could not be written; nothing changed, and asking again is safe |

## Not built yet

- **A new package due under the `security` scope halts the check.** The look reports a package the
  upgrade would newly install (an item with no `from`: a new `linux-image-<version>`, a new
  library) as due. Under `security` the apply installs only packages already installed, so such a
  package arrives only when a meta-package the apply names depends on it. When none does, the check
  finds it still due and the host halts `update_due`; put it back. The Ubuntu 24.04 hosts of
  [maintenance-first-linux-run.md](maintenance-first-linux-run.md) each report two such packages.
- **The Console link of the runs view.** The **Console** button on a rebooting or not-back host's
  row does not open the Console page in this release; open it from the server's row in Home's List
  view.
- **The package manager's lock.** The daemon's `--package-lock-wait-secs` reaches only the
  repository installer; the maintenance hook waits its own default, 300 seconds, for a held apt,
  dnf or apk lock. At the stage a lock still held defers (`package_manager_busy`); at the apply the
  install fails and halts.
- **The group's package order for drains.** Services drain in the order of their names, and resume
  in the reverse.

## Example: a kernel update on a group of three

`build` holds three Linux build hosts that run js-agent, with every maintenance key at its default
(batch 1, `min_serving` 1, no drain deadline) and `bake_beats` 2.

1. Ubuntu publishes a kernel security update. Within the hour each host's look finds it; the Home
   strip says "Updates due on 3 hosts of build", and each host page says "kernel update, restart
   needed".
2. **Update the group** is refused `last_host` (it would take all three). The operator opens
   **Schedule...** on the group's card, keeps "every host of build" and "now", ticks "Take the
   group's last host", types "kernel security update", presses **Check** (3 hosts would be taken,
   0 skipped), then **Schedule**: run `run-1796529600-1`, three hosts waiting.
3. `build-01` starts: staging, then draining while its last job runs (js-agent has cordoned it), then
   applying, rebooting (its console is on its server's row in Home's List view), checking, putting back, done. The two others
   served throughout: `min_serving` 1 needs two serving hosts, the one being taken counted.
4. `build-01` sends two healthy heartbeats; `build-02` starts.
5. `build-02`'s check fails `service_unhealthy`: js-agent had not finished starting after the
   restart. The run is halted in `build`, `maintenance_halted` stands, and `build-03` waits.
   `build-02` stays cordoned.
6. The operator runs `hk hosts get --node-id build-02` until js-agent reports healthy, then
   **Retry**. The check passes, js-agent's `resume` uncordons it, the alert clears and the run goes
   on with `build-03`.
7. When `build-03` is done the run is `done`, the daemons look again, and the notice clears.
