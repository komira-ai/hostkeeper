# A service's caller of the CI client: the template and the checklist

A service of another repository that hostkeeper deploys sets each release's desired version
through the CI client, `set-desired.yml` ([The CI client](ci-client.md)), with the release's
targets: for each platform, the `hostkeeper-archive` entry a host installs
([Targets](ci-client.md#targets)). This page is the workflow that does it, ready to copy, and
the checklist of what must be true outside the workflow before its first run.

The template is the shape of this repository's own proof run, `hello-desired.yml`
([docs/hello.md](../hello.md)), for a service named `my-service`: run by hand from `main`, one
job that reads the release and writes its targets, and one job that calls the CI client. The
workflow lint of this repository (`cargo test -p xtask --test workflows`, `template_doc.rs`) reads
the template out of this page and holds it to the rules of `hello-desired.yml` (`callers.rs`) and
to the rules of every workflow (`lint.rs`, `downloads.rs`). So the template always pins the CI
client's current revision: when this repository moves the pin, the template moves in the same
change.

**Archive entries only.** The template gives each platform `hostkeeper-archive` entries and
nothing else. Entries of the repository installer (an `apt` package from a signed repository,
[packages-repositories.md](../design/packages-repositories.md)) are refused by the coordinator
until build item A16 of the packages design ships, and installed by no daemon until A7
([packages.md](../design/packages.md), section 13). This page gains them then.

## The template

Copy it to `.github/workflows/my-service-desired.yml` in the service's repository, then replace
what the checklist names.

```yaml
# Sets the desired version of my-service on the hostkeeper coordinator through the CI client
# (komira-ai/hostkeeper, set-desired.yml), with the OIDC token GitHub issues to this workflow.
# Run by hand, from main, once the release channel holds the release.
name: my-service-desired

on:
  workflow_dispatch:
    inputs:
      version:
        description: The release of my-service whose packages are set, X.Y.Z.
        type: string
        required: true

permissions: {}

defaults:
  run:
    shell: bash

jobs:
  # Reads the release's index, archives and bundles, and writes the targets: one
  # hostkeeper-archive entry per platform the index lists. Holds no token right but
  # `contents: read`, and runs nothing of the release.
  release:
    if: ${{ github.ref == 'refs/heads/main' }}
    runs-on: ubuntu-24.04
    timeout-minutes: 15
    permissions:
      contents: read
    outputs:
      version: ${{ steps.index.outputs.version }}
      release_index: ${{ steps.index.outputs.release_index }}
      release_index_digest: ${{ steps.index.outputs.release_index_digest }}
      targets: ${{ steps.targets.outputs.targets }}
    env:
      VERSION: ${{ inputs.version }}
    steps:
      # A release is X.Y.Z: nothing else reaches the download or the job's outputs.
      - name: Refuse a version that is not X.Y.Z
        run: '[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]'
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: rustup toolchain install
      # One curl, its URL globs expanded by curl itself; a transient failure is retried, and one
      # that still fails ends the job at once, so nothing is set.
      - name: Download the release index, archives and bundles
        run: curl --proto =https --proto-redir =https --tlsv1.2 --fail --fail-early --retry 5 --retry-delay 5 --retry-max-time 300 --retry-all-errors --silent --show-error --location --create-dirs --output-dir "$RUNNER_TEMP/release" --remote-name-all "https://github.com/$GITHUB_REPOSITORY/releases/download/v$VERSION/index.json" "https://github.com/$GITHUB_REPOSITORY/releases/download/v$VERSION/my-service-$VERSION-{linux-arm64,linux-x86_64,macos-arm64}.tar.zst{,.sigstore.json}"
      # The service's own command: checks the index, each archive, bundle and manifest, and
      # appends targets=<JSON> to the output (checklist, item 2).
      - name: The targets of the release
        id: targets
        run: cargo xtask release targets --version "$VERSION" --dir "$RUNNER_TEMP/release" --repo "$GITHUB_REPOSITORY" --output "$GITHUB_OUTPUT"
      # The index's exact bytes: it is one line with no final line break.
      - name: The version, the release index and its SHA-256
        id: index
        run: printf 'version=%s\nrelease_index=%s\nrelease_index_digest=%s\n' "$VERSION" "$(< "$RUNNER_TEMP/release/index.json")" "$(sha256sum "$RUNNER_TEMP/release/index.json" | cut -d ' ' -f 1)" >> "$GITHUB_OUTPUT"

  # Calls the CI client with the targets, the index and its SHA-256, so each host is ordered its
  # own platform's entry, and a daemon before targets the package the index lists.
  set-desired:
    needs: [release]
    if: ${{ github.ref == 'refs/heads/main' }}
    permissions:
      contents: read
      id-token: write
    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@2c767d1842a88edb7120073d2dff3b4bfccc834c # v0.1.184
    with:
      service: my-service
      version: ${{ needs.release.outputs.version }}
      targets: ${{ needs.release.outputs.targets }}
      release_index: ${{ needs.release.outputs.release_index }}
      release_index_digest: ${{ needs.release.outputs.release_index_digest }}
      hostkeeper_version: 0.1.184
      hostkeeper_commit: 2c767d1842a88edb7120073d2dff3b4bfccc834c
      coordinator_url: ${{ vars.HOSTKEEPER_COORDINATOR_URL }}
      coordinator_ca: ${{ vars.HOSTKEEPER_COORDINATOR_CA }}
      audience: ${{ vars.HOSTKEEPER_CI_AUDIENCE }}
      join_tailnet: ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }}
      tailnet_client_id: ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID }}
      tailnet_audience: ${{ vars.HOSTKEEPER_TAILNET_AUDIENCE }}
      tailnet_coordinator: ${{ vars.HOSTKEEPER_TAILNET_COORDINATOR }}
      tailnet_tag: ${{ vars.HOSTKEEPER_TAILNET_TAG }}
```

What the lint holds the template to, as it holds `hello-desired.yml`:

- it runs on `workflow_dispatch` alone, with the required string input `version`, and every job
  has the exact `if: ${{ github.ref == 'refs/heads/main' }}`;
- `set-desired` calls `komira-ai/hostkeeper/.github/workflows/set-desired.yml` pinned to the CI
  client's revision, a full commit, with that release as the comment, and passes the same release
  and commit as `hostkeeper_version` and `hostkeeper_commit`;
- `set-desired` grants exactly `contents: read` and `id-token: write`, passes no secrets, passes
  `tailnet_tag` from the variable `HOSTKEEPER_TAILNET_TAG` alone, and passes `service: my-service`;
- `set-desired` passes `version`, `targets`, `release_index` and `release_index_digest`, each from
  the output of that name of `release`, and no `package_digest`;
- the outputs of `release` are exactly those four, from its steps `targets` and `index`, and its
  step `targets` runs `cargo xtask release targets ...`;
- and every rule of a workflow of this repository: `permissions: {}` at the top, each action
  pinned to a full commit, a GitHub-hosted runner, and each download retried.

## The checklist

Before the first run, in this order. Items 4 to 7 are made once by the coordinator's operator or
the tailnet's admin, not by the service's CI.

1. **The release has its packages.** The service's release `v<X.Y.Z>` carries, for each of
   `linux-x86_64`, `linux-arm64` and `macos-arm64`, the archive
   `my-service-<X.Y.Z>-<platform>.tar.zst` ([The package format](../package-format.md)) and its
   Sigstore bundle `<archive>.sigstore.json`, and the release index `index.json` that lists the
   three archives, each with its size and SHA-256. Name yours differently, or build fewer
   platforms, and change the download step's URLs to match: a platform the index lists has an
   entry, and a host of a platform it does not list is ordered nothing.
2. **The targets command.** The step `targets` runs a command of the service's own repository:
   this repository's `cargo xtask release targets` builds the targets of its own services only,
   and another repository cannot call it. Whatever the command is named, it appends
   `targets=<JSON>` to the file `--output` names, the JSON of [Targets](ci-client.md#targets):
   one object keyed by platform, each value `{"entries": [...]}`, with one `hostkeeper-archive`
   entry for each platform the index lists. The entry carries:
   - the archive's size and SHA-256, as the index lists them;
   - its locations, the store `channel` first (`my-service/<X.Y.Z>/<archive>`) and then `github`
     (`<owner>/<repo>/releases/download/v<X.Y.Z>/<archive>`);
   - its attestation, the bundle `<archive>.sigstore.json`: its size, SHA-256 and the same two
     locations, each with the bundle's file name;
   - the manifest `{"minHostkeeper": "<X.Y.Z>"}`, the `min_hostkeeper` of the archive's own
     `manifest.toml`.

   It refuses, naming every problem, an index of another service or version, an archive of
   another size or SHA-256 than the index lists, a missing bundle, and a manifest it cannot
   read; the job then fails and nothing is set. The entries must agree with the index, or the
   coordinator refuses the set (`index_invalid`).
3. **The channel holds the release.** The release channel stages `my-service`: the channel's
   trust list has a `[[service]]` table naming the service, its repository and the workflow that
   attests its packages ([The trust list](release-channel.md#the-trust-list)), and each host's
   daemon config trusts the same signer in its `[trusted_signers]`: a signer the channel lists
   and the daemons do not stages packages every host refuses to unpack. Run the workflow
   once the channel serves `my-service/<X.Y.Z>/index.json`; before that, the coordinator refuses
   the set (`not_in_channel`) when it checks the channel, and every host fails its fetch when it
   does not.
4. **The coordinator manages the service.** The rollout file has a `[[service]]` table for
   `my-service`, with its roles and its plan ([Rollout plans](../operations/rollout-plans.md));
   otherwise the set is refused (`unknown_service`).
5. **The allow-list entry.** The coordinator's `--oidc-config` file has a `[[remote_operator]]`
   entry for the service's repository ([The allow-list entry](ci-client.md#the-allow-list-entry)):
   its `repository` and `repository_id`, `job_workflow_ref` naming
   `komira-ai/hostkeeper/.github/workflows/set-desired.yml@` and the commit of the template's
   `uses:` line, `refs = ["refs/heads/main"]`, `event_names = ["workflow_dispatch"]`,
   `runner_environment = "github-hosted"`, the `audience` the service passes, and
   `services = ["my-service"]`. Restart the coordinator after the change.
6. **The repository variables.** The service's repository sets `HOSTKEEPER_COORDINATOR_URL`,
   `HOSTKEEPER_COORDINATOR_CA`, `HOSTKEEPER_CI_AUDIENCE`, and `HOSTKEEPER_TAILNET_TAG` to
   `tag:ci`, and for the join `HOSTKEEPER_TAILNET_CLIENT_ID`, `HOSTKEEPER_TAILNET_AUDIENCE` and
   `HOSTKEEPER_TAILNET_COORDINATOR` ([The repository variables](github-actions.md#the-repository-variables)).
   None is a secret. `HOSTKEEPER_TAILNET_TAG` must be set even when the job does not join: the CI
   client checks the tag at its first step, and an empty one ends the job there.
7. **The tailnet.** The tailnet has a federated identity for the service's repository, subject
   `repo:<owner>/<repo>:ref:refs/heads/main`, whose trust credential issues `tag:ci`, and its ACL
   lets `tag:ci` reach the coordinator's private listener
   ([The tag and the ACL](ci-client.md#the-tag-and-the-acl)). Without the join, the coordinator
   is not reachable from a GitHub-hosted runner and the job ends with `reason` `no_answer`.
8. **The first run.** Dispatch the workflow from `main` with a released version. It ends with
   `reason` `converged` once the fleet runs the version (or `not_converged` after half an hour,
   the roll going on). `hkctl releases get-release --service my-service --version <X.Y.Z>`
   answers the targets the coordinator stored. A refusal names its token
   ([When it fails](ci-client.md#when-it-fails)); fix what it names before you run it again.
9. **Moving the pin.** The pin is three values that change together, in one reviewed pull request
   of the service: the commit of the `uses:` line, with its release as the comment, then
   `hostkeeper_version` and `hostkeeper_commit` ([Pinning hostkeeper](ci-client.md#pinning-hostkeeper)).
   The template on this page always shows the CI client's current revision. Add the new commit
   to the allow-list entry's `job_workflow_ref` before the change runs, and keep the old one while
   a run of the old pin may still call.
