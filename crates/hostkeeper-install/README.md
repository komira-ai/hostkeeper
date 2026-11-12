# hostkeeper-install

The daemon's built-in installers ([packages design](../../docs/design/packages.md) section 5.1). Today
that is the `macos-pkg` installer of a signed flat `.pkg`
([design](../../docs/design/packages-repositories.md) section 4.7): it checks the package's signature
against the host's signers file, stages and activates it, keeps a record of what it placed, and removes or
repairs it. This page states what the code holds. It is tested against fakes on Linux, and on a Mac in
`.github/workflows/release.yml`'s job `macos-device` through
`crates/hostkeeper-privileged/tests/install_macos_pkg.rs`.

| module | what it holds |
|---|---|
| `host` | the host seam the installer runs on: the files, directories and owners it reads and writes |
| `lock` | the lock one installer run holds on a package's directory |
| `outcome` | what a step ends in, and the errors an installer reports |
| `version` | the sealed version directory of a package, `packages/<service>/<version>/`: `stage_filled` publishes a staging directory an installer filled (durable, sealed, renamed in, 0555) through the `PackageFs` port (`HostFs` on a host), `remove_orphans` removes what a crashed publish left, and `UnpackError::reason` is the token a step record reports a failure with |
| `mac_verify` | the port of the macOS checks of a package's files: the quarantine attribute and the code signature (`MacVerify`, `SignatureVerdict`, `VerifyIoError`); the daemon's `HostMacVerify` implements it |
| `tools` | the ports of the Mac's tools (`pkgutil`, `installer`, `lsbom`, `launchctl`), run through the bounded child-process runner of `hostkeeper-launchd` |
| `macos_pkg` | the installer, one module per concern, below |
| `macos_pkg::signers` | `/etc/hostkeeper/macos-pkg.toml`: the leaf-certificate fingerprints a package may be signed by, read at the daemon's start; a file that is not sound is refused, a missing one is no signers |
| `macos_pkg::signature` | the judgement of `pkgutil --check-signature`'s answer against the signers |
| `macos_pkg::xml` | the bounded reader of the XML of a package's `Distribution`, `PackageInfo` and plists |
| `macos_pkg::shape` | the shape a package must have: its components, scripts, and the paths it may place |
| `macos_pkg::payload` | the payload's files and the digests of the `tree/` copy they are checked against |
| `macos_pkg::receipt` | the receipts `pkgutil` keeps for an installed package |
| `macos_pkg::record` | the record the installer keeps (`installed` of `placed.json`) |
| `macos_pkg::stage` | unpacking and judging a package before anything is installed |
| `macos_pkg::activate` | running `installer` and loading the package's launchd jobs |
| `macos_pkg::remove` | removing what a package placed, by its record |
| `macos_pkg::drift` | comparing the installed files with the sealed copy |
