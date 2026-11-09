# Package repositories

Every release of `komira-ai/hostkeeper` that is published (see [verify-a-release.md](verify-a-release.md))
also goes into the package repositories of the organisation, at `https://packages.komira.ai`: the
Debian packages and the RPM packages of `hostkeeper` and `hostkeeper-coordinator`, the native
Debian packages of the services it releases (`hello`, and `mdm` for `amd64`), and the macOS
installers of `hostkeeper` and of `hello`. A release goes to a `testing` channel first. It is installed or downloaded from
there on clean machines, and only when all of that passed does the same release become `stable`. The
jobs are in `.github/workflows/release.yml`; the tool they run, `cargo xtask channels`, is
`crates/xtask/src/channels`.

The tool is copied from komira-ai/tierstore (`crates/xtask/src/channels`, at commit
`ff4777831c08fc13aa025b54e6e0531c2f350f4f`), with the package names, the pool directory and the checks
made data (`crates/xtask/src/channels/spec.rs`). It is meant to be replaced by ucdk's package
publishers; until then a change to the copy is made here.

## Where the files are

The host is shared by the organisation's products; each product writes only below its own pool
directory, `pool/hostkeeper`.

The apt index is one `Packages` file per architecture for every product, and the tool reads all of it
before it writes: it keeps another product's entries as they are and trims only the versions of the
packages it adds. It reads every entry strictly, so every product in the shared repository must list
`X.Y.Z` versions (no Debian revision) and the architectures `amd64` and `arm64` only; an entry that is
neither makes `apt index`, `apt promote` and `apt write` fail, naming the entry, and nothing is written.

| Channel | `testing` | `stable` |
|---|---|---|
| apt, packages `hostkeeper`, `hostkeeper-coordinator` and `hello`, architectures `amd64` and `arm64`, and `mdm`, `amd64` | `https://packages.komira.ai/apt`, suite `testing`, component `main` | the same repository, suite `stable` |
| rpm, packages `hostkeeper` and `hostkeeper-coordinator`, architectures `x86_64` and `aarch64` | `https://packages.komira.ai/rpm/testing/<arch>/` | `https://packages.komira.ai/rpm/stable/<arch>/` |
| macOS installers, `hostkeeper` and `hello`, `macos-arm64` | the directory `https://packages.komira.ai/pkg/hostkeeper/<version>/` | the pointer `https://packages.komira.ai/pkg/hostkeeper/stable.json` |

The packages themselves are in a pool both suites share: `apt/pool/hostkeeper/<name>_<version>_<arch>.deb`
and `rpm/pool/hostkeeper/<name>-<version>-1.<arch>.rpm`. A suite is an index that lists packages of
the pool, signed with the organisation's archive key. The key is published as
`https://packages.komira.ai/apt/komira-archive-keyring.gpg` (binary, for apt) and
`https://packages.komira.ai/apt/komira-archive-keyring.asc` (armored, for rpm).

The directory of the installers holds `hostkeeper-<version>-macos-arm64.pkg` and hello's
`hello-<version>-macos-arm64.pkg`, the attestation bundle of each (`<installer>.sigstore.json`), and a
`SHA256SUMS` that lists those four files.
The pointer `stable.json` is `{"version", "tag", "commit", "files": {<name>: {"sha256", "size"}}}`
for the newest promoted release.

**hostkeeper's macOS installer is signed but not notarized; hello's is neither.** The release signs
hostkeeper's with the Developer ID Installer identity before it is attested
([verify-a-release.md](verify-a-release.md)), and the pointer moves the same bytes that `testing` holds.
An MDM installs hostkeeper's as it is; by hand, install either from a terminal with `installer`, after
the checks below; a double click on a downloaded copy is refused by Gatekeeper, which asks for
notarization.

## Installing from a channel

apt (`stable`; use `testing` to take releases before they are promoted):

```sh
sudo curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
  --output /usr/share/keyrings/komira-archive-keyring.gpg \
  https://packages.komira.ai/apt/komira-archive-keyring.gpg
sudo tee /etc/apt/sources.list.d/komira.sources <<'EOF'
Types: deb
URIs: https://packages.komira.ai/apt
Suites: stable
Components: main
Signed-By: /usr/share/keyrings/komira-archive-keyring.gpg
EOF
sudo apt-get update
sudo apt-get install hostkeeper
```

`hostkeeper-coordinator` is installed the same way. The `Signed-By` field names the keyring file,
so apt trusts that key for this source alone.

dnf (`stable`; `testing` in the URL for the other channel):

```sh
sudo tee /etc/yum.repos.d/komira.repo <<'EOF'
[komira]
name=komira-ai packages
baseurl=https://packages.komira.ai/rpm/stable/$basearch
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://packages.komira.ai/apt/komira-archive-keyring.asc
EOF
sudo dnf install hostkeeper
```

Both the metadata (`repo_gpgcheck`) and each package (`gpgcheck`) are signed by the archive key.

macOS: read the pointer, download the installer and the `SHA256SUMS` of the version, compare the
digest, then install.

```sh
curl --proto '=https' --tlsv1.2 --fail --silent --show-error https://packages.komira.ai/pkg/hostkeeper/stable.json
```

The `version` it names gives the directory; the `files` give the SHA-256 and the size the installer must have.
The installer is also the attested release asset of the same name (see [verify-a-release.md](verify-a-release.md)),
so its attestation can be verified as for any other asset.

## The services' native packages and their entries

`hello`'s and `mdm`'s native packages (docs/design/packages-native.md, item A21) are the release's
assets `hello_<version>_<arch>.deb`, `hello-<version>-macos-arm64.pkg` and `mdm_<version>_amd64.deb`,
published with hostkeeper's own in the same passes, so a release is listed whole. hello's failing build
(`<version>-fail-ready`) is not published: its version is not `X.Y.Z`.

`cargo xtask release targets --service hello` (and `--service mdm`) lists each platform's native entry
before its archive one, both naming the channel `komira-testing`:

- on Linux an `apt` entry of the host's repository entry `komira-testing`, the `testing` suite above
  (`/etc/hostkeeper/repositories/komira-testing.sources`, placed by host setup), at the release's
  version, pinned by the `.deb`'s SHA-256;
- on a Mac a `macos-pkg` entry at `hostkeeper/<version>/hello-<version>-macos-arm64.pkg` in the store
  `komira-testing` of the Mac's `/etc/hostkeeper/stores.toml`, an `https` store rooted at
  `https://packages.komira.ai/pkg/`.

A host whose daemon does not announce the native installer, or has no such entry or store, is picked
the archive entry, as before.

## What the release workflow does

All of these run for the newest release of a run only: when one run releases several versions, the
package channels take the last, since a repository serves the newest version and it carries the
changes of the older ones. Each job is a green no-op until the settings below exist.

1. **Testing.** For apt, then for rpm, three jobs: `testing-<kind>-read` reads the current index and the
   ETag of each of its objects from the bucket, `testing-<kind>-index` merges the packages into it
   (every other product's entries are kept) and signs it, and `testing-<kind>-write` uploads each
   package once, then the index, with `If-Match` on the ETags that were read. The signing key and the
   cloud role are never in one job. A write that finds the index changed in between (another product's
   release) ends green with `conflict=true`, and attempts 2 and 3 (the jobs with the suffix `-2` and
   `-3`) read, merge and sign again; the third attempt fails instead of asking for a fourth. The
   installers go into `pkg/hostkeeper/<version>/` (`testing-pkg-upload`), each file once: a second put
   of the same bytes is accepted, other bytes at the same key fail.
2. **Checks.** `check-apt` installs each package from the suite `testing` in a clean Ubuntu
   container of each architecture it is built for, `check-rpm` installs each rpm with dnf in a clean
   Fedora container, both images pinned by digest, and each holds the installed system to what the
   package promises: the version its binary says (and the commit, but for `hello`, whose program
   prints its version alone), the files it ships, the user and the units, and that the units are
   enabled or not as the package says (a service's native package enables none: hostkeeper's
   installer does). `check-pkg` downloads each installer, its bundle and the directory's
   `SHA256SUMS` from the public host and compares them with the release's `SHA256SUMS`.
3. **Decision.** `promote-check` (`cargo xtask channels decide`) promotes when every write succeeded
   and every check succeeded or had nothing to check.
4. **Stable.** `stable-apt-*` and `stable-rpm-*` list the entries of the release that `testing` holds
   in the index of `stable` (held to the digests of the release's `SHA256SUMS`; nothing is uploaded, the
   packages are in the pool already). `promote-pkg` reads the directory of the installers back
   against `SHA256SUMS` and moves `stable.json` to the release, with `If-Match` on the ETag it read and
   only to a higher version, then invalidates the path in the CDN. **Promotion copies no bytes:** the
   same pool files and the same installer files that were installed and checked are the ones the
   `stable` channel names.

Re-running the failed jobs of a run resumes it: every write is write-once or idempotent, and
`promote-check` reads the results again.

## How the steps run

Every step of `release.yml` is one command on one line (the workflow lint refuses a `run: |` block,
`&&` or `;`); what is more than a command is a verb of `cargo xtask`, which has tests.

- **Secrets.** The `apt sign`, `rpm sign` and `rpm repo sign` steps carry `APT_GPG_PRIVATE_KEY` and
  `APT_GPG_PASSPHRASE` in their environment. The verb writes them to files (mode 0600) below its
  `--work` directory, signs, and removes the files and the keyring when it ends, whatever happened. A
  variable that is not set is a usage error (exit status 2) naming it. Neither is ever an argument.
- **The token request.** The `channels assume` steps pass the runner's token request URL as
  `--token-url "$ACTIONS_ID_TOKEN_REQUEST_URL"` (an `https` URL); only the bearer
  (`ACTIONS_ID_TOKEN_REQUEST_TOKEN`, secret material) is read from the environment.
- **The Fedora check.** `check-rpm` runs each program in the container as `docker exec <container>
  <program> <args>`, with no shell: `test -f` for a file, `find` for the unit's links and for the
  processes (`/proc/<pid>/comm`, read with `grep -l -x`; the image has no `procps-ng`). The
  repository file is written on the runner in `--scratch` (`$RUNNER_TEMP`) and put in the container
  with `docker cp`.

## What the owner sets

The jobs do nothing until the repository's environments hold their settings (a job with none of its
settings ends green; a job with some of them fails and names the missing ones).

| Where | Name | Holds |
|---|---|---|
| environment `apt-publish` | variable `APT_PUBLISH_ROLE_ARN` | the role the jobs assume through OIDC; its policy must let it write `apt/pool/hostkeeper/*` and `rpm/pool/hostkeeper/*` (only when the object is absent), the shared `apt/dists/*` and `rpm/<suite>/*` indexes (with `If-Match` or only when absent), and `pkg/hostkeeper/*`, and trust this workflow on `main` in this environment |
| environment `apt-publish` | variables `APT_BUCKET`, `APT_DISTRIBUTION_ID`, `APT_HOSTNAME` | the bucket, the CDN distribution to invalidate, and the public host |
| environment `apt-signing` | variable `APT_GPG_FINGERPRINT`, secrets `APT_GPG_PRIVATE_KEY` and `APT_GPG_PASSPHRASE` | the archive signing subkey |
| the repository | variable `RPM_REPO_READY` | `true` once the rpm tree of the repository is deployed; until then the rpm jobs end green and write nothing |
