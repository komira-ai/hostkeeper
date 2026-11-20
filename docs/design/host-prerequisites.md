# Host prerequisites

Status: built. The coordinator side is `crates/hostkeeper-coordinator/src/prerequisites.rs`, the
daemon side `crates/hostkeeper-daemon/src/prerequisites.rs` (with `xcode_clt.rs`), the rules both
read a set by `crates/hostkeeper-wire/src/prerequisites.rs`, and the wire
`HeartbeatResponse.prerequisites` and `HeartbeatRequest.prerequisites` of `hostkeeper.agent.v1`
(feature `prerequisites-v1`).

Extended, in design only, by [`maintenance-updates.md`](maintenance-updates.md) section 1.2: the failure reason that says why `softwareupdate` offered no tools, and a look that finds the tools on disk without their receipt or in a selected Xcode. This document describes what is built.

The owner's direction: a Mac of the build farm must not need a person to type a command on it.
Apple's Command Line Tools for Xcode (the compiler, the linker and the SDK every macOS build
needs) are not a package of any repository hostkeeper installs from, and until now reached a Mac
only by an operator who logged in and ran `softwareupdate` under `sudo`, typing the account's
password. hostkeeper is the one root daemon on every Mac; it now installs them, as root, with no
password and no sudoers change, when the Mac's host group says it needs them.

## 1. What changes

**Before.** A Mac that needs the Command Line Tools gets them by hand: an operator logs in over
SSH and runs the installer through `sudo`, which asks for the account's password, or first gives
the account a passwordless `sudo` rule so that a script can.

**After.** The coordinator is given one file, `--host-prerequisites-file`, that lists the host
prerequisites of each host group. Every daemon that runs on a host of a group is sent that group's
set and makes sure of each prerequisite of it that applies to its platform: on a Mac of a group
that lists `xcode-command-line-tools`, the daemon installs the Command Line Tools when they are
missing, headless, as root, and reports in every heartbeat whether the Mac has them and which
version. It looks again every hour and after every start (a macOS update restarts the Mac, and so
the daemon), and installs them again when they are gone. A failed install is tried again later and
later, never in a loop, and raises an alert until it succeeds.

**What does not change.** No password, no account name and no sudoers rule is involved anywhere:
the daemon already runs as root under launchd. A prerequisite is never removed: a set without it,
or a host moved to a group without it, leaves the host as it is. A Linux host is sent its group's
set like any host and does nothing for a prerequisite of macOS. Packages, their installs, the
rollouts and every other part of the heartbeat are as before.

## 2. The coordinator's file

```toml
# One table per host group; a group that is not listed needs no prerequisite.
[host-groups."mac-build"]
prerequisites = ["xcode-command-line-tools"]
```

- `prerequisites` is an array of names this coordinator knows: today only
  `xcode-command-line-tools`. A name it does not know is refused, naming the group, the name's place
  and the names it knows. At most 16 names a group, none twice. A group name follows the host
  group rule. The file is at most 64 KiB and holds nothing but `host-groups`; a group's table holds
  nothing but `prerequisites`.
- A refusal names the flag, the group and the place; never the path.
- The file is not a secret and needs no particular mode. It is read at start (a refused file stops
  the start, naming the flag) and again at every tick of `--tls-reload-secs` (60 s by default): a
  changed file is in use from that tick on; a file that cannot be read or is refused then leaves
  the sets in use and is logged as a warning.

Without the flag the coordinator sends no set, and every daemon keeps what it holds.

## 3. The wire

`prerequisites-v1` is announced by every daemon of this release and advertised by every
coordinator of it. A coordinator that was given the file answers a daemon that announced it with
the host's set (`HostPrerequisites`: the names and their SHA-256 digest,
`hostkeeper_wire::prerequisites::digest`), until the daemon's report (`PrerequisitesReport`) holds
that digest as the set it holds, or as the set it refused. The set is the host group's, or the
empty set when the host is in no group or its group is not in the file. A daemon that does not
announce the feature (every release before this one) is sent nothing; the JSON route never carries
it. The fields are additive (`crates/hostkeeper-proto/tests/additions_prerequisites.rs`).

The report is sent in every heartbeat: the digest of the set the daemon holds (empty after a start
until the first set arrives, so every start takes the set again), the last set it refused with the
reason, and one `PrerequisiteState` for each name of the held set: its status (`PENDING`,
`PRESENT`, `INSTALLING`, `FAILED`, `NOT_APPLICABLE`, `UNKNOWN`), the version when present, when
failed the reason, the count of failures in a row and the Unix time of the next try, and the Unix
time the daemon last looked at the host for it (`seen_unix`). The
coordinator keeps the last report of each host in memory and shows it in the host's heartbeat
(`hk hosts get --node-id <host>`: `host.heartbeat.prerequisites`).

A daemon reads a name it does not know (a newer coordinator's) as `UNKNOWN` and does nothing for
it, so a set is never refused for a name alone; a coordinator keeps a status value it does not know
as it came.

## 4. The daemon

**Which set.** The daemon holds the last set it was sent (which clears a refusal), after checking that the digest is the
digest of the names and that the set keeps the rules of a set (at most 16 names of the grammar,
none twice); a set that fails either is *refused*: the report names it and why, and the daemon
keeps the set it held. A set whose digest is the one held changes nothing. When a new set is held,
a prerequisite that was in the old one keeps its state (so a set sent again never resets a
failure's wait); a new one starts `PENDING`; a name of another platform is `NOT_APPLICABLE` and a
name the daemon does not know `UNKNOWN`, and neither is ever looked at.

**When it looks.** The heartbeat loop asks the daemon for its report at every heartbeat; that is
also when the daemon starts the work that is due, on a worker thread of its own, one pass at a
time, so a heartbeat never waits for an install. A pass is due when a prerequisite that applies is
`PENDING`, when one is `PRESENT` and was last looked at an hour or more ago, or when one is `FAILED`
and the time of its next try has come.

**The Command Line Tools** (`xcode-command-line-tools`, on a Mac):

1. *Look.* `pkgutil --pkg-info=com.apple.pkg.CLTools_Executables`: exit 0 means the package is
   installed, and its `version:` line is the version reported; any other exit means it is not.
   When it is, `xcode-select -p` must exit 0; when it does not, `xcode-select --switch
   /Library/Developer/CommandLineTools` and `xcode-select -p` again must both exit 0. A Mac with
   the package and a developer directory is `PRESENT`. With no package, the Mac is `PRESENT` too
   when the tools are on disk without their receipt (`/Library/Developer/CommandLineTools/usr/bin/clang`
   is a file and `xcode-select -p` exits 0; the version is empty), or when the directory
   `xcode-select -p` names is an Xcode's (`<name>.app/Contents/Developer`) whose licence is accepted
   and whose first launch ran (`xcodebuild -license check` and `xcodebuild -checkFirstLaunchStatus`
   exit 0; the version is the first line of `xcodebuild -version`, such as `Xcode 16.1`).
2. *Install,* when the look finds none of these: the state is `INSTALLING`; the marker file
   `/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress` is made (never through a link),
   which makes `softwareupdate` offer the tools; `softwareupdate --list` names them in `* Label: `
   lines, as `Command Line Tools for Xcode-<version>` (older catalogs) or
   `Command Line Tools for Xcode <version>-<version>` (current ones, the last version being the
   tools'), and the label of the highest version is installed with `softwareupdate --install
   <label>`; the marker is removed whatever happened after it was made; then step 1 must find them.
   A listing that exits non-zero, or that names no label of the tools, fails with a reason that
   also names the macOS version and build (`sw_vers`), the first line of each of the listing's
   streams that is not blank, and every label that holds `Command Line Tools` in any case or, with
   none, every label it listed; each part is cut to its own bound and the whole to the 256 bytes
   the coordinator reads.
3. Each program is run as a child with an empty environment and no input, its output kept to
   64 KiB, and killed when it runs past its deadline: 1 minute for `pkgutil`, `xcode-select`,
   `xcodebuild` and `sw_vers`,
   15 minutes for `softwareupdate --list`, 2 hours for `softwareupdate --install`.

**Failure and the wait.** A look or an install that fails sets the state `FAILED` with the reason
(the step and its exit code, and the first line of the program's standard error), counts the
failure, and sets the next try: 30 minutes after the first failure, doubled after each next one,
at most 24 hours. Nothing is tried before then; a success counts the failures back to 0. Every
failure is a line of the daemon's log, and the coordinator raises the alert `prerequisite_failed`
for the host while its last report holds a `FAILED` state. The count and the wait are kept in
memory: a pass that ends with a prerequisite still `INSTALLING` (a panic of its worker) counts
as a failure with its wait. A daemon that starts again (after an update of hostkeeper, or a restart of the Mac) looks
once more at its first set.

**Linux.** A Linux daemon (systemd or OpenRC) announces the feature, holds the set and reports
each name `NOT_APPLICABLE` (or `UNKNOWN`), and runs nothing.

## 5. A reinstalled Mac of the build farm, with no command typed on it

1. The Mac is erased and set up again; device management enrolls it and installs the bootstrap
   pkg, which installs hostkeeper's pkg; the daemon runs as root under launchd
   (`docs/operations/mac-bootstrap.md`).
2. The daemon enrolls and is approved into its host group, `mac-build` say
   (`docs/operations/enrollment.md`, "The Mac path").
3. Its first heartbeat after the approval is answered with the group's prerequisites; the daemon
   finds the Command Line Tools missing, installs them, and reports `PRESENT` with the version.
4. A build on the Mac finds `cc`, `xcrun` and the SDK.

## 6. Tests

The ordinary tests (no root, no Mac, no network) drive the daemon's logic over scripted programs
that count their calls, an injected clock and a dispatcher that runs each pass at once: a Mac
without the tools runs the install's steps once and reports `PRESENT`; a Mac with them runs only
the look; a failed install reports `FAILED`, raises nothing more until its next try and waits twice
as long after the next failure; a Linux host runs nothing. The coordinator's tests cover the file's
parsing and reloading, the decision when to send, the report's rules, its showing in `Hosts.Get`
and the alert. The real `softwareupdate` is not run by any test: it downloads from Apple.
