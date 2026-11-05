# macOS

How the daemon is meant to run on a Mac. The code is the crate
[`hostkeeper-launchd`](../crates/hostkeeper-launchd) (design sections 4.2 and 5.9). It sits behind
ports that already exist and is tested against fakes on Linux. It has not been run on a Mac yet,
so this page describes what the code does, not a supported platform.

## One daemon, run by launchd

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

## A hook step is one launchd job

Each hook step is meant to run as one launchd job with the label
`<prefix>.hostkeeper.step.<service>.<step>.<generation>`. The job would run as root, for the timeout,
the kill and the capture of the step's output only. Its plist has no sandbox profile and no `UserName`, and it has no rights, because a hook runs as root from a
verified release (design 5.3 and 5.9). A step that is tried again keeps its label, so a resumed run finds and stops the same job.

## Service state

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

## Process identity

The process behind a unit is identified by its pid and start time, and the file it runs is found
with `proc_pidpath`. Its digest is taken from the file opened by that path, and the device and
inode of the path before and after the read must be the same, so a file replaced while it is hashed
is refused.

## Units in a package

A package's `units/` may hold `<label>.plist` files. A label is the service name or starts with
`<service>.`. The manifest's `[units] run` then names labels instead of systemd unit names.

## What is not done

The crate holds the labels and plists of a step job, but the launcher that runs a step as a job is
not built. The crate is code behind ports. Nothing runs it on a Mac, the linking of plists into
`/Library/LaunchDaemons` is not built, and the daemon does not select it yet.
