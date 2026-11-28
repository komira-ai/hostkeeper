# Host health alerts: one hardware event log per host

Status: design, built in part (the wire of HH1, the daemon's state file of HH3, its Linux capacity report of HH4, its rasdaemon
source of HH9, its SMART and hwmon sources of HH11, the Linux kernel sources of HH10, its reader of the BMC's event log of HH8,
and the Mac's sources of HH12 and HH13, marked **Built** where they are). It extends the alerts of
[hostkeeper-design.md](hostkeeper-design.md) section 6.7 (Built: `Alerts.List`, `hk alerts list`,
`crates/hostkeeper-coordinator/src/alerts.rs` and its `alerts_*.rs` modules, the book `alerts_book.rs`), the daemon's observation of
its services ([service-observation.md](../service-observation.md); Built: `crates/hostkeeper-observe`), the maintenance runs' drain
and put back ([maintenance-updates.md](maintenance-updates.md); Built: `hostkeeper.operator.v1.Maintenance`) and the console's host
health ([ui-serving.md](../ui-serving.md); Built: `ui/health.js`, `ui/pages/home.js`, `ui/pages/host.js`). Facts about this
repository were read on `main` at the commit this document lands on. **Built** marks what exists (with its path), **New** what this
design adds, **[V]** a claim to check on a real host before building on it. Section 13 records the owner's decisions (2026-11-26).

The owner's direction, after a host failed with nobody told: hostkeeper should catch issues like this and issue a notification, or a
warning, for that specific host. And then: is there a way to have one standardized log that tracks all hardware issues across both
platforms? This design's answer is that log.

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
catches any cause.

**After.** Each source on the host is read and normalized into one record shape, and each record is written to the host's **hardware
event log** on the coordinator (section 2). Alerts are derived from that log (section 5), and the log, with the operators' notes in it,
is the host's history for good. For the incident, in order:

| Time | Read | Written to `host-a`'s log | What it raises |
|---|---|---|---|
| 03:04Z | the capacity of the new boot | `memory`, `DIMM D1`, warning, source `baseline`: memory 16 GiB short of the baseline, D1 empty | `capacity_regressed` (warning); a banner; the host cordoned for repair (section 6) |
| by 03:09Z | the BMC's event log | `memory`, `DIMM D1`, critical, source `sel`: uncorrectable ECC on DIMM D1 before the reboot | `hardware_fault` (critical) |
| by 03:14Z | the services' health: `js-agent-crun.service` restarting 89 times; js-agent `not placeable` | no hardware record; the coordinator's `alert_raised` records of the two alerts (section 5.2) | `service_crash_loop`, `service_unhealthy`, linked to the memory fault as likely effects |

The host's page then reads:

```
CRITICAL host-a: uncorrectable memory error on DIMM D1 before the reboot at 03:01Z (BMC event log)
  Also: memory 16 GiB short since that reboot (MemTotal 251543660 kB, was 268320876 kB; D1 empty).
  Effect: js-agent-crun.service restarted 89 times; js-agent reports "not placeable: <reason>";
          2 failed updates since the reboot: js-agent, host-config.
  Cordoned for repair: drained 03:09Z, held. Put back with notes once the DIMM is replaced.
```

For the job service the cordon is a drain that does not come back until an operator puts the host back: its running jobs finish and it
gets no new ones. Nothing on the host is repaired by hostkeeper: replacing the DIMM is a person's job, and the person who puts the host
back writes what was done into the same log.

**What does not change.** Every existing alert kind keeps its meaning, identity and log token; one keeps all but its severity:
`silent_host` becomes critical (section 5.1). `Alerts.List` changes nothing an operator set. An agent or a coordinator of an earlier
release keeps working with a later one (section 9). The daemon gains reads only, and writes nothing outside its state directory.
Nothing is sent out of the coordinator: a person is told on the console.

## 2. The hardware event log (New)

One log per host, kept by the coordinator, with one schema on Linux and on a Mac. Every hardware source of section 3 is normalized
into it: a record says what part of the machine, how bad, from where, in a sentence a person reads, and the line the source printed.
Alerts are derived from it; a person's acknowledgements and repair notes are records in it; nothing is ever removed from it.

### 2.1 The record

| Field | Meaning |
|---|---|
| `event_id` | a stable id, the same however often the source is read: the source and its own id (a SEL record id with the SEL's last erase time, a panic report's `incident_id`, a rasdaemon row id, a kernel record's boot id and sequence number, a SMART attribute's disk and value), so a record read twice is written once |
| `host_id` | the host |
| `machine_serial` | the machine's hardware serial (from the inventory), so a machine's log follows the machine when its host id changes |
| `time_unix` | when the source says it happened (the source's clock); `observed_unix`, when the coordinator first had it |
| `boot_id` | the boot it was read in; `of_previous_boot` when the source says it happened before that boot (a BERT record, a SEL entry or a pstore record older than the boot) |
| `component` | one of `memory`, `cpu`, `pcie`, `disk`, `thermal`, `power`, `board`; and `host` for what concerns the whole host (an unexpected reboot, the host off, no BMC) |
| `subject` | which one: the memory slot (`DIMM D1`), the CPU (`CPU 1`), the PCIe device (`0000:3b:00.0`), the disk (`nvme1n1`, with its serial kept as a digest), the sensor (`coretemp/Package id 0`), the power supply (`PSU 2`), `board` |
| `severity` | `info`, `warning` or `critical`, by the table of its source (section 3) |
| `state` | `event` (it happened), `asserted` (a condition began: a sensor over its threshold, a disk failing) or `deasserted` (that condition ended) |
| `source` | `rasdaemon`, `smart`, `journal`, `sel`, `sysfs`, `hwmon` and `baseline` on Linux; `panic`, `shutdown`, `diskutil`, `thermal` and `baseline` on a Mac (a `system_profiler` change is judged by the coordinator, so its record is `baseline`); `coordinator` for an alert's raising and ending; `operator` for a person's note |
| `code` | a stable token of the normalized fault, the same on both platforms where the fault is the same: `memory_uncorrectable`, `memory_corrected_rate`, `capacity_short`, `cpu_machine_check`, `pcie_fatal`, `disk_failing`, `disk_media_errors`, `io_error`, `thermal_critical`, `power_supply_failed`, `unexpected_reboot`, `panic_hardware`, `bmc_missing`, `note`, ... |
| `message` | the normalized message: one line a person reads, at most 120 bytes |
| `raw` | the source's own line, as it printed it, cut to 256 bytes |
| `about` | for a note, a deassertion or an alert's raising and ending, the `event_id` it is about (at most 64 bytes) |
| `by` | for a note, the operator who wrote it |

A record is never edited and never removed. A condition's end is a new record (`deasserted`, `about` the assertion), and a person's
note is a new record (`source` `operator`, `code` `note`, `about` the record or the alert's first record, `message` the note). **Corrected
errors are coalesced**: the corrected memory errors of one DIMM (rasdaemon's corrected rows, the SEL's corrected ECC entries), the
correctable errors of one PCIe device and the corrected machine checks of one CPU are one `info` record per subject per hour, its
`message` carrying the count (`37 corrected ECC errors on DIMM D1 in the hour`, the plural its source's table gives), and its `raw` the hour's last line; the rate alert
(`memory_corrected_rate`) reads the counts. So a record is at most about 600 bytes, and a failing part writes at most 24 records a day
of corrected errors. A machine with a few hundred records a year keeps a decade of log in a few megabytes [V: measured on the fleet
once the log runs].

### 2.2 One record from each platform

**Linux, the incident's DIMM D1** (the BMC's event log, read after the reboot):

```json
{"event_id": "sel:01a4@1790000000", "host_id": "host-a", "machine_serial": "SN-0001",
 "time_unix": 1795662012, "observed_unix": 1795662540, "boot_id": "b7c1...", "of_previous_boot": true,
 "component": "memory", "subject": "DIMM D1", "severity": "critical", "state": "event",
 "source": "sel", "code": "memory_uncorrectable",
 "message": "Uncorrectable ECC error on DIMM D1",
 "raw": "01a4 | 11/26/2026 | 03:00:12 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted"}
```

and the capacity record the coordinator wrote from the first reports of the new boot:

```json
{"event_id": "baseline:host-a:b7c1...:memory", "host_id": "host-a", "machine_serial": "SN-0001",
 "time_unix": 1795662240, "observed_unix": 1795662240, "boot_id": "b7c1...",
 "component": "memory", "subject": "DIMM D1", "severity": "warning", "state": "asserted",
 "source": "baseline", "code": "capacity_short",
 "message": "Memory 16 GiB short: MemTotal 251543660 kB, was 268320876 kB; DIMM D1 empty (inventory: 16 x 16 GiB)",
 "raw": "MemTotal: 251543660 kB | SMBIOS 17 D1: No Module Installed"}
```

**Mac, a panic** (a panic report after an unexpected restart):

```json
{"event_id": "panic:2b6f9c1e-7d1a-4c3e-9f20-6a1d0e5b8c44", "host_id": "mac-1", "machine_serial": "SN-0002",
 "time_unix": 1795640400, "observed_unix": 1795640460, "boot_id": "5e2a...", "of_previous_boot": true,
 "component": "board", "subject": "board", "severity": "critical", "state": "event",
 "source": "panic", "code": "panic_hardware",
 "message": "Kernel panic (SError), suspected hardware fault",
 "raw": "panic(cpu 4 caller 0xfffffe0012345678): \"SError interrupt, ESR 0xbe000011 ...\""}
```

### 2.3 Where it is kept

A new table of the coordinator's operations store, `hardware_events`, keyed by `host_id` and `event_id`, indexed by `machine_serial`
and by time, in both backends (Built: `store/postgres/` and `store/files.rs`, held to one behaviour by `store/conformance_tests.rs`),
added by a new migration after `0031_maintenance_operator.sql`. A record is written by a conditional insert on its key, so a record that
two coordinators receive, or that a daemon sends again, is written once. `HardwareEvents.List` (New, `hk hardware log --host-id
<host>` or `--serial <serial>`, `--since`, `--component`, `--severity`) lists a log newest first, paged; the host's page shows it. A
DIMM slot that fails twice in a year, or a machine whose disks keep failing, is one filter away.

## 3. Every source, normalized (New)

The daemon reads each source and turns each of its entries into a record of section 2.1; the coordinator writes the records of what
it compares itself (`baseline`) and of what an operator says (`operator`). A source the host does not have, or that fails, is reported
as `unsupported` or `unreadable` with its reason (section 4.1): "zero errors" from a box that cannot report errors is not healthy, and
the console says "not monitored: <source>", never a clean bill.

### 3.1 Linux

| Source | What is read | Normalized to | Severity | Poll |
|---|---|---|---|---|
| **rasdaemon** (Built: `crates/hostkeeper-daemon/src/rasdaemon.rs`) | its database `/var/lib/rasdaemon/ras-mc_event.db`, read-only (4.5), the rows after each table's last rowid read (the mark in the state file, 4.4), of its tables `mc_event` (memory, by DIMM label), `mce_record` (CPU machine checks), `aer_event` (PCIe), `mf_event` (memory failure) and `disk_errors` (failed block requests); a table the database lacks is passed over, and an empty one is read (13.2, item 1) [V: the table names of the installed release] | `memory` by DIMM label (`mc<n>` with none), `cpu` by socket, `pcie` by device, `memory` for a memory failure, `disk` by name; `raw` is the row's fields; `event_id` `rasdaemon:<table>:<rowid>@<time>`; the corrected rows of a subject, and a disk's failed requests, one record per hour with their count (2.1) | uncorrected, fatal and memory failure C; deferred, an unknown type and a machine check with no status W; corrected `info`; a disk's failed requests W (`io_error`); `memory_corrected_rate` W at 10 or more an hour on one DIMM (the coordinator's) | 60 s |
| **SMART**, through smartmontools | `smartctl --json=c --nocheck=standby --info --health --attributes /dev/<disk>` of each disk (SATA, SAS, NVMe): status, `critical_warning`, available spare, media errors, pending and reallocated sectors (Built: `crates/hostkeeper-daemon/src/smart.rs`) | `disk` by name, keyed by serial; `disk_failing` asserted while failing and deasserted when it passes again or is gone, `disk_media_errors` at each growth (`info` at a disk's first read) | failing, `critical_warning` or a spare under its threshold C (its end C too, so it is never folded); growth W | hourly |
| **The persistent journal** | the kernel's lines of the boot before and of this boot, read once per boot and then followed: machine checks (`mce:`), EDAC, AER, the BERT lines of the previous boot [V: the exact string], `[Hardware Error]`, and I/O errors (`I/O error, dev <disk>`) | `cpu`, `memory`, `pcie`, `disk` (`io_error`); the line's own severity token decides | fatal and recoverable C; corrected `info`; an I/O error W | once per boot, then followed |
| **The BMC's event log** (IPMI SEL) | `Get SEL Info`, `Get SEL Entry` over `/dev/ipmi0` (Built: `crates/hostkeeper-daemon/src/ipmi.rs`, `IPMI_DEVICE`), entries newer than the last read; the slot and sensor named by the SDR | memory (sensor type `0x0C`, including "memory device disabled"), processor (`0x07`), power supply (`0x08`), fan (`0x04`) and temperature (`0x01`) to `memory`, `cpu`, `power`, `thermal`; watchdog, critical interrupt, OS critical stop, POST error to `board`; unknown and OEM types kept as `info`, never dropped | uncorrectable memory, memory disabled, a CPU thermal trip, a PSU or fan failure, an OS critical stop C; corrected memory, a temperature threshold W | 60 to 300 s, the entries read only when the count or add time changed |

Also read, and normalized the same way: the capacity of every boot (section 4.3: `MemTotal`, the SMBIOS memory devices, CPUs, disks,
NICs), which the coordinator compares and records as `capacity_short` (`baseline`); kernel taint M, B, D and L
(`/proc/sys/kernel/tainted`, `cpu` or `memory`, `sysfs`); `HardwareCorrupted` of `/proc/meminfo` (`memory`); pstore records of the
previous boot (`board`); hwmon temperatures against their driver's own `crit` (`thermal`, the drivers `coretemp`, `k10temp`, `nvme`;
source `hwmon`, every 60 s, `thermal_critical` asserted at `crit` and deasserted at the driver's `crit_hyst` or 5 °C under `crit`,
Built: `crates/hostkeeper-daemon/src/hwmon.rs`);
ext4 `errors_count` and read-only remounts (`disk`); and a missing or silent BMC (`host`, `bmc_missing`, decision 6). A filesystem
nearly full is not hardware: it is part of `storage_failing` (5.1) with no hardware record.

**Built (HH10)**: the kernel's log and its own files, two sources of `hardware_sources.rs` (HH11) in
`crates/hostkeeper-daemon/src/kernel_sources.rs`, each read every 60 s on a worker of its own from the daemon's start on a Linux or
OpenRC host (`journalctl` runs with a 30 s deadline and a 64 KiB output cap; the reads of `/proc`, `/sys` and `/dev/kmsg` are of the
kernel's own files and never block), judged by the pure functions of `kernel_log.rs` and `kernel_files.rs`, whose module docs give
each rule; the fixtures are in `crates/hostkeeper-daemon/tests/fixtures/kernel/`.

- Source `journal`: this boot's lines are followed on `/dev/kmsg` (opened once without blocking; at most 4096 records a read; the
  mark, the journal's cursor of 4.4, is the boot and the next sequence number). Once in each boot the boot before is read with
  `journalctl --dmesg --boot=-1 --output=json --reverse --lines=100` (the delegated program of 4.2), keeping only the lines logged
  after the last one read on `/dev/kmsg` in that boot, which are the lines before a crash. Machine checks (by the status's UC bit),
  EDAC (by DIMM label), AER, APEI's `[Hardware Error]:` records (one record per error, by `section_type`; those with no `{<n>}` are
  BERT's and `of_previous_boot`), AMD's machine check decoder and I/O errors of physical disks give records; every other line
  gives none. The records of one read that agree in code, subject, severity and boot are one record that counts them. A record's
  id is `journal:<boot>:<usec>:<digest of the line>`, so a line read on `/dev/kmsg` and again from the journal is one record. A
  host with no `/var/log/journal` reports the source `unsupported`, and `/dev/kmsg` is still followed.
- Source `sysfs`: taint M (`cpu`, critical), B (`memory`), D and L (`cpu`, warning), once a boot; `HardwareCorrupted` (`memory`,
  warning) each time it grows in a boot; each ext4 filesystem's `errors_count` (`disk`, warning) each time it grows; a filesystem of a
  block device mounted read-write in this boot and read-only now (`disk`, critical, asserted; deasserted when it is writable again,
  unmounted, or its boot ended); and the dumps of `/sys/fs/pstore` and of `/var/lib/systemd/pstore` (`board`: critical
  `panic_hardware` when a panic or an oops names an uncorrected hardware error, else a panic or an oops warning, else `info`), each
  read once.
  Nothing is written, removed or cleared.
- Each read reports its source's state in `HostHealth.sources` (`ok`, `unsupported` or `unreadable` with its reason).
- **A filesystem nearly full** (Built: `crates/hostkeeper-daemon/src/filesystems.rs`, the daemon's side;
  `crates/hostkeeper-coordinator/src/alerts_health.rs`, the coordinator's rule). It is carried by `HostHealth.filesystems` (field 7,
  additive: a coordinator of a release before keeps it as a field it does not know, and a daemon of a release before sends none),
  not by a record: each heartbeat's health lists, as `statvfs` reads them then, at most 8 filesystems, each by its mount point (cut
  to 32 bytes), its size and the bytes an unprivileged writer may still write. Which ones: the root filesystem first, then, by mount
  point, every other filesystem of a local block device (no `loop`, `ram`, `zram`, `nbd` or `sr` device; no `tmpfs`, overlay,
  network or pseudo filesystem) mounted read-write, once per device, as `/proc/self/mountinfo` lists them; so wherever a host mounts
  the daemon's state, the packages or the jobs' data, it is reported. On a Mac, the data volume `/System/Volumes/Data` (`/` is the
  sealed system volume) and each volume mounted under `/Volumes`. A filesystem `statvfs` cannot read, a read-only one and one of
  no size are left out. The list goes with the memory devices and `sources` when a heartbeat is over its 16 KiB (4.1), and with
  the memory devices when the health would pass its 4096 bytes; a health with no filesystems is no word on fullness, which the
  coordinator does not read as a filesystem emptied. The coordinator's rule (5.1): 90% used (the size less the available bytes, of
  the size) is `storage_failing` at warning, 97% critical, one alert per mount point (finding `filesystem_full/<mount>`), ended at
  the first heartbeat that reports it under 90%, as the other inputs of `storage_failing` end at the first read that no longer finds
  them; no hardware record and no cordon at any level. The rule is a function HH5's derivation calls with each host's last health,
  as the coordinator stores no health before HH5.

**The BMC's event log, as built** (HH8; Built: `crates/hostkeeper-daemon/src/sel_source.rs`, the reader; `sel.rs` and
`sel_tables.rs`, the decoder and its tables; `sdr.rs`, the sensor names; the requests in `ipmi.rs`, `BmcRequest`). The reader is a
source of `crate::hardware_sources` (`for_host` lists it on Linux, after SMART, hwmon, rasdaemon and the kernel sources), on a worker of its own, every 60 s,
and after a read that failed twice as long, up to 300 s; its one mark holds the SEL's and the open `bmc_missing`. Each read asks
`Get Device ID`; a host with no `/dev/ipmi0` is source `sel` `unsupported`, a BMC that does not answer `unreadable`, and the
second such read in a row holds one `bmc_missing` record (`host`, subject `BMC`, `warning`, `asserted`), its `deasserted` record
(`about` it) held at the first read a BMC answers again; a daemon that may not open the device is `unreadable` and raises nothing.
A record's `event_id` is `sel:<record id>@<the SEL's last erase time>`; its `raw` is the entry as `ipmitool sel list` prints it,
the SDR's sensor name after the sensor (`01a4 | 11/26/2026 | 03:00:12 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted`),
and its `subject` that name with its underscores read as spaces (`DIMM D1`), or the sensor's type and number when the SDR does not
name it. A correctable ECC entry, a correctable machine check and a correctable bus error are `info`, counted per subject and
hour into one record (2.1), held once the hour is over; "correctable ECC logging limit reached" is the `warning` of the table. A
condition (a threshold, a failed or absent power supply, a disabled memory device, a throttled part) is `asserted`; any
deassertion is an `info` record `deasserted`, `about` the assertion of the same generator, sensor, event type and offset when the
reader saw it. A temperature past any threshold is a `warning` (`thermal_critical` past a critical or non-recoverable one,
`thermal_threshold` past a non-critical one); a fan below a lower critical or non-recoverable threshold `critical`
(`fan_failed`), past any other `warning`. The SDR is read whole (reservation 0) only when its last addition or erase changed. The
fixtures are written byte for byte from the IPMI 2.0 tables (`crates/hostkeeper-daemon/tests/fixtures/sel` and `sdr`); a capture
from each host model is section 10's.

**Guards.** No capacity compare in a kdump boot (`/proc/vmcore` exists, or the command line has `elfcorehdr`). During a maintenance
run the compare is deferred to the first report after the run, not dropped: a reboot that loses a DIMM is exactly the case. SEL
deassertions are `deasserted` records, and the "log cleared" and "system boot" entries are `info`. Physical devices only: no `loop`,
`ram`, `zram`, `dm`, `md`, `nbd`, `sr` or removable USB disk; a NIC only if it has a `/device` and had carrier in the baseline. CPUs
compare `present` and `smt/control`, so turning SMT off is not a lost core. (Built, the daemon's side of these filters:
`crates/hostkeeper-daemon/src/capacity.rs`, which also leaves out a disk attached through USB and an SR-IOV virtual function, and in a
kdump boot sends no capacity at all; the compare is HH5's.)

### 3.2 macOS (Apple silicon)

Macs use their built-in sources (decision 11). **Memory errors on Macs are out of scope** (decision 5): Apple silicon has no DRAM
error counters, no per-module data and no SMBIOS table a program can read, so a Mac's log has no memory error record and its page says
`memory errors: not monitored on Apple silicon`. **Apple Diagnostics** (the hardware test run by holding the power button at start-up)
needs a person at the Mac and gives its result on the screen only; no daemon can start it or read it.

| Source | What is read | Normalized to | Severity | Poll |
|---|---|---|---|---|
| **Panic reports** | `/Library/Logs/DiagnosticReports/*.panic` and the `.ips` reports whose header `bug_type` is `210` (other `.ips` reports are app crashes; `Retired/` not read), each once by its `incident_id` | `board`, `panic_hardware` for SError or AMCC wording; `panic_software` for an SoC or userspace watchdog, a kext or other software (a runaway build can trip a watchdog) | hardware C; software W | at start, 2 minutes later, then every 5 minutes; on a first install those older than 7 days are `info` history (4.5) (Built: the parser and classifier `crates/hostkeeper-daemon/src/mac_panic.rs`, the reader `mac_health.rs`) |
| **Unexpected-shutdown records** | a new `kern.bootsessionuuid` with no clean-shutdown marker (section 4.4); `raw` the panic line of that boot's panic report, or the boot's id when it has none (the unified log is not read, below) | `host`, `unexpected_reboot`, merged with a panic of the same boot (its class in the message) | W | once a boot, judged 2 minutes after the daemon's start so macOS has written the panic report (Built: `mac_health.rs`) |
| **`diskutil` SMART status** | `diskutil info -plist disk0`: `SMARTStatus` | `disk`, `disk_failing` asserted at `Failing`, deasserted at `Verified`; `Not Supported` is `unsupported`, an open assertion kept; a verdict of another word `unreadable` (Built: `crates/hostkeeper-daemon/src/mac_disk.rs`, source `diskutil`) | C | every 15 minutes |
| **Thermal pressure** | `pmset -g therm` (no root): a thermal or performance warning level above 0, or a CPU speed or scheduler limit under 100, is a limit; `powermetrics --samplers thermal -n 1` only once `pmset` shows a limit, at most every 5 minutes, its `Current pressure level` the sample (a sample within 5 minutes of its last run takes its level) [V: `-n 1`, and the words of both on a real Mac] | `thermal`, subject `thermal pressure`, code `thermal_pressure`: asserted after 5 samples in a row at heavy or worse, a warning that a trapping sample escalates to a critical assertion about it; deasserted after 10 minutes nominal, at the assertion's severity (Built: the pure rule and the source, `crates/hostkeeper-daemon/src/mac_thermal.rs`, source `thermal`) | heavy W; trapping and sleeping C | 60 s |
| **`system_profiler` inventory changes** | `system_profiler -json SPHardwareDataType SPNVMeDataType` (the installed memory and processors, and each internal disk by its BSD name, size and serial, which `SPStorageDataType`'s volumes do not give), `sysctl -n hw.memsize hw.physicalcpu hw.logicalcpu`; the source state `system_profiler` (Built: `crates/hostkeeper-daemon/src/mac_capacity.rs`, read by `mac_health.rs`; the GPU cores and `hw.perflevel0` have no `Baseline` field yet and are not read) | the capacity of the boot; the coordinator records a change as `capacity_short` (`baseline`). On a Mac it compares `hw.memsize`, the CPU and GPU core counts and the disk's size, with no DIMM list (Apple silicon has none): a capacity compare, not a memory error watch. A Mac short of its baseline is cordoned for repair as a Linux host is (section 6) | W | at start, 2 minutes later, then daily |

Not used on a Mac: `log show` (slow and heavy on a build host; `SMARTStatus` and the panic classes cover the serious cases), IOKit wear
data, `smartctl`, MDM and Apple Business. Each parser is written from a capture on a real Mac first [V].

### 3.3 The tools, and how they are installed (decision 8)

| Tool | What it does | Platform | What Macs use instead |
|---|---|---|---|
| **smartmontools** (`smartctl`) | reads a disk's own health record (SMART): whether the disk says it is failing, its media errors, reallocated and pending sectors, an NVMe disk's spare and wear | Linux | `diskutil`'s `SMARTStatus`, part of macOS |
| **rasdaemon** | listens to the kernel's hardware error events (memory errors by DIMM, CPU machine checks, PCIe errors) and decodes them into a database that keeps them across reboots | Linux only | nothing: Apple silicon reports no memory or CPU errors; the panic reports cover what reaches a crash |
| **a persistent journal** | systemd's journal kept on disk (`/var/log/journal`) rather than in memory, so the log of the boot before a crash can be read after it | Linux only | the unified log, which macOS keeps across reboots by default |

All three are installed on every Linux host **through the config package** (decided): the config package of hostkeeper's own service
in each host group ([hostgroups.md](hostgroups.md) section 6.1, `hostkeeper-config-<group>`) declares `Depends: smartmontools,
rasdaemon` and creates `/var/log/journal`, which makes journald, at its default `Storage=auto`, keep the journal on disk. That
directory is the one path the package owns outside `/etc/hostkeeper/` [V: that the package rules of hostgroups.md section 6.1 allow
it, or a journald drop-in instead]. A config package is the operator's own, built by the operator's CI (hostgroups.md section 6.1), so
this repository ships the template of those lines and the operations page that says how to add them (HH16); the tools then arrive as
every package does, by the group's plan, and leave with the group. A group with no `hostkeeper-config-<group>`, or one without the
lines, gets none of the tools, and its hosts say so on their pages: `host_not_monitored`, which is turned on with HH16, once the
template exists, so a fleet is not flooded before its config packages carry the tools. hostkeeper reads the tools; it never configures
what they record. Macs need nothing installed.

## 4. The daemon side (New)

### 4.1 What the daemon reports

A new optional message on the heartbeat, `HostHealth` (`HeartbeatRequest` field 21, New: fields 1 to 20 are taken), sent once the
coordinator's answer has advertised the feature `host-health-v1`, the mechanism of `maintenance-v1` (Built: the daemon's features and
the coordinator's `OFFERED` in `crates/hostkeeper-coordinator/src/features.rs` each gain the name). It goes on the gRPC route only
(the JSON route is frozen) and counts toward the heartbeat's bound on both, as the trusted root does (Built:
`crates/hostkeeper-link/src/beat.rs`).

```proto
message HostHealth {
  repeated HardwareEvent events = 1; // records not yet acknowledged by the coordinator, at most 6 in one beat: the most
                                     // severe first, then the oldest
  Baseline baseline = 2;             // the host now: mem_total_kib, the SMBIOS memory devices (at most 32: locator and size),
                                     // cpus_present, cpus_online, smt, disks (at most 16: size and an 8-byte digest of the serial),
                                     // nics (at most 8: name and speed)
  string boot_id = 3;                // Linux boot_id, or a Mac's kern.bootsessionuuid
  uint64 boot_unix = 4;
  bool clean_previous_shutdown = 5;
  repeated SourceState sources = 6;  // per source: ok, unsupported, unreadable (with its reason)
  repeated FilesystemUse filesystems = 7; // at most 8, the root first: mount (at most 32 bytes), size_bytes,
                                     // available_bytes, as statvfs reads them in this beat (Built, section 3.1)
}
message HardwareEvent {              // section 2.1, less what the coordinator adds (host_id, machine_serial, observed_unix, by)
  string event_id = 1;               // at most 64 bytes
  uint64 time_unix = 2;
  bool of_previous_boot = 3;
  Component component = 4;           // MEMORY, CPU, PCIE, DISK, THERMAL, POWER, BOARD, HOST
  string subject = 5;                // at most 32 bytes
  Severity severity = 6;             // INFO, WARNING, CRITICAL
  EventState state = 7;              // EVENT, ASSERTED, DEASSERTED
  string source = 8;                 // at most 16 bytes
  string code = 9;                   // at most 32 bytes
  string message = 10;               // at most 120 bytes
  string raw = 11;                   // cut to 256 bytes
  string about = 12;                 // at most 64 bytes
}
```

And on `ServiceBeat` (New; fields 1 to 15 are taken): `health_reason` 16 (the observer's reason token, Built:
`Observation::reason_token`, which only the JSON route's `health_detail` carries today), `health_body` 17 (the first 160 bytes of a
non-2xx reply, so js-agent's `503 not placeable: <reason>` reaches the coordinator), and `restarts` 18 (the restart counter, the
restarts in the last 10 minutes, the last result and exit status).

**Held until written** (Built: `crates/hostkeeper-daemon/src/health_state.rs`, the queue, and `host_health.rs`, the
`HostHealthSource` of `crates/hostkeeper-link/src/host_health.rs`, which no coordinator advertises yet). The daemon keeps every
record it has made in a queue of its state file (at most 256; past that the oldest
`info` records of a source are folded into one `info` record that counts them, and no warning or critical record is ever folded). It
sends at most 6 of them in a beat, the most severe first, then the oldest. The coordinator writes each to the log by its `event_id` and
answers with the ids it wrote (New: `HeartbeatResponse.health_written`, an answer field sent only to an agent that announced
`host-health-v1`, so it joins `GATED_FIELDS` of `features.rs`, Built); the daemon then drops them from its queue. A record whose beat
was lost, or that a coordinator restart missed, is sent again, and the conditional insert writes it once. A burst of 20 SEL entries
takes four beats, and loses none. A condition stays in the log as asserted until its `deasserted` record is written, so nothing needs
sending twice to stay visible.

**Size.** The heartbeat's bound is 16 KiB (Built: `MAX_HEARTBEAT_BYTES`), and the maintenance report (about 5 KiB) and the
prerequisites report (about 4 KiB) are counted and never dropped (Built: `beat.rs`). So `HostHealth` is at most 4 KiB encoded
(`check_health`, checked on both sides like `check_report` of the maintenance report, Built: `crates/hostkeeper-proto/src/maintenance.rs`):
6 records of at most about 600 bytes (their strings at most 328 bytes and `raw` 256, with tags), 3.6 KiB, and the baseline without its
memory devices, about 350 bytes. Together the three never-dropped reports are at most 13 of the 16 KiB. When a beat is still over,
`fit`'s `DROP_ORDER` (Built) gains two groups, dropped first: the memory devices, the `sources` and the `filesystems`, then the
services' `health_body` and `restarts`; and the beat sends fewer records, which wait in the queue for the next. The memory devices are sent in every beat that
has room for them (Built, section 4.3). A size test with 16 services and every part at its bound shows the beat fits [V].

**Expected sources.** Every Linux host is expected to have a BMC (decision 6), rasdaemon, smartmontools and a persistent journal
(3.3); a Mac the sources of 3.2. An expected source reported `unsupported` or `unreadable` raises `host_not_monitored`.

### 4.2 How each is read

The workspace forbids `unsafe` (Built: `unsafe_code = "forbid"`; `ipmi.rs` says why its `ioctl`s are `ipmi_rs`'s), and the daemon
runs as root. **Native**, from files and existing crates, each parser tested on fixtures: rasdaemon's database (read-only SQLite, through
the daemon's own reader of the file format, Built: `crates/hostkeeper-daemon/src/sqlite_read.rs`, 4.5), the
journal's kernel lines [V: through `journalctl --dmesg --boot=<n> --output=json`, a delegated program, if no reader of the journal's
files fits the workspace's crates], `/dev/kmsg`, the SEL and SDR through `ipmi_rs` with a decoder for the sensor types above (the
IPMI 2.0 event tables), sysfs and proc, the SMBIOS memory devices (`/sys/firmware/dmi/entries/17-*/raw`, the table `dmidecode --type
17` decodes), the boot id and the marker; on a Mac the panic report parser (JSON and text; falling back to the file name and the first
`panic(cpu` line), the `runs` ring and the `sysctl`s. The `runs` ring (Built: `crates/hostkeeper-daemon/src/mac_runs.rs`)
is a Mac's count of its services' restarts: launchd keeps no `NRestarts`, so the observer reads each job's `runs` (the times launchd
spawned it since it was loaded) from `launchctl print`, less the first, and the daemon keeps the counts at which it changed in its
state file (4.4), from which each heartbeat's `ServiceBeat.restarts` (count, the restarts of the last 10 minutes, the last result and
exit status) is made as a Linux host's is; a count that fell (a `bootout` and `bootstrap`) starts the job again, and a `kickstart`
counts as a restart.

**Delegated**, through the `Launchctl` port (Built: `LaunchctlProcess` of `crates/hostkeeper-launchd`, which
`crates/hostkeeper-daemon/src/mac.rs` uses on a Mac and which already runs programs on Linux: `maintenance_linux/apt.rs`, `dnf.rs` and
`rebooter.rs` of `crates/hostkeeper-daemon/src`): a fixed argument list, no shell, an empty environment, a 64 KiB output cap and a
kill at the deadline. `smartctl --json=c --nocheck=standby --info --health --attributes` (the `ioctl`s would need `unsafe`; never `-t`
or any option that writes; its exit status bits read as `smartctl(8)` documents; Built: `smart.rs`, through `LaunchctlProcess`, each
disk killed at 60 s; a `smartctl` that is not installed is the source `unsupported`, never an error), and on a Mac `diskutil`, `pmset`, `powermetrics` and `system_profiler`. Not used:
`mcelog`, `edac-utils`, `lm-sensors`, `smartd`.

### 4.3 The memory baseline: installed RAM, the DIMM list and `MemTotal` (decision 10)

**Installed RAM is not `MemTotal`.** The DIMMs installed (256 GiB, say: 16 modules of 16 GiB) are what the inventory records. The
kernel's `MemTotal` is the RAM it can hand out, and it is always a little less: the firmware keeps ranges for itself (ACPI tables, the
BMC's and the firmware's own reserved areas), and the kernel keeps its own image, its early page tables and, when one is configured, a
crash kernel's area. The gap is stable from boot to boot on the same host with the same firmware and kernel command line; a kernel
update moves it by a few MiB, a `crashkernel=` change by up to its size. The incident host read 268320876 kB (255.9 GiB) before the
fault, about 110 MiB under 256 GiB [V: the host's installed RAM, from its inventory], and 251543660 kB (239.9 GiB) after: 16777216 kB
less, exactly 16 GiB, one DIMM.

The daemon reports the capacity in every beat (Built, on Linux: `crates/hostkeeper-daemon/src/capacity.rs`, read from `/proc` and
`/sys` for each heartbeat, the SMBIOS memory devices decoded from `/sys/firmware/dmi/entries/17-*/raw` with no `dmidecode`, sent in
every heartbeat that has room for them (the daemon is not told which heartbeats arrived, so a count of first heartbeats could be
spent on lost ones), with the source state
`smbios`: `unsupported` with no entries, `unreadable` when an entry does not decode; on a board of more than 32 slots the populated
ones go first); the coordinator keeps the baseline per host in its operations store (New: a table of
both backends; the files-store coordinator keeps its fleet in memory, and the Postgres one only each host's last beat, so a baseline
must be a record of its own) and judges memory by two comparisons:

- **(a) The DIMM list against the inventory.** The inventory record gains the installed memory, `installed_memory_bytes` and
  `memory_modules` (New: `InventoryRecord` fields 18 and 19 of `enrollment.proto`; fields 1 to 17 are taken; set with
  `Enrollment.UpdateRecord`, Built, and shown on the Inventory page). The populated SMBIOS slots must number `memory_modules` and their
  sizes must sum to `installed_memory_bytes`, exactly: SMBIOS reports module sizes, not usable memory, so there is no tolerance. A
  slot empty or smaller is a `capacity_short` record naming the slot. A firmware that maps a failed module out may still list it [V:
  how each board reports a disabled DIMM], which is why (b) exists.
- **(b) `MemTotal` against the boots before.** A record is written when `MemTotal` is below the baseline's high-water `MemTotal` (the
  highest of the boots since the last rebaseline) by more than **1% or 1 GiB, whichever is larger**: on a 256 GiB host, 2.56 GiB. The
  incident's drop of 16 GiB (6.25%) is six times that; a kernel update or a `crashkernel=` of up to 1 GiB is inside it. Against the
  high-water value, a second short boot does not make the short value normal. The record shows the last boot's value too.
- **Debounce.** Both are judged on two reports at least 2 minutes apart after a new boot id, since early-boot values can settle.
- **The first baseline.** A host whose inventory has `installed_memory_bytes` is judged by (a) from its first report, and its
  `MemTotal` must be at least 95% of the installed memory [V: measure the gap on each model]. A host whose inventory has none stores its
  first report as `unconfirmed`: shown, not judged, until an operator confirms it or fills the inventory. So a host first seen already
  short is never baselined at its short value.
- CPUs, disks and NICs are judged against the high-water baseline the same way, with no tolerance but the guards of 3.1. A rebaseline
  (5.2) lowers the baseline, with notes.

### 4.4 The state file and the clean-shutdown marker

Built: `crates/hostkeeper-daemon/src/health_state.rs` (the file, `health-state.json` in the state directory),
`host_health.rs` (read at start, `running`, `clean` at SIGTERM, the boot and machine ids), `shutdown_probe.rs` (the probes below; on
OpenRC the runlevel OpenRC is changing to, its `softlevel` file reading `shutdown` or `reboot` [V]) and `terminate.rs` (the
daemon's SIGTERM, which `start::serve_until` waits for). The high-water marks are each source's own text, kept by the sources of HH8
to HH12; the baseline is HH4's. A Mac and a Linux host with no `/etc/machine-id` key the file by nothing, which any machine reads.

The daemon keeps a state file in its state directory, written whole (write, fsync, rename), keyed by machine id: the boot id, the
baseline, the high-water marks (the SEL's last record id and add time, rasdaemon's ids, the panic reports' incident ids, the pstore
names, the journal's cursor), the clean-shutdown marker and the events still held; on a Mac also the `runs` ring (4.2, Built), so a
restart of the daemon keeps its services' recent restarts. At start the daemon writes `running`. At SIGTERM it
writes `clean` with the boot id only if the system is really going down (Linux: `systemctl is-system-running` answers `stopping`, or
a reboot, poweroff or halt job is queued); a SIGTERM of a daemon restart or of hostkeeper's own self-update must not count, or a later
crash would read as a clean reboot. On a Mac launchd also sends SIGTERM at a bootout and no shutdown probe is known [V], so the marker
is advisory there and `unexpected_reboot` needs a panic report, an abnormal previous-shutdown cause or a heartbeat gap to agree. A
Mac's daemon holds that evidence as its own record (source `shutdown`, 3.2, Built: `crates/hostkeeper-daemon/src/mac_health.rs`
over `HealthKeeper::unclean_previous_boot`): one per boot id, for a boot whose first start found the file last written in another
boot with no `clean`; the file keeps that boot before (`previous_boot_id`, `previous_boot_unix`), so a restart of the daemon before
the judgement still makes it; a first start on a host, or a file moved aside, has no boot before to judge. A new
boot id with no `clean` is one `unexpected_reboot` record per boot id, merged with the SEL's "power cycle", "OS critical stop" and
"memory device disabled" entries and the pstore check. A reboot of an unattended update or of an operator raises nothing.

### 4.5 Read-only, and bounded

The daemon runs as root on both platforms (Built: `packaging/linux/hostkeeper.service` names no `User=`, and the Mac job is a launchd
daemon; "the one root process of a host", [two-programs.md](two-programs.md)). The health look uses that only to read; the one thing
that changes the host, the cordon, is a maintenance run, which runs each service's own `drain` hook (section 6).

- **Read-only.** No pstore record removed, no SEL cleared, no rasdaemon row touched, no self-test, no `fsck`, no write to any sysfs
  file. A SMART self-test or `verifyVolume` is an operator's maintenance run, never a poll.
- **IPMI.** `Get Device ID`, `Get SEL Info`, `Get SEL Entry` with reservation 0 and the SDR reads (`Get SDR Repository Info`,
  `Get SDR` with reservation 0), and nothing else (Built: the closed table `BmcRequest` of `crates/hostkeeper-daemon/src/ipmi.rs`): `Reserve SEL` can
  cancel another tool's reservation, and `Clear SEL` and `Delete SEL Entry` destroy evidence. The requests are one static table, and a
  test asserts that nothing else can be sent. Calls run on a blocking thread with a deadline (an `ioctl` on a wedged BMC can hang), one
  at a time under a lock shared with the existing BMC address reads, with backoff after a timeout. Built: the requests are sent raw
  through `ipmi_rs`'s device wrapper, as the address reads are, each answer awaited at most a second (`ANSWER_TIMEOUT`), under one
  lock of `ipmi.rs`, on the reader's own thread, whose waits double after a failed read up to 300 s.
- **Programs.** Every program the health look can run is an entry of one static table of an absolute path and fixed arguments (the
  pattern of the probe's `STEPS`, Built: `crates/hostkeeper-daemon/src/maintenance_hook/probe.rs`), with one New slot for a disk or a
  label whose value must be a name the host listed (a `/sys/block` name of `^[a-z0-9]+$`, a unit of a manifest) and may not start with
  `-`.
- **rasdaemon** (Built: `crates/hostkeeper-daemon/src/sqlite_read.rs`). Its database is read by the daemon's own reader of SQLite's
  file format, not by SQLite: the Linux build is static for musl and compiles no C but ring's (`.cargo/config.toml`), and SQLite's C
  needs a libc. The reader opens the file read-only and takes no lock, so it cannot block rasdaemon's writer; instead a read counts
  only when no hot rollback journal (`ras-mc_event.db-journal` holding a transaction) is beside the file before and after it and the
  file's change counter did not move during it, and is tried again otherwise, three times in all. Short reads: at most 256 rows at a
  time, each its own read, and 4096 rows of a table in one poll (all of them on the source's first read, so its history is judged at
  once). A database written at every attempt, in WAL mode (whose newest pages are not in the file), in UTF-16, or malformed is an
  `unreadable` source with its reason, never a crash; no file, or none of its tables, is `unsupported`. Pages are read one at a time,
  only those a read walks; a b-tree that loops or is deeper than 32 levels, and a record over 1 MiB, are malformed.
- **Deadlines.** Every read has a deadline, an output cap, and a failure of its own that is recorded in `sources` and never fails the
  heartbeat. Each source has its own worker, so a hung read stops that source only (Built: `crates/hostkeeper-daemon/src/hardware_sources.rs`,
  a thread per source started with the daemon, its state kept by `HealthKeeper::read`; when the states and six records would pass
  the 4 KiB of `HostHealth`, the beat carries fewer records).
- **Cost.** A few dozen small reads a minute; the SEL is read in full only when its count or add time changes. Programs: `smartctl`
  hourly; on a Mac `launchctl print` and `pmset` every 60 s, `diskutil` every 15 minutes, `powermetrics` at most every 5 minutes and
  only under a thermal limit, `system_profiler` at start, 2 minutes later, then daily.
- **History is not news.** The first run on a host with an old SEL, old rasdaemon rows or old panic reports writes those older than 7
  days as `info` records marked history, which raise nothing (Built: `HealthKeeper::record` of
  `crates/hostkeeper-daemon/src/host_health.rs`, for the first read of each source on a host, the message starting `history: `). A cleared SEL (record ids start again, the count falls) resets the marks.
  A BMC timestamp from before its clock was set after a power loss ("pre-init") is ordered by record id instead.
- **macOS.** Panic reports need no privacy grant [V]; the daemon already reports whether it has Full Disk Access (Built:
  `crates/hostkeeper-daemon/src/full_disk_access.rs`).

## 5. Alerts, derived from the log (New)

### 5.1 The kinds

Alerts stay derived, as today (Built: `alerts.rs`, one function per kind, `Subject` the identity); the new hardware kinds are derived
from the log. `alerts.rs` is at 998 lines (R12), so HH0 (section 12) first moves `Kind` and its `log_token`, `severity` and `proto` to
their own module; the new derivations go in `alerts_health.rs`.

| Kind (log token) | `AlertKind` | Derived from | Severity | Cordons (6) |
|---|---|---|---|---|
| `hardware_fault` | 25 | an `event` record of the log of the last 7 days, or an `asserted` one not yet deasserted, of component `memory`, `cpu`, `pcie`, `thermal`, `power` or `board`, at warning or above | the record's | when critical |
| `capacity_regressed` | 26 | an asserted `capacity_short` record not deasserted | warning (decision 4) | memory, CPU or disk |
| `storage_failing` | 27 | a `disk` record (failing, media errors, I/O errors, a filesystem error or a read-only remount), or a filesystem nearly full (90% W, 97% C; not a record) | the record's | a critical `disk` record |
| `unexpected_reboot` | 28 | an `unexpected_reboot` record of the last 24 hours | warning | no |
| `service_unhealthy` | 29 | a managed service unhealthy for `--alert-unhealthy-secs` (default 600) | warning; critical after 30 minutes | no |
| `service_crash_loop` | 30 | restarts or `runs` past `--alert-restarts` (default 5) in 10 minutes | critical | no |
| `host_not_monitored` | 31 | an expected source (4.1) `unsupported` or `unreadable` | warning | no |
| `bmc_missing` | 32 | a `bmc_missing` record not deasserted: a Linux host whose BMC is absent or does not answer (decision 6) | warning | no |

The two service kinds are not hardware: they come from the services' health (Built: the health of each `ServiceBeat`, with the New
reason, body and restarts of 4.1), not from hardware records. But for every alert that names a host, of any kind (these two, and the
Built kinds such as `failed_update`, `host_halt` and `silent_host`), the coordinator writes an `alert_raised` record to the host's log
when it is first observed and an `alert_cleared` record when it ends (5.2), under the component its records name, else `host`. So the
host's log holds the whole history of its alerts (decision 7), hardware or not.

**`silent_host` becomes critical** (decision 4: a host that is completely off or silent is critical): its arm moves from the warning
list to the critical list of `Kind::severity` (Built: `alerts.rs`). Its identity, meaning, threshold and log token do not change, and
the coordinator writes a `host` record to the log when it begins and ends. This is the one change to an existing kind.

The `ALERT_KIND_*` values and their log tokens are added to `operator.proto`, nothing renumbered (B8). Each threshold is a coordinator
flag with the default above (S2: flags, not environment), like the alert flags of today (Built: `--alert-silent-host-secs`). The
`Alert` message (Built: fields 1 to 14) gains `last_seen_unix` 15, `acknowledged` 16, `likely_cause` 17, `events` 18 (the `event_id`s
of the log records it is derived from) and `finding` 19 (below).

**Identity.** The `Subject` (Built: kind, service, host, version, request, other host, group) gains one part, `finding`: the records'
`code` and `subject` (`memory_uncorrectable/DIMM D1`), and for `unexpected_reboot` the boot id, so two DIMMs' faults are two alerts and
the same DIMM's hundredth record is one. The new kinds are not written to the `alerts` rows (Built: `store/postgres/alert_rows.rs`,
keyed by the first six parts), which decide which coordinator logs an alert: a coordinator of the release before deletes every row it
did not derive. Their once-logging is the log's own conditional insert (2.3).

`service_unhealthy` keeps an `unhealthy_since` per host and service in the operations store (New, both backends), the same on every
coordinator and kept across a restart. It counts `HEALTH_UNHEALTHY`, including a health URL that refuses connections, which is the
crash-loop case. It is not raised during a maintenance turn of the host, a repair cordon (6), or the bake of a rollout of the service
(Built: `OpsState.maintenance_runs`, the plan stages); and when at least 3 hosts and half or more of a host group's hosts are unhealthy
for one service at once (a lease-server outage answers 503 to every js-agent), it is raised as one alert of the group.

### 5.2 Clearing, acknowledging, notes: records of the same log

- **Clearing.** An alert ends when its records no longer derive it: the condition is deasserted, or the event is older than its
  window, or the service is healthy again. Today the book forgets an alert and logs nothing; New, the coordinator writes a record
  `alert_cleared` (`about` the alert's first record) and a `cleared` log line (`alert=cleared kind=<kind> host=<host>`).
- **Silence.** A host that stops beating keeps its records and its alerts, shown "as of <time>"; records are not cleared by silence.
- **Acknowledge, rebaseline and put back** are the coordinator's: an acknowledgement held by the daemon is lost with its state file and
  cannot be given to a host that is down. **Any operator** may do each, and **each needs notes on what was done** (decision 3): the call
  is refused `notes_required` without them, and a CI token is refused all three, as it is `Alerts.List`. Each writes a `note` record
  to the host's log (`source` `operator`, `by` the operator, `about` the alert's first record, `message` the notes) and is audited
  under the caller's name, the notes as the entry's reason. New RPCs of `operator.proto`, and so new verbs of `hk`, whose commands are
  generated from the protos (Built: `ctl_gen.rs` of `hostkeeper-proto`): `hk alerts ack --host-id <host> --finding <code/subject> --notes <text>` (the alert's `finding`, so two DIMMs are told apart)
  and `hk alerts rebaseline --host-id <host> --notes <text>`; putting a host back is the Built `Maintenance.PutBack`
  (`hk maintenance put-back`), whose `reason` becomes required for every turn and is written to the log as the put back's notes.
- An acknowledged alert is still derived but flagged: it leaves the banner, and comes back as new on a new boot id or a worse
  severity. A rebaseline replaces the stored baseline (and confirms an `unconfirmed` one), and the `capacity_short` record is
  deasserted.
- **The history is the log** (decision 7): every record, every alert's raising and ending, every note, for good.

### 5.3 Linking to effects

The effect (the job agent's crash loop, the lost host) was visible; the cause was not. The coordinator joins by host and boot:

- A `service_unhealthy` or `service_crash_loop` alert gets a **likely cause** when the same host has an open `capacity_regressed`,
  `hardware_fault` or `unexpected_reboot` of the same boot id, or a record `of_previous_boot` at most `--alert-cause-window-secs`
  (default 600) before that boot. Order is not required: the daemon may report the short memory after the service already went bad.
  The cause's text then names its effects, and the effect points at its cause.
- A failed update (Built: `failed_update`) on a host with an open hardware alert, begun after that alert's boot, says so in its
  `likely_cause`. A host cordoned for repair takes no new rollout orders until it is put back (6).
- js-agent's `/healthz` is the health URL its package declares (Built in komira-ai/jobscheduler: `crates/js-agent/src/health.rs`, `503`
  with `not placeable: <reason>`, and `healthUrl` in `packaging/js-agent/deb/README.md`), so its reason reaches the coordinator in
  `health_body` (4.1). js-agent also offers the job service the memory and CPU it detected (Built there: `crates/js-agent/src/host.rs`,
  `detect`); the host's page shows both numbers when they disagree with the baseline.
- A planned maintenance reboot raises no `unexpected_reboot`; a capacity drop found after it is raised.

A link is of time and place, and says "likely": a wrong link is worse than none, so a nearly-full disk and every warning are never a
cause.

## 6. Bad hardware cordons the host, through the drain path (decisions 2 and 9)

When a host gets an alert of the table's "Cordons" column, the coordinator cordons it for repair at once, with no person in the loop,
through the maintenance run's drain (Built: `hostkeeper.operator.v1.Maintenance`,
`crates/hostkeeper-coordinator/src/maintenance_runs.rs`, the turn steps of `MaintenanceHostStep`), so the host leaves service exactly
as it does for a maintenance window:

1. The coordinator schedules a run of one host, of a New kind `MAINTENANCE_KIND_REPAIR` (6 of `enum MaintenanceKind`; 0 to 5 are
   taken), as itself (audit actor `coordinator`, reason `repair: <kind> <finding>`), with no window, no batch, `take_last_host` (a
   broken host serves nothing, so the group's serving floor does not hold it back) and no drain deadline. A `note` record says so in
   the host's log.
2. The host's turn drains as every turn does (Built: the step `draining`): the daemon runs each drained service's `[hook] drain`. For
   js-agent that is its own cordon on the lease server's own-host route, with the host's own credential, and its hold file (Built there,
   [maintenance-updates.md](maintenance-updates.md) sections 5.3 and 5.4): running jobs finish, no new lease starts. A drain waits for
   its work by default and cuts nothing.
3. A repair turn has no apply step. Once drained, it stops at the Built step `halted`, with the reason `held_for_repair`: the state
   maintenance-updates.md sections 5.4 and 5.7 already define ("the host stays halted and cordoned until Put back"), with no resume
   and no rollback. js-agent's hold file keeps the cordon across a reboot and while no hook runs.
4. The operator repairs the host and puts it back with `Maintenance.PutBack`, notes required: the daemon runs each drained service's
   `resume`, js-agent uncordons, the turn ends `done`, and the notes go into the log. Retry and Cancel are not offered on a repair turn.

**What the job service sees** is a drain that does not come back until an operator puts the host back (decision 9): a cordoned host
with no new work, its jobs finishing. hostkeeper tells the job service nothing else, and no change in jobscheduler is needed.

**Rules.** One repair run per host at a time; a second cordoning alert of the same host joins the open turn's reason. A host already in
a maintenance turn has the repair run queued behind it (one turn per host, Built). A host that is silent or off is not reachable to
drain: its repair turn waits at `waiting` and drains when the host beats again, and a host that is off holds no leases. While cordoned
for repair, the host takes no new rollout orders and no maintenance run but its own. Put back with the hardware alert still open is
allowed (the operator may judge the alert wrong), and the notes say so; the alert stays until its records end it.

## 7. The console: how a person is told (decisions 1 and 12)

For now a person is told on the console alone: no webhook, mail or other channel leaves the coordinator (section 11 keeps the
notification system as an end-state question). Built today: Home's alert strip (`alertsStrip` of `ui/pages/home.js`) lists every
alert, and `ui/health.js` turns the alerts that name a host, and its unhealthy services, into the host's reasons. New:

- **A banner** at the top of every page while any host has a critical alert, and on Home while any host has a warning of the new kinds:
  "2 hosts need attention", one line each with the alert's first line, by time, linking to the host. An acknowledged alert leaves it.
  The alert strip stays. The banner reads `Alerts.List`, which `ui/calls.json` then lists for every page.
- **A badge per host**: red for critical, amber for warning, grey for sources not monitored, blue "cordoned for repair", on the fleet
  and the rack and lab views ([ui-shell-lab.md](ui-shell-lab.md)). `HEALTH_ALERTS` of `ui/health.js` gains the new kinds.
- **The host's page**: a **Hardware** panel with the baseline (memory against the inventory, the DIMM slots, CPUs, disks, NICs, as of
  which boot), each source's state, the repair cordon's state, the open alerts with their records, and the **hardware event log**,
  newest first, filtered by component and severity, each record with its normalized message and, unfolded, its raw line;
  **Acknowledge**, **Re-baseline** and **Put back** buttons, each with a notes box it will not send empty.
- `hk alerts list` lists the alerts, with `--host-id` and `--severity` filters (New); `hk hardware log` lists the log (2.3).

What a line reads like: one host; what happened; cause before effect; numbers with units; what to do; the raw line below, never a bare
error code. Section 1 shows the incident's. Others: `host-a: no BMC answers; hardware events cannot be read` (warning); `mac-1:
unexpected reboot, panic: SError, possible hardware fault` (critical; cordoned); `host-b: disk nvme1n1 failing (SMART critical
warning); 14 media errors since last week` (critical; cordoned); `host-c: /var/lib/js 97% full (12 GiB free)` (critical; not
cordoned: a full disk is not bad hardware).

## 8. Tests

All in Rust, in the crate of the code, over a fake of each source and recorded fixtures, with no real hardware: scratch directories
for `/proc` and `/sys`, a SQLite fixture of rasdaemon's tables, recorded journal output, a fake IPMI device behind the port of
`ipmi.rs`, fake programs answering recorded output, an injected clock. None times anything (T10, T12). Fixtures have generic host
names, serials and addresses.

- **Normalization, per source.** Each source's fixture gives records of exactly the expected `component`, `subject`, `severity`,
  `state`, `source`, `code`, `message` and `raw`, compared whole: rasdaemon rows of `mc_event` (corrected and uncorrected, by DIMM
  label), `mce_record`, `aer_event`, `mf_event` and `disk_errors`, a database being written and one in WAL mode (`unreadable`), no
  database and no table (`unsupported`), and an empty table (read); `smartctl --json` for pass, fail, low spare,
  `critical_warning`, media errors grown, a disk in standby, missing, timeout and over the cap; journal lines of an MCE, an EDAC error,
  a fatal and a corrected AER error, a BERT record of the previous boot, an I/O error, and unrelated lines (no record); SEL entries of
  memory (named by the SDR), memory disabled, processor, PSU, fan, temperature asserted then deasserted, OEM (`info`), "log cleared",
  a pre-init timestamp, a cleared SEL (ids start again); Mac `.panic` and `.ips` reports (SError, SoC watchdog, userspace watchdog,
  kext, truncated, a non-kernel `.ips`, a duplicate `incident_id`), `SMARTStatus` values, `pmset -g therm` and `system_profiler -json`
  outputs. The incident's SEL entry gives the Linux record of section 2.2 byte for byte, and the SError report the Mac one.
- **The queue.** Records leave the daemon's queue only when `health_written` names them; a lost beat's records are sent again; 20 SEL
  entries take four beats and all reach the log, the critical ones first; a queue past 256 folds `info` records only; corrected errors
  of one DIMM in an hour are one record with their count; a record is cut at 256 bytes of `raw`.
- **The log.** A record sent in five beats and by two coordinators is one row; records are never changed or removed; a note and a
  deassertion link by `about`; listing by host, by serial after the host id changed, by component, severity and time; both store
  backends; history records of a first run (older than 7 days) raise nothing.
- **Memory baseline.** SMBIOS type 17 entries populated, empty, size 0 and malformed; 16 of 16 slots summing to the inventory's 256 GiB
  (none), D1 empty (a record naming D1), a module smaller; `MemTotal` 268320876 kB then 251543660 kB (a record); a drop of 2.5 GiB on a
  256 GiB host (none) and of 2.6 GiB (a record); a second short boot still short against the high-water value; a first report with the
  inventory filled (judged at once) and with none (`unconfirmed`); `MemTotal` under 95% of the installed memory at the first report.
- **Daemon.** `NRestarts` rising, `start-limit-hit`, a restart by a deploy inside a run (suppressed); a disk name `-a` refused before
  any run; the IPMI request table holding no `Reserve`, `Clear` or `Delete`; no BMC device and one that does not answer
  `Get Device ID` (`bmc_missing`); the state file (a crash mid-write keeps the old one, a corrupt one moved aside, a wiped one does not
  re-baseline a short host); a kdump boot; a clean reboot with the marker; a daemon restart that does not write `clean`; loop, zram and
  USB disk churn; an unplugged NIC; SMT turned off; no memory error record on a Mac.
- **Alerts.** One test per kind, as the derivations of `derive_in` are tested today: raised from its records, one alert for many
  records of one finding, cleared when deasserted or out of its window (an `alert_cleared` record), kept across a coordinator restart,
  not cleared by silence, nothing from an old agent with no `HostHealth`, the likely-cause join in either order, suppression in a
  maintenance run, a repair cordon and a rollout bake, a group outage collapsed to one alert, the flags' thresholds; `silent_host`
  critical with its `host` records.
- **Notes.** Acknowledge, rebaseline and a put back (of a repair turn and of a halted maintenance turn) refused `notes_required` with no
  or empty notes, accepted with them by any operator, refused to a CI token; each a `note` record and an audit entry; an acknowledged
  alert back on a new boot id.
- **Repair cordon.** A critical `hardware_fault` schedules one repair run as `coordinator`; the turn drains (a fake hook records
  `drain`), stops at `halted` with `held_for_repair`, runs no apply and no `resume`; a second alert joins the open turn; a host in a
  maintenance turn gets its repair run after it; a silent host's turn waits and drains when it beats; Retry and Cancel refused on a
  repair turn; Put back runs `resume` and ends the turn; no rollout order to a cordoned host; a NIC's capacity drop and a full disk do
  not cordon.
- **Wire.** The heartbeat round trip (`proto_map.rs`), bounds refused, a heartbeat at every bound with 16 services under 16 KiB and
  `raw` dropped first, an earlier release's messages reading a heartbeat with `HostHealth` and the new coordinator reading one without
  (the pattern of `wire_absent_tests.rs`, Built), nothing sent before `host-health-v1` is advertised.
- **Console.** `ui/health.js` per new kind; the Hardware panel for every source state; the log list with its filters and raw lines;
  the banner with one critical alert, with only warnings, and with only acknowledged alerts; the notes box refusing to send empty.
- **The incident, end to end.** A scripted host: healthy beats, a reboot, memory short, D1 empty, the SEL entry, js-agent unhealthy
  with `503 not placeable` and crash looping, two failed updates. It must give the records and alerts of section 1 with their links, a
  repair run that drains js-agent and holds, then, after a put back with notes and the DIMM restored, the alerts cleared and the log
  holding every step. Its variants: the daemon's state directory wiped; the coordinator restarted between the reboot and the first
  beat; the short memory reported after the service already went unhealthy; js-agent's health URL refusing connections; a host with no
  inventory memory (`unconfirmed`).

**Why this is the whole list.** For each source it covers present and absent, each entry it can give normalized whole, a malformed or
truncated answer, a failure or hang, a reset, and its bounds; for the log a repeat, two writers, the links and the filters; for each
rule its threshold, its window and its clear; for each operator action its notes, its audit and its refusals; for the cordon each step
and each refusal; and the incident from end to end.

## 9. Compatibility

- **An older daemon, a newer coordinator**: no `host-health-v1`, no `HostHealth`, no records and no alerts of the new kinds; the host's
  page says `hardware: not reported by this daemon's release` (the rule of the trusted root, Built: `alerts.rs`, "a host whose last
  heartbeat said nothing of the root (unknown) raises none"). The coordinator sends a repair order only to a daemon that announced
  `host-health-v1`, so an older daemon is never sent a maintenance kind it does not know; an operator cordons it by hand.
- **A newer daemon, an older coordinator**: the coordinator advertises no `host-health-v1`, so the daemon sends nothing new, and is
  sent no `health_written`; an older agent, which reads the answer strictly, is never sent it either (`GATED_FIELDS`, Built).
- **An older `hk` or console**: the new `Alert` fields are unknown and the new kinds read as `ALERT_KIND_UNSPECIFIED`, shown by their
  message; `MAINTENANCE_KIND_REPAIR` reads as unspecified in a run's kinds.
- **`Maintenance.PutBack`**: a call with an empty `reason`, which is accepted today, is refused `notes_required` (decision 3); a script
  that puts hosts back must pass one.
- **`silent_host`**: the same alert, now critical; a script that filters on severity sees it among the critical ones.
- **Stores**: new tables (`hardware_events`, the baseline, `unhealthy_since`) by new migrations after `0031_maintenance_operator.sql`
  and their files counterparts; `maintenance_runs.kinds` gains `repair`; no shipped migration edited; the `alerts` rows and their key
  unchanged.
- **The inventory**: two optional fields; a record without them is a host with no installed memory known.
- **The log's schema** is additive from its first release (B8): a later record may carry a new field, a new `component`, `source` or
  `code`; a reader shows one it does not know by its `message`.

## 10. What only a person can read on the hosts

Whether each Linux host's BMC answers in-band, APEI and GHES, the installed rasdaemon's table names, what SMBIOS shows after a DIMM is
disabled, each host model's `MemTotal` gap, and the Mac captures: they shape HH4 and HH8 to HH13, not this design.

## 11. North star: a notification system (end state, not planned)

Decided for now: the console and its banner only (decision 1). The end state is a question for later, kept here so the log and the
alerts do not close it off. Options then: a webhook from the coordinator to a chat tool, a paging service or the organisation's own
notification service (secrets in files named by flags, never in argv, the environment or the repository; sent state in the operations
store, sent once across coordinators by a conditional write; one message per host per 5 minutes; a reminder while unacknowledged; a
`cleared` message; one digest, not a flood, when it is first turned on), or mail through a relay. A message would carry a record's
normalized message and never its `raw` line, serials or addresses. Open, to decide then: which destination, whether a warning is worth
a message, and whether the coordinator's network may reach one.

## 12. Implementation pull requests

Each lands with its own tests, among them one that fails without it. A daemon pull request is harmless without the coordinator's
(an older coordinator ignores the field).

| # | Pull request | After |
|---|---|---|
| HH0 | Coordinator: move `Kind` and its three matches out of `alerts.rs`, no change of behaviour | |
| HH1 | Wire: `HostHealth`, `HardwareEvent`, `Baseline`, `SourceState`, the new `ServiceBeat` and `Alert` fields and kinds, `MAINTENANCE_KIND_REPAIR`, the inventory's two fields, `check_health`, the drop groups, `host-health-v1` advertised by no coordinator yet | |
| HH2 | Coordinator: the hardware event log (`hardware_events`, both backends), `HardwareEvents.List`, `hk hardware log` | HH1 |
| HH3 | Daemon: the state file, the boot id, the clean-shutdown marker, held events, the first run's history marks | HH1 |
| HH4 | Daemon, Linux: the capacity report (memory with the SMBIOS devices, CPUs, disks, NICs, with the device filters) | HH3 |
| HH5 | Coordinator: the baseline and the inventory seed, `capacity_short` records, `alert_raised` and `alert_cleared` records of every host alert, `capacity_regressed`, `unexpected_reboot`, `hardware_fault`, `storage_failing`, `bmc_missing`, `silent_host` critical, `hk alerts ack` and `rebaseline` with notes as log records, the `ui/health.js` kinds; advertises `host-health-v1` | HH0, HH2, HH4 |
| HH6 | Observer and daemon, Linux: `NRestarts`, `Result`, `start-limit-hit`, the health reason and body | HH1 |
| HH7 | Coordinator: `service_unhealthy` and `service_crash_loop`, the likely-cause join | HH5, HH6 |
| HH8 | Daemon, Linux: the SEL and SDR decoder, `Get Device ID` | HH3 |
| HH9 | Daemon, Linux: rasdaemon's database | HH3 |
| HH10 | Daemon, Linux: the journal's kernel lines, `/dev/kmsg`, taint, pstore, `HardwareCorrupted`, ext4 errors, read-only remounts, `statvfs` (Built: section 3.1) | HH3 |
| HH11 | Daemon, Linux: `smartctl --json` and hwmon, with the source states | HH3 |
| HH12 | Daemon, Mac: boot id, marker, the panic parser and classifier, `system_profiler` and the capacity | HH3 |
| HH13 | Daemon, Mac: `SMARTStatus`, the thermal rule, the `runs` ring | HH12 |
| HH14 | Coordinator and daemon: the repair cordon (section 6), `PutBack`'s `reason` required for every turn, no rollout to a cordoned host | HH5 |
| HH15 | Console: banner, badges, the Hardware panel with the log, the notes boxes, the Inventory page's memory fields | HH2, HH14 |
| HH16 | The template of the lines a group's `hostkeeper-config-<group>` adds (`Depends: smartmontools, rasdaemon`, `/var/log/journal`, 3.3), `host_not_monitored` turned on, an operations page (what each alert means, what to do, how to acknowledge, rebaseline and put back, how to add the tools) and the "check on a host" lists as a runbook; the operators then change their own config packages | HH5 |
| HH17 | The incident replayed end to end (section 8), last | HH1 to HH16 |
| future | A notification system (section 11): not planned until the owner decides the end state | |

HH6 can be reviewed once HH1 has landed, and HH8 to HH12 in parallel once HH3 has; HH13 follows HH12. Each updates `docs/cli.md` and
the goldens its verbs change (T19), and HH5 updates the severity of `silent_host` in [hostkeeper-design.md](hostkeeper-design.md)
section 6.7 and the operations docs that name it.

## 13. Decisions and questions

### 13.1 Decided (the owner, 2026-11-26)

1. **Notification**: for now a warning banner and the console only; no webhook or other channel. The notification system is an
   end-state question (section 11); its pull request is marked future.
2. **Bad hardware cordons the host automatically, for repair**, through the drain and cordon path maintenance runs use: the host is
   cordoned and its jobs finish, with no resume until an operator puts it back (section 6).
3. **Any operator** may acknowledge an alert or put a host back, and **must write notes** on what was done (section 5.2).
4. **A capacity drop is a warning. A host that is completely off or silent is critical** (`silent_host` becomes critical, 5.1).
5. **Macs: memory errors are out of scope** (section 3.2).
6. **Every Linux host has a BMC.** A host without one is a misconfiguration and raises `bmc_missing` (5.1).
7. **The full history of every alert is kept forever**, with the notes written when the host went back into service: it is the
   hardware event log (sections 2 and 5.2).
8. **smartmontools, rasdaemon and a persistent journal are installed on every Linux host through the config package** (section 3.3).
9. **The job service** sees a hardware alert as a drain that never comes back until an operator puts the host back (section 6), as
   recommended.
10. **The capacity baseline** is seeded from the inventory's installed memory, and memory is judged by the SMBIOS DIMM list against the
    inventory and by `MemTotal` against the boots before, within 1% or 1 GiB (section 4.3), as recommended.
11. **Macs use their built-in sources**: panic reports, unexpected-shutdown records, `diskutil` SMART status, thermal pressure and
    `system_profiler` inventory changes (section 3.2).
12. **Notifications**: the UI only, for now (section 7).
13. **One standardized hardware event log per host**, kept on the coordinator with the same schema on Linux and Mac; every source is
    normalized into it, alerts are derived from it, and the full history with the operators' notes is that same log (section 2).

### 13.2 Still open

1. **Which hosts expect EDAC data in rasdaemon?** Proposed: every Linux host is expected to run rasdaemon (decision 8), but an empty
   memory table is not `host_not_monitored`, since a firmware-first host legitimately records memory errors only in the BMC's event log;
   the host's page says which of the two a host's memory errors come from. The daemon reads it so (Built: an empty `mc_event` table
   is the source `rasdaemon` read, `ok`, with no record), which the alternative would keep. Alternative: label the firmware-first hosts and expect memory
   rows from the others.
