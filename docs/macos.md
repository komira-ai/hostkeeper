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

The daemon on a Mac gives the unpack its real check (`hostkeeper_daemon::mac::HostMacVerify`):
the attribute is read with `getxattr` on the entry itself, never through a link, and the signature
is judged by `/usr/bin/codesign --verify --strict <path>`, run with that fixed argument list, no
shell, an empty environment, at most 64 KiB of each output stream kept, and killed after 30
seconds. `codesign` exit 0 is a valid signature; exit 1 is `unsigned` when it says the file is
`not signed at all` and `invalid_signature` with its words otherwise; any other end means the check
was not made.

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
  it are XML-escaped. The daemon writes it at each start, once ready, as
  `/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.fallback.plist` with an interval of 60
  seconds, only when it differs from what is there, and then boots the job out and bootstraps it
  again (`hostkeeper_daemon::selfunits`). Until the first self-update there is no `previous`, and
  launchd cannot spawn the job's program when the interval comes; the daemon's own plist still
  starts `/usr/local/bin/hostkeeper`, not the binary directory's `current`.

Since the timer runs the verb whether or not the new binary is failing, the verb decides the
burst itself on a Mac (`hostkeeper_daemon::fallback::fallback_after_burst`). Each start of the
daemon on a Mac first adds itself to the start log, `<state-dir>/hostkeeper/starts` (its version
and a stamp of the monotonic clock; the last 16 starts are kept). The verb then flips `current`
back, writes `Failed` after the commit and marks the version bad (the same code as the systemd
fallback runs) only when all of these hold; otherwise it changes nothing and exits 0, and the next
run of the timer looks again:

- the step record of `hostkeeper` waits for the restart into a version;
- no daemon holds the state directory's lock (a daemon that runs is not failing to start), and
  the verb holds that lock while it falls back;
- no other service's step record holds an unfinished update (the rule of the self-update order);
- the start log holds 3 starts of that version within 300 seconds, the latest less than 300
  seconds ago (`hostkeeper_steps::should_fall_back`, the rule of the systemd drop-in's
  `StartLimitBurst=3` and `StartLimitIntervalSec=300`, the same two constants).

A start is written by the daemon itself, first thing; a new binary the system kills before it
runs any of its code (a refused signature, another architecture) writes none, so this rule does
not fall back from it.

On Linux the verb falls back at once, as before: systemd has already counted the starts, and
nothing writes the start log.

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
fakes on Linux, and on a Mac in the device run below.

The daemon picks its adapters by the platform it was built for
(`hostkeeper_daemon::adapters::Adapters::for_platform`): on a Mac the step launcher, the
observer's service manager and process probe, and the unit control over `launchctl` and
`/Library/LaunchDaemons`, the package check above, and the Mac claims; on Linux the systemd
step launcher, observer and unit control (`systemctl`), and no package check. The admin socket's default is the platform's (`/var/run/hostkeeper/admin.sock` on a Mac),
in a directory the daemon makes mode 0700.

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
| the daemon's logs | `/var/log/hostkeeper/daemon.out.log` and `daemon.err.log` |

The daemon's plist runs `hostkeeper daemon` at load and keeps it alive. It names no user, no
sandbox profile and no environment.

#### The daemon's logs

launchd appends the daemon's standard output to `/var/log/hostkeeper/daemon.out.log` and its
standard error to `/var/log/hostkeeper/daemon.err.log`, the `StandardOutPath` and
`StandardErrorPath` of its plist. launchd does not make the directory of those files, so the pkg
installs `/var/log/hostkeeper` (root and wheel, 0755), empty.

The pkg also installs `/etc/newsyslog.d/hostkeeper.conf` (root and wheel, 0644), which macOS's
`newsyslog` reads. It has one entry for each of the two files, taken from the same paths the plist
names:

| field | value | meaning |
|---|---|---|
| owner, mode | `root:wheel`, `640` | the new empty file and the archives are readable by root and wheel only |
| count | `5` | five archives are kept (`.0` to `.4`); the oldest is deleted |
| size | `10240` | a file is rotated when it reaches 10 MiB |
| when | `*` | never rotated by time |
| flags | `NJ` | no process is signalled; the archives are bzip2-compressed (`.bz2`) |

Each file therefore takes at most about 10 MiB, plus five compressed archives of up to 10 MiB of
text each. No process is signalled because the daemon has no pid file and does not open the files
itself: launchd opens them when it starts the daemon. Until the daemon next starts (a self-update
restarts it, as does a reboot), it keeps writing to the file it was started with. After a rotation
that file has been renamed and compressed, so the output the daemon writes between the rotation
and its next start is not kept. The daemon writes only its errors and alerts there, so a file
seldom reaches the size.

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
| xpcproxy (launchd's spawn helper runs, before the job's program does) | activating |
| waiting or not running, last exit 0 or none | inactive |
| waiting or not running, non-zero last exit or a terminating signal | failed |
| the label is not loaded | not found |

A `launchctl` that fails, times out, or prints text the reader does not understand is unobserved:
the observer reports an error and never a state. The output of `launchctl print` is not a stable
interface; the reader is written against hand-made samples and is to be checked against output
captured on a device.

#### Process identity

The process behind a unit is identified by its pid and start time, and the file it runs is found
with `proc_pidpath`. A path names a file, and a new file can be renamed over that path while the
process runs, so the probe also reads the device and inode of the executable the process has
mapped (`proc_pidinfo` with `PROC_PIDREGIONPATHINFO`: the lowest region backed by a regular file,
the main executable's text, read from the mapped vnode and not from a path). The path's device and
inode must be the image's, and the same again on the file opened to hash it;
otherwise the round is `unobserved` (`proc_error`), never a version confirmed from the new file.

What the device run found on macOS 15 (arm64):

| case | `proc_pidpath` | the image's (device, inode) | the probe |
|---|---|---|---|
| file unchanged | the path it was started from | the file's | its digest |
| a new file renamed over the path | the same path, still | the old file's, not the path's | refused: the path is not the file the process runs |
| file unlinked | fails (`ENOENT`) | the old file's, still | refused (no path) |
| started through a symlink | the link's target | the target's | its digest |
| a path longer than `PROC_PIDPATHINFO_MAXSIZE` | cannot be made: macOS refuses a path over 1024 bytes (`ENAMETOOLONG`), so no process runs from one | | |
| run by another user | not started on the runner: the scratch directory is not open to `nobody` | | |

So `proc_pidpath` alone is not enough: after a rename over the path it still names the path, whose file is then another. With the image's device and inode it is: a replaced or unlinked file is refused, and never read as the new version.

#### The sealed executable

The digest of the running executable is compared with that of the executable sealed with the
recorded version, as on Linux. On a Mac the first `[units] run` entry is a launchd label, and the
sealed executable is the one its `units/<label>.plist` starts: the string of the plist's `Program`
key when it has one (launchd starts that file then), else the first string of its `ProgramArguments`
array, read with XML comments skipped and character references decoded
(`hostkeeper_placed::lint::plist_program`, the parser the placed-unit lint reads plists with). The
path rules are those of a systemd unit's `ExecStart=`: the path must go through
`<packages>/<service>/current/`, as the host spells the packages directory, and the rest of it be
plain names with no `..` component (an empty or `.` component inside the path is dropped, as a path
reads it); the file hashed is the one at the same place under the version's own directory. A program
anywhere else, a `Program` key not followed by a string, a plist with neither key, or a file that
cannot be read gives no digest, and the recorded version is then reported unconfirmed
(`version_unconfirmed`), never confirmed from another file.

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

The pkg is the first install of a Mac. It installs exactly five files and one empty directory,
and the directories that hold them (each 0755, as macOS has them already; `/usr/local/lib` and
`/usr/local/lib/hostkeeper` are made so when they are not there), all owned by root and wheel:

- `/usr/local/bin/hostkeeper`, mode 0755, the released binary;
- `/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist`, mode 0644, the daemon's job as
  `hostkeeper_launchd::daemon_plist` writes it for `DaemonJob::standard` (label
  `dev.hostkeeper.hostkeeper`, running `/usr/local/bin/hostkeeper daemon`);
- `/var/log/hostkeeper`, mode 0755, the empty directory of the daemon's logs;
- `/etc/newsyslog.d/hostkeeper.conf`, mode 0644, the rotation of those logs
  ([The daemon's logs](#the-daemons-logs));
- `/usr/local/lib/hostkeeper/gh`, mode 0755, the pinned darwin-arm64 `gh` that checks each
  package's attestation;
- `/usr/local/lib/hostkeeper/trusted_root.json`, mode 0644, the Sigstore trusted root that `gh`
  checks it against, as the pinned `gh attestation trusted-root` printed it when the pkg was built.

`/etc` and `/var` are links into `/private` on macOS, so the payload holds the log directory and
the newsyslog file under `private/` (`/private/var/log/hostkeeper`, `/private/etc/newsyslog.d/hostkeeper.conf`): a
directory named `var` or `etc` in the payload would stand in place of the link.

`gh` and the trusted root are under `/usr/local` because `/usr` is read-only on a Mac; a Linux
host keeps them under `/usr/lib/hostkeeper`. The workflow that builds the pkg pins both: it
downloads `gh` and checks the download against the version and SHA-256 in the workflow, the same
way it pins its other tools, then prints the trusted root with that `gh` and checks it against its
own pinned SHA-256. Nothing is downloaded at install. A change of either pin is a pull request; a
trusted root that Sigstore or GitHub have changed since the pin fails the build of the pkg until
the pin is moved.

The daemon on a Mac reads the CA bundle its package fetch trusts from `/etc/ssl/cert.pem`, the
system roots macOS keeps there, which the pkg does not install. (A Linux host reads
`/etc/ssl/certs/ca-certificates.crt`, or `/etc/pki/tls/certs/ca-bundle.crt` when only that one
exists.)

It has no scripts: it starts nothing and writes no config. The operator's bootstrap places
`hostkeeper.toml` and loads the job. The pkg carries no installer signature, since that needs an
Apple Developer ID Installer identity. It is attested like every other asset, and the binary in it
keeps the ad-hoc code signature the linker gives it, which the package verification above accepts.
`cargo xtask release macos-pkg` builds it with `pkgbuild` from the binary, `gh` and the trusted
root (`--binary`, `--gh`, `--trusted-root`); `cargo xtask release check-macos-pkg` reads back the
pkg that `pkgutil --expand` expanded, against the same three files, and refuses a missing file and
any other file, mode, owner, bytes or script, a plist whose output paths are not the daemon's two
log files, and a newsyslog file whose entries name other paths than those two.

hostkeeper's own package has no unit on a Mac: the manifest's launchd labels are the service name
or the service name, `.` and more, and the daemon's label is not, so the daemon's job is the
pkg's plist.

### The device run

The job `macos-device` of `.github/workflows/privileged.yml` runs on a GitHub-hosted arm64 macOS
runner (`macos-15`) after every merge to `main` and by hand, never on a pull request, as root.
Its tests (`crates/hostkeeper-privileged/tests/device_mac.rs`) are not counted by the coverage
gate; what they drive is measured against fakes on Linux. They:

- start the daemon as a launchd job of the system domain, with a config and directories of their
  own, and call `Status` on its admin socket;
- run the fixture hook in mode `sleep` as a step job through `LaunchdLauncher` with a bound of 5
  seconds: the job is booted out at the bound, the step has no exit code (a failed step), and
  launchd no longer holds the job;
- read a running job, an ended job and an unknown label through the observer's service manager:
  active, inactive, not found;
- check the process identity against real processes and print what `proc_pidpath` and the image
  lookup give when the file is unchanged, renamed over, unlinked, started through a link, at a
  path longer than `PROC_PIDPATHINFO_MAXSIZE`, and run by another user;
- check the package check's verdicts on real files: an ad-hoc signed copy of `/usr/bin/true` is
  valid, one without its signature is unsigned, one with a changed byte is invalid, and a file
  given `com.apple.quarantine` with `xattr -w` is quarantined.

## Specified, not built yet

- The placed plists of a service package linked into `/Library/LaunchDaemons` by the daemon in a
  real update (the unit control is wired; a device run of an update is not part of the run above).
- The boot id of the monotonic clock on a Mac (the start log and the step records compare stamps
  under it).
