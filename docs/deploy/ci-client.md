# The CI client: setting a service's desired version from its release job

A service ships through hostkeeper by one step at the end of its own release job: it tells the
coordinator the new version and the digest of its package, then waits for the fleet to reach it.
That step is the reusable workflow `.github/workflows/set-desired.yml` of this repository. A
service calls it; it does not write the step itself.

The workflow does the steps of the design's CI client (section 4.5), in this order:

1. Download the pinned hostkeeper release: its archive, its attestation bundle and `SHA256SUMS`.
2. Check the archive against its line of `SHA256SUMS`.
3. Verify the archive's attestation: signed by `.github/workflows/artifacts.yml` of
   `komira-ai/hostkeeper` on a GitHub-hosted runner, for a build of `refs/heads/main`.
4. Compare the commit the attestation names as the build's source with the pinned
   `hostkeeper_commit`.
5. Only then: request an OIDC token for the coordinator, write it to a file only the runner can
   read, and call `hostkeeper ctl releases set-desired` with it.
6. When `wait` is true, read convergence with `releases converge` until the fleet has converged,
   is halted, or the call is refused, at most `max_polls` times.

**A failed verification means the version is not set.** Steps 2 to 4 run before any token
exists, and no step before them can read the token request: the workflow lint
(`cargo test -p xtask --test workflows`) refuses the file otherwise. A version is set only by a
program the job has proven was built from the commit you pinned.

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
| `reason` | Why the job ended where it did, one token: `converged`, `not_waited`, `halted`, `superseded`, `not_converged`, `not_stored`, `no_answer`, or the refusal token the coordinator gave (for example `held_bad`). Empty when the job stopped before calling the coordinator. |

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

After the verification steps the job calls, with the verified program:

```sh
hostkeeper ctl --endpoint "$COORDINATOR_URL" --ca-file "$CA_FILE" --token-file "$TOKEN_FILE" --output json \
  releases set-desired --service "$SERVICE" --package-digest "$PACKAGE_DIGEST" --request "$REQUEST_FILE"
```

where the request file is `{"desired": {"version": "<version>"}}` (the version is set through
the request file because `DesiredState` is a message: see `docs/ctl.md`). Then, once per poll,
with a new token each time (the coordinator accepts each token once):

```sh
hostkeeper ctl --endpoint "$COORDINATOR_URL" --ca-file "$CA_FILE" --token-file "$TOKEN_FILE" --output json \
  releases converge --service "$SERVICE"
```

The token file is created with mode 0600 in the runner's temporary directory and removed after
each call. The token is never an argument (another process on the runner can read a process's
arguments) and never an environment variable, and it is masked in the log.

Each state the convergence read reports is printed once, when it is first seen: the state, the
desired version, and the hosts of each role at the version, updating, failed and down.

## When it fails

| step that failed | what it means | what to do |
|---|---|---|
| Check the inputs | An input breaks its rule: a service name or version with other characters, a digest that is not 64 lower-case hex digits, a commit that is not 40, a URL that is not `https://`, a `max_polls` outside 1 to 1000. Nothing was downloaded. | Fix the input. |
| Download the hostkeeper release | No release has the pinned `hostkeeper_version`, or one of its three assets is missing. The version is not set. | Check the pin against the releases of `komira-ai/hostkeeper`. |
| Check the archive against SHA256SUMS | The archive is not the one `SHA256SUMS` lists, or `SHA256SUMS` does not name it exactly once. The version is not set. | Do not retry with another archive. Report it to the hostkeeper maintainers. |
| Verify the attestation of the archive | The archive has no attestation from `artifacts.yml` of `komira-ai/hostkeeper` on `refs/heads/main`, built on a GitHub-hosted runner from `hostkeeper_commit`. The version is not set. | Check that `hostkeeper_commit` is the commit of `hostkeeper_version`. If it is, the release is not the one that was built: report it. |
| Compare the attested source commit | The attestation names another source commit than `hostkeeper_commit`. The version is not set. | As above. |
| Unpack the verified hostkeeper | The verified archive holds no `bin/hostkeeper`. The version is not set. | Report it. |
| Request the OIDC token | GitHub issued no token: the calling job does not grant `id-token: write`. The version is not set. | Grant `id-token: write` to the calling job. |
| Set the desired version | The coordinator refused the call, and `reason` is its token: `unauthorized` or `forbidden` (no allow-list entry admits this repository, workflow, audience or service), `unknown_service`, `invalid_request`, `held_bad` (the version is marked bad; only an operator can lift the mark, design 8.1), `keys_unavailable` (retry later). `no_answer`: the coordinator could not be reached. `not_stored`: it answered with another version. The version is not set, except for `not_stored` and possibly `no_answer`: read `releases converge` before you retry. | Fix what the token names, then run the job again. |
| Wait for the fleet to converge | The version is set. `halted`: a host halted the service, and no host is ordered until an operator clears the halt. `superseded`: the desired version changed while the job waited (an automatic rollback, or a newer release). `not_converged`: `max_polls` reads passed and the fleet had not converged; the roll goes on. A refusal token: the read was refused. | Look at the service in the coordinator's UI or with `hostkeeper ctl releases status`. |

## What it does not do

- It does not change the coordinator's allow-list, the service's definition, holds or marks.
- It does not build, sign or publish the service's package; the package and its digest come from
  the service's own release job.
- It does not retry a refused call.
