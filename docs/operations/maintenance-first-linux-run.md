# The first real Linux maintenance run on the farm

This note records the first maintenance run on a real Linux build node. It covers the release that
turned maintenance on, the due lists the six all-in-one nodes reported, what one press of Update did
on one node, and what surprised us. The procedure is [maintenance.md](maintenance.md). The design is
[maintenance-updates.md](../design/maintenance-updates.md), section 8. Every time is UTC, on
2026-11-21. Every read was `hk hosts get`, `hk maintenance get` or `hk maintenance list`, run as an
operator. No shell was taken on any node.

## 1. The release

| Time | What |
|---|---|
| 19:42 | komira-ai/hostkeeper#513 merged to main as 267fac8e. The coordinator now offers `maintenance-v1` to every gRPC heartbeat. |
| 20:02 to 20:04 | `release` run 44944146840 for 267fac8e: success. |
| 20:12:55 | v0.1.431 published. |
| by 20:17:30 | The coordinator answers `GET /v1/version` with `0.1.431` / `267fac8e2e54`. Its machine's updater took the release; nobody acted. |

Before 0.1.431 no host sent a maintenance report, because the coordinator advertised no
`maintenance-v1`. At 20:17 four of the six all-in-one nodes announced it. By 20:18 all of them had
sent a report or a failed look, on daemon 0.1.430: no daemon update was needed for this.

js-agent with `drain` and `resume` (komira-ai/jobscheduler#947) was already on every node: the
group's desired js-agent is 0.1.317, and every node runs it, healthy. It came through jobscheduler's
own release and its `hostkeeper-desired` workflow; nothing here rolled it.

## 2. The due lists before

The first look of each node, at 20:17:34 to 20:17:40. "new" means the package is not installed (the
item has no `from`).

| Node | OS, running kernel / newest installed | Due |
|---|---|---|
| spoolr-node-01 | Ubuntu 26.04, 7.0.0-44 / 7.0.0-44 | nothing |
| spoolr-node-02 | Ubuntu 24.04, 7.0.0-43 / 7.0.0-43 | `linux`: libdrm-amdgpu1 to 2.4.125-1ubuntu0.1~24.04.2 (security, new); linux-image-6.8.0-151-generic to 6.8.0-151.151 (kernel, new, needs reboot) |
| spoolr-node-03 | Ubuntu 24.04, 7.0.0-42 / 7.0.0-43 | `reboot`: kernel (7.0.0-42 running), reboot-required, needrestart; `linux`: the same two as node-02 |
| spoolr-node-04 | Ubuntu 24.04, 7.0.0-43 / 7.0.0-43 | `linux`: libdrm-amdgpu1 (security, new); python3-jwt 2.7.0-1ubuntu0.4 to 2.7.0-1ubuntu0.5 (security); linux-image-6.8.0-151-generic (kernel, new, needs reboot) |
| spoolr-node-05 | Ubuntu 24.04, 7.0.0-42 / 7.0.0-43 | as node-03 |
| spoolr-node-06 | not read | look failed: `apt: apt-get update failed with exit code 100: E: Could not get lock /var/lib/apt/lists/lock. It is held by process 2413341 (apt-get)` |

## 3. The node and the press

The node chosen is spoolr-node-04. It is not the coordinator's machine (the coordinator does not
run on an all-in-one node). It has only `linux` items, so the `reboot` gap of section 4 does not
stop it, and it has one installed security update (python3-jwt) for the apply to install. Every
all-in-one node runs its own SeaweedFS filer Postgres and tierstore, which have no `drain` step.
They stop with the node's restart and start again with it.

The press is one `Maintenance.Schedule` naming the host. It is the request the Update button on the
host's Updates tab sends (`ui/schedule.js`, `updateRequest`):

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    maintenance schedule --host-ids spoolr-node-04 --confirm true --reason "first real Linux maintenance run"
```

Not run yet: it is a cluster write, made by the operator. Expected from the code (section 4, item
1): stage, drain (js-agent cordons the node and waits for its leases), apply (python3-jwt alone), no
restart, then `check: exit 1, update_due: libdrm-amdgpu1 2.4.125-1ubuntu0.1~24.04.2 is still due`.
The host halts, cordoned, until **Put back**. The step times and the due list after are added here
once the run is made.

## 4. What surprised us

1. **Update cannot finish on any of these nodes as built (read from the code, before the run).**
   Under the default `security` scope the apply names only installed packages
   (`security_names` in `crates/hostkeeper-daemon/src/maintenance_linux/apt.rs`). It relies on a
   meta-package to pull a new package in. The look, and the check after the apply, report new
   packages as due. Every node's list has two new packages (libdrm-amdgpu1 and
   linux-image-6.8.0-151-generic) and no meta-package, so after the apply the check would find them
   still due and halt the host with `update_due`.
2. **A pending kernel makes Update defer.** The `reboot` kind's stage still answers
   `unsupported_kind` (`RebootKind::prepare` in `crates/hostkeeper-daemon/src/maintenance_linux/kinds.rs`), as
   [maintenance.md](maintenance.md#not-built-yet) says. Nodes 03 and 05 run 7.0.0-42 with 7.0.0-43
   installed, so Update on either would defer at the stage. The in-process e2e does not catch this,
   because its launcher answers the stage itself. The reboot-only run the plan asked for cannot be
   made until this is fixed.
3. **A look fails on a held apt lock.** The look's `apt-get update` waits for no lock, unlike the
   hook's stage and apply. spoolr-node-06's first look collided with another `apt-get`, and the host
   shows no due list until its next look, an hour later.
4. **An older-series kernel is due on a host booted from a newer one.** linux-image-6.8.0-151 is
   due, and marked as needing a reboot, on hosts that run 7.0. Installing it changes neither the
   running kernel nor the newest one.

## 5. Bugs filed

Each is to be fixed in its own pull request, with a test that fails before the fix:

- the security scope's apply and the look disagree on new packages (section 4, item 1);
- the `reboot` kind's stage is `unsupported_kind`, so Update defers a host with a pending kernel (item 2);
- the look's `apt-get update` waits for no apt lock (item 3), together with the question of item 4.
