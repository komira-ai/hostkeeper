# Package repositories

Every release of `komira-ai/hostkeeper` that is published (see [verify-a-release.md](verify-a-release.md))
also goes into the package repositories of the organisation, at `https://packages.komira.ai`: the
Debian packages and the RPM packages of `hostkeeper` and `hostkeeper-coordinator`, the native
Debian packages of the services it releases (`hello`, `pxe`, and `mdm` for `amd64`), and the macOS
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
packages it adds. It reads every entry of a suite strictly in the form that suite takes, so every
product in the shared repository must list, in `testing` and `stable`, `X.Y.Z` versions exactly (no
Debian revision, no `~`), and in `testing-fail` the one form and package below, and in all of them the
architectures `amd64` and `arm64` only; an entry that is not of its suite's form makes `apt index`,
`apt promote` and `apt write` fail, naming the entry, and nothing is written. The suite `testing-fail`
lists `hello` alone, at `X.Y.Z~fail.ready` exactly: a plain `X.Y.Z` entry, an entry of another package,
or any other version in it is refused in the same way, and an `X.Y.Z~fail.ready` entry in `testing` or
`stable` is too.

| Channel | `testing` | `stable` |
|---|---|---|
| apt, packages `hostkeeper`, `hostkeeper-coordinator`, `hello` and `pxe`, architectures `amd64` and `arm64`, and `mdm`, `amd64` | `https://packages.komira.ai/apt`, suite `testing`, component `main` | the same repository, suite `stable` |
| rpm, packages `hostkeeper` and `hostkeeper-coordinator`, architectures `x86_64` and `aarch64` | `https://packages.komira.ai/rpm/testing/<arch>/` | `https://packages.komira.ai/rpm/stable/<arch>/` |
| macOS installers, `hostkeeper` and `hello`, `macos-arm64` | the directory `https://packages.komira.ai/pkg/hostkeeper/<version>/` | the pointer `https://packages.komira.ai/pkg/hostkeeper/stable.json` |

The packages themselves are in a pool every suite shares: `apt/pool/hostkeeper/<name>_<version>_<arch>.deb`
and `rpm/pool/hostkeeper/<name>-<version>-1.<arch>.rpm`. A suite is an index that lists packages of
the pool, signed with the organisation's archive key (the same key for all three apt suites). The key is published as
`https://packages.komira.ai/apt/komira-archive-keyring.gpg` (binary, for apt) and
`https://packages.komira.ai/apt/komira-archive-keyring.asc` (armored, for rpm).

The directory of the installers holds `hostkeeper-<version>-macos-arm64.pkg` and hello's
`hello-<version>-macos-arm64.pkg`, the attestation bundle of each (`<installer>.sigstore.json`), and a
`SHA256SUMS` that lists those four files.
The pointer `stable.json` is `{"version", "tag", "commit", "files": {<name>: {"sha256", "size"}}}`
for the newest promoted release.

**Every macOS installer the release publishes, hostkeeper's and hello's, is signed but not
notarized.** The release signs each with the Developer ID Installer identity before it is attested
([verify-a-release.md](verify-a-release.md)), and the pointer moves the same signed bytes that
`testing` holds. An MDM installs hostkeeper's as it is, and a host's `macos-pkg` installer stages
hello's only because it is signed; by hand, install either from a terminal with `installer`, after
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

## The native packages and their entries

`hello`'s, `pxe`'s and `mdm`'s native packages (docs/design/packages-native.md, item A21) are the release's
assets `hello_<version>_<arch>.deb`, `hello-<version>-macos-arm64.pkg`, `pxe_<version>_<arch>.deb` and
`mdm_<version>_amd64.deb`,
published with hostkeeper's own in the same passes, so a release is listed whole. hostkeeper's own native
packages (item A23) are the daemon's `hostkeeper_<version>_<arch>.deb` and the installer
`hostkeeper-<version>-macos-arm64.pkg`, the packages a host is set up from.

**hello's failing build** is the same program started with `--fail-ready` (its units pass the flag, so
`/readyz` answers 503), built to prove that a host rolls back to the previous hello. Its service
version is `<version>-fail-ready`, which hello's `/version` serves and its hook compares with the
order's service version; its `.deb` has the Debian version `<version>~fail.ready`. A `~` sorts below
everything, the end of the version included, so apt orders the failing build below the release
`<version>` it is built from and never takes it for, or prefers it over, that release; a version
`<version>-fail-ready` would be the Debian revision `ready` of `<version>-fail` and sort above it.
The release carries the `.deb` of each Linux architecture as the asset `hello_<version>-fail-ready_<arch>.deb`
(an asset name carries the service version, since a hosting service may rename a `~`), attested like
every native package (`hello_<version>-fail-ready_<arch>.deb.sigstore.json`). It is published to the
suite **`testing-fail`** of the same apt repository, which lists that build alone, under
`X.Y.Z~fail.ready`, and nothing else; the pool file is `apt/pool/hostkeeper/hello_<version>~fail.ready_<arch>.deb`,
the name its control file gives. `testing` and `stable` never list it, and it is never promoted. Its
`.pkg` is not released, so no Mac is ordered the failing build. A host that is to be ordered it needs
the repository entry `/etc/hostkeeper/repositories/komira-testing-fail.sources` with `Suites: testing-fail`
and the same keyring as `komira-testing`, which its daemon writes once the coordinator's registry entry
`komira` lists the suite `testing-fail`.

`cargo xtask release targets` (`--service hostkeeper`, the default, and `--service hello`,
`--service mdm` and `--service pxe`) lists, for each platform, the native entry alone (docs/design/packages-native.md,
section 10.5, A24): for each Linux platform an `apt` entry of the host's repository entry `komira-testing`, the `testing` suite
above (`/etc/hostkeeper/repositories/komira-testing.sources`, which each Linux host's daemon writes
from the coordinator's registry entry `komira`, suites `testing` and `stable`:
[repository-keys.md](../operations/repository-keys.md#put-the-repository-in-the-registry); the
coordinator refuses the set (`repository_unknown`) while its registry lacks `komira`), at the release's
version, pinned by the `.deb`'s SHA-256; for `macos-arm64` (hostkeeper and hello; mdm and pxe have no
macOS platform) a `macos-pkg` entry of the signed `.pkg`, located in the store `komira-testing` at
`hostkeeper/<version>/<file>` (the installers' `testing` directory, rooted at `pkg/`) and pinned by
its SHA-256 and size. It writes no `hostkeeper-archive` entry, and the macOS list carries the
`.pkg`'s entry alone, which a Mac whose daemon announces `macos-pkg` (its signers file
`/etc/hostkeeper/macos-pkg.toml` loaded, `/usr/sbin/installer` and `/usr/sbin/pkgutil` present)
takes; a Mac that does not announce it is ordered nothing (`installer_dropped` or
`no_build_for_host`, as the coordinator's pick gives). `--service hello --fail-ready`, given
explicitly and never by default, writes the failing build's entries instead: for each Linux
platform, an `apt` entry of the repository entry `komira-testing-fail` (the suite `testing-fail`), at
the package version `<version>~fail.ready`, pinned by the SHA-256 of
`hello_<version>-fail-ready_<arch>.deb`, with the manifest of the failing build's archive; none for
macOS, where the failing build has no `.pkg`. Without `--fail-ready` the entries are as above, and a
failing build is never among them.

hostkeeper's entries carry the floor of its archive (`min_hostkeeper`, the oldest daemon that reads
the release) and no hook command: its daemon updates itself through its package with built-in steps
(docs/design/packages-native.md, section 10.4). A host whose daemon does not announce `apt`, or has no
such entry, is ordered nothing (`installer_dropped`); stage 9's counts are zero before such a release
is set (docs/operations/move-to-native.md). A host picked an entry before the publish below has put its
package into `testing` defers it (`not_in_repository`) and takes it once it is there.

## Private repositories: a host group's configuration

The repository above is public: every host reads it from `https://packages.komira.ai` itself. A
site's configuration packages (`tierstore-config-web`, say: docs/design/hostgroups.md section 6)
go to a **private repository** instead, which only the coordinator reads, and each host through
the coordinator. A private repository is an entry of the coordinator's registry with
`access = "hosts"` (every entry without `access` is `public`):

```json
{ "repository": { "name": "lab", "kind": "apt", "access": "hosts",
    "uri": "https://config.example.org/apt",
    "credential_file": "/etc/hostkeeper-coordinator/lab.credential",
    "suites": ["web", "db"], "components": ["main"],
    "fingerprints": ["<40 hex>"], "keyring": "<base64>" },
  "reason": "the lab's configuration packages" }
```

- **The origin.** The operator's CI publishes the repository to `uri`, an `https` origin that the
  coordinator can read and hosts cannot, signed with a key of its own (never the public
  repository's), whose fingerprint the entry pins as any entry does. The coordinator sends the
  origin the whole `Authorization` header value that `credential_file` holds (`Bearer <token>` or
  `Basic <base64>`): one line of printable ASCII, at most 4 KiB, a regular file (not a link) of the
  coordinator's user, mode 0600 or 0400. `Repositories.Put` refuses the entry `credential_unusable` when the file is
  not so, and the coordinator reads it again at every read from the origin, so a new credential
  takes effect without a restart. A public entry names no credential file. The credential is never
  sent to a host, logged or answered; `hkctl repositories get` shows the file's path.
- **One suite and one pool per host group**: the suite `dists/<group>/` lists that group's packages
  alone, from the pool `pool/<group>/`, so a host learns nothing of another group's configuration.
  The entry's `suites` are the groups it serves; a release's repository entry names the channel
  `<name>-<group>` (`lab-web`), and `<name>-<group>` is at most 32 bytes. A coordinator that holds
  a host group serves and pushes by the group in the host's enrollment record (a host in none
  reads nothing; docs/operations/hostgroups.md); one that holds none
  uses the role the host's certificate names.
- **What a host reads.** Only through the coordinator, over its mutual-TLS connection with its
  enrolled certificate, on the agent listener (and the LAN listener, which serves the same):
  `GET /repositories/<name>/dists/<group>/...` and `GET /repositories/<name>/pool/<group>/...`,
  for its own group, when the entry's suites list it. Any other path, another group's included, is
  answered 404 `not_found`, as a path that does not exist, and the coordinator logs the refused read
  with the host and the path. The coordinator reads each answer from the origin and passes its bytes
  through unchanged, with the origin's `Content-Length` (an origin that sends none is answered
  502); it parses and signs nothing. An origin that is down, refuses the credential or answers
  anything but 200 or 404 is answered 502 `origin_failed`, and the coordinator logs the
  repository, the path and the origin's status, never the credential.
- **On a Linux host.** The coordinator sends a private entry only to a daemon that announces
  `private-repository-v1`, with the host's group as its one suite. The daemon writes it as
  `/etc/hostkeeper/repositories/<name>-<group>.sources` whose `URIs` is its local mirror,
  `file:/var/lib/hostkeeper/mirror/<name>`. At the `stage` of an order from it, the daemon fetches
  the suite's `InRelease` from the coordinator, then each index file it lists for the host's
  architecture (`binary-<arch>`, `binary-all`, `i18n`), each checked against the size and SHA-256
  that `InRelease` lists, into a new directory that it renames over the mirror whole (directories
  0755, files 0644, which apt's fetch user reads). apt then reads the mirror and checks it as it
  checks any repository: `InRelease` signed by the pinned key, the index's digest, the `.deb`'s
  digest, and the order's pin. The daemon fetches the pinned package's pool file into the mirror
  and checks it against the pin before apt copies it. apt is given no client certificate. A fetch
  whose files do not match the `InRelease` it read (the origin published in between) is fetched
  again once, and then the order defers `fetch_failed`, as it does while the coordinator or the
  origin is down. A group whose suite is not published yet defers `not_in_repository`. When a set
  the host is sent no longer holds the entry, or holds it with another suite (the host moved to
  another group), the daemon deletes the old mirror; a host that is sent no set at all (none of
  its services names a repository it may read) keeps its mirror until it is next sent one.
- **On a Mac.** A configuration `.pkg` is an artifact entry located in the store `coordinator`,
  which every Mac daemon of this release has and no `stores.toml` defines, at the locator
  `repositories/<name>/pool/<group>/<file>.pkg`. The daemon fetches `/` and the locator from the
  coordinator over its connection, under the same rule of its own group's pool, and checks the
  entry's size and SHA-256 before it uses a byte, and the `.pkg`'s signer as for any `.pkg`.
- **Older daemons.** A daemon that does not announce `private-repository-v1` is sent no private
  entry, and an entry of a private repository (or of the store `coordinator`) is `no_target` for
  it; it runs every other package as before.

No package carries a secret, private or not: a private repository is a place for configuration
that is not public, never for credentials (docs/design/hostgroups.md section 6.5).

## Packaging a service

Ship the `.deb` or `.pkg` as you would anyway: the binary where you want it, your maintainer scripts, your unit or
plist. hostkeeper imposes no rule on a package's files, paths, units or scripts; what it checks is the bytes (the
order's SHA-256 pin and the repository's or the package's signature) and that no package of the same name was installed
by hand. Around its own `apt-get install` and `dpkg --purge` it keeps a `postinst` from starting or restarting the unit
through `invoke-rc.d` or `deb-systemd-invoke`, as debhelper's scripts do, until the hand-over (a `policy-rc.d` that
exits 101, present only while hostkeeper runs the package manager); a script that calls `systemctl` itself is not
held back. A `.pkg`
has no such guard: the scripts run when `installer` runs them, before `cutover`, so a `.pkg` whose scripts load its job
needs the built-in steps or a hook that tolerates it.

The hook is named in the release manifest's entry, not in the package. `hook.command[0]` is the absolute path of a file
the package installs, and the elements after it are leading arguments:

```json
{
  "installer": "apt",
  "repository": "komira-stable",
  "package_version": "1.6.0-1",
  "digest": "<64 hex>",
  "manifest": {
    "package": "tierstore",
    "run": ["tierstore.service"],
    "hook": {
      "command": ["/usr/bin/tierstore", "update-hook"],
      "timeout_secs": { "...": "..." }
    }
  }
}
```

hostkeeper runs `/usr/bin/tierstore update-hook <step> [<version dir>] --params <file>` from the sealed copy of that
file in the version it staged, so `prepare` runs the version being installed before it is installed. `stage` refuses
(`manifest_invalid`) a command whose file is not an executable regular file of the package. The contract the hook
keeps (the steps, the exit codes, the params file) is that of
[`crates/hostkeeper-hook`](../../crates/hostkeeper-hook/README.md).

Without a hook command the built-in steps stop, swap and start the `run` units. A `run` unit needs no `[Install]`
section: hostkeeper writes the links of one that has `[Install]` targets, and starts any other, or one whose file is
not in the package, at `cutover` and at every daemon start.

## What the release workflow does

All of these run for the newest release of a run only: when one run releases several versions, the
package channels take the last, since a repository serves the newest version and it carries the
changes of the older ones. Each job is a green no-op until the settings below exist.

1. **Testing.** Every job that signs or puts the release's files first checks them against the
   release's `SHA256SUMS` and checks each asset's attestation by this repository's workflow at the
   release's commit (`cargo xtask release check`), as `verify` did; the workflow lint
   (`crates/xtask/tests/workflows/packages.rs`) holds every such job to those two steps, right
   after the download and before any other verb. For apt, then for rpm, three jobs: `testing-<kind>-read` reads the current index and the
   ETag of each of its objects from the bucket, `testing-<kind>-index` merges the packages into it
   (every other product's entries are kept) and signs it, and `testing-<kind>-write` uploads each
   package once, then the index, with `If-Match` on the ETags that were read. The signing key and the
   cloud role are never in one job. A write that finds the index changed in between (another product's
   release) ends green with `conflict=true`, and attempts 2 and 3 (the jobs with the suffix `-2` and
   `-3`) read, merge and sign again; the third attempt fails instead of asking for a fourth. hello's
   failing build goes into the suite `testing-fail` by three more jobs of the apt kind,
   `testing-fail-apt-read`, `testing-fail-apt-index` and `testing-fail-apt-write` (the same
   environments, settings, key and checks of the release's files, `--suite testing-fail`, the
   artifacts `apt-fail-current` and `apt-fail-next`), in one attempt: its write passes `--last`, so a
   lost race for the index fails that job. They are not stages of `promote-check`: the failing build
   is never promoted, and their result decides nothing about the release. The installers go into `pkg/hostkeeper/<version>/` (`testing-pkg-upload`), each file once: a second put
   of the same bytes is accepted, other bytes at the same key fail.
2. **Checks.** `check-apt` installs each package from the suite `testing` in a clean Ubuntu
   container of each architecture it is built for, `check-rpm` installs each rpm with dnf in a clean
   Fedora container, both images pinned by digest, and each holds the installed system to what the
   package promises: the version its binary says (and the commit, but for `hello`, whose program
   prints its version alone), the files it ships, the user and the units, and that the units are
   enabled or not as the package says (hostkeeper enables the `[Install]` links of a `run` unit
   itself and starts one that nothing enables). `check-pkg` downloads each installer, its bundle and the directory's
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
