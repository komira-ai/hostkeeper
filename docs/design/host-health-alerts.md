# Host health alerts: hardware and service faults, raised per host

Status: design. Nothing of it is built. It extends the alerts of
[hostkeeper-design.md](hostkeeper-design.md) section 6.7 (Built: `Alerts.List`, `hk alerts list`,
`crates/hostkeeper-coordinator/src/alerts.rs` and its `alerts_*.rs` modules, the book `alerts_book.rs`), the daemon's observation of
its services ([service-observation.md](../service-observation.md); Built: `crates/hostkeeper-observe`), the maintenance runs' drain
and put back ([maintenance-updates.md](maintenance-updates.md); Built: `hostkeeper.operator.v1.Maintenance`) and the console's host
health ([ui-serving.md](../ui-serving.md); Built: `ui/health.js`, `ui/pages/home.js`, `ui/pages/host.js`). Facts about this
repository were read on `main` at the commit this document lands on. **Built** marks what exists (with its path), **New** what this
design adds, **[V]** a claim to check on a real host before building on it. Section 9 records the owner's decisions (2026-11-26)
that shape it.

The owner's direction, after a host failed with nobody told: hostkeeper should catch issues like this and issue a notification, or
a warning, for that specific host.

## 1. The incident, and what catches it

A Linux build host (here `host-a`) rebooted at 03:01Z after its firmware recorded a fatal memory error on DIMM D1. It came up 16 GiB
short: `MemTotal` read 251543660 kB against 268320876 kB before (239.9 GiB against 255.9 GiB). js-agent's unit
`js-agent-crun.service` went into a restart loop (89 restarts and more), because the job service refused the host's new shape, and
js-agent's own health route answered `503 not placeable`. The js-agent and config updates on the host failed. The job service read
the host lost for over 90 minutes. hostkeeper raised nothing that said why, for three reasons:

- The heartbeat carries no memory, CPU, disk or sensor data (Built: `HeartbeatRequest`, `agent.proto`, fields 1 to 20).
- No alert reads a service's health: `HEALTH_UNHEALTHY` (`agent.proto`, `enum Health`) is shown on the host but no derivation of
  `derive_in` (`alerts.rs`) raises an alert from it. `silent_host` needs the host to stop beating (`alerts_maintenance.rs`), and it
  kept beating.
- `maintenance_not_back` covers planned reboots only. An unplanned reboot raises nothing.

Three catchers, each enough on its own, in order of how little they need: (1) **capacity compared with a baseline**, which needs no
hardware support; (2) **the BMC's event log**, which survives the crash; (3) **a crash loop or an unhealthy managed service**, which
catches any cause. This design builds all three, and the slower signals with them.

**After.** About 3 minutes after the host's first heartbeat after the boot (`capacity_regressed` needs two reports at least 2 minutes
apart after a new boot id, section 2.5), the console shows `host-a` amber, Home shows a warning banner, and the host's page reads:

```
WARNING host-a: memory 16 GiB short since the reboot at 03:01Z
  MemTotal 251543660 kB, was 268320876 kB at the last boot; the inventory says 256 GiB installed.
  SMBIOS: DIMM D1 empty, 15 of 16 slots populated (the inventory: 16).
  Cordoned for repair: draining (js-agent cordoned; its running jobs finish, no new ones start).
  First seen 03:04Z.
```

Within 5 minutes the BMC's event log adds the cause, a critical `hardware_fault`; within 10 the crash loop and js-agent's `not
placeable` join as likely effects (section 3.3):

```
CRITICAL host-a: uncorrectable memory error on DIMM D1 before the reboot at 03:01Z (BMC event log)
  Also: memory 16 GiB short since that reboot.
  Effect: js-agent-crun.service restarted 89 times; js-agent reports "not placeable: <reason>";
          2 failed updates since the reboot: js-agent, host-config.
  Cordoned for repair: drained 03:09Z, held. Put back with notes once the DIMM is replaced.
```

The first finding of bad hardware has the coordinator cordon the host through the drain that maintenance runs already use (section
3.5). For the job service that is a drain that does not come back until an operator puts the host back: its running jobs finish and it
gets no new ones. Nothing on the host is repaired by hostkeeper: replacing the DIMM is a person's job, and the person who puts the host
back writes what was done, kept with the alert for good (section 3.2).

**What does not change.** Every existing alert kind keeps its meaning, identity and log token; one keeps all but its severity:
`silent_host` becomes critical (section 3.1, decision 4). `Alerts.List` changes nothing an operator set. An agent or a coordinator of
an earlier release keeps working with a later one (section 5). The daemon gains reads only, and writes nothing outside its state
directory. Nothing is sent out of the coordinator: a person is told on the console (decisions 1 and 12).

## 2. What the daemon watches (New)

Severity: W is warning, C is critical. Polls are the daemon's. Counters reset at each boot, so every counter signal is keyed by host
and boot id. A source the host does not have (no EDAC driver, no `smartctl`) is reported as `unsupported`, and one that fails as
`unreadable` with its reason, in the report's `sources` (section 2.4): "zero errors" from a box that cannot report errors is not
healthy, and the console says "not monitored: <source>", never a clean bill. A Linux host with no BMC is a misconfiguration and an
alert of its own, `bmc_missing` (decision 6).

### 2.1 Linux

| Signal | Source | Poll | Sev | Clears |
|---|---|---|---|---|
| Capacity drop: memory (section 2.6), online CPUs, physical disks (size, count), physical NICs (speed) below the host's baseline | `/proc/meminfo`, the SMBIOS memory devices, `/sys/devices/system/cpu/{online,present}` and `smt/control`, `/sys/block/*/size`, `/sys/class/net/*/speed` | reported in every beat, read again 2 minutes after a new boot id; the coordinator compares | W (decision 4) | the value returns, or an operator rebaselines with notes |
| Host off or silent | the coordinator: no heartbeat for `--alert-silent-host-secs` (Built: `silent_host`) | | C (decision 4) | a heartbeat |
| No BMC | `/dev/ipmi0` absent, or no answer to `Get Device ID` (Built: the device of `crates/hostkeeper-daemon/src/ipmi.rs`) | at start, then hourly | W | the BMC answers |
| Unexpected reboot: a new boot id with no maintenance run and no clean-shutdown marker | `/proc/sys/kernel/random/boot_id`, the daemon's marker (2.5 step 6) | at daemon start | W | after 24 hours, or acknowledged |
| Crash loop of a managed service | systemd `NRestarts`, `Result`, `ActiveState`, `SubState`. Built: `crates/hostkeeper-observe/src/systemd.rs` reads `ActiveState`, `SubState`, `MainPID` and `LoadState`; `NRestarts` and `Result` are new reads in the same `systemctl show` | every observer round | C | 30 minutes without a restart |
| Service unhealthy for N minutes | the health of the `ServiceBeat` the host already reports (Built), with the reason and the first 160 bytes of the reply (New, 2.4) | already reported | W; C past 30 minutes | health returns |
| BMC event log: new entries for memory (including "memory device disabled"), processor, power supply, fan, temperature, watchdog, critical interrupt, OS critical stop, system boot or power cycle, POST error; unknown and OEM types kept raw as info, never dropped | IPMI SEL over `/dev/ipmi0` through `ipmi_rs` (Built: `ipmi.rs`, `IPMI_DEVICE`, which asks the BMC its address today) | every 60 to 300 s: the entries are read only when `Get SEL Info` shows a new count or add time; the DIMM name comes from the SDR sensor name, the SDR read once per boot | C for uncorrectable memory, memory device disabled, CPU thermal trip, PSU or fan failure, OS critical stop; W for corrected memory, a temperature threshold | an assertion clears at its deassertion, else 7 days after the last entry, or acknowledged |
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
| rasdaemon history | its SQLite database `/var/lib/rasdaemon/ras-mc_event.db`, opened read-only, read by `max(id)` (section 2.7) | 60 s | as the EDAC rows | as EDAC |

**Guards.** No capacity compare in a kdump boot (`/proc/vmcore` exists, or the command line has `elfcorehdr`). During a maintenance
run the compare is deferred to the first report after the run, not dropped: a reboot that loses a DIMM is exactly the case. SEL
deassertions and the "log cleared" and "system boot" entries are not faults. The baseline counts physical devices only: no `loop`,
`ram`, `zram`, `dm`, `md`, `nbd`, `sr` or removable USB disk; a NIC only if it has a `/device` and had carrier in the baseline; a speed
that cannot be read on a port that is down is unknown, not a drop. CPUs compare `present` and `smt/control`, so turning SMT off is not
a lost core.

### 2.2 macOS (Apple silicon, launchd)

**Memory errors on Macs are out of scope** (decision 5). Apple silicon has no DRAM error counters, no per-DIMM data and no SMBIOS
memory table that a program can read, so hostkeeper does not claim to watch a Mac's memory, and a Mac's page says `memory errors:
not monitored on Apple silicon`. What Apple does expose to a daemon, and hostkeeper reads (decision 11):

| What Apple exposes | Read with | What hostkeeper does with it |
|---|---|---|
| Hardware and memory inventory | `system_profiler -json SPHardwareDataType SPMemoryDataType SPStorageDataType`, `sysctl hw.memsize hw.physicalcpu hw.logicalcpu hw.perflevel0.physicalcpu` | the capacity baseline |
| Disk SMART status | `diskutil info -plist disk0`: `SMARTStatus` | `storage_failing` |
| Panic reports | `/Library/Logs/DiagnosticReports/*.panic` and the `.ips` reports whose header `bug_type` is `210` | `hardware_fault` (class panic) |
| Unexpected-shutdown records | the previous shutdown cause in the unified log, a new `kern.bootsessionuuid` with no clean-shutdown marker | `unexpected_reboot` |
| Thermal pressure | `pmset -g therm`, then `powermetrics --samplers thermal -n 1` once a limit shows | `hardware_fault` (class thermal) |
| Crash loops of managed jobs | `launchctl print system/<label>`: `runs`, `last exit code` | `service_crash_loop` |

What Apple does not expose: **Apple Diagnostics** (the hardware test run by holding the power button at start-up) needs a person at
the Mac and gives its result on the screen only; there is no API for a daemon to start it or read it. And there are no memory error
counters.

| Signal | Source | Poll | Sev | Clears |
|---|---|---|---|---|
| Panic report, classified: SError or AMCC wording is a suspected hardware fault (C); an SoC watchdog, a userspace watchdog timeout, a kext or other software is W, since a runaway build can trip a watchdog | the panic reports above, `Retired/` not read, each once by its header's `incident_id` | at start, then every 5 minutes; on a first install only the last 7 days | C or W by class | acknowledged, or 30 days |
| Unexpected reboot | `sysctl`, the daemon's marker, which is advisory on a Mac (2.5 step 6): it needs a panic report, an abnormal previous-shutdown cause, or a heartbeat gap to agree | at start | W | 24 hours or acknowledged; one alert per boot id, merged with any panic |
| Capacity drop: `hw.memsize`, core counts, performance-level counts, disk size, GPU cores | `sysctl`, `diskutil info -plist disk0`, `system_profiler`; a lower `hw.memsize` is compared as capacity, not watched as a memory error | at start and 2 minutes later | W | the value returns, or an operator rebaselines with notes |
| Storage failing | `SMARTStatus` | every 15 minutes | C for `Failing`; `Not Supported` is unknown, no alert | the status is `Verified` again |
| Sustained thermal pressure | `pmset -g therm` (needs no root); `powermetrics` (root, heavy on a build host) only once `pmset` shows a limit, at most every 5 minutes [V: `-n 1`] | 60 s | W after 5 samples at heavy; C at trapping | 10 minutes nominal |
| Crash loop of a managed job | `launchctl print`, a ring of samples | 60 s | C | 30 minutes without a restart |
| APFS or storage errors of the unified log | not read: `log show` is slow, heavy on a build host and noisy; `SMARTStatus` and the panic classes cover the serious cases | | | |

Not used on a Mac: IOKit wear data and `smartctl` (uncertain there), MDM and Apple Business (inventory only). Each parser is written
from a capture on a real Mac first [V].

### 2.3 How each is read: native or delegated

The workspace forbids `unsafe` (Built: `unsafe_code = "forbid"`; `ipmi.rs` says why its `ioctl`s are `ipmi_rs`'s), the daemon runs
as root, and every delegated tool is a dependency to install, version and parse. So:

**Native**, from files and existing crates, each parser tested on fixtures: the capacity baseline, the SMBIOS memory devices
(`/sys/firmware/dmi/entries/17-*/raw`, the table `dmidecode --type 17` decodes), the boot id and the clean-shutdown marker; the SEL
and SDR through `ipmi_rs`, with a decoder for an allowlist of sensor types (memory `0x0C`, processor `0x07`, power supply `0x08`, fan
`0x04`, temperature `0x01`, watchdog `0x23`, by the IPMI 2.0 event tables), the raw record kept as evidence; systemd properties through
`hostkeeper-observe`; taint, pstore, EDAC, `HardwareCorrupted`, AER, `thermal_throttle`, hwmon, ext4 `errors_count`, `statvfs`; the
boot-time kernel log scan (evidence, not the first detector, since message text varies between kernels); on a Mac the boot id, the
panic report parser (JSON and text; falling back to the file name and the first `panic(cpu` line), the `runs` ring and the capacity
`sysctl`s.

**Delegated**, through a process runner: a fixed argument list, no shell, an empty environment, an output cap and a kill at the
deadline. That is the `Launchctl` port (Built: `LaunchctlProcess` of `crates/hostkeeper-launchd`, which
`crates/hostkeeper-daemon/src/mac.rs` uses that way on a Mac, and which already runs programs on Linux: `maintenance_linux/apt.rs`,
`dnf.rs` and `rebooter.rs` of `crates/hostkeeper-daemon/src`); no new runner.
- `smartctl --json --nocheck=standby` for SATA, SAS and NVMe: the `ioctl`s would need `unsafe`. Never `-t` or any option that
  writes. Its exit status bits are read as `smartctl(8)` documents. A missing binary is an `unsupported` source.
- `diskutil`, `powermetrics`, `pmset`, `system_profiler` on a Mac.
- rasdaemon: its database, read-only. It decodes MCE and CPER records and maps DIMM labels; hostkeeper does not do that again. Absent
  is an `unsupported` source, not an error.
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
  Baseline baseline = 2;             // the host now: mem_total_kib, the SMBIOS memory devices (at most 32: locator and size),
                                     // cpus_present, cpus_online, smt, disks (at most 16: size and an 8-byte digest of the serial),
                                     // nics (at most 8: name and speed)
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
The part never dropped is the findings without their evidence (12 of at most about 210 bytes: 2.5 KiB) and the baseline without its
memory devices (at most about 350 bytes), under 3 KiB together. The evidence is at most 1 KiB in one beat (the rest waits for the next),
and `fit`'s `DROP_ORDER` (Built) gains two groups, dropped first: the findings' evidence, the memory devices and the `sources`, then the
services' `health_body` and `restarts`. The memory devices are sent at least once in each boot's first beats and when they change, so
the coordinator always holds the boot's list. A size test with 16 services and every part at its bound shows the beat fits [V].

The baseline and the slow facts are also shown on the host's page through a new `Collector` of the maintenance look (Built:
`crates/hostkeeper-daemon/src/maintenance_collectors.rs`, the hourly look of `maintenance_facts.rs`); the findings take the heartbeat so a fault is seen within one
beat, not within an hour.

**Expected sources.** Every Linux host is expected to have a BMC (decision 6), SMART-capable disks and a persistent journal (2.7).
EDAC is expected only on hosts the facts file labels `expects=edac` (Built: host labels, `facts.rs`), since a firmware-first host
legitimately has none (open question 1, section 9.2). An expected source reported `unsupported` or `unreadable` is `host_not_monitored`;
a Mac is expected to have none of the Linux sources.

### 2.5 Boot to boot

The daemon keeps a state file in its state directory, written whole (write, fsync, rename), keyed by machine id: the boot id, the
baseline, the high-water marks (the SEL's last record id and add time, rasdaemon's id, the panic reports' incident ids, the pstore
names), the clean-shutdown marker and the findings still held.

1. At start the daemon reads the current baseline. A boot id equal to the stored one is no new boot.
2. The daemon does not judge capacity alone: it reports the current baseline and boot id in every beat, and the coordinator compares
   (section 2.6). Both numbers go in the alert's text.
3. A capacity above the daemon's stored baseline raises that baseline quietly, shown as info on the host's page.
4. The coordinator keeps the **baseline** per host in its operations store (New: a table of `store/postgres/` and its records in
   `store/files.rs`, both held to one behaviour by `store/conformance_tests.rs`, Built; the files-store coordinator keeps its fleet in
   memory, and the Postgres one keeps only each host's last beat, so a baseline must be a record of its own). It derives
   `capacity_regressed` when a reported value is short of it (section 2.6) in two reports at least 2 minutes apart after a new boot id
   (early-boot values can settle). The stored value only goes up, or down through an operator's rebaseline. So a wiped or reinstalled
   daemon cannot re-baseline a short host, and the incident is caught with no daemon state at all.
5. **The clean-shutdown marker.** At start the daemon writes `running` to its state file. At SIGTERM it writes `clean` with the boot
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

### 2.6 The memory baseline: installed RAM, the DIMM list and `MemTotal` (decision 10)

**Installed RAM is not `MemTotal`.** The DIMMs installed (256 GiB, say: 16 modules of 16 GiB) are what the inventory records. The
kernel's `MemTotal` is the RAM it can hand out, and it is always a little less: the firmware keeps ranges for itself (ACPI tables, the
BMC's and the firmware's own reserved areas), and the kernel keeps its own image, its early page tables and, when one is configured, a
crash kernel's area. The gap is stable from boot to boot on the same host with the same firmware and kernel command line; a kernel
update moves it by a few MiB, a `crashkernel=` change by up to its size. The incident host read 268320876 kB (255.9 GiB) before the
fault, about 110 MiB under 256 GiB [V: the host's installed RAM, from its inventory], and 251543660 kB (239.9 GiB) after: 16777216 kB
less, exactly 16 GiB, one DIMM.

So memory is judged by two comparisons, each with its own source of truth:

- **(a) The DIMM list against the inventory.** The inventory record gains the installed memory, `installed_memory_bytes` and
  `memory_modules` (New: `InventoryRecord` fields 18 and 19 of `enrollment.proto`; fields 1 to 17 are taken; set with
  `Enrollment.UpdateRecord`, Built, and shown on the Inventory page). The daemon reports the SMBIOS memory devices (the table
  `dmidecode --type 17` prints: each slot's locator, and its module's size or none). The populated slots must number
  `memory_modules` and their sizes must sum to `installed_memory_bytes`, exactly: SMBIOS reports module sizes, not usable memory, so
  there is no tolerance. A slot empty or smaller is `capacity_regressed` naming the slot (`DIMM D1 empty`). A firmware that maps a
  failed module out may still list it [V: how each board reports a disabled DIMM], which is why (b) exists.
- **(b) `MemTotal` against the boots before.** The alert is raised when `MemTotal` is below the baseline's high-water `MemTotal` (the
  highest of the boots since the last rebaseline) by more than **1% or 1 GiB, whichever is larger**: on a 256 GiB host, 2.56 GiB. The
  incident's drop of 16 GiB (6.25%) is six times that; a kernel update or a `crashkernel=` of up to 1 GiB is inside it. Against the
  high-water value, a second short boot does not make the short value normal. The alert shows the last boot's value too, for the
  difference a person reads ("was 268320876 kB at the last boot").
- **The first baseline.** A host whose inventory has `installed_memory_bytes` is baselined from it at once: its first report is judged
  by (a), and its `MemTotal` must be at least 95% of the installed memory [V: measure the gap on each model]. A host whose inventory
  has none stores its first report as `unconfirmed`: shown, not alarmed, until an operator confirms it or fills the inventory. So a
  host first seen already short is never baselined at its short value.

### 2.7 The support tools, and what they are (decision 8)

| Tool | What it does | Platform | What Macs use instead |
|---|---|---|---|
| **smartmontools** (`smartctl`) | reads a disk's own health record (SMART): whether the disk says it is failing, its media errors, reallocated and pending sectors, an NVMe disk's spare and wear | Linux (hostkeeper does not use it on Macs) | `diskutil`'s `SMARTStatus`, part of macOS |
| **rasdaemon** | listens to the kernel's hardware error events (memory errors by DIMM, CPU machine checks, PCIe errors) and decodes them into a database that keeps them across reboots | Linux only | nothing: Apple silicon reports no memory or CPU errors; the panic reports cover what reaches a crash |
| **a persistent journal** | systemd's journal kept on disk (`/var/log/journal`) rather than in memory, so the log of the boot before a crash can still be read after it | Linux only | the unified log, which macOS keeps across reboots by default |

**Recommended:** all three on every Linux host, as host prerequisites (Built: the coordinator's prerequisites file and its report,
[host-prerequisites.md](host-prerequisites.md)), so a host without one says so on its page and raises `host_not_monitored`. hostkeeper
reads them; it never configures what they record. Macs need nothing installed.

### 2.8 Read-only, and bounded

The daemon runs as root on both platforms (Built: `packaging/linux/hostkeeper.service` names no `User=`, and the Mac job is a launchd
daemon; "the one root process of a host", [two-programs.md](two-programs.md)). The health look uses that only to read; the one thing
it changes, the cordon, is the coordinator's maintenance run, which runs each service's own `drain` hook (section 3.5).

- **Read-only.** No pstore record removed, no SEL cleared, no self-test, no `fsck`, no write to any sysfs file (an EDAC counter
  reset is a write). A SMART self-test or `verifyVolume` is an operator's maintenance run, never a poll.
- **IPMI.** `Get Device ID`, `Get SEL Info`, `Get SEL Entry` with reservation 0 and the SDR reads, and nothing else: `Reserve SEL` can
  cancel another tool's reservation, and `Clear SEL` and `Delete SEL Entry` destroy evidence. The requests are one static table, and a
  test asserts that nothing else can be sent. Calls run on a blocking thread with a deadline (an `ioctl` on a wedged BMC can hang), one
  at a time under a lock shared with the existing BMC address reads, with backoff after a timeout. If `ipmi_rs` has no SEL and SDR
  commands, raw requests go through the same wrapper [V].
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

| Kind (log token) | `AlertKind` | Meaning | Severity | Cordons (3.5) | Subject |
|---|---|---|---|---|---|
| `hardware_fault` | 25 | a finding of class memory, cpu, power, fan, thermal, pcie, kernel, pstore, panic (a Mac's panic report, its severity by its class) or error record of the previous boot | the finding's | when critical | host, finding code, subject |
| `capacity_regressed` | 26 | memory, CPU, disk or NIC short of the baseline (2.6) | warning (decision 4) | memory, CPU or disk | host, resource |
| `storage_failing` | 27 | a SMART, filesystem or nearly-full finding | the finding's | a critical SMART finding or a filesystem error (never nearly-full) | host, device serial or filesystem |
| `unexpected_reboot` | 28 | a new boot id with no maintenance run on the host and no clean-shutdown marker | warning | no | host, boot id |
| `service_unhealthy` | 29 | a managed service unhealthy for `--alert-unhealthy-secs` (default 600) | warning; critical after 30 minutes | no | host, service |
| `service_crash_loop` | 30 | restarts or `runs` past `--alert-restarts` (default 5) in 10 minutes | critical | no | host, service |
| `host_not_monitored` | 31 | an expected source (2.4) reports `unreadable` or `unsupported` | warning | no | host, source |
| `bmc_missing` | 32 | a Linux host whose BMC is absent or does not answer (decision 6) | warning | no | host |

**`silent_host` becomes critical** (decision 4: a host that is completely off or silent is critical): its arm moves from the warning
list to the critical list of `Kind::severity` (Built: `alerts.rs`). Its identity, meaning, threshold and log token do not change. This
is the one change to an existing kind, and a person reading `Alerts.List` sees it: the console shows a silent host red, not amber.

The `ALERT_KIND_*` values and their log tokens are added to `operator.proto`, nothing renumbered (B8). Each threshold is a coordinator
flag with the default above (S2: flags, not environment), like the alert flags of today (Built: `--alert-silent-host-secs`).

**Identity.** The `Subject` (Built: kind, service, host, version, request, other host, group) gains one part, `finding`: the finding's
code and subject, and for `unexpected_reboot` the boot id (`sel_memory_ue/DIMM_D1`), so two DIMMs' findings are two alerts. The
`Alert` carries it as `finding` (below). The new kinds are not written to the `alerts` rows (Built: `store/postgres/alert_rows.rs`,
keyed by the first six parts), which decide which coordinator logs an alert: a coordinator of the release before deletes every row it
did not derive. Their once-logging is a new table of the same rule, `health_alert_rows`, keyed by kind, host, service and `finding`.

`service_unhealthy` needs to know since when a service has been unhealthy, which a derivation cannot remember, so the operations store
keeps an `unhealthy_since` per host and service (New, a table of both backends, written at the observation), the same on every
coordinator and kept across a restart. It counts `HEALTH_UNHEALTHY`, including a health URL that refuses connections, which is the
crash-loop case. It is not raised during a maintenance turn of the host, a repair cordon (3.5), or the bake of a rollout of the
service (Built: `OpsState.maintenance_runs`, the plan stages); and when at least 3 hosts and half or more of a host group's hosts are
unhealthy for one service at once (a lease-server outage answers 503 to every js-agent), it is raised as one alert of the group, not
one per host.

The `Alert` message (Built: `operator.proto`, fields 1 to 14: host, service, message, first observed, reason, detail, since, ...)
gains `last_seen_unix` 15, `acknowledged` 16, `likely_cause` 17 (the alert this one is linked to, 3.3), `evidence` 18, `finding` 19 and
`history_id` 20 (its record in the history, 3.2). One host can have several findings; each is its own alert (its own `Subject`), so one
can clear while another stands, and the console groups them by host.

### 3.2 Dedup, clearing, acknowledging, and the history kept for good

- **Identity** is the `Subject`: the same finding in the next beats is one alert.
- **First seen** comes from the daemon's finding, so it survives a coordinator restart; the book keeps its own first observation as
  today (Built: `alerts_book.rs`).
- **Clearing.** An alert ends when the daemon stops reporting the finding (its clear rule fired) and the next 15 s observation no
  longer derives it. Today the book forgets it and logs nothing. New: a `cleared` log line (`alert=cleared kind=<kind> host=<host>`)
  and the history below.
- **Silence.** A host that stops beating keeps its last findings, shown "as of <time>", and `silent_host` is raised as today, now
  critical; findings are not cleared by silence.
- **Acknowledge, rebaseline and put back** are the coordinator's, not the daemon's: an acknowledgement held by the daemon is lost with
  its state file and cannot be given to a host that is down. **Any operator** may do each, and **each needs notes on what was done**
  (decision 3): the call is refused `notes_required` without them. New RPCs of `operator.proto`, and so new verbs of `hk`, whose
  commands are generated from the protos (Built: `ctl_gen.rs` of `hostkeeper-proto`): `hk alerts ack --host-id <host> --code <code>
  --notes <text>` and `hk alerts rebaseline --host-id <host> --notes <text>`; putting a host back is the Built
  `Maintenance.PutBack` (`hk maintenance put-back`), whose `reason` becomes required for every turn, repair or not, and is kept as the
  put back's notes. A CI token is refused all three, as it is `Alerts.List` today. An acknowledged alert is still
  derived but flagged: it leaves the banner, and comes back as new on a new boot id or a worse severity. A rebaseline replaces the
  stored baseline (and confirms an `unconfirmed` one). Each is audited under the caller's name with the host as the entry's node, its
  notes as the entry's reason.
- **The history, kept for good** (decision 7). Every alert that names a host, of any kind and severity,
  gets one record when it is first observed: host id, the host's hardware serial (from the inventory, so a machine's record follows the
  machine when its host id changes), kind, finding, severity, first and last seen, the evidence, the likely cause, when it cleared, and
  every note written on it (acknowledge, rebaseline, put back), each with its operator and time. Records are never pruned; a host's
  records are a few hundred bytes a fault, so a fleet keeps decades in a few megabytes. New table `alert_history` and its notes, in both
  store backends. `Alerts.History` (New, `hk alerts history --host-id <host>` or `--serial <serial>`) lists them, newest first, paged;
  the host's page shows them, so a DIMM slot that fails twice in a year, or a host whose disks keep failing, is visible at a glance.

### 3.3 Linking to effects

The effect (the job agent's crash loop, the lost host) was visible; the cause was not. The coordinator joins by host and boot:

- A `service_unhealthy` or `service_crash_loop` alert gets a **likely cause** when the same host has an open `capacity_regressed`,
  `hardware_fault` or `unexpected_reboot` of the same boot id, or a SEL finding from at most `--alert-cause-window-secs` (default 600)
  before that boot. Order is not required: the daemon may report the short memory after the service already went bad, and the two
  clocks differ. The cause's text then names its effects, and the effect points at its cause.
- A failed update (Built: `failed_update`) on a host with an open hardware alert, begun after that alert's boot, says so in its
  `likely_cause`. A host cordoned for repair takes no new rollout orders until it is put back (3.5).
- The job service's side: js-agent's `/healthz` is the health URL its package declares (Built in komira-ai/jobscheduler:
  `crates/js-agent/src/health.rs`, `503` with `not placeable: <reason>`, and `healthUrl` in `packaging/js-agent/deb/README.md`), so its
  reason reaches the coordinator in `health_body` (2.4) and is shown as the service's reason. js-agent also offers the job service the
  memory and CPU it detected (Built there: `crates/js-agent/src/host.rs`, `detect`); the host's page shows both numbers when they
  disagree with the baseline.
- A planned maintenance reboot raises no `unexpected_reboot`; a capacity drop found after it is raised.

A link is of time and place, and says "likely": a wrong link is worse than none, so `storage_failing` of a nearly-full disk and every
warning are never a cause.

### 3.4 The console: how a person is told (decisions 1 and 12)

For now a person is told on the console alone: no webhook, mail or other channel leaves the coordinator (section 7 keeps the
notification system as an end-state question). Built today: Home's alert strip (`alertsStrip` of `ui/pages/home.js`) lists every
alert, and `ui/health.js` turns the alerts that name a host, and its unhealthy services, into the host's reasons, shown on the host's
page, its row on Home and the fleet view. New:

- **A banner** at the top of every page while any host has a critical alert, and on Home while any host has a warning of the new kinds:
  "2 hosts need attention", one line each with the alert's first line (`host-a: uncorrectable memory error on DIMM D1 ...`), by time,
  linking to the host. An acknowledged alert leaves it. The alert strip stays. The banner reads `Alerts.List`, which `ui/calls.json`
  then lists for every page.
- **A badge per host**: red for critical, amber for warning, grey for sources not monitored, blue "cordoned for repair", on the fleet
  and the rack and lab views ([ui-shell-lab.md](ui-shell-lab.md)). `HEALTH_ALERTS` of `ui/health.js` gains the new kinds.
- **The host's page**: a **Hardware** panel with the baseline (memory against the inventory, the DIMM slots, CPUs, disks, NICs, as of
  which boot), the previous boot's when it changed, each open finding with severity, first and last seen and its evidence, each source's
  state (ok, unsupported, unreadable), the repair cordon's state, and the host's whole alert history with its notes; **Acknowledge**,
  **Re-baseline** and **Put back** buttons, each with a notes box it will not send empty (the same calls as the `hk` verbs).
- `hk alerts list` lists the same alerts, with `--host-id` and `--severity` filters (New).

What a line reads like: one host; what happened; cause before effect; numbers with units; what to do; the raw evidence below, never a
bare error code. Section 1 shows the incident's. Others:

- `host-a: no BMC answers; hardware events cannot be read` (warning).
- `mac-1: unexpected reboot, panic: SError, possible hardware fault` (critical; cordoned).
- `host-b: disk nvme1n1 failing (SMART critical warning); 14 media errors since last week` (critical; cordoned).
- `host-c: /var/lib/js 97% full (12 GiB free)` (critical; not cordoned: a full disk is not bad hardware).

### 3.5 Bad hardware cordons the host, through the drain path (decisions 2 and 9)

When a host gets an alert of the table's "Cordons" column, the coordinator cordons it for repair at once, with no person in the loop.
It does so through the maintenance run's drain (Built: `hostkeeper.operator.v1.Maintenance`, `crates/hostkeeper-coordinator/src/
maintenance_runs.rs`, the turn steps of `MaintenanceHostStep`), so the host leaves service exactly as it does for a maintenance window:

1. The coordinator schedules a run of one host, of a New kind `MAINTENANCE_KIND_REPAIR` (6 of `enum MaintenanceKind`; 0 to 5 are
   taken), as itself (audit actor `coordinator`, reason `repair: <kind> <finding>`), with no window, no batch, `take_last_host` (a
   broken host serves nothing, so the group's serving floor does not hold it back) and no drain deadline.
2. The host's turn drains as every turn does (Built: the step `draining`): the daemon runs each drained service's `[hook] drain`. For
   js-agent that is its own cordon on the lease server's own-host route, with the host's own credential, and its hold file (Built there,
   [maintenance-updates.md](maintenance-updates.md) sections 5.3 and 5.4): running jobs finish, no new lease starts. A drain waits for its work by
   default and cuts nothing.
3. A repair turn has no apply step. Once drained, it stops at the Built step `halted`, with the reason `held_for_repair`: the state
   maintenance-updates.md sections 5.4 and 5.7 already define ("the host stays halted and cordoned until Put back"), with no resume
   and no rollback. js-agent's hold file keeps the cordon across a reboot and while no hook runs.
4. The operator repairs the host and puts it back with the Built `Maintenance.PutBack`, notes required: the daemon runs each drained
   service's `resume`, js-agent uncordons, the turn ends `done`. Retry and Cancel are not offered on a repair turn.

**What the job service sees** is a drain that does not come back until an operator puts the host back (decision 9): a cordoned host
with no new work, its jobs finishing. hostkeeper tells the job service nothing else, and no change in jobscheduler is needed.

**Rules.** One repair run per host at a time; a second cordoning alert of the same host joins the open turn's reason. A host already in a
maintenance turn has the repair run queued behind it (its turn ends first, as Built: one turn per host). A host that is silent or off is
not reachable to drain: its repair turn waits at `waiting` and drains when the host beats again, and a host that is off holds no
leases. While cordoned for repair, the host takes no new rollout orders and no maintenance run but its own. Put back with the hardware
alert still open is allowed (the operator may judge the alert wrong), and the note says so; the alert stays until its clear rule fires.

## 4. Tests

All in Rust, in the crate of the code, over a fake of each source and recorded fixtures, with no real hardware: scratch directories
for `/proc` and `/sys`, a fake IPMI device behind the port of `ipmi.rs`, fake programs answering recorded output, an injected clock.
None times anything (T10, T12). Fixtures have generic host names, serials and addresses.

- **Files.** A reader takes a root path, so tests build a tree of `/proc` and `/sys` files: `meminfo` with and without a drop; EDAC
  counters 0 then 1 in one boot (C) and a counter reset at a new boot (none); a firmware-first host's zeros (`no EDAC data`); AER files;
  taint values M, B with and without `HardwareCorrupted`, D, L, W; hwmon absent-sensor values (0, -128000), 3 bad samples to raise and
  10 good to clear; pstore records read once across boots; a read-only remount; `statvfs` at 89.9%, 90% and 97%, clearing at 85%; bind
  mounts read once.
- **Memory baseline.** SMBIOS type 17 entries populated, empty, size 0 and malformed; 16 of 16 slots summing to the inventory's 256 GiB
  (none), D1 empty (warning naming D1), a module smaller; `MemTotal` 268320876 kB then 251543660 kB (warning); a drop of 2.5 GiB on a
  256 GiB host (none) and of 2.6 GiB (warning); a second short boot still short against the high-water value; a first report with the
  inventory filled (judged at once) and with none (`unconfirmed`, no alarm); `MemTotal` under 95% of the installed memory at the first
  report (warning).
- **IPMI.** Recorded SEL records: an uncorrectable memory entry with its DIMM named by the SDR, an assertion then its deassertion,
  "log cleared", pre-init timestamps, a cleared SEL (ids start again), a timeout then backoff; no device and a device that does not
  answer `Get Device ID` (`bmc_missing`); the request table holding no `Reserve`, `Clear` or `Delete`. Proves the decoder's allowlist,
  the high-water mark and the first run's 7 days.
- **systemd.** `NRestarts` rising, `start-limit-hit`, a restart by a deploy inside a run (suppressed), a oneshot unit.
- **Programs.** `smartctl --json` outputs for pass, fail, low spare, `critical_warning`, a disk in standby, unsupported, missing,
  timeout and over the cap; a disk name `-a` refused before any run; Mac `diskutil` `SMARTStatus` values, `pmset -g therm` outputs,
  `system_profiler -json` outputs.
- **Mac.** `.panic` and `.ips` fixtures in JSON and text (SError, SoC watchdog, userspace watchdog, kext, truncated, a non-kernel
  `.ips`, a duplicate `incident_id`); `launchctl print` with `runs` rising and reset; no memory error finding on a Mac (a lower `hw.memsize` is a capacity compare
  only). Replaced
  by captures from a real Mac before each parser lands.
- **State file.** A crash mid-write keeps the old file; a corrupt file is moved aside; a wiped state does not re-baseline a short host.
- **Boot.** A kdump boot (no compare); a clean reboot with the marker (nothing); a daemon restart that does not write `clean`, so a
  later crash still reads as unexpected; loop, zram and USB disk churn; an unplugged NIC; SMT turned off.
- **Coordinator**, one test per kind, as the derivations of `derive_in` are tested today: raised, one alert across beats, cleared
  when the finding stops, kept across a coordinator restart (both store backends), not cleared by silence, nothing from an old agent
  with no `HostHealth`, the likely-cause join in either order, suppression in a maintenance run, a repair cordon and a rollout bake, a
  group outage collapsed to one alert, the flags' thresholds; `silent_host` critical; a maintenance reboot that loses a DIMM alerting
  after the run.
- **Notes and history.** Acknowledge, rebaseline and a put back (of a repair turn and of a halted maintenance turn) refused `notes_required` with no or empty notes, accepted with
  them by any operator, refused to a CI token; each audited with its notes; an acknowledged alert back on a new boot id; a history
  record written at first observation, its clear and each note added, never pruned, read by host id and by serial after the host id
  changed; both store backends, two coordinators writing one record.
- **Repair cordon.** A critical `hardware_fault` schedules one repair run of the host as `coordinator`; the turn drains (a fake hook
  records `drain`), stops at `halted` with `held_for_repair`, and runs no apply and no `resume`; a second alert joins the open turn; a
  host in a maintenance turn gets its repair run after it; a silent host's turn waits and drains when it beats; Retry and Cancel refused
  on a repair turn; Put back runs `resume` and ends the turn; no rollout order to a cordoned host; a warning `capacity_regressed` of a
  NIC does not cordon; a full disk does not cordon.
- **Wire.** The heartbeat round trip (`proto_map.rs`), bounds refused, a heartbeat at every bound with 16 services under 16 KiB and
  the evidence and memory devices dropped first, an earlier release's messages reading a heartbeat with `HostHealth` and the new
  coordinator reading one without (the pattern of `wire_absent_tests.rs`, Built), nothing sent before `host-health-v1` is advertised;
  an older daemon given a repair order (section 5).
- **Console.** `ui/health.js` per new kind; the Hardware panel for every source state; the banner with one critical alert, with only
  warnings, and with only acknowledged alerts; the notes box refusing to send empty; the history list.
- **The incident, end to end.** A scripted host: healthy beats, a reboot, memory short, D1 empty, a SEL entry, js-agent unhealthy with
  `503 not placeable` and crash looping, two failed updates. It must give the alerts of section 1 with their links, a repair run that
  drains js-agent and holds, then, after a put back with notes and a DIMM restored, clear, with the history holding every step. Its
  variants: the daemon's state directory wiped; the coordinator restarted between the reboot and the first beat; the short memory
  reported after the service already went unhealthy; js-agent's health URL refusing connections; a host with no inventory memory
  (`unconfirmed`, no alarm).

**Why this is the whole list.** For each source it covers present and absent, a normal reading, each fault the source reports, a
malformed or truncated answer, a failure or hang, a counter reset at a boot, and its bounds; for each rule its threshold, its hold and
its clear; for each stored record a restart and two coordinators; for each operator action its notes, its audit and its refusals; for
the cordon each step and each refusal; and the incident from end to end.

## 5. Compatibility

- **An older daemon, a newer coordinator**: no `host-health-v1`, no `HostHealth`, no alert; the host's page says `hardware: not
  reported by this daemon's release` (the rule of the trusted root, Built: `alerts.rs`, "a host whose last heartbeat said nothing of
  the root (unknown) raises none"). The coordinator sends a repair order only to a daemon that announced `host-health-v1`, so an older
  daemon is never sent a maintenance kind it does not know; its host's alerts are shown, and an operator cordons it by hand.
- **A newer daemon, an older coordinator**: the coordinator advertises no `host-health-v1`, so the daemon sends nothing new.
- **An older `hk` or console**: the new `Alert` fields are unknown and the new kinds read as `ALERT_KIND_UNSPECIFIED`, shown by their
  message; `MAINTENANCE_KIND_REPAIR` reads as unspecified in a run's kinds.
- **`Maintenance.PutBack`**: a call with an empty `reason`, which is accepted today, is refused `notes_required` (decision 3); a script
  that puts hosts back must pass one.
- **`silent_host`**: the same alert, now critical; a script that filters on severity sees it among the critical ones.
- **Stores**: new tables by new migrations after `0031_maintenance_operator.sql` and their files counterparts; `maintenance_runs.kinds`
  gains the value `repair`; no shipped migration edited; the `alerts` rows and their key unchanged, since the new kinds are logged
  through `health_alert_rows` (3.1).
- **The inventory**: two optional fields; a record without them is a host with no installed memory known.

## 6. What only a person can read on the hosts

Whether each Linux host's BMC answers in-band, APEI and GHES, an EDAC driver, what SMBIOS shows after a DIMM is disabled, each host
model's `MemTotal` gap, and the Mac captures: they shape HH7 to HH13, not this design.

## 7. North star: a notification system (end state, not planned)

Decided for now: the console and its banner only (decision 1). The end state is a question for later, kept here so the console's data
model does not close it off. Options then: a webhook from the coordinator to a chat tool, a paging service or the organisation's own
notification service (secrets in files named by flags, never in argv, the environment or the repository; sent state in the operations
store, sent once across coordinators by a conditional write; one message per host per 5 minutes; a reminder while unacknowledged; a
`cleared` message; one digest, not a flood, when it is first turned on), or mail through a relay. What a message may carry would follow
section 3.4's lines and never carry a finding's raw evidence, journal lines, serials or addresses. Open, to decide then: which
destination, whether a warning is worth a message, and whether the coordinator's network may reach one.

## 8. Implementation pull requests

Each lands with its own tests, among them one that fails without it. A daemon pull request is harmless without the coordinator's
(an older coordinator ignores the field).

| # | Pull request | After |
|---|---|---|
| HH0 | Coordinator: move `Kind` and its three matches out of `alerts.rs`, no change of behaviour | |
| HH1 | Wire: `HostHealth`, `Finding`, `Baseline`, `SourceState`, the new `ServiceBeat` and `Alert` fields and kinds, `MAINTENANCE_KIND_REPAIR`, the inventory's two fields, `check_health`, the drop groups, `host-health-v1` advertised by no coordinator yet | |
| HH2 | Daemon: the state file, the boot id, the clean-shutdown marker, the first run's marks | HH1 |
| HH3 | Daemon, Linux: the capacity baseline (memory with the SMBIOS devices, CPUs, disks, NICs, with the device filters) | HH2 |
| HH4 | Coordinator: the baseline table and the inventory seed, `capacity_regressed`, `unexpected_reboot`, `hardware_fault`, `storage_failing`, `host_not_monitored`, `bmc_missing`, `silent_host` critical, `hk alerts ack` and `rebaseline` with notes, the `ui/health.js` kinds; advertises `host-health-v1` | HH0, HH3 |
| HH5 | Coordinator: `service_unhealthy` and `service_crash_loop` from the services' health, the likely-cause join | HH4, HH6 |
| HH6 | Observer and daemon, Linux: `NRestarts`, `Result`, `start-limit-hit`, the health reason and body | HH1 |
| HH7 | Daemon, Linux: `Get Device ID`, the SEL and SDR decoder, the high-water mark, the severity table | HH2 |
| HH8 | Daemon, Linux: taint, pstore, the boot-time kernel log scan, ext4 errors, read-only remounts, `statvfs` | HH2 |
| HH9 | Daemon, Linux: EDAC, `HardwareCorrupted`, AER, `thermal_throttle`, with boot-keyed rates | HH2 |
| HH10 | Daemon, Linux: hwmon and `smartctl --json`, with the source states | HH2 |
| HH11 | Daemon, Linux: the rasdaemon reader | HH2 |
| HH12 | Daemon, Mac: boot id, marker, the panic parser and classifier, capacity, `system_profiler` | HH2 |
| HH13 | Daemon, Mac: `SMARTStatus`, the thermal rule, the `runs` ring | HH12 |
| HH14 | Coordinator: the alert history kept for good, its notes, `Alerts.History`, the `cleared` log line | HH4 |
| HH15 | Coordinator and daemon: the repair cordon (section 3.5): the repair run, its turn held at `halted`, `PutBack`'s `reason` required for every turn, no rollout to a cordoned host | HH4 |
| HH16 | Console: banner, badges, the Hardware panel with its history and notes boxes, the Inventory page's memory fields | HH14, HH15 |
| HH17 | Host prerequisites: smartmontools, rasdaemon and a persistent journal on Linux hosts (2.7); an operations page (what each alert means, what to do, how to acknowledge, rebaseline and put back) and the "check on a host" lists as a runbook | HH5 |
| HH18 | The incident replayed end to end (section 4), last | HH1 to HH8, HH14, HH15 |
| future | A notification system (section 7): not planned until the owner decides the end state | |

HH6 can be reviewed once HH1 has landed, and HH7 to HH12 in parallel once HH2 has; HH13 follows HH12; HH14 and HH15 can be reviewed in
parallel. Each updates `docs/cli.md` and the goldens its verbs change (T19), and HH4 updates the severity of `silent_host` in
[hostkeeper-design.md](hostkeeper-design.md) section 6.7 and the operations docs that name it.

## 9. Decisions and questions

### 9.1 Decided (the owner, 2026-11-26)

1. **Notification**: for now a warning banner and the console only; no webhook or other channel. The notification system is an
   end-state question (section 7); its pull request is marked future.
2. **Bad hardware cordons the host automatically, for repair**, through the drain and cordon path maintenance runs use: the host is
   cordoned and its jobs finish, with no resume until an operator puts it back (section 3.5).
3. **Any operator** may acknowledge an alert or put a host back, and **must write notes** on what was done (section 3.2).
4. **A capacity drop is a warning. A host that is completely off or silent is critical** (`silent_host` becomes critical, 3.1).
5. **Macs: memory errors are out of scope** (section 2.2).
6. **Every Linux host has a BMC.** A host without one is a misconfiguration and raises `bmc_missing` (3.1).
7. **The full history of every alert is kept forever**, with the notes written when the host went back into service, to diagnose
   long-term issues per machine (section 3.2).
8. **Support tools** are explained in section 2.7: smartmontools, rasdaemon and a persistent journal, all three recommended on every
   Linux host; Macs use what macOS has.
9. **The job service** sees a hardware alert as a drain that never comes back until an operator puts the host back (section 3.5).
10. **The capacity baseline** is seeded from the inventory's installed memory, and memory is judged by the SMBIOS DIMM list against the
    inventory and by `MemTotal` against the last boot, within 1% or 1 GiB (section 2.6).
11. **Macs' diagnostics**: what Apple exposes to a daemon and what it does not, Apple Diagnostics included (section 2.2).
12. **Notifications**: the UI only, for now (section 3.4).

### 9.2 Still open

1. **Which hosts expect EDAC?** Proposed: only hosts labelled `expects=edac` in the facts file, since a firmware-first host
   legitimately reports none; every Linux host is expected to have a BMC, SMART and a persistent journal (2.4). Alternative: expect EDAC
   on every Linux host and label the exceptions.
