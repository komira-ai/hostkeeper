# macOS

What hostkeeper does differently on a Mac (design section 5.9). The update flow is the same as on
Linux: an order, the five steps of the hook, a step record, a verdict. This page says what the
code in this repository does on a Mac today, and lists what the design specifies that is not
built yet.

## Built

### Package verification

On a Mac, the unpack of a package (`hostkeeper-unpack`, `unpack` with `Platform::MacOs`) adds
two checks to the archive rules, after the archive's SHA-256 is checked and before the manifest
is read or the tree is sealed:

- no entry of the unpacked tree, file or directory, carries a `com.apple.quarantine`
  extended attribute. The archive rules already refuse every extended attribute in the tar
  bytes; this check is on the tree on disk, because macOS can add the attribute when a tool
  writes files, whatever the archive held;
- every Mach-O file of the tree (found by its magic number) has a valid code signature of its
  own bytes, as `codesign --verify --strict` judges it: ad-hoc or any identity. It is not a
  check of a team identity or of notarisation; which signers the daemon trusts is the
  package's attestation (design section 5.5, step 2). Hook scripts and other files that are
  not Mach-O have no signature and are not checked.

A quarantined entry, an unsigned binary or an invalid signature fails the order: `Failed`, class
`release`, reason `bad_archive`, with the relative path in the detail. A check that cannot be
made (the tool could not run, was killed or did not finish) is `Deferred`, class
`environment`, reason `verify_unavailable`: the package is not judged bad. In every failing
case the staging directory is removed and nothing is sealed or published. On Linux neither check
runs. [package-format.md](package-format.md) has the rule as part of the package format.

### Live leases: `leases_live`

A Mac that holds live leases defers an update rather than cut them. The service's own hook
decides it, in `prepare`, after undoing anything it placed: it exits `75` with the line

```json
{"class":"environment","reason":"leases_live","retry_after_secs":60}
```

The daemon never decides `leases_live` itself. It reads the exit like any other `75` from
`prepare`: no cutover is attempted, the hook's `rollback` runs as after any deferred `prepare`,
and the order ends `Deferred` with the hook's reason and retry time.
The token is `hostkeeper_hook::REASON_LEASES_LIVE`, beside `drain_bound`.

### Self-update fallback

On Linux the daemon's unit restarts it, and an `OnFailure=` unit runs the previous binary's
`self-update-fallback` verb once the new binary has failed its start limit. launchd has neither
a start limit nor `OnFailure=`, so on a Mac the same verb is run by a timer job. Two plists
serve it:

- the daemon's own `LaunchDaemons` plist, the one `hostkeeper_launchd::plist::daemon_plist`
  renders (see [One daemon, run by launchd](#one-daemon-run-by-launchd)): `KeepAlive` is plain
  `true`, the counterpart of the unit's `Restart=always`, because the daemon exits cleanly after
  switching to a new binary and must be started again. It sets no `ThrottleInterval`: launchd
  waits its default 10 seconds between two starts;
- `hostkeeper_steps::selfupdate::launchd_fallback_job`: the job `<label>.fallback`, which runs
  `previous self-update-fallback --dir <dir>` every `StartInterval` seconds and not at load.
  The plist is deterministic (keys sorted, fixed indentation) and every path and the label in
  it are XML-escaped.

### Enrollment claims

A Mac's enrollment request claims its serial number (`IOPlatformSerialNumber`) and its platform
UUID (`IOPlatformUUID`), in the `mac_serial` and `platform_uuid` fields of the claims; the Linux
claims (machine id, DMI serial and product uuid, network addresses) are left empty.
`hostkeeper_enroll::MacClaims` reads them from the output of
`ioreg -rd1 -c IOPlatformExpertDevice` through a port, and `parse_ioreg` parses it. Unlike a
Linux claim, which is absent when it cannot be read, a missing or malformed Mac value is an error
that stops enrollment before any request is posted. No placeholder is ever sent.

### launchd adapters, behind ports

The crate [`hostkeeper-launchd`](../crates/hostkeeper-launchd) holds the launchd side of the
ports the daemon already has: labels, the plists of the daemon, of a step job and of a service package's job, the
`launchctl` port and its reader of `launchctl print`, the observer's service manager and
process probe, the unit control of the linking side, and the step launcher. It is tested against
fakes on Linux and is not wired into the daemon yet; nothing of it has run on a Mac.

#### One daemon, run by launchd

On macOS the daemon is one launchd daemon run as root, the same `hostkeeper daemon` as on Linux.
Its label is `<REVERSE_DNS_PREFIX>.hostkeeper`, with `REVERSE_DNS_PREFIX` = `dev.hostkeeper`. The
maintainers must confirm the prefix before any Mac enrolls, since a label is the identity of a job.

| what | where |
|---|---|
| config | `/etc/hostkeeper/hostkeeper.toml` |
| state directory, packages directory | as on Linux (`/var/lib/updater/state`, `/var/lib/hostkeeper/packages`); both exist on macOS |
| admin socket | `/var/run/hostkeeper/admin.sock` (macOS has a read-only root and no `/run`) |
| units linked from a package | `/Library/LaunchDaemons/<label>.plist` (the linking is not built yet) |
| the daemon's plist program | `/usr/local/bin/hostkeeper` |
| the daemon's logs | `/var/log/hostkeeper` |

The daemon's plist runs `hostkeeper daemon` at load and keeps it alive. It names no user, no
sandbox profile and no environment.

#### A hook step is one launchd job

`hostkeeper_launchd::LaunchdLauncher` is the step launcher of a Mac: it implements the step
runner's `StepLauncher`, the trait `SystemdLauncher` implements on Linux, under the same
contract. Each step runs as one transient launchd job in the system domain, as root, with the
label `<prefix>.hostkeeper.step.<service>.<step>.<generation>`, read from the step's unit name
`updater-<service>-<step>-<generation>`. The job is there for the kill of the step's process
group and the capture of its output only: its plist has no sandbox profile, no `UserName` and no
`KeepAlive`, and it has no rights, because a hook runs as root from a verified release (design
5.3 and 5.9). A step that is tried again keeps its label, so a resumed run finds and stops the
same job.

One run of a step:

1. the call is checked as on Linux: the executable and the params file are absolute paths, and
   the executable is an executable file (a missing one is a hook that did not start);
2. a job of the same label still loaded from an earlier run is booted out;
3. `/var/run/hostkeeper/steps/<label>/` gets the job's plist, `<label>.plist`, and two empty
   files, `stdout` and `stderr`, which the plist names as the job's output (launchd appends to
   them, so nothing of an earlier run is read as this one's);
4. `launchctl bootstrap system <plist>` loads the job, and `RunAtLoad` starts it;
5. `launchctl print system/<label>` is read every 500 ms until the job has ended (no process,
   and a last exit code or a terminating signal) or is no longer loaded;
6. the last 1 MiB of each output file is kept, as on Linux, and the last line of the kept
   standard output is the step's report; the job is booted out (unloaded) and its directory
   removed.

| how the job ended | the step's exit |
|---|---|
| `last exit code = N`, no terminating signal | code `N` |
| a terminating signal | no code: a failed step |
| still running at the step's bound | booted out by the launcher, no code: a failed step |
| no longer loaded (stopped) | no code: a failed step |

launchd has no counterpart of `RuntimeMaxSec=`, so the launcher enforces the bound itself: a
step bounded at `n` seconds that is still running at the first read at or after `n` seconds is
booted out. A `prepare` declared with `prepare = 0` is unbounded: the launcher never boots it
out, and nothing but the hook's own end ends it. Because the bound is the launcher's, a bounded
job outlives a daemon that dies while it runs; the daemon, back, finds the step `running` in its
record and stops its job by name before it runs the step again.

Stopping a step is `launchctl bootout system/<label>`, which waits for the job to end: launchd
sends `SIGTERM`, then `SIGKILL` to the job's process group after its exit timeout (launchd's
default; the plist sets none). A label that is not loaded is stopped already, so a second stop,
or a stop of a step that has ended, is not an error.

A step file that cannot be read keeps nothing of that stream (the report is then `{}`, and the
exit code decides). A failure of `launchctl` (a load that fails, a read that fails, a boot-out
at the bound that fails) is a failure of the launcher, not of the step: the launcher boots the
job out once more and removes its directory, and the runner leaves the step `running` and stops
with the error (resume stops the job by name if it is still there).

The equivalence tests (`crates/hostkeeper-launchd/tests/equivalence.rs`) run the same scenarios
against both launchers through the trait: a bounded step that overruns is stopped at its bound
and failed, an unbounded one is not, a double stop is `Ok`, and the capture keeps the tail. The
real `launchctl` run is not part of the pull-request check (no Mac runs there): the host's
launcher, `LaunchdLauncher::system`, is compiled on macOS only.

#### Service state

The state of a service is read from `launchctl print system/<label>`:

| launchd | state |
|---|---|
| running | active |
| spawn scheduled | activating |
| waiting or not running, last exit 0 or none | inactive |
| waiting or not running, non-zero last exit or a terminating signal | failed |
| the label is not loaded | not found |

A `launchctl` that fails, times out, or prints text the reader does not understand is unobserved:
the observer reports an error and never a state. The output of `launchctl print` is not a stable
interface; the reader is written against hand-made samples and is to be checked against output
captured on a device.

#### Process identity

The process behind a unit is identified by its pid and start time, and the file it runs is found
with `proc_pidpath`. Its digest is taken from the file opened by that path, and the device and
inode of the path before and after the read must be the same, so a file replaced while it is hashed
is refused.

#### Units in a package

A package's `units/` may hold `<label>.plist` files. A label is the service name or starts with
`<service>.`. The manifest's `[units] run` then names labels instead of systemd unit names.
The plists of a service package are rendered by `hostkeeper_launchd::service_plist`: the program and
its arguments, `RunAtLoad` false, and a `KeepAlive` that holds the job alive only while a given path
exists, so a slot without its marker file never starts.

### Release and installer pkg

Every release ships macos-arm64 beside the two Linux platforms: the binary tarball
`hostkeeper-<v>-macos-arm64.tar.gz` with its SBOM, hostkeeper's own package `.tar.zst` (its
`manifest.toml` and `bin/hostkeeper`, and no `units/`), and the installer pkg
`hostkeeper-<v>-macos-arm64.pkg`. It also ships the packages of `hello`, the example service of
`examples/hello`, and of its failing build, for macos-arm64: `bin/hello`, `manifest.toml` and
`units/hello.a.plist` and `units/hello.b.plist`, the plists of its two slots rendered by
`hostkeeper_launchd::service_plist`; `hello` carries the same ad-hoc code signature as the
hostkeeper binary. The artifacts workflow builds them on a GitHub-hosted Apple
silicon runner, never on a pull request; it runs by hand until its push-to-`main` trigger lands with
the rest of the release automation.

The pkg is the first install of a Mac. It installs exactly two files, and the directories that
hold them (each 0755, as macOS has them already), all owned by root and wheel:

- `/usr/local/bin/hostkeeper`, mode 0755, the released binary;
- `/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist`, mode 0644, the daemon's job as
  `hostkeeper_launchd::daemon_plist` writes it for `DaemonJob::standard` (label
  `dev.hostkeeper.hostkeeper`, running `/usr/local/bin/hostkeeper daemon`).

It has no scripts: it starts nothing and writes no config. The operator's bootstrap places
`hostkeeper.toml` and loads the job. The pkg carries no installer signature, since that needs an
Apple Developer ID Installer identity. It is attested like every other asset, and the binary in it
keeps the ad-hoc code signature the linker gives it, which the package verification above accepts.
`cargo xtask release macos-pkg` builds it with `pkgbuild`; `cargo xtask release check-macos-pkg`
reads back the pkg that `pkgutil --expand` expanded, and refuses any other file, mode, owner, bytes
or script.

hostkeeper's own package has no unit on a Mac: the manifest's launchd labels are the service name
or the service name, `.` and more, and the daemon's label is not, so the daemon's job is the
pkg's plist.

## Specified, not built yet

- The daemon run under launchd with the adapters above, the step launcher among them, wired
  in, and the placed plists in `/Library/LaunchDaemons`.
- The step launcher run against the real `launchctl` as root on a device.
- The real adapters behind the ports above: the quarantine read (`listxattr`/`getxattr`),
  `codesign` run with a fixed argument list, and `ioreg`.
- The self-update verb's decision on a Mac: whether the new binary failed its burst of starts
  within the window, from the starts the daemon recorded.
- Whether `proc_pidpath` of a binary replaced while it runs tells the truth on a device (the
  probe above refuses a path whose device and inode changed while it was read).
- The device run: the daemon under launchd on a GitHub-hosted macOS runner, in the workflow that
  runs on pushes to `main` and on dispatch, never on a pull request.
