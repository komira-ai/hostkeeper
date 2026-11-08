# The CI client: setting a service's desired version from its release job

A service ships through hostkeeper by one step at the end of its own release job: it tells the
coordinator the new version and its release (the release index, which lists a package for each
platform, and the index's digest; or its targets, which list for each platform the artifacts a
host may install, beside the index or alone), then waits for the fleet to reach it.
That step is the reusable workflow `.github/workflows/set-desired.yml` of this repository. A
service calls it; it does not write the step itself.

The workflow does these steps, in this order (docs/design/two-programs.md, section 5). Steps 2, 3
and 6 run the checks of [Verify a release](verify-a-release.md) (steps 2, 3 and 6 there) on
`hkctl`, the command line of the coordinator ([docs/cli.md](../cli.md)), with `--format json`
added to the attestation check so that step 4 can read it; step 1 downloads with `curl` instead of
`gh release download`, and step 4 is a further check of the attested commit. The workflow lint
keeps steps 2 and 3 equal to the commands of that page, word for word, step 6 equal to its
`./hkctl --version` check and expected output, and the downloaded asset and tag equal to its
tables: a change to either file alone is refused.

First of all, before it downloads anything, the job checks `tailnet_tag`, the tag the join would
give the runner ([The tag and the ACL](#the-tag-and-the-acl)), and stops when it is not `tag:`
followed by a Tailscale tag name. The value is checked whether or not the job joins; the join is
the one step that reads it.

1. Download `hkctl-<hostkeeper_version>` of the pinned hostkeeper release, one bash script for
   every platform, and its `SHA256SUMS`.
2. Check the downloaded files against `SHA256SUMS`.
3. Verify hkctl's attestation: signed by `.github/workflows/artifacts.yml` of
   `komira-ai/hostkeeper` on a GitHub-hosted runner, for a build of `refs/heads/main` at the pinned
   `hostkeeper_commit`.
4. Compare the commit every verified attestation names as the build's source with the pinned
   `hostkeeper_commit`.
5. Install the verified hkctl, executable, as `hkctl`.
6. Check that `hkctl --version` prints `hkctl <hostkeeper_version>+<the first 12 hex digits of
   hostkeeper_commit>`, exactly.
7. Only when `join_tailnet` is true: join the tailnet, so that the runner can reach the
   coordinator's private listener ([Reaching the coordinator](#reaching-the-coordinator)). This is
   the first step that uses an action, and it runs after every check above.
8. Write the coordinator's CA to a file, the one root hkctl trusts for the call.
9. Write the request of the set: the version, and the one package digest, or the release index
   with its digest, or the targets, alone or with the index and its digest
   ([Targets](#targets)). A caller that gives no way, the package digest with another, half of
   the index, or targets that are not one JSON object, stops here, before any token exists.
10. Set the desired version: `hkctl releases set-desired`, as one command
    ([The calls](#the-calls)). hkctl asks the runner for a fresh OIDC token for `audience` and
    sends it to the coordinator through a pipe: no file holds it, no argument list and no
    other step's environment.
11. Only when `wait` is true and the set succeeded: wait for the fleet, `hkctl releases
    wait-converged`, one call the coordinator holds until the fleet has converged on the
    version, the service is halted, another version is set, or `max_polls` times 10 seconds have
    passed. It asks for a token of its own.
12. Whatever happened before it: write the job's outputs from what the two calls left
    ([Outputs](#outputs)), and fail the job unless the reason is `converged` or `not_waited`.

**A failed verification means the version is not set.** Steps 1 to 4 run nothing of hostkeeper's,
no step before step 7 uses an action, and no step before step 10 can request a token: the workflow
lint (`cargo test -p xtask --test workflows`) refuses the file otherwise. A version is set only by a
program the job has proven was built from the commit you pinned.

The coordinator must answer `Releases.WaitConverged` for step 11: a coordinator of a release
before that method refuses the wait (`unimplemented`, read as `no_answer`) after the version is
set. Pin a `hostkeeper_version` your coordinator is at least as new as.

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
      release_index: ${{ needs.publish.outputs.release_index }}
      release_index_digest: ${{ needs.publish.outputs.release_index_sha256 }}
      hostkeeper_version: 0.1.0
      hostkeeper_commit: 0123456789abcdef0123456789abcdef01234567
      coordinator_url: https://coordinator.example
      coordinator_ca: ${{ vars.HOSTKEEPER_COORDINATOR_CA }}
      audience: my-service-release
```

`release_index` is the text of the `index.json` the publish job wrote (design, section 5.5), and
`release_index_digest` its SHA-256, taken from the file the publish job uploaded: the two must be
the same bytes, so pass the text unchanged (no newline added or dropped). A service whose hosts
are all of one platform may pass `package_digest` instead, never with another way; a release with
targets passes `targets` beside the index ([Targets](#targets)).
Each host then fetches the package its platform has in the index, and checks the index it fetches
against `release_index_digest` before it reads it. A host of a platform the index does not list is
ordered nothing, and shows the hold `no_package_for_platform`
([Holds and force](../operations/holds-and-force.md)): that is not a failed update.

The calling job grants `id-token: write` and `contents: read`, and nothing else. The workflow
takes no secrets: the OIDC token is its only credential, and GitHub issues it to the job.

The coordinator must hold an allow-list entry for the calling repository and workflow that names
the service and the audience you pass (design, section 5.1: a CI token sets the desired version
only of the services its entry names).

## The release channel check

A coordinator started with `--release-channel <base-url>` (an `https://` URL such as
`https://channel.example/releases`, the same channel the daemon config's `release_channel`
names) checks every set against it before anything changes: it fetches
`<base-url>/<service>/<version>/index.json` and refuses the set unless the channel holds that
version with the package the set names. Without the flag it fetches nothing and refuses nothing
for the channel. Only `https://` is taken; any other scheme stops the coordinator's start.

| token | when | retry? |
|---|---|---|
| `not_in_channel` | The channel has no index for the version (404), or it could not be read: no connection, no answer within the coordinator's deadlines (`--release-channel-connect-secs`, default 10, and `--release-channel-timeout-secs`, default 30), a redirect or another status, an answer over 64 KiB, or an answer that is not an index or lists no package. | Yes. Nothing changed and no host was ordered. Make this job `needs:` the job that publishes the package and syncs the channel, or run the job again once the channel sync has finished. |
| `channel_digest_mismatch` | The channel's index for the version names another service or version, or lists no package whose SHA-256 is `package_digest`. | No. The channel holds another artifact for this version than the one you are setting; fix the channel or the digest. |

Both are refusals like the others (gRPC `FAILED_PRECONDITION`, the token as the message), so the
job ends with the token as `reason` and the version is not set.

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
new caller needs a reviewed change to the lint. Add the new commit to `job_workflow_ref`, and
restart the coordinator, before the change that moves the `uses:` pin runs, or the coordinator
refuses the token (`unauthorized`); keep the old commit listed while a run of the old pin may still
call (a re-run of an older run uses its old pin).

The entry for this repository's first caller, `hello-desired.yml`, the hello proof run
([docs/hello.md](../hello.md)):

```toml
[[remote_operator]]
issuer = "https://token.actions.githubusercontent.com"
audience = "hostkeeper-ci"
repository_id = "123456789"            # gh api repos/komira-ai/hostkeeper --jq .id
repository = "komira-ai/hostkeeper"
job_workflow_ref = [
  "komira-ai/hostkeeper/.github/workflows/set-desired.yml@3018137f2c8806dd22bf2071a23565d4a2d64e8d",
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

`release.yml` sets hostkeeper's own desired version through the client too, while the
repository variable `RELEASE_SETS_DESIRED` is `true` ([Deploying hostkeeper from GitHub
Actions](github-actions.md)). It pins the same revision of `set-desired.yml` as
`hello-desired.yml` (the workflow lint holds both callers to it), so the same entry covers it with
two changes: `event_names` adds `workflow_run` and `schedule`, the events `release.yml` runs on,
and `services` adds `hostkeeper`:

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
| `package_digest` | string | no |  | The SHA-256 of the one package of this version that every host is ordered, 64 lower-case hex digits. For a fleet of one platform: a host of another platform would refuse the package. Give this, or `release_index` with `release_index_digest`, or `targets`; never this with another. |
| `release_index` | string | no |  | The text of the version's release index (`index.json`), byte for byte, at most 3 KiB (a coordinator before `SetDesired` took targets takes a request of at most 4 KiB, the index included; a three-platform index is under 1 KiB). Each host is ordered the package its own platform has in it. Comes with `release_index_digest`; not with `package_digest`. |
| `release_index_digest` | string | no |  | The SHA-256 of `release_index`, 64 lower-case hex digits, as the release job that published the index wrote it down. The coordinator checks the index against it, and every host checks the index it fetches against it. |
| `targets` | string | no |  | The targets of the version: one JSON object keyed by platform (`linux-x86_64`, `linux-arm64`, `macos-arm64`), each value `{"entries": [...]}` in the order the service prefers, under 128 KiB, the most the runner passes in one environment value or argument ([Targets](#targets)). Alone, or with `release_index` and `release_index_digest`; not with `package_digest`. Needs a coordinator that takes `SetDesired` with targets. |
| `hostkeeper_version` | string | yes |  | The hostkeeper release whose command line, `hkctl`, makes the calls. |
| `hostkeeper_commit` | string | yes |  | The commit of `komira-ai/hostkeeper` that release was built from, 40 lower-case hex digits. |
| `coordinator_url` | string | yes |  | The coordinator's private listener, as `https://<host>[:<port>]`. |
| `coordinator_ca` | string | yes |  | The PEM certificates of the CA the coordinator's certificate must chain to. No other root is trusted. |
| `audience` | string | yes |  | The audience of the OIDC token, as the coordinator's allow-list entry for the calling repository names it. |
| `wait` | boolean | no | `true` | Whether to wait for the fleet to converge on the version. |
| `max_polls` | number | no | `180` | How long to wait for the fleet, in tens of seconds, from 1 to 180: the wait is one call the coordinator holds for at most this many times 10 seconds, so the default waits half an hour. A value above 180 (more than the coordinator's 1800 seconds) ends the job with the reason `invalid_wait`, after the version is set. |
| `join_tailnet` | boolean | no | `false` | Whether to join the tailnet after the verification, so that a GitHub-hosted runner can reach a `coordinator_url` on it ([Reaching the coordinator](#reaching-the-coordinator)). |
| `tailnet_client_id` | string | no |  | The client ID of the tailnet's federated identity for the calling repository. Not a secret. Needed when `join_tailnet` is true. |
| `tailnet_audience` | string | no |  | The audience of that federated identity. Not a secret. Needed when `join_tailnet` is true. |
| `tailnet_coordinator` | string | no |  | The coordinator's name or address on the tailnet. With `join_tailnet`, the join waits until it answers a `tailscale ping` (at most 3 minutes), so the first call does not race the tailnet's spread of the new node. Empty: no wait. |
| `tailnet_tag` | string | no | `tag:hk-ci` | The tag the join gives the runner's node: the tag the federated identity's trust credential issues ([The tag and the ACL](#the-tag-and-the-acl)). `tag:` followed by a lower-case letter, then lower-case letters, digits and `-`; one tag, not a list. Any other value, the empty one included, ends the job at its first step, whether or not it joins. |

## Outputs

| output | meaning |
|---|---|
| `desired_version` | The desired version the coordinator stored; empty when it stored none. |
| `converged` | `true` when the fleet converged on the version, `false` otherwise. |
| `reason` | Why the job ended where it did, one token: `converged`, `not_waited`, `halted`, `superseded`, `not_converged`, `not_stored`, `no_answer`, or the refusal token the coordinator gave (for example `held_bad`, `not_in_channel`, `channel_digest_mismatch`, `invalid_wait` or `too_many_waits`). Empty when a step before the set failed (a verification, the install, the CA or the request), so the set never ran. |

The last step writes them with one jq program, held in the workflow beside the step that runs it,
from the outcome of each call, the answer each wrote (`--response-file`) and the diagnostics each
wrote to its own file, where hkctl prints a refusal as `refused: <token>`. A token is taken only
when it is 1 to 64 characters of `[a-z0-9_]`, and a version is written only when it is a version
(1 to 255 of `A-Za-z0-9._+-`, not starting with `-` or `.`): nothing else of a caller's input or of
an answer reaches the outputs. The first row that holds, in order:

| What the calls left | `reason` | `converged` | `desired_version` |
|---|---|---|---|
| the set did not run: a step before it failed | empty | `false` | empty |
| the set failed with `refused: <token>` | the token | `false` | empty: the version is not set |
| the set failed otherwise (no answer, an answer that is not the gateway's, a reason outside the grammar, cancelled) | `no_answer` | `false` | empty: whether it is set is not known |
| the stored version differs from the requested one | `not_stored` | `false` | the stored one, when it is a version; else empty |
| `wait` is false | `not_waited` | `false` | the requested version |
| the wait failed with `refused: <token>` (`invalid_wait`, `too_many_waits`, ...) | the token | `false` | the requested version |
| the wait failed otherwise or was cancelled, or its outcome is absent, `WAIT_OUTCOME_UNSPECIFIED` or one this table does not name | `no_answer` | `false` | the requested version |
| outcome `WAIT_OUTCOME_CONVERGED`, `WAIT_OUTCOME_HALTED`, `WAIT_OUTCOME_SUPERSEDED` | `converged`, `halted`, `superseded` | `true` only for `converged` | the requested version |
| outcome `WAIT_OUTCOME_DEADLINE` | `not_converged` | `false` | the requested version |

The job succeeds only with `converged` or `not_waited`; otherwise the step prints one `::error::`
line that says whether the version is set, followed by the failed call's diagnostics.

## Pinning hostkeeper

Pin three things, and change them together, in a reviewed pull request of the service:

- the `uses:` line, to the full commit of this repository whose `set-desired.yml` you call, with
  the release as its comment;
- `hostkeeper_version`, the release whose hkctl the job downloads: one that has the asset
  `hkctl-<version>`;
- `hostkeeper_commit`, the commit of `komira-ai/hostkeeper` that release was built from.

Why: the downloaded hkctl holds your OIDC token. A pin means a new hostkeeper release,
or a moved tag, cannot change what runs with that token until you change the pin. The commit is
what the attestation proves: the job refuses a release whose attestation names any other source
commit, so a release replaced after you pinned it is refused, not run.

A pin of an older revision of `set-desired.yml` keeps working as it did: it downloads its own
release's tarball and runs that release's `hostkeeper ci set-desired`, which polls
`Releases.Converge`. The two ways differ for the coordinator's allow-list only in the commit of
`job_workflow_ref`; move the pin and that entry together, as above.

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
- the node is ephemeral and pre-approved, tagged `tailnet_tag` (`tag:hk-ci` unless the caller
  passes another), and logged out when the job ends; Tailscale removes it soon after.

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
      # The tag the trust credential issues; tag:hk-ci when the variable is empty.
      tailnet_tag: ${{ vars.HOSTKEEPER_TAILNET_TAG || 'tag:hk-ci' }}
```

The client ID and the audience are not secrets (Tailscale shows both in its admin console), so
they are repository variables, not secrets. The coordinator's certificate must name the host
`coordinator_url` names, and its `--private-listen` must be the address the runner reaches on the
tailnet.

### The tag and the ACL

The runner's node carries one tag, `tailnet_tag`: whatever tag the trust credential of the
caller's federated identity issues ([The federation](#the-federation-set-up-once-by-a-tailnet-admin)).
Tailscale refuses the join when the job asks for a tag the credential does not issue. By default
it is `tag:hk-ci`, a tag of its own for the CI client. A tailnet whose CI already joins with a
credential of another tag, such as an organisation's credential whose nodes are `tag:ci`, passes
that tag instead (`tailnet_tag: tag:ci`) and needs no second credential.

Whichever tag it is, the ACL must let it reach the coordinator's private listener. Tag the
coordinator's machine `tag:hk-coordinator`, and give the tailnet policy file this stanza, with the
port of `coordinator_url` (443 when the URL names none) and the job's tag in place of `tag:hk-ci`
(a tag already in the policy keeps its own `tagOwners` entry):

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
removed: while it is there, the tag reaches every node. The `tests` entry makes Tailscale
refuse a policy edit under which the tag can reach the coordinator's SSH port or cannot reach the
listener. The heartbeat listener needs no grant for CI.

With `tag:hk-ci` the node reaches the coordinator's private listener and nothing else. A tag
shared with other CI jobs, such as `tag:ci`, keeps the grants those jobs need, and the join runs
with all of them: the stanza adds the coordinator's listener to them and takes nothing away.

`release.yml`'s `channel` job, which reads the release channel before this repository's own call
of the workflow ([Deploying hostkeeper from GitHub Actions](github-actions.md)), does not join the
tailnet: it reads `HOSTKEEPER_RELEASE_CHANNEL` from the runner as it is, so the channel must be
reachable from a GitHub-hosted runner, and no grant to the channel's port is needed for it. Were
the channel served only on the tailnet, a grant from the tag to the channel's port would not be
enough: that job would have to join first, which it does not.

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
   - scope: **Auth Keys**, write, with the one tag the job passes as `tailnet_tag` (`tag:hk-ci`
     unless it passes another). A credential the tailnet already has for its CI, with its own
     tag, serves as well: give its subject this repository's job, and pass its tag.
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
can add only ephemeral nodes of the credential's tag, which reach what the ACL grants that tag:
with `tag:hk-ci` and the stanza above, only the coordinator's private listener.

### Not chosen: a reverse proxy

A reverse proxy on the public network that forwards CI's calls to the private listener would let
a hosted runner call the coordinator without joining anything, and no action would run in the
job. It is not done here: it puts the private listener's CI path on the public network, against
the design's private listener, and needs a new deployed component. It is the alternative a later
change can take up.

## The calls

Each call is one `hkctl` command. Its globals name the coordinator, its CA and the token's
audience; `--github-oidc-audience` makes hkctl ask the runner for a fresh GitHub Actions ID token
for that audience, print `::add-mask::<token>` so the log never shows it, and send it to the
coordinator through a pipe. The coordinator accepts each token once, so each call asks for its
own. The answer goes to the file of `--response-file`, so that stdout carries only the mask line,
and the workflow sends each call's diagnostics to a file of its own (`2>`), which the outputs step
reads. The set:

```sh
hkctl --endpoint https://<coordinator> --ca-file "$RUNNER_TEMP/hkctl-coordinator-ca.pem" \
  --github-oidc-audience "$AUDIENCE" --response-file "$RUNNER_TEMP/hkctl-set.json" \
  releases set-desired --service "$SERVICE" --request "$RUNNER_TEMP/hkctl-request.json"
```

where the request file is `{"desired":{"version":"<version>"},"packageDigest":"<hex>"}`, or, with
a release index, `{"desired":{"version":"<version>"},"releaseIndex":"<text of
index.json>","releaseIndexDigest":"<hex>"}`: the canonical JSON names of the request, the index
one JSON string of its exact text. The request step builds it with `jq --arg` from the step's
environment, and does not hash or parse the index: the coordinator does, and refuses a digest
that is not the index's (`index_digest_mismatch`) or an index that is not this service's at this
version (`index_invalid`). A coordinator before `SetDesired` took targets takes a request of at
most 4 KiB, the index included, so the index stays within 3 KiB. With targets, the request file
also holds `"targets":{...}`, the object of the input as an object, not a string
([Targets](#targets)).

Then the wait, with `--wait-secs` ten times `max_polls`:

```sh
hkctl --endpoint https://<coordinator> --ca-file "$RUNNER_TEMP/hkctl-coordinator-ca.pem" \
  --github-oidc-audience "$AUDIENCE" --response-file "$RUNNER_TEMP/hkctl-wait.json" \
  releases wait-converged --service "$SERVICE" --version "$VERSION" --wait-secs 1800
```

The coordinator answers at once when the wait is over already, and otherwise after the change
that ends it; hkctl gives curl a time limit of the wait plus its own timeout. A wait cut by a
restart of the coordinator or by the network ends with no answer (`no_answer`): the version is
set and the roll goes on, and the job can be run again.

## Targets

`targets` names, for each platform, the artifacts of the version a host may install, one entry
per installer in the order the service prefers (docs/design/packages.md, sections 3 and 7). Each
host is ordered the entry of the installer it runs, else the first whose installer its daemon
announces; a platform left out is ordered nothing. An entry, in the request's canonical JSON:

```json
{"installer": "hostkeeper-archive",
 "locations": [{"store": "channel", "locator": "hello/0.1.5/hello-0.1.5-linux-x86_64.tar.zst"},
               {"store": "github", "locator": "komira-ai/hostkeeper/releases/download/v0.1.5/hello-0.1.5-linux-x86_64.tar.zst"}],
 "digest": "<64 hex>", "bytes": "48812",
 "attestation": {"digest": "<64 hex>", "bytes": "7311", "locations": [...]},
 "manifest": {"minHostkeeper": "0.1.0"}}
```

A location names a store of the host's own configuration and a path below it: `channel` is the
release channel, the one store a daemon without `/etc/hostkeeper/stores.toml` has, and `github`
the GitHub release asset, for a host whose stores define it. A daemon tries them in order and
skips a store it does not define. A `hostkeeper-archive` manifest holds `minHostkeeper` alone:
the hook, units and files are in the attested `manifest.toml` inside the archive.

This repository builds the targets of its own releases with `cargo xtask release targets
--service <hostkeeper|hello|mdm> [--fail-ready] --version <X.Y.Z> --dir <assets> --repo
komira-ai/hostkeeper`, from the release's assets: for each platform the package's index lists,
one `hostkeeper-archive` entry with the archive's size and SHA-256, its `.sigstore.json` bundle,
both locations, and the `min_hostkeeper` of the archive's own `manifest.toml`. It refuses a
release whose index, archives, bundles or manifests do not check out, and entries the
coordinator or the daemon would refuse, and prints `targets=<JSON>` (appended to `--output`).

**Give the index too while daemons before targets run.** A daemon that does not announce
`target-v1` is never sent a target: it is ordered the archive through the release channel, by
the index. So a set whose lists hold `hostkeeper-archive` entries also gives `release_index` and
`release_index_digest`, and the coordinator refuses the set (`index_invalid`) unless the index
lists exactly the platforms with an archive entry, each with that entry's size and SHA-256. The
targets of `release targets` agree with the index they are read from.

**What the coordinator checks.** It refuses targets that break the rules of a release
(`target_invalid`: an unknown platform, a list of no entry or more than 8, an installer twice, a
digest, size, location or manifest out of its bounds, or for the service `hostkeeper` a list but
one `hostkeeper-archive` entry), and the same version set before with other targets
(`release_exists`, do not retry: a release never changes; set a new version). A set with targets
is at most 256 KiB, the index included. `hkctl releases get-release --service <service>
--version <version>` reads back what was set. A coordinator before targets refuses the set for
its size or the unknown field, and nothing is set: send targets only once your coordinator takes
them.

## When it fails

| step that failed | what it means | what to do |
|---|---|---|
| Check the tailnet tag | `tailnet_tag` is not `tag:` followed by a lower-case letter, then lower-case letters, digits and `-` (jq's error says so), with or without `join_tailnet`. Nothing is downloaded and the version is not set. | Pass one tag, such as `tag:ci`, or leave the input out for `tag:hk-ci`. |
| Download hkctl of the hostkeeper release | No release `v<hostkeeper_version>`, or it has no `hkctl-<hostkeeper_version>` (a release before hkctl) or no `SHA256SUMS`. The version is not set. | Check the pin against the releases of `komira-ai/hostkeeper`. |
| Check hkctl against SHA256SUMS | A downloaded file does not have the bytes `SHA256SUMS` lists. The version is not set. | Do not retry with another file. Report it to the hostkeeper maintainers. |
| Verify the attestation of hkctl | hkctl has no attestation from `artifacts.yml` of `komira-ai/hostkeeper` on `refs/heads/main`, built on a GitHub-hosted runner from `hostkeeper_commit`. The version is not set. | Check that `hostkeeper_commit` is the commit of `hostkeeper_version`. If it is, the release is not the one that was built: report it. |
| Compare the attested source commit | An attestation names another source commit than `hostkeeper_commit` (`jq` prints `false`). The version is not set. | As above. |
| Install the verified hkctl | The runner could not write the file. The version is not set. | Run the job again. |
| Check the version of the verified hkctl | hkctl does not say `hkctl <hostkeeper_version>+<first 12 hex digits of hostkeeper_commit>`. The version is not set. | Check the two pins belong together. |
| Join the tailnet | Only with `join_tailnet`. Tailscale refused the federated identity (no credential for this client ID and audience, a subject that does not match the job, or a `tailnet_tag` the credential does not issue), or `tailnet_coordinator` did not answer a ping within 3 minutes. The version is not set. | Check the [federation](#the-federation-set-up-once-by-a-tailnet-admin) and the [ACL](#the-tag-and-the-acl). |
| Write the request of the set | No way of naming the release, `package_digest` with `release_index`, `release_index_digest` or `targets`, one of the index pair alone, or `targets` that are not one JSON object (jq's error names the rule). `reason` is empty. The version is not set. | Give one way of naming the release: `package_digest`, the index pair, or `targets` alone or with the index pair. |
| Set the desired version | The coordinator refused the call, and `reason` is its token: `unauthorized` or `forbidden` (no allow-list entry admits this repository, workflow, audience or service), `unknown_service`, `invalid_request` (a service, version or digest the coordinator does not take), `index_digest_mismatch` (the digest is not the SHA-256 of the index text), `index_invalid` (the index is not this service's at this version, is given with a package digest, or does not list exactly the `hostkeeper-archive` entries of the targets), `held_bad` (the version is marked bad; only an operator can lift the mark, design 8.1), `keys_unavailable` (retry later), `not_in_channel` (the coordinator's release channel does not hold the version yet; retry once the channel sync has finished), `channel_digest_mismatch` (the channel's index of the version is not the index the set names, or does not list `package_digest`; do not retry), `target_invalid` (the targets break a rule of a release, [Targets](#targets)), `release_exists` (the version was set before with other targets; do not retry). `no_answer`: the coordinator could not be reached (with `join_tailnet` false this is what a GitHub-hosted runner gets, because the private listener is not reachable from it, [Reaching the coordinator](#reaching-the-coordinator)), GitHub issued no token (the calling job does not grant `id-token: write`), or hkctl refused a flag. `not_stored`: it answered with another version. The version is not set, except for `not_stored` and possibly `no_answer`: read `hkctl releases status --service <service>` before you retry. | Fix what the token names, then run the job again. |
| Wait for the fleet to converge | The version is set. `halted`: a host halted the service, and no host is ordered until an operator clears the halt. `superseded`: the desired version changed while the job waited (an automatic rollback, or a newer release). `not_converged`: `max_polls` times 10 seconds passed and the fleet had not converged; the roll goes on. `invalid_wait`: `max_polls` is above 180 (or below 1). `too_many_waits`: the coordinator already holds 16 waits on its listener; run the job again. `no_answer`: the wait was cut (a restart of the coordinator, the network) or got no token. | Look at the service in the coordinator's UI or with `hkctl releases status`. |

## What it does not do

- It does not change the coordinator's allow-list, the service's definition, holds or marks.
- It does not build, sign or publish the service's package, its release index or its targets;
  they and their digests come from the service's own release job. It does not hash or parse the
  index, and does not check the targets beyond their being one JSON object.
- It does not retry a refused call, `not_in_channel` included: run the job again once the
  release channel holds the version. It does not call again after a cut wait.
