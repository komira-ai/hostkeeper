# Held scripts

The scripts in this directory are run by the repository's owner, by hand, from an operator box. No
workflow, test or other script runs them. Each one:

- changes nothing by default: it makes its reads and prints each change it would make as
  `DRY-RUN: <command>`, and acts only with `--apply`;
- prints every call before it makes it: `READ:` a read, `+` a change (a read repeated while it
  waits, once);
- never forces: it passes no flag that overrides a refusal (such as `--take-last-host`), and with
  `--apply` stops at the first refusal or failure rather than going on to the next change;
- takes its settings as flags, and reaches the coordinator through `hk` and its settings file
  (`hk settings init`, [docs/cli.md](../../cli.md)), so it holds no address, token or path of its
  own;
- writes files only into a directory it is given (`--record`), never under `/tmp`, and never
  overwrites or removes one;
- is POSIX sh, run with `sh <script>`, and `sh -n` and `bash -n` clean.

| Script | What |
|---|---|
| [xcode-keep.sh](xcode-keep.sh) | Set one group's `maintenance.xcode_keep` and `xcode_default`, and nothing else of it: the group as the coordinator answered it with those keys changed, checked leaf by leaf, put with the generation read, checked again by the put's audit entry and a second read, with the line that puts the old body back. A Mac is due an Xcode only once its group keeps one; `--default 0` (a version the store keeps no release of) has the turns install Xcode and leave `xcode-select` as it is. Run it before `xcode-install.sh` |
| [xcode-install.sh](xcode-install.sh) | Install an Xcode release the store holds on farm Macs, one Mac at a time, each in one maintenance run of the kind `xcode` followed to its end, with the run's audit entries ([maintenance.md](../maintenance.md#bringing-in-a-new-xcode)) |
| [mac-facts.sh](mac-facts.sh) | Read what each Mac of a group reports for a macOS update (the update account, FileVault and its authenticated restart, device-management conflicts, the macOS updates due, the daemon and its report) and the coordinator's preview of a macOS turn on each, and print each Mac's gaps. It changes nothing and has no `--apply` |
| [mac-policy.sh](mac-policy.sh) | Set one group's standing maintenance policy for its Macs: `batch`, `min_serving`, `drain_deadline_secs`, `return_secs`, and `macos_major` off, and nothing else of it, checked and put the way xcode-keep.sh puts its keys ([maintenance-first-mac-run.md](../maintenance-first-mac-run.md#2-the-standing-policy)) |
| [macos-update.sh](macos-update.sh) | Install the newest minor macOS update on one Mac in one maintenance run of the kind `macos` followed to its end, then a run of the kind `probe` once it is back, with the runs' audit entries and `sw_vers` before and after ([maintenance-first-mac-run.md](../maintenance-first-mac-run.md#3-the-first-macos-update-on-one-drained-mac)) |
| [mac-schedule.sh](mac-schedule.sh) | Store a group's next macOS window: one run of the kind `macos` on the group with a window start and end, after the coordinator's preview ([maintenance-first-mac-run.md](../maintenance-first-mac-run.md#4-the-schedule)) |
