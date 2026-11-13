# The CI client: setting a service's desired version from its release job

A service ships through hostkeeper by one step at the end of its own release job: it tells the
coordinator the new version and its targets (the list, for each platform, of the artifacts a host
may install), then waits for the fleet to reach it.
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
9. Write the request of the set: the version and the targets ([Targets](#targets)). A caller
   whose targets are empty or are not one JSON object stops here, before any token exists.
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

## Before it runs: the release must be installable

A host installs the version from the package repository its entry names, not from GitHub. The
order is:

1. the service's release workflow publishes the signed GitHub release and its packages;
2. the packages reach the repository the entries name
   ([package-repositories.md](package-repositories.md));
3. then this workflow sets the desired version.

A version set before the repository holds its package is not refused by the coordinator: the hosts
ordered to it fail to install it and retry until the repository has it. Call this workflow once
the packages are in the repository, not straight after the release is published.

## Calling it

A service calls it from a workflow of its own, once the repository holds the release's packages.
The whole workflow, for a service that sets each release with its targets, is the
template of [A service's caller](service-caller.md), with the checklist of what must be set up
before its first run. The workflow lint of this repository holds that template to the rules of
`hello-desired.yml`, so it always pins the CI client's current revision. Its calling job:

- grants `contents: read` and `id-token: write`, and nothing else;
- calls `komira-ai/hostkeeper/.github/workflows/set-desired.yml` pinned to a full commit, with
  its release as the comment, and passes that release and commit as `hostkeeper_version` and
  `hostkeeper_commit` ([Pinning hostkeeper](#pinning-hostkeeper));
- passes `service`, `version` and `targets`, optionally `host_group` ([Host groups](#host-groups)), the coordinator's address, CA and audience, `tailnet_tag`, and the join inputs `join_tailnet`
  (true when `HOSTKEEPER_TAILNET_CLIENT_ID` is set), `tailnet_client_id`, `tailnet_audience` and
  `tailnet_coordinator`, from the job that read the release and from repository variables.

`targets` is what the service's own command wrote from the release's files ([Targets](#targets)).
A host is ordered the entry of its own platform; a host of a platform the targets do not list is
ordered nothing, and shows the hold `no_package_for_platform`
([Holds and force](../operations/holds-and-force.md)): that is not a failed update. The package
digest and the release index are no longer inputs; a caller pinned to an older revision of the
client may still pass them ([Pinning hostkeeper](#pinning-hostkeeper)).

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
  "komira-ai/hostkeeper/.github/workflows/set-desired.yml@2c767d1842a88edb7120073d2dff3b4bfccc834c",
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
| `host_groups` | absent | Optional. The host groups a token of the entry may deploy to ([Host groups](#host-groups)). Absent: any group that lists the service. |

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

## Host groups

A coordinator that holds host groups ([docs/operations/hostgroups.md](../operations/hostgroups.md))
keeps one desired version of a service for each group that lists it. The workflow's input
`host_group` names the group the version is set in and waited on. With it empty, as before the
input existed, the workflow names no group and the coordinator reads the call as one deployment to
each group that lists the service, each rolled by its own group's policy. A coordinator that holds
no host group takes the call as it always did.

A non-empty `host_group` needs a coordinator that holds host groups and the `hkctl` of a hostkeeper
release that has `--host-group`: with `host_group` empty the workflow passes no `--host-group`
flag at all, since hkctl writes every flag it is given into the request and a coordinator older
than its client refuses a field it does not know ([the gateway](../gateway.md)), so an empty input
sends the bytes it sent before. Only a non-empty one sends the field, and an older coordinator
then refuses it.

### Limiting a token to host groups

An allow-list entry may name `host_groups`, the groups its token may deploy to. Each name is a host
group name (1 to 32 bytes of `a-z`, `0-9` and `-`, starting with a letter, not ending with `-`
and holding no `--`); the coordinator
refuses to start on a name that is not one, or is empty or only whitespace.

```toml
services = ["hello"]
host_groups = ["canary", "prod"]
```

- With `host_groups` absent, a token may deploy its `services` to any group that lists them.
- With it present, a token may deploy only to those groups. `host_groups = []` names no group, so
  the token deploys nowhere.
- A set that names a group outside the list is refused `forbidden`, before the coordinator looks
  at whether the group exists or lists the service, and nothing is written.
- A set that names no group is one deployment to each group the token may deploy to that lists the
  service, in name order, one audit entry for each. When no group lists the service it is refused
  `not_in_host_group`; when some group lists it but none is the token's, `forbidden`.
  `if_generation` with no group is refused `ambiguous_host_group` when more than one of those
  groups lists the service (the count is over the groups the token may deploy to).
- A token with `host_groups` that sets on a coordinator that holds no host group is refused
  `forbidden`: its deployments are limited to groups, and there are none. The frozen route
  `POST /v1/desired` follows the same rules, as the call that names no group.
- The wait and the reads are not limited: `Releases.Converge`, `Releases.WaitConverged` and
  `Releases.GetRelease` answer any admitted token, as before. The list limits deployments only.

### Promotion across groups

The coordinator has no promotion rule; the order of groups is the caller's. A caller that wants a
canary group first calls the workflow in one job with `host_group` set to the canary group and
`wait` true, so the job ends green only when that group converged, and calls it again in a second
job that `needs` the first, with the next group. Each call is its own deployment with its own
audit entry, and a version marked bad is bad in every group, so a build that fails its canary
group is held back from the next. A call with `wait` false, or a first job that failed, promotes
nothing.

This repository's own callers, `release.yml` and `hello-desired.yml`, and the template of
[A service's caller](service-caller.md) stay pinned to the revision of `set-desired.yml` they
call now, which declares no `host_group` input, so they pass none: their set is one deployment to
each group that lists the service. `hostkeeper` is listed in every group, so a release of
hostkeeper is one deployment per group, each rolled by its own group's policy. Chaining the groups
in order lands when that pin moves to a revision that has the input, and the callers pass
`hostkeeper_version` and `hostkeeper_commit` of a release whose hkctl has `--host-group`.

## Inputs

| input | type | required | default | meaning |
|---|---|---|---|---|
| `service` | string | yes |  | The service whose desired version is set, as the coordinator names it. |
| `version` | string | yes |  | The version of the service to set. |
| `targets` | string | no |  | The targets of the version: one JSON object keyed by platform (`linux-x86_64`, `linux-arm64`, `macos-arm64`), each value `{"entries": [...]}` in the order the service prefers, under 128 KiB, the most the runner passes in one environment value or argument ([Targets](#targets)). Needs a coordinator that takes `SetDesired` with targets. |
| `host_group` | string | no |  | The host group the version is set in and waited on, as the coordinator names it ([Host groups](#host-groups)). Empty: each group that lists the service (for a token whose allow-list entry names `host_groups`, each of those that lists it), the call sending no group, as before this input. A non-empty value needs a coordinator that holds host groups and the `hkctl` of a hostkeeper release that has `--host-group`. |
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
| `reason` | Why the job ended where it did, one token: `converged`, `not_waited`, `halted`, `superseded`, `not_converged`, `not_stored`, `no_answer`, `coordinator_older` (the coordinator is older than the hkctl that made the call), or the refusal token the coordinator gave (for example `held_bad`, `target_invalid`, `invalid_wait` or `too_many_waits`). Empty when a step before the set failed (a verification, the install, the CA or the request), so the set never ran. |

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
| the set failed with `refused: coordinator_older`: hkctl got `invalid_request` and its call to the coordinator's `GET /v1/version` showed the coordinator older than hkctl | `coordinator_older` | `false` | empty: the version is not set |
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
`Releases.Converge`. Keep such a pin's `hostkeeper_version` at a release that has that verb:
newer releases of the `hostkeeper` binary have no `ci` verb (`hostkeeper ci` is a usage error,
exit status 2), so an older revision given a newer `hostkeeper_version` fails without setting
anything; move to the current revision instead. The two ways differ for the coordinator's
allow-list only in the commit of `job_workflow_ref`; move the pin and that entry together, as
above.

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
      # ... the inputs of the template (service-caller.md), with coordinator_url naming the coordinator
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
that tag instead (`tailnet_tag: tag:ci`) and needs no second credential. hostkeeper's own callers,
`release.yml` and `hello-desired.yml`, do that: each passes `tag:ci` from the repository variable
`HOSTKEEPER_TAILNET_TAG`, with no fallback, so the variable must be set
([The repository variables](github-actions.md#the-repository-variables)).

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
shared with other CI jobs, such as `tag:ci` (the tag of hostkeeper's own callers), keeps the
grants those jobs need, and the join runs with all of them: the stanza, with `tag:ci` in place of
`tag:hk-ci`, adds the coordinator's listener to them and takes nothing away.

This repository's own deploy ([Deploying hostkeeper from GitHub Actions](github-actions.md))
joins the tailnet in the CI client alone.

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
reads. The set's diagnostics also go to its step's own log, through `tee` (`2> >(tee <file> >&2)`),
and the step ends with hkctl's exit code, so a red set step names the cause: the coordinator's code
(`hkctl: the coordinator answered FAILED_PRECONDITION`) and the `refused: <token>` line, or `no
answer`. The set:

```sh
hkctl --endpoint https://<coordinator> --ca-file "$RUNNER_TEMP/hkctl-coordinator-ca.pem" \
  --github-oidc-audience "$AUDIENCE" --response-file "$RUNNER_TEMP/hkctl-set.json" \
  releases set-desired --service "$SERVICE" --request "$RUNNER_TEMP/hkctl-request.json" \
  --host-group web
```

where the request file is `{"desired":{"version":"<version>"},"targets":{...}}`: the canonical
JSON names of the request, the targets the object of the input as an object, not a string
([Targets](#targets)). The request step builds it with `jq --arg` from the step's environment.

Then the wait, with `--wait-secs` ten times `max_polls`:

```sh
hkctl --endpoint https://<coordinator> --ca-file "$RUNNER_TEMP/hkctl-coordinator-ca.pem" \
  --github-oidc-audience "$AUDIENCE" --response-file "$RUNNER_TEMP/hkctl-wait.json" \
  releases wait-converged --service "$SERVICE" --version "$VERSION" --wait-secs 1800 \
  --host-group web
```

`--host-group web` is what both calls pass when `host_group` is `web`. The workflow writes it as
the one word `${HOST_GROUP:+"--host-group=$HOST_GROUP"}` after the method's other flags, which bash
expands to nothing when `host_group` is empty and to the one argument `--host-group=web` otherwise:
an empty `host_group` sends no flag, so the request has no host group field and an older
coordinator takes it as it always did.

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
{"installer": "apt", "digest": "<64 hex>",
 "repository": "komira-testing", "packageVersion": "0.1.5",
 "manifest": {"package": "hello", "run": ["hello@a.service", "hello@b.service"],
              "hook": {...}, "minHostkeeper": "0.1.0", "installerBounds": {...}}}
```

An `apt` entry names the repository the host installs from (an entry of the host's own
repository configuration), the version of the package there, and the SHA-256 of the `.deb` the
host must find ([package-repositories.md](package-repositories.md), "The native packages and
their entries"). The manifest holds what the host needs to run the release: the units, the hook's
bounds, the readiness URL, the params and the oldest hostkeeper that takes it.

**Every list needs an entry its hosts can install.** No host is picked a `hostkeeper-archive`
entry: the archive installer is withdrawn ([packages-native.md](../design/packages-native.md),
section 10.1), and a host that runs the service from `apt` is picked its list's `apt` entry and no
other ([packages.md](../design/packages.md), section 3.3). A list of `hostkeeper-archive` entries
alone orders no host, but the coordinator still stores the set, and with `wait` false the job
still ends `not_waited`: each host shows `installer_dropped` (it runs the service from an
installer the list has no entry of) or `no_build_for_host` in `hkctl releases status --service
<service>`, and the version does not converge. A service whose hosts install it from a package
repository lists, for each Linux platform, the `apt` entry of its `.deb` as that repository
publishes it: `repository` the hosts' entry (the stem of the file
`/etc/hostkeeper/repositories/<entry>.sources`, such as `komira-testing`), `packageVersion` the
version the repository lists the package at (what `apt-get install <package>=<version>` takes, and
the `.deb`'s control `Version`, which the daemon checks), and `digest` the `.deb`'s SHA-256. A
version keeps the targets it was first set with (`release_exists`), so a set whose lists left
hosts out is put right by setting a new version with the right targets, or a version that was set
with no targets, no package digest and no release index.

This repository builds the targets of its own releases with `cargo xtask release targets
--service <hostkeeper|hello|mdm> [--fail-ready] --version <X.Y.Z> --dir <assets> --repo
komira-ai/hostkeeper`,
from the release's assets: its releases list native entries alone
([packages-native.md](../design/packages-native.md), section 10.5). For each Linux platform, the
one native entry of the package's `.deb`, an `apt` entry of the repository entry `komira-testing`
pinned by the `.deb`'s SHA-256, its manifest compiled into xtask; for macOS (hostkeeper and
hello) the one `macos-pkg` entry of the signed `.pkg`, in the store `komira-testing` at
`hostkeeper/<version>/<file>`, pinned by its SHA-256 and size. There are two forms of a Mac's
entry, and a coordinator takes both: the **explicit** one above (a location in a store of the Mac's
`stores.toml`, which each release sent before and which is unchanged), and the **named-version**
one, `{"installer": "macos-pkg", "repository": "komira-testing", "packageVersion": "<X.Y.Z>",
"digest": "<SHA-256>", "bytes": "<size>", "manifest": {...}}`, which names the channel, the version
and the file's size and leaves the location to the coordinator: it finds the `.pkg` in the repository
that serves the channel ([package-repositories.md](package-repositories.md#structure-of-a-repository)),
serves it to the Mac from the implicit `coordinator` store, and a Mac needs no `komira-testing` store.
`bytes` is required in the named-version form and refused on any other repository entry; a
coordinator before this form refuses it `target_invalid`, so a release writes it only for a
coordinator that has it. A host whose daemon announces
neither `apt` nor, on a Mac, `macos-pkg` (its signers file loaded, `/usr/sbin/installer` and
`/usr/sbin/pkgutil` present) is ordered nothing (`installer_dropped`, or `no_build_for_host`). `--repo` is read and not used. It refuses a release whose `.deb`s or their
bundles do not check out, and entries the coordinator or the daemon would refuse, and prints
`targets=<JSON>` (appended to `--output`). `--fail-ready` (hello only, explicit only) writes
hello's failing build instead: per Linux platform one `apt` entry of the repository entry
`komira-testing-fail` (the suite `testing-fail`) at the Debian version `<X.Y.Z>~fail.ready`,
pinned by the SHA-256 of the release's `hello_<X.Y.Z>-fail-ready_<arch>.deb`, its manifest
hello's; it refuses a `.deb` whose control is not hello at that version for the architecture, or
whose bundle does not list it. `hello-desired.yml` passes it for its `fail-ready` variant.

**What the coordinator checks.** It refuses targets that break the rules of a release
(`target_invalid`: an unknown platform, a list of no entry or more than 8, an installer twice, a
digest, size, location or manifest out of its bounds, or for the service `hostkeeper` a list that
is not one native entry), and the same version set before with other targets
(`release_exists`, do not retry: a release never changes; set a new version). A set with targets
is at most 256 KiB. `hkctl releases get-release --service <service>
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
| Write the request of the set | `targets` is empty or is not one JSON object (jq's error names the rule). `reason` is empty. The version is not set. | Give the targets of the release as one JSON object. |
| Set the desired version | The coordinator refused the call, and `reason` is its token: `unauthorized` or `forbidden` (no allow-list entry admits this repository, workflow, audience or service), `unknown_service`, `invalid_request` (a service or version the coordinator does not take), `not_in_host_group` (the group does not exist or does not list the service, or, with no `host_group`, no group lists it), `invalid_request` also when `host_group` is not a host group name, `ambiguous_host_group` (no `host_group`, and the set carries a generation while more than one group lists the service), `limits_in_host_group` (the request carries `max_updating` or `min_up`, which belong to the group's policy), `forbidden` also when the entry's `host_groups` do not include the group, or, with no `host_group`, include none that lists the service ([Host groups](#host-groups)), `held_bad` (the version is marked bad; only an operator can lift the mark, design 8.1), `keys_unavailable` (retry later), `coordinator_older` (the coordinator is older than the hostkeeper release whose hkctl made the call: it refused a field it does not know, and hkctl's one more call to the coordinator's `GET /v1/version` showed it older; do not retry, update the coordinator, [coordinator-update.md](coordinator-update.md), then run the job again), `target_invalid` (the targets break a rule of a release, [Targets](#targets)), `release_exists` (the version was set before with other targets; do not retry). `no_answer`: the coordinator could not be reached (with `join_tailnet` false this is what a GitHub-hosted runner gets, because the private listener is not reachable from it, [Reaching the coordinator](#reaching-the-coordinator)), GitHub issued no token (the calling job does not grant `id-token: write`), or hkctl refused a flag. `not_stored`: it answered with another version. The version is not set, except for `not_stored` and possibly `no_answer`: read `hkctl releases status --service <service>` before you retry. | Fix what the token names, then run the job again. |
| Wait for the fleet to converge | The version is set. `halted`: a host halted the service, and no host is ordered until an operator clears the halt. `superseded`: the desired version changed while the job waited (an automatic rollback, or a newer release). `not_converged`: `max_polls` times 10 seconds passed and the fleet had not converged; the roll goes on. `invalid_wait`: `max_polls` is above 180 (or below 1). `too_many_waits`: the coordinator already holds 16 waits on its listener; run the job again. `no_answer`: the wait was cut (a restart of the coordinator, the network) or got no token. | Look at the service in the coordinator's UI or with `hkctl releases status`. |

## What it does not do

- It does not change the coordinator's allow-list, the service's definition, holds or marks.
- It does not build, sign or publish the service's package or its targets; they and their digests
  come from the service's own release job. It does not check the targets beyond their being one
  JSON object.
- It does not retry a refused call: fix what the token names and run the job again. It does not
  call again after a cut wait.
