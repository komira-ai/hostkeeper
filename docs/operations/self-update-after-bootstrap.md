# hostkeeper's self-update on hosts the deb or the pkg installed

A host is bootstrapped by installing the hostkeeper deb, rpm or macOS pkg. That daemon runs, but
no package of the service `hostkeeper` is installed under its packages directory
(`/var/lib/hostkeeper/packages`), so its heartbeat lists no entry `hostkeeper`. This page says
how such a host reaches the desired release of hostkeeper, and what to do by hand for the daemons
that cannot get there by themselves.

## What the coordinator does

The coordinator takes a host whose heartbeat's `agent` names itself `hostkeeper` (every daemon
that announces `order-v1` does) as running the service `hostkeeper` at the agent's version,
listed or not (`crates/hostkeeper-coordinator/src/agent_service.rs`):

- the host counts among hostkeeper's hosts, at the version its binary runs; it is never `missing`
  for not listing the service;
- a host that lists no `hostkeeper` is ordered `install` of the desired release; a host that lists
  it is ordered `update`;
- the version counted is the agent's even when the host lists hostkeeper at another version (the
  package `current` names), so a host whose self-update fell back, or that installed a release
  it does not run, is counted at the version that runs.

## What the daemon does

A daemon of the release that carries this change, or a later one, runs an `install` of
`hostkeeper` as it runs an `update` of it: the release's steps, then the self-update follow-on,
which stages the release's binary, switches the binary directory's `current` and restarts into
it (design, section 6.6). One order brings a bootstrapped host to the desired release.

The release's hook is its own binary, `bin/hostkeeper update-hook`, which a daemon's step
launcher runs as `bin/hostkeeper <step> ...`; every step does nothing. Its manifest asks for a
daemon of 0.1.53 or later, so that the daemon that runs now, older than the release, takes it.

## Daemons before this change

They take an `install` of hostkeeper by the path every service is installed by: the package is
placed and its unit linked, and the binary that runs stays the one that ran. Such a daemon cannot
self-update to the release it installed: an `update` to it replaces no release, so the daemon
would run it and record it succeeded without restarting. The coordinator therefore does not order
it: the host is told nothing for that version, the rollout status gives `unsupported` as the
reason it is not told, and the service stays `in_progress`. The next release CI sets is ordered to
it as an `update`, which replaces the release installed and runs the self-update. So:

| daemon | platform | reached by |
|---|---|---|
| 0.1.53 to the last release before this change | Linux | two releases: the first one set after this change is installed, the next one is the self-update. Nothing by hand. |
| any | macOS | **not reached.** Exclude the Mac from hostkeeper and upgrade it by hand, below. |

A daemon that answered an earlier install of hostkeeper and never reported it (a service its
heartbeat does not list has its verdicts kept until it lists it) may hold that order's try in its
step record. The coordinator orders the next install one try past an order that went 120 seconds
(`ORDER_AGE`) with no word, so such a host takes the next order instead of refusing it
`try_conflict` for good.

## Macs

No Mac updates hostkeeper by itself yet, for two reasons:

- a daemon before 0.1.95 halts every package it installs at `prepare` as `artifact_changed`
  (`/var` is a link to `/private/var`), hostkeeper's included;
- on every release, the daemon's launchd job starts `/usr/local/bin/hostkeeper`, not the binary
  its self-update switched to ([macOS](../macos.md#self-update-fallback)), so a Mac restarts
  into the binary it ran, which records the update `Failed` after the commit
  (`self_update_fallback`) and the version bad. Such a failure counts against the release like
  any other, toward its automatic rollback.

Since the coordinator now orders every host whose agent is hostkeeper, including the Macs,
exclude each Mac from hostkeeper before CI sets a release this change carries, and upgrade it by
hand. Run the `ctl` lines on a workstation with an operator token for the coordinator, the others
on the Mac as root.

1. Take the Mac out of hostkeeper:

   ```
   hostkeeper ctl hosts exclude --service hostkeeper --node-id <node id> --reason "macOS: upgraded by hand"
   ```

2. Fetch the pkg of the release (`hostkeeper-<version>-macos-arm64.pkg`) and verify it as
   [Verify a release](../deploy/verify-a-release.md) says.

3. Install it, and restart the daemon's job into the binary it installed (the pkg has no
   scripts, so nothing restarts it):

   ```
   sudo installer -pkg hostkeeper-<version>-macos-arm64.pkg -target /
   sudo launchctl kickstart -k system/dev.hostkeeper.hostkeeper
   ```

4. Check that it runs and talks to the coordinator: `sudo hostkeeper admin status` prints
   `link.consecutive_failures: 0` after a heartbeat period, and
   `hostkeeper ctl hosts get --node-id <node id>` answers `heartbeat.agent.version` at the new
   version.

Keep the exclusion until a release starts a Mac's daemon from the binary its self-update
switched to.
