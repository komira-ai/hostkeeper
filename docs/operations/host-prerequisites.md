# Host prerequisites: the Command Line Tools on a Mac

How a Mac gets Apple's Command Line Tools for Xcode (the compiler, the linker and the SDK every
macOS build needs) with no command typed on it ([design/host-prerequisites.md](../design/host-prerequisites.md)).
The coordinator holds one file that lists the host prerequisites of each host group; each host's
daemon, which runs as root, makes sure of the ones that apply to its platform. No password, no
account name and no sudoers rule is involved, and nobody logs in to the Mac.

## Give the coordinator the file

Write the file, one table per host group:

```toml
[host-groups."mac-build"]
prerequisites = ["xcode-command-line-tools"]
```

Name it in the coordinator's flags (`host-prerequisites-file = "/etc/hostkeeper-coordinator/host-prerequisites.toml"`
in `serve.toml`, or `--host-prerequisites-file` on its command line) and restart it once. From then
on the coordinator reads the file again at every tick of `--tls-reload-secs` (60 s by default): edit
it in place and every host of a changed group has the new set within a heartbeat after that tick.
The file holds no secret and needs no particular mode. At start a file that breaks a rule stops the
coordinator with a message naming `--host-prerequisites-file`, the group and the place; on a later
tick it is logged as a warning and the sets in use stay.

Rules: the only name this release knows is `xcode-command-line-tools`; at most 16 names a group,
none twice; the file is at most 64 KiB and holds nothing but `host-groups`, and a group's table
nothing but `prerequisites`. A group the file does not list needs nothing, and so does a host in no
group. Without the flag the coordinator sends no set and every daemon keeps what it holds.

A prerequisite is never removed. A set without it, a group taken out of the file, or a host moved
to another group leaves the Command Line Tools on the Mac.

## What each host does

| Host | What the daemon does for `xcode-command-line-tools` |
|---|---|
| Mac | looks with `pkgutil` and `xcode-select`, and, with no receipt, for the tools' compiler on disk or a selected Xcode whose licence is accepted and whose first launch ran (`xcodebuild`); when it finds none of these, installs the tools with `softwareupdate` as root; looks again every hour and after every start of the daemon, and installs it again when it is gone |
| Linux (systemd or OpenRC) | holds the set, reports the name `NOT_APPLICABLE`, and runs nothing |

The install makes the marker file `/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress`,
which makes `softwareupdate` offer the tools, installs the highest version it lists, and removes the
marker again whatever happened. Each program runs with an empty environment, no input and a
deadline. The install downloads from Apple, so the Mac needs a route to it.

## Check a host

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts get --node-id <id>
```

`host.heartbeat.prerequisites` is the daemon's last report: `digest` of the set it holds, `refused`
when it refused the last set sent, and one entry in `states` for each name, whose `seenUnix` is
when the daemon last looked at the host for it (absent before it first did).

| `status` | It means |
|---|---|
| `PENDING` | the daemon holds the name and has not looked at the host for it yet |
| `PRESENT` | the host has it; `version` is the version `pkgutil` gives, empty for tools on disk without their receipt, or the selected Xcode's (`Xcode 16.1`) |
| `INSTALLING` | the host did not have it and the daemon is installing it now: the listing, the install and the look after it, each with its deadline, at most about two hours and twenty minutes |
| `FAILED` | the last look or install failed: `reason` names the step, its exit code and the first line of the program's error (a listing that exited non-zero or offered no tools also the macOS version and build, the first line of each stream and the labels holding `Command Line Tools`), `failures` counts the tries in a row and `nextTryUnix` is when the daemon tries again |
| `NOT_APPLICABLE` | the name is of another platform than the host's (the Command Line Tools on Linux); nothing is done |
| `UNKNOWN` | the daemon does not know the name (a newer coordinator's); nothing is done |
| `prerequisites` empty (`{}`) | the daemon holds no set: the coordinator was not given the file, or has not sent it since the daemon started |
| no `prerequisites` | the daemon predates the feature, or the heartbeat came over the JSON route |

A `refused` entry means the daemon will not take that set: `reason` says whether the digest was not
the names' or a rule of a set was broken. The daemon keeps the set it held; fix the file and the
next set is taken.

The daemon's log has a line for every failure and for every install.

## The alert `prerequisite_failed`

The coordinator raises `prerequisite_failed` for a host while its last report holds a `FAILED`
state, and it clears when the report no longer does. See it with `hk alerts list`. What to do:

1. Read `reason` and `failures` in `hosts get`. A Mac that cannot reach Apple's update servers, or
   that has no disk space for the download, fails every time until that is fixed.
2. Nothing needs doing on the Mac to start another try: the daemon tries again at `nextTryUnix`. It
   waits 30 minutes after the first failure, doubles the wait after each next one, and never waits
   more than 24 hours. A restart of the daemon (an update of hostkeeper, a restart of the Mac) looks
   once more at once.
3. The count and the wait are kept in memory, so a restart is the way to make the daemon try now.

## A reinstalled Mac

A Mac erased and set up again gets hostkeeper from its Blueprint ([mac-bootstrap.md](mac-bootstrap.md)),
enrolls and is approved into its host group ([enrollment.md](enrollment.md), "The Mac path"). At
its first heartbeat after the approval it is sent the group's set, finds the Command Line Tools
missing, installs them and reports `PRESENT` with the version. A build on the Mac then finds `cc`,
`xcrun` and the SDK.
