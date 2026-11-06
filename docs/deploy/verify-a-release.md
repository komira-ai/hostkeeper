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
| `--version` output | `hostkeeper X.Y.Z+<first 12 hex digits of the commit>`; the coordinator's `hostkeeper-coordinator X.Y.Z+<first 12 hex digits of the commit>` |

The workflow `artifacts.yml` builds the assets on a push to `refs/heads/main`, writes one
`SHA256SUMS`, and signs build-provenance attestations (sigstore) for every summed asset, plus SBOM
attestations. The workflow `release.yml` publishes those exact files; it never rebuilds or signs.
The signer is therefore `artifacts.yml` on `refs/heads/main`, not the release workflow and not a
tag. A `[trusted_signers]` entry in a daemon config must name `artifacts.yml` on `refs/heads/main`.

## The assets

`<version>` is X.Y.Z. `<platform>` is `linux-x86_64`, `linux-arm64` or `macos-arm64`;
`<linux-platform>` is `linux-x86_64` or `linux-arm64`; `<deb-arch>` is `amd64` or `arm64`;
`<rpm-arch>` is `x86_64` or `aarch64`. The deb, the rpm and the coordinator are for the two Linux
platforms; the installer pkg is for `macos-arm64`.

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
| `hostkeeper_<version>_<deb-arch>.deb` | the deb package |
| `hostkeeper-<version>-1.<rpm-arch>.rpm` | the rpm package |
| `hostkeeper-coordinator-<version>-<linux-platform>.tar.gz` | the coordinator binary |
| `hostkeeper-coordinator-<version>-<linux-platform>.cdx.json` | its CycloneDX SBOM |
| `hostkeeper-coordinator-<version>-<linux-platform>.image.tar` | the coordinator's container image `hostkeeper-coordinator:<version>`: a tarball that is an OCI image layout and that `docker load` also reads |
| `hostkeeper-<version>-macos-arm64.pkg` | the installer of a Mac's first install: `/usr/local/bin/hostkeeper` (0755), `/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist` (0644), the log directory `/var/log/hostkeeper` (0755) and its rotation `/etc/newsyslog.d/hostkeeper.conf` (0644), owned by root and wheel, no scripts; not signed, attested |
| `index.json` | the release index of the hostkeeper packages: for each of the three platforms the package file, its SHA-256 and size |
| `hello-index.json` | the release index of the hello packages (`service` `hello`, `version` `<version>`) |
| `hello-fail-ready-index.json` | the release index of hello's failing build (`service` `hello`, `version` `<version>-fail-ready`) |
| `SHA256SUMS` | the SHA-256 of every asset except itself and the bundles |
| `<asset>.sigstore.json` | the attestation bundle of each `.tar.gz`, `.tar.zst`, `.deb`, `.rpm`, `.image.tar` and `.pkg` |

Each index has the same schema. An index is a lookup, not a trust anchor: it has no bundle of its
own, and a host trusts a package for its own attestation and manifest, never for its entry in the
index. (`SHA256SUMS` lists every index, so the provenance of a release covers their bytes too.)
Every `.tar.zst` is attested, hello's packages included: a host takes no unsigned package.

The coordinator's image runs `/usr/bin/hostkeeper-coordinator` as its entrypoint, as the
distroless `nonroot` user, configured by its flags alone (`packaging/container/Dockerfile`). It is
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
hostkeeper-<version>-macos-arm64.pkg
hostkeeper-<version>-macos-arm64.pkg.sigstore.json
hello-<version>-<platform>.tar.zst
hello-<version>-<platform>.tar.zst.sigstore.json
hello-<version>-<platform>.tar.zst.sha256
hello-<version>-fail-ready-<platform>.tar.zst
hello-<version>-fail-ready-<platform>.tar.zst.sigstore.json
hello-<version>-fail-ready-<platform>.tar.zst.sha256
```

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

Then, for each `.tar.gz`, `.tar.zst`, `.deb`, `.rpm`, `.image.tar` and `.pkg` you use, with the
bundle released next to it (the other assets have no bundle and are verified online, step 3):

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

The SBOM is attested as well. Verify the binary tarball (the daemon's, or the coordinator's
`hostkeeper-coordinator-<version>-<linux-platform>.tar.gz`) with the same identity flags as step 3 and
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

What it proves: the binary was built from the pinned commit and reports the version of the tag.

Refuse when: the output differs in any character.

### 7. Package digest for desired state

```sh
cat release/hostkeeper-<version>-<platform>.tar.zst.sha256
sha256sum release/hostkeeper-<version>-<platform>.tar.zst
```

The digest in the first output equals the digest of the package that step 3 (or step 4) verified.
This SHA-256 is what CI puts in the desired state with the version (design section 4.7); the daemon
checks the package against it, and checks an attestation for it, before it reads the archive.

What it proves: the digest placed in desired state is the digest of the attested package.

Refuse when: the two digests differ, or the package did not pass step 3 or step 4.

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
- runs step 6 on the linux-x86_64 binaries, the daemon's and the coordinator's
  (`cargo xtask release check-binary`).

The test `verify_a_release_commands_match_the_ones_the_verify_job_runs`
(`cargo test -p xtask`, in `crates/xtask/src/release/doc_tests.rs`) keeps these commands equal to
the ones written here, and `asset_names_are_the_documented_contract` keeps the list of every name
equal to the names the tooling produces.

The CI client, `.github/workflows/set-desired.yml` ([The CI client](ci-client.md)), runs steps 2
and 3 and the `--version` check of step 6 as written here, on the binary tarball for
`linux-x86_64`. The workflow lint (`cargo test -p xtask --test workflows`) refuses a change to
those commands, to the tag or to the binary tarball's name in this document that the workflow
does not follow, and the reverse.

## Refuse

Do not use a release when any of these holds:

- a checksum is wrong;
- an attestation is missing;
- the signer workflow is not `artifacts.yml`;
- the ref is not `refs/heads/main` (a tag, a branch, a pull request);
- the source commit differs from the pin;
- the build ran on a self-hosted runner;
- the binary's `--version` differs from `hostkeeper <X.Y.Z>+<first 12 hex digits of $PIN>`
  (`hostkeeper-coordinator <X.Y.Z>+<first 12 hex digits of $PIN>` for the coordinator).
