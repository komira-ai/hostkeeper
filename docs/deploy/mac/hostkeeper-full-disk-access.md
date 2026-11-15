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

So a profile can name neither a path (it changes at each update) nor an identifier or Team ID (the
shipped binary has none that is stable). **The profile in this directory therefore matches no
released binary today.** It is written for the signature it needs: the release signs the daemon's
Mach-O with a Developer ID Application identity of Team `U7RTX4XD3X` and the fixed identifier
`dev.hostkeeper.hostkeeper`. That needs a Developer ID Application certificate, which the account
does not hold (only the Installer identity), and a signing step for the Mach-O in `artifacts.yml`; neither is part of this change. Until then, uploading the profile grants nothing.

Not verified without a Mac: that TCC accepts `IdentifierType` `bundleID` holding a signing
identifier for a program that is not a bundle, and that the grant is attributed to the daemon when
it runs `systemsetup`. The daemon's own report of whether Full Disk Access is held settles both on
`mac-studio-01` itself.

Key names (`Services`, `SystemPolicyAllFiles`, `Identifier`, `IdentifierType`, `CodeRequirement`,
`Authorization`, `StaticCode`) are Apple's PPPC payload keys as known; Apple's developer page could
not be fetched as text, so they are unverified by fetch. The file parses as a property list.

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
