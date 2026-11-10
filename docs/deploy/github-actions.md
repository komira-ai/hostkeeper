# Deploying hostkeeper from GitHub Actions

A push to `main` that changes code becomes a hostkeeper release by itself
([release.yml](../../.github/workflows/release.yml)). With the repository variable
`RELEASE_SETS_DESIRED` set to `true`, the same run then deploys that release to the fleet: it
writes the release's targets, hostkeeper's native package for each Linux platform, and sets it as
the desired version of the service `hostkeeper` on the coordinator, through one call with an OIDC
token. The call names no host group, so it is one deployment to each group that lists
`hostkeeper` (every group does). The coordinator's rollout gate does the rest: each group's
update plan rolls the release out over that group's hosts stage by stage, the canary or first
stage of hosts first, then more hosts once each stage has baked.

The release also waits for the coordinator. CI never changes the coordinator: its own machine
updates it from each published release
([coordinator-update.md](coordinator-update.md)), and the deploy sets the hosts' desired version
only once the coordinator runs that release, because a coordinator older than the release refuses
the fields the release sends (`invalid_request`).

CI holds no secret for the deploy: a release is set
with native entries alone ([packages-native.md](../design/packages-native.md), section 10.5), which
a host installs from the project's package repository, where the release's own package-channel
jobs put its `.deb` ([package-repositories.md](package-repositories.md)). The coordinator call's
one credential is the OIDC token GitHub issues to the job.

## The path, end to end

1. **Build and sign.** A push to `main` runs `ci`, `privileged` and `artifacts`. `artifacts`
   builds every file of the release and attests it ([Verify a release](verify-a-release.md)).
2. **Release.** When all three have passed for the commit, `release.yml` plans the release
   (`plan`), checks every file and attestation again (`verify`), and publishes the GitHub
   release `v<X.Y.Z>` (`publish`). Nothing is rebuilt.
3. **Into the repository.** The release's package-channel jobs (`testing-apt-*`) put the
   daemon's `.deb` into the repository's `testing` suite. The deploy does not wait for the
   repository: a host ordered before the `.deb` is in the suite defers its `stage`
   (`not_in_repository`, an environment deferral) and tries again on the deferral's schedule.
4. **Write the targets** (job `targets` of `release.yml`). The job takes the newest release the
   run published, and its files `verify` checked, from the run's own artifact. Then
   `cargo xtask release targets`, compiled from the release's own commit, writes the release's
   targets from those files: per Linux platform, hostkeeper's native entry alone, an `apt` entry
   of the daemon's `.deb` in the repository's `testing` suite pinned by its SHA-256, its
   manifest made by `release targets` itself (its hook bounds, run unit and `min_hostkeeper`); for
   macOS its signed `.pkg`'s `macos-pkg` entry alone, which a Mac whose daemon announces
   `macos-pkg` takes (any other Mac is ordered nothing) ([packages design](../design/packages.md),
   section 3.7). A file of the release that does not check out fails the job, and nothing is set. Last,
   it lists the published releases: when a higher version than the run's is published already
   (a late run of an older commit, released after a newer one), the next step is skipped, so
   that the fleet is never moved back. The job reads the repository (`contents: read`) and holds
   no other token right; it joins no tailnet.
5. **Wait for the coordinator** (job `coordinator` of `release.yml`, after `targets`, under
   the condition `set-desired` has). The coordinator is updated by its own machine
   (`hostkeeper-coordinator-update`, a release asset, on a timer there, [coordinator-update.md](coordinator-update.md)),
   not by CI. The job joins the tailnet as `set-desired` does (the same pinned action and the
   same repository variables, after a check of `HOSTKEEPER_TAILNET_TAG`), writes the CA from
   `HOSTKEEPER_COORDINATOR_CA`, and asks the private listener, with one `curl` that retries for
   30 minutes, `GET /v1/version?at_least=<X.Y.Z>`. The route takes no token: it answers 200 when
   the coordinator is at least that version, 409 (`coordinator_older`) while it is older, and
   a coordinator from before the route answers 404; curl retries until 200. The job holds
   `id-token: write` for the tailnet join alone, reads no repository and runs none of our code
   (curl, jq and the pinned tailscale action). curl writes the last answer's HTTP status to the
   step's output. When the 30 minutes pass, the job fails, `set-desired` does not run (the hosts'
   desired version is not set), and one error annotation says why: after a 409 or a 404, `No
   coordinator updater reported this release (<X.Y.Z>) (is the timer installed?)`; after anything
   else (000, no answer: the tailnet join, the CA or the URL variable), that the coordinator did
   not answer, with the last status.
6. **Set the desired version** (job `set-desired` of `release.yml`, which needs `coordinator`). The job calls the CI client
   ([set-desired.yml](ci-client.md)) once: it verifies the pinned hostkeeper release's `hkctl`,
   joins the tailnet when `HOSTKEEPER_TAILNET_CLIENT_ID` is set, and runs `hkctl releases set-desired` for the
   service `hostkeeper` at `<X.Y.Z>`; hkctl requests the OIDC token itself and hands it to the
   call through a pipe, never a file or an argument. The call names the release by its targets
   alone, so each host is ordered its own platform's native package; a
   daemon before `target-v1`, or one that announces no `apt`, is ordered nothing
   (`installer_dropped`): stage 9's counts are zero before this release is set.
   `hkctl releases get-release --service hostkeeper --version <X.Y.Z>` answers the targets the
   coordinator stored, one list per platform (`--host-group <group>` asks as that group, which must
   list `hostkeeper`).
   The job does not wait for the fleet (`wait: false`); it ends when the coordinator has stored
   the version, with `reason` `not_waited`.
7. **The coordinator checks the call.** The allow-list entry must admit the token and name the
   service `hostkeeper` ([below](#the-allow-list-entry)). A version below the
   service's `min_version`, or marked bad, is refused. A coordinator that holds host groups
   makes one deployment to each group that lists `hostkeeper` and the entry may deploy to (an
   entry with `host_groups` limits them); a refusal for any group writes nothing for any, and no
   group that lists it is `not_in_host_group`. Nothing changes on a refusal.
8. **The update plan orders the hosts.** Each group's policy sets the plan of its hosts
   ([below](#set-up-once-outside-ci)): `stages` (by default `[1, 10, 50, 100]`: the cumulative
   percent of the group's eligible hosts that run the version once each stage is done) and
   `canary` (a first stage of `count` hosts). The coordinator orders only the hosts of the first
   stage; the next stage starts
   when every host of the stage has reported the new version healthy for `bake_beats`
   heartbeats (a stage marked `advance` manual waits for an operator's `Rollouts.Advance`).
   The hosts of a stage update in parallel, up to the policy's `max_updating` (by default 1).
   Two groups never wait on each other, so the release rolls in every group at once, each by its
   own policy; the caller chaining the groups one after the other (a canary group first) is a
   later change, with the CI client's `host_group` input. A host whose update fails keeps its
   update slot until it is healthy, and its stage is not done until an operator resolves it, so
   the next stage is not ordered the version meanwhile
   ([rollout-plans.md](../operations/rollout-plans.md); design, section 5.4).
9. **Each host updates itself.** At its next heartbeat an ordered host stages the `.deb` from its
   repository entry, checks it against the pinned SHA-256, and updates hostkeeper by the
   hand-over, installing the package once the new binary has confirmed itself
   ([packages-native.md](../design/packages-native.md), section 10.4).

Watch the roll on the UI's Rollout page, or with `hkctl releases converge --service hostkeeper`.

## Turning it on and off

The deploy runs only while the repository variable `RELEASE_SETS_DESIRED` is `true`, exactly:

```sh
gh variable set RELEASE_SETS_DESIRED --repo komira-ai/hostkeeper --body true    # on
gh variable delete RELEASE_SETS_DESIRED --repo komira-ai/hostkeeper             # off
```

Unset, or any other value (`false` included), the jobs `targets`, `coordinator` and `set-desired` are skipped
and `release.yml` releases as before. Turning it off stops new desired versions from CI only: a
roll already in progress goes on, and an operator holds or halts it on the coordinator
([Holds and force](../operations/holds-and-force.md)). `RELEASE_FREEZE` is the other switch: it
stops the releases themselves, and with them the deploy.

## The repository variables

None of these is a secret: the coordinator's address and CA, the audience, and the tailnet
federation's client ID are all public values, so they are variables, not secrets.

| variable | meaning |
|---|---|
| `RELEASE_SETS_DESIRED` | `true` turns the deploy on. |
| `HOSTKEEPER_COORDINATOR_URL` | The coordinator's private listener, `https://<host>[:<port>]` (shared with `hello-desired.yml`). |
| `HOSTKEEPER_COORDINATOR_CA` | The PEM certificates of the CA the coordinator's certificate chains to (shared). |
| `HOSTKEEPER_CI_AUDIENCE` | The audience of the OIDC token, as the allow-list entry names it (shared). |
| `HOSTKEEPER_TAILNET_CLIENT_ID` | The client ID of the tailnet's federated identity (shared). Set: both calls of the CI client and the job `coordinator` join the tailnet. Empty: none joins, and the coordinator must be reachable from the runner some other way. |
| `HOSTKEEPER_TAILNET_AUDIENCE` | The audience of that federated identity (shared). |
| `HOSTKEEPER_TAILNET_COORDINATOR` | The coordinator's name on the tailnet, which the CI client's join pings until it answers (shared). |
| `HOSTKEEPER_TAILNET_TAG` | The tag the join gives the runner's node, the one the federation's trust credential issues: `tag:ci` (shared). Both callers pass it as `tailnet_tag`. It must be set, even with `HOSTKEEPER_TAILNET_CLIENT_ID` empty: the CI client checks the tag at its first step, with or without the join, and an empty one ends the job there, before anything is downloaded or set. |

The tailnet, its tag (`HOSTKEEPER_TAILNET_TAG`, `tag:ci`), its ACL and its federation
are set up as for any caller of the CI client
([Reaching the coordinator](ci-client.md#reaching-the-coordinator),
[The tag and the ACL](ci-client.md#the-tag-and-the-acl)). For this
repository, the federation's subject is
`repo:komira-ai/hostkeeper:ref:refs/heads/main`, its trust credential issues `tag:ci`, and the
ACL's stanza names `tag:ci` in place of `tag:hk-ci`. `tag:ci` is shared with other CI jobs, so
the join keeps the grants they need; the stanza adds the coordinator's private listener to them
and takes nothing away. The job `coordinator` reaches that same private listener over the tailnet
with no token, as `tag:ci`, so the stanza that lets `tag:ci` reach the listener for `set-desired`
is the one it needs: no further grant. The callers name no fallback tag: the CI client's default, `tag:hk-ci`,
is not a tag this repository's credential issues, so an unset variable fails at the client's
first step rather than at the join.

## Set up once, outside CI

Each of these is a change an operator makes; nothing in CI writes them.

**The project's package repository** holds the release's `.deb` in its `testing` suite, and each
host has its repository entry and key ([package-repositories.md](package-repositories.md),
[move-to-native.md](../operations/move-to-native.md)). The coordinator's
`--release-channel` is not read by this deploy; the operator drops it once stage 9's release has
converged on every host.

**hostkeeper's rollout** is the policy of each host group, which lists `hostkeeper` besides its
other packages; the policy is the one each group's `hkctl host-groups put` carries
([hostgroups.md](../operations/hostgroups.md#policy)), with its canary and bake:

```json
{ "hostGroup": { "name": "storage",
    "packages": ["hostkeeper", "my-service"],
    "policy": { "canary": { "count": 1 },
                "bakeBeats": 3 } } }
```

`bakeBeats` is the healthy heartbeats at the new version before the next stage, and `canary` one
host that goes first, alone, and bakes. On a coordinator that holds no group, a `[[service]]`
table of the rollout file (`--rollout-file`) with `name = "hostkeeper"`, `roles`, `bake_beats`
and `canary` does this instead.

**The allow-list entry.** `release.yml` calls the CI client as `hello-desired.yml` does, at the
same revision ([Pinning the client](#pinning-the-client)), so the entry for this repository
([The allow-list entry](ci-client.md#the-allow-list-entry)) admits it with two changes:
`event_names` adds `workflow_run` and `schedule`, the events `release.yml` runs on, and
`services` adds `hostkeeper`; `host_groups` may name the groups the token may deploy to (left
out, any group that lists the service). `job_workflow_ref` names `set-desired.yml` at the commit both
callers pin, once:

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
event_names = ["workflow_dispatch", "workflow_run", "schedule"]
runner_environment = "github-hosted"
services = ["hello", "hostkeeper"]
host_groups = ["storage", "farm"]       # optional: left out, any group that lists the service
```

With `host_groups`, a set that names a group not in the list is refused `forbidden`; a set that
names none is one deployment to each group of the list that lists the service; it is `forbidden`
when other groups list the service but none of the list does, and `not_in_host_group` when no
group lists it. On a coordinator that holds no group, an entry with `host_groups`
is refused every set (`forbidden`): its deployments are limited to groups, and there are none.

Restart the coordinator after changing the file: it reads the allow-list when it starts.

**Moving the pin.** Add the new commit to `job_workflow_ref` and restart the coordinator before
the pull request that moves the pin merges, or the first run after it is refused
(`unauthorized`). Keep the old commit listed while a run of the old pin may still call: a
**Re-run failed jobs** of a release run from before the move runs that run's `release.yml`, with
its old pin. Remove it once the coordinator's audit log shows no call from it; that removal is
what ends the old revision's path.

## Pinning the client

`release.yml` pins the CI client like any caller ([Pinning hostkeeper](ci-client.md#pinning-hostkeeper)):
the `uses:` line names a commit of this repository with its release as the comment, and
`hostkeeper_version` and `hostkeeper_commit` name that same release. The program that holds the
OIDC token is a published release, verified before it runs, never the commit being released.

Both callers of this repository, `release.yml` and `hello-desired.yml`, pin one revision: commit
`2c767d1842a88edb7120073d2dff3b4bfccc834c`, release `v0.1.184`, the first whose
`set-desired.yml` takes the tailnet tag (`tailnet_tag`), on top of a release's targets and hkctl
(one set, and for `hello-desired.yml` one held wait). A coordinator that does not take `SetDesired` with targets refuses their sets; the
coordinator is updated to one that does before the pin moves. For a release, the job `coordinator`
holds `set-desired` until the coordinator runs that release. The workflow
lint (`cargo test -p xtask --test workflows`, `callers.rs`) names that revision, as
`CLIENT_COMMIT` and `CLIENT_RELEASE`, and refuses a caller whose `uses:` commit, version comment,
`hostkeeper_version` or `hostkeeper_commit` is not it. The pin moves in a reviewed pull request
that changes those two values and both callers together, after the allow-list entry names the
new commit (above).

## When it fails

A failure of any of the three jobs leaves the release published; only the deploy did not happen.
Once the cause is fixed, use **Re-run failed jobs** on the release run: it runs `targets`,
`coordinator` and `set-desired` again (those that failed, and those that depend on them), and the
call gets a new token.

| job and step | what it means | what to do |
|---|---|---|
| `targets`: The targets of the release | A file of the release does not check out: one of hostkeeper's `.deb`s, its bundle or its control file. Its log names every problem. Nothing is set. | Fix the release (a new commit releases again), then the next run sets it. |
| `coordinator`: Wait until the coordinator runs this release | After 30 minutes the coordinator was still older than the release (409 `coordinator_older`, or 404 from one before the route), or not reachable (the tailnet join, the CA or the URL variables). The job's error annotation says which (`No coordinator updater reported this release ... (is the timer installed?)` for the first), and `set-desired` did not run: the hosts' desired version is not set. | On the coordinator's machine, install the timer of `hostkeeper-coordinator-update` from the release asset if it is not there, or read its journal ([coordinator-update.md](coordinator-update.md)); once the coordinator runs the release, **Re-run failed jobs**. |
| `set-desired`: any verification step | The pinned hostkeeper release is not the one that was built ([When it fails](ci-client.md#when-it-fails)). Nothing is set. | Check the pin. |
| `set-desired`: Set the desired version | The coordinator refused the call, and `reason` is its token: `unauthorized` or `forbidden` (the allow-list entry, above, its `host_groups` included), `not_in_host_group` (no group lists `hostkeeper`), `limits_in_host_group` (the set carried `max_updating` or `min_up`), `target_invalid` (a list the coordinator's checks refuse), `held_bad`, `below_min_version`, `no_answer` (the coordinator is not reachable: the tailnet variables) ([the full table](ci-client.md#when-it-fails)). | Fix what the token names, then re-run. |

A run that published several releases (each run releases every commit since the last release whose
runs are done, so one run can publish several) sets only the newest:
a host goes straight to the newest. A run whose newest release
is older than one already published sets nothing: `set-desired` is skipped, not failed.

## What it does not do

- The deploy jobs upload nothing. The job `coordinator` sends the
  coordinator one read-only request and no token.
- It does not wait for the fleet to converge: a roll by stages, each baked before the next, takes
  longer than a release run should hold the release group, and a slow roll is not a failed
  release.
- It never changes the coordinator, not its version, allow-list, rollout file, holds or marks:
  the coordinator's own machine updates it ([coordinator-update.md](coordinator-update.md)), and
  CI only waits until it runs the release.
- It sets only `hostkeeper`. The hello proof run stays a dispatch by hand
  ([docs/hello.md](../hello.md)).
