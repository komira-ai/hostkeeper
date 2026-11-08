# Packages from a package repository: the target names a package, a version and a repository

Status: design, for the maintainers' ruling. Nothing here is built. This amends [packages.md](packages.md),
[packages-lifecycle.md](packages-lifecycle.md) and [packages-tests.md](packages-tests.md), as the maintainers ruled:
hostkeeper installs a service's package from a package repository with the host's package manager
(`apt-get install tierstore=<V>` from the suite the host is configured with; on a Mac, `brew install` from a tap). It
does not fetch a package file and install that file. The repository's signing key (for a tap, which has none, the
pin) is the trust. Where those three files disagree with this one about a service's package from a repository, this
one wins. Section 0 lists what it replaces. Section numbers like "packages.md
section 5.2" are that file's; a bare "section 3" is this file's.

The release machinery that every repository of the project shares is moving to this flow: publish each release to
the **testing** channel of the project's package repository, check that it installs on clean machines, then promote
the same bytes to the **stable** channel. Those two channels are the repositories this design names. GitHub (each
service's CI) decides which version each host should run, by `SetDesired`; hostkeeper installs exactly that version
from the channel the order names, and nothing installs a managed package by hand. tierstore goes first; its farm
stage (komira-ai/tierstore#255) moves to the testing channel. So hostkeeper's order for such a service says "install
version V of package P, whose SHA-256 is D, from repository R". Nothing below is specific to tierstore.

Scope: apt on Linux and Homebrew (`brew`) on macOS, both designed in full; the farm's Macs run services through
Homebrew. Every format the project publishes gets the same two channels, and the installer is one **repository
installer** with a backend per package manager, so the next manager is a backend, not a new installer design
(section 6). dnf is the next one and gets its full design when the first RPM host exists.

## 0. What this replaces

| Where | Before | After |
|---|---|---|
| packages.md 3.1, 3.4, 9.4: the target | Every entry is an artifact: `locations`, `digest`, `bytes`, `attestation`, `manifest` | Also a **repository entry**: `repository` and `package_version` set, a required `digest` pin, no other artifact field (section 2) |
| packages.md 5.2, 5.4: `deb`, `rpm`, `arch`, `apk`, `macos-pkg` | Installers that fetch a package file by digest, check its attestation and install the file with the format's tool | Withdrawn. `apt` (A7) installs from an apt repository and `brew` (A9) from a Homebrew tap (section 4.7); `dnf` (A8) is later (section 6); `arch` and `apk` are withdrawn with no successor |
| packages.md 5.2, 5.3: "the daemon never runs `apt`"; "no installer fetches" | Every tool run has `PrivateNetwork=yes` | A repository installer fetches through the package manager, from the one repository the order names, in `stage` only. On Linux that is two commands that run no package script, and every other tool run keeps `PrivateNetwork=yes` (section 4.6); on macOS the fetches run as the prefix's owner and root runs only the pinned, sealed bottle (section 4.7) |
| packages.md 7.4, 8: trust for a service's package | `[trusted_signers]` and the artifact's attestation | The repository's key, part of hostkeeper's host configuration placed at host setup, and the required digest pin; for a tap, the pin and a tap that only fast-forwards (section 4.7). No attestation is checked or accepted for a repository entry (section 3) |
| packages-lifecycle.md 6.1, 6.4 to 6.6 for `deb` and `rpm` | The kept artifact; rollback installs it; `dpkg --purge`; a package installed by hand refused (`package_not_ours`) | The kept index and package apt fetched; rollback is a downgrade with apt; the hold; `dpkg --purge`; a package at a version hostkeeper did not set is drift, repaired and reported (section 4.4) |
| packages-tests.md: `deb`, `rpm`, `arch`, `macos-pkg` cases | Device tests that install a fixture file | The cases of A16 and A7 (packages-tests.md), with a fixture repository on the runner's disk |

Everything else in packages.md stays: the platform map, the pick by installer name (packages.md section 3.3), the
manifest outside the package, built-in steps, `hostkeeper-archive`, `tarball` and `oci-image`, stores, the staged
path, and hostkeeper's own release, which stays `hostkeeper-archive` on every platform.

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
    { "installer": "brew", "repository": "komira-testing", "package_version": "1.6.0",
      "digest": "<64 hex of the bottle>", "manifest": { "package": "tierstore", "run": ["tierstore"], ... } } ] }
  ```

- `komira-testing` is not a URL. It names an entry of the host's own repository configuration, which host setup put
  there with the repository's signing key (a tap's entry has none), as part of hostkeeper's own configuration
  (section 3). The coordinator
  never sends a URL, a suite or a key.
- A Debian or Ubuntu host with that entry runs, in effect, `apt-get install tierstore=1.6.0-1` against that
  repository alone, refuses it unless its SHA-256 is the order's, holds the package (`apt-mark hold`), and starts the
  service's units itself. A package found at any other version is put back to the desired one and reported as drift.
- A Mac with that channel's tap configured runs `brew install` of `tierstore@1.6.0` from it as the Homebrew prefix's
  owner, refuses it unless the bottle's SHA-256 is the order's, pins it, and runs the service as root from its own
  sealed copy of the bottle (section 4.7).
- A host without a repository configured does not announce `apt` or `brew` and takes the next entry it can install (here
  the tarball), or shows `no_build_for_host`, as today.

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
managers' (dpkg's, rpm's and Homebrew's names and versions), so a later backend needs no coordinator change. An artifact entry is checked exactly as today.
A16 owns these checks, the two fields' comments and the header of `release.proto`, and the crate's installer set
(`Installer`, `names_a_package`, `is_repository`); A7 owns the daemon's rules that depend on the installer: `apt`'s grammars for
`package` and `package_version` and the manifest rules of section 3.4, and A9 owns `brew`'s (section 4.7).

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
- **A daemon with `apt` or `brew`** given an artifact installer's entry with `repository` set, or a repository entry with
  artifact fields, answers `Failed`, `release`, `order_invalid`.

### 2.4 The digest pin

`digest` is **required** on every repository entry, from every caller: CI always knows the `.deb`'s (or the
bottle's) SHA-256, because it built and published the file, and an operator who sets a version by hand copies it from the release. The daemon
checks, before it downloads anything, that the `SHA256` field of the package's stanza in the repository's signed index
equals it, and refuses the order otherwise (`Failed`, `release`, `package_digest_mismatch`). apt then checks the file
it downloads against the same index, and the daemon hashes the downloaded file itself and compares it with the pin.
For `brew` the pin is checked against the formula's bottle `sha256` instead (section 4.7).

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

The entry's name is the file's stem. On Linux the directory holds `<name>.sources` files and the keyrings they
name, nothing else; on macOS it holds `<name>.tap` files (section 4.7). A host's daemon reads it at start and refuses to start (as for a bad `stores.toml`, rule S2: configuration in
files) on any other file, a `.sources` file whose stem is not of the grammar of section 2.2, a file or keyring not
root-owned or writable by group or others, or a stanza that is not exactly one
stanza with `Types: deb`, one URI of `https://` or `file:/` (a repository on the host's own disk, as the device tests
and an install image use), one suite, at least one component, and `Signed-By:` an absolute path to a keyring in that
directory. `Trusted:`, `Allow-Insecure:`, `Allow-Weak:`, `Allow-Downgrade-To-Insecure:` and `Check-Valid-Until: no`
are refused: an entry that turns off apt's signature checks is never used. A daemon that refuses to start runs no
self-update either, which is why host setup checks the directory with the daemon's own loader before it writes it
(section 3.2), as it does `hostkeeper.toml`.

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
`hostkeeper.toml` (hostkeeper-mdm.md) places the `.tap` entries too. That is A17.

**Rotation is host setup's too.** The repository's owner publishes the next key ahead of time and signs the suites
with both keys for an overlap. Host setup replaces the keyring with one that holds both keys, on every host, before
the repository stops signing with the old key, and with the new key alone after. apt reads the keyring at every
`apt-get update`, so a new keyring needs no daemon restart; a new or changed `.sources` file takes effect at the
daemon's next start. hostkeeper never changes a keyring, and no order can name one. A keyring package from the
repository itself (Debian's `debian-archive-keyring` pattern) is not used: a key that signs its own successor cannot
be taken back after it leaks.

### 3.3 What is trusted, and what is not used

This section is apt's; a Homebrew tap has no signature, and section 4.7 says what a Mac trusts instead. For an apt
entry, the chain is apt's: the keyring on the host verifies the suite's `InRelease`, which lists the
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

| Guarantee (packages.md section 8.1) | `apt` entry | `brew` entry (section 4.7) |
|---|---|---|
| An order names exact bytes | The pin, checked against the signed index and the downloaded file | The pin, checked against the formula's bottle `sha256` and the daemon's own copy |
| Integrity before any byte is used | apt verifies the suite's signature and every hash before it hands a file over; the daemon extracts nothing before that, and checks the pin | The daemon extracts only its own copy of the bottle, after hashing it against the pin |
| Who decides the bytes | The holder of the repository's key and CI's pin, both; not `[trusted_signers]` | CI's pin, through a tap that only fast-forwards |
| What runs as root | The package's maintainer scripts, from the signed package, in `activate` and `remove`; the hook, a program of the signed package's tree | The sealed tree of the pinned bottle only (the program and the hook); the formula's Ruby runs as the owner, never as root |
| Removal | The package, purged, whatever version is installed (section 4.4) | Every version of the service's formula, uninstalled |

### 3.4 The manifest of a repository entry

The package carries its own units and maintainer scripts. The manifest keeps only what hostkeeper needs to own the
service:

| Field | Repository entry |
|---|---|
| `package` | Required: the package's name, checked by the daemon against the package manager's grammar and against the package's own control field (for `brew`, the formula's name and version as `brew info` gives them) |
| `hook` | Required. `command` empty: built-in steps. Otherwise a program of the package's extracted tree (`tree/usr/lib/<package>/…` for `apt`, a path of the keg such as `tree/libexec/…` for `brew`), under today's rules. The bounds as today |
| `run` | Required, 1 to 64: the units the package installs that hostkeeper starts, stops, enables and observes. For `apt` each must be a unit file of the package, under `lib/systemd/system/` or `usr/lib/systemd/system/` of its tree; for `brew` a launchd label whose plist the bottle carries |
| `health_url` | Optional, as today |
| `installer_bounds` | Required: `stage_secs` bounds the index fetch, the download and the checks; `activate_secs` the install; `remove_secs` the purge |
| `place` | Refused for `apt`, whose package places its own files. For `brew`, required with exactly one `unit` entry per `run` label (the plist in the bottle and its path under `/Library/LaunchDaemons/`) and no `file` entry |
| `containers` | Refused: the package runs no image |
| `params` | Allowed only with a hook program, which reads them; refused with built-in steps, which read none |
| `min_hostkeeper` | As today |

## 4. The repository installer, and its `apt` backend

One installer kind, `repository`, in `hostkeeper-install`, does the steps of this section for every package manager:
the version directory and its seal, the work directory that sees one repository, the pin, the two-phase fetch, the
simulation, the hold, rollback from the kept copy, purge, drift, the units and the failure classes. What differs per
manager is a **backend**: the commands that read the index, simulate, download, install, hold, query and purge, and
how their output maps onto section 4.5's reasons. `apt` (sections 4.1 to 4.6) and `brew` (section 4.7) are the two
backends designed here. A host announces the backend's name (`apt`, `brew`) as its installer, so the coordinator's pick by name (packages.md section 3.3) is
unchanged, and one host announces at most one backend: the one of its own package database (packages.md section 5.4).

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
| `stage` | 1. `apt-get update --error-on=any` in the work directory. 2. Read the stanza of `package` at `V` for architecture `A` or `all` (`apt-cache show`); none: `not_in_repository`. Check its `Size` against `max_package_bytes` and its `SHA256` against the pin. 3. The dependency check, before anything is downloaded: `apt-get install --simulate -y --allow-downgrades --allow-change-held-packages package=V` must show that package and no other installed, upgraded, downgraded or removed. 4. `apt-get download package=V`, which fetches that one file, checks it against the index and takes no dpkg lock, into the work directory's archive directory under the name apt gives it; the daemon hashes it against the pin. 5. `dpkg-deb --field` of the file: `Package` and `Version` must equal `package` and `V`, `Architecture` `A` or `all`. 6. `dpkg-deb --extract` into `tree/` (no maintainer script runs), then the core's bounded walk (packages.md section 5.2). 7. Every `run` unit is a unit file of the tree. 8. Seal | Steps 1 and 4 only, which run no package script |
| `activate` | Under the host-wide installer lock (packages.md section 5.2): `apt-get install -y --no-download --allow-downgrades --allow-change-held-packages -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold package=V`, with `DEBIAN_FRONTEND=noninteractive`; then `dpkg-query` must show `package` at `V`, status `install ok installed`; `apt-mark hold package`; `systemctl daemon-reload`; enable each `run` unit not yet enabled and take away the links of a unit this version no longer runs (section 5); switch `current` | None (`PrivateNetwork=yes`) |
| `retire` | Delete a version directory that is neither `current` nor `previous`. Never runs apt or dpkg | None |
| `remove` | `apt-mark unhold package`; `dpkg --purge package`, whatever version is installed (a version hostkeeper did not set is drift, reported, section 4.4), tried again within `remove_secs` while dpkg reports its lock held (dpkg has no lock timeout of its own); remove the enablement links hostkeeper recorded; `systemctl daemon-reload` | None |

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
| The package's control fields are not `package`, `V`, and `A` or `all`; a `run` unit is not a unit file of the package | `stage` 5, 7 | `Failed`, `release`, `manifest_invalid` |
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
order names, and never while a package script runs. On macOS, where a launcher cannot cut the network, the brew
backend's guarantee is section 4.7's instead: nothing it runs is root, and root runs only the pinned bottle's sealed
tree.

### 4.7 The `brew` backend (macOS)

The project's macOS route is Homebrew: the release machinery publishes each release's formula and bottle to a
testing tap, and promotes the same bottle to a stable tap. The farm's Macs run services this way. The backend follows
section 4's shape step for step; this section says where Homebrew differs.

**The entry.** A channel is a tap. Its entry is `/etc/hostkeeper/repositories/<name>.tap`, root:root 0644, under
section 3.1's rules for the directory (on macOS the directory holds `.tap` files, on Linux `.sources` files; any
other file refuses the start):

```toml
remote = "https://github.com/example/homebrew-testing"   # the tap's Git remote: https:// or, for tests, file:/
prefix = "/opt/homebrew"                                  # the Homebrew prefix on this Mac
owner  = "admin"                                          # the account that owns the prefix
```

The daemon refuses to start on an entry that does not parse, whose `remote` is not `https://` or `file:/`, whose
`owner` is not an account of the host or is root, or whose prefix's `Cellar/`, `Library/Taps/` and resolved
`bin/brew` are owned by another account or writable by others (group write for `admin`, Homebrew's own default, is
allowed). A prefix or a `brew` not yet installed is not a refusal: the Mac does not announce `brew` until it is there,
as a Linux host without apt's tools does not announce `apt` (section 4.1). The tap is tapped under a name of
hostkeeper's, `hostkeeper/<name>`, so a formula's full name is `hostkeeper/<name>/<package>@<V>` and no tap the owner
adds by hand can stand in for it. A Mac announces `brew` when it has at least one such entry and its brew. A7 owns the
directory's loader (section 3.1); A9 owns the `.tap` schema and these checks. On a Mac the entries are placed by the
MDM kit with `hostkeeper.toml` (hostkeeper-mdm.md), which is A17's Mac half.

**The version: one formula per release, `<package>@<V>`.** A plain formula installs whatever version its file holds
at the tap's head, so `brew install` has no way to say "version V", and `brew pin` stops upgrades but chooses no
version. So the release machinery, which already generates tierstore's formula, writes a versioned formula per
release, the file of the formula `tierstore@1.6.0`, and never edits it after; promotion copies it to the stable tap unchanged. The
entry's `manifest.package` is the base name (`tierstore`), its `package_version` the formula's version (`1.6.0`),
and the daemon installs `hostkeeper/<name>/<package>@<package_version>`. The brew backend refuses a
`package_version` with `:`, `~` or `+`, which no formula name may hold (`order_invalid`). Each such formula is
`keg_only :versioned_formula`, declares no dependency, defines no `post_install`, and has one bottle under the tag
`all`, `cellar :any_skip_relocation` (a static build, which needs no rewriting at pour), so one SHA-256 serves every
Mac; a formula with per-tag bottles is refused (`manifest_invalid`). The bottle carries the service's launchd plist in the keg, at the path the entry's `place`
names.

**Trust.** A tap has no signature. What a Mac trusts is the tap's Git remote over HTTPS, the pin, and nothing the
prefix's owner can change:

- The **pin is the integrity**: the daemon refuses a formula whose bottle `sha256` for the host's tag is not the
  entry's `digest`, and hashes the downloaded bottle itself against it.
- The tap's history only moves forward: the daemon updates it by fast-forward alone, and a remote whose history was
  rewritten is refused (`tap_rewritten`).
- **Homebrew runs as the prefix's owner, never as root.** It refuses to run as root, and it must not: a formula is
  Ruby code that `brew` evaluates. The daemon is root and starts each `brew` and `git` run through the step launcher
  as `/usr/bin/sudo --non-interactive --set-home --user=<owner> /usr/bin/env -i <environment> <command>`, so the
  account switch, the owner's groups and its home are sudo's (no code of hostkeeper's changes its own uid), and the
  environment is a fixed list with nothing inherited: `HOME`, `USER` and `LOGNAME` of the owner, `TMPDIR` a
  per-service directory the owner owns, `PATH=<prefix>/bin:/usr/bin:/bin`, `HOMEBREW_NO_AUTO_UPDATE=1`,
  `HOMEBREW_NO_INSTALL_CLEANUP=1`, `HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK=1`, `HOMEBREW_NO_ANALYTICS=1`,
  `HOMEBREW_NO_ENV_HINTS=1`, and `HOMEBREW_CACHE` a per-service directory the owner owns. These are the child's
  environment, set by the daemon; none is configuration the daemon reads (rule S2). The step launcher gains this
  call, a tool run as another account with a given environment, in A9; today's launcher takes only a service, an
  executable, a unit and a bound.
- **Root never runs a byte from the prefix.** The prefix and its kegs are the owner's, so a program there could be
  changed by anything that runs as the owner. The daemon copies the pinned bottle out of the cache into its own
  root-owned staging directory, hashes the copy, and extracts that copy into the version's sealed `tree/`. The service
  runs as root from `current/tree/`, never from the keg. Whatever runs as the owner (the tap's Ruby, `git`, `brew`)
  can change the tap, the cache and the keg, and so can only change Homebrew's view of the service, which the
  observer reports as drift; it cannot change what root runs. The keg is what Homebrew knows the service by: its version,
  its pin, the link an operator's shell finds; the sealed tree is what runs.

**The version directory** is section 4.2's, with `brew/` in place of `apt/`: `brew/<name>.tap` (the entry as it was
at stage), `brew/<formula>.rb` (the formula file at the tap commit staged), `brew/commit` (that commit),
`brew/<bottle>.tar.gz` (the pinned bottle), and `tree/`, the keg the bottle holds.

| Step | Does | Network |
|---|---|---|
| `stage` | 1. Tap the channel if it is not (`brew tap hostkeeper/<name> <remote>`), then fast-forward it (`git -C <tap> fetch` and `merge --ff-only`); a fetch that fails is `fetch_failed`, history that does not fast-forward `tap_rewritten`. 2. `brew info --json=v2 <formula>`: the formula exists (else `not_in_repository`), its version is V, it is keg-only, it has no dependency (else `dependency_changes`), no `post_install` (else `manifest_invalid`), a bottle under the tag `all` (else `no_bottle_for_host`) whose cellar is `any_skip_relocation` (else `manifest_invalid`) and whose `sha256` is the pin (else `package_digest_mismatch`). 3. `brew fetch --force-bottle <formula>`, which downloads the bottle and checks it against the formula; find it with `brew --cache --force-bottle <formula>` and copy it to the staging directory and hash the copy against the pin. 4. Extract the copy with the bounded reader (packages.md section 5.2) into `tree/`, then the core's walk and `MacVerify` (packages.md section 5.2). 5. Every `run` label's plist is a file of the tree that the entry's `place` names, and passes the placer's lint: its program is under `current/tree/`. 6. Seal | Steps 1 and 3, as the owner; no package code runs as root |
| `activate` | Under the host-wide installer lock: `brew install --force-bottle <formula>` from the cached bottle (the daemon puts its sealed copy back in the cache first, so nothing is downloaded); `brew unlink` the service's other kegs and `brew link --force <formula>`; `brew pin <formula>`; `brew list --versions <formula>` must show V; record in `placed.json` the installer `brew`, the formula, the version and the owner; place the plist from `current/tree/` into `/Library/LaunchDaemons/` as a link, by today's placement; switch `current` | None needed: the bottle is in the cache |
| `retire` | `brew unpin` and `brew uninstall` the formula of a version that is neither `current` nor `previous`, then delete its version directory, so a crash between the two leaves a directory and no unrecorded keg | None |
| `remove` | Remove the placed plist links; `brew unpin` and `brew uninstall --force` every formula of the service hostkeeper recorded (current and previous), and any other `hostkeeper/<name>/<package>@*` formula, of this service's package in its own tap, installed by hand (drift, reported) | None |

**Who writes the launchd job: hostkeeper, from the bottle.** The formula's `service` block is not used, and
`brew services` never runs. `brew services` would load the job as the owner, or as root from a plist that points into
the owner's prefix, and it would compete with hostkeeper for the job's start and stop. The plist is a file of the
pinned bottle, placed by hostkeeper exactly as a tarball's units are (packages.md section 4.4: a unit `place` entry,
its program only through `current/`), so its text is part of the pinned bytes and never the unattested manifest's.
hostkeeper loads, starts and stops the job, as for every macOS service.

**Update, rollback, the hold, removal.**

- **Update** to V: `stage` V, the hook's `prepare` from V's tree, `activate` V, then `cutover`, `verify`, `cleanup`.
  Both kegs, V's and the previous P's, stay installed: they are two formulae.
- **The hold** is `brew pin`. An operator's `brew upgrade` skips a pinned formula, and a versioned formula has no
  newer version to move to anyway, since its file never changes.
- **Rollback** to P: switch `current` back to P's tree, `brew unlink` V and `brew link --force` P's kept keg. Nothing
  is fetched and nothing is installed, so the network and the tap need not be up.
- **A version no longer on the host** is staged again from the tap, which keeps every versioned formula.
- **Uninstall** keeps section 4.4's order with `remove` above. The service's data stays, as for every installer.

**Drift.** At every heartbeat the observer reads the prefix directly, with no brew run: the service's kegs under
`<prefix>/Cellar/<package>@*/` and the link `<prefix>/opt/<package>@<V>`. It compares the linked keg with `current`,
and the keg's files with the sealed tree, leaving out what pouring adds (`INSTALL_RECEIPT.json` and `.brew/`). A different keg linked, a version of
the service's formula installed outside hostkeeper, or a keg whose files differ from the tree is drift, reported as
for apt (`ServiceBeat.drift`). The next `install` or `update` corrects it: it uninstalls an unrecorded keg,
reinstalls a changed one from the sealed bottle (`brew unpin`, `brew reinstall --force-bottle`, since brew refuses
to reinstall a pinned formula), and links and pins the desired
version, and its verdict is `Succeeded`, `drift_repaired`. Since root runs only the sealed tree, a changed keg is a
drift of Homebrew's view, never of what runs.

**Failures.** As section 4.5, with these for brew:

| What happened | Step | Outcome |
|---|---|---|
| The order's `repository` is not a `.tap` entry of this Mac | `stage` | `Failed`, `environment`, `repository_unknown` |
| The tap's remote cannot be reached; the bottle cannot be downloaded | `stage` 1, 3 | `Deferred`, `environment`, `fetch_failed` |
| The tap's history does not fast-forward | `stage` 1 | `Failed`, `environment`, `tap_rewritten` |
| No formula `<package>@V` in the tap (`brew info` exits non-zero and the formula file is not in the tap) | `stage` 2 | `Deferred`, `environment`, `not_in_repository` |
| The formula's `all` bottle `sha256` is not the pin | `stage` 2 | `Failed`, `release`, `package_digest_mismatch` |
| The formula has no `all` bottle | `stage` 2 | `Failed`, `release`, `no_bottle_for_host` |
| The formula declares a dependency | `stage` 2 | `Failed`, `environment`, `dependency_changes` |
| The formula is not keg-only, defines `post_install`, needs relocation, has per-tag bottles, or its plist is not in the tree | `stage` 2, 5 | `Failed`, `release`, `manifest_invalid` |
| The downloaded bottle is not the pin | `stage` 3 | `Deferred`, `environment`, `store_digest_mismatch` |
| Another Homebrew process holds its lock | any | `Deferred`, `environment`, `package_manager_busy` |
| `brew install`, `link` or `pin` failed, or V is not listed after it | `activate` | `Failed`, `release`, `brew_error`, with brew's last error line; rolled back to P, or a first install removed |

A9's device tests run in the `macos-device` job against a tap and bottles on the runner's disk (`file:` remote and
bottle URLs), installed as a runner account that owns a Homebrew prefix with no `homebrew/core` tap, and show that
`activate` and a rollback fetch nothing.

## 5. Health and hand-over

**The units are hostkeeper's to run.** On Linux, the release machinery's install check on clean machines asserts that the
package installs its units disabled and not started (tierstore's does today). hostkeeper then owns them as it owns a
tarball's: each `activate` enables every `run` unit not yet enabled by writing its `WantedBy` links and recording
them in `placed.json`, and takes away the links of a unit the version no longer runs, as hostkeeper-design.md section
4.7 does for placed units, so the units start at host boot; the daemon starts, stops and restarts them in built-in
steps, in `force` and at its own start; `remove` takes the links away. A package whose own scripts enable or start its units is not stopped from doing so (packages-lifecycle.md
section 6.2): its enablement link is left unclaimed, as today's rule says, and a start in `postinst` is a hand-over
outside the hook. The install check is where such a package is refused, before any host sees it.

**On a Mac** the same holds for the launchd job: hostkeeper places the plist from the sealed tree (section 4.7),
loads and starts the job in built-in steps, in `force` and at its own start, and removes it in `remove`. The job's
program is under `current/tree/`, so a rollback's switch of `current` re-points it. macOS has no hand-over of ports
(hostkeeper-design.md), so an update of a Mac service is a restart. The observer hashes the running program against
the sealed tree, which is the file it runs.

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
`version_mismatch`. For `apt` the observer also reads the installed version with `dpkg-query` (for `brew`, the
prefix, section 4.7); a version other than the one
recorded is drift (section 4.4). The health URL is probed as today.

## 6. The next backends

Every format the project publishes gets the same two channels, testing and stable, with the same bytes promoted, so
each later manager is a backend of section 4's installer, shaped like `apt` rather than `brew` where its repository
is signed: an entry of its own kind under `/etc/hostkeeper/repositories/` with its key, a work directory that sees that repository alone, the pin against the
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
- The host: `/etc/hostkeeper/repositories/` is a new directory an older daemon does not read; `placed.json`'s
  `installed` gains the installers `apt` and `brew` and the `repository` key, which an older binary keeps on rewrite
  (`crates/hostkeeper-placed/src/store.rs`); a version directory with `apt/` or `brew/` is one an older binary does
  not hold,
  except after a fallback, which packages.md section 9.1 covers: the earlier binary reports such a service as
  unreadable and leaves it, and a rollback of `hostkeeper` below A7 (A9 on a Mac) on a host that holds an `apt`
  (`brew`) version is `rollback_below_installer`.
- `agent.proto`: `ServiceBeat.drift = 14`, an optional message (the version found, the version recorded), which an
  older coordinator skips; its test is in `additions_installers.rs` beside `ServiceBeat.installer` (A7).
- No refusal is relaxed or tightened for an artifact entry, and `deb`, `rpm`, `arch`, `apk` and `macos-pkg` were never
  listed by any daemon, so no host was ever ordered one: withdrawing them changes nothing that shipped. A set that
  names them is still accepted, as a set with any unknown installer name is, and no host is ever picked for it.

The stages of packages.md section 9.2 gain one line each: A16 is released with or after stage 1's coordinator and
before any daemon announces `apt` or `brew`; A7 and A9 are in stage 4, as every installer is; A17 follows A7 and
A9. Stages 5 and 6 (A11, A12)
and their order are unchanged.

## 8. The build items, re-scoped

The channel's removal still holds for a service that comes from a repository: such a service never used the channel,
so it adds nothing to A11's or A12's conditions, and its hosts count toward A11's condition through `target-v1`
alone. A11 still follows stage 4, as packages.md section 9.2 orders it. hostkeeper's
own release stays `hostkeeper-archive` with its index, and the channel keeps hostkeeper's archives, unchanged.

| # | New scope, one line |
|---|---|
| A4 | Unchanged, and the `agent.proto` header sentence (packages.md section 7.1) also says an order may name a package, its version and a repository the host's configuration defines; A4 reads no repository configuration |
| A6 | Unchanged; what a repository installer needs of A6's trait and loader, A7 adds |
| A7 | The repository installer kind with its backend interface, and the `apt` backend: the installer trait's `stage` from a repository entry, `VersionDir::read` of a directory with `apt/` and no `artifact`, the two fetch commands as the only runs without `PrivateNetwork=yes`; the repository configuration and its checks at start, the announce rule, `stage` (update, index read, pin, download, control fields, extract, walk, units, simulation), `activate` with the hold and enablement, rollback by downgrade from the kept index and package, `retire`, `remove` by purge, drift and `ServiceBeat.drift` shown by the coordinator, the failure reasons of section 4.5, `coordinator_too_old` for an entry with no repository; replaces `deb` |
| A8 | `dnf` backend (later): its own design pull request once an RPM host exists (section 6), then the backend; replaces `rpm`. Until then an RPM-family host takes the tarball |
| A9 | The `brew` backend (section 4.7): the `.tap` entry and its checks at start, the announce rule, brew and git run as the prefix's owner with the fixed environment, `stage` (fast-forward, `brew info`, pin, `brew fetch`, the copy, extract, walk, `MacVerify`, plist), `activate` (install from the cached bottle, link, pin), rollback by relinking, `retire` and `remove` with `brew uninstall`, drift, the failure reasons; after A7 and A16; replaces `macos-pkg` |
| A10 | Unchanged (`oci-image`) |
| A11 | Unchanged; repository services never used the channel and add nothing to its conditions |
| A12 | Unchanged |
| A13 | Withdrawn, with `apk` |
| A14 | Withdrawn, with `arch` |
| A16 (new) | `Target.repository` and `Target.package_version` in `release.proto`; `hostkeeper-release`'s checks of section 2.2 and its installer set (`apt` and `brew` beside `hostkeeper-archive`, `tarball`, `oci-image`; `deb`, `rpm`, `arch`, `apk`, `macos-pkg` dropped; a later backend adds its name); the coordinator's audit of a repository entry; after A2, before A7 |
| A17 (new) | Host setup places repository entries and keyrings: `hostkeeper-pxe --repositories <dir>` checked by the daemon's loader, the enrollment runbook's step, the MDM kit's `.tap` entries on a Mac, and a rotation runbook (section 3.2); after A7 and A9 |

A5 (CI's archive entries for hostkeeper and `hello`) and A15 (the static build) are unaffected. A service's release
workflow sends repository entries once A16 and A7 are released; that is the shared release machinery's change, not
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
| Who enables the units | hostkeeper, by `WantedBy` links it records, at each `activate` for the units not yet enabled | The package installs them disabled, as the install check asserts; the owner of the start is the owner of the enablement |
| The hook of a repository package | A program of the extracted tree of the downloaded package, or built-in steps | The program is part of the signed package, and the hook contract stays the same for every installer |
| Key placement and rotation | Host setup (`hostkeeper-pxe --repositories`, the runbook), with an overlap of two keys | The coordinator never holds the trust root; a self-renewing keyring package cannot be revoked after a leak |
| `deb`, `rpm`, `arch`, `apk`, `macos-pkg` | Withdrawn | The ruling: no package file is installed; no daemon ever listed them |
| brew's version | One versioned formula per release, `<package>@<V>`, never edited after | `brew install` has no version argument, and `brew pin` chooses none |
| Who runs brew | The prefix's owner, by the step launcher, with a fixed environment; never root | Homebrew refuses root, and a formula is Ruby that must not run as root |
| What runs as root on a Mac | The pinned bottle's sealed copy in `current/tree/`, never the keg | The prefix is the owner's; root must run nothing the owner could change |
| Who writes the launchd job | hostkeeper places the plist from the bottle, as a tarball's units; `brew services` is never used | The job's text is then part of the pinned bytes, and hostkeeper alone starts and stops it |
| brew's trust | The pin (required), and a tap that only fast-forwards | A tap has no signature |
| brew's hold and rollback | `brew pin`; rollback relinks the kept previous keg | Both kegs stay installed, so a rollback needs no network |
