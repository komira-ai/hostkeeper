# Host health alerts: hardware and service faults, raised per host

Status: design. Nothing of it is built. It extends the alerts of
[hostkeeper-design.md](hostkeeper-design.md) section 6.7 (Built: `Alerts.List`, `hk alerts list`,
`crates/hostkeeper-coordinator/src/alerts.rs` and its `alerts_*.rs` modules, the book `alerts_book.rs`), the daemon's observation of
its services ([service-observation.md](../service-observation.md); Built: `crates/hostkeeper-observe`) and the console's host health
([ui-serving.md](../ui-serving.md); Built: `ui/health.js`, `ui/pages/home.js`, `ui/pages/host.js`). Facts about this repository were
read on `main` at the commit this document lands on. **Built** marks what exists (with its path), **New** what this design adds, **[V]**
a claim to check on a real host before building on it.

The owner's direction, after a host failed with nobody told: hostkeeper should catch issues like this and issue a notification, or
a warning, for that specific host.

## 1. The incident, and what catches it

A Linux build host (here `host-a`) rebooted at 03:01Z after its firmware recorded a fatal memory error on DIMM D1. It came up 16 GiB
short: `MemTotal` read 251543660 kB against 268320876 kB before (240 GiB of 256 GiB). js-agent's unit `js-agent-crun.service` went
into a restart loop (89 restarts and more), because the job service refused the host's new shape, and js-agent's own health route
answered `503 not placeable`. The js-agent and config updates on the host failed. The job service read the host lost for over 90
minutes. hostkeeper raised nothing that said why, for three reasons:

- The heartbeat carries no memory, CPU, disk or sensor data (Built: `HeartbeatRequest`, `agent.proto`, fields 1 to 20).
- No alert reads a service's health: `HEALTH_UNHEALTHY` (`agent.proto`, `enum Health`) is shown on the host but no derivation of
  `derive_in` (`alerts.rs`) raises an alert from it. `silent_host` needs the host to stop beating (`alerts_maintenance.rs`), and it
  kept beating.
- `maintenance_not_back` covers planned reboots only. An unplanned reboot raises nothing.

Three catchers, each enough on its own, in order of how little they need: (1) **capacity compared with a baseline**, which needs no
hardware support; (2) **the BMC's event log**, which survives the crash; (3) **a crash loop or an unhealthy managed service**, which
catches any cause. This design builds all three, and the slower signals with them.

**After.** `capacity_regressed` is derived from two reports at least 2 minutes apart after the new boot id (section 2.5), so about
3 minutes after the host's first heartbeat after the boot, the host has a critical alert and the coordinator's webhook sends:

```
CRITICAL host-a: memory 16 GiB short since the reboot at 03:01Z
  Memory is 240 GiB now, 256 GiB in the baseline (MemTotal 251543660 kB, was 268320876 kB).
  Likely action: take the host out of service and check its DIMMs; then hk alerts rebaseline --host-id host-a.
  First seen 03:04Z. https://hostkeeper.example.com/#/host/host-a
```

The BMC's finding comes at its next event-log read (at most 5 minutes), the crash loop within 10 minutes, js-agent unhealthy after
`--alert-unhealthy-secs`, and the failed updates when they fail. Each joins the alert as a likely effect (section 3.3), and the
sink sends what joined in one update per host per 5 minutes (section 4.1):

```
UPDATE host-a: memory 16 GiB short since the reboot at 03:01Z
  The BMC logged "Uncorrectable ECC, DIMM D1" before the reboot: replace or reseat DIMM D1.
  Effect: js-agent-crun.service restarted 89 times; js-agent reports "not placeable: <reason>".
UPDATE host-a: memory 16 GiB short since the reboot at 03:01Z
  Effect: 2 failed updates on the host since the reboot: js-agent, host-config.
```

The console shows the host red, with a banner on Home and the whole alert, cause and effects, on the host's page. Nothing on
the host is changed by hostkeeper: replacing the DIMM is a person's job.

**What does not change.** Every existing alert kind keeps its meaning, identity, severity and log token. `Alerts.List` changes
nothing an operator set. An agent or a coordinator of an earlier release keeps working with a later one (section 6). The daemon
gains reads only, and writes nothing outside its state directory. A coordinator with no sink configured sends nothing, as today.

## 2. What the daemon watches (New)

Severity: W is warning, C is critical. Polls are the daemon's. Counters reset at each boot, so every counter signal is keyed by host
and boot id. A source the host does not have (no BMC, no EDAC driver, no `smartctl`) is reported as `unsupported`, and one that fails
as `unreadable` with its reason, in the report's `sources` (section 2.4): "zero errors" from a box that cannot report errors is not
healthy, and the console says "not monitored: <source>", never a clean bill.

### 2.1 Linux

| Signal | Source | Poll | Sev | Clears |
|---|---|---|---|---|
| Capacity drop: `MemTotal`, online CPUs, physical disks (size, count), physical NICs (speed) below the host's baseline | `/proc/meminfo`, `/sys/devices/system/cpu/{online,present}` and `smt/control`, `/sys/block/*/size`, `/sys/class/net/*/speed` | reported in every beat, read again 2 minutes after a new boot id; the coordinator compares (section 2.5) | C for memory, CPU or disk; W for NIC speed | the value returns, or an operator rebaselines |
| Unexpected reboot: a new boot id with no maintenance run and no clean-shutdown marker | `/proc/sys/kernel/random/boot_id`, the daemon's marker (2.5 step 6) | at daemon start | W | after 24 hours, or acknowledged |
| Crash loop of a managed service | systemd `NRestarts`, `Result`, `ActiveState`, `SubState`. Built: `crates/hostkeeper-observe/src/systemd.rs` reads `ActiveState`, `SubState`, `MainPID` and `LoadState`; `NRestarts` and `Result` are new reads in the same `systemctl show` | every observer round | C | 30 minutes without a restart |
| Service unhealthy for N minutes | the health of the `ServiceBeat` the host already reports (Built), with the reason and the first 160 bytes of the reply (New, 2.4) | already reported | W; C past 30 minutes | health returns |
| BMC event log: new entries for memory (including "memory device disabled"), processor, power supply, fan, temperature, watchdog, critical interrupt, OS critical stop, system boot or power cycle, POST error; unknown and OEM types kept raw as info, never dropped | IPMI SEL over `/dev/ipmi0` through `ipmi_rs` (Built: `crates/hostkeeper-daemon/src/ipmi.rs`, `IPMI_DEVICE`, which asks the BMC its address today) | every 60 to 300 s: the entries are read only when `Get SEL Info` shows a new count or add time; the DIMM name comes from the SDR sensor name, the SDR read once per boot | C for uncorrectable memory, memory device disabled, CPU thermal trip, PSU or fan failure, OS critical stop; W for corrected memory, a temperature threshold | an assertion clears at its deassertion, else 7 days after the last entry, or acknowledged |
| Firmware error record | one scan per boot, at daemon start, of the kernel log for the BERT lines of the previous boot [V: the exact string] and `[Hardware Error]` lines, then `/dev/kmsg` read for new ones; the line's own severity token decides (fatal or recoverable C; corrected W, since corrected GHES and AER lines print `[Hardware Error]` too) | once per boot id, then each look | C or W by token | the next boot, or acknowledged |
| Kernel taint: M (machine check), B (bad page), D (oops), L (soft lockup) | `/proc/sys/kernel/tainted` [V: the bit numbers, `admin-guide/tainted-kernels.rst`] | 60 s | C for M; W for B, D and L (B and D also come from software bugs); B is C when `HardwareCorrupted` is above 0 | the next boot. Other bits are ignored |
| Previous boot panicked | new files in `/sys/fs/pstore` or `/var/lib/systemd/pstore` | once per boot id | C | acknowledged |
| Memory ECC counters | `/sys/devices/system/edac/mc/mc*/dimm*/{dimm_ce_count,dimm_ue_count,dimm_location,dimm_label}` and `mc*/{ce_noinfo_count,ue_noinfo_count}`. A firmware-first (GHES) host can show zeros while errors flow elsewhere, so zeros there are a source of "no EDAC data", not a healthy one | 60 s | C for any uncorrected; W for 10 or more corrected an hour on one DIMM | the rate under its limit for 24 hours |
| Poisoned memory | `HardwareCorrupted` in `/proc/meminfo` | 60 s | C above 0 | the next boot |
| PCIe AER | `/sys/bus/pci/devices/*/aer_dev_{correctable,nonfatal,fatal}` | 60 s | C for fatal; W for a fast correctable rate | the rate falls |
| CPU thermal throttling | `/sys/devices/system/cpu/cpu*/thermal_throttle/*` (Intel only) | 60 s | W | no new event for 30 minutes |
| Temperatures, fans | `/sys/class/hwmon`, drivers on an allowlist (`coretemp`, `k10temp`, `nvme`; not `drivetemp`, which sends SMART commands); the driver's own `crit` and `alarm` files, no thresholds of hostkeeper's | 60 s; 3 bad samples to raise, 10 good to clear | W; C at `crit` | hysteresis |
| Disk health | `smartctl --json --nocheck=standby` (SATA, SAS, NVMe): status, `critical_warning`, available spare, media errors, pending and reallocated sectors, keyed by serial | hourly | C for a failed status or `critical_warning`; W for media errors or sectors that grow | counters still for 7 days |
| Filesystem errors | `/sys/fs/ext4/*/errors_count`; a read-only remount in `/proc/mounts` | 60 s | C | the next boot, or acknowledged |
| Filesystem nearly full | `statvfs` of each local filesystem, one per device (bind mounts and btrfs subvolumes once) | 60 s | W at 90% of bytes or inodes; C at 97% | 5 points under the threshold crossed |
| rasdaemon history (optional) | its SQLite database `/var/lib/rasdaemon/ras-mc_event.db`, opened read-only, read by `max(id)` | 60 s | as the EDAC rows | as EDAC |

**Guards.** No capacity compare in a kdump boot (`/proc/vmcore` exists, or the command line has `elfcorehdr`). During a maintenance
run the compare is deferred to the first report after the run, not dropped: a reboot that loses a DIMM is exactly the case. The
`MemTotal` tolerance is the larger of 1 GiB and 1%, so a `crashkernel=` change stays inside it. SEL deassertions and the "log cleared"
and "system boot" entries are not faults. The baseline counts physical devices only: no `loop`, `ram`, `zram`, `dm`, `md`, `nbd`, `sr`
or removable USB disk; a NIC only if it has a `/device` and had carrier in the baseline; a speed that cannot be read on a port that is
down is unknown, not a drop. CPUs compare `present` and `smt/control`, so turning SMT off is not a lost core.

### 2.2 macOS (Apple silicon, launchd)

Apple silicon has no DRAM error counters, no per-DIMM data and no SMBIOS memory table. Detection is indirect, and an alert's text
says so rather than imply that memory is healthy.

| Signal | Source | Poll | Sev | Clears |
|---|---|---|---|---|
| Panic report, classified: SError, AMCC or DRAM wording is a suspected hardware fault (C); an SoC watchdog, a userspace watchdog timeout, a kext or other software is W, since a runaway build can trip a watchdog | `/Library/Logs/DiagnosticReports/Kernel_*.panic` and the `.ips` reports whose header `bug_type` is `210` (other `.ips` reports are app crashes; `Retired/` is not read), each once by its header's `incident_id` | at start, then every 5 minutes; on a first install only the last 7 days | C or W by class | acknowledged, or 30 days |
| Unexpected reboot: a new `kern.bootsessionuuid` with no clean-shutdown marker | `sysctl`, the daemon's marker, which is advisory on a Mac (2.5 step 6): it needs a panic report, an abnormal previous-shutdown cause, or a heartbeat gap to agree | at start | W | 24 hours or acknowledged; one alert per boot id, merged with any panic |
| Capacity drop: `hw.memsize`, core counts, performance-level counts, disk size, GPU cores | `sysctl`, `diskutil info -plist disk0`, `system_profiler` | at start and 2 minutes later | C | an operator accepts the new baseline |
| Storage failing | `diskutil info -plist disk0`: `SMARTStatus` | every 15 minutes | C for `Failing`; `Not Supported` is unknown, no alert | the status is `Verified` again |
| Sustained thermal pressure | `pmset -g therm` (needs no root); `powermetrics --samplers thermal -n 1` (root, heavy on a build host) only once `pmset` shows a limit, at most every 5 minutes [V: `-n 1`] | 60 s | W after 5 samples at heavy; C at trapping | 10 minutes nominal |
| Crash loop of a managed job | `launchctl print system/<label>`: `runs`, `last exit code` | 60 s, a ring of samples | C | 30 minutes without a restart |
| APFS or storage errors of the unified log | not in the end state's first build: `log show` is slow, heavy on a build host and noisy; `SMARTStatus` and the panic classes cover the serious cases | | | |

Not used on a Mac: IOKit wear data and `smartctl` (uncertain there), MDM and Apple Business (inventory only). Each parser is written
from a capture on a real Mac first [V].

### 2.3 How each is read: native or delegated

The workspace forbids `unsafe` (Built: `unsafe_code = "forbid"`; `ipmi.rs` says why its `ioctl`s are `ipmi_rs`'s), the daemon runs
as root, and every delegated tool is a dependency to install, version and parse. So:

**Native**, from files and existing crates, each parser tested on fixtures: the capacity baseline, the boot id and the clean-shutdown
marker; the SEL and SDR through `ipmi_rs`, with a decoder for an allowlist of sensor types (memory `0x0C`, processor `0x07`, power
supply `0x08`, fan `0x04`, temperature `0x01`, watchdog `0x23`, by the IPMI 2.0 event tables), the raw record kept as evidence;
systemd properties through `hostkeeper-observe`; taint, pstore, EDAC, `HardwareCorrupted`, AER, `thermal_throttle`, hwmon, ext4
`errors_count`, `statvfs`; the boot-time kernel log scan (evidence, not the first detector, since message text varies between
kernels); on a Mac the boot id, the panic report parser (JSON and text; falling back to the file name and the first `panic(cpu`
line), the `runs` ring and the capacity `sysctl`s.

**Delegated**, through a process runner: a fixed argument list, no shell, an empty environment, an output cap and a kill at the
deadline. On a Mac that is the `Launchctl` port (Built: `LaunchctlProcess` of `crates/hostkeeper-launchd`, which
`crates/hostkeeper-daemon/src/mac.rs` already uses that way). The same port already runs programs on Linux (Built:
`maintenance_linux/apt.rs`, `dnf.rs` and `rebooter.rs` of `crates/hostkeeper-daemon/src`), so it runs `smartctl` and the other
delegated tools there too; no new runner.
- `smartctl --json --nocheck=standby` for SATA, SAS and NVMe: the `ioctl`s would need `unsafe`. Never `-t` or any option that
  writes. Its exit status bits are read as `smartctl(8)` documents. A missing binary is an `unsupported` source.
- `diskutil`, `powermetrics`, `pmset`, `system_profiler` on a Mac.
- rasdaemon, optionally: its database read-only. It decodes MCE and CPER records and maps DIMM labels; hostkeeper does not do that
  again. Absent is an `unsupported` source, not an error.
- Not used: `mcelog`, `edac-utils`, `lm-sensors`, `smartd`.

### 2.4 What the daemon reports

A new optional message on the heartbeat, `HostHealth` (`HeartbeatRequest` field 21, New: fields 1 to 20 are taken), sent once the
coordinator's answer has advertised the feature `host-health-v1`, the mechanism of `maintenance-v1` (Built: the daemon's features and
the coordinator's `OFFERED` in `crates/hostkeeper-coordinator/src/features.rs` each gain the name). It goes on the gRPC route only
(the JSON route is frozen) and counts toward the heartbeat's bound on both, as the trusted root does (Built:
`crates/hostkeeper-link/src/beat.rs`).

```proto
message HostHealth {
  repeated Finding findings = 1;     // at most 12 held; past that the least severe, then the oldest, are left out and counted
  Baseline baseline = 2;             // the host now: mem_total_kib, cpus_present, cpus_online, smt, disks (at most 16: size and an
                                     // 8-byte digest of the serial), nics (at most 8: name and speed)
  string boot_id = 3;                // Linux boot_id, or a Mac's kern.bootsessionuuid
  uint64 boot_unix = 4;
  bool clean_previous_shutdown = 5;
  repeated SourceState sources = 6;  // per source: ok, unsupported, unreadable (with its reason)
}
message Finding {
  string code = 1;                   // a stable token of at most 32 bytes: capacity_memory, sel_memory_ue, edac_ue, panic_hw, ...
  Severity severity = 2;             // WARNING or CRITICAL
  string subject = 3;                // at most 32 bytes: mem, DIMM_D1, nvme0, a service's unit or label
  string summary = 4;                // one line a person can read, at most 120 bytes
  uint64 first_seen_unix = 5;
  uint64 last_seen_unix = 6;
  string evidence = 7;               // the raw record, at most 256 bytes, sent only in the beat after it is new or changed
  string boot_id = 8;                // empty when it is the report's boot_id
}
```

And on `ServiceBeat` (New; fields 1 to 15 are taken): `health_reason` 16 (the observer's reason token, Built:
`Observation::reason_token`, which only the JSON route's `health_detail` carries today), `health_body` 17 (the first 160 bytes of a
non-2xx reply, so js-agent's `503 not placeable: <reason>` reaches the coordinator), and `restarts` 18 (the restart counter, the
restarts in the last 10 minutes, the last result and exit status).

**Size.** The heartbeat's bound is 16 KiB (Built: `MAX_HEARTBEAT_BYTES`), and the maintenance report (about 5 KiB) and the
prerequisites report (about 4 KiB) are counted and never dropped (Built: `beat.rs`). So `HostHealth` is at most 3 KiB encoded
(`check_health`, checked on both sides like `check_report` of the maintenance report, Built: `crates/hostkeeper-proto/src/maintenance.rs`).
The part never dropped is the findings without their evidence (12 of at most about 210 bytes: 2.5 KiB) and the baseline (at most
about 350 bytes), under 3 KiB together. The evidence is at most 1 KiB in one beat (the rest waits for the next), and `fit`'s
`DROP_ORDER` (Built) gains two groups, dropped first: the findings' evidence and the `sources`, then the services' `health_body` and
`restarts`. A size test with 16 services and every part at its bound shows the beat fits [V].

The baseline and the slow facts are also shown on the host's page through a new `Collector` of the maintenance look (Built:
`maintenance_collectors.rs`, the hourly look of `maintenance_facts.rs`); the findings take the heartbeat so a fault is seen within one
beat, not within an hour.

**Expectation labels.** What a host should have comes from labels of the facts file (Built: host labels, `facts.rs`), New label
values `expects=ipmi`, `expects=edac`, `expects=smart`. Without them `host_not_monitored` (3.1) would either flood (every Mac, every
host with no BMC) or never fire.

### 2.5 Boot to boot: "memory 16 GiB short since the reboot"

The daemon keeps a state file in its state directory, written whole (write, fsync, rename), keyed by machine id: the boot id, the
baseline, the high-water marks (the SEL's last record id and add time, rasdaemon's id, the panic reports' incident ids, the pstore
names), the clean-shutdown marker and the findings still held.

1. At start the daemon reads the current baseline. A boot id equal to the stored one is no new boot.
2. The daemon does not judge capacity alone: it reports the current baseline and boot id in every beat, and the coordinator
   compares (step 4). Both numbers go in the alert's text.
3. A capacity above the daemon's stored baseline raises that baseline quietly, shown as info on the host's page.
4. The coordinator keeps the **high-water baseline** per host in its operations store (New: a table of `store/postgres/` and its
   records in `store/files.rs`, both held to one behaviour by `store/conformance_tests.rs`, Built; the files-store coordinator keeps
   its fleet in memory, and the Postgres one keeps only each host's last beat, so a baseline must be a record of its own). It derives `capacity_regressed` when a reported value is below it beyond
   tolerance in two reports at least 2 minutes apart after a new boot id (early-boot values can settle). The stored value only goes
   up, or down through an operator's rebaseline. So a wiped or reinstalled daemon cannot re-baseline a short host, and the incident is
   caught with no daemon state at all.
5. **The first baseline.** A host with no stored baseline stores its first report as `unconfirmed`: shown, not alarmed, until an
   operator confirms it. A host first seen already short would otherwise bake the short value in. It may be seeded from the SMBIOS
   total of populated DIMMs, kept only as a weak W cross-check [V: how vendors report a disabled DIMM there].
6. **The clean-shutdown marker.** At start the daemon writes `running` to its state file. At SIGTERM it writes `clean` with the boot
   id only if the system is really going down (Linux: `systemctl is-system-running` answers `stopping`, or a reboot, poweroff or halt
   job is queued). A SIGTERM of a daemon restart or of hostkeeper's own self-update must not count: it would leave a stale `clean`, and
   a later crash would read as a clean reboot. On a Mac launchd also sends SIGTERM at a bootout and no shutdown probe is known [V], so
   the marker is advisory there and `unexpected_reboot` needs corroboration (a panic report, an abnormal previous-shutdown cause, a
   heartbeat gap). A new boot id with no `clean` is an unexpected reboot, merged with the SEL's "power cycle", "OS critical stop" and
   "memory device disabled" entries and the pstore check into one finding per boot id. A reboot of an unattended update or of an
   operator, which shuts down cleanly, raises nothing.

**Held findings.** Alerts are derived and forgotten when their condition ends (Built: `alerts.rs`, "A condition that ends is
forgotten"), and a past fatal event is not "still true" in any live counter. So the daemon holds a finding until its clear rule fires
(the tables' last column), and reports it in every beat; an operator's acknowledgement lives at the coordinator (3.2). That is what
keeps a one-time SEL entry or an error record of the previous boot visible.

### 2.6 Read-only, and bounded

The daemon runs as root on both platforms (Built: `packaging/linux/hostkeeper.service` names no `User=`, and the Mac job is a launchd
daemon; "the one root process of a host", [two-programs.md](two-programs.md)). It uses that only to read.

- **Read-only.** No pstore record removed, no SEL cleared, no self-test, no `fsck`, no write to any sysfs file (an EDAC counter
  reset is a write). A SMART self-test or `verifyVolume` is an operator's maintenance run, never a poll.
- **IPMI.** `Get SEL Info`, `Get SEL Entry` with reservation 0 and the SDR reads, and nothing else: `Reserve SEL` can cancel another
  tool's reservation, and `Clear SEL` and `Delete SEL Entry` destroy evidence. The requests are one static table, and a test asserts
  that nothing else can be sent. Calls run on a blocking thread with a deadline (an `ioctl` on a wedged BMC can hang), one at a time
  under a lock shared with the existing BMC address reads, with backoff after a timeout. If `ipmi_rs` has no SEL and SDR commands,
  raw requests go through the same wrapper [V].
- **Programs.** Every program the health look can run is an entry of one static table of an absolute path and fixed arguments (the
  pattern of the probe's `STEPS`, Built: `crates/hostkeeper-daemon/src/maintenance_hook/probe.rs`), with one New slot for a unit, a
  label or a disk whose value must be a name the host listed (a `/sys/block` name of `^[a-z0-9]+$`, a unit of a manifest) and may not
  start with `-`.
- **rasdaemon.** Its database is opened `mode=ro` with a busy timeout and short reads, so it cannot block rasdaemon's writer; a locked
  database or an unexpected journal mode is an `unreadable` source, never a crash.
- **Deadlines.** Every read has a deadline (a sysfs read can hang on a wedged device), an output cap, and a failure of its own that is
  recorded in `sources` and never fails the heartbeat. Each source has its own worker, so a hung read stops that source only.
- **Cost.** A few dozen small reads a minute; the SEL is read in full only when its count or add time changes. Programs: `smartctl`
  hourly; on a Mac `launchctl print` and `pmset` every 60 s, `diskutil` every 15 minutes, `powermetrics` at most every 5 minutes and
  only under a thermal limit, `system_profiler` at start and 2 minutes later.
- **History is not news.** The first run on a host with an old SEL or old panic reports reports only the entries of the last 7 days
  and marks the rest as seen. A cleared SEL (record ids start again, the count falls) resets the marks. A BMC timestamp from before its
  clock was set after a power loss ("pre-init") is ordered by record id instead.
- **macOS.** Panic reports need no privacy grant [V]; the daemon already reports whether it has Full Disk Access (Built:
  `crates/hostkeeper-daemon/src/full_disk_access.rs`).

## 3. Coordinator side (New)

### 3.1 The alert kinds

Alerts stay derived, as today: one function per kind, `Subject` the identity (Built: `alerts.rs`). `alerts.rs` is at 998 lines
(R12), so HH0 (section 8) first moves `Kind` and its `log_token`, `severity` and `proto` to their own module; the new derivations go
in `alerts_health.rs`.

| Kind (log token) | `AlertKind` | Meaning | Severity | Subject |
|---|---|---|---|---|
| `hardware_fault` | 25 | a finding of class memory, cpu, power, fan, thermal, pcie, kernel, pstore, panic (a Mac's panic report, its severity by its class, 2.2) or error record of the previous boot | the finding's | host, finding code, subject |
| `capacity_regressed` | 26 | memory, CPU, disk or NIC below the baseline | critical; warning for a NIC | host, resource |
| `storage_failing` | 27 | a SMART, filesystem or nearly-full finding | the finding's | host, device serial or filesystem |
| `unexpected_reboot` | 28 | a new boot id with no maintenance run on the host and no clean-shutdown marker | warning | host, boot id |
| `service_unhealthy` | 29 | a managed service unhealthy for `--alert-unhealthy-secs` (default 600) | warning; critical after 30 minutes | host, service |
| `service_crash_loop` | 30 | restarts or `runs` past `--alert-restarts` (default 5) in 10 minutes | critical | host, service |
| `host_not_monitored` | 31 | a source the host's expectation labels say it has reports `unreadable` or `unsupported` | warning | host, source |
| `notify_failed` | 32 | the sink failed to deliver for 30 minutes (section 4.1) | warning | sink |

The `ALERT_KIND_*` values and their log tokens are added to `operator.proto`, nothing renumbered (B8).

**Identity.** The `Subject` (Built: kind, service, host, version, request, other host, group) gains one part, `finding`: the finding's
code and subject, and for `unexpected_reboot` the boot id (`sel_memory_ue/DIMM_D1`), so two DIMMs' findings are two alerts. The
`Alert` carries it as `finding` (below). The new kinds are not written to the `alerts` rows (Built: `store/postgres/alert_rows.rs`,
keyed by the first six parts), which decide which coordinator logs an alert: a coordinator of the release before deletes every row it
did not derive. Their once-logging is a new table of the same rule, `health_alert_rows`, keyed by kind, host, service and `finding`. Each threshold is a coordinator
flag with the default above (S2: flags, not environment), like the alert flags of today (Built: `--alert-silent-host-secs`).

`service_unhealthy` needs to know since when a service has been unhealthy, which a derivation cannot remember, so the operations store
keeps an `unhealthy_since` per host and service (New, a table of both backends, written at the observation), the same on every
coordinator and kept across a restart. It counts `HEALTH_UNHEALTHY`, including a health URL that refuses connections, which is the crash-loop case. It is
not raised during a maintenance turn of the host or the bake of a rollout of the service (Built: `OpsState.maintenance_runs`, the plan
stages); and when at least 3 hosts and half or more of a host group's hosts are unhealthy for one service at once (a lease-server outage answers 503 to
every js-agent), it is raised as one alert of the group, not one per host.

The `Alert` message (Built: `operator.proto`, fields 1 to 14: host, service, message, first observed, reason, detail, since, ...)
gains `last_seen_unix` 15, `acknowledged` 16, `likely_cause` 17 (the alert this one is linked to, 3.3), `evidence` 18 and `finding`
19. One host can
have several findings; each is its own alert (its own `Subject`), so one can clear while another stands, and the console groups them
by host.

### 3.2 Dedup, first and last seen, clearing, acknowledging

- **Identity** is the `Subject`: the same finding in the next beats is one alert.
- **First seen** comes from the daemon's finding, so it survives a coordinator restart; the book keeps its own first observation as
  today (Built: `alerts_book.rs`).
- **Clearing.** An alert ends when the daemon stops reporting the finding (its clear rule fired) and the next 15 s observation no
  longer derives it. Today the book forgets it and logs nothing. New: a `cleared` log line (`alert=cleared kind=<kind> host=<host>`)
  and a short history per host, the last 20 cleared alerts with when each began and ended, so a person who looks 20 minutes later
  still sees that something happened and when it ended. The history is a new table of the operations store, not the `alerts` rows
  (Built: `store/postgres/alert_rows.rs`), because a coordinator of the release before deletes every `alerts` row it did not derive.
- **Silence.** A host that stops beating keeps its last findings, shown "as of <time>", and `silent_host` is raised as today;
  findings are not cleared by silence.
- **Acknowledge and rebaseline** are the coordinator's, not the daemon's: an acknowledgement held by the daemon is lost with its
  state file and cannot be given to a host that is down. New RPCs of `operator.proto`, and so new verbs of `hk`, whose commands are
  generated from the protos (Built: `ctl_gen.rs` of `hostkeeper-proto`): `hk alerts ack --host-id <host> --code <code>` and
  `hk alerts rebaseline --host-id <host>`, with a store record keyed by host, code, subject and boot id. An acknowledged alert is still
  derived but flagged: it leaves the banner and the notifications, and comes back as new on a new boot id or a worse severity. A
  rebaseline replaces the stored high-water baseline (and confirms an `unconfirmed` one). Both are audited under the caller's name
  with the host as the entry's node.

### 3.3 Linking to effects

The effect (the job agent's crash loop, the lost host) was visible; the cause was not. The coordinator joins by host and boot:

- A `service_unhealthy` or `service_crash_loop` alert gets a **likely cause** when the same host has an open `capacity_regressed`,
  `hardware_fault` or `unexpected_reboot` of the same boot id, or a SEL finding from at most `--alert-cause-window-secs` (default 600) before that boot. Order is not required: the
  daemon may report the short memory after the service already went bad, and the two clocks differ. The cause's message then names its
  effects (section 4.3), and the effect points at its cause.
- A failed update (Built: `failed_update`) on a host with an open critical hardware alert, begun after that alert's boot, says so in its
  `likely_cause`; whether such a host takes rollouts at all is question 2.
- The job service's side: js-agent's `/healthz` is the health URL its package declares (Built in komira-ai/jobscheduler:
  `crates/js-agent/src/health.rs`, `503` with `not placeable: <reason>`, and `healthUrl` in `packaging/js-agent/deb/README.md`), so its
  reason reaches the coordinator in `health_body` (2.4) and is shown as the service's reason. js-agent also offers the job service the
  memory and CPU it detected (Built there: `crates/js-agent/src/host.rs`, `detect`); the host's page shows both numbers when they
  disagree with the baseline. No change in jobscheduler is needed.
- A planned maintenance reboot raises no `unexpected_reboot`; a capacity drop found after it is raised.

A link is of time and place, and says "likely": a wrong link is worse than none, so `storage_failing` of a nearly-full disk and every
warning are never a cause.

### 3.4 The console

Built today: Home's alert strip (`alertsStrip` of `ui/pages/home.js`) lists every alert, and `ui/health.js` turns the alerts that name
a host, and its unhealthy services, into the host's reasons, shown on the host's page, its row on Home and the fleet view. New:

- **A badge per host**: red "hardware" for critical, amber for warning, grey for sources not monitored, on the fleet and the rack and
  lab views ([ui-shell-lab.md](ui-shell-lab.md)). `HEALTH_ALERTS` of `ui/health.js` gains the new kinds but `notify_failed`.
- **The host's page**: a **Hardware** panel with the baseline (memory, CPUs, disks, NICs, as of which boot), the previous baseline when
  it changed, each open finding with severity, first and last seen and its evidence, each source's state (ok, unsupported, unreadable),
  the recent cleared alerts, and **Acknowledge** and **Re-baseline** buttons (the same calls as the `hk` verbs).
- **A banner**: Home shows one when any host has a critical alert of the new kinds, "2 hosts need attention", one line each, by time,
  linking to the host. The alert strip stays.
- `hk alerts list` lists the same alerts, with `--host-id` and `--severity` filters (New).

## 4. How a person is told (New)

### 4.1 The channels

Today the coordinator has no pager (Built: `alerts.rs`, "The coordinator has no pager"); alerts leave only as `alert=` log lines and
`Alerts.List`, and nobody looks unless they look.

| Option | For | Against |
|---|---|---|
| Console banner and `hk alerts list` only | no new moving part | nobody looks unless they look |
| A webhook from the coordinator to a chat tool, a paging service or the organisation's own notification service | works with what people already watch; one place; the book already dedups | needs a destination and a secret, delivery retries and a sent state |
| Mail (SMTP submission) | universal | needs a relay; slow to be noticed |

**The end state**: a webhook sink in the coordinator, plus mail as a second kind of sink, both named by flags, their URL, relay and
credentials read from files the flags name (a webhook URL is itself a secret; never in argv, the environment or the repository).
`--notify-webhook-url-file`, `--notify-webhook-key-file` (the HMAC-SHA256 key that signs the body), and for mail `--notify-smtp-file`.
The console and `hk` stay as the pull side. The sink:

- covers every critical alert, not only the new kinds (`silent_host` and `maintenance_not_back` deserve a message too);
- sends at a critical alert's first observation and when a warning has stayed open 15 minutes; a reminder every 6 hours while it is
  open and not acknowledged; a `cleared` message when it ends;
- sends at most one message per host per 5 minutes, merging what came in between;
- keeps what it sent in the operations store and sends a message only after its conditional write of that message's row succeeded,
  the exactly-once rule of the periodic jobs of [coordinator-multi-instance.md](coordinator-multi-instance.md) section 9 (Built), so two
  coordinators do not send twice and a restart does not send again;
- at its first start with alerts already open, marks them sent and sends one digest, not a flood;
- tries a failed send again after 1 minute, doubling up to 30; a sink failing for 30 minutes is `notify_failed`, on the console and
  through the other sink only.

The coordinator checks the files at start and does not start if one is wrong (B20: a config mistake is fixed at its source and found
once, at deploy); `hk alerts notify-test` (New, audited) sends one test message for that check. Which destination is the owner's call
(question 1), and whether the coordinator's network reaches one at all is question 12.

### 4.2 What a notification carries

The webhook body is one JSON document, `schema` 1 (a later schema only adds fields, B8), with `text` holding the message as a person
reads it, so a chat tool can show it as it is:

```json
{"schema": 1, "event": "opened", "host_id": "host-a", "host_group": "linux-build", "severity": "critical",
 "alerts": [{"kind": "capacity_regressed", "subject": "mem"}, {"kind": "hardware_fault", "subject": "DIMM_D1"},
            {"kind": "service_crash_loop", "service": "js-agent", "likely_cause": "capacity_regressed"}],
 "text": "CRITICAL host-a: memory 16 GiB short since the reboot at 03:01Z\n...",
 "url": "https://hostkeeper.example.com/#/host/host-a"}
```

A sink is outside the coordinator's trust: a message carries the host, the kinds, the codes, the subjects, counts and sizes, a
service's reason and a link, and never a finding's raw evidence, journal lines, `log_tail` text, serials or addresses. Those are on the
console and in `hk`, behind an operator's sign-in.

### 4.3 What a good message reads like

One host; one line that says what happened; cause before effect; numbers with units; what to do; the raw evidence below, never a bare
error code. Section 1 shows the incident's. Three alerts stand behind it, `capacity_regressed` (mem), `hardware_fault`
(`sel_memory_ue`, DIMM D1, the SEL record as evidence) and `service_crash_loop` (js-agent), with `service_unhealthy` and the two
`failed_update`s, joined by the likely-cause rule. Others:

- With no BMC: the same first two lines about capacity, and the cause line reads `no BMC event log on this host`.
- On a Mac: `mac-1: unexpected reboot, panic: SError, possible hardware fault (Apple silicon has no memory error counters)`.
- `host-b: disk nvme1n1 failing (SMART critical warning); 14 media errors since last week`.
- `host-c: /var/lib/js 97% full (12 GiB free)`.

## 5. Tests

All in Rust, in the crate of the code, over a fake of each source and recorded fixtures, with no real hardware: scratch directories
for `/proc` and `/sys`, a fake IPMI device behind the port of `ipmi.rs`, fake programs answering recorded output, an injected clock,
a webhook receiver and an SMTP server on loopback port 0. None times anything (T10, T12). Fixtures have generic host names, serials
and addresses.

- **Files.** A reader takes a root path, so tests build a tree of `/proc` and `/sys` files: `meminfo` with and without a drop,
  tolerance just under and over 1 GiB and 1%; EDAC counters 0 then 1 in one boot (C) and a counter reset at a new boot (none); a
  firmware-first host's zeros (`no EDAC data`); AER files; taint values M, B with and without `HardwareCorrupted`, D, L, W; hwmon
  absent-sensor values (0, -128000), 3 bad samples to raise and 10 good to clear; pstore records read once across boots; a read-only
  remount; `statvfs` at 89.9%, 90% and 97%, clearing at 85%; bind mounts read once.
- **IPMI.** Recorded SEL records: an uncorrectable memory entry with its DIMM named by the SDR, an assertion then its deassertion,
  "log cleared", pre-init timestamps, a cleared SEL (ids start again), no device, a timeout then backoff; the request table holding no
  `Reserve`, `Clear` or `Delete`. Proves the decoder's allowlist, the high-water mark, the first run's 7 days, and that unsupported is
  quiet.
- **systemd.** `NRestarts` rising, `start-limit-hit`, a restart by a deploy inside a run (suppressed), a oneshot unit.
- **Programs.** `smartctl --json` outputs for pass, fail, low spare, `critical_warning`, a disk in standby, unsupported, missing,
  timeout and over the cap; a disk name `-a` refused before any run; Mac `diskutil` `SMARTStatus` values, `pmset -g therm` outputs.
- **Mac.** `.panic` and `.ips` fixtures in JSON and text (SError, SoC watchdog, userspace watchdog, kext, truncated, a non-kernel
  `.ips`, a duplicate `incident_id`); `launchctl print` with `runs` rising and reset. Replaced by captures from a real Mac before each
  parser lands.
- **State file.** A crash mid-write keeps the old file; a corrupt file is moved aside; a wiped state does not re-baseline a short host.
- **Boot.** A kdump boot (no compare); a clean reboot with the marker (nothing); a daemon restart that does not write `clean`, so a
  later crash still reads as unexpected; loop, zram and USB disk churn; an unplugged NIC; SMT turned off.
- **Coordinator**, one test per kind, as the derivations of `derive_in` are tested today: raised, one alert across beats, cleared
  when the finding stops, kept across a coordinator restart (the baseline and history tables, both store backends), not cleared by
  silence, nothing from an old agent with no `HostHealth`, the likely-cause join in either order, suppression in a maintenance run and
  a rollout bake, a group outage collapsed to one alert, the flags' thresholds; a first report `unconfirmed` and not alarmed; a
  maintenance reboot that loses a DIMM alerting after the run; acknowledge and rebaseline audited, an acknowledged alert back on a new
  boot id.
- **Wire.** The heartbeat round trip (`proto_map.rs`), bounds refused, a heartbeat at every bound with 16 services under 16 KiB and
  the evidence dropped first, an earlier release's messages reading a heartbeat with `HostHealth` and the new coordinator reading one
  without (the pattern of `wire_absent_tests.rs`, Built), nothing sent before `host-health-v1` is advertised.
- **Sink.** First send, reminder, cleared, the 5-minute merge, retries on the injected clock, `notify_failed`; two coordinators
  deliver once; a first start with open alerts sends one digest; the webhook body and its signature equal to the expected bytes; the
  evidence, serials and journal lines absent, by equality of the whole body; the URL and key never in a log line.
- **Console.** `ui/health.js` per new kind; the Hardware panel for every source state; the banner with one critical alert and with
  only warnings.
- **The incident, end to end.** A scripted host: healthy beats, a reboot, memory short, a SEL entry, js-agent unhealthy with `503 not
  placeable` and crash looping, two failed updates. It must give the three alerts, the joined message of section 1, one webhook
  delivery, then clear after a rebaseline. Its variants: the daemon's state directory wiped; the coordinator restarted between the
  reboot and the first beat; the short memory reported after the service already went unhealthy; js-agent's health URL refusing
  connections; a first report from a host already short (`unconfirmed`, no alarm).

**Why this is the whole list.** For each source it covers present and absent, a normal reading, each fault the source reports, a
malformed or truncated answer, a failure or hang, a counter reset at a boot, and its bounds; for each rule its threshold, its hold and
its clear; for each stored record a restart and two coordinators; for the sink the exact bytes, what must never be in them, a failure
and a retry; and the incident from end to end.

## 6. Compatibility

- **An older daemon, a newer coordinator**: no `host-health-v1`, no `HostHealth`, no alert; the host's page says `hardware: not
  reported by this daemon's release` (the rule of the trusted root, Built: `alerts.rs`, "a host whose last heartbeat said nothing of
  the root (unknown) raises none").
- **A newer daemon, an older coordinator**: the coordinator advertises no `host-health-v1`, so the daemon sends nothing new.
- **An older `hk` or console**: the new `Alert` fields are unknown and the new kinds read as `ALERT_KIND_UNSPECIFIED`, shown by their
  message.
- **Stores**: new tables by new migrations after `0031_maintenance_operator.sql` and their files counterparts; no shipped migration
  edited; the `alerts` rows and their key unchanged, since the new kinds are logged through `health_alert_rows` (3.1).

## 7. Questions for the owner

1. Who gets told, and where? Which destination for critical alerts (a chat channel, a pager, mail), and is a warning that stays open
   15 minutes worth a message, or only a banner?
2. Should a host with an open critical hardware alert take no new rollouts and be drained, or only be flagged?
3. Who acknowledges and re-baselines: any operator, or only named ones? Should a rebaseline need a written reason?
4. Is it right that a capacity drop is critical by default for memory, CPU and disk, and only a warning for NIC speed?
5. Are Macs held to the same standard? A Mac with Apple silicon has no memory error counters, so "no fault seen" there means less. Is
   a printed "not monitored for memory errors" on Macs enough?
6. Should hostkeeper require a BMC with in-band IPMI on every Linux host (a host without one raises `host_not_monitored`), or is a host
   without one an accepted exception?
7. How long is the cleared-alert history kept: days, or for good, as a hardware record per machine?
8. May hostkeeper install and enable support packages on hosts (smartmontools, rasdaemon, a persistent journal) as part of host setup,
   or only use what is already there?
9. Should the job service be told of a hardware alert too, so it can stop placing work on a host before it fails, or is hostkeeper
   the only place faults are raised?
10. Who confirms each host's first baseline, and may it be seeded from an inventory or the SMBIOS total? A host that is already short
    must not be baselined at its short value.
11. Which hosts carry which expectation labels (`ipmi`, `edac`, `smart`)? That decides `host_not_monitored`.
12. Does the network the coordinator runs on let it reach a chat, paging or mail service at all?

What only a person can read on the hosts (whether each has a BMC, APEI and GHES, an EDAC driver, a persistent journal, rasdaemon,
what SMBIOS shows after a DIMM is disabled, and the Mac captures) shapes HH7 to HH13, not this design.

## 8. Implementation pull requests

Each lands with its own tests, among them one that fails without it. A daemon pull request is harmless without the coordinator's
(an older coordinator ignores the field).

| # | Pull request | After |
|---|---|---|
| HH0 | Coordinator: move `Kind` and its three matches out of `alerts.rs`, no change of behaviour | |
| HH1 | Wire: `HostHealth`, `Finding`, `Baseline`, `SourceState`, the new `ServiceBeat` and `Alert` fields and kinds, `check_health`, the drop groups, `host-health-v1` advertised by no coordinator yet | |
| HH2 | Daemon: the state file, the boot id, the clean-shutdown marker, the first run's marks | HH1 |
| HH3 | Daemon, Linux: the capacity baseline (memory, CPUs, disks, NICs, with the device filters) | HH2 |
| HH4 | Coordinator: the baseline table, `capacity_regressed`, `unexpected_reboot`, `hardware_fault`, `storage_failing`, `host_not_monitored`, `hk alerts ack` and `rebaseline`, the `ui/health.js` kinds; advertises `host-health-v1` | HH0, HH3 |
| HH5 | Coordinator: `service_unhealthy` and `service_crash_loop` from the services' health, the likely-cause join | HH4, HH6 |
| HH6 | Observer and daemon, Linux: `NRestarts`, `Result`, `start-limit-hit`, the health reason and body | HH1 |
| HH7 | Daemon, Linux: the SEL and SDR decoder, the high-water mark, the severity table | HH2 |
| HH8 | Daemon, Linux: taint, pstore, the boot-time kernel log scan, ext4 errors, read-only remounts, `statvfs` | HH2 |
| HH9 | Daemon, Linux: EDAC, `HardwareCorrupted`, AER, `thermal_throttle`, with boot-keyed rates | HH2 |
| HH10 | Daemon, Linux: hwmon and `smartctl --json`, with the source states | HH2 |
| HH11 | Daemon, Linux: the optional rasdaemon reader | HH2 |
| HH12 | Daemon, Mac: boot id, marker, the panic parser and classifier, capacity | HH2 |
| HH13 | Daemon, Mac: `SMARTStatus`, the thermal rule, the `runs` ring | HH12 |
| HH14 | Coordinator: the cleared history and the `cleared` log line | HH4 |
| HH15 | Console: badges, the Hardware panel, the banner | HH4 |
| HH16 | Coordinator: the webhook and mail sinks, reminders, cleared messages, `hk alerts notify-test`; an operations page for them | HH5, HH14 |
| HH17 | Docs: an operations page (what each alert means, what to do, how to acknowledge and rebaseline) and the "check on a host" lists as a runbook | HH5 |
| HH18 | The incident replayed end to end (section 5), last | HH1 to HH8, HH14, HH16 |

HH6 can be reviewed once HH1 has landed, and HH7 to HH12 in parallel once HH2 has; HH13 follows HH12; HH14 and HH15 can be reviewed in
parallel. Each updates `docs/cli.md` and the goldens its verbs
change (T19), and HH16 rewrites the sentence of [hostkeeper-design.md](hostkeeper-design.md) section 6.7 that says the coordinator has no pager and
calls out to nothing but the Xcode feed, since the sinks call out too.
