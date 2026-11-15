# Full Disk Access for the hostkeeper daemon on a Mac

`hostkeeper-full-disk-access.mobileconfig` (this directory) is the Privacy Preferences Policy
Control (PPPC) profile that grants the daemon Full Disk Access (`SystemPolicyAllFiles`). It is for
the SSH work in [../../design/ssh-through-coordinator.md](../../design/ssh-through-coordinator.md)
section 3, where `systemsetup -setremotelogin on` is reported to need that right on macOS 13 and
later. Push it only if the daemon reports `needs_full_disk_access`; no Mac needs a command typed on
it, before or after.

## What the shipped daemon is signed with (read from release 0.1.333, without a Mac)

Read from `hostkeeper-0.1.333-macos-arm64.pkg` (payload `usr/local/bin/hostkeeper`, byte-identical
to the binary in `hostkeeper-0.1.333-macos-arm64.tar.gz`) by parsing the Mach-O `LC_CODE_SIGNATURE`
blob:

| Property | Value |
|---|---|
| signature | ad hoc, from the linker (CodeDirectory flags `0x20002`); no Team ID; no requirements blob |
| signing identifier | `hostkeeper-8ffd75d278ace627` in 0.1.333, `hostkeeper-e16ec805f4d77047` in 0.1.332: the linker derives it from the build, so it changes at every release |
| designated requirement | none that survives a build: an ad-hoc binary's is `cdhash H"..."`, one value per build |
| the pkg | signed (xar signature) by `Developer ID Installer: Komira AI, Inc. (U7RTX4XD3X)`; that signs the installer, not the Mach-O inside |
| how it runs | launchd runs `/usr/local/bin/hostkeeper daemon`, which execs `/var/lib/updater/state/hostkeeper/bin/current`, a link to `versions/<N>` that each update replaces (`docs/macos.md`) |

## How the profile names the program, and why

Apple's documentation of the PPPC `Identity` dictionary (developer.apple.com, DeviceManagement,
`PrivacyPreferencesPolicyControl.Services.Identity`, fetched 2026-11-15) says of `IdentifierType`:
"Application bundles must be identified by bundle ID. Nonbundled binaries must be identified by
installation path." The daemon is a bare Mach-O, so the profile uses `IdentifierType` `path`, not
`bundleID`. The `CodeRequirement` is required and is checked against the binary at that path.

A path must be one that does not move. The daemon execs `versions/<N>` under
`/var/lib/updater/state/hostkeeper/bin/`, which changes at every update, so that path cannot be
named. The one fixed path is the installed binary `/usr/local/bin/hostkeeper`, which launchd runs and
the pkg installs; it is the same Mach-O as the daemon's, so it carries the same signature. The
profile names it, with the requirement `identifier "dev.hostkeeper.hostkeeper" and anchor apple
generic and certificate leaf[subject.OU] = "U7RTX4XD3X"`, which a re-signed build of the same
identity satisfies without a change to the profile.

This settles the design's fallback (docs/design/ssh-through-coordinator.md section 3) as the plan:
the process that runs `systemsetup` must be the installed binary at `/usr/local/bin/hostkeeper`,
started by the daemon as a child, never the exec'd `versions/<N>` binary, because only the installed
path is named. Not verified without a Mac: that TCC attributes the Full Disk Access check of
`systemsetup` to that child and not to the daemon or to `systemsetup` itself. The daemon's own
report of whether Full Disk Access is held (`needs_full_disk_access`) settles that on `mac-studio-01`.

## What is missing: signing the daemon

**The profile matches no released binary today.** The release signs the daemon's Mach-O only
ad hoc. It needs a Developer ID Application certificate for Team `U7RTX4XD3X` (the account holds only
the Installer identity) and a signing step for the Mach-O in `artifacts.yml` with the fixed
identifier `dev.hostkeeper.hostkeeper`. That work is tracked in komira-ai/hostkeeper#401 and is not
part of this change. Until a release carries it, uploading the profile grants nothing and does no harm.

Key names (`Services`, `SystemPolicyAllFiles`, `Identifier`, `IdentifierType`, `CodeRequirement`,
`Authorization`, `StaticCode`) are checked against that Apple documentation, which also shows that
the whole `com.apple.TCC.configuration-profile-policy` payload is marked deprecated from macOS 27,
in favour of the declarative management configuration; this profile is for the macOS the Macs run
now. The file parses as a property list.

## Adding it to the Blueprint in Apple Business (once for the account, not per Mac)

1. Sign in to Apple Business. Open **Devices**, then **Configurations**.
2. Choose **Custom** (add a custom configuration), then **Upload** and pick
   `hostkeeper-full-disk-access.mobileconfig`.
3. Set the platform to **macOS**, name it `hostkeeper Full Disk Access`, and **Save**.
4. Open **Blueprints**, pick the Blueprint the Macs are assigned to (the one that holds the
   hostkeeper bootstrap pkg and the `dev.hostkeeper.config` profile), choose **Edit**, then
   **Configurations**, add `hostkeeper Full Disk Access`, and save.

Every Mac in that Blueprint, enrolled now or later, receives the profile on its own; nothing is run
on a Mac. A PPPC grant needs a supervised Mac, which Automated Device Enrollment gives. Menu names
are from the design's reading of Apple Business and are not checked against the live console.
