# What Update finds on Ubuntu build hosts, before its first run

The first thing maintenance delivers is the one-button Linux update. Before that button was pressed
on real hardware, the due lists of six Linux build hosts were read and compared with the code that
would act on them. This page records what the lists hold, what an operator should expect Update to
do with each, and three gaps that stop Update from finishing on such hosts until they are fixed. The
procedure is [maintenance.md](maintenance.md); the design is
[maintenance-updates.md](../design/maintenance-updates.md), section 8. Every read here was
`hk hosts get`, as an operator; no host was changed.

## How the reports arrive

A daemon announces `maintenance-v1`, and sends its report, only after the coordinator advertises the
feature in a heartbeat answer. A coordinator from 0.1.431 on (komira-ai/hostkeeper#513) advertises
it to every gRPC heartbeat. Once the coordinator runs that release, each host announces the feature and
sends its first look at its next heartbeats, on the daemon release it already runs: the daemons
need no update for it. The look runs when the report is first asked for, so the first
heartbeat after the advertisement carries no due list yet, and the next one does.

## The due lists

The six hosts are called A to F here. "New" means the package is not installed yet (the item has
no `from`).

| Host | OS, running kernel / newest installed | Due |
|---|---|---|
| A | Ubuntu 26.04, 7.0.0-44 / 7.0.0-44 | nothing |
| B | Ubuntu 24.04, 7.0.0-43 / 7.0.0-43 | `linux`: libdrm-amdgpu1 to 2.4.125-1ubuntu0.1~24.04.2 (security, new); linux-image-6.8.0-151-generic to 6.8.0-151.151 (kernel, new, needs reboot) |
| C | Ubuntu 24.04, 7.0.0-42 / 7.0.0-43 | `reboot`: kernel (7.0.0-42 running), reboot-required, needrestart; `linux`: the same two as B |
| D | Ubuntu 24.04, 7.0.0-43 / 7.0.0-43 | `linux`: libdrm-amdgpu1 (security, new); python3-jwt 2.7.0-1ubuntu0.4 to 2.7.0-1ubuntu0.5 (security); linux-image-6.8.0-151-generic (kernel, new, needs reboot) |
| E | Ubuntu 24.04, 7.0.0-42 / 7.0.0-43 | as C |
| F | not read | look failed: `apt: apt-get update failed with exit code 100: E: Could not get lock /var/lib/apt/lists/lock. It is held by process <pid> (apt-get)` |

The Ubuntu 24.04 hosts run the 7.0 hardware-enablement kernel. Their simulated `dist-upgrade` also
brings in two packages that are not installed: a new library, and a kernel image of the older 6.8
series.

## What Update would do with them today

Update on one host is one `Maintenance.Schedule` naming the host: the request the Update button on
the host's Updates tab sends (`updateRequest` in `ui/schedule.js`).

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance schedule --host-ids <host-id> --confirm true --reason "<reason>"
```

Read from the code against the lists above, as built:

- **D**, which has only `linux` items: stage, then drain (js-agent cordons the host and waits for
  its leases), then the apply installs python3-jwt alone, with no restart. The check then fails with
  `check: exit 1, update_due: libdrm-amdgpu1 2.4.125-1ubuntu0.1~24.04.2 is still due`. The host
  halts cordoned until **Put back** (gap 1).
- **B**: the same, except that the apply has nothing to install.
- **C** and **E**: deferred at the stage with `unsupported_kind`, because their lists hold `reboot`
  items (gap 2, fixed since: their stage now passes, and the turn goes on as D's, with one reboot).
- **F**: nothing, until its next look (gap 3).

So no first run should be made on a serving host until gaps 1 and 2 are fixed. A test host may be
used to see the halt and Put back on hardware.

## The three gaps

1. **The security scope's apply and its check disagree on new packages.** Under the default
   `security` scope the apply names only installed packages (`security_names` in
   `crates/hostkeeper-daemon/src/maintenance_linux/apt.rs`). It relies on a meta-package it names to
   pull a new package in. The look, and the check after the apply, report new packages as due. These
   hosts' lists hold two new packages and no meta-package, so the check finds them still due and
   halts with `update_due`. [maintenance.md](maintenance.md#not-built-yet) names this until it is
   fixed. A related open question: a 6.8 kernel image is due, and marked as needing a reboot, on
   hosts booted from 7.0. Installing it changes neither the running kernel nor the newest one.
2. **A pending kernel makes Update defer.** The `reboot` kind's stage still answers
   `unsupported_kind` (`RebootKind::prepare` in
   `crates/hostkeeper-daemon/src/maintenance_linux/kinds.rs` returns the `Unsupported` stub of
   `maintenance_hook/kind.rs`). Update includes `reboot` whenever a
   host reports a restart it needs, so every host with a kernel installed and not running defers.
   The in-process e2e (`crates/hostkeeper-daemon/tests/maintenance_linux_e2e.rs`) does not show
   this, because its launcher answers the stage itself. A `reboot`-only run cannot be made until it
   is fixed. **Fixed** (komira-ai/hostkeeper#524): the `reboot` kind's stage succeeds and runs no
   program, and the e2e runs the real hook's stage, with a host that has both a `linux` and a
   `reboot` item due going through Update to done.
3. **A look fails on a held apt lock.** The look's `apt-get update` waits for no lock, unlike the
   hook's stage and apply, which wait up to the daemon's `--package-lock-wait-secs` (300 by
   default) for each program they run. A look that collides with another `apt-get` (the
   repository installer's, or unattended-upgrades') fails, and the host shows no due list until its
   next look, an hour later.

Each is to be fixed in its own pull request, with a test that fails before the fix. Once gaps 1 and
2 are released, the first run is the command above on one host whose drain moves its work elsewhere.
That host should not be the coordinator's or its database's, and it should be in a group where
`min_serving` other hosts still serve.
