# The coordinator's MDM (superseded)

Status: **removed.** This document designed an MDM server inside the coordinator: NanoHUB's
protocol served by the coordinator itself, Apple Business Manager sync, a SCEP identity CA, APNs
push, an operator setup page, a policy gate that approved a Mac by a one-time nonce, and a LAN
package proxy on the MDM listener. It was built, and on 2026-11-14 the owner decided to delete all
of the MDM from the coordinator: "I think we can delete all the mdm inside of the coordinator."

What replaces it: a Mac is onboarded through Apple Business built-in device management, which
installs a small bootstrap package. The package installs hostkeeper, and hostkeeper enrolls through
the normal approval flow (an operator's click, or an inventory record by serial). The design is
`docs/design/mac-bootstrap.md` and the runbook `docs/operations/mac-bootstrap.md`.

What was removed, and what stays:

| Removed | Why nothing needs it now |
|---|---|
| the crates `hostkeeper-mdm`, `-mdm-core`, `-mdm-step` (the Linux MDM host kit and the `mdm` package) | Apple's own management takes the place of NanoHUB and NanoDEP |
| `hostkeeper-mdm-ade`, `-mdm-scep`, `-mdm-push`, `-mdm-server`, `-mdm-gate`, `-mdm-model` | the coordinator's MDM server, its identity CA, push, wire types, policy gate and model checks |
| the coordinator's `--mdm-*` flags, `--mac-serial-allowlist` and `--proxy-cache-dir`, the MDM listener, the ABM sync, the `Mdm` service | the flags are now unknown arguments (usage error, exit 2); a call of the `Mdm` service is `UNIMPLEMENTED` |
| the package proxy (`docs/design/package-proxy.md`) | it was served only on the MDM listener's public-CA certificate; it was not deployed |
| the UI's MDM setup and devices pages, the `hkctl mdm` tests | the pages called the removed service |
| the release assets `hostkeeper-mdm-<version>-<platform>.*` and `mdm_<version>_amd64.deb` | withdrawn (`docs/deploy/verify-a-release.md`) |
| `xtask mdm-signing-test`, `abm-takeover-token`, `release mdm-fetch` | tools of the removed MDM |

| Stays | Why |
|---|---|
| enrollment and its approval by an operator or by an inventory record (`Enrollment.*`) | the bootstrap path enrolls through it |
| the macos-pkg installer, the daemon's managed-preference config (`dev.hostkeeper.config`) | the bootstrap package uses them |
| `mdm.proto` and every message and enum of it, `EnrollRequest.mac_nonce`, the alert kinds 12 to 15, the audit actors `mdm-nonce` and `mdm-gate` | a service, field or value that shipped is never taken away (`crates/hostkeeper-coordinator/src/compat.rs`); they are inert |
| the tables of migrations `0005`, `0011` and `0012` (`mdm_devices`, `mdm_enrollments`, `mdm_identities`, `mdm_commands`, `abm_devices`, `abm_state`, `mdm_secrets`, `mdm_settings`) | a format changes by addition only; no code reads or writes them, and a release before this one finds them where it left them |

The text of the removed design, its build log and its runbooks are in the history of the
repository (the last commit that holds them is the parent of the one that removed them).
