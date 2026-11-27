# Service observation

How the daemon tells whether a service runs, which version runs, and whether it answers, without
asking the service (design section 5.8). The code is the crate `hostkeeper-observe`.

A hook that exits 0 while its service is down does not make the service healthy: the heartbeat
carries what the daemon observed, never the hook's verdict, so the coordinator's bake, which
counts healthy heartbeats, does not advance on a hook's word (design section 8.5).

## What is observed

For each installed service, once per round:

1. **Liveness**, from the service manager: the state of every unit in the manifest's
   `[units] run` (systemd's `ActiveState`, `SubState`, `MainPID` and `LoadState`).
2. **Version**, from the step record. It is confirmed, never chosen, by the running executable:
   the main process of the first active unit is identified by (pid, start time), the file behind
   `/proc/<pid>/exe` is hashed, and the process is identified again. The digest is compared with
   the one sealed for the recorded version. A digest is cached under (pid, start time, size,
   modification time), so an unchanged process is not hashed again. `ObserveOptions` has
   `confirm_exe_hash` (default on) to switch the check off.
3. **Health**, from the manifest's `[health] url`: one HTTP GET per active run instance, with
   `{port}` replaced by the number after the instance's `@` (`tierstore@9301.service` gives 9301).
   Only `http://` to `127.0.0.1` or `[::1]`, no redirect followed, at most 4 KiB of the reply read,
   and only a 2xx status is healthy. A service with no run units (`unmonitored`) has its URL
   probed once; a `{port}` in it has no instance to be filled from and is `url_invalid`. When an
   answer is not 2xx the probe reads on past its header block, to at most 160 bytes of the body
   (and never past the 4 KiB), and the observation keeps the body of the first failing instance
   in run order as text: invalid UTF-8 replaced, every control character a space, white space
   trimmed, cut at a character's boundary (a job agent's `503 not placeable: <reason>`). An
   answer that is 2xx, one that never came and an empty body give none.
4. **Restarts**, from the service manager too, in the same `systemctl show`: each run unit's
   `NRestarts` (its count of automatic restarts), `Result` (`success`, `exit-code`, `signal`,
   `start-limit-hit`, ...) and, when `ExecMainCode=1` (the main process exited by itself; 2 and
   3 are a signal), its exit code `ExecMainStatus`. On a Mac, from `launchctl print` of each run
   job: its `runs` less the first spawn as the count, a result of `signal` when a signal ended
   its last run, `success` when it exited 0 and `exit-code` otherwise, and that exit code. A unit
   with no `NRestarts` (a systemd before 235, a unit type that is not a service), a job whose
   print has no `runs`, one the manager does not know, and every unit on OpenRC, which keeps no
   counter, has no restarts. An `NRestarts`, or with
   `ExecMainCode=1` an `ExecMainStatus`, that is not a number is a `manager_error`, never a
   count. A round discarded after a read of the unit set keeps the restarts of that read: a unit
   in a restart loop changes its main pid between the two reads, and its count still reaches
   the heartbeat.

The unit set is read before and after the version and health probes; a unit whose activity or
main pid changed in between discards the round.

**The daemon's own service.** `hostkeeper` is observed through the unit the daemon itself runs
as, which the platform's adapters name: `hostkeeper.service` on Linux, which its package's
`[units] run` names too, and on a Mac the launchd job `dev.hostkeeper.hostkeeper`, which the
installer pkg places and the macOS package does not name (`Observer::with_run_of`: units given
for a service whose manifest names none). Its version is confirmed against the file of that
version in the daemon's binary directory (`<state-dir>/hostkeeper/bin/versions/<version>`, the
copy of the release's binary that the self-update staged and that the unit or the job runs through
`bin/current`: the package unit starts it, and the bootstrap unit's and the plist's packaged binary execs it), not through the unit file, whose program is outside the package. So the daemon
reports its own service live, healthy and at the version it runs on both platforms; while the
package's `current` names a version the job does not run (the moment between the switch and the
restart, or after a fallback), it is `version_mismatch`.

### Restarts in the last ten minutes

The heartbeat's `restarts` of a service (`ServiceRestarts`) is made by the daemon's observer port
(`DaemonObserver`, `crates/hostkeeper-daemon/src/link_adapters.rs`) from each observation's unit
counts and a `RestartWindow` (`hostkeeper-observe`, `restarts.rs`) it keeps in memory, read at the
daemon's monotonic second:

- `count` is the sum of the run units' `NRestarts`; `recent` the sum of each unit's rise since
  its oldest observation of the last 600 seconds, so a unit's first observation counts none and
  a restart more than ten minutes old is never counted. Both saturate.
- systemd sets `NRestarts` back to 0 when a unit is started or restarted by hand, as a deploy
  does: a count that fell, or an observation at a time before the last, starts the unit's window
  again, so a deploy's restart is not counted as a crash.
- `last_result` and `exit_status` are those of the first run unit whose result is not `success`
  (nor empty), else of the first run unit; `last_result` is cut to 32 bytes.
- At most 256 observations of a unit are kept, the oldest dropped first. A unit that leaves the
  run set, and a service observed with no counts (not observed, or with no unit the manager
  counts), are forgotten; the window starts empty at each start of the daemon.
- On a Mac the window is the `runs` ring of the daemon's health state file instead
  (`crates/hostkeeper-daemon/src/mac_runs.rs`, docs/macos.md): the same counts, at the Unix
  second, keeping a sample only when a job's count changed (at most 64 of a job), so it is written
  when a job restarts and survives a restart of the daemon. launchd's count starts again at a
  `bootout` and `bootstrap`, which is a count that fell; a `kickstart` by hand is counted.

A unit at its start limit (`Result=start-limit-hit`) is `failed`, so its service is `stopped`
(unhealthy), and its restarts say `start-limit-hit` with the count that led to it.

## States

| State | When |
|---|---|
| `absent` | No `current`, or the service manager knows none of the run units |
| `unmonitored` | The manifest has no `[units] run` and the observer was given no units for the service: no unit or process to observe; its health URL, if any, is probed once |
| `transitioning` | A run unit is starting or stopping, even beside an active one: a hand-over under way |
| `live` | At least one run unit is active and none is starting or stopping; an inactive standby instance beside it is not a stop |
| `stopped` | Every run unit is inactive, failed or unknown to the manager |
| `unobserved` | A port failed, or the unit set changed during the round |

**An observation failure is `unobserved`, never `absent` or `stopped`.** Its reason is
`manager_error`, `proc_error`, `health_error` or `changed_during_round`, with the error text for
the operator.

A live service also has a version view (`recorded`, confirmed or not; `version_mismatch`; or
unknown when there is no main process, no recorded version, or the process restarted while it
was read) and a health view (`healthy`; unhealthy with `status_non_2xx`, `redirect`,
`unreachable` or `url_invalid`; or not probed when the manifest has no `[health]`).

## Reason tokens

`Observation::reason_token()` is the one token the daemon's status shows: the state's token when
the service is not live (`absent`, `stopped`, `transitioning`, `unobserved`, `unmonitored`;
for an unmonitored service whose health URL failed, the health reason instead), and for a live
service, in this order, `version_mismatch`, the health reason, `version_unknown`,
`version_unconfirmed`, or none.

## The heartbeat

`beat_health` (`hostkeeper-observe`) maps an observation to a `Health` and the version of a
`ServiceBeat`. The daemon's heartbeat sends that health through `observation`
(`crates/hostkeeper-daemon/src/link_adapters.rs`), which has no `HEALTH_UNSPECIFIED`: a
coordinator reads a service's health as healthy, unhealthy or absent. The column `Sent` is what
the heartbeat carries:

| Observation | `beat_health` | Sent | Version |
|---|---|---|---|
| `absent` | `HEALTH_ABSENT` | `HEALTH_ABSENT` | empty |
| `unmonitored`, no `[health]` | `HEALTH_UNSPECIFIED` | `HEALTH_HEALTHY` | recorded |
| `unmonitored`, healthy | `HEALTH_HEALTHY` | `HEALTH_HEALTHY` | recorded |
| `unmonitored`, unhealthy | `HEALTH_UNHEALTHY` | `HEALTH_UNHEALTHY` | recorded |
| `stopped`, `transitioning`, `unobserved` | `HEALTH_UNHEALTHY` | `HEALTH_UNHEALTHY` | recorded |
| `live` with `version_mismatch` | `HEALTH_UNHEALTHY` | `HEALTH_UNHEALTHY` | recorded |
| `live`, unhealthy | `HEALTH_UNHEALTHY` | `HEALTH_UNHEALTHY` | recorded |
| `live`, healthy | `HEALTH_HEALTHY` | `HEALTH_HEALTHY` | recorded |
| `live`, no `[health]`, version not contradicted | `HEALTH_HEALTHY` | `HEALTH_HEALTHY` | recorded |

`HEALTH_UNHEALTHY` means "does not, or the daemon cannot tell"; the reason token tells the cases
apart.

Over gRPC each `ServiceBeat` also carries three facts of the observation, whatever the coordinator
advertises (a coordinator that predates them skips them):

- `health_reason`: the detail (the reason token, or `not_observed` and `own_unit_not_loaded`
  below) when it is a token of `[a-z0-9_]`, 1 to 64 bytes; none when there is no detail.
- `health_body`: the body of the health URL's answer that was not 2xx, as above, at most 160
  bytes.
- `restarts`: the restarts of its run units ("Restarts in the last ten minutes" above); none
  where the service manager keeps no counter.

The JSON route is frozen and carries none of them, but they count toward the heartbeat's bound on
both routes; a heartbeat over its 16 KiB leaves out every service's `health_body` and `restarts`
first, and keeps `health_reason`. A service the daemon cannot observe at all (no `current`, or a manifest that does not load)
is sent `HEALTH_UNHEALTHY` with the detail `not_observed`.

An `unmonitored` service with no `[health]` (a configuration package, which only places files: no
run units, no health URL) is sent `HEALTH_HEALTHY`, as a `live` service with no `[health]` is,
since nothing of it can fail to serve. Sent unhealthy, it would keep its host from ever counting
as serving in a maintenance run (`min_serving`) and from baking after its turn
(`docs/operations/maintenance.md`, "Who is taken, and when"). Its detail stays `unmonitored`.
The Linux maintenance check reads `beat_health` itself and counts `HEALTH_UNSPECIFIED`, like
`HEALTH_HEALTHY`, as no unhealthy service.

`HEALTH_SILENT` is not in the table: no daemon sends it. The coordinator answers it in place of the
health a host last reported, in a service's view of a host that is not live (the `health` of a
`HostService` in `Releases.Status`, `Hosts.List` and `Hosts.Get`), since that report is 45 seconds old or more. The
heartbeat the coordinator keeps for the host, and shows as `heartbeat`, stays as the host sent it. A
heartbeat that carries `HEALTH_SILENT` is read as one that names no health.

The daemon's own service is the one exception to the first row: observed `absent` while its package is installed
(the service manager finds no unit, as a unit left in `/etc/systemd/system/` that hides the package's leaves it), the
daemon reports it `HEALTH_UNHEALTHY`, reason `own_unit_not_loaded`, running, since it is that process
(`crates/hostkeeper-daemon/src/link_adapters.rs`); `HEALTH_ABSENT` would read as not installed to the coordinator.

A Mac's daemon also keeps its own launchd job from being marked disabled in launchd's override
database, which would stop it from coming back after a reboot: at its start and after each
heartbeat it enables the job again and logs `alert=own_job_disabled` (docs/macos.md, "The daemon
keeps its own job enabled"). That alert is a line of the daemon's log; the heartbeat's health of
the service `hostkeeper` is unchanged by it.

## The thin modules

Everything above is pure logic over four ports (`ServiceManager`, `ProcessProbe`, `HealthClient`,
`ExpectedExe`) and runs in the tests against fakes. The parts that touch the host are small:

- `systemd.rs`: one `systemctl show` per unit through a `CommandRunner` (the real one,
  `command.rs`, runs a child process with its output capped and a deadline), asking for the
  state and the restart properties at once. launchd will be another adapter of the same port.
- `proc.rs`: field 22 of `/proc/<pid>/stat`, and the hash of the file behind `/proc/<pid>/exe`
  read through the open descriptor.
- `health.rs`: the HTTP GET over a `Connect` transport; the real one is TCP, the tests use an
  in-memory one.

## The registration socket

kbf's agent took liveness from services on a registration socket, `/run/updater/register.sock`.
hostkeeper does not carry it. At start, before any unit is touched, the daemon calls
`unlink_stale_registration_socket` with its run directory (`updater` in the platform's run
directory, `/run/updater` by default), right after it takes the lock on the state
directory: a `register.sock` left there is removed (whatever its file kind), a missing one is
fine, and any other error stops the start.
