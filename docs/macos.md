# macOS

What hostkeeper does differently on a Mac (design section 5.9). The update flow is the same as on
Linux: an order, the five steps of the hook, a step record, a verdict. This page says what the
code in this repository does on a Mac today.

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

### Self-update hand-over

hostkeeper updates itself by a hand-over (design section 4.3 of
[two programs](design/two-programs.md)): the daemon stages the new binary as `versions/<N>`,
points the link `next` of the binary directory at it, writes the record as waiting for the
restart into `N` with a start count of 0, and exits 0. `current` is not moved. launchd starts the
job's program again (`KeepAlive`), and the binary `current` names, the old one, execs `next`
while the count lasts, raising it at each exec; the new binary's start finishes the switch and
confirms the update. After three starts of the new binary that do not settle the update, the old
binary settles it `Failed` and runs. There is no fallback job on a Mac, and no unit on Linux:
launchd's relaunch pause (10 seconds, below) paces the tries, and the Mac fallback is tested under
the real launchd (`a_next_binary_that_cannot_start_is_fallen_back_from_under_launchd`).

A release before 0.1.194 cannot update to this one (its floor, `OLDEST_READER`), so the daemon
writes, reads and removes no fallback job, and the binary has no `self-update-fallback` verb.

The plist is the daemon's own `LaunchDaemons` plist, the one
`hostkeeper_launchd::plist::daemon_plist` renders (see
[One daemon, run by launchd](#one-daemon-run-by-launchd)). Its program is the installed binary,
`/usr/local/bin/hostkeeper` (`hostkeeper_launchd::plist::INSTALLED_BINARY`), run as `daemon`.
Started from that path, the daemon seeds the binary directory of hostkeeper's own update when
`current` starts nothing and execs `current`
(`/var/lib/updater/state/hostkeeper/bin/current`, the path `hostkeeper_daemon::selfupdate::binary_dir`
derives from the daemon's state directory), the link a self-update switches to the new binary,
with its own arguments (`hostkeeper_daemon::packaged`), at each start; so launchd's job runs the
binary the last update chose. `KeepAlive` is plain `true`, the counterpart of the unit's
`Restart=always`, because the daemon exits cleanly after switching to a new binary and must be
started again, from what `current` names then. It sets no `ThrottleInterval`: launchd waits its
default 10 seconds between two starts.

launchd has no counterpart of the Linux package unit's `ExecStartPre=`, and needs none: the job's
program is the installed binary, which seeds the binary directory at each start. A `current` that
later starts nothing (removed by hand, or a link to a version that is gone) is seeded again by the
next start, since the installed binary, not `current`, is what launchd runs.

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
step launcher, observer and unit control (`systemctl`), and no package check. Each set names the
unit the daemon runs as, which the observer watches as the service `hostkeeper`: on a Mac the
daemon's job `dev.hostkeeper.hostkeeper`, since the macOS package of hostkeeper names no unit
(docs/service-observation.md, "The daemon's own service"). The daemon binds no socket; the platform's run directory (`/var/run` on a Mac) holds the `updater` directory of the registration-socket cleanup.

#### One daemon, run by launchd

On macOS the daemon is one launchd daemon run as root, the same `hostkeeper daemon` as on Linux.
Its label is `<REVERSE_DNS_PREFIX>.hostkeeper`, with `REVERSE_DNS_PREFIX` = `dev.hostkeeper`. The
maintainers must confirm the prefix before any Mac enrolls, since a label is the identity of a job.

| what | where |
|---|---|
| config | `/etc/hostkeeper/hostkeeper.toml` |
| stores config | `/etc/hostkeeper/stores.toml`, optional; without it the one store is `release_channel` (docs/design/packages.md section 7). On a managed Mac the daemon's start writes it from the managed preferences of `dev.hostkeeper.stores` when that domain is there, replacing a file that differs |
| macos-pkg signers | `/etc/hostkeeper/macos-pkg.toml`, optional, read at the start of a Mac only; on a managed Mac the daemon's start writes it from the managed preferences of `dev.hostkeeper.macos-pkg` when that domain is there, replacing a file that differs; a file that is not sound (a link, not root's, writable by others, not TOML of the signers) stops the start with the config exit status |
| state directory, packages directory | as on Linux (`/var/lib/updater/state`, `/var/lib/hostkeeper/packages`); both exist on macOS |
| run directory | `/var/run` (macOS has a read-only root and no `/run`); the daemon binds no socket there |
| units linked from a package | `/Library/LaunchDaemons/<label>.plist`, a link to `<packages>/<service>/current/units/<label>.plist` |
| the daemon's plist program | `/usr/local/bin/hostkeeper daemon`, which seeds the binary directory when `current` starts nothing and execs `/var/lib/updater/state/hostkeeper/bin/current` |
| the `hostkeeper` of the command line | `/usr/local/bin/hostkeeper` |
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

A step that is not one of the five of an update (the hook's `uninstall`, a `[hook] boot` step)
runs the same way, as one job: `<hook> <hook args> <step> --params <file>`, bounded by its
timeout (a timeout of 0 is no bound). The unit of a `[hook] boot` step is
`updater-<service>-boot-<generation>`, whatever the step's name, and its label
`<prefix>.hostkeeper.step.<service>.boot.<generation>`.

Hostkeeper's own update asks nothing of the new binary: a release is judged by its floor alone
(the README's self-update section).

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

The daemon places them as it places systemd units (design section 4.7, "Placed paths"): each
`units/<label>.plist` is linked at `/Library/LaunchDaemons/<label>.plist`, pointing at
`<packages>/<service>/current/units/<label>.plist`, and recorded in `placed.json`. A plist's label
must be the service's name or start with `<service>.`. A `run` label gets no `WantedBy=` link and
needs no `[Install]` section: launchd loads every plist of its directory at boot, so the placed
link is what enables the job. At each switch of an update (before `cutover`, and back before
`rollback`) and after a removal, the unit control (`hostkeeper_launchd::LaunchdUnitControl`) acts on
the links that are new, re-pointed (`current` moved, so the link resolves to another file) or gone
since its last reload: a job whose link is new or re-pointed is booted out if launchd holds it and
bootstrapped from its link, and a job whose link is gone is booted out. Loading a job does not
start it (the plist's `RunAtLoad` and `KeepAlive` decide that), so the hook finds both slots
loaded and starts the one it wants with `launchctl kickstart`. A job is stopped by its label: an
uninstall boots out each `run` label, then removes every placed link.

### Release and installer pkg

Every release ships macos-arm64 beside the two Linux platforms: the binary tarball
`hostkeeper-<v>-macos-arm64.tar.gz` with its SBOM, hostkeeper's own package `.tar.zst` (its
`manifest.toml` and `bin/hostkeeper`, and no `units/`), and the installer pkg
`hostkeeper-<v>-macos-arm64.pkg`. It also ships the packages of `hello`, the example service of
`examples/hello`, and of its failing build, for macos-arm64: `bin/hello`, `manifest.toml` and
`units/hello.a.plist` and `units/hello.b.plist`, the plists of its two slots rendered by
`hostkeeper_launchd::service_plist`; `hello` carries the same ad-hoc code signature as the
hostkeeper binary. The artifacts workflow builds them on a GitHub-hosted Apple
silicon runner, on each push to `main` and by hand, never on a pull request.

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

The file `hostkeeper-<v>-macos-arm64.pkg` is a distribution pkg: `productbuild` wraps the component
pkg that `pkgbuild` makes from the payload and the script, and the wrapper holds the one
component and nothing else (no installer script, no installation check, no volume check). The
installer runs the component's one script, a `postinstall` of two commands, as root once the
files are in place. The last one is the script's own status, which fails the install when the
job cannot be loaded:

```
#!/bin/sh
# The hostkeeper pkg's postinstall: load the daemon's launchd job, once any copy
# loaded before is booted out (which fails when none is loaded). The job's program,
# the binary the pkg installed, puts itself in the binary directory of hostkeeper's
# own update when current starts nothing and runs current, at each start.
'/bin/launchctl' bootout 'system/dev.hostkeeper.hostkeeper' 2>/dev/null
exec '/bin/launchctl' bootstrap system '/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist'
```

`launchctl bootout` unloads a copy of the job loaded before, and its status is ignored
because it fails when none is loaded. `launchctl bootstrap` loads the job, and `RunAtLoad`
starts it. The script runs the bootstrap with `exec`, so its status is the install's.

The script no longer seeds the binary directory or writes the config; the daemon's start does
both, each time it starts.

1. The seed (`hostkeeper_daemon::seed`, run by `hostkeeper_daemon::packaged` when the running
   binary is `/usr/local/bin/hostkeeper`): when `current` of the binary directory starts nothing,
   it stages the installed binary as its version (`versions/<version>`) and points `current` at
   it; when `current` starts a binary, as after a self-update, it changes nothing, so installing
   the pkg again never puts an older binary back over an update. A seed that cannot do its work
   ends the start with its message in `daemon.err.log`: the binary or the directory cannot be
   read or written, or a daemon holds the state directory's lock while `current` starts nothing.
   Then the start execs `current` with its own arguments.
2. The daemon's start writes the daemon config (`hostkeeper_daemon::mac_config::at_start`), on
   its `--config`, before it reads it, from the managed preferences file of the host
   (`HostFiles::managed_config`, which `HostFiles::for_platform` sets on a Mac and not on Linux). A config that is there
   as a regular file is kept: nothing is written and the managed file is not read. Anything else
   at the config path, a link or a directory, is left as it is and refused
   (`<path> is there but is not a regular file; it is not replaced`), since the daemon
   reads its config only from a regular file and not through a link. Otherwise it reads the managed preferences file, which the
   config profile of the MDM makes (see the farm's config profile in
   [hostkeeper-mdm.md](design/hostkeeper-mdm.md) section 4). It accepts that file only when root
   owns it, no one else can write it and it is a regular file (checked on the open file, so a link
   is refused). The file is a property list, binary or XML, whose top level is a dictionary. The
   start renders it as TOML and has the daemon's config loader judge that text, so a missing key,
   an unknown key or a bad value is refused with the loader's reason, as in a hand-written file.
   A date, a data value or a uid, and an integer that does not fit 64 bits, has no form in the
   config and is refused naming its key. The config is then written atomically (a temporary file in
   the config's directory, synced, hard-linked to the config path, which fails when a config
   appeared meanwhile; that one is judged as above), as root's file of mode 0600, and its directory is made
   with mode 0755 when it is missing. The file starts with a comment that names the managed
   preferences domain `dev.hostkeeper.config`. With neither a managed file nor a config, it
   fails with a message that names both paths.
   Right after the config, the start renders two more domains to their files
   (`hostkeeper_daemon::mac_files`), each judged alone. The managed preferences of
   `dev.hostkeeper.stores` (`HostFiles::managed_stores`, `/Library/Managed Preferences/dev.hostkeeper.stores.plist`)
   become `/etc/hostkeeper/stores.toml`, and those of `dev.hostkeeper.macos-pkg`
   (`HostFiles::managed_macos_pkg`) become `/etc/hostkeeper/macos-pkg.toml`. A domain that is not
   there changes nothing: the operator's file stays and none is made. A domain that is there is
   read and rendered as above, judged by the file's own loader (the stores loader, which also
   reads their credential files, or the signers loader) before anything is written, and refused
   when the file would be larger than that loader reads; the file starts with a comment naming the
   domain. Unlike the config, the managed preferences are these files' source: a file that holds
   exactly the rendered text, root's with mode 0644, is left alone, and any other is replaced (a new file in the same directory, mode 0644, synced and renamed over it), so a
   rotation the MDM pushes takes effect at the next start. A link or a directory at the file's
   path is refused and left. Any refusal stops the start with exit status 3, naming the managed
   file and the reason.

A config that cannot be made at a start (no managed file, a link at the config path, a managed
file the loader refuses) stops the start with exit status 3, the message in
`/var/log/hostkeeper/daemon.err.log`, and launchd starts the job again, which stops again. So a
pkg installed on a Mac with neither a config nor the managed preferences installs, loads its job,
and the daemon refuses each start, naming the missing file in `daemon.err.log`; the install does
not fail. The MDM gate installs the managed preferences before the pkg, so this is a hand install
done out of order.

The manual path, for a Mac without an MDM: place `/etc/hostkeeper/hostkeeper.toml` first, then
install the pkg. The daemon's start keeps that file, and the postinstall loads the job.

The pkg carries no installer signature yet. Which identity will sign it, a farm installer
identity whose root the MDM installs or an Apple Developer ID Installer identity, is decided by
the device test of the MDM design (section 7.2, task M11), whose kit and runbook are
[operations/pkg-signing-test.md](operations/pkg-signing-test.md). It is attested like every other asset, and the binary in it
keeps the ad-hoc code signature the linker gives it, which the package verification above accepts.
`cargo xtask release macos-pkg` builds the component with `pkgbuild` from the binary, `gh` and the
trusted root (`--binary`, `--gh`, `--trusted-root`), then runs `productbuild --package` over it
with the pkg identifier and the version and no signature; only the product goes to `--out`, and
the digest line is its digest. `cargo xtask release check-macos-pkg` reads back the product
archive that `pkgutil --expand` expanded, layer by layer.

- The outer layer holds exactly the file `Distribution` and the directory `hostkeeper.pkg`, no
  link in place of either, and no other entry. `Distribution` must have the root element
  `installer-gui-script`; every `pkg-ref` with an `id` has the identifier `dev.hostkeeper.hostkeeper`
  and none lacks one; exactly one `pkg-ref` names the file `#hostkeeper.pkg` and carries the
  release version; a `product` element has the identifier and the version; the `onConclusion`
  attribute of that reference, when present, is `none`; and it holds no `script`,
  `installation-check` or `volume-check` element.
- The component directory `hostkeeper.pkg` is read as the whole pkg was before the wrapper: the
  files `Bom`, `PackageInfo` and `Payload`, and the directory `Scripts` that `pkgutil` expands the
  pkg's scripts into, against the same three files. It refuses a missing part and any other file,
  mode, owner or bytes, any script but that `postinstall` (a regular file of mode 0755) with exactly
  the bytes above, a plist whose program is not `/usr/local/bin/hostkeeper` or whose output
  paths are not the daemon's two log files, and a newsyslog file whose entries name other paths
  than those two.

Since no pull request runs the macOS build, the unit tests of the check read fixtures, not what
`pkgbuild`, `productbuild` and `pkgutil` really make. A pull request that changes `crates/xtask/src/release/macos_pkg*`,
`crates/hostkeeper-launchd/` or `.github/workflows/artifacts.yml` therefore links, before it
merges, a green run of the artifacts workflow started by hand on its branch (`gh workflow run
artifacts.yml --ref <branch>`), which builds, expands and checks the pkg on the macOS runner and
signs and publishes nothing.

hostkeeper's own package has no unit on a Mac: the manifest's launchd labels are the service name
or the service name, `.` and more, and the daemon's label is not, so the daemon's job is the
pkg's plist.

### Boot id

The daemon's clock reads the boot id through a port, `hostkeeper_steps::BootIdSource`. On a Mac
it is `hostkeeper_launchd::SysctlBootId`: the `kern.bootsessionuuid` sysctl, a UUID the kernel
makes at each boot, accepted only as a UUID (`hostkeeper_launchd::parse_boot_session_uuid`), so a
boot id is never empty. One that cannot be read or parsed is an error, read as "the host may have
rebooted", never as the same boot: a stamp of an unknown boot measures no elapsed time (the step
runner begins such a window again once, when it first meets it, then counts it on its own
process's monotonic clock); the start pass reads the step records as after a boot, alerts
`journal_unavailable`, writes no `.boot-id` and runs no hook. A stamp of an unknown boot is stored with an empty `boot_id`, the value
older releases wrote on a Mac, so the on-disk format is unchanged.

### The device run

The job `macos-device` of `.github/workflows/privileged.yml` runs on a GitHub-hosted arm64 macOS
runner (`macos-15`) after every merge to `main` and by hand, as root, and in
`.github/workflows/pr-device.yml`, a copy of it key for key, on every pull request that changes a
path the macOS jobs build or read (CONTRIBUTING.md has the path rule), beside the job `macos-test`, which runs the tests of the Mac's crates on the same runner (CONTRIBUTING.md
says which).
Its tests (`crates/hostkeeper-privileged/tests/device_mac.rs`, `device_mac_e2e.rs`,
`device_mac_selfupdate.rs` and `self_update_across_releases.rs`) are not
counted by the coverage gate; what they drive is measured against fakes on Linux. They:

- start the daemon as a launchd job of the system domain, with a config and directories of their
  own, read the Mac's boot id in the `.boot-id` its start
  pass wrote;
- run the fixture hook in mode `sleep` as a step job through `LaunchdLauncher` with a bound of 5
  seconds: the job is booted out at the bound, the step has no exit code (a failed step), and
  launchd no longer holds the job;
- read a running job, an ended job and an unknown label through the observer's service manager:
  active, inactive, not found;
- read the boot id from `kern.bootsessionuuid` twice: a UUID, the same on both reads;
- check the process identity against real processes and print what `proc_pidpath` and the image
  lookup give when the file is unchanged, renamed over, unlinked, started through a link, at a
  path longer than `PROC_PIDPATHINFO_MAXSIZE`, and run by another user;
- check the package check's verdicts on real files: an ad-hoc signed copy of `/usr/bin/true` is
  valid, one without its signature is unsigned, one with a changed byte is invalid, and a file
  given `com.apple.quarantine` with `xattr -w` is quarantined;
- update hello's macOS package end to end (`device_mac_e2e.rs`): the real coordinator, in the
  test's process, orders each version, and the daemon, a launchd job, fetches it from a release
  channel the test serves on loopback, checks its attestation with the pinned `gh` against a test
  trust root, unpacks it with the checks above, links its plists and runs hello's hook in step
  jobs. The package is the released one with the test's paths and port, and with a `bin/hello`
  that answers `/version` with the version it was installed as. The test asserts from
  `/Library/LaunchDaemons`, the package directory, `launchctl print` of the slots' jobs, the
  daemon's `Status` and the coordinator:
  - install `1.0.0`: both plists are links into `current` and resolve to `1.0.0`'s, launchd holds
    both jobs loaded from them, `hello.a` serves `1.0.0`, and `Status` reports hello live and
    healthy with the running executable confirmed as `1.0.0`'s;
  - update to `1.1.0`: `current` is `1.1.0`, both jobs were booted out and bootstrapped again from
    `1.1.0`'s plists and run its program, `hello.b` serves it, and `Status` confirms it;
  - update to `1.2.0`, whose server answers `/readyz` with 503: the update fails in
    `gate_release`, reason `not_ready`, `current` is `1.1.0` again, both jobs are loaded from
    `1.1.0`'s plists and none from `1.2.0`'s, `hello.b` serves `1.1.0`, and the coordinator marks
    `1.2.0` bad;
  - uninstall: both plists are gone, launchd holds neither job, and hello's package directory and
    its paths in `placed.json` are gone.
- update hostkeeper itself under launchd (`device_mac_selfupdate.rs`, the Mac twin of the Linux
  `self_update.rs`): the daemon, a launchd job of the daemon's own plist from `current` of its
  binary directory, is ordered to the next version with releases the test seals in its package
  store and a coordinator of files the test writes:
  - an installed daemon ordered the update: it links `next` at the new binary and exits, launchd
    starts the job's program, the old binary execs the new one, which finishes the switch
    (`current` the new version, `previous` the old one), settles the update `Succeeded` and
    reports it;
  - a daemon the pkg installed (no package of hostkeeper) ordered the install of the next
    release: the same, and the package is installed at the new version;
  - a daemon the pkg installed ordered the install of the version it runs: `Succeeded` at once,
    with the same process;
  - a next binary that cannot start: the old binary execs it three times, then settles the update
    `Failed` (`self_update_fallback`), lists the version bad and runs, with `current` never
    moved;
  - the real coordinator in the test's process, answering each heartbeat the daemon wrote (its
    rollout asks two healthy heartbeats of a host at its version), orders the update: the daemon
    restarts into the next version, the new binary reports the update's `Succeeded` under the
    coordinator's order id and its own job running, healthy and at the new version, and the
    coordinator reads the host baked and hostkeeper converged.
- update hostkeeper itself from the published release `previous-release.toml` pins to this build
  (`self_update_across_releases.rs`, the cross-release rig, which runs on Linux too; CONTRIBUTING.md
  says how the pin moves): the released daemon is the job `dev.hostkeeper.hostkeeper` of the
  daemon's own plist in `/Library/LaunchDaemons`, ordered by the real coordinator with packages
  from the loopback release channel. The pin is release 0.1.194, which hands over: it updates to
  this build by the hand-over, and this build settles the update `Succeeded`; a version of this
  build that cannot start is fallen back from by the released binary's hand-over (three tries,
  then `Failed`, `self_update_fallback`), with no fallback job armed or removed; and this build
  updates to itself under a newer version name by the hand-over.
