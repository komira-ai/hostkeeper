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
7. Only when `join_tailnet` is true: join the tailnet, so that the runner can reach the
   coordinator's private listener ([Reaching the coordinator](#reaching-the-coordinator)). This is
   the first step that uses an action, and it runs after every check above.
8. Only then run the verified program's CI client, `hostkeeper ci set-desired`, as one command.
   It requests an OIDC token for the coordinator, writes it to a file only the runner can read,
   and calls `hostkeeper ctl releases set-desired` with it. When `wait` is true it then reads
   `hostkeeper ctl releases converge` until the fleet has converged, is halted, or the call is
   refused, at most `max_polls` times, and writes the job's outputs.

**A failed verification means the version is not set.** Steps 1 to 4 run nothing of hostkeeper's,
no step before step 7 uses an action, and no step before step 7 can request a token: the workflow
lint (`cargo test -p xtask --test workflows`) refuses the file otherwise. A version is set only by a
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

## The allow-list entry

The coordinator reads its allow-list at start from the file of `--oidc-config`, one
`[[remote_operator]]` table per entry (`crates/hostkeeper-oidc`, `config.rs`). The first entry
that admits a token wins, and the token sets the desired version only of the services that entry
names.

**`job_workflow_ref` names `set-desired.yml`, not your workflow.** GitHub sets a job's
`job_workflow_ref` to the reusable workflow the job runs, so every caller's token names
`komira-ai/hostkeeper/.github/workflows/set-desired.yml@<the commit of your uses: line>`. The
entry cannot tell two workflows of one repository apart that call the same pin: it admits any
workflow of the repository that calls that pin from a ref in `refs`. In this repository the workflow
lint (`cargo test -p xtask --test workflows`, `callers.rs`) holds the callers to a fixed list, so a
new caller needs a reviewed change to the lint. Change `job_workflow_ref` in the same change as
the `uses:` pin, or the coordinator refuses the token (`unauthorized`).

The entry for this repository's one caller today, `hello-desired.yml`, the hello proof run
([docs/hello.md](../hello.md)):

```toml
[[remote_operator]]
issuer = "https://token.actions.githubusercontent.com"
audience = "hostkeeper-ci"
repository_id = "123456789"            # gh api repos/komira-ai/hostkeeper --jq .id
repository = "komira-ai/hostkeeper"
job_workflow_ref = [
  "komira-ai/hostkeeper/.github/workflows/set-desired.yml@f04696da7e2bdcbaf9112450ab651a1ab24e61bf",
]
refs = ["refs/heads/main"]
event_names = ["workflow_dispatch"]
runner_environment = "github-hosted"
services = ["hello"]
```

| key | value here | why |
|---|---|---|
| `audience` | `hostkeeper-ci` | The caller's `audience` input; the repository variable `HOSTKEEPER_CI_AUDIENCE` holds it for `hello-desired.yml`. |
| `repository_id` | the repository's number | It does not change when the repository is renamed or moved; `repository` is checked too when given. |
| `job_workflow_ref` | `set-desired.yml` at the pinned commit | The ref of the `uses:` line, as above. |
| `refs` | `refs/heads/main` | `hello-desired.yml` sets nothing from another branch: its jobs run only on main, and a token from another ref is refused here as well. |
| `event_names` | `workflow_dispatch` | `hello-desired.yml` runs by hand only. The default is `push` and `workflow_dispatch`; a `pull_request` event is refused in any entry. |
| `services` | `hello` | What a token of the entry may set. |

When `release.yml` sets hostkeeper's own desired version (it does not call the client yet), the
same entry covers it with two changes: `event_names` adds `workflow_run` and `schedule`, the events
`release.yml` runs on, and `services` adds `hostkeeper`:

```toml
event_names = ["workflow_dispatch", "workflow_run", "schedule"]
services = ["hello", "hostkeeper"]
```

Adding or changing the entry is a change to the coordinator's configuration, made by its operator;
the coordinator reads the file when it starts, so restart it afterwards. Nothing in CI writes it.

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
| `join_tailnet` | boolean | no | `false` | Whether to join the tailnet after the verification, so that a GitHub-hosted runner can reach a `coordinator_url` on it ([Reaching the coordinator](#reaching-the-coordinator)). |
| `tailnet_client_id` | string | no |  | The client ID of the tailnet's federated identity for the calling repository. Not a secret. Needed when `join_tailnet` is true. |
| `tailnet_audience` | string | no |  | The audience of that federated identity. Not a secret. Needed when `join_tailnet` is true. |
| `tailnet_coordinator` | string | no |  | The coordinator's name or address on the tailnet. With `join_tailnet`, the join waits until it answers a `tailscale ping` (at most 3 minutes), so the first call does not race the tailnet's spread of the new node. Empty: no wait. |

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

## Reaching the coordinator

`coordinator_url` is the coordinator's private listener, the one CI's OIDC tokens are presented to.
It is bound to the private network, off the public one (design, section 4.3). The workflow runs on
a GitHub-hosted runner (`ubuntu-24.04`), which is not on that network, and no other kind of runner
is used. So without a way in, every call gets no answer and the job ends with `reason` set to
`no_answer`.

The way in is the tailnet. With `join_tailnet: true` the job joins the runner to it, after every
verification step and before the client runs:

- the step uses `tailscale/github-action`, pinned in `set-desired.yml` to a full commit, and the
  Tailscale tarball it installs is pinned by version and SHA-256;
- it authenticates with workload identity federation: the action asks GitHub for an OIDC token
  for the federated identity's audience and exchanges it with Tailscale. No auth key or OAuth
  secret exists, in this repository or in the caller's secrets;
- the node is ephemeral and pre-approved, tagged `tag:hk-ci`, and logged out when the job ends;
  Tailscale removes it soon after.

Leave `join_tailnet` false when the coordinator's private listener is reachable from the runner
some other way; then the step is skipped and no action runs in the job.

### Calling it with the join

```yaml
    with:
      # ... the inputs of "Calling it" above, with coordinator_url naming the coordinator
      # by the name it has on the tailnet and in its certificate.
      join_tailnet: true
      tailnet_client_id: ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID }}
      tailnet_audience: ${{ vars.HOSTKEEPER_TAILNET_AUDIENCE }}
      tailnet_coordinator: hk-coordinator
```

The client ID and the audience are not secrets (Tailscale shows both in its admin console), so
they are repository variables, not secrets. The coordinator's certificate must name the host
`coordinator_url` names, and its `--private-listen` must be the address the runner reaches on the
tailnet.

### The tag and the ACL

`tag:hk-ci` is the one tag of the CI nodes. It may reach the coordinator's private listener and
nothing else. Tag the coordinator's machine `tag:hk-coordinator`, and give the tailnet policy file
this stanza, with the port of `coordinator_url` (443 when the URL names none):

```jsonc
{
  "tagOwners": {
    "tag:hk-ci": ["autogroup:admin"],
    "tag:hk-coordinator": ["autogroup:admin"],
  },
  "grants": [
    // CI runners reach the coordinator's private listener, and nothing else.
    {"src": ["tag:hk-ci"], "dst": ["tag:hk-coordinator"], "ip": ["tcp:443"]},
  ],
  "tests": [
    {"src": "tag:hk-ci", "accept": ["tag:hk-coordinator:443"], "deny": ["tag:hk-coordinator:22"]},
  ],
}
```

The policy is default-deny only once the default allow-all grant (`{"src": ["*"], "dst": ["*"], "ip": ["*"]}`) is
removed: while it is there, `tag:hk-ci` reaches every node. The `tests` entry makes Tailscale
refuse a policy edit under which the tag can reach the coordinator's SSH port or cannot reach the
listener. The heartbeat listener needs no grant for CI.

### The federation, set up once by a tailnet admin

This is a manual step in the Tailscale admin console, done once per service repository that
calls the workflow. Nothing in this repository runs it.

1. Add the stanza above to the tailnet policy file, and tag the coordinator's machine
   `tag:hk-coordinator`.
2. Under **Settings**, **Trust credentials**, add a credential of kind **OpenID Connect**:
   - issuer: **GitHub** (`https://token.actions.githubusercontent.com`);
   - subject: the `sub` claim of the service's release job, for example
     `repo:<owner>/<service-repo>:ref:refs/heads/main`, or
     `repo:<owner>/<service-repo>:environment:<name>` when the job uses an environment. Name one
     repository, never a wildcard over owners or repositories;
   - optionally a custom claim `job_workflow_ref` that names
     `komira-ai/hostkeeper/.github/workflows/set-desired.yml` at the commit the service pins, so
     only this workflow can join;
   - scope: **Auth Keys**, write, with the one tag `tag:hk-ci`.
3. Copy the credential's client ID and audience into the service repository's variables
   `HOSTKEEPER_TAILNET_CLIENT_ID` and `HOSTKEEPER_TAILNET_AUDIENCE`.

To revoke CI's access, delete the credential; nodes it made are ephemeral and go away on their
own.

### What the join is trusted with

The job holds `id-token: write`, so the join action can ask GitHub for an OIDC token for any
audience, the coordinator's included, and can write the runner's files, the verified hostkeeper
program included. That is why it runs only after every verification step (it cannot change what
was verified, only what runs after), only when `join_tailnet` is true, and only at the commit
`set-desired.yml` pins: moving that pin is a reviewed change to this repository, and a service
picks it up only when it moves its own pin of `set-desired.yml`. The tailnet token the action gets
can add only ephemeral `tag:hk-ci` nodes, which reach only the coordinator's private listener.

### Not chosen: a reverse proxy

A reverse proxy on the public network that forwards CI's calls to the private listener would let
a hosted runner call the coordinator without joining anything, and no action would run in the
job. It is not done here: it puts the private listener's CI path on the public network, against
the design's private listener, and needs a new deployed component. It is the alternative a later
change can take up.

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
| Join the tailnet | Only with `join_tailnet`. Tailscale refused the federated identity (no credential for this client ID and audience, a subject that does not match the job, or a tag other than `tag:hk-ci` on the credential), or `tailnet_coordinator` did not answer a ping within 3 minutes. The version is not set. | Check the [federation](#the-federation-set-up-once-by-a-tailnet-admin) and the [ACL](#the-tag-and-the-acl). |
| Set the desired version and wait for the fleet: before the call | An input breaks its rule (exit 2): a service name or version with other characters, a digest that is not 64 lower-case hex digits, a URL that is not `https://`, a `max_polls` outside 1 to 1000. Or GitHub issued no token (`reason` is `no_answer`): the calling job does not grant `id-token: write`. The version is not set. | Fix the input, or grant `id-token: write` to the calling job. |
| Set the desired version and wait for the fleet: the set | The coordinator refused the call, and `reason` is its token: `unauthorized` or `forbidden` (no allow-list entry admits this repository, workflow, audience or service), `unknown_service`, `invalid_request`, `held_bad` (the version is marked bad; only an operator can lift the mark, design 8.1), `keys_unavailable` (retry later). `no_answer`: the coordinator could not be reached; with `join_tailnet` false this is what a GitHub-hosted runner gets, because the private listener is not reachable from it ([Reaching the coordinator](#reaching-the-coordinator)). `not_stored`: it answered with another version. The version is not set, except for `not_stored` and possibly `no_answer`: read `releases converge` before you retry. | Fix what the token names, then run the job again. |
| Set the desired version and wait for the fleet: the wait | The version is set. `halted`: a host halted the service, and no host is ordered until an operator clears the halt. `superseded`: the desired version changed while the job waited (an automatic rollback, or a newer release). `not_converged`: `max_polls` reads passed and the fleet had not converged; the roll goes on. A refusal token: the read was refused. | Look at the service in the coordinator's UI or with `hostkeeper ctl releases status`. |

## What it does not do

- It does not change the coordinator's allow-list, the service's definition, holds or marks.
- It does not build, sign or publish the service's package; the package and its digest come from
  the service's own release job.
- It does not retry a refused call.
