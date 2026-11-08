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

On a Linux host the deb or the rpm installed, that install meets two more things, which a daemon
handles from the release that carries the change that leaves the deb's enable link alone (the
first release after 0.1.100 that `hostkeeper --version` shows with that change) on:

- The deb enabled `hostkeeper.service` with its own `WantedBy=` link,
  `/etc/systemd/system/multi-user.target.wants/hostkeeper.service` to
  `/usr/lib/systemd/system/hostkeeper.service`, which `placed.json` does not list. The release
  runs the same unit, so its install claims that path. The daemon leaves a link like this, to a
  unit file of the same name, as it is and does not claim it: the unit is already enabled, and
  systemd starts `hostkeeper.service` from `/etc/systemd/system`, where the install links the
  release's unit. An earlier daemon refuses the install `path_conflict` before it writes anything.
- Before it stages the new binary, the daemon asks it for its self-update protocol in a transient
  unit. An earlier Linux daemon has no way to run that probe and refuses every self-update
  `protocol_unreadable` (komira-ai/hostkeeper#102). A macOS daemon runs the probe as a launchd
  job from the release that carries that change on; an earlier one refuses every self-update
  `protocol_unreadable` the same way, even an install of the version it already runs. The probe
  is run by the daemon that runs, not by the release, so such a Mac is brought once, by hand, to
  that release or a later one (stage its binary in the binary directory, switch `current` to it,
  and `launchctl kickstart -k system/dev.hostkeeper.hostkeeper`) before it is included again.

A verdict of a service the heartbeat does not list, such as the refusal of an install that wrote
nothing, is sent in the next heartbeat. The service is listed `absent` at the order's target with
that verdict until the coordinator acknowledges it, and with no verdict after that, which the
coordinator reads as still reporting the one it acknowledged. So the coordinator hears how the
order ended, the order no longer holds the update slot, and a refused version is not ordered to
the host again. The entry goes once the service is listed; a restart of the daemon also drops
it, and the coordinator may then order the install once more. An earlier daemon keeps such a
verdict unsent until the service is listed, which a refused install never is.

The daemon that runs a self-update exits before its verdict (`Deferred`, `awaiting_restart`)
reaches a heartbeat, and the coordinator ends the order once the host runs its target, so it never
orders it again. The next binary reports the verdict instead: the daemon that ran the order keeps
the order's id in its step record, the settlement keeps it beside the settled verdict (the new
binary's `Succeeded`, or after a fallback the `Failed` `self_update_fallback`, whether the
fallback unit or job or the previous binary's start settled it), and every start of the daemon
sends that verdict under that id and try, from its first heartbeat until the coordinator
acknowledges it; the record then stops keeping it. The coordinator then holds it as
the host's last update of hostkeeper, in place of an older verdict. A daemon keeps the id from the
release that carries that change on, so a self-update an earlier daemon ran reports no verdict,
and the coordinator keeps showing the host's older verdict until hostkeeper's next self-update.
That holds no rollout: the bake and the convergence read the health the running binary reports.
From the same release on, a Mac's daemon reports its own service as its launchd job
`dev.hostkeeper.hostkeeper`, live, healthy and at the version it runs; an earlier one reports it
`unmonitored` and unhealthy, which keeps the host from baking, so a Mac is brought to that release
by its last self-update from an earlier daemon (or by hand) and bakes from then on.

So a Linux daemon from before that release, installed by the deb, cannot update itself to any
release. Bring it once, by hand, to that release or a later one the way the host's packaging
upgrade does: stage that release's binary in the binary directory, switch `current` to it, and restart
`hostkeeper.service`. From that release on, the coordinator's install, and every update after it,
runs as the self-update. Until then, hold or exclude the host for the service `hostkeeper`.

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
| 0.1.95 or later, its launchd job the plist of a pkg whose program is `current` (below) | macOS | as on Linux. |
| any other | macOS | **not reached.** Exclude the Mac from hostkeeper and install the pkg by hand once, below. |

A daemon that answered an earlier install of hostkeeper and never reported it (a daemon from
before that release keeps the verdicts of a service its heartbeat does not list until it lists
it)
may hold that order's try in its step record. The coordinator orders the next install one try past an order that went 120 seconds
(`ORDER_AGE`) with no word, so such a host takes the next order instead of refusing it
`try_conflict` for good.

The daemon also sends the try its step record holds for each service in every heartbeat, so a
coordinator that restarted after a self-update's install, and so never heard that install's
verdict, orders the next update past it. A daemon of an earlier release does not send it; such a
host may refuse the coordinator's first update `try_conflict` (or `stale_try`). The coordinator
reads either refusal as a disagreement on the try, not as the host failing the version: it orders
the host again past the refused try in the same answer, and the host is neither failed here nor
holds back its stage. A host a coordinator of an earlier release left failed here on such a
refusal is ordered again once the coordinator runs this release.

## Macs

A Mac reaches the desired release of hostkeeper by itself, as a Linux host does, once two things
hold:

- its daemon is 0.1.95 or later: an earlier one halts every package it installs at `prepare` as
  `artifact_changed` (`/var` is a link to `/private/var`), hostkeeper's included;
- its launchd job is the plist of a pkg whose program is `current` of the binary directory,
  `/var/lib/updater/state/hostkeeper/bin/current` (`sudo launchctl print
  system/dev.hostkeeper.hostkeeper` names the program). That job starts the binary the
  self-update switched to ([macOS](../macos.md#self-update-fallback)), and the pkg's
  `postinstall` points `current` at the installed binary on a first install. The plist of an
  earlier pkg starts
  `/usr/local/bin/hostkeeper` instead, so after each self-update the Mac restarts into the binary
  it ran, which records the update `Failed` after the commit (`self_update_fallback`) and the
  version bad. Such a failure counts against the release like any other, toward its automatic
  rollback.

A self-update never changes the plist (hostkeeper's own package has no unit on a Mac), so a Mac
installed from an earlier pkg keeps that plist until the pkg is installed again by hand. Do it
once on each such Mac, with a pkg whose plist starts `current`; keep the Mac excluded from
hostkeeper until it is done. Run the `hkctl` lines on a workstation with an operator token for the
coordinator, the others on the Mac as root.

1. Take the Mac out of hostkeeper, if it is not out already:

   ```
   hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
       hosts exclude --service hostkeeper --node-id <node-id> \
       --reason "macOS: pkg installed by hand"
   ```

2. Fetch the pkg of the release (`hostkeeper-<version>-macos-arm64.pkg`) and verify it as
   [Verify a release](../deploy/verify-a-release.md) says.

3. Stop the daemon's job, so launchd drops the old plist and the daemon releases the state
   directory's lock, which the pkg's `postinstall` takes:

   ```
   sudo launchctl bootout system/dev.hostkeeper.hostkeeper
   ```

4. Remove `current` of the binary directory, if it is there. A self-update the old plist could not
   start into left it naming the version that update staged, which the Mac recorded bad; without
   it the `postinstall` points `current` at the binary the pkg installs:

   ```
   sudo rm -f /var/lib/updater/state/hostkeeper/bin/current
   ```

5. Install the pkg. The installer runs the `postinstall`, which seeds `current`, keeps the
   `/etc/hostkeeper/hostkeeper.toml` the Mac has (the pkg never replaces it) and loads the job. It
   fails the install with the message of the command that failed: the seed's if it could not give
   `current` a binary, `mac-config`'s if the Mac has neither that file nor a managed config.

   ```
   sudo installer -pkg hostkeeper-<version>-macos-arm64.pkg -target /
   ```

6. Check that it runs the pkg's binary through `current` and talks to the coordinator:
   `sudo launchctl print system/dev.hostkeeper.hostkeeper` names the program
   `/var/lib/updater/state/hostkeeper/bin/current`, `readlink` of that path names
   `versions/<version>`, `sudo hostkeeper admin status` prints
   `link.consecutive_failures: 0` after a heartbeat period, and
   `hkctl hosts get --node-id <node id>` answers `heartbeat.agent.version` at the new
   version.

7. Put the Mac back into hostkeeper; its next self-update starts the new binary:

   ```
   hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
       hosts include --service hostkeeper --node-id <node-id>
   ```
