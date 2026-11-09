# Packages from a package repository: the target names a package, a version and a repository

Status: design, for the maintainers' ruling. Build item A16 is **Built**: `Target.repository` and
`Target.package_version` (`crates/hostkeeper-proto/proto/hostkeeper/release/v1/release.proto`), the checks of section
2.2 and the installer set (`crates/hostkeeper-release/src/target.rs`, `installer.rs`, `grammar.rs`), and the
coordinator's audit of a repository entry and its release before's reader on what it stores
(`crates/hostkeeper-coordinator/src/releases.rs`). Build item A7 is **Built**: the repository installer
kind, its backend interface and the `apt` backend (`crates/hostkeeper-install/src/repository/`: the configuration of
section 3.1, the announce rule of section 4.1, `stage`, `activate` with the hold and the enablement, rollback from the
kept index and package, `retire`, `remove` by purge, drift and the failure reasons of section 4.5, with the manifest's hook
program checked at `stage` and the `policy-rc.d` guard around dpkg of packages-native.md section 10.2); the tool runs that fetch,
confined (`StepLauncher::run_tool`); the daemon's check of the configuration at start and its answers
`order_invalid` and `coordinator_too_old` to an entry of the wrong kind; and `ServiceBeat.drift`, sent over gRPC and
shown by `Hosts.List` and `Hosts.Get`. The daemon runs it (`crates/hostkeeper-daemon/src/repository.rs`): a host
announces `apt` as section 4.1 says, an order of an `apt` entry is staged into a sealed version directory with its
`target.json` (section 4.2; `target.rs`), activated before `cutover`, rolled back from the kept previous version,
purged at uninstall and read for drift at each heartbeat, each failure the order's verdict with the class and reason
of section 4.5; a package without a hook runs the built-in steps (`builtin.rs`). Build item A17 is
**Built**: `hostkeeper-pxe serve --repositories` checked by the loader's `load_as` (`crates/hostkeeper-pxe/src/serve.rs`,
`crates/hostkeeper-pxe-seed/src/seed.rs`), the enrollment runbook's step, the MDM profile's two domains and their
re-send (`crates/hostkeeper-mdm-gate/src/profile.rs`, `enroll.rs`), the daemon's rendering of them
(`crates/hostkeeper-daemon/src/mac_config.rs`), and the rotation runbook
([repository-keys.md](../operations/repository-keys.md)). Nothing else here is built. This amends [packages.md](packages.md),
[packages-lifecycle.md](packages-lifecycle.md) and [packages-tests.md](packages-tests.md), as the maintainers ruled:
hostkeeper installs a Linux service's package from a package repository with the host's package manager
(`apt-get install tierstore=<V>` from the suite the host is configured with). It does not fetch a `.deb` and install
that file. The repository's signing key and the required pin are the trust. A Mac has no such repository: it installs
a signed `.pkg` with `installer`, fetched by digest as an artifact (section 4.7). Where those three files disagree with
this one about a service's package from a repository, or a Mac's `.pkg`, this one wins. Section 0 lists what it
replaces. Section numbers like "packages.md
section 5.2" are that file's; a bare "section 3" is this file's.

**Revised by section 10 ([packages-native.md](packages-native.md)).** hostkeeper's own package format ends:
`hostkeeper-archive` goes after the farm has moved, `tarball` and `oci-image` are withdrawn, an `apk` backend serves
the Alpine daemon, and hostkeeper itself is installed and updated from its `.deb` and `.pkg` through `apt` and
`macos-pkg`. Wherever this file sends a host or a service to the tarball (an RPM-family host before `dnf`, a service
whose hand-over needs the old files), sections 10.1 and 10.6 say what holds instead. Four more things below change. A
`macos-pkg` entry carries no attestation, and the host checks none: wherever this file names the attestation of a
`.pkg` (sections 1, 3.3, 4.7, 9), the pin and the signature are the whole check, and the publish job checks the
attestation before it signs (10.3); `[trusted_signers]` then guards no install, only `hostkeeper-pxe`'s check. Any package may have maintainer scripts (10.2): on `apt` a `policy-rc.d` that exits 101
is written around the dpkg runs, so a script that starts through `invoke-rc.d` or `deb-systemd-invoke` starts nothing before the hand-over. A package's hook is the manifest's
`hook.command`, an absolute path of a file of the package and leading arguments (10.2), so the caller of `SetDesired`
chooses the program among the package's files and its arguments (section 3.3). And hostkeeper's own release is a native entry per platform (10.4),
so where this file says it stays `hostkeeper-archive`, that holds only until the move of 10.6.

The release machinery that every repository of the project shares is moving to this flow: publish each release to
the **testing** channel of the project's package repository, check that it installs on clean machines, then promote
the same bytes to the **stable** channel. Those two channels are the repositories this design names. GitHub (each
service's CI) decides which version each host should run, by `SetDesired`; hostkeeper installs exactly that version
from the channel the order names, and nothing installs a managed package by hand. tierstore goes first; its farm
stage (komira-ai/tierstore#255) moves to the testing channel. So hostkeeper's order for such a service says "install
version V of package P, whose SHA-256 is D, from repository R". Nothing below is specific to tierstore.

Scope: apt on Linux, and the `macos-pkg` installer on macOS, both designed in full: the farm's Macs install services
from signed `.pkg` files. A Homebrew tap stays the route for people installing a service on their own Mac; hostkeeper
never uses it. Every format the project publishes gets the same two channels. On Linux the installer is one
**repository installer** with a backend per package manager, so the next manager is a backend, not a new installer
design (section 6). dnf is the next one and gets its full design when the first RPM host exists.

## 0. What this replaces

| Where | Before | After |
|---|---|---|
| packages.md 3.1, 3.4, 9.4: the target | Every entry is an artifact: `locations`, `digest`, `bytes`, `attestation`, `manifest` | Also a **repository entry**: `repository` and `package_version` set, a required `digest` pin, no other artifact field (section 2) |
| packages.md 5.2, 5.4: `deb`, `rpm`, `arch`, `apk` | Installers that fetch a package file by digest, check its attestation and install the file with the format's tool | Withdrawn. `apt` (A7) installs from an apt repository; `dnf` (A8) is later (section 6); `arch` and `apk` are withdrawn with no successor |
| packages.md 5.2: `macos-pkg`, and this file's first section 4.7, a Homebrew backend | `pkgutil --expand-full`, `installer -pkg`, a package found on the host refused (`package_conflict`) | `macos-pkg` (A9): a signed `.pkg` fetched by digest as an artifact, its signature checked against configured signers, drift corrected, rollback from the kept `.pkg`, removal by the receipt (section 4.7) |
| packages.md 5.2, 5.3: "the daemon never runs `apt`"; "no installer fetches" | Every tool run has `PrivateNetwork=yes` | A repository installer fetches through the package manager, from the one repository the order names, in `stage` only: two commands that run no package script, and every other tool run keeps `PrivateNetwork=yes` (section 4.6). `macos-pkg` fetches nothing itself; the daemon's own fetch brings its `.pkg` (section 4.7) |
| packages.md 7.4, 8: trust for a service's package | `[trusted_signers]` and the artifact's attestation | The repository's key, part of hostkeeper's host configuration placed at host setup, and the required digest pin. No attestation is checked or accepted for a repository entry (section 3). A Mac's `.pkg` keeps the digest and attestation of every artifact and adds its signature, checked against the host's configured signers (section 4.7) |
| packages-lifecycle.md 6.1, 6.4 to 6.6 for `deb` and `rpm` | The kept artifact; rollback installs it; `dpkg --purge`; a package installed by hand refused (`package_not_ours`) | The kept index and package apt fetched; rollback is a downgrade with apt; the hold; `dpkg --purge`; a package at a version hostkeeper did not set is drift, repaired and reported (section 4.4) |
| packages-tests.md: `deb`, `rpm`, `arch`, `macos-pkg` cases | Device tests that install a fixture file | The cases of A16 and A7 (packages-tests.md), with a fixture repository on the runner's disk, and A9's, with signed fixture `.pkg` files |

Everything else in packages.md stays: the platform map, the pick by installer name (packages.md section 3.3), the
manifest outside the package, built-in steps, `hostkeeper-archive`, `tarball` and `oci-image`, stores, the staged
path, and hostkeeper's own release, which stays `hostkeeper-archive` on every platform until section 10 retires
the archive and the `tarball` and `oci-image` installers.

## 1. What changes for the operator and for a service's CI

**Before** (packages.md, as merged). A Debian host of a service that ships a `.deb` is ordered the file: the daemon
fetches it from the stores the entry names, checks its size, digest and Sigstore attestation, and installs it with
`dpkg --install`.

**After.**

- The release machinery publishes the `.deb` to a suite of an apt repository, as it does for every other consumer of
  the repository. CI then calls `SetDesired` with, for each Linux platform, an entry that names the package, its
  version, the channel, and the SHA-256 of the `.deb`, which every repository entry must carry:

  ```json
  "linux-x86_64": { "entries": [
    { "installer": "apt", "repository": "komira-testing", "package_version": "1.6.0-1",
      "digest": "<64 hex>", "manifest": { "package": "tierstore", "run": ["tierstore.service"], ... } },
    { "installer": "tarball", "locations": [...], "digest": "<64 hex>", "bytes": "...",
      "attestation": {...}, "manifest": {...} } ] },
  "macos-arm64": { "entries": [
    { "installer": "macos-pkg", "locations": [ { "store": "komira-testing", "locator": "tierstore/1.6.0/tierstore-1.6.0.pkg" } ],
      "digest": "<64 hex of the .pkg>", "bytes": "...", "attestation": {...},
      "manifest": { "package": "dev.tierstore.server", "run": ["dev.tierstore.server"], ... } } ] }
  ```

- `komira-testing` is not a URL. It names an entry of the host's own repository configuration, which host setup put
  there with the repository's signing key, as part of hostkeeper's own configuration (section 3). The coordinator
  never sends a URL, a suite or a key. On the Mac, `komira-testing` names a store of the host's `stores.toml`, the
  channel's location for `.pkg` files, placed the same way.
- A Debian or Ubuntu host with that entry runs, in effect, `apt-get install tierstore=1.6.0-1` against that
  repository alone, refuses it unless its SHA-256 is the order's, holds the package (`apt-mark hold`), and starts the
  service's units itself. A package found at any other version is put back to the desired one and reported as drift.
- A Mac fetches the `.pkg` from that store, refuses it unless its SHA-256 is the order's, its attestation checks out
  and it is signed by an identity the Mac's configuration names, and runs `installer -pkg` of it as root (section 4.7).
- A Linux host without a repository configured does not announce `apt`, and a Mac without signers configured does not
  announce `macos-pkg`; each takes the next entry it can install (here the tarball on Linux), or shows
  `no_build_for_host`, as today.

**What does not change.** The pick of an entry by installer name; one installer per service on a host; the manifest
in the call; the hook contract and built-in steps; health observation and the rollout gate; `Force`; hostkeeper's
own release; the old daemons, which are never ordered anything but archives.

## 2. The target of a repository installer

### 2.1 The message

```proto
// release.proto, additive (A16)
message Target {
  // 1 to 6 as today
  optional string repository = 7;       // set: a repository entry. The name of an entry of the host's
                                        // repository configuration: 1 to 32 bytes of [a-z0-9-]
  optional string package_version = 8;  // with repository only: the version as the package manager
                                        // spells it, epoch and revision included ("1:1.6.0-1")
}
```

The package's name is the manifest's existing `package` field, "the identity the format's own tool knows the
package by", which is exactly what it was designed for. A second field on `Target` would be a second way to say the
same thing. The version is `package_version`, not `version`: a package manager's version carries a revision and may
carry an epoch, so it is often not the release's `desired.version` (`1.6.0` ships as `1.6.0-1`), and one word for both
would invite reading one as the other.

An entry is a **repository entry** when `repository` is set, and an **artifact entry** (today's) when it is not. The
coordinator tells them apart by that field alone and never by the installer's name, so a new repository installer
(dnf, later) still needs no coordinator change: the pick stays the equality of `installer` strings
(packages.md section 3.3).

### 2.2 What the coordinator checks and stores

A repository entry is checked by A16's rules in the shared crate `hostkeeper-release`, beside today's:

| Check, repository entry | Refusal |
|---|---|
| `repository` 1 to 32 bytes of `[a-z0-9-]`; `package_version` present, 1 to 128 bytes of `[A-Za-z0-9.+~:_-]`, its first byte a digit | `TARGET_INVALID` |
| `package_version` present on an entry without `repository` | `TARGET_INVALID` |
| No `locations`, `bytes` 0, no `attestation`: a repository entry names no artifact, and an attestation nobody checks must not look checked | `TARGET_INVALID` |
| `digest` absent, or not 64 lower-case hex (the SHA-256 of the package file, required from every caller) | `TARGET_INVALID` |
| `manifest.package` present: 1 to 128 bytes of `[A-Za-z0-9+._@-]`, its first byte a letter or digit | `TARGET_INVALID` |
| The manifest's installer-free checks (packages.md section 4.4), as for any entry | `TARGET_INVALID` |
| The service `hostkeeper` with a repository entry (only one `hostkeeper-archive` entry is allowed, as today) | `TARGET_INVALID` |

A repository entry whose manifest has no `package` is refused there too. The grammars are the union of the
managers' (dpkg's and rpm's names and versions), so a later backend needs no coordinator change. An artifact entry is checked exactly as today.
A16 owns these checks, the two fields' comments and the header of `release.proto`, and the crate's installer set
(`Installer`, `names_a_package`, `is_repository`); A7 owns the daemon's rules that depend on the installer: `apt`'s grammars for
`package` and `package_version` and the manifest rules of section 3.4.

The coordinator stores the entry as it was sent, as for every entry: in `<state-file>.releases` or the `releases`
table's `targets` column, as proto3 JSON. A release stays immutable (packages.md section 3.4): the same version set
again with another repository, package version or pin is `RELEASE_EXISTS`. The audit entry of the set lists, for a
repository entry, its installer, the repository, the package, the package version, the pin and the
SHA-256 of the manifest's encoding. `GetRelease` answers the entry as stored; a LAN mirror's sync job has nothing to
pull for it.

### 2.3 Old coordinators and old daemons

- **A coordinator before A16** (A2 and A3 are built) refuses a set with a repository entry, loudly: over gRPC it skips the
  unknown fields 7 and 8 and the entry has no locations (`TARGET_INVALID`); on the gateway the unknown key is refused
  (`invalid_request`). Nothing is set, and CI's call fails.
- **A coordinator rolled back below A16 after such a release was set** keeps the stored JSON as it read it (it reads
  without refusing unknown fields and writes back what it read), but its own copy of the entry lacks the two fields.
  It could then order an `apt` entry with no `repository`. A daemon given a repository installer's entry with no
  `repository` answers `Deferred`, `environment`, `coordinator_too_old`, and does nothing: only a coordinator that
  dropped the field can send that, since every coordinator from A16 refuses such a set.
- **A daemon before `target-v1`** is never sent a target (packages.md section 9.1).
- **A `target-v1` daemon without `apt`** does not list `apt` in `HeartbeatRequest.installers`, so the coordinator never
  picks the entry for it. It takes the next entry it lists, or shows `no_build_for_host`. A host that runs the service
  from a tarball keeps the tarball entry, wherever the `apt` entry stands (packages.md section 3.3). If such a daemon
  were sent an `apt` entry anyway, it answers `installer_not_listed`, as for any installer it does not list.
- **A daemon with `apt` or `macos-pkg`** given an artifact installer's entry with `repository` or `package_version`
  set, or a repository entry with artifact fields, answers `Failed`, `release`, `order_invalid`.

### 2.4 The digest pin

`digest` is **required** on every repository entry, from every caller: CI always knows the `.deb`'s SHA-256, because it built and published the file, and an operator who sets a version by hand copies it from the release. The daemon
checks, before it downloads anything, that the `SHA256` field of the package's stanza in the repository's signed index
equals it, and refuses the order otherwise (`Failed`, `release`, `package_digest_mismatch`). apt then checks the file
it downloads against the same index, and the daemon hashes the downloaded file itself and compares it with the pin.

The order's `package_digest` for a repository entry is the pin (the coordinator fills it from the entry, A3), so
packages.md section 8.1's rule that the entry's `digest` equals `package_digest` holds unchanged.

With the pin, an order names exact bytes, as every artifact order does. A package re-uploaded under the same version
with other bytes, by mistake or by whoever holds the repository's key, is refused on every host instead of being
installed on the hosts that stage after the upload and not on the others. Promotion from testing to stable copies the
same bytes, so a version's pin is the same in both channels.

## 3. Trust: the host's repository configuration

### 3.1 The entries

```
/etc/hostkeeper/repositories/            root:root 0755
  komira-testing.sources                 root:root 0644, one deb822 stanza
  komira-stable.sources
  komira.gpg                             root:root 0644, the keyring the stanzas' Signed-By names
```

```
# komira-testing.sources
Types: deb
URIs: https://packages.example.com/apt
Suites: testing
Components: main
Signed-By: /etc/hostkeeper/repositories/komira.gpg
```

The entry's name is the file's stem. The directory holds `<name>.sources` files and the keyrings they name, nothing
else; a Mac has no repository entries and does not read it (section 4.7). A Linux host's daemon reads it at start and refuses to start (as for a bad `stores.toml`, rule S2: configuration in
files) on any other file, a `.sources` file whose stem is not of the grammar of section 2.2, a file or keyring not
root-owned or writable by group or others, or a stanza that is not exactly one
stanza with `Types: deb`, one URI of `https://` or `file:/` (a repository on the host's own disk, as the device tests
and an install image use), one suite, at least one component, and `Signed-By:` an absolute path to a keyring in that
directory; the keyring is read, at most 1 MiB, and must be what apt reads at its name: armored when it is named `.asc`,
a binary OpenPGP keyring otherwise (an empty file, or armor in a `.gpg`, is refused). `Trusted:`, `Allow-Insecure:`, `Allow-Weak:`, `Allow-Downgrade-To-Insecure:` and `Check-Valid-Until: no`
are refused: an entry that turns off apt's signature checks is never used. A daemon that refuses to start runs no
self-update either, which is why host setup checks the directory with the daemon's own loader before it writes it
(section 3.2), as it does `hostkeeper.toml`: `hostkeeper check-repositories --dir DIR` judges a staged directory as
the daemon would take it here (a `Signed-By` of this directory names the file of `DIR`), exits 0 listing each entry,
and 1 with the loader's refusal (README, "Checking a repositories directory").

These files are hostkeeper's and not the system's: they are not in `/etc/apt/sources.list.d/`, so the host's own
`apt-get upgrade` and unattended upgrades never see the repository through them. An operator may list the same
repository for the system too (for a command-line tool); the hold of section 4.4 keeps such upgrades off the
packages hostkeeper manages.

The two entries a host has are the two channels, testing and stable, of the same repository and key. A repository
name means the same channel on every host: CI's order names it, and every host it reaches reads it from its own disk.
Names that say the origin and the channel (`komira-testing`, `komira-stable`) keep that plain. A host whose configuration lacks the named entry answers `Failed`, `environment`, `repository_unknown`: host
setup is wrong there, and the rollout gate stops on it.

### 3.2 Who places the key and who rotates it

The entries and the key are part of hostkeeper's own host configuration, beside `hostkeeper.toml` and `stores.toml`,
and they are placed where that configuration is placed: at host setup. The coordinator never sends them. On a host installed by network
(network-install.md), `hostkeeper-pxe` takes a directory of entries and keyrings with `--repositories <dir>`, checks
it with the daemon's own loader before it writes a single seed (as it does `hostkeeper.toml`), and writes it to
`/etc/hostkeeper/repositories/` through the same autoinstall user data. On a host set up by hand, the enrollment
runbook (docs/operations/enrollment.md) gains the step that copies them. On a Mac, the MDM kit that places
`hostkeeper.toml` (hostkeeper-mdm.md) places the channel stores of `stores.toml` and the signers of
`macos-pkg.toml` (section 4.7) too. That is A17.

**Rotation is host setup's too.** The repository's owner publishes the next key ahead of time and signs the suites
with both keys for an overlap. Host setup replaces the keyring with one that holds both keys, on every host, before
the repository stops signing with the old key, and with the new key alone after. apt reads the keyring at every
`apt-get update`, so a new keyring needs no daemon restart; a new or changed `.sources` file takes effect at the
daemon's next start. hostkeeper never changes a keyring, and no order can name one. A keyring package from the
repository itself (Debian's `debian-archive-keyring` pattern) is not used: a key that signs its own successor cannot
be taken back after it leaks.

### 3.3 What is trusted, and what is not used

This section is apt's; section 4.7 says what a Mac trusts instead. For an apt entry, the chain is apt's: the keyring on the host verifies the suite's `InRelease`, which lists the
SHA-256 of the `Packages` index, which lists the SHA-256 of the `.deb`. The pin ties that last digest to CI's.
hostkeeper checks nothing about who built the bytes beyond that.

- **`attestation` is not used** for a repository entry. The coordinator refuses one (section 2.2), so no one reads a
  release as checked against `[trusted_signers]` when it was not. `[trusted_signers]` keeps guarding every artifact
  entry and hostkeeper's own release.
- **Whoever holds the repository's signing key publishes the bytes** a host may install from it, and CI's pin decides
  which of them, so a host installs a package only when both agree. The key is the release machinery's, which
  publishes to the repository. hostkeeper's attestation said "this organisation's CI built these bytes"; the
  repository's signature says "the repository's owner published them", and the pin says "and CI asked for these".
- **The manifest is still not attested** (packages.md section 8.2), and still decides the hook command, its bounds and
  the `run` units. For a repository entry the hook program is a file of the signed package (section 4.3), so the
  caller of `SetDesired` chooses among the package's programs, as it chooses among a tarball's today.

| Guarantee (packages.md section 8.1) | `apt` entry | `macos-pkg` entry (section 4.7) |
|---|---|---|
| An order names exact bytes | The pin, checked against the signed index and the downloaded file | The entry's `digest`, checked against the fetched file, as for every artifact |
| Integrity before any byte is used | apt verifies the suite's signature and every hash before it hands a file over; the daemon extracts nothing before that, and checks the pin | The core checks size, digest and attestation, then `pkgutil` the signature, before anything is expanded |
| Who decides the bytes | The holder of the repository's key and CI's pin, both; not `[trusted_signers]` | CI's pin, `[trusted_signers]` for the attestation, and the holder of a configured signing identity, all three |
| What runs as root | The package's maintainer scripts, from the signed package, in `activate` and `remove`, with a `policy-rc.d` that exits 101 in place so that none starts a service through `invoke-rc.d` or `deb-systemd-invoke`; the hook, the sealed copy in the tree of a file of the signed package | The package's scripts and payload, from the pinned and signed `.pkg`, in `activate`, as `installer` runs them; the hook, the sealed copy in the tree of a file of the payload |
| Removal | The package, purged, whatever version is installed (section 4.4) | Every file the receipt lists, then the receipt (`pkgutil --forget`) |

### 3.4 The manifest of a repository entry

The package carries its own units and maintainer scripts. The manifest keeps only what hostkeeper needs to own the
service, the hook included. A `macos-pkg` entry's manifest follows the same rules, with these differences: `package` is the component's
package identifier, `run` holds launchd labels whose plists the payload installs, and the hook is a file of the
payload (section 4.7):

| Field | Repository entry |
|---|---|
| `package` | Required: the package's name, checked by the daemon against the package manager's grammar and against the package's own control field |
| `hook` | Required. `command` empty: built-in steps. Otherwise `command[0]` is the absolute path of a file the package installs (an executable regular file of the extracted tree, links followed inside the tree), and `command[1..]` are leading arguments. The daemon runs the sealed copy `tree/<path>`. `boot` needs a command. The bounds as today |
| `run` | Required, 1 to 64: the units that hostkeeper starts, stops and observes. No rule on a unit's name or place: a unit whose file is in the tree and has `[Install]` targets gets those links, and any other is started by hostkeeper at `cutover` and at every daemon start |
| `health_url` | Optional, as today |
| `installer_bounds` | Required: `stage_secs` bounds the index fetch, the download and the checks; `activate_secs` the install; `remove_secs` the purge |
| `place` | Refused for `apt`, whose package places its own files |
| `containers` | Refused: the package runs no image |
| `params` | Allowed only with a hook program, which reads them; refused with built-in steps, which read none |
| `min_hostkeeper` | As today |

## 4. The repository installer, and its `apt` backend

One installer kind, `repository`, in `hostkeeper-install`, does the steps of this section for every package manager:
the version directory and its seal, the work directory that sees one repository, the pin, the two-phase fetch, the
simulation, the hold, rollback from the kept copy, purge, drift, the units and the failure classes. What differs per
manager is a **backend**: the commands that read the index, simulate, download, install, hold, query and purge, and
how their output maps onto section 4.5's reasons. `apt` (sections 4.1 to 4.6) is the backend designed here. A host
announces the backend's name (`apt`) as its installer, so the coordinator's pick by name (packages.md section 3.3) is
unchanged, and one host announces at most one backend: the one of its own package database (packages.md section 5.4).
A Mac's installer, `macos-pkg`, is not a backend of this kind but an artifact installer (section 4.7).

### 4.1 When a daemon announces it

A Linux daemon announces `apt` (packages.md section 5.4's table) when the host's family is `deb`; `/usr/bin/apt-get`,
`/usr/bin/apt-mark`, `/usr/bin/dpkg`, `/usr/bin/dpkg-deb` and `/usr/bin/dpkg-query` are present; the installed `apt` is
2.2 or later (Debian 11, Ubuntu 22.04 and later: the first with `apt-get update --error-on=any`, without which an
update that fails its signature check still exits 0); and at least one entry is in `/etc/hostkeeper/repositories/`
(every entry there passed section 3.1's checks, or the daemon did not start). A host set up without a repository therefore never announces `apt` and keeps taking
tarballs.

### 4.2 The version directory

```
/var/lib/hostkeeper/packages/<service>/<version>/    0555, sealed
  target.json            the order's entry, as canonical JSON
  apt/<repository>.sources   the host's entry as it was at stage
  apt/lists/             the suite's InRelease and Packages index, as apt fetched and verified them
  apt/archives/<file>.deb    the package apt downloaded and checked against that index
  tree/                  what dpkg-deb --extract made of it: the hook, the units, the files the observer hashes
```

The seal and `Staged::check` cover the whole directory (packages-lifecycle.md section 6.1). apt never writes into it.
Each apt run works in `/var/lib/hostkeeper/apt/<service>/`, which the daemon fills from the version directory before
the run (and which `stage` fills from the repository); apt's locks and partial files live there. Every run passes,
on its command line, which wins over `/etc/apt/apt.conf.d/`:

- `Dir::Etc::SourceList=/dev/null` and `Dir::Etc::SourceParts=` the work directory's copy of the one entry, so the
  repository the order names is the only one apt sees;
- `Dir::Etc::Trusted=/dev/null` and `Dir::Etc::TrustedParts=` an empty directory, so only the entry's `Signed-By`
  keyring can sign;
- `Dir::Etc::Preferences=/dev/null` and `Dir::Etc::PreferencesParts=` an empty directory, so no host pin chooses the
  version;
- `Dir::State::Lists` and `Dir::Cache::Archives` in the work directory, `Dir::Cache::pkgcache=` and
  `Dir::Cache::srcpkgcache=` empty, so the host's own lists and caches are neither read nor written;
- `APT::Get::AllowUnauthenticated=false`, `Acquire::AllowInsecureRepositories=false`,
  `Acquire::AllowDowngradeToInsecureRepositories=false`, `APT::Install-Recommends=false`,
  `APT::Install-Suggests=false`, and `DPkg::Lock::Timeout` within the step's bound.

The system's dpkg database (`/var/lib/dpkg`) is the one apt installs into, as it must be. The host's proxy settings in
`apt.conf.d` still apply.

### 4.3 The steps

`package` is the manifest's, `V` the entry's `package_version`, `A` the host's dpkg architecture.

| Step | Does | Network |
|---|---|---|
| `stage` | 1. `apt-get update --error-on=any` in the work directory. 2. Read the stanza of `package` at `V` for architecture `A` or `all` (`apt-cache show`); none: `not_in_repository`. Check its `Size` against `max_package_bytes` and its `SHA256` against the pin. 3. The dependency check, before anything is downloaded: `apt-get install --simulate -y --allow-downgrades --allow-change-held-packages package=V` must show that package and no other installed, upgraded, downgraded or removed. 4. `apt-get download package=V`, which fetches that one file, checks it against the index and takes no dpkg lock, into the work directory's archive directory under the name apt gives it; the daemon hashes it against the pin. 5. `dpkg-deb --field` of the file: `Package` and `Version` must equal `package` and `V`, `Architecture` `A` or `all`. 6. `dpkg-deb --extract` into `tree/` (no maintainer script runs), then the core's bounded walk (packages.md section 5.2) and the normalization of the copy's modes (setuid, setgid and group or other write bits stripped, a foreign owner changed to the tree directory's owner; dpkg installs from the kept `.deb`, not from this copy). 7. When the manifest names a hook program, it is an executable regular file of the tree, links followed inside the tree (else `manifest_invalid`). 8. Seal | Steps 1 and 4 only, which run no package script |
| `activate` | Under the host-wide installer lock (packages.md section 5.2): `apt-get install -y --no-download --allow-downgrades --allow-change-held-packages -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold package=V`, with `DEBIAN_FRONTEND=noninteractive`, between the writing and the removal of `/usr/sbin/policy-rc.d` (`exit 101`; section 5); then `dpkg-query` must show `package` at `V`, status `install ok installed`; `apt-mark hold package`; write the `[Install]` links of each `run` unit whose file is in the tree and take away the links of a unit this version no longer runs (section 5); `systemctl daemon-reload`; switch `current` | None (`PrivateNetwork=yes`) |
| `retire` | Delete a version directory that is neither `current` nor `previous`. Never runs apt or dpkg | None |
| `remove` | `apt-mark unhold package`; `dpkg --purge package` (each try between the writing and the removal of `policy-rc.d`), whatever version is installed (a version hostkeeper did not set is drift, reported, section 4.4), tried again within `remove_secs` while dpkg reports its lock held (dpkg has no lock timeout of its own); remove the enablement links hostkeeper recorded; `systemctl daemon-reload` | None |

Before `activate` changes anything, the daemon writes `installed` in `placed.json` with the service's installer
(`apt`), the package, the version it is installing and the repository, marked pending, as packages-lifecycle.md
section 6.5 records any native package; it marks it done once `dpkg-query` shows the version. A crash between the two
resumes by running `activate` again, which is idempotent: apt finds the version installed and does nothing but the hold.

`--allow-change-held-packages` is needed because hostkeeper's own hold is on the package; the hold is set again after
every install. The simulation of step 3 is the dependency check of packages.md section 5.4 for this installer: the
work directory sees only the one repository and the host's installed packages, so a dependency neither installed nor
in that repository is unmet, and one only in that repository would be installed beside the package, which hostkeeper
refuses because it would then hold a package it did not record. A package of the shared release machinery declares no
dependency but its C library baseline (packages.md section 5.4), so it passes on every release it supports.

### 4.4 Update, rollback, the hold, and uninstall

- **Update** to V: `stage` V, the hook's `prepare` from V's tree, `activate` V, then `cutover`, `verify`, `cleanup`
  (packages-lifecycle.md section 6.2). The installed package moves from the previous version to V in one apt run.
- **The hold.** `apt-mark hold` is set after every install and lifted only by `remove`. Unattended upgrades and an
  operator's `apt-get upgrade` skip a held package, so nothing but hostkeeper moves a package hostkeeper manages,
  even when the host's own sources list the same package. hostkeeper's own runs pass `--allow-change-held-packages`.
- **Rollback** to the previous version P, which is still on the host: `activate` P from P's sealed directory, with no
  `apt-get update` and no download: apt finds P in P's own copy of the index and P's `.deb` in P's archive copy, and
  installs it with `--allow-downgrades`, running the older package's scripts. The run passes
  `Acquire::Check-Valid-Until=false`, this run only: P's index was checked within its validity at P's stage, is
  sealed and re-hashed before the step, and a rollback must not depend on how long ago that was. The repository need
  not still list P, and the network need not be up. A `ForceRollback` and an automatic rollback take the same path. A first install
  that fails has no previous version: `remove` purges it.
- **A version no longer on the host** (a `Force` or `ForceRollback` to an older version than `previous`) is staged
  again from the repository, which must still list it: otherwise `not_in_repository`.
- **Uninstall** keeps today's order (packages-lifecycle.md section 6.5), with `remove` above. **Purge, not remove:**
  `dpkg --purge` also deletes the package's configuration files, so after an uninstall nothing the package listed
  stays, which is closed-world removal (packages.md section 2). A plain remove would leave configuration files no
  later install could be sure of. The service's data under `/var/lib/<service>` stays, as for every installer, unless
  the package's own purge script deletes it. `dpkg --purge` refuses a package another installed package depends on;
  that is `Failed`, `environment`, `dependency_changes`, and nothing is removed.
- **Drift: hostkeeper is the reconciler.** Nothing installs a managed package by hand. A package found at a version
  hostkeeper did not set (moved by hand, or installed outside hostkeeper before hostkeeper's first install) is
  **drift**, and is repaired, not refused. The observer reads the installed version with `dpkg-query` at every
  heartbeat; when it is not the version `placed.json` records, or the package is installed and no record names it,
  the heartbeat reports the service at the version found with a `drift` report (`ServiceBeat.drift = 14`: the
  version found and the version recorded, or none). The coordinator orders by what hosts report, so it orders the
  desired version again; `install` or `update` then stages it, installs it over what was found (a downgrade if need
  be, with `--allow-downgrades`), holds it, records it, and its verdict is `Succeeded` with the reason
  `drift_repaired` and the version found. It is a report, never a failure: it counts toward no rollback, and `Status`
  lists the host's drift until the next clean heartbeat. The only way to move a host off the desired version is a
  `Force` through the coordinator. Two services whose releases name the same package are still refused,
  `package_conflict` (packages-lifecycle.md section 6.5), since that is a release mistake and not drift.

### 4.5 How the daemon reports failure

Each failure is an outcome with a class and a reason, as for every installer. The daemon classifies by the step and
the state it checks, and reads apt's messages only where apt has no other signal.

| What happened | Step | Outcome |
|---|---|---|
| The order's `repository` is not an entry of this host | `stage` | `Failed`, `environment`, `repository_unknown` |
| `apt-get update` could not reach the repository | `stage` 1 | `Deferred`, `environment`, `fetch_failed`, as for an artifact no store answered |
| The suite's signature did not verify: no signature, an unknown, expired or revoked key, a bad signature, or a `Valid-Until` passed (apt's `NO_PUBKEY`, `EXPKEYSIG`, `REVKEYSIG`, `BADSIG`, "is not signed", "is no longer signed", "has expired" errors) | `stage` 1 | `Failed`, `environment`, `signature`. Nothing is downloaded |
| The index lists no `package` at `V` for the host's architecture | `stage` 2 | `Deferred`, `environment`, `not_in_repository` |
| The index's `SHA256` is not the pin | `stage` 2 | `Failed`, `release`, `package_digest_mismatch`. Nothing is downloaded |
| The index's `Size` is over `max_package_bytes` | `stage` 2 | `Failed`, `release`, `package_too_large` |
| The index's `SHA256` is the pin, but the downloaded file is not (apt's hash check, or the daemon's) | `stage` 4 | `Deferred`, `environment`, `store_digest_mismatch`, as for a mirror that gave other bytes: the repository or a cache in front of it served a stale file |
| The package's control fields are not `package`, `V`, and `A` or `all`; the manifest's hook program is not an executable regular file of the package | `stage` 5, 7 | `Failed`, `release`, `manifest_invalid` |
| A dependency is unmet | `stage` 3 | `Failed`, `environment`, `dependency_missing` (the reason of packages.md section 5.4) |
| The install would install, upgrade, downgrade or remove another package; a purge would leave a dependent package | `stage` 3, `remove` | `Failed`, `environment`, `dependency_changes` |
| The dpkg lock is held past `DPkg::Lock::Timeout`, or past `remove_secs` for the purge (an unattended upgrade is running) | `stage` 3, `activate`, `remove` | `Deferred`, `environment`, `package_manager_busy` |
| dpkg was interrupted before this run and needs `dpkg --configure -a`, which would touch other packages | any | `Failed`, `environment`, `dpkg_interrupted`. hostkeeper runs nothing host-wide |
| The install itself failed: a maintainer script exited non-zero, or `dpkg-query` does not show `V` installed afterwards | `activate` | `Failed`, `release`, `dpkg_error`, with apt's last error line. The update rolls back to the previous version (section 4.4), or a first install is purged |

`not_in_repository` defers rather than fails, because a suite seen through a cache may lag the publish by minutes, and
a failure of class `release` on a canary would mark the version bad. The deferral is shown by `Status` with its
reason, and nothing changes on the host. The release machinery sets the desired version only once the suite's signed
index lists it, so a version that is never there is CI's mistake, caught in CI. A7's device test shows each row
against the apt of the oldest and newest Debian and Ubuntu releases the project lists
(`crates/hostkeeper-privileged/linux-releases.toml`); where apt's message for a row differs between them, A7 names the
form each gives.

### 4.6 What the sandbox allows

Every apt, dpkg and dpkg-deb run is a transient unit, as every tool run is. The two fetch commands of `stage`
(`apt-get update` and `apt-get download`) are the only ones with the network: they run under
`ProtectSystem=strict` with `ReadWritePaths=` the work directory (neither takes the dpkg lock), they run no maintainer script, and the sources they
can reach are the one entry's URI. Every other run, the install and the purge included, has `PrivateNetwork=yes`, so
a maintainer script cannot fetch anything either. packages.md section 5.3's rule "fetch nothing, and let no tool
fetch" now reads: an installer fetches only in `stage`, only through its package manager, only from the repository the
order names, and never while a package script runs. On macOS, where a launcher cannot cut the network, `macos-pkg`'s
guarantee is section 4.7's instead: no tool hostkeeper runs fetches, the daemon's own fetch is the only one it makes,
and the package's scripts, which keep the Mac's network, come from the pinned, attested and signed package.

### 4.7 The Mac: the `macos-pkg` installer

On macOS, hostkeeper installs a service from a signed flat `.pkg` with `installer`, run as root by the daemon. A `.pkg`
has no repository and no signed index, so `macos-pkg` is not a backend of the repository installer: it is an
**artifact installer**, as `tarball` is. Its entry is an artifact entry (packages.md section 3.1), fetched and proved
exactly as today (packages.md section 7.4), and the order names the exact bytes by the entry's required `digest`. It
keeps the shape of the `macos-pkg` row packages.md section 5.2 had before this file (expand with `pkgutil`, install
with `installer -pkg ... -target /`, remove by the receipt) and changes four things: the package must carry a
signature of a configured identity; a package hostkeeper did not install, or at another version, is drift that
hostkeeper corrects, not `package_conflict`; a rollback installs the kept previous `.pkg`; and `remove` deletes every
file the receipt lists, changed or not.

**The entry.** CI sends, for `macos-arm64`:

```json
{ "installer": "macos-pkg",
  "locations": [ { "store": "komira-testing", "locator": "tierstore/1.6.0/tierstore-1.6.0.pkg" } ],
  "digest": "<64 hex of the .pkg>", "bytes": "...", "attestation": {...},
  "manifest": { "package": "dev.tierstore.server", "run": ["tierstore"], ... } }
```

- **The channel is a store.** The release machinery signs each release's `.pkg`, attests it, publishes it to the
  testing location of the project's package site, and promotes the same file to the stable location
  (in hostkeeper's release workflow the promotion copies nothing: `pkg/hostkeeper/<version>/` is the testing
  location, `pkg/hostkeeper/stable.json` names the version that is stable, and the file is the same one at the
  same path; docs/deploy/package-repositories.md; the signature waits for M11). A Mac's
  `stores.toml` (packages.md section 7.2) defines each location as an `https` store, under the same names as the apt
  entries, `komira-testing` and `komira-stable`, and the entry's location names the channel CI sets the release for.
  A Mac whose `stores.toml` lacks that store answers `Failed`, `environment`, `no_store` (packages.md section 7.4), as
  an apt host without the entry answers `repository_unknown`. A mirror store serves the file by digest, as for any
  artifact.
- **The version is the release's.** The entry has no `repository` and no `package_version` (a daemon given either on
  a `macos-pkg` entry answers `Failed`, `release`, `order_invalid`, section 2.3): the version is the order's
  `desired.version`, and the package's own `PackageInfo` `version` must equal it, so the receipt's version, the
  release's and the one hostkeeper records are one string. Testing and stable carry the same file, so the digest is
  the same from either.
- **The names.** `package` is the component's package identifier, the name `pkgutil` knows the receipt by, and may be
  reverse-DNS. Each `run` entry is a launchd label under packages.md section 4.4's rule, the service's name or a label
  that starts with it and a dot (`tierstore`, `tierstore.worker`), so the two differ as they do here.
- **No coordinator change.** `macos-pkg` is in A1's installer set already (`crates/hostkeeper-release/src/installer.rs`),
  with `package` required; A16 keeps it there.

**Trust.** Two checks, each before the next step reads a byte:

- **The digest and the attestation**, by the core (packages.md section 7.4), as for every artifact: the bytes are the
  ones CI asked for, and CI built them.
- **The signature.** `pkgutil --check-signature` of the verified file must exit 0 and report a signature whose
  certificate chain the Mac trusts and that `pkgutil` accepts (a signature with a secure timestamp stays good after its
  certificate expires), and the leaf certificate's SHA-256 fingerprint must be one of the host's configured signers.
  The daemon takes the fingerprint from the first certificate of the printed chain (the leaf), joining the lines after
  `SHA256 Fingerprint:` and dropping all white space and case. Otherwise the order fails `signature`. The chain is
  trusted by Apple's roots for a Developer ID Installer identity, or by the root the MDM's config profile installs for
  a farm identity (hostkeeper-mdm.md section 7.2); trust alone admits any developer's package, so the leaf pin says
  whose.

**Who signs.** The identity is hostkeeper's own pkg's, whichever one hostkeeper-mdm.md's M11 chooses, so the farm has
one Mac format and one signing setup. A service's `.pkg` is signed before its release is attested and published,
because the attested digest, CI's pin and both channels' file must be of the same signed bytes: a package signed per
farm after its release would have a digest CI never pinned. With Developer ID, the release machinery signs, as
hostkeeper-mdm.md section 7.3 already has the release workflow sign hostkeeper's own pkg. With the farm identity,
section 7.3's re-signing on the MDM host after the release stays the route for hostkeeper's own pkg, which MDM
delivers by its own allowlisted digest, and does not serve a service's package; the release machinery would then hold
the farm's installer key, which section 7.3 rules out today. So A9 waits on M11's outcome, and a farm-identity outcome
reopens section 7.3 for services' packages before A9 is built.

A `.pkg`'s `preinstall` and `postinstall` run as root, with the Mac's network, and are covered only by the pin and the
signature, as hostkeeper's own package's scripts are (10.4). Whoever holds a configured signer's key, with CI's pin, can
run code as root on every Mac that names it, so keep the signer set small.

The signers are host configuration, in `/etc/hostkeeper/macos-pkg.toml`, root:wheel 0644:

```toml
# SHA-256 of each leaf certificate a service's .pkg may be signed by, as pkgutil --check-signature prints it
signers = ["<64 hex>"]   # 1 to 4: two during a rotation's overlap
```

It is a file of its own and not a key of `hostkeeper.toml`, which refuses unknown keys and is what the previous
binary reads after a fallback (the reason `stores.toml` is one, packages.md section 7.2). The daemon reads it at start
and refuses to start (rule S2) on a file not root-owned or writable by group or others, an unknown key, no signer,
more than four, or a signer that is not 64 hex digits. Rotation is host setup's, as an apt key's is (section 3.2): the
new leaf is added on every Mac before the release machinery signs with it, and the old one is taken away after; a
changed file takes effect at the daemon's next start.

**How a managed Mac gets the two files.** The MDM kit's config profile (hostkeeper-mdm.md section 4) carries two more
managed-preference domains, `dev.hostkeeper.stores` and `dev.hostkeeper.macos-pkg`, whose keys are those of
`stores.toml` and `macos-pkg.toml`. The daemon's start renders each to its file by the steps of `mac_config::at_start` (the property
list as TOML, checked by the daemon's own loader before it is written) with one difference: the managed preferences
are these files' source, so a file that differs from them is replaced, and a rotation the MDM pushes takes effect at
the daemon's next start. The coordinator takes the two files as `--mdm-stores` and `--mdm-macos-pkg` and checks each
with the daemon's own loader at its start; a store naming a credential file is refused, since a profile carries no
secret. The gate keeps, with the config profile it sent a Mac, the SHA-256 of the profile's settings, and records it
as delivered only once the Mac acknowledges that profile. It sends the config profile again to an enrolled Mac on its
allowlist, approved or not, once whenever the settings it would send now differ from those delivered (a record with
none counts as different, so a Mac enrolled before the two domains gets them once); a send answered with an error is
tried again only after the gate's resend delay, never in a loop. The nonce and the pkg keep their rules.
`hostkeeper.toml` keeps its rule (written only when there is none), since it holds the trust root a reinstall must
not change, so a profile sent again never changes it. A Mac with neither domain keeps whatever files its operator
wrote. That is A17's Mac half.

**When a daemon announces it.** A macOS daemon announces `macos-pkg` when `/usr/sbin/installer` and `/usr/sbin/pkgutil`
are present and `/etc/hostkeeper/macos-pkg.toml` names at least one signer (every file there passed the checks above,
or the daemon did not start). A Mac without the file never announces it and takes the next entry it can install (a
tarball), or shows `no_build_for_host`.

**Where a payload may go.** packages.md section 4.4's refused prefixes include `/usr` and `/var/lib`, and macOS's own
`/usr/local` may belong to another account, so a service's payload holds exactly: files and directories under
`/Library/Application Support/<service>/`, and one plist `/Library/LaunchDaemons/<label>.plist` per `run` label, with
the directories above them (`Library`, `Library/Application Support`, `Library/LaunchDaemons`) as directory entries
only. Its data lives in a directory of the same tree that the payload does not list, so neither an update nor `remove`
touches it.

**The version directory** is packages-lifecycle.md section 6.1's: `target.json`, `artifact` (the signed `.pkg`, 0444,
kept for rollback), and `tree/`, the component's payload as `installer` places it, so `tree/Library/…` is `/Library/…`.
Beside them, `macos-pkg/PackageInfo` and `macos-pkg/Bom`, the component's two files as the expansion gave them, and
`macos-pkg/artifact.pkg`, a hard link of `artifact` under the name `installer -pkg` takes (it refuses a path that does
not end in `.pkg`). The
seal and `Staged::check` cover all of it.

**The service runs from the receipt's install location**, as an apt package runs from where dpkg put it, and not from
`current/tree/`. `installer` runs as root and installs the package's files as the package gives them, `stage`
admits only root-owned entries (the sealed copy's setuid, setgid and group or other write bits are stripped; the copy is
the reference, not what `installer` writes), and `activate` refuses a parent directory another account could write. The
package's own scripts and launchd plist name the install paths too. The installed files are what runs; `tree/` is their sealed reference, which the hook runs from
and the observer hashes against. The observer reads the first `run` label's program from the plist in `tree/` and
hashes the running process's executable against the same path in `tree/` (packages-lifecycle.md section 6.6), and
drift (below) hashes every installed file against it.

| Step | Does | Network |
|---|---|---|
| `stage` | 1. The core's fetch and proof: the `.pkg` and its bundle from the entry's stores, size, digest, attestation (packages.md section 7.4). 2. `pkgutil --check-signature` of the verified file: a trusted chain whose leaf is a configured signer (else `signature`). 3. `pkgutil --expand-full` into the staging directory, then the core's bounded walk and `MacVerify` (packages.md section 5.2). 4. The shape: a component package, or a product archive whose `Distribution` names one component (one distinct `pkg-ref` id, with one path), selected by default, and has no `script`, `installation-check` or `volume-check` element; the component's `PackageInfo` has `identifier` equal to `package`, `version` equal to the order's version, `install-location` `/`, and no `relocate` or `bundle` element, so `installer` writes exactly what the `Bom` lists. 5. The `Bom` (`lsbom`) and the expanded payload list the same paths with the same types, modes and owners; every entry is owned by root, of group wheel or admin, and is no device or fifo. 6. When the manifest names a hook program, it is an executable regular file of the payload, links followed inside the payload (else `manifest_invalid`). 7. The payload becomes `tree/`, its modes normalized as for `apt`; seal | Step 1 only: the daemon's own fetch. No tool run fetches |
| `activate` | Under the host-wide installer lock (packages.md section 5.2): each payload path's parent directories already on the host are root-owned and not writable by others, nor by a group but admin (else `unsafe_directory`); each payload file already on the host that a receipt of another identifier lists (`pkgutil --file-info-plist`) is `package_conflict`, and nothing is installed; a file there that no receipt lists is replaced and is the package's from then. Write `installed` in `placed.json`, marked pending: the installer, the identifier, the version, and `created_dirs`, the payload's directories not yet on the host, added to those already recorded. `installer -allowUntrusted -pkg <version dir>/macos-pkg/artifact.pkg -target /`; `pkgutil --pkg-info-plist <identifier>` must show the version, and every payload file at its install path must hash to its copy in `tree/`. Delete each file and link of the previous version's `Bom` that this version's does not list, then each recorded directory left empty, which leaves `created_dirs`. `launchctl enable system/<label>` for each `run` label, `launchctl disable` for a label the previous version ran and this one does not. Mark `installed` done; switch `current` | None needed. The package's scripts keep the Mac's network, which no launcher on macOS takes away |
| `retire` | Delete a version directory that is neither `current` nor `previous`. Never runs `installer` or `pkgutil` | None |
| `remove` | Under the installer lock: delete every file and link (no directory) that the receipt (`pkgutil --only-files --files <identifier>`, joined to the receipt's install prefix) or the `Bom` of `current` or `previous` lists, whatever its content now, but a path a receipt of another identifier also lists, and never through a link in a parent; then each directory of `created_dirs` left empty, deepest first, never one that was on the host before hostkeeper's first install; `pkgutil --forget <identifier>` | None |

`installer` passes `-allowUntrusted` because hostkeeper made the trust decision itself at `stage`, against its own
signers, and the bytes it installs are the sealed ones `Staged::check` re-hashes before the step: so a certificate
rotated out or an MDM root taken away since then cannot block an install hostkeeper already proved, a rollback above
all. A crash inside `activate` resumes by running it again, which is idempotent: `installer` installs the same bytes
again, and the deletions and the enablement find nothing left to do. The `activate` of a first install, with no
previous version, deletes nothing.

**Files the receipt does not list stay.** A macOS package has no removal script, so whatever its `preinstall` or
`postinstall` made beyond the payload (a cache, a log, the service's data) is left by `remove`, as packages-lifecycle.md
section 6.5 leaves the files a package's scripts make for every format. A service's package keeps its scripts' files
under its own directory of `/Library/Application Support/`. A file the receipt lists is deleted even when it changed
since the install, as `dpkg --purge` deletes a changed configuration file, so an uninstall leaves nothing the package
installed (closed-world removal, packages.md section 2).

**The launchd job is the package's plist, started by hostkeeper.** The package installs its plist in
`/Library/LaunchDaemons/`, as a `.deb` installs its unit file, so the job's text is part of the pinned and
signed bytes; `place` is refused for `macos-pkg`, and no rule applies to the plist's keys. `launchctl enable` at
`activate` writes the override that lets launchd load a `run` label, and hostkeeper bootstraps, starts and stops it in
built-in steps, in `force` and at its own start, and boots it out before `remove`. A label a version no longer runs is
disabled. launchd keeps an override after the plist is gone; one that names no plist loads nothing, so `remove` leaves
it. The package's scripts run during `activate`, before `cutover`, and nothing holds back a job they load (section 5).

**Update, rollback, the hold, removal.**

- **Update** to V: `stage` V, the hook's `prepare` from V's tree, `activate` V, then `cutover`, `verify`, `cleanup`
  (packages-lifecycle.md section 6.2). macOS has no hand-over of ports, so `cutover` restarts the job.
- **Rollback** to the previous version P: `activate` P from P's sealed directory: `installer` installs P's kept `.pkg`,
  which `Staged::check` re-hashes before the step, then the files of V's `Bom` that P's does not list are deleted, and
  the directories V created that are left empty. Nothing is fetched, so the stores and the network need not be up. The
  signature is not checked again: it was checked at P's stage, and a certificate that has expired or been rotated out
  since must not block a rollback. A `ForceRollback` and an automatic rollback take the same path. A first install
  that fails has no previous version: `remove` takes it off.
- **A version no longer on the host** (a `Force` to an older version than `previous`) is staged again from the
  entry's stores, and checked again in full, its signature included.
- **The hold.** A Mac has no tool that upgrades a `.pkg` on its own, so there is nothing to hold; anything else that
  installs over a managed package is drift.
- **Uninstall** keeps packages-lifecycle.md section 6.5's order with `remove` above. The service's data stays, as for
  every installer.

**Drift.** At every heartbeat the observer reads the receipt, `/var/db/receipts/<identifier>.plist`, with no tool run,
and the files of the recorded version's `Bom`: it compares each file's size, mode and modification time with what it
saw last, and hashes against `tree/` only a file whose values changed (and every file once after the daemon starts). A
receipt at a version other than the one `placed.json` records, a receipt where no record names the package (installed
by hand before hostkeeper's first install), a recorded package with no receipt (`pkgutil --forget` by hand), or a
payload file missing or with other bytes is drift, reported as for apt (`ServiceBeat.drift`, section 4.4). The next
`install` or `update` corrects it: it installs the desired version's sealed `.pkg` over what it found (a downgrade if
need be), deletes the files of the found version's receipt that the desired version does not list, records the
version, and its verdict is `Succeeded`, `drift_repaired`, with the version found.

**Failures.** Those of the core's fetch and proof (`no_store`, `fetch_failed`, `store_digest_mismatch`,
`package_digest_mismatch`, `not_attested`, packages.md section 7.4) are unchanged, and these are `macos-pkg`'s:

| What happened | Step | Outcome |
|---|---|---|
| The entry has `repository` or `package_version` set | before `stage` | `Failed`, `release`, `order_invalid` |
| The package is unsigned; `pkgutil` does not accept its chain; its leaf is not a configured signer | `stage` 2 | `Failed`, `environment`, `signature`. Nothing is expanded |
| The tree fails the walk or `MacVerify` | `stage` 3 | As packages.md section 5.2 |
| More than one component, or one not selected by default; a `Distribution` script or check; an `identifier` other than `package`; a `version` other than the order's; an `install-location` other than `/`, a `relocate` or a `bundle`; a `Bom` that differs from the payload, or an entry not owned by root, of another group than wheel or admin, a device or a fifo; a hook program that is not an executable regular file of the payload | `stage` 4 to 6 | `Failed`, `release`, `manifest_invalid` |
| Another service records the same identifier (packages-lifecycle.md section 6.5) | `stage` | `Failed`, `release`, `package_conflict` |
| A payload file on the host belongs to a receipt of another identifier | `activate` | `Failed`, `release`, `package_conflict`. Nothing is installed |
| A parent directory of a payload path is not root's, or another account can write it | `activate` | `Failed`, `environment`, `unsafe_directory`. Nothing is installed |
| Another install holds macOS's installer, in the form A9's device tests pin for each macOS release | `activate`, rollback | `Deferred`, `environment`, `package_manager_busy` |
| `installer` exits non-zero for any other reason, `pkgutil` does not show the version after it, or an installed file does not hash to `tree/` | `activate` | `Failed`, `release`, `installer_error`, with `installer`'s last error line. An update rolls back to P; a first install is removed |

A9's device tests run in the `macos-device` job, as root, against fixture packages signed by an identity the test
makes (a root and an installer leaf with the code-signing usage, the root trusted for package signing in the System
keychain for the run and taken out after), whose leaf the fixture's `macos-pkg.toml` names, served from an `https`
store on loopback whose certificate the test makes and trusts the same way. A second root the Mac does not trust signs
the untrusted fixture. They show the rows above where the real `installer` and `pkgutil` give them, that `activate` and
a rollback fetch nothing, and that a rollback still installs after the test's root is taken out of the trust settings;
where `pkgutil`'s or `installer`'s output differs between the oldest and newest macOS the project supports, A9 names
the form each gives.

## 5. Health and hand-over

**The units are hostkeeper's to run.** On Linux a package installs its units as it likes, enabled or not. Around
`apt-get install` and `dpkg --purge` the installer holds a `policy-rc.d` that exits 101 (written to
`/usr/sbin/policy-rc.d`, mode 0755, removed afterwards on success and on failure; a stale one is removed at daemon
start before any package-manager call; an operator's own is left untouched and then rules), which `invoke-rc.d` and
`deb-systemd-invoke` honour, so through them no script of the package, nor of a dependency apt installs in the same
run, starts or restarts a service (one that calls `systemctl` itself is not held back): the start is the hand-over's. A `postinst` may enable the units, and they stay enabled. hostkeeper
never refuses a `run` unit. Each `activate` writes the `WantedBy=` and `RequiredBy=` links of every `run` unit whose
file is in the tree and takes away the links of a unit the version no longer runs (recorded in `placed.json`, as
hostkeeper-design.md section 4.7 does for placed units), leaving any link already there, so those units start at host
boot. A `run` unit with no such link (no `[Install]` targets, or no unit file in the package) is started by hostkeeper
at `cutover` and at every daemon start, which starts such units of each `apt` service's `current` version. The daemon
starts, stops and restarts all of them in built-in steps, in `force` and at its own start; `remove` takes its links away.

**On a Mac** the package's scripts run when `installer` runs them, during `activate`, before `cutover`, and there is
no equivalent of `policy-rc.d`: a service whose `.pkg` scripts bootstrap its job starts it before `cutover`, so such a
service needs the built-in steps or a hook that tolerates it. hostkeeper enables the job at each `activate`
(`launchctl enable`), loads and starts it in built-in steps, in `force` and at its own start, and boots it out before
`remove` (section 4.7). macOS has no hand-over of ports (hostkeeper-design.md), so an update of a Mac service is a
restart. The observer hashes the running program against the same path in the sealed tree.

**The first deploy.** A host that has never run the service gets an `install` order, as for any installer: `stage`,
the hook's `prepare` if there is a hook, `activate` (install, hold, enable), then `cutover`, which starts the units, as
there is nothing to hand over from, then `verify` and `cleanup`. The lab's plan of a forced first deploy
(`Releases.Force`) reaches only hosts that already announce the service. So on fresh hosts the first deploy of a
repository service is the coordinator's `install` of the hosts the service is assigned to, and `Force` is what moves a
version off the plan afterwards: stop the `run` units, `stage` the target if it is not on the host, `activate` it (a
downgrade if need be), start the units, report, with no hook. A host where the package is already installed (by hand,
or by an earlier tool) reports it as drift, and the first `install` installs the desired version over it (section
4.4).

**Later hand-offs.** With built-in steps, `cutover` restarts the `run` units: a gap of one restart. With a hook, the
hook does the hand-over from the new version's tree, as for an archive: `prepare` runs before `activate`, and
`cutover` after it. tierstore's port-forward hand-over (host-updates.md section 4.1) works this way, with one
constraint every `apt` package has: apt installs one version at a time. During the hand-over the old process
runs from the old files' inodes, which dpkg has replaced on disk. A hand-over that needs the old version's files at
their paths (to re-execute, or to read its own data files) cannot be done from an `apt` package, and such a
service stays a tarball. The package must not restart its units in its scripts (debhelper's `--no-start
--no-restart-after-upgrade`), so the hook decides when the new version starts.

**Observation.** packages-lifecycle.md section 6.6 is kept for a native package: the observer reads the first `run`
unit's program from the unit text in `tree/`, and hashes the running process's executable against that file of the
sealed tree. A process left from the previous version runs the old inode, whose bytes differ, and shows
`version_mismatch`. For `apt` the observer also reads the installed version with `dpkg-query` (for `macos-pkg`, the
receipt, section 4.7); a version other than the one
recorded is drift (section 4.4). The health URL is probed as today.

## 6. The next backends

Every format the project publishes gets the same two channels, testing and stable, with the same bytes promoted, so
each later manager is a backend of section 4's installer, shaped like `apt`: an entry of its own kind under `/etc/hostkeeper/repositories/` with its key, a work directory that sees that repository alone, the pin against the
SHA-256 its signed metadata lists, a fetch in `stage` only, a kept index and package per version so a rollback needs
no network, the manager's hold, purge, and drift. **dnf** (Fedora, RHEL and its rebuilds, Amazon Linux) is the next:
a `.repo` entry with `gpgcheck=1`, `repo_gpgcheck=1` and its `gpgkey`, `dnf install package-V`, `dnf versionlock` as
the hold and `dnf downgrade` for a rollback. It gets its full design, in a pull request of its own, when the first RPM
host exists (A8).

## 7. Compatibility and the staged path

Every change is additive (rule B8):

- `release.proto`: `Target.repository = 7` and `Target.package_version = 8`, both optional. An older reader keeps them
  and writes them back (`additions_repository.rs`, A16, the B8 test in both directions for the wire: a reader built
  from the baseline keeps the new fields; the new reader takes an entry without them as an artifact entry).
- The coordinator's releases: the same JSON with two more keys, which a coordinator before A16 keeps on rewrite. A16's
  test starts the previous coordinator's reader on a releases file and a Postgres table holding a repository entry.
- The host: `/etc/hostkeeper/repositories/` and `/etc/hostkeeper/macos-pkg.toml` are new files an older daemon does
  not read; `placed.json`'s `installed` gains the installers `apt` and `macos-pkg`, the `repository` key and
  `created_dirs`, which an older binary keeps on rewrite (`crates/hostkeeper-placed/src/store.rs`); a version directory
  with `apt/` or `macos-pkg/` is one an older binary does not hold,
  except after a fallback, which packages.md section 9.1 covers: the earlier binary reports such a service as
  unreadable and leaves it, and a rollback of `hostkeeper` below A7 (A9 on a Mac) on a host that holds an `apt`
  (`macos-pkg`) version is `rollback_below_installer`.
- `release.proto` gains nothing for `macos-pkg`: its entry is an artifact entry, and the name is in A1's set already.
- `agent.proto`: `ServiceBeat.drift = 14`, an optional message (the version found, the version recorded), which an
  older coordinator skips; its test is in `additions_installers.rs` beside `ServiceBeat.installer` (A7).
- No refusal is relaxed or tightened for an artifact entry, and `deb`, `rpm`, `arch`, `apk` and `macos-pkg` were never
  listed by any daemon, so no host was ever ordered one, and nothing here changes what shipped. A set that
  names them is still accepted, as a set with any unknown installer name is, and no host is ever picked for it.

The stages of packages.md section 9.2 gain one line each: A16 is released with or after stage 1's coordinator and
before any daemon announces `apt`; A7 and A9 are in stage 4, as every installer is; A17 follows A7 and
A9. Stages 5 and 6 (A11, A12)
and their order are unchanged.

## 8. The build items, re-scoped

The channel's removal still holds for a service that comes from a repository: such a service never used the channel,
so it adds nothing to A11's or A12's conditions, and its hosts count toward A11's condition through `target-v1`
alone. A11 still follows stage 4, as packages.md section 9.2 orders it. hostkeeper's
own release stays `hostkeeper-archive` with its index, and the channel keeps hostkeeper's archives, until section 10:
its build items (10.8) re-scope A6, A7 and A9 again, add A18 to A28, and withdraw A10, A11 and A12.

| # | New scope, one line |
|---|---|
| A4 | Unchanged, and the `agent.proto` header sentence (packages.md section 7.1) also says an order may name a package, its version and a repository the host's configuration defines; A4 reads no repository configuration |
| A6 | Unchanged here; what a repository installer needs of A6's trait and loader, A7 adds. Re-scoped again by 10.8 (no `tarball`) |
| A7 | The repository installer kind with its backend interface, and the `apt` backend: the installer trait's `stage` from a repository entry, `VersionDir::read` of a directory with `apt/` and no `artifact`, the two fetch commands as the only runs without `PrivateNetwork=yes`; the repository configuration and its checks at start, the announce rule, `stage` (update, index read, pin, download, control fields, extract, walk, units, simulation), `activate` with the hold and enablement, rollback by downgrade from the kept index and package, `retire`, `remove` by purge, drift and `ServiceBeat.drift` shown by the coordinator, the failure reasons of section 4.5, `coordinator_too_old` for an entry with no repository; replaces `deb` |
| A8 | `dnf` backend (later): its own design pull request once an RPM host exists (section 6), then the backend; replaces `rpm`. Until then an RPM-family host takes the tarball |
| A9 | The `macos-pkg` installer (section 4.7): the signers file, `stage` with the signature check, `activate` with `installer`, rollback from the kept `.pkg`, removal by the receipt, drift; after A6, A7 and M11 (hostkeeper-mdm.md) |
| A10 | Unchanged here; withdrawn by 10.8 |
| A11 | Unchanged here; repository services never used the channel and add nothing to its conditions. Withdrawn by 10.8 |
| A12 | Unchanged here; withdrawn by 10.8 |
| A13 | Withdrawn, with `apk` |
| A14 | Withdrawn, with `arch` |
| A16 (new) | `Target.repository` and `Target.package_version` in `release.proto`; `hostkeeper-release`'s checks of section 2.2 and its installer set (`apt` beside `hostkeeper-archive`, `tarball`, `macos-pkg`, `oci-image`; `deb`, `rpm`, `arch`, `apk` dropped; a later backend adds its name); the coordinator's audit of a repository entry; after A2, before A7 |
| A17 (new) | Host setup places repository entries and keyrings: `hostkeeper-pxe --repositories <dir>` checked by the daemon's loader, the enrollment runbook's step, and a rotation runbook (section 3.2); on a Mac, the MDM kit's profile carries the channel stores and the signers as the domains `dev.hostkeeper.stores` and `dev.hostkeeper.macos-pkg`, which the daemon's start renders to `stores.toml` and `macos-pkg.toml` with its own loader (section 4.7), and the signer rotation runbook; after A7 and A9 |

A5 (CI's archive entries for hostkeeper and `hello`) and A15 (the static build) are unaffected. A service's release
workflow sends repository entries once A16 and A7 are released, and `macos-pkg` entries once A9 is; that is the shared release machinery's change, not
an item of this repository.

## 9. Decisions

| Question | Decision | Why |
|---|---|---|
| The target's form | `installer`, `repository`, `package_version`, a required `digest`, and the package's name in the existing `manifest.package` | `manifest.package` already means the name the package manager knows; a second field would be a second way to say it |
| `version` or `package_version` | `package_version` | A manager's version carries a revision and an epoch, so it is often not `desired.version`; one name for both invites confusion |
| How the coordinator tells a repository entry | By `repository` being set, never by the installer's name | A new repository manager then needs no coordinator change, as the pick needs none |
| The digest pin | Required on every repository entry, from every caller; checked against the signed index and the downloaded file | CI always knows the file's SHA-256; an order then names exact bytes, and a re-uploaded version is refused everywhere |
| Attestation | Refused on a repository entry | An attestation no one checks must not look checked |
| Where the repository configuration lives | `/etc/hostkeeper/repositories/`, not `/etc/apt/sources.list.d/` | hostkeeper's view is the one repository, and the system's upgrades never see it through hostkeeper's entries |
| An entry that weakens apt's checks | Refused at start | A repository entry is the trust root; a bad one is a host setup mistake to fix at the source (rule B20) |
| A host without the named repository | `Failed`, `environment`, `repository_unknown` | A setup mistake must stop the rollout, not retry forever |
| Announcing `apt` | Only with apt 2.2 or later and at least one valid entry | Older apt exits 0 on a failed signature; a host with no repository must take another entry |
| Other repositories at install | Invisible: apt sees the one entry, no other key, no host pin | The order names one repository; nothing else may choose the bytes |
| Dependencies | The simulation must change no other package | hostkeeper removes only what it recorded; a dependency installed beside the package would be neither recorded nor removed |
| Rollback | A downgrade from the previous version's kept index and package, no network | The repository may have dropped the version; a rollback must work with the repository down |
| Downloading then installing | apt's own two phases, download then `--no-download`, from the kept index | Network only where no package script runs, and a failure's class follows from the phase |
| `not_in_repository` | Deferred | A cached suite may lag the publish; a release failure on a canary would mark a good version bad |
| Signature failure | Failed, environment | Never install, and stop the rollout until host setup or the repository is fixed |
| Uninstall | `dpkg --purge` | Closed-world removal: the package's configuration files go too |
| The hold | `apt-mark hold` after every install, lifted only by `remove` | Unattended upgrades and an operator's upgrade never move a managed package |
| A package at a version hostkeeper did not set, or installed outside it | Drift: the desired version is installed over it, held and recorded; reported (`ServiceBeat.drift`, `drift_repaired`), never refused | hostkeeper is the reconciler and nothing installs by hand; only a `Force` moves a version off the plan |
| One installer per manager, or one kind | One `repository` installer kind with a backend per manager; the announced name is the backend's | The steps, checks and classes are the same for every manager; the pick by name stays unchanged |
| The repositories | Two channels, testing and stable, of one repository and key, as host configuration | CI decides the version per host; hostkeeper installs exactly that from the channel named |
| Who starts the units | hostkeeper: at host boot by the `WantedBy` links it writes for a unit whose file is in the tree, else directly at `cutover` and at daemon start; no script starts one meanwhile through `invoke-rc.d` or `deb-systemd-invoke` (`policy-rc.d`) | The package may enable its units however it likes; the hand-over needs the start to be hostkeeper's |
| The hook of a repository package | The manifest's `hook.command`: a file of the downloaded package, run as its sealed copy in the tree, or built-in steps | The program is part of the signed package, and the hook contract stays the same for every installer |
| Key placement and rotation | Host setup (`hostkeeper-pxe --repositories`, the runbook), with an overlap of two keys | The coordinator never holds the trust root; a self-renewing keyring package cannot be revoked after a leak |
| `deb`, `rpm`, `arch`, `apk` | Withdrawn | The ruling: no Linux package file is installed; no daemon ever listed them |
| The Mac installer | `macos-pkg`: a signed `.pkg` installed by `installer` as root. No Homebrew backend; a Homebrew tap stays a route for people installing a service on their own Mac, and hostkeeper never uses it | `installer` runs as root, which fits hostkeeper's one root daemon, where Homebrew refuses root; no package manager to install and keep current on each Mac; MDM already needs a signed `.pkg` for hostkeeper's own first install, so one Mac format and one signing setup |
| A Mac's entry | An artifact entry: the channel's store (`komira-testing`, `komira-stable`) as its location, the required `digest`, the attestation; the version is the release's, and the package's `PackageInfo` version must equal it | A `.pkg` has no repository or index to name a version in; an artifact entry already names exact bytes, needs no coordinator change, and the same file in both channels has the same digest |
| A Mac's trust | The digest and attestation of every artifact, and `pkgutil --check-signature`: a chain `pkgutil` accepts whose leaf is one of the host's configured signers (`macos-pkg.toml`) | Trust in the chain alone admits any developer's package; the leaf pin says whose, and the identity is hostkeeper's own pkg's |
| Who signs a service's `.pkg` | The release machinery, before the attestation and the publish to testing; A9 waits on M11's identity | CI's pin, the attestation and both channels must name the same signed bytes; a per-farm signature after release would not |
| A Mac's channel stores and signers | Files the daemon renders from two managed-preference domains of the MDM kit's profile, replaced when the profile changes | The MDM is a managed Mac's source of configuration, and a signer rotation must reach every Mac without a reinstall |
| Where a Mac service runs | From the receipt's install location, under `/Library/Application Support/<service>/`; `tree/` is the sealed reference the hook runs from and drift hashes against | `installer` installs root-owned files no other account can change, `activate` refuses a parent another account could write, and the package's scripts and plist name those paths |
| Who writes the launchd job | The package installs its plist; hostkeeper enables, loads, starts and stops the job; `place` is refused | The job's text is part of the pinned, signed bytes, and hostkeeper starts and stops the job at the hand-over |
| Rollback on a Mac | `installer -allowUntrusted` of the previous version's kept `.pkg`, with no fetch and no second signature check | A rollback must work with the stores down, and after a certificate expired or was rotated out; hostkeeper proved those bytes at their stage and re-hashes them |
| Removal on a Mac | Every file the receipt or a kept `Bom` lists, changed or not; the directories hostkeeper's installs created, when empty; then `pkgutil --forget`. Files the receipt does not list stay | Closed-world removal, as `dpkg --purge`; a directory that was on the host before is not the package's, and a `.pkg` has no removal script to say more |
| Drift on a Mac | The receipt's version and the payload's files, against the record and the sealed tree; corrected by installing the desired `.pkg` over it, `drift_repaired` | hostkeeper is the reconciler, as on Linux; nothing installs a managed package by hand |

## 10. The end state: native packages only

hostkeeper's own package format retired, hostkeeper included: what we run and the end state, where each piece of
today's archive goes, trust, hostkeeper's own update, the release channel's end, the move of the farm, what is
deleted, the build items A18 to A28 and the decisions are in [packages-native.md](packages-native.md), kept in a file
of its own so this one stays under 1000 lines (rule R12).
