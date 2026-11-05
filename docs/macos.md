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
a start limit nor `OnFailure=`, so on a Mac the same verb is run by a timer job.
`hostkeeper_steps::selfupdate` returns the text of both plists:

- `launchd_main_plist`: the daemon's `LaunchDaemons` plist, running `current` at load with
  `KeepAlive` set to `true` (the counterpart of the unit's `Restart=always`: the daemon exits
  cleanly after switching to a new binary and must be started again) and a `ThrottleInterval`;
- `launchd_fallback_job`: the job `<label>.fallback`, which runs
  `previous self-update-fallback --dir <dir>` every `StartInterval` seconds and not at load.

The plists are deterministic (keys sorted, fixed indentation) and every path and label in them
is XML-escaped.

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
ports the daemon already has: labels, the plists of the daemon and of a step job, the
`launchctl` port and its reader of `launchctl print`, the observer's service manager and
process probe, and the unit control of the linking side. It is tested against fakes on Linux
and is not wired into the daemon yet; nothing of it has run on a Mac.

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

Each hook step is meant to run as one launchd job with the label
`<prefix>.hostkeeper.step.<service>.<step>.<generation>`. The job would run as root, for the timeout,
the kill and the capture of the step's output only. Its plist has no sandbox profile and no `UserName`, and it has no rights, because a hook runs as root from a
verified release (design 5.3 and 5.9). A step that is tried again keeps its label, so a resumed run finds and stops the same job.

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

### Release and installer pkg

Every release ships macos-arm64 beside the two Linux platforms: the binary tarball
`hostkeeper-<v>-macos-arm64.tar.gz` with its SBOM, hostkeeper's own package `.tar.zst` (its
`manifest.toml` and `bin/hostkeeper`, and no `units/`), and the installer pkg
`hostkeeper-<v>-macos-arm64.pkg`. The artifacts workflow builds them on a GitHub-hosted Apple
silicon runner, on a push to `main` or by hand, never on a pull request.

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

- The daemon run under launchd with the adapters above wired in, the launcher that runs a hook
  step as a launchd job (timeout, kill and log capture only), and the placed plists in
  `/Library/LaunchDaemons`.
- The real adapters behind the ports above: the quarantine read (`listxattr`/`getxattr`),
  `codesign` run with a fixed argument list, and `ioreg`.
- The self-update verb's decision on a Mac: whether the new binary failed its burst of starts
  within the window, from the starts the daemon recorded.
- Whether `proc_pidpath` of a binary replaced while it runs tells the truth on a device (the
  probe above refuses a path whose device and inode changed while it was read).
- The device run: the daemon under launchd on a GitHub-hosted macOS runner, in the workflow that
  runs on pushes to `main` and on dispatch, never on a pull request.
