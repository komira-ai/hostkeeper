# The Mac enrollment baseline

A Mac that enrolls through the coordinator's MDM gate gets its configuration at enrollment, not
afterwards by hand: Screen Sharing turned on, and any configuration profile the farm wants on every
Mac of a kind. The gate keeps it there. Nobody runs a command on a Mac to enable Screen Sharing.

This is the gate's baseline ([design: hostkeeper-mdm.md, section 6.4](../design/hostkeeper-mdm.md);
the console design it comes from is `docs/design/console-broker.md` section 10). The code is
`crates/hostkeeper-mdm-gate/src/baseline.rs`.

## What the operator writes

One file, named by `--mdm-baseline-file` on the coordinator, and the profile files it names.

```toml
# /etc/hostkeeper/mdm-baseline.toml
[[group]]
host_group = "build"            # the role of the Macs' --mac-serial-allowlist entries

[[group.entry]]
remote_desktop = true           # Screen Sharing: EnableRemoteDesktop

[[group.entry]]
profile = "/etc/hostkeeper/baseline/fskit-extension.mobileconfig"
```

- **A Mac's host group is the role of its entry in `--mac-serial-allowlist`.** A Mac whose role is
  not listed gets no baseline. A `host_group` that no allowlist entry has is a mistake in one of
  the two files, and the coordinator refuses to start, naming it.
- **An entry is exactly one of** `remote_desktop = true` and `profile = "<path>"`. A group lists
  each profile identifier once and `remote_desktop` once, and has at least one entry.
- **A profile is an unsigned property list** of a `Configuration` profile with a
  `PayloadIdentifier`, sent as it is. It may not have the identifier of the gate's own profiles
  (`<--mdm-profile-identifier>.config` and `.nonce`).
- **The baseline file and every profile it names must be files of root's** that neither the group
  nor others can write: they say what every Mac of the group is configured with, as root. The
  coordinator checks owner and mode at start, as it does for `--mdm-pkg-allowlist`, and does not
  start otherwise.
- **Flags.** `--mdm-baseline-file` needs `--mdm-profile-identifier` (the enrollment side) and
  `--mac-serial-allowlist`. `--mdm-baseline-check-s` (default 21600, at least 1) is the check
  interval below. Both are read at start: a change needs a restart. A restart sends no baseline
  again by itself; a profile file whose bytes changed is sent again to every Mac of its group.

## What the gate does

1. **At enrollment.** Once a Mac has acknowledged the config profile, the gate sends it its
   group's entries: an `InstallProfile` for each `profile`, and `EnableRemoteDesktop` for
   `remote_desktop`, which it sends only to a Mac whose `DeviceInformation` answer said it is
   supervised (Apple's schema marks the command `supervised: true`). They are queued together
   with the hostkeeper pkg, before the Mac's daemon has posted its request, and whether or not an
   operator has approved the Mac yet. Each send NanoHUB queued is audited `mdm_baseline` by
   `mdm-gate` with the serial, the group and the entry.
2. **Kept.** The gate sends a `ProfileList` command once every profile entry is acknowledged, at
   each of the Mac's `TokenUpdate`s and every `--mdm-baseline-check-s` seconds after its last
   answer. A profile the answer does not list is sent again. `EnableRemoteDesktop` has no read-back
   in Apple's schema (no `DeviceInformation` query reports Screen Sharing), so it is sent again at
   each interval after its last acknowledged answer. Apple's text says it enables Remote Desktop and
   leaves all other options unchanged; that a repeat is harmless is inferred from that sentence and
   not measured on a Mac.
3. **Macs enrolled before this.** A Mac already enrolled, such as the farm's, is sent the baseline
   at the gate's next look after the coordinator starts with the flag; nothing is enrolled again.
4. **A failure never fails enrollment.** The nonce, the approval and the pkg do not wait for the
   baseline, and a baseline that fails changes none of them. A command the Mac answered with an
   error is sent again after the check interval. A command NanoHUB did not queue is sent at the
   next look (once a minute), and one the Mac has not answered is never sent again, so a Mac that
   is off piles nothing up in NanoHUB's queue.

## Reading it

`hkctl mdm list-devices` shows, for each Mac with a baseline, `baseline_state` and, when it failed,
`baseline_reason`:

| `baseline_state` | Meaning | What to do |
|---|---|---|
| absent (`MDM_BASELINE_STATE_UNSPECIFIED`) | No baseline applies: the coordinator has none, the device is not an enrolled Mac of the allowlist, or its group is not in the file. | Check the Mac's role in the allowlist. |
| `MDM_BASELINE_STATE_PENDING` | An entry is not sent or not answered yet, or the Mac has no config profile yet. | Wait; the Mac fetches commands when it checks in. A Mac that stays pending is off, or its config profile is not acknowledged (the device's `last_error`). |
| `MDM_BASELINE_STATE_APPLIED` | Every entry was acknowledged, and none is missing from the last profile list. | Nothing. |
| `MDM_BASELINE_STATE_FAILED` | The Mac answered an entry with an error, or `remote_desktop` is listed for a Mac that is not supervised. `baseline_reason` says which. | An unsupervised Mac cannot take Screen Sharing from MDM: enable it in System Settings, or re-enroll the Mac through Automated Device Enrollment. An errored entry is sent again at the next check. |

A Mac whose Screen Sharing is off while its state is `APPLIED` was changed on the Mac after the
last send; the next check turns it on again.

## What changes, and what does not

- **Format.** The gate's device record gains a baseline (field 21) and a list of unanswered
  baseline commands (field 22), both new optional fields of schema 1. The unanswered
  `EnableRemoteDesktop` and `ProfileList` are in field 22 and not in the list of field 15 because
  the release before refuses a record with a sent command of a type it does not send; it skips
  fields 21 and 22, so it reads every record this release writes, and this release reads a record
  without them as one with no baseline sent. `Mdm.ListDevices` gains `baseline_state` (20) and
  `baseline_reason` (21) on `MdmDevice`, new optional fields.
- **Flags.** Both flags are new. A coordinator started without them runs as before and sends no
  baseline. The release before does not know the flags, so a rollback means dropping them from
  its command line.
- **Rollback.** Dropping the baseline does not remove what a Mac holds: remove a baseline profile
  with `RemoveProfile` on the Mac by hand, and turn Screen Sharing off in System Settings, or
  leave them.

## Apple's commands

The three commands are read from Apple's `apple/device-management` repository, `mdm/commands/`:
`remotedesktop.enable.yaml` (`RequestType` `EnableRemoteDesktop`, no payload keys, macOS 10.14.4
and later, device channel, `supervised: true`, `requiresdep: false`), `profile.list.yaml`
(`ProfileList`, optional `ManagedOnly`, the answer's `ProfileList` an array of dictionaries each
with a `PayloadIdentifier`) and `profile.install.yaml` (`InstallProfile`, `Payload` the profile's
data). The gate sends `ManagedOnly` false, so a profile the Mac holds from another source is
listed too and a baseline profile is judged by its identifier alone.
