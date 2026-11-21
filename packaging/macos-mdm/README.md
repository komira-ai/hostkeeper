# hostkeeper's privacy profile

`hostkeeper-full-disk-access.mobileconfig` is the configuration profile that device management
installs on a Mac so hostkeeper has Full Disk Access. Without it, root is not enough on current
macOS: a daemon cannot delete a local account, remove another account's home or
`~/Library/Preferences`, or open the system's privacy database.

It holds one Privacy Preferences Policy Control payload
(`com.apple.TCC.configuration-profile-policy`) with one entry under `SystemPolicyAllFiles`:

| Key | Value |
|---|---|
| `Identifier` | `dev.hostkeeper.hostkeeper` |
| `IdentifierType` | `bundleID` (the binary embeds an Info.plist with this bundle identifier, `crates/hostkeeper/macos/Info.plist`) |
| `CodeRequirement` | the designated requirement of the release's signature: the identifier, Apple's Developer ID chain and the team `U7RTX4XD3X`; no hash of a build, so every release meets it |
| `Authorization` | `Allow` |

The requirement is the one `crates/xtask/src/release/mac_programs.rs` pins and
`cargo xtask release check-code-signature` holds every release's hostkeeper to; the test
`the_committed_profile_grants_full_disk_access_to_hostkeeper_by_its_signing_requirement`
(`crates/xtask/src/release/mac_programs/profile_tests.rs`) fails when the two differ.

A privacy payload is honoured only from device management on a Mac enrolled with user approval or
by Automated Device Enrollment; installed by hand it is refused. How the owner adds it to Apple
Business, and how to see that it is in force, is in
[docs/operations/mac-full-disk-access.md](../../docs/operations/mac-full-disk-access.md).
hostkeeper reports whether the grant is in force in every heartbeat (`docs/macos.md`, "Full Disk
Access").
