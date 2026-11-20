# Mac bootstrap: a tiny pkg that Apple installs, and hostkeeper takes over

Status: designed and built (2026-11-14). A part is **Built** when it carries a path; the rest is
**Designed**. It is a living document: when a task of section 12 lands, its row changes from **Designed**
to **Built** with the path, and the text of the decision stays. This is the owner's decision of 2026-11-14,
worked out against the code. The owner gave the green light to implement after the design and the
nine open questions were settled with defaults the same day; each is marked "default, owner may change"
in section 13 and where it shapes the text.

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

1. The Blueprint pins the bootstrap pkg (URL and SHA-256). It has its own identifier, `dev.hostkeeper.bootstrap`, and changes rarely. The
   Blueprint also carries the farm's two values as the managed preference domain `dev.hostkeeper.config` (section 4); a Mac set up by hand has them in a file.
2. The bootstrap, run by launchd at load, reads the farm values, fetches hostkeeper's signed macOS pkg from the public repository, checks it (size and
   SHA-256 of the repository's pointer, then `pkgutil --check-signature`, then the signer's Team ID), writes the daemon's config when it is absent
   (section 6), writes the update-account file when the settings name the update account (section 4), runs `installer`, and exits. The pkg carries no
   secret, and the bootstrap **does no enrollment**.
3. hostkeeper's own daemon starts, and enrolls through the existing enrollment request and approval flow (`hostkeeper-enroll`, `hostkeeper-enrollment`):
   a key made on the Mac, the coordinator's CA pinned by its SHA-256, approval by the hash of the key (an operator's click, or an inventory record by serial).
4. From then on hostkeeper updates itself and the coordinator installs services through host groups, as on Linux. Apple never sees those packages,
   so declarative package management has nothing to drift against.

| | Today (coordinator MDM) | After |
|---|---|---|
| Who gets hostkeeper onto a Mac | the coordinator's MDM, `InstallEnterpriseApplication` | Apple Business, Blueprint, the bootstrap pkg only; the bootstrap installs hostkeeper |
| Daemon config | managed preferences from the MDM config profile | the bootstrap writes it (section 6) from the Blueprint's preference domain or a settings file |
| Approval | MDM nonce, bound to ABM serial and supervision | inventory record by serial, or an operator's click, as on Linux |
| Apple-held state | none we can edit | the Blueprint (URL and hash of one pkg, and the preference domain) |
| Coordinator-held state | ABM token, push certificate, SCEP CA, MDM name and TLS certificate | none for Macs: the enrollment CA and the host groups it already has |

What does not change: the daemon, the enrollment protocol, the host certificate, the host groups, the package trust of
[packages-repositories.md](packages-repositories.md) section 4.7, and the Linux path.

## 2. What exists and is reused, and what is new (all **Built**)

| Piece | Where | Used for |
|---|---|---|
| Enroller: key made first, CA checked against `ca_sha256` before anything is sent, request posted once per key, signed poll, certificate checked and stored | `crates/hostkeeper-enroll` | the **daemon's** enrollment, unchanged; the bootstrap does not link it |
| Mac claims: serial and platform UUID from `ioreg` | `crates/hostkeeper-enroll/src/claims.rs` (`MacClaims`) | the request's claims; a record matches a Mac by serial |
| CA, request store, approval by operator or by inventory record, host groups | `crates/hostkeeper-enrollment`, [enrollment-coordinator.md](../enrollment-coordinator.md), [operations/enrollment.md](../operations/enrollment.md) | the approval flow, unchanged |
| The Mac pkg: binary, launchd job, pinned `gh`, trusted root; a postinstall that loads the job; the start keeps an existing `/etc/hostkeeper/hostkeeper.toml` | `crates/xtask/src/release/macos_pkg.rs`, `crates/hostkeeper-launchd/src/postinstall.rs`, `crates/hostkeeper-daemon/src/mac_config.rs` | what the bootstrap installs; "the manual path" of hostkeeper-mdm.md section 4 (deleted; its text is in git history) |
| The daemon config loader and its trust rule (`DaemonConfig::parse`, `read_trusted`) | `crates/hostkeeper-config` | the bootstrap reads its settings files as the daemon reads its config, and parses the config it writes with the daemon's own loader |
| The tool port for `pkgutil` and `installer`, and the `pkgutil --check-signature` reader | `crates/hostkeeper-install` (`tools`, `macos_pkg/signature.rs`) | the bootstrap's tools behind a port with fakes, and its reading of the status and the leaf's fingerprint |
| The HTTPS transport port (`Transport`, `UreqTransport`) | `crates/hostkeeper-package/src/net.rs` | the bootstrap's downloads |
| Repository descriptor `hostkeeper-repository.json` (`formats.macos-pkg`), the pointer `pkg/hostkeeper/stable.json` with each file's `sha256` and `size`, versioned directories | `crates/hostkeeper-release/src/descriptor.rs`, [deploy/package-repositories.md](../deploy/package-repositories.md) | where the bootstrap finds hostkeeper's pkg and its digest |
| Developer ID Installer signing of the Mac pkgs on a push to `main`, then `check-pkg-signature` | `.github/workflows/artifacts.yml` (`sign-pkg`, `check-pkg`) | signing the bootstrap pkg too (section 7) |
| A pkg-installed daemon is brought to the desired release by an `install` order | [operations/self-update-after-bootstrap.md](../operations/self-update-after-bootstrap.md) | "hostkeeper updates itself" |
| **The bootstrap program** | `crates/hostkeeper-bootstrap` | sections 4, 6 and 9 |
| **The bootstrap pkg's layout** (plist, postinstall) | `packaging/macos-bootstrap/` | section 8 |
| **The pkg's builder and its check** | `crates/xtask/src/release/macos_bootstrap.rs` (`release macos-bootstrap-pkg`, `release check-macos-bootstrap-pkg`) | section 8 |

## 3. The sequence

```mermaid
sequenceDiagram
    autonumber
    participant AB as Apple Business (Blueprint)
    participant Mac as Mac: bootstrap (root, launchd)
    participant Repo as packages.komira.ai
    participant D as Mac: hostkeeper daemon
    participant Co as Coordinator (LAN listener)
    participant Op as Operator / inventory record
    AB->>Mac: install bootstrap pkg (URL + SHA-256 pinned) and the dev.hostkeeper.config preferences
    Mac->>Mac: postinstall loads job dev.hostkeeper.bootstrap
    Mac->>Mac: read coordinator + ca_sha256 (managed preferences, else bootstrap.toml)
    Mac->>Repo: GET hostkeeper-repository.json, then pkg/hostkeeper/stable.json
    Mac->>Repo: GET hostkeeper-<v>-macos-arm64.pkg (streamed, hashed as written)
    Mac->>Mac: size and SHA-256 equal the pointer's; pkgutil --check-signature; Team ID pinned
    Mac->>Mac: write hostkeeper.toml and macos-pkg.toml when absent
    Mac->>Mac: installer -pkg (postinstall loads dev.hostkeeper.hostkeeper); the bootstrap exits 0
    D->>Co: TLS probe, SHA-256 of the CA vs ca_sha256 (nothing sent on mismatch)
    D->>Co: Enroll {csr, claims: serial + platform UUID}
    alt a record matches the serial, window open, unused
        Co->>Co: approve in the record's host group (enrollment.auto_approved)
    else no record
        Op->>Co: Enrollment.Approve {request, key_sha256, host_group}
        Co->>Co: write the Mac's inventory record from its serial (record_add by coordinator)
    end
    D->>Co: Poll (unsigned, then signed with the key)
    Co-->>D: 7-day host certificate
    D->>Co: heartbeats
    Co-->>D: orders: the host group's packages, hostkeeper's own update first in the list
```

Arrows 1 to 9 are new (the bootstrap); arrows 10 to 15 are `Enroller::run_once` as built, run by the daemon; nothing in them changed.

## 4. What the bootstrap carries, and where its settings come from

The pkg carries no config and no farm value: one release asset has one digest and one attestation for every farm
(hostkeeper-mdm.md section 4 (deleted; its text is in git history), which this keeps), and the endpoint address must not be committed (rule S1).
The settings are read at each start by `crates/hostkeeper-bootstrap/src/settings.rs`, from the first of two sources that is there
(**Q1, default, owner may change**):

1. **The managed preferences of the domain `dev.hostkeeper.config`**, `/Library/Managed Preferences/dev.hostkeeper.config.plist`: what a Blueprint's
   configuration writes. This is the domain the daemon's own `mac_config` reads, so one payload serves both.
2. **The file `/Library/Application Support/hostkeeper/bootstrap.toml`**, TOML with the same keys, for a Mac set up by hand.

Either is read as a file root owns that only its owner can write (`hostkeeper_config::trust::read_trusted`), and a source that is there and refused is an
error, never a reason to try the next one. The keys; **every number is a key with a default from the start**, none is a constant of the code:

| Key | Default | Why |
|---|---|---|
| `coordinator` (required) | none | the LAN listener the daemon dials (`--lan-listen`; the Macs are on the lab LAN only, coordinator-mdm.md 4.7, removed); farm-specific |
| `ca_sha256` (required) | none | the whole trust in the coordinator, for the daemon: it probes the TLS chain, takes its last certificate (the CA) and compares its SHA-256; farm-specific; the CA is made once |
| `release_channel` | `https://packages.komira.ai` | the repository base URL; public |
| `team_id` | `U7RTX4XD3X` (**Q8, default, owner may change**) | the Team ID of the Developer ID Installer identity that must have signed hostkeeper's pkg |
| `macos_pkg_signers` | empty | leaf SHA-256s written to `macos-pkg.toml` (1 to 4); empty means the leaf of the pkg that was checked (section 6) |
| `[trusted_signers]` | `komira-ai`, `.github/workflows/release.yml`, `refs/tags/*`, hosted runner | the daemon's loader requires it; the daemon no longer reads it |
| `[bounds]` `fetch_timeout_secs` | 1800 | the longest one download may take |
| `[bounds]` `connect_timeout_secs` | 30 | the longest a connection is waited for |
| `[bounds]` `max_package_bytes` | 536870912 | the largest pkg, and so the most bytes written for one (the daemon's own default bound) |
| `[bounds]` `tool_timeout_secs` | 60 | the longest `pkgutil` is waited for |
| `[bounds]` `installer_timeout_secs` | 600 | the longest `installer` is waited for |
| `[bounds]` `wait_secs` | 900 | how long a failed run keeps retrying before it exits (section 9) |
| `[bounds]` `retry_secs` | 30 (at least 1) | the pause between two attempts |
| `update_account_name` | none (all four or none) | the update account of [maintenance-updates.md](maintenance-updates.md) section 5.5, `hkupdate` there; written to the update-account file |
| `update_account_password` | none (all four or none) | its password; written to the update-account file |
| `admin_name` | none (all four or none) | the administrator made at Setup Assistant, a holder of a Secure Token, which gives the update account its own |
| `admin_password` | none (all four or none) | that administrator's password; the bootstrap writes it nowhere |

**The update account's four keys** (`crates/hostkeeper-bootstrap/src/update_account.rs`) are given all four or none. A source with none of them
makes no update-account file, as before they existed; such a Mac has no update account and is never ordered a macOS update. A source with some of
them is refused, naming the first one missing in the order of the table; nothing is fetched. Each is a string (a number, say, is refused as "is not a
string", never quoted): a name is ASCII letters, digits, `_`, `.` and `-`, not starting with `-` (the rule of the daemon's reader); a password is not
empty and holds no line break; the two names differ; and the file the first two make is at most 4096 bytes, the reader's bound. No error or log line holds a value of these keys, and no debug print holds a
password: an error names the key and the rule, and a password prints as `Password(withheld)` (the names print as they are).

The bootstrap writes `/Library/Application Support/hostkeeper/update-account` (root's, mode 0600; section 6) from the first two, in the layout the
daemon's reader takes (`crates/hostkeeper-daemon/src/update_account.rs`): the line `name=<update_account_name>`, then the line
`password=<update_account_password>`, each ending in a newline, the value byte for byte.

**The administrator's password is used for the account-creation call only and not kept: the bootstrap writes no copy of it.** The daemon reads the
two `admin_` keys from the same settings source (the managed preferences, else `bootstrap.toml`), under the same trust rule, at the start that makes
the account, and uses them for that call alone (**Designed**: the daemon's account-making start is later work; until it lands the two keys are checked
by the bootstrap and read by nothing). A one-shot file handed from the bootstrap to the daemon's first start was the other way, and is not
chosen: the source holds the password anyway, so a second copy adds a file to remove and a window in which it is there, and a daemon that never ran
the call (a release before it, or a start that failed first) would leave it behind. The settings that hold the passwords are never committed to a
repository and never printed. The operator sets a `bootstrap.toml` that holds them to mode 0600; the bootstrap checks only that no account but
root can write it (the trust rule), not who can read it. The mode macOS gives the managed preferences file is its own
**[V]**, and the owner's ruling that the update account's password is not treated as a secret (maintenance-updates.md section 5.5, "Threat model")
is what covers it.

The text of either source is also made into the daemon's config and parsed by the daemon's own loader before anything is fetched, so a value the daemon
would refuse stops the bootstrap with the loader's reason, not the daemon at its first start.

**Trust in the coordinator is a pin on the CA's hash, not a CA bundle and not trust on first use.** This is the Linux path's rule
([network-install.md](network-install.md) section 5): the image carries only public facts. The bootstrap itself talks to the repository only, over
HTTPS under the system's roots; it never talks to the coordinator.

**No enrollment secret, and none is needed.** The key is made by the daemon on the Mac, the request is approved by the SHA-256 of that key (compared whole, in
constant time), and the claims "authorise nothing" (`claims.rs`). The bootstrap pkg is hosted at a public URL, so it could not hold a secret anyway:
a secret in it is a secret everyone has. What keeps the wrong machine out is the approval, not the pkg (section 5).

**Not checked, later work:** the pkg's identifier and a minimum version. The pin is the Team ID (section 7), so another pkg of the same team served from the repository would pass; the repository is the origin the owner trusts, and an identifier check needs `pkgutil --expand`, which was not measured.

**Unverified:** whether a Blueprint of Apple Business's built-in management can carry a custom preference domain. If it cannot, the Macs use the
fallback file (a held step of the owner's per Mac, or a per-org pkg built in the org's own pipeline, signed by the same identity, which is not a change of
this repository).

## 5. The approval and the host group

Unchanged code, as [operations/enrollment.md](../operations/enrollment.md) "The Mac path" says it. A Mac's request is approved in one of two ways
(**Q3 and Q6, default, owner may change**: an operator's click is acceptable for every Mac, and a re-imaged Mac takes `--replace true`):

- **Inventory record by serial.** `hk enrollment add-record --host-id mac-03 --host-group build --serial <serial>`. A record that names a group is
  the approval, for a window (24 hours by default, 7 days at most) and once; the request must match exactly one record and no other pending request
  may contest it. The coordinator approves it itself, in the record's group, and audits `enrollment.auto_approved`.
- **An operator** on the Enrollment page or `hk enrollment approve`, naming the key's hash and the `host_group`. The coordinator then writes
  the Mac's inventory record from its serial, added by `coordinator`, with no group and no window, so the Mac is listed in the inventory and the
  record approves nothing ([operations/enrollment.md](../operations/enrollment.md#a-host-approved-without-a-record)); a Mac approved so before
  this is recorded at the coordinator's next start.

**What is lost against MDM, plainly.** The MDM nonce path proved the serial through Apple (ABM table, supervision, `EnrolledViaDEP`). Without MDM the
serial is a claim the Mac makes about itself; anyone on the lab LAN who knows a serial can post a request that claims it. The record's window and its
one use are the control, as for a Linux server's MAC. So the practice is: add the record when the Mac is about to be powered on, and read the key
hash in the coordinator against the daemon's log line when it matters. The `default_group` and "every ABM Mac is eligible" of
coordinator-mdm.md 4.6 (removed) depend on ABM and are not carried over.

**What installing first costs (Q7, default, owner may change).** The daemon enrolls, so the approval gates the daemon's heartbeat and its orders, not the
download of its pkg. The pkg is public either way.

**Host group.** The group comes only from the record or the approval. A request approved with no group enrolls the host **unassigned**: it is ordered
no package, hostkeeper's own update included, until `HostGroups.Move` ([hostgroups.md](hostgroups.md) section 1.2). The daemon still runs and heartbeats; it just stays
at the version the pkg carried.

## 6. How hostkeeper gets its daemon config

Today the Mac's daemon config comes from managed preferences, and `mac_config::at_start` writes `/etc/hostkeeper/hostkeeper.toml` only when no file
exists, so "a reinstall never changes a Mac's trust root". The bootstrap uses exactly that rule, with no daemon change
(`crates/hostkeeper-bootstrap/src/files.rs`):

1. After the pkg has passed every check and before `installer`, the bootstrap writes `/etc/hostkeeper/hostkeeper.toml` (root, 0600) from `coordinator`,
   `ca_sha256`, `release_channel` and `[trusted_signers]`, and `/etc/hostkeeper/macos-pkg.toml` (0644) from `macos_pkg_signers`, or, when that is empty,
   from the leaf certificate of the pkg it has just checked (its Team ID is pinned and Apple issued it, so the leaf is the signer a later release of the
   same identity needs a rotation for). Each is written whole to a temporary file in the same directory, synced, and hard-linked to its path, which fails when the
   file appeared meanwhile; a file that is there as a regular file is kept as it is (refused when group- or other-writable), and a link or a directory is refused and left. If `installer` then fails, the files this attempt made are removed. The staging directory must be owned by root and not group- or other-writable, or the attempt fails.
2. The text was parsed by the daemon's own loader (`DaemonConfig::parse`, and the signers file's parser) when the settings were read, so a value the
   daemon would refuse stops the bootstrap, not the daemon.
3. When the settings name the update account (section 4), the bootstrap writes `/Library/Application Support/hostkeeper/update-account` the same
   way, with mode 0600, after the two configs and before `installer`, so the daemon's first start finds it. One that is there is kept as it is, even
   when its bytes differ from the settings (the account it names may already have that password; its contents are not checked, and the daemon's
   reader judges them), but only when it is a regular file of root's with
   mode 0600 exactly, what the daemon's reader requires; any other file there is refused and left, and the attempt fails. If a write or `installer`
   fails, every file this attempt made is removed.
4. The pkg's postinstall loads the job; the daemon start finds the files and keeps them.

`stores.toml` is not written: a pkg the coordinator resolves from a repository entry is fetched from the coordinator itself
([packages-repositories.md](packages-repositories.md) 4.7), so a Mac needs no store.

**A gap this opens, not closed here (Q4, Q5 and Q9, default: out of scope now).** With the MDM idle, nothing refreshes `hostkeeper.toml` or `macos-pkg.toml` on
an enrolled Mac: the config profile was the only carrier, and a signer rotation reached Macs through it. The rotation rule already allows two signers during an
overlap, so a rotation is a new leaf added to the file first, a release signed with it after, and the old leaf dropped last. How the file changes on an enrolled
Mac (a host group's configuration pkg, [hostgroups.md](hostgroups.md) section 6.6, or re-running the bootstrap) is **later work**.

## 7. Signing and notarization

**What the release produces.** The hostkeeper pkg, hello's pkg and the bootstrap pkg are signed by `Developer ID Installer: Komira AI, Inc. (U7RTX4XD3X)`
with a trusted timestamp (`artifacts.yml` `sign-pkg`, then `check-pkg`, on a push to `main` only), attested, and **not notarized**
(`docs/macos.md` "The installer's signature", `docs/deploy/verify-a-release.md`). There is no Developer ID **Application** identity in the workflow:
the binaries inside carry the linker's ad-hoc signature (the deleted `hostkeeper-mdm.md` section 2).

**Q2 (default, owner may change): no notarization work now; MB4 is deferred.** The bootstrap pkg is signed with the existing identity, built and uploaded
beside the hostkeeper pkg with a versioned URL, and its SHA-256 is in the release's `SHA256SUMS` and in the `SHA256SUMS` of its version directory (section 8).

**What Apple Business requires of a Blueprint package is not in this repository and I did not check Apple's text.** What the owner said is
"a signed .pkg from a URL plus SHA-256". A signed pkg of our identity is the same kind the MDM's `InstallEnterpriseApplication` took, and our own doc records that
nothing Apple states asked that install for notarization, "added only if a Mac refuses the signed pkg without it" (`docs/macos.md`). Whether the declarative
path asks more is unverified. If it does, notarizing a pkg is more than adding a step:

- `notarytool` accepts a pkg only if the Mach-O files in it carry a Developer ID **Application** signature with the hardened runtime and a secure
  timestamp. The bootstrap binary would need that identity, which the org may not have. (Only for the bootstrap: hostkeeper's pkg is fetched and run through
  `installer` by the bootstrap, not opened by a user, so I expect no Gatekeeper check on it; not tested here.)
- The notarization ticket is stapled, which changes the pkg's bytes, so the digest, the attestation and `SHA256SUMS` must be of the stapled file,
  after `sign-pkg` and before `check-pkg`.
- A new secret (a notary API key) in the `pkg-signing` environment, held by the same rules (B10, the workflow lint names the job).

**The signer pin (Q8, default, owner may change).** The bootstrap pins the signer of hostkeeper's pkg by **Team ID**, not by a leaf fingerprint, so a new
Developer ID Installer certificate of the same team needs no new bootstrap (`crates/hostkeeper-bootstrap/src/signer.rs`). The check is: `pkgutil --check-signature`
exits 0; its status is "signed by a developer certificate issued by Apple" (the other statuses the host's macos-pkg installer takes, a certificate trusted through
a profile and Apple's own software, are refused, since a root of the farm's own could issue a leaf that names any Team ID); the leaf's name is a
`Developer ID Installer:` identity ending in `(<Team ID>)`; and the size and SHA-256 are the repository pointer's. The pointer is read from the same origin as the file, so
its digest guards against a cut or corrupted transfer, not against the origin: the signer pin is the check that decides.

## 8. Where the bootstrap is hosted, how it is built, and how a new one is rolled out

**The pkg.** `hostkeeper-bootstrap-<version>-macos-arm64.pkg` (`cargo xtask release macos-bootstrap-pkg`, checked by `check-macos-bootstrap-pkg`;
`crates/xtask/src/release/macos_bootstrap.rs`), a distribution pkg of one component, identifier `dev.hostkeeper.bootstrap`, that installs exactly two files and the
directories above them, every entry owned by root and wheel (`packaging/macos-bootstrap/`):

| Installed at | Mode | Source |
|---|---|---|
| `/usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap` | 0755 | the binary of `crates/hostkeeper-bootstrap` |
| `/Library/LaunchDaemons/dev.hostkeeper.bootstrap.plist` | 0644 | `packaging/macos-bootstrap/dev.hostkeeper.bootstrap.plist` |

Its one script is `postinstall` (`packaging/macos-bootstrap/postinstall`): `launchctl print` of the job, and `launchctl bootstrap` only when it is not loaded; a loaded
job is never booted out. The job runs the bootstrap at load and again only after a run that failed (`KeepAlive` `SuccessfulExit` false, `ThrottleInterval` 60), and writes
`/var/log/hostkeeper-bootstrap.log`.

**The release.** `artifacts.yml` builds it on the macOS runner of the release pipeline only (job `build-macos`; never a pull request, rules B9 and B10), signs it in `sign-pkg` with the same
identity and keychain as the other two pkgs, checks the signature and that signing changed no file in `check-pkg`, and attests it. The workflow lint
(`crates/xtask/tests/workflows/pkg_signing.rs`) holds each step, with a negative case for each.

**Hosting (default, owner may change).** The release publishes it as one more installer of the product, in the directory of the hostkeeper pkg:
`https://packages.komira.ai/pkg/hostkeeper/<version>/hostkeeper-bootstrap-<version>-macos-arm64.pkg`, with its `.sigstore.json` bundle. The first version of this design said
`pkg/hostkeeper-bootstrap/<version>/`; the publishing role may write only below `pkg/hostkeeper/` (`crates/xtask/src/channels/spec.rs`, `Product::installers`), and a directory of its
own would be an IAM change of the owner's. The descriptor's `macos-pkg` `packages` mapping names `hostkeeper-bootstrap` as living in the `hostkeeper` directory (`channels descriptor`).
The Blueprint takes that versioned URL and the file's SHA-256 from the `files` of the version's `SHA256SUMS` (published in each release's `SHA256SUMS` too), the signed bytes. It
never takes `stable.json`: a pointer moves, a Blueprint pin must not. (The pointer lists the bootstrap pkg among the `files` of the version; the bootstrap reads only the entry of hostkeeper's own pkg.)

**Rollout of a new bootstrap is editing the Blueprint**: a new URL and hash, by the owner, in Apple Business. No fleet rollout exists or is needed.
An enrolled Mac's bootstrap is a no-op (section 9), and only a Mac not yet set up needs the newest one. A bootstrap changes only for: a changed
farm value, a new settings key, a wire change the old bootstrap cannot speak (B8 makes the repository formats additive, so the old one keeps working), or a Mac OS
change. Whether Apple reinstalls a pkg when the Blueprint's pin changes, and whether it reinstalls a removed one, is unverified (question 2); the
bootstrap stays installed, idle, rather than deleting itself, so a declarative "reinstall if missing" has nothing to reinstall.

## 9. What the bootstrap does, and latency per step

The bootstrap is a root launchd job (`dev.hostkeeper.bootstrap`, `RunAtLoad`, restarted by launchd only after a failure). On each start (`crates/hostkeeper-bootstrap/src/run.rs`):

1. If `/etc/updater/pki/current` is there, or `/usr/local/bin/hostkeeper` is there **and** the marker `/var/lib/hostkeeper-bootstrap/installed` is (made after `installer` exited 0), exit 0 and do nothing. This makes it idempotent and a no-op on every enrolled Mac
   and on one the bootstrap installed. The binary alone is not enough: the pkg lays its payload down before its postinstall, so a failed postinstall leaves a binary and no loaded job, and that Mac is installed again. A Mac with a hand-installed binary and no marker is installed again too, which is harmless: existing configs are kept.
2. Read the settings (section 4), then the repository's descriptor and pointer (`hostkeeper_release::descriptor`), and download the pkg into a new root-only file in
   `/var/lib/hostkeeper-bootstrap` (mode 0600, never replacing a file), hashed in the same pass, held to the pointer's size and SHA-256 and to `max_package_bytes`.
3. `pkgutil --check-signature` and the Team ID pin (section 7). A pkg that fails any check is deleted, and nothing else is written or run.
4. Write `hostkeeper.toml` and `macos-pkg.toml` when absent, and the update-account file when the settings name the update account (section 6),
   run `installer -pkg <file> -target /`, delete the file and exit 0.

**The bounded wait (default, owner may change).** A failed attempt (no settings yet, no network, a bad pkg, a failed install) is retried after `retry_secs`, until the sum of
the pauses would pass `wait_secs`; then the program exits non-zero with the last error and launchd starts it again after its throttle. The Blueprint's preference payload may arrive
after the pkg, so settings that appear during the wait are used by the next attempt. A step changes nothing it has not finished, so every attempt is safe to repeat.

**Latency budget.** Terms: **W** a Mac-to-`packages.komira.ai` round trip; **F** one fsync; **D** the pkg download, size over throughput; **I** `installer`, with its postinstall;
**A** Apple's delivery of the bootstrap; **P** `pkgutil --check-signature`. All **Inferred**: nothing here is measured, and a term is measured once on the farm Mac (task MB1's
measurement, still to do).

| Step | Cost |
|---|---|
| Apple delivers and installs the bootstrap | **A** (Apple's schedule; not ours) |
| Read the settings | local reads |
| Descriptor, then pointer (one connection reused if the transport keeps it) | about 4W |
| The pkg: request, streamed, hashed in the same pass, no whole-file buffer | W + D |
| `pkgutil --check-signature` | **P**, local |
| Write the two config files | 2F (a temporary file synced, then linked) |
| `installer`, with its postinstall | **I** |

**Bootstrap to an installed daemon:** A + 5W + D + P + 2F + I, plus up to `retry_secs` for each failed attempt. Then the daemon's enrollment (its own cost, [hostkeeper-design.md](hostkeeper-design.md) section 5.7)
and the operator's time when no record matches. `D` is the one term that grows with the release; the bootstrap itself is small. The numbers are design figures, not tests (T10).

## 10. What happened to the coordinator's MDM code, ABM sync and push certificate

**Decision (Q9, changed by the owner on 2026-11-14): the MDM code is removed from the coordinator.** The owner said: "I think we can delete all the mdm inside of the coordinator."
The removal is the change that deleted the nine `hostkeeper-mdm*` crates, the coordinator's `--mdm-*` flags, `--mac-serial-allowlist` and `--proxy-cache-dir`, the MDM listener, the ABM sync, push and SCEP code, the `Mdm` service implementation, the package proxy and the release assets of the `mdm` package, with its supersession check (B22) in its pull request. What it kept is additive-only (B8):
the migrations and their tables (unused), `mdm.proto` (every call answers `UNIMPLEMENTED`), `EnrollRequest.mac_nonce` (read and ignored), the alert kinds 12 to 15, and the audit actors `mdm-nonce` and `mdm-gate`. [coordinator-mdm.md](coordinator-mdm.md) is now a short superseded note.

| Part | Now |
|---|---|
| `--mdm-listen`, `--mdm-api-url` and the rest | removed; a coordinator started with one exits with a usage error, so the deployment that starts it stops passing them first |
| ABM token (`401`), ABM key pair, push certificate | no code reads them; nothing to renew; nothing to do with `js-mdm` in Apple Business |
| `--mac-serial-allowlist` | removed; a Mac is approved by an operator's click or an inventory record by serial |
| The package proxy (PP1 to PP6) | removed with the MDM listener (it was served only there and was never deployed); whether the Macs need a LAN pkg source is question 4 |

**Migration, in order.**

1. Merge this design and the bootstrap. Nothing running changes.
2. In Apple Business, turn on built-in device management, assign the Macs to it, and create the Blueprint with the bootstrap's URL and hash
   (owner's action; and, per section 4, the configuration payload of `dev.hostkeeper.config`).
3. Add the record (serial and group) of the first Mac, wipe it, and watch it install hostkeeper and enroll. The Mac that enrolled before the NUC move has no coordinator
   record and is wiped and enrolled like any other.
4. After the farm's Macs have run a few weeks on the new path, open the removal step as its own pull request with a supersession table.

## 11. Failure modes

| Failure | What happens | What the operator sees |
|---|---|---|
| **No settings** (the Blueprint's preferences not there yet, no fallback file) | The attempt fails with `no farm values`, and is retried every `retry_secs` until `wait_secs`; settings that arrive meanwhile are used. Then the program exits non-zero and launchd starts it again | `/var/log/hostkeeper-bootstrap.log`; no request in the queue |
| **Settings refused** (not owned by root, writable by others, a link, an unknown key, a value the daemon's loader refuses) | The same retry; the reason names the file and the key; nothing is fetched | the log |
| **Repository unreachable, descriptor or pointer wrong** | The same retry; the reason names the URL and the status | the log; the host never has hostkeeper |
| **Pkg refused** (size or digest differs from the pointer, `pkgutil` fails, the status is not Apple's, the Team ID is not the pinned one) | The file is deleted before anything is written or run: no config, no `installer`. The attempt is retried after `retry_secs`, since the pointer may move to a good file | the log, naming the Team ID found and the one pinned |
| **The update account's keys are partial or a value breaks its rule** | Settings refused, the same retry; the reason names the file, the key and the rule, never the value | the log |
| **An update-account file is there with another owner or mode, or something not a file is at its path** | The attempt fails before `installer`, the file is left, and the configs this attempt made are removed; the same retry | the log, naming the uid and mode found |
| **`installer` fails** | The staged file is removed, and so are the config files and the update-account file this attempt made (a file the operator had stays), so a corrected setting is read on the next attempt; that attempt installs again | the log, with `installer`'s last line |
| **Coordinator unreachable at first boot** (down, wrong LAN, CA hash mismatch) | Not the bootstrap's: the daemon's CA check fails first, so nothing is sent and no key is made on a hash mismatch (`UntrustedCoordinator`); a connection failure is a failed step, retried every poll period | the daemon's log; no request in the queue |
| **Approval never given** | The request waits and expires after `--enrollment-request-ttl` (72 h). The daemon posts again with the same key and the request is listed again | the request in the Enrollment page; after the TTL, under `unenrolled` |
| **The Mac is re-imaged** | The Blueprint installs the bootstrap again; the files of the old install are gone, so it installs hostkeeper again, and the daemon makes a new key and a new request. The host id is held by the old key, so the record does not approve it (the record is used, and "no other key holds the host id" fails): an operator approves with `--replace true` in the same group. A different group is refused `host_group_differs` | one click; the old key's certificate stops being admitted |
| **The bootstrap runs on a Mac that has hostkeeper** | Step 1 of section 9: it exits 0 | none |

## 12. Tasks, one pull request each

Size: S under a day, M a few days. All of them land in the pull request that carries this design (#388), by the owner's decision of 2026-11-14.

| # | Task | Size | State |
|---|---|---|---|
| MB1 | Crate `hostkeeper-bootstrap`: the loop of section 9 over its ports (the network, `pkgutil` and `installer`, the pause), the settings, the fetch, the check and the install; `main.rs` only forwards to the library (T4). The farm Mac measures the terms of section 9 | M | **Built**: `crates/hostkeeper-bootstrap`. The measurement on the farm Mac is not done |
| MB2 | `cargo xtask release macos-bootstrap-pkg` and its `check`: one component, the binary, the plist, a postinstall that loads the job, identifier `dev.hostkeeper.bootstrap` | S | **Built**: `crates/xtask/src/release/macos_bootstrap.rs`, `packaging/macos-bootstrap/` |
| MB3 | `artifacts.yml`: the pkg joins `build-macos`, `sign-pkg` and `check-pkg`, is attested, and the release publishes it beside the hostkeeper pkg; the workflow lint gains the job lines with a negative case (T15) | S | **Built**: `.github/workflows/artifacts.yml`, `crates/xtask/tests/workflows/pkg_signing.rs`, `crates/xtask/src/release/contract.rs`, `crates/xtask/src/channels/spec.rs` |
| MB4 | Notarization of the bootstrap alone, only if question 2 says so | M | **Deferred** (Q2 default: no notarization work now) |
| MB5 | `docs/operations/mac-bootstrap.md`: the owner's Blueprint steps, the record, the approval, the logs; `docs/macos.md` updated | S | **Built**: `docs/operations/mac-bootstrap.md` |
| MB6 | Coordinator: approve a re-imaged Mac's request by the serial of an inventory record, additive | S | **Not planned** (Q6 default: an operator's click with `--replace true`) |

## 13. The nine questions, as settled on 2026-11-14

The owner gave the green light to implement and asked for these defaults. Each is **default, owner may change**; what stays open is named.

1. **How do the two farm values reach a pkg that is the same for everyone?** (default) The managed preference domain `dev.hostkeeper.config` (a Blueprint configuration), with the
   fallback file `/Library/Application Support/hostkeeper/bootstrap.toml` for a manual install (section 4). Nothing lab-specific is committed. **Open:** whether Apple Business's built-in
   management can carry a custom preference payload.
2. **What does a Blueprint's package need and do?** (default) No notarization work now (MB4 deferred): the bootstrap pkg is signed with the existing Developer ID Installer identity in the release workflow, built and
   uploaded beside the hostkeeper pkg under a versioned URL, its SHA-256 in `SHA256SUMS` (section 8). **Open:** whether notarization is required, whether Apple reinstalls the pkg when the pin changes or
   the receipt is missing, and whether the URL must answer `Content-Length` and `Range`; the repository does not hold Apple's text.
3. **Is an inventory record or a click acceptable as the approval for every Mac?** (default) Yes: an operator's click in the UI, and an inventory record by serial works as today (section 5).
4. **Where does the package proxy serve Macs from once the MDM listener is idle?** (default) Out of scope now; the proxy was removed with the MDM listener (section 10).
5. **How does `hostkeeper.toml` or `macos-pkg.toml` change on an enrolled Mac?** (default) Out of scope now; noted as later work (section 6).
6. **Re-imaged Macs.** (default) An operator's click with `--replace true`; MB6 is not planned.
7. **Install first, or enroll first?** (default) Install hostkeeper first and let its daemon enroll, the existing manual path, with no certificate hand-over. The bootstrap reads the farm values, fetches, checks and installs the signed pkg, writes the configs when absent, and exits (sections 1, 6 and 9).
8. **The signer pin.** (default) The Team ID `U7RTX4XD3X`, as the default of the config key `team_id`, overridable; plus `pkgutil --check-signature`, plus the size and the SHA-256 of the repository's pointer (section 7).
9. **Remove the MDM code now or later?** Now: the owner decided on 2026-11-14 to delete it (section 10).
