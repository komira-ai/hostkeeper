# Mac bootstrap: a tiny pkg that Apple installs, and hostkeeper takes over

Status: design, for the owner's review (2026-11-14). Nothing in this document is built unless it is tagged **Built** with a path. It is a living
document: when a task of section 10 lands, its row changes from **Designed** to **Built** with the path, and the text of the decision stays.
This is the owner's decision of 2026-11-14, worked out against the code; where the code says the decision needs a choice, the choice is an open
question (section 11), not a silent pick.

The owner's words: "We could also just make a one-time installer package that gets deployed. It's some simple code that doesn't change very
often. It goes and reaches out to coordinator to register, and then coordinator then installs services?", after "should we just turn on built
in device management? And then can we tap into that to deploy hostkeeper?"

## 1. The problem, and the solution in one page

**Problem.** A new Mac must reach "hostkeeper runs, the coordinator manages it" with no per-Mac hand work. Today the coordinator runs its own MDM
for that ([coordinator-mdm.md](coordinator-mdm.md)): ABM sync, an ADE profile, SCEP, APNs, and a pkg and a config profile sent to each Mac. Two
things broke the premise. The ABM token of the `js-mdm` server entry now answers `401`, and the owner cannot edit `js-mdm` in Apple Business, so
the coordinator cannot be repaired from our side. And the coordinator has no record of the Mac that enrolled before the move to the NUC, so
there is no device state to continue from.

**Solution.** Apple Business now has built-in device management. Its Blueprints install a signed `.pkg` from a URL and a SHA-256 that we host. We
use that for one thing only, a tiny **hostkeeper bootstrap** pkg. Everything after it is hostkeeper's own path, as on Linux.

1. The Blueprint pins the bootstrap pkg (URL and SHA-256). It has its own identifier, `dev.hostkeeper.bootstrap`, and changes rarely.
2. At first boot the bootstrap enrolls with the coordinator by the existing enrollment request and approval flow (`hostkeeper-enroll`,
   `hostkeeper-enrollment`): a key made on the Mac, the coordinator's CA pinned by its SHA-256, approval by the hash of the key. It carries no secret.
3. After approval it fetches hostkeeper's signed macOS pkg from the public repository, checks it as hostkeeper checks a `macos-pkg` (digest, then
   `pkgutil --check-signature` and a pinned signer), installs it, and hands over its host certificate.
4. From then on hostkeeper updates itself and the coordinator installs services through host groups, as on Linux. Apple never sees those packages,
   so declarative package management has nothing to drift against.

| | Today (coordinator MDM) | After |
|---|---|---|
| Who gets hostkeeper onto a Mac | the coordinator's MDM, `InstallEnterpriseApplication` | Apple Business, Blueprint, the bootstrap pkg only |
| Daemon config | managed preferences from the MDM config profile | the bootstrap writes it (section 4); or the Blueprint carries it (question 1) |
| Approval | MDM nonce, bound to ABM serial and supervision | inventory record by serial, or an operator, as on Linux |
| Apple-held state | none we can edit | the Blueprint (URL and hash of one pkg) |
| Coordinator-held state | ABM token, push certificate, SCEP CA, MDM name and TLS certificate | none for Macs: the enrollment CA and the host groups it already has |

What does not change: the daemon, the enrollment protocol, the host certificate, the host groups, the package trust of
[packages-repositories.md](packages-repositories.md) section 4.7, and the Linux path.

## 2. What exists and is reused (all **Built**)

| Piece | Where | Used for |
|---|---|---|
| Enroller: key made first, CA checked against `ca_sha256` before anything is sent, request posted once per key, signed poll, certificate checked and stored | `crates/hostkeeper-enroll` (`enroller.rs`, `verify.rs`, `grpc.rs`) | the bootstrap's whole enrollment step, linked as a library |
| Mac claims: serial and platform UUID from `ioreg` | `crates/hostkeeper-enroll/src/claims.rs` (`MacClaims`) | the request's claims; a record matches a Mac by serial |
| The pki directory, `/etc/updater/pki` (`pending/key`, `gen-<n>/`, `current`) | `crates/hostkeeper-enroll/src/store.rs` (`PkiDir`) | the hand-over to the daemon |
| CA, request store, approval by operator or by inventory record, host groups | `crates/hostkeeper-enrollment`, [enrollment-coordinator.md](../enrollment-coordinator.md), [operations/enrollment.md](../operations/enrollment.md) | the approval flow, unchanged |
| The Mac pkg: binary, launchd job, pinned `gh`, trusted root; a postinstall that loads the job; the start keeps an existing `/etc/hostkeeper/hostkeeper.toml` | `crates/xtask/src/release/macos_pkg.rs`, `crates/hostkeeper-launchd/src/postinstall.rs`, `crates/hostkeeper-daemon/src/mac_config.rs` | what the bootstrap installs; "the manual path" of [hostkeeper-mdm.md](hostkeeper-mdm.md) section 4 |
| `macos-pkg` signature rule: `pkgutil --check-signature` exits 0, and the leaf's SHA-256 is one of `signers` (1 to 4) of `/etc/hostkeeper/macos-pkg.toml` | `crates/hostkeeper-install/src/macos_pkg/signature.rs`, `signers.rs` | the bootstrap's check of hostkeeper's pkg |
| Repository descriptor `hostkeeper-repository.json` (`formats.macos-pkg`), the pointer `pkg/hostkeeper/stable.json` with each file's `sha256` and `size`, versioned directories | `crates/hostkeeper-release/src/descriptor.rs`, [deploy/package-repositories.md](../deploy/package-repositories.md) | where the bootstrap finds hostkeeper's pkg and its digest |
| Developer ID Installer signing of both Mac pkgs on a push to `main`, then `check-pkg-signature` | `.github/workflows/artifacts.yml` (`sign-pkg`, `check-pkg`) | signing the bootstrap too |
| A pkg-installed daemon is brought to the desired release by an `install` order | [operations/self-update-after-bootstrap.md](../operations/self-update-after-bootstrap.md) | "hostkeeper updates itself" |

## 3. The sequence

```mermaid
sequenceDiagram
    autonumber
    participant AB as Apple Business (Blueprint)
    participant Mac as Mac: bootstrap (root, launchd)
    participant Co as Coordinator (LAN listener)
    participant Op as Operator / inventory record
    participant Repo as packages.komira.ai
    AB->>Mac: install bootstrap pkg (URL + SHA-256 pinned)
    Mac->>Mac: postinstall loads job dev.hostkeeper.bootstrap
    Mac->>Co: TLS probe, SHA-256 of the CA vs ca_sha256 (nothing sent on mismatch)
    Mac->>Mac: make P-256 key, write it durably (pending/key)
    Mac->>Co: Enroll {csr, claims: serial + platform UUID, image: bootstrap version}
    alt a record matches the serial, window open, unused
        Co->>Co: approve in the record's host group (enrollment.auto_approved)
    else no record
        Op->>Co: Enrollment.Approve {request, key_sha256, host_group}
    end
    Mac->>Co: Poll (unsigned, then signed with the key)
    Co-->>Mac: 7-day host certificate
    Mac->>Mac: verify certificate against the pinned CA, store gen-1, current
    Mac->>Repo: GET hostkeeper-repository.json, then pkg/hostkeeper/stable.json
    Mac->>Repo: GET hostkeeper-<v>-macos-arm64.pkg (streamed, hashed as written)
    Mac->>Mac: size and SHA-256 equal the pointer's; pkgutil --check-signature; leaf pinned
    Mac->>Mac: installer -pkg (postinstall loads dev.hostkeeper.hostkeeper)
    Mac->>Co: hostkeeper daemon heartbeats with the certificate the bootstrap stored
    Co-->>Mac: orders: the host group's packages, hostkeeper's own update first in the list
```

The first four arrows after the Blueprint are `Enroller::run_once` as built; nothing in them is new. What is new is arrow 8 on (fetch, check, install,
hand over) and the pkg around it.

## 4. What the bootstrap carries

One root-owned file in its payload, `/usr/local/lib/hostkeeper-bootstrap/bootstrap.toml`, plus the binary and a launchd plist. Every number in it is
a key with a default from the start; none is a constant of the code.

| Value | Why | Source |
|---|---|---|
| `coordinator` | the LAN listener the Mac dials (`--lan-listen`; the Macs are on the lab LAN only, [coordinator-mdm.md](coordinator-mdm.md) 4.7) | farm-specific |
| `ca_sha256` | the whole trust in the coordinator: the enroller probes the TLS chain, takes its last certificate (the CA) and compares its SHA-256, then trusts that CA alone for every call (`grpc.rs`) | farm-specific; the CA is made once, so it does not change |
| `release_channel` | the repository base URL, `https://packages.komira.ai` | public |
| `macos_pkg_signers` | the leaf SHA-256 of the Developer ID Installer identity, as `macos-pkg.toml`'s `signers` (1 to 4) | public |
| `[bounds]` | the fetch timeout, the largest pkg, how long to wait for approval, the pause between failed fetches | defaults in code, as `hostkeeper-config` bounds are |

**Trust in the coordinator is a pin on the CA's hash, not a CA bundle and not trust on first use.** This is the Linux path's rule
([network-install.md](network-install.md) section 5): the image carries only public facts.

**No enrollment secret, and none is needed.** The key is made on the Mac, the request is approved by the SHA-256 of that key (compared whole, in
constant time), and the claims "authorise nothing" (`claims.rs`). The bootstrap pkg is hosted at a public URL, so it could not hold a secret anyway:
a secret in it is a secret everyone has. What keeps the wrong machine out is the approval, not the pkg (section 5).

**How the two farm-specific values reach a pkg that is the same for everyone.** [hostkeeper-mdm.md](hostkeeper-mdm.md) section 4 chose "the pkg
carries no config" so that one release asset has one digest and one attestation for every farm. That reasoning still holds, and the endpoint
address must not be committed (rule S1). So the two values are not in the release's pkg. Question 1 offers the choices; the preferred one keeps the
existing path: the Blueprint also delivers the preference domain `dev.hostkeeper.config` that `mac_config` already reads, and the bootstrap reads it
the same way, with the same trust check (root-owned, closed to others, `hostkeeper_config::trust::read_trusted`). If Apple's built-in management cannot
deliver a custom preference domain, the fallback is a per-org build of the bootstrap in the org's own pipeline, signed by the same identity, which is
a held step of the owner's, not a change of this repository.

## 5. The approval and the host group

Unchanged code, as [operations/enrollment.md](../operations/enrollment.md) "The Mac path" says it. A Mac request is approved in one of two ways:

- **Inventory record by serial.** `hkctl enrollment add-record --host-id mac-03 --host-group build --serial <serial>`. A record that names a group is
  the approval, for a window (24 hours by default, 7 days at most) and once; the request must match exactly one record and no other pending request
  may contest it. The coordinator approves it itself, in the record's group, and audits `enrollment.auto_approved`.
- **An operator** on the Enrollment page or `hkctl enrollment approve`, naming the key's hash and the `host_group`.

**What is lost against MDM, plainly.** The MDM nonce path proved the serial through Apple (ABM table, supervision, `EnrolledViaDEP`). Without MDM the
serial is a claim the Mac makes about itself; anyone on the lab LAN who knows a serial can post a request that claims it. The record's window and its
one use are the control, as for a Linux server's MAC. So the practice is: add the record when the Mac is about to be powered on, and read the key
hash in the coordinator against the bootstrap's log line when it matters. The `default_group` and "every ABM Mac is eligible" of
[coordinator-mdm.md](coordinator-mdm.md) 4.6 depend on ABM and are not carried over (question 3).

**Host group.** The group comes only from the record or the approval. A request approved with no group enrolls the host **unassigned**: it is ordered
no package, hostkeeper's own update included, until `HostGroups.Move` ([hostgroups.md](hostgroups.md) section 1.2). The bootstrap's hostkeeper still
runs and heartbeats; it just stays at the version the pkg carried.

## 6. How hostkeeper gets its daemon config

Today the Mac's daemon config comes from managed preferences, and `mac_config::at_start` writes `/etc/hostkeeper/hostkeeper.toml` only when no file
exists, so "a reinstall never changes a Mac's trust root". The bootstrap uses exactly that rule, with no daemon change:

1. After approval and before `installer`, the bootstrap writes `/etc/hostkeeper/hostkeeper.toml` (root, 0600, whole by temporary file, fsync, rename)
   from `coordinator`, `ca_sha256`, `release_channel` and the trusted signers of the release workflow, and `/etc/hostkeeper/macos-pkg.toml` from
   `macos_pkg_signers`. It refuses to replace a file that exists, as `mac_config` does.
2. It runs the daemon's own loader on the text first (`DaemonConfig::parse`), so a value the daemon would refuse stops the bootstrap, not the daemon.
3. The pkg's postinstall loads the job; the daemon start finds the file and keeps it.

`stores.toml` is not written: a pkg the coordinator resolves from a repository entry is fetched from the coordinator itself
([packages-repositories.md](packages-repositories.md) 4.7), so a Mac needs no store.

**A gap this opens, not closed here.** With the MDM gone, nothing refreshes `hostkeeper.toml` or `macos-pkg.toml` on a Mac: the config profile was the
only carrier, and a signer rotation reached Macs through it. The rotation rule already allows two signers during an overlap, so a rotation is a new
leaf added to the bootstrap and the file first, a release signed with it after, and the old leaf dropped last. How the file changes on an enrolled Mac
is question 5.

## 7. Signing and notarization

**What the release produces today.** The hostkeeper pkg and hello's pkg are signed by `Developer ID Installer: Komira AI, Inc. (U7RTX4XD3X)` with a
trusted timestamp (`artifacts.yml` `sign-pkg`, then `check-pkg`, on a push to `main` only), attested, and **not notarized**
(`docs/macos.md` "The installer's signature", `docs/deploy/verify-a-release.md`). There is no Developer ID **Application** identity in the workflow:
the binary inside carries the linker's ad-hoc signature (`hostkeeper-mdm.md` section 2). Notarization is nowhere in the release.

**What Apple Business requires of a Blueprint package is not in this repository and I did not check Apple's text.** What the owner said is
"a signed .pkg from a URL plus SHA-256". What I can say from the code: a signed pkg of our identity is the same kind the MDM's
`InstallEnterpriseApplication` took, and our own doc records that nothing Apple states asked that install for notarization, "added only if a Mac
refuses the signed pkg without it" (`docs/macos.md`). Whether the declarative path asks more is unverified (question 2).

If it does, notarizing a pkg is more than adding a step:

- `notarytool` accepts a pkg only if the Mach-O files in it carry a Developer ID **Application** signature with the hardened runtime and a secure
  timestamp. The bootstrap binary would need that identity, which the org may not have. (Only for the bootstrap: hostkeeper's pkg is fetched and run through
  `installer` by the bootstrap, not opened by a user, so I expect no Gatekeeper check on it, as for the `macos-pkg` installer today; not tested here.)
- The notarization ticket is stapled, which changes the pkg's bytes, so the digest, the attestation and `SHA256SUMS` must be of the stapled file,
  after `sign-pkg` and before `check-pkg`.
- A new secret (a notary API key) in the `pkg-signing` environment, held by the same rules (B10, the workflow lint names the job).

Because the bootstrap is small and changes rarely, notarizing only it is cheap if needed, and notarizing hostkeeper's pkg is not needed for this design.

## 8. Where the bootstrap is hosted, and how a new one is rolled out

**Hosting.** The release publishes it like hello's pkg: `hostkeeper-bootstrap-<version>-macos-arm64.pkg` and its bundle, at
`https://packages.komira.ai/pkg/hostkeeper-bootstrap/<version>/`, the file layout of the descriptor's `macos-pkg` format with a `packages` mapping
for the directory. The Blueprint takes that versioned URL and the file's SHA-256 from the `files` of the version's `SHA256SUMS`, the signed bytes. It
never takes `stable.json`: a pointer moves, a Blueprint pin must not.

**Rollout of a new bootstrap is editing the Blueprint**: a new URL and hash, by the owner, in Apple Business. No fleet rollout exists or is needed.
An enrolled Mac's bootstrap is a no-op (section 9), and only a Mac not yet enrolled needs the newest one. A bootstrap changes only for: a changed
farm value, a new signer leaf, a wire change the old bootstrap cannot speak (B8 makes enrollment additive, so the old one keeps working), or a Mac OS
change. Whether Apple reinstalls a pkg when the Blueprint's pin changes, and whether it reinstalls a removed one, is unverified (question 2); the
bootstrap stays installed, idle, rather than deleting itself, so a declarative "reinstall if missing" has nothing to reinstall.

## 9. What the bootstrap does, and latency per step

The bootstrap is a root launchd job (`dev.hostkeeper.bootstrap`, `RunAtLoad`, restarted by launchd only after a failure). On each start:

1. If `/usr/local/bin/hostkeeper` and its job are present and the pki directory holds a certificate, exit 0 and do nothing. This makes it idempotent
   and a no-op on every enrolled Mac.
2. Enroll: `Enroller::run_loop` unchanged, polling every `RETRY_SECS` (5) until approved. A failed step changes nothing it has not finished and is retried.
3. Fetch, check, install, hand over (sections 6 and 3).

**Latency budget.** Terms: **R** a Mac-to-coordinator round trip on the lab LAN; **W** a Mac-to-`packages.komira.ai` round trip; **F** one fsync;
**P** the poll period, 5 s (`RETRY_SECS`, **Built**); **D** the pkg download, size over throughput; **I** `installer`, with its postinstall;
**A** Apple's delivery of the bootstrap; **O** the operator's time when no record matches. All **Inferred**: nothing here is measured, and a term is
measured once on the farm Mac at MB1 (the build item).

| Step | Cost |
|---|---|
| Apple delivers and installs the bootstrap | **A** (Apple's schedule; not ours) |
| CA probe (TCP, TLS handshake, no data) | 2R |
| Key made and written | F |
| `Enroll` on a fresh channel (handshake 2R, call R), coordinator writes the request, and the record approves in the same write | 3R + 2F |
| Poll unsigned (nonce) and signed (answer); the certificate is made at this poll | 2R + F |
| Verify the certificate, store `gen-1` and `current` | a few F |
| Descriptor, then pointer (one connection) | 3W + W |
| The pkg: request, streamed, hashed in the same pass, no whole-file buffer | W + D |
| `pkgutil --check-signature`, then `installer` | local process time, plus I |
| First heartbeat of the daemon | its start plus R |

**With a record, first boot to installed:** A + 7R + 4F + 5W + D + I, and if the request lands just after a poll, up to one P more. **With manual
approval** the poll runs every P and the total gains O. `D` is the one term that grows with the release; the bootstrap itself is small. The numbers
are design figures, not tests (T10).

## 10. What happens to the coordinator's MDM code, ABM sync and push certificate

**Decision proposed: keep the code, idle and off; stop building the tasks that are not built; remove in a later, separate step.** The reasons:
the code is a large built surface (the server, SCEP, APNs, the ABM client and the setup page: tasks CM5 to CM22c of
[coordinator-mdm.md](coordinator-mdm.md) section 11), and removal is a multi-crate deletion with its own supersession check (B22). The owner's
rule "fix only lock-in now, defer refactors" applies. Idle costs nothing: with no `--mdm-*` flag the listener and the gate do not run.

| Part | Now | Later |
|---|---|---|
| `--mdm-listen`, `--mdm-api-url` and the rest | dropped from the NUC's unit (the owner's held script; not touched here) | the three NanoHUB flags go by CM14a/c as designed; the rest are removed with the code |
| ABM token (`401`), ABM key pair | not uploaded to the new store, so the setup says *not configured* and, as I read 4.5, the sync does not start; the `401` is not chased. Nothing to do with `js-mdm` in Apple Business | removed with the setup page |
| Push certificate | not uploaded; it is Apple's yearly renewal to forget. Nothing renews it | removed |
| Not-yet-built CM tasks (CM9, CM12b, CM23, CM10 and the rest of the cutover) | on hold; not started | closed when the removal lands |
| `--mac-serial-allowlist` | still read if given, but its nonce path needs the gate, so it approves nothing without it: it is a no-op | removed by its own deprecation (B8) |
| The package proxy (PP1 to PP6) | it serves the Macs' pkgs through `--mdm-listen`; whether it should be a listener of its own is question 4 | follows the answer |

**Migration, in order.**

1. Merge this design. Land MB1 to MB3 (section 12). Nothing running changes.
2. In Apple Business, turn on built-in device management, assign the Macs to it, and create the Blueprint with the bootstrap's URL and hash
   (owner's action; and, per question 1, the config payload).
3. Add the record (serial and group) of the first Mac, wipe it, and watch it enroll. The Mac that enrolled before the NUC move has no coordinator
   record and is wiped and enrolled like any other.
4. After the farm's Macs have run a few weeks on the new path, open the removal step as its own pull request with a supersession table.

## 11. Failure modes

| Failure | What happens | What the operator sees |
|---|---|---|
| **Coordinator unreachable at first boot** (down, wrong LAN, CA hash mismatch) | The CA check fails first, so nothing is sent and no key is made on a hash mismatch (`UntrustedCoordinator`); a connection failure is a failed step, retried every P. Nothing is installed, so a Mac that never reaches the coordinator never has hostkeeper | `/var/log/hostkeeper-bootstrap.log` on the Mac; no request in the queue |
| **Approval never given** | The request waits and expires after `--enrollment-request-ttl` (72 h). The coordinator answers a later `Poll` pending forever, and no answer tells the daemon it expired: it starts over only when the same key **posts again**. So the bootstrap exits after its `approval_wait` bound and launchd restarts it, which re-posts, and the request is listed again | the request in the Enrollment page; after the TTL, under `unenrolled` |
| **Pkg signature bad** (digest differs from the pointer, `pkgutil` fails, or the leaf is not a pinned signer) | Refused before `installer` reads it; the file is deleted; `hostkeeper` is not installed. The bootstrap keeps its certificate and retries after its pause, since the pointer may move to a good file. The same check order as `macos-pkg`: size and digest first, then the signature | the host is approved and **never sends a heartbeat**; the coordinator records the first heartbeat since approval, so the host shows approved and silent; the reason is in the Mac's log |
| **The pointer is wrong or old** | The digest is read from the same origin as the file, so it guards against corruption, not against the origin: the signer pin is the check that decides. An old signed hostkeeper installs and then updates itself to the desired version | none |
| **The Mac is re-imaged** | The Blueprint installs the bootstrap again; the pki directory is gone, so it makes a new key and a new request. The host id is held by the old key, so the record does not approve it (the record is used, and "no other key holds the host id" fails): an operator approves with `--replace true` in the same group. A different group is refused `host_group_differs` | one click; the old key's certificate stops being admitted |
| **A failed install after the certificate is stored** | `installer` is run again on the next start (it installs the same bytes). The certificate in the pki directory is kept, so a retry does not enroll again | the same as a bad signature |
| **The bootstrap runs on an enrolled Mac** | Step 1 of section 9: it exits 0 | none |

The wiped-Mac replacement is automatic only on the MDM path (the old key's own request claimed the same serial and the new MDM enrollment is newer).
Without MDM it is an operator's click; an additive coordinator rule that does the same by the serial of an inventory record is question 6.

## 12. Tasks, one pull request each

Size: S under a day, M a few days. All **Designed**.

| # | Task | Size |
|---|---|---|
| MB1 | Crate `hostkeeper-bootstrap`: the loop of section 9 over `hostkeeper-enroll`'s ports, the config check, the fetch, the check and the install, behind ports so every step has a fake; `main.rs` only forwards to the library (T4). The farm Mac measures the terms of section 9 | M |
| MB2 | `cargo xtask release macos-bootstrap-pkg` and its `check`: one component, the binary, the plist, the config, a postinstall that loads the job, identifier `dev.hostkeeper.bootstrap` | S |
| MB3 | `artifacts.yml`: the pkg joins `sign-pkg` and `check-pkg`; the release publishes it under `pkg/hostkeeper-bootstrap/<version>/`; the workflow lint gains the job lines with a negative case (T15) | S |
| MB4 | Notarization of the bootstrap alone, only if question 2 says so | M |
| MB5 | `docs/operations/mac-bootstrap.md`: the owner's Blueprint steps, the record, the approval, the logs; `docs/macos.md` and the table in [coordinator-mdm.md](coordinator-mdm.md) updated | S |
| MB6 | Coordinator: approve a re-imaged Mac's request by the serial of an inventory record, additive, only if question 6 says yes | S |

## 13. Open questions for the owner

1. **How do the two farm values (coordinator address, CA hash) reach a pkg that is the same for everyone?** (a) The Blueprint also delivers the
   `dev.hostkeeper.config` preference domain, if Apple Business's built-in management can carry custom settings: no change to the daemon, no
   per-org build. (b) A per-org bootstrap build in the org's own pipeline, signed with our identity. (c) Bake them into the public pkg, which puts
   an endpoint in a public repository against rule S1. I prefer (a), then (b). Can you see in Apple Business whether a Blueprint can hold a custom
   settings payload?
2. **What does a Blueprint's package need and do?** Is notarization required (and so a Developer ID Application identity, which the workflow does
   not have)? Does Apple reinstall the pkg when the pin changes or the receipt is missing? Must the URL answer `Content-Length` and `Range`? I did
   not read Apple's text and the repository does not hold it.
3. **Is an inventory record or a click acceptable as the approval for every Mac?** It replaces ABM's proof of the serial, and with it the "buy,
   power on, it enrolls" of [coordinator-mdm.md](coordinator-mdm.md) with "buy, add a record, power on". Or should the coordinator keep a read-only
   ABM sync as the proof of ownership? That needs a working ABM token, which is the thing that broke.
4. **Where does the package proxy serve Macs from** once the MDM listener is idle?
5. **How does `hostkeeper.toml` or `macos-pkg.toml` change on an enrolled Mac?** Options: a host group's configuration pkg
   ([hostgroups.md](hostgroups.md) section 6.6), or re-running the bootstrap. Which?
6. **Re-imaged Macs:** is an operator's click with `--replace true` enough, or should the coordinator approve it by the serial of its record (MB6)?
7. **Install first, or enroll first?** This design enrolls first, as you described, so nothing is installed before an approval and the bootstrap
   hands the certificate over. The simpler variant is to install hostkeeper first and let its daemon enroll, which is the built "manual path" and has
   no hand-over; the cost is that the approval gates only the daemon's start, not the download. The pkg is public either way.
8. **The signer pin:** the leaf's fingerprint (1 to 4, with the next certificate pre-pinned) or the Team ID? A new Developer ID Installer
   certificate otherwise needs a new bootstrap before a Mac can take a release signed with it.
9. **Remove the MDM code now or later?** Section 10 proposes later.
