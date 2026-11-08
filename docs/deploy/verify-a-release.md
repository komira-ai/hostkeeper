# Verify a release

Every push to `main` of `komira-ai/hostkeeper` that changes code becomes a GitHub release
`v<X.Y.Z>`. X.Y is the content of the `VERSION` file at the root. Z is the number of first-parent
commits on `main` since the commit that last changed `VERSION`, that commit counting as 1. This
page is how a consumer proves that the files of a release are the ones this repository built,
before it uses them.

## The identity

| Part | Value |
|---|---|
| Repository | `komira-ai/hostkeeper` |
| Signer workflow | `komira-ai/hostkeeper/.github/workflows/artifacts.yml` |
| Source ref | `refs/heads/main` |
| Tag | `v<X.Y.Z>` |
| `--version` output | `hostkeeper X.Y.Z+<first 12 hex digits of the commit>`; the coordinator's `hostkeeper-coordinator X.Y.Z+<first 12 hex digits of the commit>`; the MDM kit's `hostkeeper-mdm X.Y.Z+<first 12 hex digits of the commit>`; the command line's `hkctl X.Y.Z+<first 12 hex digits of the commit>`; `hostkeeper-pxe X.Y.Z`, with no commit |

The workflow `artifacts.yml` builds the assets on a push to `refs/heads/main`, writes one
`SHA256SUMS`, and signs build-provenance attestations (sigstore) for every summed asset, plus SBOM
attestations. The workflow `release.yml` publishes those exact files; it never rebuilds or signs.
The signer is therefore `artifacts.yml` on `refs/heads/main`, not the release workflow and not a
tag. A `[trusted_signers]` entry in a daemon config must name `artifacts.yml` on `refs/heads/main`.

## The assets

`<version>` is X.Y.Z. `<platform>` is `linux-x86_64`, `linux-arm64` or `macos-arm64`;
`<linux-platform>` is `linux-x86_64` or `linux-arm64`; `<deb-arch>` is `amd64` or `arm64`;
`<rpm-arch>` is `x86_64` or `aarch64`. The deb, the rpm, the coordinator and the MDM kit's binary
are for the two Linux platforms; the installer pkg is for `macos-arm64`.

| Asset | Content |
|---|---|
| `hostkeeper-<version>-<platform>.tar.gz` | the daemon binary |
| `hostkeeper-<version>-<platform>.cdx.json` | its CycloneDX SBOM |
| `hostkeeper-<version>-<platform>.tar.zst` | the hostkeeper self-update package: `manifest.toml`, `bin/hostkeeper` and, on Linux, `units/hostkeeper.service` (the macOS package has no unit) |
| `hostkeeper-<version>-<platform>.tar.zst.sha256` | the SHA-256 of that package |
| `hello-<version>-<platform>.tar.zst` | the package of `hello`, the example service of `examples/hello`: `manifest.toml`, `bin/hello` and, on Linux, `units/hello@.service`, on macOS `units/hello.a.plist` and `units/hello.b.plist` |
| `hello-<version>-<platform>.tar.zst.sha256` | the SHA-256 of that package |
| `hello-<version>-fail-ready-<platform>.tar.zst` | hello's failing build: the same package at the version `<version>-fail-ready`, whose unit (or plists) start `hello` with `--fail-ready`, so that it never reports ready (the build a host is rolled back from); it differs from the hello package only in the manifest's `version` and the command line its unit or plists start it with |
| `hello-<version>-fail-ready-<platform>.tar.zst.sha256` | the SHA-256 of that package |
| `hostkeeper_<version>_<deb-arch>.deb` | the deb package: the daemon, its unit, its tmpfiles rules, its config template, the pinned `gh` and Sigstore trusted root it checks packages with, `/usr/bin/hostkeeper-pxe` (the boot server of network install, docs/design/network-install.md section 3, with no unit: it runs only when an operator starts it), and the link `/etc/issue.d/hostkeeper.issue` to the console's issue file the daemon writes while it waits for its enrollment |
| `hostkeeper-<version>-1.<rpm-arch>.rpm` | the rpm package, with the same files and link |
| `hostkeeper-coordinator-<version>-<linux-platform>.tar.gz` | the coordinator binary |
| `hostkeeper-coordinator-<version>-<linux-platform>.cdx.json` | its CycloneDX SBOM |
| `hostkeeper-coordinator-<version>-<linux-platform>.image.tar` | the coordinator's container image `hostkeeper-coordinator:<version>`: a tarball that is an OCI image layout and that `docker load` also reads |
| `hostkeeper-mdm-<version>-<linux-platform>.tar.gz` | the MDM kit's binary, `hostkeeper-mdm` (`crates/hostkeeper-mdm`), which sets up and migrates the MDM server stack on its Linux host |
| `hostkeeper-mdm-<version>-<linux-platform>.cdx.json` | its CycloneDX SBOM |
| `hostkeeper-<version>-macos-arm64.pkg` | the installer of a Mac's first install: `/usr/local/bin/hostkeeper` (0755), `/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist` (0644), the log directory `/var/log/hostkeeper` (0755) and its rotation `/etc/newsyslog.d/hostkeeper.conf` (0644), the pinned `gh` at `/usr/local/lib/hostkeeper/gh` (0755) and the Sigstore trusted root at `/usr/local/lib/hostkeeper/trusted_root.json` (0644), owned by root and wheel; a distribution pkg (`productbuild` around the component pkg); the component's one script, a `postinstall`, runs the installed binary's `self-update-seed`, which points `/var/lib/updater/state/hostkeeper/bin/current` (the plist's program) at that binary when it starts nothing, then `mac-config`, which writes `/etc/hostkeeper/hostkeeper.toml` from the MDM's managed preferences unless that file exists, then loads the daemon's job; not signed, attested |
| `index.json` | the release index of the hostkeeper packages: for each of the three platforms the package file, its SHA-256 and size |
| `hello-index.json` | the release index of the hello packages (`service` `hello`, `version` `<version>`) |
| `hello-fail-ready-index.json` | the release index of hello's failing build (`service` `hello`, `version` `<version>-fail-ready`) |
| `hkctl-<version>` | the command line of the coordinator ([docs/cli.md](../cli.md)): one bash script for every platform, generated from the protobuf services, that calls the coordinator's JSON gateway with `curl`; it needs bash 3.2 or later and curl 7.55 or later. No host package carries it |
| `netboot.json` | the boot sets of network install (docs/design/network-install.md, section 5): for each Linux platform the Ubuntu live-server ISO, the shim, the signed network GRUB, the kernel and the initrd a bare machine boots, each with the name it is served as, its size, its SHA-256 and where it comes from, rendered from `packaging/netboot/pins.toml` (`cargo xtask release netboot`) |
| `SHA256SUMS` | the SHA-256 of every asset except itself and the bundles |
| `<asset>.sigstore.json` | the attestation bundle of each `.tar.gz`, `.tar.zst`, `.deb`, `.rpm`, `.image.tar` and `.pkg`, of `netboot.json` and of `hkctl-<version>` |

Each index has the same schema. An index is a lookup, not a trust anchor: it has no bundle of its
own, and a host trusts a package for its own attestation and manifest, never for its entry in the
index. (`SHA256SUMS` lists every index, so the provenance of a release covers their bytes too.)
Every `.tar.zst` is attested, hello's packages included: a host takes no unsigned package.
`netboot.json` is attested with a bundle of its own, unlike the indexes: it is the trust anchor of
the boot files it pins, which come from Canonical, not from this release. `hostkeeper-pxe fetch`
(item N8 of docs/design/network-install.md, `crates/hostkeeper-pxe`) verifies that attestation
before it reads a pin in it.

The coordinator's image runs `/usr/bin/hostkeeper-coordinator` as its entrypoint, as the
distroless `nonroot` user, configured by its flags alone (`packaging/container/Dockerfile`). It
also holds the operator web app (`ui/` of the commit) at `/usr/share/hostkeeper-coordinator/ui`,
owned by root, which the coordinator serves under `/ui/` when given no `--ui-dir`; `cargo xtask
release check-image-ui` refuses an image that does not hold it file for file. It is
built twice from clean contexts and released only when the two builds are byte for byte equal, so
anyone can rebuild it from the commit and compare; before it is released it is also loaded and run
through its entrypoint with `--version`, which must print the coordinator's stamp.

A release holds the assets of all three platforms; `cargo xtask release check` refuses one that
lacks any of them, and each index lists the package of each. The pkg carries no installer
signature (that needs an Apple Developer ID Installer identity, which this project does not hold):
verify it by its attestation, steps 3 and 4, like any other asset. Names are only ever added,
never renamed or removed, so a consumer ignores assets it does not know.

### Every name

The whole list. A line with placeholders stands for one name per value of them, the placeholders
of one line taken together in the order the sentence above gives their values: a `<platform>`
line for each of the three platforms, a `<linux-platform>`, `<deb-arch>` or `<rpm-arch>` line for
each Linux platform. The release tooling produces exactly these names (`cargo xtask release
check` refuses a directory that holds any other), and a test keeps this list equal to them.

```text
SHA256SUMS
index.json
hello-index.json
hello-fail-ready-index.json
netboot.json
netboot.json.sigstore.json
hkctl-<version>
hkctl-<version>.sigstore.json
hostkeeper-<version>-<platform>.tar.gz
hostkeeper-<version>-<platform>.tar.gz.sigstore.json
hostkeeper-<version>-<platform>.cdx.json
hostkeeper-<version>-<platform>.tar.zst
hostkeeper-<version>-<platform>.tar.zst.sigstore.json
hostkeeper-<version>-<platform>.tar.zst.sha256
hostkeeper_<version>_<deb-arch>.deb
hostkeeper_<version>_<deb-arch>.deb.sigstore.json
hostkeeper-<version>-1.<rpm-arch>.rpm
hostkeeper-<version>-1.<rpm-arch>.rpm.sigstore.json
hostkeeper-coordinator-<version>-<linux-platform>.tar.gz
hostkeeper-coordinator-<version>-<linux-platform>.tar.gz.sigstore.json
hostkeeper-coordinator-<version>-<linux-platform>.cdx.json
hostkeeper-coordinator-<version>-<linux-platform>.image.tar
hostkeeper-coordinator-<version>-<linux-platform>.image.tar.sigstore.json
hostkeeper-mdm-<version>-<linux-platform>.tar.gz
hostkeeper-mdm-<version>-<linux-platform>.tar.gz.sigstore.json
hostkeeper-mdm-<version>-<linux-platform>.cdx.json
hostkeeper-<version>-macos-arm64.pkg
hostkeeper-<version>-macos-arm64.pkg.sigstore.json
hello-<version>-<platform>.tar.zst
hello-<version>-<platform>.tar.zst.sigstore.json
hello-<version>-<platform>.tar.zst.sha256
hello-<version>-fail-ready-<platform>.tar.zst
hello-<version>-fail-ready-<platform>.tar.zst.sigstore.json
hello-<version>-fail-ready-<platform>.tar.zst.sha256
```

## What the deb and the rpm check packages with

A host checks the attestation of every package before it installs it, its first deploy of a
service included, with `gh attestation verify` run offline (the commands of step 4). The deb and
the rpm carry the two files that check runs on, at the paths the daemon reads
(`crates/hostkeeper-daemon/src/cli.rs`, `GH_BINARY` and `SIGSTORE_TRUSTED_ROOT`):

| path | mode | what |
|---|---|---|
| `/usr/lib/hostkeeper/gh` | 0755 | `gh` of the version of the workflow-level `GH_VERSION` of `.github/workflows/artifacts.yml` (the macOS job pins its own), for the package's architecture, taken from cli/cli's release archive `gh_<version>_linux_<deb-arch>.tar.gz` |
| `/usr/lib/hostkeeper/trusted_root.json` | 0644 | the Sigstore trusted root, as `gh attestation trusted-root` printed it when the release was built: one JSON line for the Sigstore public-good instance and one for GitHub's |

Both are owned by root. The distribution's own `gh` will not do: Ubuntu 24.04 ships gh 2.46.0,
which has no `gh attestation`. A host without these two files cannot check any package, and every
check ends `attestation_unavailable`.

Where they come from. The build job of `artifacts.yml` downloads the `gh` archive of each
architecture and checks it against its pinned SHA-256 (`gh-sha256` in the job's matrix, the digest
cli/cli publishes in `gh_<version>_checksums.txt`) before anything is unpacked. It then runs that
`gh` as `gh attestation trusted-root`, which fetches both trusted roots through their TUF
repositories and verifies them from the TUF root built into `gh`. `cargo xtask release
nfpm-stage` takes `gh` out of the same archive, checks the archive against the pin again, refuses a
`gh` that is not a program of the package's architecture and a trusted root with a line that is
not one, and writes both into `target/nfpm/`, where `packaging/linux/nfpm.yaml` takes them from.
A missing file or a digest that differs from the pin fails the build, and no deb or rpm is made.
After nfpm has built them, `cargo xtask release check-linux-pkg` reads the deb and the rpm back
(the deb's `data.tar.gz`; the rpm's header and payload) and fails the build unless each installs
both files at these paths, a regular file of exactly this mode, owned by root and group root,
with the bytes staged, and installs `/etc/issue.d/hostkeeper.issue` as a link to
`/run/hostkeeper/enrollment.issue`, the console's issue file the daemon writes.

Bumping the pin: set the workflow-level `GH_VERSION` in `artifacts.yml` to the new release and both `gh-sha256`
values of the build job's matrix to the SHA-256s of `gh_<version>_linux_amd64.tar.gz` and
`gh_<version>_linux_arm64.tar.gz` from that release's `gh_<version>_checksums.txt`, in one commit.

The trusted root is captured anew at each build, so each deb and rpm carries the trusted root of
the day it was built. hostkeeper's self-update does not replace these files, but hostkeeper's own
package carries the committed root too, and the daemon uses whichever copy was written last (see
[The trusted root on a host](#the-trusted-root-on-a-host)). A stale trusted root keeps verifying
bundles signed under the keys it holds. When Sigstore starts signing with a certificate authority
or a log key the host's trusted root does not hold, every bundle signed after that is refused on
that host, as not attested by the trusted signer, until a newer root reaches it: an update of
hostkeeper made while the old root still verifies it, a newer deb or rpm, or a copy by hand.

## Steps

`$PIN` is the full 40-hex commit the consumer pinned. Run the steps in order; any refusal stops
the consumer from using the release.

### 1. Download

```sh
gh release download v<X.Y.Z> --repo komira-ai/hostkeeper --dir release
```

To fetch only what is needed, add `--pattern <asset>` for each asset and `--pattern SHA256SUMS`.

What it proves: nothing. The release channel is not trusted; the next steps decide.

Refuse when: the download fails or `SHA256SUMS` is absent.

### 2. Checksums

```sh
cd release && sha256sum --check --strict --ignore-missing SHA256SUMS
```

`--ignore-missing` is only for a partial download, where `SHA256SUMS` lists files that were not
fetched. After a full download, drop it, so that a missing file is an error.

What it proves: each downloaded file has the bytes that `SHA256SUMS` lists.

Refuse when: any file fails its checksum, or a line of `SHA256SUMS` is malformed (`--strict`).
`SHA256SUMS` is not itself attested, so this step is a transport check only: it proves the files
are the ones the list names, not that the list came from this repository. Step 3 is what ties
each file to this repository.

### 3. Attestation, online

Run once per asset you use: every asset `SHA256SUMS` lists is a subject of the release's
provenance attestation.

```sh
gh attestation verify release/<asset> \
  --repo komira-ai/hostkeeper \
  --signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml \
  --source-ref refs/heads/main \
  --source-digest "$PIN" \
  --deny-self-hosted-runners
```

- `--repo` requires that the attested repository is `komira-ai/hostkeeper`.
- `--signer-workflow` requires that the signing workflow is `artifacts.yml`, so a different
  workflow in the same repository cannot sign an asset.
- `--source-ref` requires `refs/heads/main`, so a tag, another branch or a pull request cannot.
- `--source-digest` requires that the attested commit equals the pin.
- `--deny-self-hosted-runners` requires that the build ran on a GitHub-hosted runner.

What it proves: a build of commit `$PIN` on `main` of this repository, by `artifacts.yml` on a
hosted runner, produced a file with this digest.

Refuse when: no attestation exists; the signer workflow, the ref, the repository or the commit
differs; the runner is self-hosted.

### 4. Attestation, offline

For a host without access to the GitHub API. Once, on a host that has it:

```sh
gh attestation trusted-root > trusted_root.jsonl
```

This saved root is only for checking a release by hand. A hostkeeper host needs no such step: its
root ships in the packages (see [The trusted root on a host](#the-trusted-root-on-a-host)).

Then, for each `.tar.gz`, `.tar.zst`, `.deb`, `.rpm`, `.image.tar` and `.pkg`, and `netboot.json`,
you use, with the bundle released next to it (the other assets have no bundle and are verified online, step 3):

```sh
gh attestation verify release/<asset> \
  --bundle release/<asset>.sigstore.json \
  --custom-trusted-root trusted_root.jsonl \
  --repo komira-ai/hostkeeper \
  --signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml \
  --source-ref refs/heads/main \
  --source-digest "$PIN" \
  --deny-self-hosted-runners
```

What it proves: the same as step 3, checked against the saved trusted root and the bundle instead
of the API.

Refuse when: the same as step 3, or the bundle is absent.

### 5. SBOM attestation (optional)

The SBOM is attested as well. Verify the binary tarball (the daemon's, the coordinator's
`hostkeeper-coordinator-<version>-<linux-platform>.tar.gz`, or the MDM kit's
`hostkeeper-mdm-<version>-<linux-platform>.tar.gz`) with the same identity flags as step 3 and
with `--predicate-type` set to the CycloneDX predicate type that `attest-sbom` records:

```sh
gh attestation verify release/hostkeeper-<version>-<platform>.tar.gz \
  --predicate-type <the CycloneDX predicate type> \
  --repo komira-ai/hostkeeper \
  --signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml \
  --source-ref refs/heads/main \
  --source-digest "$PIN" \
  --deny-self-hosted-runners
```

What it proves: the `.cdx.json` listing was produced for that file by the same signer.

Refuse when: as step 3.

### 6. The binary says its version and commit

```sh
tar -xzf release/hostkeeper-<version>-<platform>.tar.gz
./hostkeeper --version
```

Expected output, exactly: `hostkeeper <X.Y.Z>+<first 12 hex digits of $PIN>`.

For the coordinator, unpack `hostkeeper-coordinator-<version>-<linux-platform>.tar.gz` and run
`./hostkeeper-coordinator --version`; its output is, exactly,
`hostkeeper-coordinator <X.Y.Z>+<first 12 hex digits of $PIN>`.

For the MDM kit, unpack `hostkeeper-mdm-<version>-<linux-platform>.tar.gz` and run
`./hostkeeper-mdm --version`; its output is, exactly,
`hostkeeper-mdm <X.Y.Z>+<first 12 hex digits of $PIN>`.

For the command line, `hkctl-<version>` is one bash script for every platform, with nothing to
unpack. Make it executable under its own name and run it:

```sh
install -m 0755 release/hkctl-<version> ./hkctl
./hkctl --version
```

Expected output, exactly: `hkctl <X.Y.Z>+<first 12 hex digits of $PIN>`.

What it proves: the binary was built from the pinned commit and reports the version of the tag.

Refuse when: the output differs in any character.

### 7. Release index digest for desired state

```sh
grep "  <service>-index.json$" release/SHA256SUMS
sha256sum release/<service>-index.json
cat release/hostkeeper-<version>-<platform>.tar.zst.sha256
sha256sum release/hostkeeper-<version>-<platform>.tar.zst
```

(`index.json` for the hostkeeper packages; `<service>` is `hello` for the hello packages.)

The digest of the index in the first two outputs is equal, and the index lists, for
`<platform>`, the package file and the digest in the last two outputs, which are equal. These
are what CI puts in the desired state with the version (design section 4.7): the index text and
its SHA-256, so that each host is ordered the package of its own platform. The coordinator
checks the index against its digest and keeps each platform's package digest; the daemon checks
the index it fetches against the digest before it reads it, then the package against the
index, and checks an attestation for the package, before it reads the archive. A fleet of one
platform may put the one package digest in the desired state instead.

What it proves: the digest placed in desired state is the digest of the verified index, and the
index lists the attested package.

Refuse when: the digests differ, the index lists another digest for the package than the package
has, or the package did not pass step 3 or step 4.

## What the release workflow runs

Before anything is published, the `verify` job of `release.yml`, on the files of the commit's
`artifacts.yml` run:

- runs step 2 as `sha256sum --check --strict SHA256SUMS`: the download is whole, so without
  `--ignore-missing`;
- runs `cargo xtask release check`, which holds the files to the names above, and for every
  attested file runs the command of step 3 and the command of step 4 with `$PIN` the commit
  released. Step 4 runs without `--custom-trusted-root`: the runner fetches the trusted root
  itself. Each command gets `--format json` added, so that the attestations it verified are read
  back, and each must name this commit and the file's SHA-256;
- runs step 6 on the linux-x86_64 binaries, the daemon's, the coordinator's and the MDM kit's
  (`cargo xtask release check-binary`); `cargo xtask release check` holds `hkctl-<version>` to the
  expected output of step 6 too, by its one version line, without running it.

The test `verify_a_release_commands_match_the_ones_the_verify_job_runs`
(`cargo test -p xtask`, in `crates/xtask/src/release/doc_tests.rs`) keeps these commands equal to
the ones written here, and `asset_names_are_the_documented_contract` keeps the list of every name
equal to the names the tooling produces.

The CI client, `.github/workflows/set-desired.yml` ([The CI client](ci-client.md)), runs steps 2
and 3 and hkctl's `--version` check of step 6 as written here, on `hkctl-<version>`. The workflow
lint (`cargo test -p xtask --test workflows`) refuses a change to those commands, to the tag or to
hkctl's asset name in this document that the workflow does not follow, and the reverse.

## The trusted root on a host

The daemon checks every package's attestation offline, as step 4 does, against a Sigstore
trusted root on the host. Nobody runs the command of step 4 on a host. The root comes with the
packages:

- The release commits the root, as `gh attestation trusted-root` prints it, at
  `packaging/sigstore/trusted_root.json`.
- hostkeeper's own package (`hostkeeper-<version>-<platform>.tar.zst`) carries that file as
  `files/trusted_root.json`. The daemon reads it from
  `/var/lib/hostkeeper/packages/hostkeeper/current/files/trusted_root.json`, so each update of
  hostkeeper brings the root of its release.
- The host's own copy is at `/usr/lib/hostkeeper/trusted_root.json` on Linux, where the deb and
  the rpm put it ([What the deb and the rpm check packages with](#what-the-deb-and-the-rpm-check-packages-with)),
  and at `/usr/local/lib/hostkeeper/trusted_root.json` on a Mac, where the installer pkg puts it. It is
  the root of a host before its first update of hostkeeper, and the place to copy a root by hand.

When both files are present, the daemon uses the one written last; a tie in the same second goes
to the packaged one. A root an operator copies after a rotation therefore takes over from the
one an older package carried. The choice goes by when each file reached the host, not by what
it holds. A later update or rollback of hostkeeper to a version that carries an older root
brings that root back into use, and so does a copy that keeps its source's time (`cp -p`,
`rsync -t`). Copy with plain `cp` or `install`, and copy again after such a rollback. When
neither file is present, `gh` is given the host's path and the check fails there.

A key that Sigstore rotates reaches the hosts with the next release made after the committed root
changes. That release is checked against the root the hosts already hold, so the update has to
land while the old root still verifies it. The scheduled workflow `trusted-root.yml` fetches
the live root once a day and fails when its bytes differ from the committed file. The failed run
is the notice to commit the new root, and it keeps the live root as its artifact `trusted-root`.
The comparison is of bytes, so a change in how the runner's `gh` writes the same root also turns
the run red. The run prints the `gh` version, and the fix is the same: commit the live output.

The admin `Status` reports the root the daemon uses:

- `trusted_root_age_secs`: the seconds since that file was last written on the host (the format
  has no field for when the root was made). It is absent when there is no file.
- `trusted_root_reason`: one token, or empty when the root needs no attention:

| reason | when |
|---|---|
| `trusted_root_stale` | the root is older than `--trusted-root-max-age-days` (default 90): the host has not taken an update of hostkeeper in that time |
| `trusted_root_missing` | neither file is present |
| `trusted_root_unparseable` | the file cannot be read, or is not one or more JSON trusted roots; this wins over `trusted_root_stale` |

The reason is a signal only. A stale root does not stop a package check: the check goes on with
the root there is, and fails only if that root no longer verifies the package.

## Refuse

Do not use a release when any of these holds:

- a checksum is wrong;
- an attestation is missing;
- the signer workflow is not `artifacts.yml`;
- the ref is not `refs/heads/main` (a tag, a branch, a pull request);
- the source commit differs from the pin;
- the build ran on a self-hosted runner;
- the binary's `--version` differs from `hostkeeper <X.Y.Z>+<first 12 hex digits of $PIN>`
  (`hostkeeper-coordinator <X.Y.Z>+<first 12 hex digits of $PIN>` for the coordinator,
  `hostkeeper-mdm <X.Y.Z>+<first 12 hex digits of $PIN>` for the MDM kit).
