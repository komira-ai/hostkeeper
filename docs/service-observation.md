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
   probed once; a `{port}` in it has no instance to be filled from and is `url_invalid`.

The unit set is read before and after the version and health probes; a unit whose activity or
main pid changed in between discards the round.

**The daemon's own service.** `hostkeeper` is observed through the unit the daemon itself runs
as, which the platform's adapters name: `hostkeeper.service` on Linux, which its package's
`[units] run` names too, and on a Mac the launchd job `dev.hostkeeper.hostkeeper`, which the
installer pkg places and the macOS package does not name (`Observer::with_run_of`: units given
for a service whose manifest names none). Its version is confirmed against the file of that
version in the daemon's binary directory (`<state-dir>/hostkeeper/bin/versions/<version>`, the
copy of the release's binary that the self-update staged and that the unit starts through
`bin/current`), not through the unit file, whose program is outside the package. So the daemon
reports its own service live, healthy and at the version it runs on both platforms; while the
package's `current` names a version the job does not run (the moment between the switch and the
restart, or after a fallback), it is `version_mismatch`.

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

`beat_health` maps an observation to the `Health` and version of a `ServiceBeat`:

| Observation | `Health` | Version |
|---|---|---|
| `absent` | `HEALTH_ABSENT` | empty |
| `unmonitored`, no `[health]` | `HEALTH_UNSPECIFIED` | recorded |
| `unmonitored`, healthy | `HEALTH_HEALTHY` | recorded |
| `unmonitored`, unhealthy | `HEALTH_UNHEALTHY` | recorded |
| `stopped`, `transitioning`, `unobserved` | `HEALTH_UNHEALTHY` | recorded |
| `live` with `version_mismatch` | `HEALTH_UNHEALTHY` | recorded |
| `live`, unhealthy | `HEALTH_UNHEALTHY` | recorded |
| `live`, healthy | `HEALTH_HEALTHY` | recorded |
| `live`, no `[health]`, version not contradicted | `HEALTH_HEALTHY` | recorded |

`HEALTH_UNHEALTHY` means "does not, or the daemon cannot tell"; the reason token tells the cases
apart.

## The thin modules

Everything above is pure logic over four ports (`ServiceManager`, `ProcessProbe`, `HealthClient`,
`ExpectedExe`) and runs in the tests against fakes. The parts that touch the host are small:

- `systemd.rs`: one `systemctl show` per unit through a `CommandRunner` (the real one,
  `command.rs`, runs a child process with its output capped and a deadline). launchd will be
  another adapter of the same port.
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
