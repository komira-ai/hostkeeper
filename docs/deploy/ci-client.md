# The CI client: setting a service's desired version from its release job

A service ships through hostkeeper by one step at the end of its own release job: it tells the
coordinator the new version and the digest of its package, then waits for the fleet to reach it.
That step is the reusable workflow `.github/workflows/set-desired.yml` of this repository. A
service calls it; it does not write the step itself.

The workflow does the steps of the design's CI client (section 4.5), in this order. Steps 2, 3 and
6 run the checks of [Verify a release](verify-a-release.md) (steps 2, 3 and 6 there), with
`--format json` added to the attestation check so that step 4 can read it; step 1 downloads with
`curl` instead of `gh release download`, and step 4 is a further check of the attested commit.
The workflow lint keeps steps 2 and 3 equal to the commands of that page, word for word, step 6
equal to its `--version` check and expected output, and the downloaded asset and tag equal to
its tables: a change to either file alone is refused.

1. Download the pinned hostkeeper release's binary tarball for `linux-x86_64` and its
   `SHA256SUMS`.
2. Check the downloaded files against `SHA256SUMS`.
3. Verify the tarball's attestation: signed by `.github/workflows/artifacts.yml` of
   `komira-ai/hostkeeper` on a GitHub-hosted runner, for a build of `refs/heads/main` at the pinned
   `hostkeeper_commit`.
4. Compare the commit every verified attestation names as the build's source with the pinned
   `hostkeeper_commit`.
5. Unpack the verified `hostkeeper`.
6. Check that `hostkeeper --version` prints `hostkeeper <hostkeeper_version>+<the first 12 hex
   digits of hostkeeper_commit>`, exactly.
7. Only then run the verified program's CI client, `hostkeeper ci set-desired`, as one command.
   It requests an OIDC token for the coordinator, writes it to a file only the runner can read,
   and calls `hostkeeper ctl releases set-desired` with it. When `wait` is true it then reads
   `hostkeeper ctl releases converge` until the fleet has converged, is halted, or the call is
   refused, at most `max_polls` times, and writes the job's outputs.

**A failed verification means the version is not set.** Steps 1 to 4 run nothing of hostkeeper's
and no step before step 7 can request a token: the workflow lint
(`cargo test -p xtask --test workflows`) refuses the file otherwise. A version is set only by a
program the job has proven was built from the commit you pinned.

## Before it runs: the release must be in the channel

A host fetches the version from the release channel (`{release_channel}/S/V/`), not from GitHub.
The order is:

1. the service's release workflow publishes the signed GitHub release;
2. `cargo xtask release channel-sync` on the channel's host stages it into the channel
   ([release-channel.md](release-channel.md#sync-new-releases-into-the-channel)), within one
   period of its timer;
3. then this workflow sets the desired version.

A version set before the channel holds it is not refused by the coordinator: every host ordered
to it fails its fetch (`fetch_failed`) and retries until the channel has it. Call this workflow
once the version is in the channel: from a job that runs after the sync (by hand, or after a
check that `S/V/index.json` is served), not straight after the release is published.

## Calling it

In the service's release workflow, after the job that publishes the package:

```yaml
jobs:
  set-desired:
    needs: publish
    permissions:
      contents: read
      id-token: write
    uses: komira-ai/hostkeeper/.github/workflows/set-desired.yml@0123456789abcdef0123456789abcdef01234567 # v0.1.0
    with:
      service: my-service
      version: ${{ needs.publish.outputs.version }}
      package_digest: ${{ needs.publish.outputs.package_sha256 }}
      hostkeeper_version: 0.1.0
      hostkeeper_commit: 0123456789abcdef0123456789abcdef01234567
      coordinator_url: https://coordinator.example
      coordinator_ca: ${{ vars.HOSTKEEPER_COORDINATOR_CA }}
      audience: my-service-release
```

The calling job grants `id-token: write` and `contents: read`, and nothing else. The workflow
takes no secrets: the OIDC token is its only credential, and GitHub issues it to the job.

The coordinator must hold an allow-list entry for the calling repository and workflow that names
the service and the audience you pass (design, section 5.1: a CI token sets the desired version
only of the services its entry names).

## Inputs

| input | type | required | default | meaning |
|---|---|---|---|---|
| `service` | string | yes |  | The service whose desired version is set, as the coordinator names it. |
| `version` | string | yes |  | The version of the service to set. |
| `package_digest` | string | yes |  | The SHA-256 of the service's package for this version, 64 lower-case hex digits. Every host checks the package it fetches against it. |
| `hostkeeper_version` | string | yes |  | The hostkeeper release whose program makes the calls. |
| `hostkeeper_commit` | string | yes |  | The commit of `komira-ai/hostkeeper` that release was built from, 40 lower-case hex digits. |
| `coordinator_url` | string | yes |  | The coordinator's private listener, as `https://<host>[:<port>]`. |
| `coordinator_ca` | string | yes |  | The PEM certificates of the CA the coordinator's certificate must chain to. No other root is trusted. |
| `audience` | string | yes |  | The audience of the OIDC token, as the coordinator's allow-list entry for the calling repository names it. |
| `wait` | boolean | no | `true` | Whether to wait for the fleet to converge on the version. |
| `max_polls` | number | no | `180` | The most convergence reads before the job fails, from 1 to 1000. One read every 10 seconds, so the default waits about half an hour. |

## Outputs

| output | meaning |
|---|---|
| `desired_version` | The desired version the coordinator stored; empty when it stored none. |
| `converged` | `true` when the fleet converged on the version, `false` otherwise. |
| `reason` | Why the job ended where it did, one token: `converged`, `not_waited`, `halted`, `superseded`, `not_converged`, `not_stored`, `no_answer`, or the refusal token the coordinator gave (for example `held_bad`). Empty when a verification step failed or a flag was refused (exit 2); `no_answer` also when the client could not get a token or write its files. |

## Pinning hostkeeper

Pin three things, and change them together, in a reviewed pull request of the service:

- the `uses:` line, to the full commit of this repository whose `set-desired.yml` you call, with
  the release as its comment;
- `hostkeeper_version`, the release the job downloads;
- `hostkeeper_commit`, the commit of `komira-ai/hostkeeper` that release was built from.

Why: the downloaded hostkeeper program holds your OIDC token. A pin means a new hostkeeper release,
or a moved tag, cannot change what runs with that token until you change the pin. The commit is
what the attestation proves: the job refuses a release whose attestation names any other source
commit, so a release replaced after you pinned it is refused, not run.

## The calls

The client (`crates/hostkeeper-ci`) runs ctl inside its own process, so no token is
ever an argument of a process. Its two calls are these commands, with the token in a file:

```sh
hostkeeper ctl --endpoint "$COORDINATOR_URL" --ca-file "$CA_FILE" --token-file "$TOKEN_FILE" --output json \
  releases set-desired --service "$SERVICE" --package-digest "$PACKAGE_DIGEST" --request "$REQUEST_FILE"
```

where the request file is `{"desired":{"version":"<version>"}}` (the version is set through
the request file because `DesiredState` is a message: see `docs/ctl.md`). Then, once per poll,
with a new token each time (the coordinator accepts each token once):

```sh
hostkeeper ctl --endpoint "$COORDINATOR_URL" --ca-file "$CA_FILE" --token-file "$TOKEN_FILE" --output json \
  releases converge --service "$SERVICE"
```

Each token is requested from GitHub for the `audience` input, masked in the log, written to
`hostkeeper-oidc-token` in the runner's temporary directory with mode 0600, and removed after its
call. It is never an argument (another process on the runner can read a process's arguments) and
never an environment variable.

The first read comes right after the version is set; each later one 10 seconds after the one
before. The client prints what a read reports, as one line (the state, the desired version, and
the hosts of each role at the version, updating, failed and down), when it differs from the line
it printed last: a run of identical reads is printed once, and a state seen again after another
one is printed again. A read that gets no answer is printed as `no answer` the same way, and
counts as one of the `max_polls` reads.

## When it fails

| step that failed | what it means | what to do |
|---|---|---|
| Download the hostkeeper release | No release `v<hostkeeper_version>`, or it has no tarball for `linux-x86_64` or no `SHA256SUMS`. The version is not set. | Check the pin against the releases of `komira-ai/hostkeeper`. |
| Check the release against SHA256SUMS | A downloaded file does not have the bytes `SHA256SUMS` lists. The version is not set. | Do not retry with another file. Report it to the hostkeeper maintainers. |
| Verify the attestation of the tarball | The tarball has no attestation from `artifacts.yml` of `komira-ai/hostkeeper` on `refs/heads/main`, built on a GitHub-hosted runner from `hostkeeper_commit`. The version is not set. | Check that `hostkeeper_commit` is the commit of `hostkeeper_version`. If it is, the release is not the one that was built: report it. |
| Compare the attested source commit | An attestation names another source commit than `hostkeeper_commit` (`jq` prints `false`). The version is not set. | As above. |
| Unpack the verified hostkeeper | The verified tarball holds no `hostkeeper`. The version is not set. | Report it. |
| Check the version of the verified hostkeeper | The program does not say `hostkeeper <hostkeeper_version>+<first 12 hex digits of hostkeeper_commit>`. The version is not set. | Check the two pins belong together. |
| Set the desired version and wait for the fleet: before the call | An input breaks its rule (exit 2): a service name or version with other characters, a digest that is not 64 lower-case hex digits, a URL that is not `https://`, a `max_polls` outside 1 to 1000. Or GitHub issued no token (`reason` is `no_answer`): the calling job does not grant `id-token: write`. The version is not set. | Fix the input, or grant `id-token: write` to the calling job. |
| Set the desired version and wait for the fleet: the set | The coordinator refused the call, and `reason` is its token: `unauthorized` or `forbidden` (no allow-list entry admits this repository, workflow, audience or service), `unknown_service`, `invalid_request`, `held_bad` (the version is marked bad; only an operator can lift the mark, design 8.1), `keys_unavailable` (retry later). `no_answer`: the coordinator could not be reached. `not_stored`: it answered with another version. The version is not set, except for `not_stored` and possibly `no_answer`: read `releases converge` before you retry. | Fix what the token names, then run the job again. |
| Set the desired version and wait for the fleet: the wait | The version is set. `halted`: a host halted the service, and no host is ordered until an operator clears the halt. `superseded`: the desired version changed while the job waited (an automatic rollback, or a newer release). `not_converged`: `max_polls` reads passed and the fleet had not converged; the roll goes on. A refusal token: the read was refused. | Look at the service in the coordinator's UI or with `hostkeeper ctl releases status`. |

## What it does not do

- It does not change the coordinator's allow-list, the service's definition, holds or marks.
- It does not build, sign or publish the service's package; the package and its digest come from
  the service's own release job.
- It does not retry a refused call.
