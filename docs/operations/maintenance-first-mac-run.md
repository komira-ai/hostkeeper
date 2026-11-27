# The Macs' standing update policy, their schedule, and the first macOS update on one Mac

The farm's Macs are the host group `mac-build`. This page holds what that group's maintenance is
set to and why, how its macOS windows are scheduled, and how the first macOS update through a run
is made on one drained Mac, with what is recorded. Each change is a held script of
[held/](held/README.md), run by the owner; the procedure behind each is
[maintenance.md](maintenance.md#the-mac-macos-and-xcode), and the design is
[maintenance-updates.md](../design/maintenance-updates.md), section 10.

None of it has been run yet. The order is the sections below, top to bottom. Each step's dry run
comes before its `--apply`, and nothing goes on past a step that did not end well.

## 1. What each Mac reports

`sh held/mac-facts.sh` reads the group and each Mac in it, and changes nothing. For each Mac it
prints the facts below, then the coordinator's preview of a macOS turn on that Mac alone (a
Schedule without `--confirm`, which stores nothing), and last one line, `ready` or the gaps. A
preview that is refused, or that does not take the Mac, is a gap.

| Fact | Where in `hk hosts get` | A macOS turn needs | Otherwise |
|---|---|---|---|
| Device management | `maintenance.mac.mdmConflicts` | empty | left out of `macos`, `mdm_conflict` ([what device management must not set](maintenance.md#what-device-management-must-not-set)) |
| Update account | `maintenance.mac.updateAccount` | `present`, `admin` and `secureToken`, each true | left out, `no_update_account` ([setting it up](maintenance.md#setting-up-the-update-account-on-a-new-mac); a Mac enrolled before the keys needs the file written by hand) |
| FileVault | `maintenance.mac.filevault` | `FILE_VAULT_STATE_OFF` | left out, `filevault_not_ready`: no run orders `macos` on a Mac whose FileVault is not off, whatever `authrestartSupported` says (komira-ai/hostkeeper#604) |
| Due macOS update | `maintenance.due[]` of kind `MAINTENANCE_KIND_MACOS` | one of the running major (`from` and `to` share their first number); a new major is not counted while `macos_major` is off | not in the run; the preview is refused `nothing_due` |
| Report | `maintenanceStale`, `maintenance.lookFailed` | fresh, and the last look did not fail | the script calls the Mac not ready |
| Daemon | `heartbeat.agent.version`, `maintenance.onDemand` | 0.1.480 or later, and `probe` on demand | not ready |

Setting up the update account needs the bootstrap's four keys, among them the password of the
administrator made at Setup Assistant, which grants the account its Secure Token. Only the owner
has it. A Mac with gaps is fixed at the Mac (or with `hk ssh`), as the linked sections say, and
read again. These facts are not known yet for the farm Macs: the first step is this read.

## 2. The standing policy

`sh held/mac-policy.sh` (dry run), then `--apply --record <dir>`, sets these keys of `mac-build`'s
maintenance table and leaves every other key as it is, among them `xcode_keep` and
`xcode_default`, which [xcode-keep.sh](held/xcode-keep.sh) sets:

| Key | Value | Why |
|---|---|---|
| `batch` | 1 | one Mac in a turn at a time: a macOS update takes a Mac out for up to the 2 h download deadline (while it still serves), the 3 h install deadline and its restart |
| `min_serving` | 2 | a turn starts only while three of the four Macs serve, so two always do; a Mac that is out for another reason holds the run back rather than leaving one Mac to build |
| `drain_deadline_secs` | 14400 | a drain waits four hours for the builds on the Mac, then puts the Mac back and the turn defers; no build is cut |
| `return_secs` | 3600 | the Mac platform's default, stated: a Mac silent an hour after its restart is `not_back` and raises `maintenance_not_back` |
| `macos_major` | off | a turn takes only the newest minor update of the running major; a new major is a decision of its own ([Major macOS upgrades](maintenance.md#major-macos-upgrades-macos_major)) |

The script refuses a group with fewer hosts than `min_serving` + 1, whose turns would never start,
and refuses while a run that is not ended names the group or one of its Macs.

## 3. The first macOS update, on one drained Mac

`sh held/macos-update.sh --mac mac-studio-01` (dry run), then with `--apply --record <dir>`. The
first Mac is `mac-studio-01`, the same first Mac as the Xcode rollout. The script checks that
the Mac is ready (section 1, plus: its group's `macos_major` is off, its group has another host,
since the script never takes a group's last host, and no open run names it). It then makes two
runs, each followed to its end:

1. **The update**: `maintenance schedule --kinds macos --host-ids mac-studio-01 --confirm true`.
   The Mac's turn is stage (the download, while it serves), drain, apply (`softwareupdate
   --install` with the update account, then the restart), check (`sw_vers` reports the target
   build), put back, done ([A Mac's turn](maintenance.md#a-macs-turn)). A Mac `not_back` after its
   restart is followed on, since its turn goes on when it reports again. The turn counts as an
   update only when the Mac then reports a build other than the one it ran, and a version at least
   the one that was due. A put back also ends a turn done, so `applied` alone is not proof.
2. **The probe**, once the Mac is back: `maintenance schedule --kinds probe --host-ids
   mac-studio-01 --confirm true`, which drains the Mac again, runs `sw_vers` and `fdesetup` as root
   in launchd, and puts it back ([Probe](maintenance.md#probe-drain-read-put-back)). It shows the
   Mac drains and is put back on the new macOS, and reads `sw_vers` on the Mac itself.

A turn that defers is cancelled, so it is not offered again unattended. A turn that halts is left
halted with the Mac cordoned, and the script prints `retry-host` and `put-back` for the owner to
choose. With `--record <dir>` each answer is kept there, with `mac-studio-01-audit.txt` (the audit
entries of both runs) and `mac-studio-01-sw_vers.txt` (before: the Mac's report; after: its report
and the probe's `sw_vers`).

The run's record goes here once it is made: the two runs' ids, the audit lines, and `sw_vers`
before and after.

## 4. The schedule

A group's policy holds no window, and the coordinator repeats nothing: a window is one stored run
(komira-ai/hostkeeper#603). So `mac-build`'s standing schedule is one window a week, **Saturday
22:00 UTC for six hours**, stored the week before by `sh held/mac-schedule.sh --start
<Saturday>T22:00:00Z` (dry run), then `--apply`. It is one `maintenance schedule --host-group
mac-build --kinds macos` with that window, which takes each Mac with a minor update due, one at
a time, by the policy above. A Mac in its turn at the window's end finishes its turn, and the
others wait for the next window. The script refuses while a run is open on the group, while
`macos_major` is true, and when the preview takes no Mac; it prints each Mac's verdict
(`waiting`, or `skipped` and why). Its first window comes after section 3 has ended well.

A run that would take every Mac of the group is refused `last_host`, which happens once a new
update is due on all four, and the script never passes `--take-last-host`. Then give `--except`
with one Mac: the run names the other three by `--host-ids` in place of the group, and the Mac left
out is taken by the next window, or by `held/macos-update.sh`.

## Gaps

- **FileVault on**: such a Mac is never ordered `macos`, so the daemon's authenticated restart is
  not reached (komira-ai/hostkeeper#604). Until then a build Mac keeps FileVault off, as
  [maintenance.md](maintenance.md#filevault) says.
- **No standing window** in a group's policy (komira-ai/hostkeeper#603): the schedule is stored by
  hand, one window at a time.
- **[xcode-install.sh](held/xcode-install.sh)** stops on a `not_back` Mac and prints two commands
  the coordinator refuses for it (komira-ai/hostkeeper#605).
- **[The group's settings](maintenance.md#the-groups-settings)** say `Get` answers every
  maintenance key; a zero or false one is left out (komira-ai/hostkeeper#606). The scripts read each
  key with a default.
- **No download size** in a due macOS item, so a script cannot tell before the stage whether the Mac
  has the twice-the-download free space it needs; it prints `dataFreeBytes`, and a stage short of
  space defers `free_space_short` with the Mac unchanged.
