# Host health alerts: hardware and service faults, one incident per host

Status: design. Nothing of it is built. It extends the alerts of
[hostkeeper-design.md](hostkeeper-design.md) section 6.7 (Built: `Alerts.List`, `hk alerts list`,
`crates/hostkeeper-coordinator/src/alerts.rs` and its `alerts_*.rs` modules, the book `alerts_book.rs` and the Postgres rows
`store/postgres/alert_rows.rs`), the daemon's observation of its services ([service-observation.md](../service-observation.md);
Built: `crates/hostkeeper-observe`) and the console's host health ([ui-serving.md](../ui-serving.md); Built: `ui/health.js`,
`ui/pages/home.js`, `ui/pages/host.js`). Facts about this repository were read on `main` at the commit this document lands on.
**Built** marks what exists (with its path), **New** what this design adds, **[V]** a claim to check before building on it.

The owner's direction, after a host failed with nobody told: hostkeeper should catch issues like this and issue a notification, or
a warning, for that specific host.

## 1. What changes for the operator

**What happened, with hostkeeper as it is.** A Linux build host (here `host-a`) rebooted at 03:01Z. Its firmware recorded a fatal
memory error on DIMM D1 and mapped the DIMM out, so the host came up 16 GiB short: `MemTotal` read 251543660 kB against 268320876 kB
before (240 GiB of 256 GiB). js-agent's container unit, `js-agent-crun.service`, went into a restart loop (89 restarts and more),
because the job service refused the host's new shape. Two updates on the host (js-agent's and a config package's) failed. The job
service read the host `lost` for over 90 minutes. hostkeeper's heartbeat from the host stayed live throughout, so `Alerts.List` named
the host only in the two `failed_update`s, each on its own and with no word of why. A person found the cause by watching a rollout and
reading the host's journal by hand.

**After.** Within a minute of the host's first heartbeat after the boot, one critical **incident** is open for `host-a`, and each
channel of the coordinator's notify file (a webhook, mail) gets one message:

```
host-a: memory error on DIMM D1, host booted 16 GiB short
  memory error   DIMM D1, fatal, recorded by the firmware before the boot at 03:01Z (BMC event log)
  capacity       RAM 240 GiB, was 256 GiB; DIMM D1 16 GiB -> none; cores 64, disks 2: unchanged
  console        https://hostkeeper.example.com/#/host/host-a
```

As the effects appear, the same incident grows, with at most one short message per ten minutes:

```
host-a: also js-agent cannot join (job service: lost); js-agent-crun.service restarting (89 restarts)
host-a: also 2 failed updates since the boot: js-agent, host-config
```

The console shows the incident at the top of every page, in a red banner naming the host, and on the host's page with every reading
behind it. There, and in `hk alerts list`, its summary line is `host-a: memory error on DIMM D1, host booted 16 GiB short; js-agent
cannot join`, and the five other alerts (the capacity drop, the crash loop, js-agent unhealthy and the two failed updates) are linked
to it, not listed as unrelated failures. Nothing on the host is changed by hostkeeper: replacing the DIMM is a person's job, and once
the host has its 256 GiB back the capacity alert clears by itself (or an operator accepts the new capacity, section 3.5).

**What does not change.** Every existing alert kind keeps its meaning, identity, severity and log token. `Alerts.List` changes
nothing an operator set. An agent or a coordinator of an older release keeps working with a newer one (section 8). The daemon gains
reads, and writes nothing outside its own state directory. A coordinator with no notify file sends nothing to anyone, as today.

## 2. What the daemon watches (New)

A new **health look** in the daemon reads each source below on its own cadence (the pattern of the maintenance look,
`crates/hostkeeper-daemon/src/maintenance_facts.rs`, Built: a look never blocks the heartbeat, and a failed look keeps the last report
and says it failed). The look turns what it read into a bounded **health report** that rides the heartbeat (section 2.8). The daemon
reports **readings and events**, not judgements: thresholds, rates and severities are the coordinator's (section 3), so a threshold
changes with a coordinator flag and never needs a release of every daemon.

Each source has a port (a trait) and one real adapter per platform, like the observer's ports
(`crates/hostkeeper-observe/src/ports.rs`, Built), so every test runs over a fake of the source (section 9). A source that is absent on
a host (no BMC, no EDAC driver, no `smartctl`) is **not read**, with its reason, and the host's page says so; it is never an alert and
never reads as healthy. A source that answered in this boot and stops answering is the alert `health_unread` (section 3.1).

### 2.1 Memory and CPU faults

| Source | Platform | What is read | Cadence |
|---|---|---|---|
| EDAC counters | Linux | `/sys/devices/system/edac/mc/mc*/dimm*/{dimm_label,dimm_location,dimm_ce_count,dimm_ue_count}`, and per controller `ce_noinfo_count`, `ue_noinfo_count` | 60 s |
| Kernel log | Linux | `/dev/kmsg`, opened read-only and non-blocking: read from the oldest record, skipping those at or below the cursor's sequence number for this boot (an `EPIPE` means records were overwritten, and the look says how many were lost). The records whose text contains `mce:`, `EDAC`, `[Hardware Error]:` or `Machine check events logged` | 60 s |
| Boot error record (BERT) | Linux | The kernel prints the firmware's record of the error that ended the previous boot as `[Hardware Error]:` lines early in this boot. If the ring had already wrapped past them at the first look of a boot, the look reads the same lines from `journalctl --dmesg --boot=0 --grep=<the four strings>` on a systemd host [V] | each boot |
| pstore | Linux | The names and first 4 KiB of `/sys/fs/pstore/*` and of `/var/lib/systemd/pstore/*/` (the previous boot's crash records), each once: the cursor keeps the names and times it has read. Never removed | each boot |
| BMC event log (IPMI SEL) | Linux, a host with a BMC | `Get SEL Info` and `Get SEL Entry` over `/dev/ipmi0` (the device the daemon already opens for the BMC's address, `crates/hostkeeper-daemon/src/ipmi.rs`, Built), entries after the last record id read: sensor types memory (`0x0C`: correctable and uncorrectable ECC, memory device disabled), processor (`0x07`: IERR, machine check), critical interrupt (`0x13`) and temperature (`0x01`) | 300 s |
| DIMM inventory (SMBIOS type 17) | Linux | `/sys/firmware/dmi/entries/17-*/raw`: each memory device's locator (`D1`), bank, size, and `No Module Installed` or size 0 for an empty or mapped-out slot [V: a mapped-out DIMM reads as one of these on the boards we run] | each boot, then hourly |
| Panic reports | macOS | The names and first 4 KiB of `/Library/Logs/DiagnosticReports/*.panic` and of the `.ips` reports there whose `bug_type` is `210` (a kernel panic), newer than the newest read: the panic string names a machine check or a memory fault | 300 s |
| Memory modules | macOS | `system_profiler -json SPMemoryDataType`: each module's slot, size and status on a Mac with slotted memory; a Mac with Apple silicon reports one block and no status [V] | each boot, then hourly |

**What a Mac has.** A Mac has no EDAC, no SEL and no firmware error table the daemon can read: its memory and CPU faults are the
panic reports and, on a Mac with slotted memory, the module status. A Mac with Apple silicon that corrects a memory error says
nothing a reader can see; it is covered only when it panics.

**Naming the DIMM.** A finding names a DIMM by what the firmware calls it: the EDAC `dimm_label` when the board sets one, else the
SMBIOS locator of the same slot, else the EDAC location (`mc0 csrow3 channel1`). A SEL record names a DIMM only when its event data
does (an OEM field on many boards [V]); one that does not is a memory error of the host, not of a DIMM, and the coordinator may name
the DIMM from a capacity change of the same boot (section 4.3).

**Events and counters.** A record of the kernel log, the SEL, pstore or a panic report is an **event** with a stable id (source,
boot id, record id or sequence number), its time, the source's severity word, the component and one line of text of at most 160
bytes. A counter (EDAC, filesystem error counts, SMART counts) is reported as its **total since boot**, never as a delta, so a lost
heartbeat or a daemon restart loses nothing; rates and growth are the coordinator's (section 3.4).

**History is not news.** The first read of a SEL, a pstore directory or the panic reports (a new host, a wiped state directory)
finds records of any age. The daemon reports each record with its time, and the coordinator treats a record older than
`--alert-event-hold-secs` (section 3.3) when first seen as history: shown on the host's page, never alerted. A SEL timestamp before
the BMC's clock was set (below `0x20000000`, the IPMI rule for "since power-on") has no known age, and the daemon marks it so; the
coordinator treats it as history. When the SEL's record ids start again or its last-erase time changes (`Get SEL Info`), the look forgets
its SEL cursor and reads it anew under the same rule.

### 2.2 Capacity across a boot

At each boot, and hourly, the look records the host's **capacity**:

| | Linux | macOS |
|---|---|---|
| RAM | `MemTotal` of `/proc/meminfo`, and the SMBIOS type 17 devices: locator and size, `none` for an empty slot | `sysctl -n hw.memsize`; the `SPMemoryDataType` slots, where it has them |
| Cores | `/sys/devices/system/cpu/online` and `present` | `sysctl -n hw.physicalcpu hw.logicalcpu` |
| Disks | `/sys/block/*`: each whole disk that is not `loop`, `ram`, `zram` or `dm-`, with its size and its identity: the NVMe `device/serial`, else `device/wwid`, else `smartctl`'s `serial_number` [V: `sd` disks have no `device/serial`] | `diskutil list -plist physical` and `diskutil info -plist` of each: size and identity [V] |

At most 32 DIMMs and 24 disks; a host with more says so in `left_out`. The report carries the capacity with the boot id and boot
time the maintenance report already reads (Built: `MaintenanceHost.boot_id`, `boot_unix`). The comparison with the host's earlier
capacity is the coordinator's (section 3.5): it keeps a baseline per host, so a reinstalled daemon or a wiped state directory does not
lose it.

### 2.3 Disk health

| Source | Platform | What is read | Cadence |
|---|---|---|---|
| SMART | Linux | `smartctl --json --health --attributes --nocheck=standby /dev/<disk>` (smartmontools, read-only, and never spins up a disk in standby): the overall result, NVMe `critical_warning`, `media_errors`, `percentage_used`; ATA reallocated, pending and offline-uncorrectable sector counts | hourly, and once after each boot |
| SMART | macOS | `diskutil info -plist <disk>`: `SMARTStatus`. `Verified`, `Not Supported` and an absent key are not a fault; any other value (`Failing`, `About to Fail`) is | hourly |
| Filesystem errors | Linux | ext4 `/sys/fs/ext4/<dev>/errors_count`; btrfs `/sys/fs/btrfs/<uuid>/devinfo/*/error_stats` [V: kernels from 5.14]; kernel log records of a filesystem shut down or found corrupt (XFS `Corruption detected`, `Filesystem has been shut down`) | 60 s |
| Read-only remount | Linux and macOS | `/proc/self/mounts` (Linux) or `getfsstat` (macOS): a filesystem the look saw read-write in this boot and now reads read-only | 60 s |
| Use | Linux and macOS | `statvfs` of each mounted local filesystem (ext4, xfs, btrfs, zfs, vfat, apfs), one per device: bind mounts, btrfs subvolumes and the volumes of one APFS container are read once, as the container | 60 s |

A Mac has no filesystem error counter; its filesystem faults are the read-only remount and the panic reports. `smartctl` is not a
dependency of the daemon's package: the Debian and RPM packages recommend `smartmontools` (a weak dependency), and a host without it
reports SMART `not read: smartctl not installed`. A disk name reaches `smartctl` only if it is a name of `/sys/block` matching
`^[a-z0-9]+$`.

### 2.4 Temperature, where the host has a sensor

| Source | Platform | What is read | Cadence |
|---|---|---|---|
| hwmon | Linux | `/sys/class/hwmon/hwmon*/temp*_input` with the same sensor's `_max` and `_crit`, its device's `name` and its `_label` | 60 s |
| BMC event log | Linux, with a BMC | SEL temperature records (sensor type `0x01`): the BMC's own upper non-critical and upper critical thresholds, asserted and deasserted | 300 s |
| Thermal state | macOS | `pmset -g therm`: the thermal and performance warning levels and the CPU speed limit | 60 s |

The daemon judges no temperature against a number of its own: a sensor counts as hot only against the threshold its driver, its BMC
or macOS states. A sensor with no threshold is shown on the host's page and never alerts. A sensor is identified by its hwmon
device's `name` and its own label or index (`coretemp/Package id 0`, `nvme/Composite` of `nvme1`), so two sockets or two disks are
two sensors.

### 2.5 Units in a crash loop

The observer already reads each managed unit's `ActiveState`, `SubState`, `MainPID` and `LoadState` every round (Built:
`crates/hostkeeper-observe/src/systemd.rs`). It adds:

| Platform | What is read | From |
|---|---|---|
| systemd | `NRestarts` (the automatic restarts since the unit was last started by hand or by a step), `Result`, `ExecMainStatus`, `ExecMainCode` and `ActiveEnterTimestampMonotonic`, in the same `systemctl show` call | the managed units (`[units] run` of each package) and the daemon's own |
| launchd | `runs`, `last exit code` and `state` of `launchctl print system/<label>` | the managed jobs and the daemon's own |
| OpenRC | the service's status word (`started`, `crashed`, `stopped`) at each round; OpenRC has no restart counter, so the daemon counts the transitions it sees from `crashed` back to `started`, and may miss one that happens between two rounds [V: `supervise-daemon` respawns] | the managed services |

The report carries, per unit, the restart counter, the restarts in the last 10 minutes (from the counters of the last 10 minutes of
rounds, which the look keeps in its cursor file, so a daemon restart does not empty the window), the rounds in a row the unit has not
been active, and the last result and exit status. The daemon's own unit is read at its start: a daemon that systemd or launchd keeps
restarting reports, at each start, how many times it was restarted. It also reads the unit's last 5 journal lines
(`journalctl --unit=<unit> --lines=5 --output=cat --boot=0`, each cut to 200 bytes; on OpenRC and macOS none) when the unit
restarted in the last 10 minutes: they go to the console and to `hk`, never to a notification channel (section 5.4).

Only managed units are read. A unit of the host that no package names is not hostkeeper's to judge; the host's page shows the list
of `systemctl list-units --state=failed --plain --no-legend`, and it raises no alert.

### 2.6 The managed services' own health

Two ways for a service to say how it is, both declared in its package's manifest (AGENTS.md B17: per-service logic stays in the
service's own hook, and hostkeeper reads a generic answer):

1. **`[health] url`** (Built): a loopback GET, 2xx healthy. The observer computes a reason token (`status_non_2xx`,
   `unreachable`, ...; Built: `Observation::reason_token`), which the JSON route of the heartbeat carries as `health_detail` and the
   gRPC `ServiceBeat` does not: it carries only `HEALTH_UNHEALTHY`. New: the `ServiceBeat` carries the reason, and the first 160 bytes
   of a non-2xx reply's body, so a service that answers `503 not placeable: shape refused` says so.
2. **`[hook] health`** (New): for a service with no listener, such as js-agent, a manifest may name a `health` step, as it may name
   `drain` and `resume` (Built in the manifest's rules: `is_drain_step`, `is_resume_step` of
   `crates/hostkeeper-config/src/manifest.rs`). The daemon runs it every 60 s, as it runs every hook step: as root (hostkeeper's
   trust is the signed package, [hostkeeper-design.md](hostkeeper-design.md) section 1), in a transient unit bound by
   `[hook.timeout_secs] health` (default 10 s, at most 30 s). The `health` step must change nothing, like `verify`; on systemd its
   unit adds `ProtectSystem=strict`, `ReadWritePaths=` the hook's `state_dir` only, `PrivateTmp=yes` and `NoNewPrivileges=yes`, so a
   step that tries to write elsewhere fails. The step prints one JSON line of at most 1 KiB:

```json
{"healthy": false, "serving": false, "reason": "not_placeable", "summary": "cannot join",
 "detail": "the job service refuses the host's shape: 240 GiB, 64 cores",
 "outside": {"system": "job-service", "state": "lost", "since_unix": 1795662240}}
```

| Key | Meaning |
|---|---|
| `healthy` | required |
| `serving` | optional: `false` says the host does none of the service's work; absent is unknown |
| `reason` | required when not healthy: a token of at most 32 bytes of `[a-z0-9_]` |
| `summary` | optional: at most 40 bytes of plain words that complete "js-agent ..." in a message; absent, `unhealthy` |
| `detail` | optional: at most 160 bytes, for the console only |
| `outside` | optional: the service's own reading of a system outside hostkeeper, here the job service's state of this host: `system` and `state` tokens of at most 32 bytes, `since_unix` |

hostkeeper never calls the job service itself (as [maintenance-updates.md](maintenance-updates.md) section 5.2 says of drain):
js-agent's `health` step (New in komira-ai/jobscheduler) reads the host's own record with the host's own credential (Built there:
`GET /v1/hosts/{id}`, whose `state` is `awaiting`, `live` or `lost`, `crates/js-agent/src/own_host.rs`). Because the step is its own
process, it answers while the agent's unit is down or restarting, which is when the answer matters. A step that exits non-zero,
passes its bound, prints more than 1 KiB or no valid line is the health `unread` with that reason (`hook_failed`, `hook_timeout`,
`hook_output`), which the coordinator treats as unhealthy (section 3.1).

A package that names `[hook] health` sets the manifest's `min_hostkeeper` (Built: `HostkeeperVersion` of `manifest.rs`) to the first
release that runs it, so an older daemon refuses that version by its floor, as it refuses any version it cannot run, and keeps the one
it has.

### 2.7 How often, and what a look costs

| Cadence | Sources |
|---|---|
| Every observer round (Built) | unit state and restart counters (section 2.5), `[health] url` |
| 60 s | EDAC, kernel log, filesystem errors, mounts, `statvfs`, hwmon, `pmset -g therm`, `[hook] health` |
| 300 s | SEL, panic reports |
| Hourly, and once after each boot | SMART, SMBIOS and `SPMemoryDataType`, capacity |

The first look after a boot reads every source at once, so the first heartbeat after a boot that came up short carries the capacity
and the boot error record.

### 2.8 The report on the wire

A new optional `HealthReport` on the heartbeat (`HeartbeatRequest` field 21, New; fields 1 to 20 are taken). The daemon sends it, and
the new `ServiceBeat` fields, only after the coordinator's answer has advertised the feature `host-health-v1`: the mechanism of
`maintenance-v1` (Built: the daemon's `FEATURES` and the coordinator's `OFFERED` in `crates/hostkeeper-coordinator/src/features.rs`
each gain the name). It goes on the gRPC route only (the JSON route is frozen) and counts toward the heartbeat's bound on both, as the
trusted root does (Built: `crates/hostkeeper-link/src/beat.rs`).

```proto
message HealthReport {
  uint64 looked_unix = 1;
  string boot_id = 2;                     // the boot this report is about
  Capacity capacity = 3;                  // section 2.2: ram, cores, dimms (at most 32), disks (at most 24)
  repeated HealthEvent events = 4;        // at most 12: id, unix, source, severity word, component, text, history
  repeated Counter counters = 5;          // at most 24: source, component, name, total since boot
  repeated DiskHealth disks = 6;          // at most 24: identity, name, SMART result and counts
  repeated MountUse mounts = 7;           // at most 8, the fullest first: mount point, device, bytes and inodes
  repeated Temperature temperatures = 8;  // at most 8, the nearest to its threshold first
  repeated SourceState sources = 9;       // each source: read, not_read, failing; reason and since
  uint32 left_out = 10;                   // entries dropped by the bounds
}

// ServiceBeat (fields 1 to 15 are taken), New:
optional string health_reason = 16;       // the observer's token, or the hook's reason, or unread's reason
optional string health_body = 17;         // at most 160 bytes of a non-2xx reply, or the hook's detail
repeated UnitRestarts restarts = 18;      // unit, counter, last 10 min, rounds not active, result, exit status
optional HealthOutside health_outside = 19;
optional bool serving = 20;
optional string health_summary = 21;
```

When the events are more than 12, the look keeps the uncorrected and fatal ones first, then the newest. The look keeps the boot's
events in its cursor file, so every report carries the boot's state, and a report that is lost or a daemon that restarts loses
nothing.

**Size.** The encoded report is at most 3 KiB (`MAX_HEALTH_BYTES`, checked by `check_health` as `check_report` checks the maintenance
report, Built: `crates/hostkeeper-proto/src/maintenance.rs`). The heartbeat's size check (Built: `fit` and `DROP_ORDER` of `beat.rs`,
`MAX_HEARTBEAT_BYTES` 16 KiB) gains two groups, dropped first: the report's readings (`disks`, `mounts`, `temperatures`, `sources`),
then the services' `health_body` and `restarts`. The report's `capacity`, `events` and `counters` (at most about 1.5 KiB) are counted
and never dropped, like the maintenance report (at most about 5 KiB) and the prerequisites report (at most about 4 KiB). The worst case
of every never-dropped part, with 16 services, must stay under 16 KiB: HH1's size test (extending `wire_size_tests.rs`, Built) shows
it [V].

## 3. How a reading becomes an alert (New)

The coordinator derives the new alerts as it derives every alert today: in one function of what it holds and the time (Built:
`alerts::derive_in`, observed every 15 s and at each `Alerts.List`). What a rule needs from earlier observations (when a condition
began, its last event, its window counts) is the **condition state** of section 3.4, which the observation reads and, on one
coordinator, writes. The derivations are in new modules, `alerts_health.rs` and `alerts_correlate.rs`; `alerts.rs` is at 998 lines
(R12), so HH0 first moves `Kind` and its `log_token`, `severity` and `proto` to `alerts_kind.rs`.

### 3.1 The new kinds

| Kind (log token) | `AlertKind` | Raised when | Severity | Kind of condition |
|---|---|---|---|---|
| `memory_error` | 25 | an uncorrected or fatal memory event (EDAC `ue` grown, a fatal BERT record, a SEL uncorrectable ECC or memory-device-disabled record, a panic naming a memory fault); or corrected errors on one DIMM over `--alert-ce-per-day` (default 24) in the last 24 hours | critical; warning for corrected only | event |
| `cpu_error` | 26 | an uncorrected machine check, a processor IERR in the SEL, a panic naming a machine check; or corrected machine checks over `--alert-ce-per-day` | critical; warning for corrected only | event |
| `capacity_drop` | 27 | the RAM, the cores, a DIMM or a disk of this boot is short of the host's baseline (3.5) | critical | standing |
| `disk_failing` | 28 | SMART overall failed, NVMe `critical_warning` not 0, macOS SMART not `Verified` (2.3) | critical | standing |
| `disk_errors` | 29 | media errors, reallocated, pending or uncorrectable sectors that grew | warning | event |
| `filesystem_error` | 30 | an ext4 or btrfs error counter that grew, a filesystem shut down or found corrupt | critical | event |
| `filesystem_read_only` | 31 | a filesystem read-write earlier in this boot and read-only now | critical | standing |
| `disk_nearly_full` | 32 | a filesystem at `--alert-disk-warn-percent` (90) of bytes or inodes; at `--alert-disk-critical-percent` (97) | warning; critical | standing |
| `temperature` | 33 | Linux: a sensor at its own `_max` (warning) or `_crit` (critical); a SEL upper non-critical (warning) or upper critical (critical) threshold asserted. macOS: a thermal or performance warning level set, or a CPU speed limit under 100 (warning; macOS acts on its own critical state) | warning or critical | standing |
| `unit_crash_loop` | 34 | a managed unit restarted `--alert-restarts` (5) times in 10 minutes; or it has not been active for 3 rounds in a row with a restart counter of 5 or more | critical | standing |
| `service_unhealthy` | 35 | a managed service unhealthy or unread for `--alert-unhealthy-secs` (300), by its `[health] url` or its `[hook] health` | warning; critical when it says `serving: false` | standing |
| `health_unread` | 36 | a source that answered in this boot has failed for an hour; or a host that announced `host-health-v1` sent no report for an hour while heard | warning | standing |
| `notify_failed` | 37 | a channel failed to deliver for 30 minutes (section 5.2) | warning | standing |

Each threshold is a coordinator flag (S2: flags, not environment) with the default above, read once at start, like the alerts' flags
today (Built: `--alert-silent-host-secs`, `--alert-cert-window-secs`); a value out of its range (a percent not in 50 to 99, a count of
0) stops the coordinator at start. `ALERT_KIND_*` values 25 to 37 and their log tokens are added to `operator.proto`; nothing is
renumbered (B8). `AlertSeverity` keeps its two values: critical needs a person now, warning needs one soon. A kind whose severity
depends on the reading carries the higher of its readings.

**Not raised while it is planned.** `unit_crash_loop` and `service_unhealthy` of a service are not raised while the host is in a
maintenance turn between drain and putting back (Built: the turns of `OpsState.maintenance_runs`), or while the service has an update
running (Built: `ServiceBeat.updating`): those stops are hostkeeper's own, and their failures are `maintenance_halted` and
`failed_update`. A hardware kind is raised in a maintenance turn as anywhere: a reboot is when a DIMM fails.

### 3.2 The alert record

An alert's identity stays its `Subject` (Built: kind, service, host, version, request, other host), and gains one part, the
**component key**: the DIMM's locator, the disk's identity (serial or wwid), the filesystem's device, the unit's name, the sensor's
device and label; empty for a host-wide finding (a memory error that names no DIMM, labelled `memory`; a capacity drop, one alert per
host, labelled `capacity`). The key may hold a serial; it is hashed and never shown. What is shown is the component's **label**: `DIMM D1`, `disk nvme1n1`,
`/var/lib/js`, `js-agent-crun.service`, `coretemp/Package id 0`. A disk renamed from `sdb` to `sdc` keeps its key and its alert.

`Alert` (operator.proto) gains, as optional fields after the 14 it has:

| Field | Number | Meaning |
|---|---|---|
| `alert_id` | 15 | `al-` and the first 16 hex characters of the SHA-256 of the canonical subject with its component key: the same for the same condition on every coordinator and after a restart |
| `component` | 16 | the label; empty for the kinds of today |
| `incident_id` | 17 | the incident the alert belongs to (section 4.2); every alert is in one, alone or not |
| `caused_by` | 18 | the `alert_id` of the incident's head when this alert is linked to it as an effect or as a root of the same boot; empty for the head and for an alert alone |
| `link` | 19 | why it is linked: `same_boot`, `after_boot`, `after_event`; empty when `caused_by` is |
| `evidence` | 20 | at most 4 `AlertEvidence {source, observed_unix, text}`: what was read, from where, and when |
| `boot_id` | 21 | the boot the finding belongs to, for the kinds of section 2 |
| `summary` | 22 | for an incident's head: the incident's summary line (section 4.4) |

The new kinds are not written to the Postgres `alerts` rows (Built: `alert_rows.rs`, which decide which coordinator logs an alert):
their once-logging, like all their state, is in the condition state (3.4). A coordinator of the release before deletes every
`alerts` row it did not derive, so a new kind's row there would be deleted and re-inserted, and logged, every 15 s while two releases
share one database.

### 3.3 Dedup and clearing

- **One alert per condition.** The same event in the next report, or a counter grown again, updates the alert's evidence and count and
  never opens a second alert. Its log line is written once, when the condition state is first written.
- **A standing condition clears by itself** when what raised it has been gone for its hold: `disk_nearly_full` 5 points under the
  threshold it crossed (the state keeps which); `temperature` 3 degrees under its threshold, the SEL threshold deasserted or the Mac's
  levels clear, for 10 minutes; `unit_crash_loop` active with no restart for 10 minutes; `service_unhealthy` healthy for 1 minute;
  `disk_failing`, `filesystem_read_only` and `health_unread` at once; `capacity_drop` when the capacity returns (3.5). A condition that
  returns within its hold is the same alert, so a flapping sensor is one alert, not twenty.
- **An event condition does not end by itself.** A memory error of yesterday is still a fact about the DIMM. `memory_error`,
  `cpu_error`, `disk_errors` and `filesystem_error` stay open for `--alert-event-hold-secs` (default 7 days) after their last event,
  or until an operator clears them: `hk alerts clear --alert-id al-... --reason "DIMM D1 replaced"` (New verb, `Alerts.Clear`, for an
  operator and refused to a CI token as `Alerts.List` is, audited as `alert_clear` under the caller's name with the host as the
  entry's node). A clear covers the events up to its time: the same SEL record in the next report does not reopen the alert, and a
  newer event does.
- **A standing condition cannot be cleared,** since it is still true: `Alerts.Clear` refuses it as `standing`. A capacity drop is
  accepted instead (3.5); a failing disk ends when it is replaced or removed and its baseline follows (3.5).
- **A restart re-observes, and does not re-log or re-notify**: the condition state and the notifications are stored (3.4, 5.2).

### 3.4 The condition state

| Table (Postgres; the files backend keeps the same records) | Holds |
|---|---|
| `health_conditions (alert_id PK, host, kind, component_key, first_unix, last_event_unix, last_bad_unix, last_good_unix, crossed, window, anchor_boot, anchor_unix, incident_id, logged)` | per open or held condition: when it began, its last event, when it was last bad and last good, the threshold it crossed, its window counts (corrected errors per 24 h, restarts), its anchor (4.1) and incident |
| `alert_clears (alert_id, through_unix, by_name, reason, at_unix)` | the operators' clears |
| `capacity_baselines (host PK, ram_bytes, cores, dimms, disks, set_unix, set_by)` | 3.5 |
| `incidents (incident_id PK, host, opened_unix, head, closed_unix)` | 4.2 |
| `notifications (channel, incident_id, opened_unix, event, seq, decided_unix, sent_unix, tries, last_error)` | 5.2 |

These are in the coordinator's operations store, in both backends (Built: `store/files.rs` and `store/postgres/`, held to one behaviour
by `store/conformance_tests.rs`), added by new migrations after `0031_maintenance_operator.sql` and their files counterparts. They are
written only by the coordinator that holds the periodic job `alerts` (Built: the jobs of `store/postgres/periodic.rs`; with the files
backend, the one coordinator), at its 15 s observation; `Alerts.List` on any coordinator derives from them and writes none. A
coordinator that starts reads them back, so a hold, a window or an incident survives a restart. While a coordinator of an earlier
release holds the job, nobody writes them: the new kinds are still derived from the reports, but holds and windows do not advance and
nothing is notified until a coordinator of this release takes the job back [V: the job moves at a rollout's restart].

### 3.5 The capacity baseline

The coordinator keeps one baseline per host: RAM, cores, the DIMMs by locator and size, the disks by identity and size. Each part is
judged by itself:

- The first capacity a host reports becomes its baseline, audited as the coordinator's `capacity_baseline`. A host whose first report
  is already short is not caught: the baseline knows no better. Its page shows the baseline's date, and enrollment says nothing of
  capacity.
- A part **added** or **grown** (a DIMM in an empty slot, a new disk) joins the baseline, audited, with no alert. RAM's total is raised
  only when no part is short, so a boot that gains a disk and loses a DIMM still alerts for the DIMM.
- A part **short** raises the host's one `capacity_drop`, naming each part short: `RAM 240 GiB, was 256 GiB; DIMM D1 16 GiB -> none`.
  RAM counts as short when `MemTotal` fell by more than 1% or 512 MiB, whichever is more (`MemTotal` moves by a few MiB between
  kernels); a DIMM, a core or a disk counts by itself, so a DIMM mapped out is named even when the sum hides it. A second part short
  later is added to the same alert, and its incident grows.
- The alert stays while the host runs short, through any number of boots: the baseline is not the last boot, so a second reboot at
  240 GiB does not make 240 GiB normal. It clears when every part returns, or when an operator accepts the host's current capacity as
  its baseline: `hk hosts accept-capacity --host-id host-a --reason "D1 removed, RMA open"` (New, `Hosts.AcceptCapacity`, audited
  `capacity_accepted`). A planned change (a resized VM, a disk taken out) is accepted the same way.
- **A replaced disk.** A disk missing from the baseline while a disk not in it has appeared, of at least its size, when the missing one
  had a `disk_failing` or `disk_errors` alert in the last 30 days, is a replacement: the new disk takes its place in the baseline,
  audited `capacity_baseline` with the reason `disk_replaced`, and no alert. Any other missing disk is short.

## 4. Correlation: one incident explains the others (New)

`alerts_correlate.rs` links a host's open alerts after they are derived, so `Alerts.List`, the log, the console and the notifications
all see the same links. Links never cross hosts.

### 4.1 Tiers and anchors

| Rank | Kind | Tier |
|---|---|---|
| 1 | `memory_error` | 1, hardware |
| 2 | `cpu_error` | 1 |
| 3 | `capacity_drop` | 1 |
| 4 | `disk_failing` | 1 |
| 5 | `filesystem_error` | 1 |
| 6 | `filesystem_read_only` | 1 |
| 7 | `temperature` (critical only) | 1 |
| 8 | `unit_crash_loop` | 2, the host's services |
| 9 | `service_unhealthy` | 2 |
| 10 | `failed_update`, `host_halt`, `maintenance_halted`, `maintenance_not_back`, `repeated_deferral`, `silent_host` (Built kinds) | 3, effects |

`disk_errors`, `disk_nearly_full`, a warning `temperature`, `health_unread` and `notify_failed` are never linked: a full disk explains
a failed update only sometimes, and a wrong link is worse than none. Neither are the other Built kinds.

An alert's **anchor** is a boot and a time, fixed when its condition state is first written:

- a finding read in a boot, whatever its source (a BERT record, a SEL or pstore record of the error that ended the previous boot, a
  capacity short): that boot, at its `boot_unix`. A `capacity_drop` keeps the anchor of the first boot it was short in;
- any other alert: the boot the host reported when the alert began, at the alert's first observation.

### 4.2 The rule, and incidents

1. **Roots.** Tier 1 alerts of one host with the same anchor boot form one root group. The group's **head** is its alert of the lowest
   rank (then the lower `alert_id`): a memory error before the capacity drop it caused. The others are linked to the head as
   `same_boot`.
2. **Effects.** An alert B of tier 2 or 3 is linked to a head A when A's group anchors to B's anchor boot, and B began no earlier than
   one minute before A's anchor time and no later than `--alert-link-secs` (default 6 hours) after it. If more than one head qualifies,
   B joins the one of the latest anchor not after B's start, then the lowest rank. A tier 3 alert with no tier 1 head may link, by the
   same rule, to a tier 2 alert of the host as its head (a crash loop explaining the failed updates).
3. **Incidents.** Each head, with what is linked to it, is an incident; every alert linked to nothing is an incident of its own. An
   incident's id is the `alert_id` of the first alert of it to be observed, kept in `incidents` while the incident is open; it does not
   change when another root joins, takes the head, or clears first. An incident closes when all its alerts have cleared; the same
   condition returning after that opens a new incident (a new `opened_unix`), even with the same id. An incident's severity is the
   highest of its alerts'.

A link is a judgement of time and place, not of cause, and the console and messages say "since the boot", not "because". Two hardware
alerts of one boot are one incident whatever their ids; an effect of that boot joins it whichever of the two came first.

### 4.3 A memory error that names no DIMM

A `memory_error` whose events name no DIMM (a SEL record without one, a BERT record, a panic report) takes its label from a
`capacity_drop` of the same boot when exactly one DIMM is short: `DIMM D1`, with the evidence `named by the capacity change of the same
boot`. Its key stays host-wide, so the alert keeps its identity when the capacity returns. With no DIMM short, or more than one, its
label is `memory`, and the summary names the DIMMs short: `memory error, DIMMs D1 and D3 missing`.

### 4.4 What the incident says

The summary line names the host, then the head, then a same-boot root of another kind, then the first service in the incident that is
not serving (by its hook's `summary`), in that order, in at most 120 characters:
`host-a: memory error on DIMM D1, host booted 16 GiB short; js-agent cannot join`. The lines after it list the evidence, then the
effects: services, then updates, then the rest. The job service's reading appears as what js-agent said (`job service: lost`), since
hostkeeper does not read the job service.

`hk alerts list` keeps its order (Built: by kind, then service, host and version) and its fields, and adds the new ones; `--host-id`
and `--severity` (New flags) filter it.

## 5. How a person is told (New)

### 5.1 Channels

The coordinator reads a **notify file** named by `--notify-file` (none: no channel, nothing is sent, as today). Secrets are files it
names, never values in the file and never environment variables (S2):

```toml
[[channel]]
name = "ops-webhook"
kind = "webhook"
url = "https://notify.example.com/hooks/hostkeeper"      # https only
secret_file = "/etc/hostkeeper-coordinator/notify/ops-webhook.key"
min_severity = "critical"

[[channel]]
name = "ops-mail"
kind = "mail"
smtp = "smtp.example.com:587"                            # submission, with STARTTLS required
from = "hostkeeper@example.com"
to = ["ops@example.com"]
password_file = "/etc/hostkeeper-coordinator/notify/ops-mail.password"
min_severity = "warning"
host_groups = ["linux-build"]                            # absent: every host group
```

- **Webhook.** A `POST` of one JSON document (5.4) over HTTPS, signed with HMAC-SHA256 of the body under the key in `secret_file`
  (`X-Hostkeeper-Signature: sha256=<hex>`). The body has a `text` field with the message's lines, so a chat service, an incident tool
  or the organisation's own notification service that takes a webhook can show it as it is. hostkeeper names none of them.
- **Mail.** One message per notification, its subject the message's first line, through the SMTP submission server the file names.
- **The console** needs no configuration: every alert is on it (section 6).

The file is checked at start, and a coordinator whose file is wrong (an unknown key, an `http://` webhook, a secret file it cannot
read) does not start: a config mistake is fixed at its source and found once, at deploy (B20). `hk alerts notify-test --channel
ops-webhook` (New, `Alerts.NotifyTest`, audited) sends one test message through a channel, for that deploy check.

### 5.2 What is sent, and when

A notification is per **incident** (4.2) and per channel whose `host_groups` hold the host and whose `min_severity` the incident's
severity meets (an incident whose head is a warning and whose effect is critical is critical):

| Event | When | Message |
|---|---|---|
| opened | a critical incident: at its first observation; a warning one: once it has stood 10 minutes (`--notify-warning-hold-secs`) | the summary line, the evidence, the console link |
| grew | an alert joined, or the severity rose | one line, `host-a: also ...`, at most one per incident per 10 minutes, the joins of those 10 minutes in it |
| cleared | the incident closed and stayed closed for 10 minutes | one line, `host-a: cleared: capacity back to 256 GiB` |

The opened message is sent at once, not held for effects to appear: a fatal memory error is worth a page before its effects are known,
and the effects follow as grew lines.

The coordinator stores each notification it decides to send in `notifications` (3.4), keyed by channel, incident, the incident's
`opened_unix`, the event and, for grew lines, a sequence number, before it sends it. So a restart does not send an opened incident
again, a recurrence after a close is a new opening, and only the coordinator holding the job sends. A notification is tried until it is
sent: after 1 minute, then doubling up to 30 minutes, on the coordinator's clock (injected in tests). A channel that has failed for
30 minutes is the alert `notify_failed`, shown on the console and sent through the other channels only.

**A flood is one message.** When more than 10 incidents would be opened within 5 minutes (a rack loses power, a switch fails), the
11th and later are not sent one by one: one message lists them by kind and host (`12 hosts silent: host-a, host-b, ... and 2 more`),
and their grew lines are folded into one such message per 10 minutes. Their cleared lines are folded the same way.

### 5.3 What a message reads like

The first line says the host, what is wrong, and what it does to the host's work, in that order:

- `host-a: memory error on DIMM D1, host booted 16 GiB short; js-agent cannot join`
- `host-b: disk nvme1n1 failing (SMART: critical warning, 14 media errors); 2 failed updates`
- `host-c: /var/lib/js 97% full (12 GiB free)`
- `host-d: tierstore@9301.service restarting (7 restarts in 10 min, exit 1)`
- `mac-1: CPU speed limited to 60% (thermal warning)`

The lines after it are the evidence (source and time of each finding), the effects, and the host's console page.

### 5.4 What a notification carries, and never carries

A channel is outside the coordinator's trust. A notification carries the host id and group, the kinds, the component labels, counts,
sizes, reason tokens, a hook's `summary` and `outside` tokens, and a link. It never carries journal lines, a hook's `detail`, a reply
body, `log_tail` text, a component key, a serial or an address, which may hold what a service logged or what identifies hardware.
Those are on the console and in `hk`, behind an operator's sign-in. The webhook document:

```json
{"schema": 1, "event": "opened", "incident": "al-3c9f0e1b5a7d2e64", "opened_unix": 1795662120,
 "host_id": "host-a", "host_group": "linux-build", "severity": "critical",
 "text": "host-a: memory error on DIMM D1, host booted 16 GiB short\n...",
 "alerts": [{"alert_id": "al-3c9f0e1b5a7d2e64", "kind": "memory_error", "component": "DIMM D1"},
            {"alert_id": "al-81d4c2a90f3b6e17", "kind": "capacity_drop", "component": "capacity", "link": "same_boot"}],
 "url": "https://hostkeeper.example.com/#/host/host-a"}
```

`schema` is 1; a later schema only adds fields (B8).

## 6. The console (New)

- **The banner.** While any critical incident is open, every page shows one red banner at its top: the first incident's summary line
  and a link to its host, with `and 2 more` linking to Home. A warning incident shows on Home and the host's page only. The banner reads
  `Alerts.List`, which `ui/calls.json` then lists for every page, at each page load and every 30 s.
- **Home.** The alerts strip (Built: `alertsStrip` of `ui/pages/home.js`) groups alerts by incident: the summary line, with the linked
  alerts folded under it. The degraded-hosts summary counts the new kinds (Built: `HEALTH_ALERTS` of `ui/health.js` gains every kind of
  section 3.1 but `notify_failed`).
- **The host's page.** The section `Why host-a is not healthy` (Built: `healthSection` of `ui/pages/host.js`) shows each incident with
  its evidence and its linked alerts, a **Clear** button on an event kind (it calls `Alerts.Clear` with a reason the operator types),
  and **Accept capacity** on a capacity drop. A new **Health** section shows the last report: capacity against the baseline and the
  baseline's date, DIMMs, disks with SMART, the fullest filesystems, temperatures against their thresholds, the managed units' restarts
  with their last journal lines, each service's health answer with its `detail`, history records, the host-wide failed units, and each
  source's state with its age (`SEL: read 2 min ago`, `SMART: not read: smartctl not installed`, `temperature: no sensor on this host`,
  `health: not reported by this daemon's release`). `Hosts.Get` carries the report as `Host.health_report` (New field 30; fields 1 to
  29 are taken) and the baseline as `Host.capacity_baseline` (31).

## 7. Read-only collection, and its bounds (New)

hostkeeper fixes no hardware and changes nothing it reads. The daemon runs as root (Built: "the one root process of a host",
[two-programs.md](two-programs.md)); the health look uses that only to read:

| What | Why root | Never |
|---|---|---|
| `/dev/kmsg`, opened read-only, non-blocking | `dmesg_restrict` | writes to it, or clears the ring |
| `/dev/ipmi0`: `Get SEL Info`, `Get SEL Entry` (NetFn storage), with the BMC address reads of today | the device is root's | `Clear SEL`, `Reserve SEL`, any `Set` or chassis command: the request table has no other entries (a whole entry is read with reservation 0, which needs none [V]) |
| `/sys/firmware/dmi/entries/17-*/raw`, `/sys/fs/pstore/*`, `/var/lib/systemd/pstore` | root-only files | removes a pstore record |
| `/sys/devices/system/edac`, `/sys/fs/ext4`, `/sys/fs/btrfs`, `/sys/class/hwmon`, `/sys/block`, `/proc/meminfo`, `/proc/self/mounts`, `statvfs` | most are world-readable; one reader for all | writes to `/sys` (an EDAC counter reset is a write) |
| `/Library/Logs/DiagnosticReports` (first 4 KiB of each new report) | root-only on a Mac | removes or moves a report |
| Programs: `smartctl`, `systemctl show`, `systemctl list-units`, `journalctl`, `launchctl print`, `diskutil list`, `diskutil info`, `system_profiler`, `pmset -g therm`, `sysctl -n` | `smartctl` opens the raw disk | any other program, or any other argument |

The `[hook] health` step is the service's code, not hostkeeper's: it runs as root like every hook step (2.6), under the read-only unit
properties of section 2.6 on systemd, and under its contract alone on OpenRC and macOS. hostkeeper's trust in it is its trust in the
signed package, as for `verify`.

**How it stays bounded.**

- **A fixed table.** Every program the look can run is one entry of a static table, `READS`, of an absolute path and fixed
  arguments (the pattern of the probe's `STEPS`, Built: `crates/hostkeeper-daemon/src/maintenance_hook/probe.rs`), each with a deadline
  and an output cap. New: an entry may have one slot, for a unit, a launchd label or a disk, whose value must match that slot's rule
  (a unit or label of a manifest, a `/sys/block` name of `^[a-z0-9]+$`) and may not start with `-`. A test asserts that the runner
  refuses any argv not in the table. The IPMI requests are one such table too.
- **Deadlines and caps.** A program is killed at its deadline (`smartctl` 30 s, the others 10 s), its stdout capped at 64 KiB
  (Built: `ProcessRunner` of `crates/hostkeeper-observe/src/command.rs`, which keeps the daemon's environment less the service
  manager's variables), and run with its environment cleared and stdin closed (New for this runner; Built in `LineProcess` of
  `crates/hostkeeper-daemon/src/line_process.rs`). A file read reads at most 64 KiB. A look reads at most 4096 kernel records and keeps
  at most 256 matching ones, reads 64 SEL entries and 8 new panic reports; the rest wait for the next look.
- **One worker per source.** Each source runs on its own worker, one read at a time. A read that hangs in the kernel (a sysfs file of a
  dying disk, `smartctl` stuck in the kernel) leaves that source `failing: stuck` and its worker blocked: that source is not read again
  until the read returns, the other sources go on, and the heartbeat never waits.
- **One write.** The look writes only its cursor file in the daemon's state directory, whole and renamed: per boot, the kernel record
  sequence, the SEL's last record id and erase time, the pstore and panic reports read, the boot's events (at most 32), and the units'
  counters of the last 10 minutes.

## 8. Compatibility

- **An older daemon and a newer coordinator**: the daemon announces no `host-health-v1`, sends no report, and its host's page says
  `health: not reported by this daemon's release`. It raises no `health_unread`.
- **A newer daemon and an older coordinator**: the coordinator does not advertise `host-health-v1`, so the daemon sends no report and
  none of the new `ServiceBeat` fields.
- **An older daemon and a package that names `[hook] health`**: refused by the package's `min_hostkeeper` (2.6), as today.
- **An older `hk` or console and a newer coordinator**: they read the new `Alert` fields as unknown and the new kinds as
  `ALERT_KIND_UNSPECIFIED`, and show them by their message.
- **Two coordinator releases on one database**: the new tables are the new release's alone; the `alerts` rows hold no new kind (3.2);
  the state advances only on a coordinator of this release holding the job (3.4).
- **Stores**: new tables, added by new migrations; none edits a shipped one.

## 9. Tests

All in Rust, in the crate of the code, over a fake of each source: scratch directories holding a sysfs or `/proc` tree, fake programs
answering recorded output, a fake IPMI device, a fake kernel log, an injected clock. The notification tests run a webhook receiver and
an SMTP server in the test, on loopback port 0. None times anything (T10, T12). Fixtures are recorded outputs with host names, serials
and addresses replaced by generic ones.

**Memory and CPU faults** (daemon `health_memory_tests.rs`): an EDAC tree whose `dimm_ue_count` goes 0 then 1 is a total of 1 on its
`dimm_label`; a total lower than the last in the same boot is a source fault (`failing: counter fell`), and a lower one after a new boot
id is a new boot; no `/sys/devices/system/edac` is `not read: no EDAC driver`; a kernel log with an MCE record, a `[Hardware Error]`
BERT record and 300 unrelated records gives the two events; 5000 records give at most 4096 read and the cursor where it stopped; an
`EPIPE` gives the lost count; a ring wrapped before the first look of a boot reads the journal fallback; a fake IPMI device answering
memory, processor and temperature entries, a `0xC1` completion code, a cleared SEL (ids start again) and a pre-init timestamp; the
request table holding no `Clear SEL` or `Reserve SEL`; SMBIOS type 17 entries installed, empty, size 0 and malformed; pstore records
read once across two looks; panic reports of type 210 naming a machine check, of another type, and one longer than 4 KiB; more than 12
events keeping the fatal ones.

**History** (daemon): a first read of a SEL holding records of last year reports them as history; a record of this boot is not.

**Capacity** (daemon `health_capacity_tests.rs`, coordinator `alerts_health_tests.rs`): the first report sets the baseline; a DIMM
added joins it; `MemTotal` 268320876 kB then 251543660 kB with D1 `none` is one `capacity_drop` naming RAM and D1; `MemTotal` 4 MiB
lower is none; a core offline; a disk gained and a DIMM lost in one boot still alerts; a missing disk; a disk renamed `sdb` to `sdc`
with the same identity is not; a disk replaced after a `disk_failing` is taken in without an alert, and one with no failing record is
short; the alert held over a second boot at the lower capacity; cleared by the return and by `Hosts.AcceptCapacity`, which sets the
baseline and is audited; 33 DIMMs give `left_out`.

**Disk health** (daemon `health_disk_tests.rs`, coordinator): `smartctl --json` outputs of a passing NVMe disk, `critical_warning` 4,
media errors grown, an ATA disk with pending sectors grown, a disk in standby, `smartctl` missing, `smartctl` killed at its deadline; a
disk name `-a` or `sda1;x` refused before any run; `diskutil` `Verified`, `Failing`, `About to Fail`, `Not Supported`; an ext4
`errors_count` grown; a btrfs `error_stats`; a mount read-write then read-only; two APFS volumes of one container read once; `statvfs`
at 89.9%, 90% and 97%, inodes at 90%; clearing at 85% after crossing 90, at 92% after crossing 97, and not at 86% after crossing 90.

**Temperature** (daemon `health_temperature_tests.rs`, coordinator): hwmon at `_max`, at `_crit`, with no threshold (shown, no alert),
with a non-numeric `_input`; two `Composite` sensors of two NVMe disks are two sensors; SEL upper critical asserted then deasserted;
`pmset -g therm` with no warning, a thermal warning level, a speed limit of 60; clearing after 10 minutes 3 degrees under, and not after
9 minutes.

**Crash loops** (observer `restarts_tests.rs`, coordinator): restart counters 0, 4 and 5 within 10 minutes of the injected clock; a
counter of 89 with the unit `activating (auto-restart)` for 3 rounds; a counter that falls to 0 after a start by a step; a unit active
with no restart for 10 minutes clears; the 10-minute window across a daemon restart (from the cursor file); `NRestarts` missing on an
old systemd (`not read`); `launchctl print` with `runs` 1 then 7; OpenRC `crashed` and `started` transitions; the daemon's own restart
count read at its start; no alert while the host is in a maintenance turn or the service is updating; journal lines cut at 200 bytes.

**Service health** (observer and daemon `health_service_tests.rs`): a `[health] url` answering 503 with a body gives `status_non_2xx`
and the body's first 160 bytes; a `health` step (a fake hook program) printing the js-agent answer above, printing nothing, 2 KiB,
not JSON, a reason with a capital letter, a `summary` of 41 bytes, exiting 1, killed at its deadline; `serving: false` making it
critical and `serving` absent not; an unread step for 300 s raising `service_unhealthy` as unread; `outside` carried to the evidence.

**Correlation and dedup** (coordinator `alerts_correlate_tests.rs`): the incident of section 1 replayed from fixtures gives one incident
headed by `memory_error`, `capacity_drop` linked `same_boot`, and the crash loop, js-agent unhealthy and the two failed updates linked
`after_boot`; its summary line equal, as a whole string, to the one of section 4.4; the memory error from a SEL record with no DIMM
labelled `DIMM D1` from the capacity drop, and `memory` with two DIMMs short; a failed update that began before the boot, and one 7
hours after it, not linked; a `disk_nearly_full` never linked; the head the same whichever of the two roots was observed first and for
any order of input; the incident id unchanged when its first root clears; alerts on two hosts never linked; the same SEL record in five
reports is one alert; a clear, then the same event (stays cleared), then a newer one (reopens); `Alerts.Clear` refused for a standing
kind, for a CI token and for an unknown id.

**Condition state** (coordinator, both backends, the conformance tests): a hold, a window and an incident survive a restart; only the
job holder writes; `Alerts.List` on another coordinator writes nothing; a log line written once across a restart and across two
coordinators.

**Notifications** (coordinator `notify_tests.rs`): the notify file refused at start for an `http://` webhook, a missing secret file, an
unknown key, an unknown severity; a webhook body and its signature equal to the expected bytes; a mail message equal to the expected
text; the journal line, hook detail, reply body and serial of the incident absent, by equality of the whole body with the expected one;
a warning held 10 minutes, a critical one not; grew lines coalesced to one per 10 minutes; retries after 1, 2, 4 ... 30 minutes on the
injected clock; `notify_failed` after 30 minutes and not sent on its own channel; a restart sends nothing again; two coordinators on one
Postgres send once; a recurrence after a close is sent as a new opening; 11 incidents in 5 minutes are 10 messages and one summary;
`host_groups` and `min_severity` routing.

**Wire and versions** (`hostkeeper-proto`, `hostkeeper-link`, `hostkeeper-wire`): every bound of `check_health`; a heartbeat with 16
services and every never-dropped part at its bound under 16 KiB, and the readings dropped first when over; a heartbeat with a
`HealthReport` read by a copy of the previous release's messages (the pattern of `wire_absent_tests.rs`, Built), and one without read
by the new coordinator; no report and no new `ServiceBeat` field sent before `host-health-v1` is advertised; a manifest naming
`[hook] health` refused by a daemon below its `min_hostkeeper`.

**Console** (`hostkeeper-ui-tests`): the banner on every page with one critical incident, absent with only warnings, `and 2 more`;
Home grouping by incident; the host page's Health section with every source state; Clear and Accept capacity calling their methods
with the typed reason.

The privileged tests (`crates/hostkeeper-privileged`, as root in `privileged.yml` and `pr-device.yml`) read the real kernel log, the
real `/sys` and `systemctl show` of a unit made to restart, and assert that the look writes nothing outside its cursor file.

**Why this list is complete.** For each source it covers: present and absent, a normal reading, each fault the source reports, a
malformed or truncated answer, a program that fails or hangs, a counter that resets, and the bounds; for each rule: just under and at
its threshold, its hold, and its clear; for each store record: a restart and two coordinators; for each channel: the exact bytes sent,
what must never be in them, a failure and a retry; and the incident of section 1 end to end.

## 10. Implementation pull requests

| # | Pull request | After |
|---|---|---|
| HH0 | Coordinator: move `Kind` and its three matches from `alerts.rs` to `alerts_kind.rs`, no change of behaviour | |
| HH1 | Wire: `HealthReport`, the new `ServiceBeat`, `Alert` and `Host` fields, the new `AlertKind` values, `check_health`, the drop groups; `host-health-v1` advertised by no coordinator yet; size and version tests | |
| HH2 | Daemon: the health look, one port and fake per source of sections 2.1 to 2.4, `READS`, the cursor file; Linux and Mac collectors | HH1 |
| HH3 | Observer and daemon: restart counters and journal lines, the reason and body of `[health] url`; `[hook] health` in the manifest and its step | HH1 |
| HH4 | Coordinator: the derivations of section 3, the condition state, clears and the capacity baseline in both stores, `Alerts.Clear`, `Hosts.AcceptCapacity`, `hk alerts clear`, `hk hosts accept-capacity`, `--host-id` and `--severity`; advertises `host-health-v1` | HH0, HH2, HH3 |
| HH5 | Coordinator: correlation and incidents (section 4) | HH4 |
| HH6 | Coordinator: the notify file, the channels, the `notifications` store, `Alerts.NotifyTest`; a `docs/operations/` page for the notify file | HH5 |
| HH7 | Console: banner, Home grouping, the host's Health section, Clear and Accept capacity | HH5 |
| HH8 | komira-ai/jobscheduler: js-agent's `health` step and its manifest's `[hook] health` and `min_hostkeeper`; then the incident replayed on a test host | HH3 |

HH2 and HH3 can land in parallel, as can HH6 and HH7. Each updates `docs/cli.md` and the goldens its verbs change (T19), and the alert
kind list of [hostkeeper-design.md](hostkeeper-design.md) section 6.7, whose "no pager" sentence HH6 replaces.

## 11. Questions for the owner

1. **Corrected memory errors.** Proposed: a warning once a DIMM passes 24 in a day, sent to channels whose `min_severity` is
   `warning`. Alternative: on the console only, never sent.
2. **A host with a hardware incident.** Proposed: hostkeeper tells and stops nothing. Alternative: the coordinator also holds new
   update orders for the host while it has an open tier 1 alert, so a rollout does not count a broken host as a failed update.
3. **Channels in the first build.** Proposed: webhook and mail together (HH6). Alternative: webhook first, since the organisation's
   notification service takes one, and mail later.
4. **A failing disk awaiting replacement.** Proposed: `disk_failing` stays open and cannot be cleared while SMART says failing (3.3),
   so it stays on the banner. Alternative: a clear that mutes it for a set time.
5. **The first page.** Proposed: a critical incident is sent at once and its effects follow as grew lines (5.2). Alternative: hold
   the opened message two minutes so the first message names more of its effects.
