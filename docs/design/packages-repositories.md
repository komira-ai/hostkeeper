# Packages from a package repository: the target names a package, a version and a repository

Status: design, for the maintainers' ruling. Nothing here is built. This amends [packages.md](packages.md),
[packages-lifecycle.md](packages-lifecycle.md) and [packages-tests.md](packages-tests.md), as the maintainers ruled:
hostkeeper installs a service's Linux package from a package repository with the system package manager
(`apt-get install tierstore=<V>` from the suite the host is configured with). It does not fetch a package file and
install that file. The repository's signing key is the trust. Where those three files disagree with this one about a
Linux distribution's package, this one wins. Section 0 lists what it replaces. Section numbers like "packages.md
section 5.2" are that file's; a bare "section 3" is this file's.

The release machinery that every repository of the project shares is moving to this flow: publish each release to a
testing suite of an apt repository, check that it installs on clean machines, then promote the same bytes to the
stable suite (and to a Homebrew tap for macOS). tierstore goes first; its farm stage
(komira-ai/tierstore#255) moves to the testing suite. The step after the clean-machine check is "install in the lab
through hostkeeper". So hostkeeper's order for such a service must say "install version V of package P from
repository R". Nothing below is specific to tierstore.

## 0. What this replaces

| Where | Before | After |
|---|---|---|
| packages.md 3.1, 3.4, 9.4: the target | Every entry is an artifact: `locations`, `digest`, `bytes`, `attestation`, `manifest` | Also a **repository entry**: `repository` and `package_version` set, no artifact fields, an optional `digest` pin (section 2) |
| packages.md 5.2, 5.4: `deb`, `rpm`, `arch` | Installers that fetch a package file by digest, check its attestation and install the file with `dpkg`, `rpm` or `pacman` | Withdrawn. `apt` (A7) installs from a repository. `dnf` (A8), `pacman` (A14) and `apk` (A13) follow the same shape, later (section 6) |
| packages.md 5.2, A9: `macos-pkg` | A built-in installer for a flat `.pkg` | Withdrawn: no service ships one for hostkeeper to install, and the project's macOS route is Homebrew. `brew` (A9, later) takes its place (section 6.4) |
| packages.md 5.2, 5.3: "the daemon never runs `apt`"; "no installer fetches" | Every tool run has `PrivateNetwork=yes` | A repository installer fetches through the package manager, from the one repository the order names, in `stage` only, in two commands that run no package script. Every other tool run keeps `PrivateNetwork=yes` (section 4.6) |
| packages.md 7.4, 8: trust for a Linux package | `[trusted_signers]` and the artifact's attestation | The repository's key, placed on the host by host setup, and the optional digest pin. No attestation is checked or accepted for a repository entry (section 3) |
| packages-lifecycle.md 6.1, 6.4 to 6.6 for `deb` and `rpm` | The kept artifact; rollback installs it; `dpkg --purge` | The kept index and package apt fetched; rollback is a downgrade with apt; the hold; `dpkg --purge` (section 4) |
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
  version and the repository, and the digest of the `.deb` as a pin:

  ```json
  "linux-x86_64": { "entries": [
    { "installer": "apt", "repository": "komira-testing", "package_version": "1.6.0-1",
      "digest": "<64 hex>", "manifest": { "package": "tierstore", "run": ["tierstore.service"], ... } },
    { "installer": "tarball", "locations": [...], "digest": "<64 hex>", "bytes": "...",
      "attestation": {...}, "manifest": {...} } ] }
  ```

- `komira-testing` is not a URL. It names an entry of the host's own repository configuration, which host setup put
  there with the repository's signing key (section 3). The coordinator never sends a URL, a suite or a key.
- A Debian or Ubuntu host with that entry runs, in effect, `apt-get install tierstore=1.6.0-1` against that
  repository alone, holds the package (`apt-mark hold`), and starts the service's units itself.
- A host without an `apt` repository configured does not announce `apt` and takes the next entry it can install (here
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
(dnf, pacman) still needs no coordinator change: the pick stays the equality of `installer` strings
(packages.md section 3.3).

### 2.2 What the coordinator checks and stores

A repository entry is checked by A16's rules in the shared crate `hostkeeper-release`, beside today's:

| Check, repository entry | Refusal |
|---|---|
| `repository` 1 to 32 bytes of `[a-z0-9-]`; `package_version` present, 1 to 128 bytes of `[A-Za-z0-9.+~:_-]`, its first byte a digit | `TARGET_INVALID` |
| `package_version` present on an entry without `repository` | `TARGET_INVALID` |
| No `locations`, `bytes` 0, no `attestation`: a repository entry names no artifact, and an attestation nobody checks must not look checked | `TARGET_INVALID` |
| `digest` empty (no pin) or 64 lower-case hex | `TARGET_INVALID` |
| `manifest.package` present: 1 to 128 bytes of `[A-Za-z0-9+._@-]`, its first byte a letter or digit | `TARGET_INVALID` |
| The manifest's installer-free checks (packages.md section 4.4), as for any entry | `TARGET_INVALID` |
| The service `hostkeeper` with a repository entry (only one `hostkeeper-archive` entry is allowed, as today) | `TARGET_INVALID` |

A repository entry whose manifest has no `package` is refused there too. The grammars are the union of the
managers' (dpkg, rpm, pacman, apk and Homebrew names and versions). An artifact entry is checked exactly as today.
A16 owns these checks, the two fields' comments and the header of `release.proto`, and the crate's installer set
(`Installer`, `names_a_package`); A7 owns the daemon's rules that depend on the installer: `apt`'s grammars for
`package` and `package_version` and the manifest rules of section 3.4.

The coordinator stores the entry as it was sent, as for every entry: in `<state-file>.releases` or the `releases`
table's `targets` column, as proto3 JSON. A release stays immutable (packages.md section 3.4): the same version set
again with another repository, package version or pin is `RELEASE_EXISTS`. The audit entry of the set lists, for a
repository entry, its installer, the repository, the package, the package version, the pin (or `no pin`) and the
SHA-256 of the manifest's encoding. `GetRelease` answers the entry as stored; a LAN mirror's sync job has nothing to
pull for it.

### 2.3 Old coordinators and old daemons

- **A coordinator before A16** (A2 or A3 built) refuses a set with a repository entry, loudly: over gRPC it skips the
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
- **A daemon with `apt`** given an artifact installer's entry with `repository` set, or an `apt` entry with
  artifact fields, answers `Failed`, `release`, `order_invalid`.

### 2.4 The digest pin

`digest` stays, optional, for a repository entry. When given, the daemon checks, before it downloads anything, that the
`SHA256` field of the package's stanza in the repository's signed index equals it, and refuses the order otherwise
(`Failed`, `release`, `package_digest_mismatch`). apt then checks the file it downloads against the same index. The
daemon also hashes the downloaded file itself and compares it with the pin.

The order's `package_digest` for a repository entry is the pin, or empty when there is none (the coordinator fills
it from the entry, A3). packages.md section 8.1's rule that the entry's `digest` equals `package_digest` applies, and
an empty one names no bytes: the daemon then checks no digest beyond apt's own (A7).

The pin is cheap (the release machinery knows the `.deb`'s SHA-256 when it publishes it) and it closes one gap of the
repository's trust: a package re-uploaded under the same version with other bytes, by mistake or by whoever holds the
repository's key, is refused instead of being installed on the hosts that stage after the upload and not on the
others. **Recommended: the shared release machinery always sends the pin.** The coordinator does not require it, so an
operator can set a version by hand, and a manager whose index has no SHA-256 of the file (apk, section 6.2) refuses a
pin rather than ignore it.

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
else. A host's daemon reads it at start and refuses to start (as for a bad `stores.toml`, rule S2: configuration in
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

A repository name means the same suite on every host of a service: CI's order names it, and every host it reaches
reads it from its own disk. Names that say the origin and the suite (`komira-testing`, `komira-stable`) keep that
plain. A host whose configuration lacks the named entry answers `Failed`, `environment`, `repository_unknown`: host
setup is wrong there, and the rollout gate stops on it.

### 3.2 Who places the key and who rotates it

**Host setup places the key and the entries, never the coordinator.** On a host installed by network
(network-install.md), `hostkeeper-pxe` takes a directory of entries and keyrings with `--repositories <dir>`, checks
it with the daemon's own loader before it writes a single seed (as it does `hostkeeper.toml`), and writes it to
`/etc/hostkeeper/repositories/` through the same autoinstall user data. On a host set up by hand, the enrollment
runbook (docs/operations/enrollment.md) gains the step that copies them. That is A17.

**Rotation is host setup's too.** The repository's owner publishes the next key ahead of time and signs the suites
with both keys for an overlap. Host setup replaces the keyring with one that holds both keys, on every host, before
the repository stops signing with the old key, and with the new key alone after. apt reads the keyring at every
`apt-get update`, so a new keyring needs no daemon restart; a new or changed `.sources` file takes effect at the
daemon's next start. hostkeeper never changes a keyring, and no order can name one.

A keyring package from the repository itself (Debian's `debian-archive-keyring` pattern) is not used: a key that
signs its own successor cannot be taken back after it leaks. Question 2 asks how a fleet changes host setup files.

### 3.3 What is trusted, and what is not used

For a repository entry, the chain is apt's: the keyring on the host verifies the suite's `InRelease`, which lists the
SHA-256 of the `Packages` index, which lists the SHA-256 of the `.deb`. The pin, when given, ties that last digest to
CI's. hostkeeper checks nothing about who built the bytes beyond that.

- **`attestation` is not used** for a repository entry. The coordinator refuses one (section 2.2), so no one reads a
  release as checked against `[trusted_signers]` when it was not. `[trusted_signers]` keeps guarding every artifact
  entry and hostkeeper's own release.
- **Whoever holds the repository's signing key decides the bytes** a host may install from it, within the pin when
  one is given. That is the release machinery that publishes to the repository. hostkeeper's attestation said "this
  organisation's CI built these bytes"; the repository's signature says "the repository's owner published them".
  Promotion from testing to stable copies the same bytes, so the pin of a version is the same in both suites.
- **The manifest is still not attested** (packages.md section 8.2), and still decides the hook command, its bounds and
  the `run` units. For a repository entry the hook program is a file of the signed package (section 4.3), so the
  caller of `SetDesired` chooses among the package's programs, as it chooses among a tarball's today.

| Guarantee (packages.md section 8.1) | Repository entry |
|---|---|
| An order names exact bytes | When pinned: the pin, checked against the signed index and the downloaded file. Unpinned: the package and version the signed index lists at stage, then kept on the host |
| Integrity before any byte is used | apt verifies the suite's signature and every hash before it hands a file over; the daemon extracts nothing before that, and checks the pin |
| Who decides the bytes | The holder of the repository's key (and CI's pin), not `[trusted_signers]` |
| What runs as root | The package's maintainer scripts, from the signed package, in `activate` and `remove`; the hook, a program of the signed package's tree |
| Removal | The package, purged, while its version is the one hostkeeper recorded (section 4.4) |

### 3.4 The manifest of a repository entry

The package carries its own units and maintainer scripts. The manifest keeps only what hostkeeper needs to own the
service:

| Field | Repository entry |
|---|---|
| `package` | Required: the package's name, checked by the daemon against the package manager's grammar and against the package's own control field |
| `hook` | Required. `command` empty: built-in steps. Otherwise a program of the package's extracted tree (`tree/usr/lib/<package>/…`), under today's rules. The bounds as today |
| `run` | Required, 1 to 64: the units the package installs that hostkeeper starts, stops, enables and observes. Each must be a unit file of the package, under `lib/systemd/system/` or `usr/lib/systemd/system/` of its tree |
| `health_url` | Optional, as today |
| `installer_bounds` | Required: `stage_secs` bounds the index fetch, the download and the checks; `activate_secs` the install; `remove_secs` the purge |
| `place`, `containers` | Refused: the package places its own files, and runs no image |
| `params` | Allowed only with a hook program, which reads them; refused with built-in steps, which read none |
| `min_hostkeeper` | As today |

## 4. The `apt` installer

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
| `remove` | `apt-mark unhold package`; `dpkg --purge package`, only while `dpkg-query` shows the version hostkeeper recorded, tried again within `remove_secs` while dpkg reports its lock held (dpkg has no lock timeout of its own); remove the enablement links hostkeeper recorded; `systemctl daemon-reload` | None |

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
- **A package hostkeeper did not install.** When `package` is installed and `placed.json` does not record it for this
  service, `install` and `force` alike are refused `Failed`, `release`, `package_not_ours` before anything changes,
  and another service's record is `package_conflict` (packages-lifecycle.md section 6.5). hostkeeper never takes over
  a package someone else installed; the operator purges it first. A recorded package that someone moved by hand to
  another version is left alone at `remove`, and raises `installed_changed`.

### 4.5 How the daemon reports failure

Each failure is an outcome with a class and a reason, as for every installer. The daemon classifies by the step and
the state it checks, and reads apt's messages only where apt has no other signal.

| What happened | Step | Outcome |
|---|---|---|
| The order's `repository` is not an entry of this host | `stage` | `Failed`, `environment`, `repository_unknown` |
| `apt-get update` could not reach the repository | `stage` 1 | `Deferred`, `environment`, `fetch_failed`, as for an artifact no store answered |
| The suite's signature did not verify: no signature, an unknown, expired or revoked key, a bad signature, or a `Valid-Until` passed (apt's `NO_PUBKEY`, `EXPKEYSIG`, `REVKEYSIG`, `BADSIG`, "is not signed", "is no longer signed", "has expired" errors) | `stage` 1 | `Failed`, `environment`, `signature`. Nothing is downloaded |
| The index lists no `package` at `V` for the host's architecture | `stage` 2 | `Deferred`, `environment`, `not_in_repository` |
| The index's `SHA256`, or the downloaded file's, is not the pin | `stage` 2, 4 | `Failed`, `release`, `package_digest_mismatch` |
| The index's `Size` is over `max_package_bytes` | `stage` 2 | `Failed`, `release`, `package_too_large` |
| The downloaded file is not what the index lists, with no pin given (apt's hash check) | `stage` 4 | `Deferred`, `environment`, `store_digest_mismatch`, as for a mirror that gave other bytes |
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
order names, and never while a package script runs.

## 5. Health and hand-over

**The units are hostkeeper's to run.** The release machinery's install check on clean machines asserts that the
package installs its units disabled and not started (tierstore's does today). hostkeeper then owns them as it owns a
tarball's: each `activate` enables every `run` unit not yet enabled by writing its `WantedBy` links and recording
them in `placed.json`, and takes away the links of a unit the version no longer runs, as hostkeeper-design.md section
4.7 does for placed units, so the units start at host boot; the daemon starts, stops and restarts them in built-in
steps, in `force` and at its own start; `remove` takes the links away. A package whose own scripts enable or start its units is not stopped from doing so (packages-lifecycle.md
section 6.2): its enablement link is left unclaimed, as today's rule says, and a start in `postinst` is a hand-over
outside the hook. The install check is where such a package is refused, before any host sees it.

**The first deploy.** A host that has never run the service gets an `install` order, as for any installer: `stage`,
the hook's `prepare` if there is a hook, `activate` (install, hold, enable), then `cutover`, which starts the units, as
there is nothing to hand over from, then `verify` and `cleanup`. The lab's plan of a forced first deploy
(`Releases.Force`) reaches only hosts that already announce the service, which a host does only once hostkeeper has
installed the package. So on fresh hosts the first deploy of a repository service is the coordinator's `install` of
the hosts the service is assigned to, and `Force` is what fixes a bad version afterwards: stop the `run` units,
`stage` the target if it is not on the host, `activate` it (a downgrade if need be), start the units, report, with no
hook. A host where the package was installed by hand is refused `package_not_ours`, by `install` and by `force`
(section 4.4).

**Later hand-offs.** With built-in steps, `cutover` restarts the `run` units: a gap of one restart. With a hook, the
hook does the hand-over from the new version's tree, as for an archive: `prepare` runs before `activate`, and
`cutover` after it. tierstore's port-forward hand-over (host-updates.md section 4.1) works this way, with one
constraint every repository package has: apt installs one version at a time. During the hand-over the old process
runs from the old files' inodes, which dpkg has replaced on disk. A hand-over that needs the old version's files at
their paths (to re-execute, or to read its own data files) cannot be done from a repository package, and such a
service stays a tarball. The package must not restart its units in its scripts (debhelper's `--no-start
--no-restart-after-upgrade`), so the hook decides when the new version starts.

**Observation.** packages-lifecycle.md section 6.6 is kept for a native package: the observer reads the first `run`
unit's program from the unit text in `tree/`, and hashes the running process's executable against that file of the
sealed tree. A process left from the previous version runs the old inode, whose bytes differ, and shows
`version_mismatch`. The observer also reads the installed version with `dpkg-query`; a version other than the one
recorded shows `installed_changed`. The health URL is probed as today.

## 6. The other repository managers

Each follows the shape of section 4: an entry of the host's configuration under `/etc/hostkeeper/repositories/`,
placed by host setup with its key; a work directory that sees that repository alone; a fetch in `stage` only; a
kept index and package per version so a rollback needs no network; a hold; removal while the recorded version is
installed. The installer's name is the manager's. Each is designed in full in its own pull request before it is built.

### 6.1 dnf (Fedora, RHEL and its rebuilds, Amazon Linux)

A `.repo` file with `gpgcheck=1`, `repo_gpgcheck=1` and `gpgkey=file:///etc/hostkeeper/repositories/<key>`; dnf runs
with `--setopt=reposdir=` the work directory, `--repo=<name>` and its cache in the work directory. `stage` is
`dnf makecache`, the simulation `dnf install --assumeno`, then `dnf install --downloadonly` of `package-V` (the
version as `version-release`, epoch as `E:`) into the version's own cache directory (`keepcache=1`, no `--destdir`, so
the package stays where dnf's cache finds it), with the pin checked against the `sha256` checksum of the package in
the signed repository metadata. `activate` is `dnf --cacheonly install package-V` from the version's kept metadata
and package (dnf also checks the package's own signature against the key), and a rollback is `dnf --cacheonly
downgrade package-P` from the previous version's copy, with no network, the versionlock lifted around it. The hold is `dnf versionlock` (built into dnf5; a plugin before it).
`remove` is `rpm --erase`; rpm has no purge, so a changed configuration file stays as `.rpmsave`. Failure reasons map
one to one onto section 4.5. openSUSE and SLES use zypper, which takes the same shape with `zypper addlock` as the
hold; it is a separate installer, `zypper`, when a service asks for it.

### 6.2 apk (Alpine)

A repository URL and its RSA keys in a key directory, passed with `--repositories-file` and `--keys-dir`; `apk add
package=V` from that repository alone, with `--cache-dir` in the work directory; the world file's `package=V`
constraint is itself the hold, so `apk upgrade` cannot move it; a rollback is `apk add package=P` from P's kept cache;
removal is `apk del`. APKINDEX lists a SHA-1 of the package's control section, not a SHA-256 of the file, so an `apk`
entry with a pin is refused (`Failed`, `release`, `order_invalid`) until the index gives one. It still waits on a
daemon that runs without systemd (packages.md question 5).

### 6.3 pacman (Arch Linux and its derivatives)

A `pacman.conf` fragment that names the one repository with `SigLevel = Required DatabaseRequired`, passed with
`--config`; `--dbpath` in the work directory, whose `local/` is a link to the system's `/var/lib/pacman/local` and whose
`sync/` holds the one repository's database; `--cachedir` in the work directory; and `--gpgdir` a pacman keyring that
host setup made with `pacman-key` from the entry's key. A sync database holds one version of each package, so the entry's `package_version` must be the version
the database lists (`not_in_repository` otherwise). For the same reason pacman cannot choose an older version from a
database, so a rollback is the one place a package file is installed: the previous version's kept package, which
pacman fetched from the repository, installed with `pacman --upgrade`, which checks its signature against the same
key again. A14's design names this exception for the maintainers' ruling (question 4). The pin is checked against the
database's `%SHA256SUM%`. pacman has no hold command: the hold is that the repository is in hostkeeper's configuration
and not in `/etc/pacman.conf`, so `pacman -Syu` never sees a newer version; host setup must not list it there. Removal
is `pacman --remove --nosave`.

### 6.4 brew (macOS)

The repository is a tap; the host's configuration entry (not the order) names its Git remote and the account that
owns the Homebrew prefix. **A formula per
release, `name@V`, not `brew install name` with a pin.** A plain formula installs whatever version its file holds at
the tap's head, so `brew install` cannot be told "version V", and `brew pin` stops upgrades but chooses no version. The
release machinery adds the formula `tierstore@1.6.0` to the tap at each release and never edits it after, so
`brew install <tap>/tierstore@1.6.0` installs exactly V. `manifest.package` is the base name (`tierstore`) and the
daemon installs `<package>@<package_version>`. Each such formula is `keg_only` and declares `conflicts_with` the
other versions, so both kegs stay installed side by side and hostkeeper links the current one with `brew link
--force`; a rollback is `brew unlink` of V and `brew link --force` of P, with no network. A tap has no signature: the trust is the tap's Git remote over HTTPS and the
bottle's `sha256` the formula holds, so for `brew` the pin is required, and the daemon checks the formula's bottle
digest equals it before it installs (an entry without a pin is `order_invalid`). Homebrew refuses to run as root, and
the daemon is root: A9's design settles running brew as the prefix's owner, and who writes the service's launchd job
(the formula's `service` block or hostkeeper), which is why A9 is later.

## 7. Compatibility and the staged path

Every change is additive (rule B8):

- `release.proto`: `Target.repository = 7` and `Target.package_version = 8`, both optional. An older reader keeps them
  and writes them back (`additions_repository.rs`, A16, the B8 test in both directions for the wire: a reader built
  from the baseline keeps the new fields; the new reader takes an entry without them as an artifact entry).
- The coordinator's releases: the same JSON with two more keys, which a coordinator before A16 keeps on rewrite. A16's
  test starts the previous coordinator's reader on a releases file and a Postgres table holding a repository entry.
- The host: `/etc/hostkeeper/repositories/` is a new directory an older daemon does not read; `placed.json`'s
  `installed` gains the installer `apt` and the `repository` key, which an older binary keeps on rewrite
  (`crates/hostkeeper-placed/src/store.rs`); a version directory with `apt/` is one an older binary does not hold,
  except after a fallback, which packages.md section 9.1 covers: the earlier binary reports such a service as
  unreadable and leaves it, and a rollback of `hostkeeper` below A7 on a host that holds an `apt` version is
  `rollback_below_installer`.
- No refusal is relaxed or tightened for an artifact entry, and `deb`, `rpm`, `arch` and `macos-pkg` were never
  listed by any daemon, so no host was ever ordered one: withdrawing them changes nothing that shipped. A set that
  names them is still accepted, as a set with any unknown installer name is, and no host is ever picked for it.

The stages of packages.md section 9.2 gain one line each: A16 is released with or after stage 1's coordinator and
before any daemon announces `apt`; A7 is in stage 4, as every installer is; A17 follows A7. Stages 5 and 6 (A11, A12)
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
| A7 | `apt`: the installer trait's `stage` from a repository entry, `VersionDir::read` of a directory with `apt/` and no `artifact`, the two fetch commands as the only runs without `PrivateNetwork=yes`; the repository configuration and its checks at start, the announce rule, `stage` (update, index read, pin, download, control fields, extract, walk, units, simulation), `activate` with the hold and enablement, rollback by downgrade from the kept index and package, `retire`, `remove` by purge, the failure reasons of section 4.5, `coordinator_too_old` for an entry with no repository; replaces `deb` |
| A8 | `dnf` (later): its own design pull request on section 6.1, then the installer; replaces `rpm`. Until then an RPM-family host takes the tarball |
| A9 | `brew` (later): its own design pull request on section 6.4 (brew as the prefix's owner, the launchd job), then the installer; `macos-pkg` is withdrawn |
| A10 | Unchanged (`oci-image`) |
| A11 | Unchanged; repository services never used the channel and add nothing to its conditions |
| A12 | Unchanged |
| A13 | `apk` (later), as section 6.2: the repository installer, no pin; still after a daemon that runs without systemd |
| A14 | `pacman` (later), as section 6.3; replaces `arch` |
| A16 (new) | `Target.repository` and `Target.package_version` in `release.proto`; `hostkeeper-release`'s checks of section 2.2 and its installer set (`apt`, `apk`, `brew`, `dnf`, `pacman` beside `hostkeeper-archive`, `tarball`, `oci-image`; `deb`, `rpm`, `arch`, `macos-pkg` dropped); the coordinator's audit of a repository entry; after A2, before A7 |
| A17 (new) | Host setup places repository entries and keyrings: `hostkeeper-pxe --repositories <dir>` checked by the daemon's loader, the enrollment runbook's step, and a rotation runbook (section 3.2); after A7 |

A5 (CI's archive entries for hostkeeper and `hello`) and A15 (the static build) are unaffected. A service's release
workflow sends repository entries once A16 and A7 are released; that is the shared release machinery's change, not
an item of this repository.

## 9. Decisions

| Question | Decision | Why |
|---|---|---|
| The target's form | `installer`, `repository`, `package_version`, optional `digest`, and the package's name in the existing `manifest.package` | `manifest.package` already means the name the package manager knows; a second field would be a second way to say it |
| `version` or `package_version` | `package_version` | A manager's version carries a revision and an epoch, so it is often not `desired.version`; one name for both invites confusion |
| How the coordinator tells a repository entry | By `repository` being set, never by the installer's name | A new repository manager then needs no coordinator change, as the pick needs none |
| The digest pin | Kept, optional; checked against the signed index and the downloaded file; refused by managers whose index has no SHA-256; required for `brew` | Cheap, and it refuses a re-uploaded version; a tap has no signature, so for brew the pin is the integrity |
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
| A package installed outside hostkeeper | `package_not_ours`, for install and force | hostkeeper does not adopt what it did not install |
| Who enables the units | hostkeeper, by `WantedBy` links it records, at each `activate` for the units not yet enabled | The package installs them disabled, as the install check asserts; the owner of the start is the owner of the enablement |
| The hook of a repository package | A program of the extracted tree of the downloaded package, or built-in steps | The program is part of the signed package, and the hook contract stays the same for every installer |
| Key placement and rotation | Host setup (`hostkeeper-pxe --repositories`, the runbook), with an overlap of two keys | The coordinator never holds the trust root; a self-renewing keyring package cannot be revoked after a leak |
| brew's version | One formula per release, `name@V` | `brew install` has no version argument, and `brew pin` chooses none |
| `deb`, `rpm`, `arch`, `macos-pkg` | Withdrawn | The ruling: no package file is installed (pacman's rollback is the one exception, question 4); no service ships a `.pkg` for hostkeeper; no daemon ever listed them |

## 10. Questions for the maintainers

1. **A pin required of the shared release machinery.** Section 2.4 recommends the release workflow always pins. The
   coordinator could refuse an unpinned repository entry set by CI's token and accept one set by an operator; that is
   a rule on the caller, which this design leaves out.
2. **Changing host setup files on a running fleet.** A key rotation (section 3.2) and a new repository entry change
   files under `/etc/hostkeeper/` on every host. Today only a reinstall or an operator on the host does. A fleet-wide
   way (a host-setup service of hostkeeper's own, attested as hostkeeper's release is) would be a design of its own.
3. **Adopting a package installed by hand.** Section 4.4 refuses it. An explicit operator verb could record a package
   at its installed version as hostkeeper's; nothing needs it yet.
4. **pacman's rollback from a kept file.** pacman cannot choose an older version from a sync database, so section 6.3
   rolls back by installing the previous version's kept package, which pacman fetched from the repository and checks
   against its key again. That departs from "no package file is installed" for one step of one later installer; the
   other choice is a pacman repository that keeps every version under its own name, which the release machinery would
   have to publish.
