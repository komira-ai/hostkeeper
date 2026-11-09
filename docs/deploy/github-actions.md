# Deploying hostkeeper from GitHub Actions

A push to `main` that changes code becomes a hostkeeper release by itself
([release.yml](../../.github/workflows/release.yml)). With the repository variable
`RELEASE_SETS_DESIRED` set to `true`, the same run then deploys that release to the fleet: it
waits until the release channel holds the release, and sets it as the desired version of the
service `hostkeeper` on the coordinator, through one call with an OIDC token. The coordinator's
rollout gate does the rest: the service's update plan rolls the release out stage by stage, the
canary or first stage of hosts first, then more hosts once each stage has baked.

CI uploads nothing and holds no secret. The release reaches the channel through the channel's own
sync, which pulls from GitHub; the coordinator call's one credential is the OIDC token GitHub
issues to the job.

## The path, end to end

1. **Build and sign.** A push to `main` runs `ci`, `privileged` and `artifacts`. `artifacts`
   builds every file of the release and attests it ([Verify a release](verify-a-release.md)).
2. **Release.** When all three have passed for the commit, `release.yml` plans the release
   (`plan`), checks every file and attestation again (`verify`), and publishes the GitHub
   release `v<X.Y.Z>` (`publish`). Nothing is rebuilt.
3. **Into the channel.** `cargo xtask release channel-sync` runs on the host that serves the
   release channel, from a systemd timer or a compose sidecar, ten minutes after its last run
   ends. It finds the new release, checks it as a host would, and stages it as
   `<channel>/hostkeeper/<X.Y.Z>/` ([Stage a release into the release channel](release-channel.md#sync-new-releases-into-the-channel)).
   CI does not push into the channel: nothing in a workflow can write to that host.
4. **Wait for the channel** (job `channel` of `release.yml`). The job takes the newest release
   the run published and its release index, `index.json`, from the files `verify` checked. When
   `HOSTKEEPER_TAILNET_CLIENT_ID` is set, it first joins the tailnet as the CI client does (the
   same pinned action and Tailscale tarball, the same repository variables, an ephemeral node
   tagged `HOSTKEEPER_TAILNET_TAG` authenticated with an OIDC token), and waits until the
   channel's host, the host of `HOSTKEEPER_RELEASE_CHANNEL` without its port or path, answers a
   `tailscale ping` (the host is taken out of the URL by the xtask of the workflow's own commit,
   since the release's commit may be older than the workflow and lack that verb): a channel
   served only on the tailnet is reachable from the runner. Empty, it
   does not join, and the channel must be reachable from the runner as it is. It reads
   `<channel>/hostkeeper/<X.Y.Z>/index.json` every 40 seconds, a 404 included, until the
   channel serves it, for at most 30 minutes. The served index must then be the published one,
   byte for byte. Then `cargo xtask release targets`, compiled from the release's own commit,
   writes the release's targets from the same checked files: one `hostkeeper-archive` entry per
   platform the index lists, with the archive's two locations (the channel's path, then the
   release asset), its size and SHA-256, its attestation bundle and the `min_hostkeeper` of its
   manifest ([packages design](../design/packages.md), section 3.7). A file of the release that
   does not check out fails the job, and nothing is set. Last, it lists the published releases:
   when a higher version than the run's is published already (a late run of an older commit,
   released after a newer one), the next step is skipped, so that the fleet is never moved back.
   The job reads the repository (`contents: read`) and holds `id-token: write` for the join, and
   no other token right.
5. **Set the desired version** (job `set-desired` of `release.yml`). The job calls the CI client
   ([set-desired.yml](ci-client.md)) once: it verifies the pinned hostkeeper release's `hkctl`,
   joins the tailnet when `HOSTKEEPER_TAILNET_CLIENT_ID` is set, and runs `hkctl releases set-desired` for the
   service `hostkeeper` at `<X.Y.Z>`; hkctl requests the OIDC token itself and hands it to the
   call through a pipe, never a file or an argument. The call names the release
   by its targets, and by its index and the index's SHA-256, so each host is ordered the package
   of its own platform: a daemon that reads targets its platform's entry, one before them the
   package the index lists. `hkctl releases get-release --service hostkeeper --version <X.Y.Z>`
   answers the targets the coordinator stored, one list per platform.
   The job does not wait for the fleet (`wait: false`); it ends when the coordinator has stored
   the version, with `reason` `not_waited`.
6. **The coordinator checks the call.** The allow-list entry must admit the token and name the
   service `hostkeeper` ([below](#the-allow-list-entry)). With `--release-channel`, the
   coordinator reads the channel's index of the version itself and refuses a version the channel
   does not hold (`not_in_channel`) or holds with other bytes (`channel_digest_mismatch`). A
   version below the service's `min_version`, or marked bad, is refused too. Nothing changes on a
   refusal.
7. **The update plan orders the hosts.** The `[[service]]` table of hostkeeper in the rollout file
   may set `stages` (by default `[1, 10, 50, 100]`: the cumulative percent of the eligible hosts
   that run the version once each stage is done) and `canary = { count = n }` (a first stage of
   `n` hosts). The coordinator orders only the hosts of the first stage; the next stage starts
   when every host of the stage has reported the new version healthy for `bake_beats`
   heartbeats (a stage marked `advance = "manual"` waits for an operator's `Rollouts.Advance`).
   The hosts of a stage update in parallel, up to the desired state's `max_updating` for each
   role (by default 1, or 2 for `daemon` and `mac-daemon`). A host whose update fails keeps its
   update slot until it is healthy, and its stage is not done until an operator resolves it, so
   the next stage is not ordered the version meanwhile
   ([rollout-plans.md](../operations/rollout-plans.md); design, section 5.4).
8. **Each host updates itself.** At its next heartbeat an ordered host fetches its platform's
   package from the channel, checks it against the index and its attestation, and updates
   hostkeeper (design, section 6.6).

Watch the roll on the UI's Rollout page, or with `hkctl releases converge --service hostkeeper`.

## Turning it on and off

The deploy runs only while the repository variable `RELEASE_SETS_DESIRED` is `true`, exactly:

```sh
gh variable set RELEASE_SETS_DESIRED --repo komira-ai/hostkeeper --body true    # on
gh variable delete RELEASE_SETS_DESIRED --repo komira-ai/hostkeeper             # off
```

Unset, or any other value (`false` included), the jobs `channel` and `set-desired` are skipped
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
| `HOSTKEEPER_RELEASE_CHANNEL` | The base URL of the release channel, `https://<host>[:<port>][/<path>]`, the same as the daemon config's `release_channel` and the coordinator's `--release-channel`. The runner of the job `channel` must reach it: on the tailnet, when `HOSTKEEPER_TAILNET_CLIENT_ID` is set (then `<host>` is the channel host's name or address on the tailnet, letters, digits, `.` and `-`), or as it is otherwise. |
| `HOSTKEEPER_COORDINATOR_URL` | The coordinator's private listener, `https://<host>[:<port>]` (shared with `hello-desired.yml`). |
| `HOSTKEEPER_COORDINATOR_CA` | The PEM certificates of the CA the coordinator's certificate chains to (shared). |
| `HOSTKEEPER_CI_AUDIENCE` | The audience of the OIDC token, as the allow-list entry names it (shared). |
| `HOSTKEEPER_TAILNET_CLIENT_ID` | The client ID of the tailnet's federated identity (shared). Set: the job `channel` and both calls of the CI client join the tailnet. Empty: none of them joins, and the channel and the coordinator must be reachable from the runner some other way. |
| `HOSTKEEPER_TAILNET_AUDIENCE` | The audience of that federated identity (shared). |
| `HOSTKEEPER_TAILNET_COORDINATOR` | The coordinator's name on the tailnet, which the CI client's join pings until it answers (shared). The job `channel` pings the channel's host instead. |
| `HOSTKEEPER_TAILNET_TAG` | The tag the join gives the runner's node, the one the federation's trust credential issues: `tag:ci` (shared). Both callers pass it as `tailnet_tag`, and the job `channel` joins with it. It must be set, even with `HOSTKEEPER_TAILNET_CLIENT_ID` empty: the CI client checks the tag at its first step, with or without the join, and an empty one ends the job there, before anything is downloaded or set. |

The tailnet, its tag (`HOSTKEEPER_TAILNET_TAG`, `tag:ci`), its ACL and its federation
are set up as for any caller of the CI client
([Reaching the coordinator](ci-client.md#reaching-the-coordinator),
[The tag and the ACL](ci-client.md#the-tag-and-the-acl)). For this
repository, the federation's subject is
`repo:komira-ai/hostkeeper:ref:refs/heads/main`, its trust credential issues `tag:ci`, and the
ACL's stanza names `tag:ci` in place of `tag:hk-ci`. `tag:ci` is shared with other CI jobs, so
the join keeps the grants they need; the stanza adds the coordinator's private listener to them
and takes nothing away. For a channel served only on the tailnet, the ACL also grants `tag:ci`
the channel's host and port (the port of `HOSTKEEPER_RELEASE_CHANNEL`, 443 when it names none):
the job `channel` joins with that tag, pings the channel's host and reads the channel there. The callers name no fallback tag: the CI client's default, `tag:hk-ci`,
is not a tag this repository's credential issues, so an unset variable fails at the client's
first step rather than at the join.

## Set up once, outside CI

Each of these is a change an operator makes; nothing in CI writes them.

**The channel's sync** runs on the channel's host with `--trust-list` naming its trust list,
which lists hostkeeper's services with their signer, `artifacts.yml` on `refs/heads/main`
([channel-sync](channel-sync/), [The trust list](release-channel.md#the-trust-list)).

**The coordinator** is started with `--release-channel` naming the channel, so that it checks
every set against it ([The release channel check](ci-client.md#the-release-channel-check)).

**hostkeeper's rollout** is a `[[service]]` table of the rollout file (`--rollout-file`), with
its canary and bake:

```toml
[[service]]
name = "hostkeeper"
roles = ["storage", "daemon"]   # the roles of the fleet's hosts
bake_beats = 3                  # healthy heartbeats at the new version before the next host
canary = { count = 1 }          # one host goes first, alone, and bakes
```

**The allow-list entry.** `release.yml` calls the CI client as `hello-desired.yml` does, at the
same revision ([Pinning the client](#pinning-the-client)), so the entry for this repository
([The allow-list entry](ci-client.md#the-allow-list-entry)) admits it with two changes:
`event_names` adds `workflow_run` and `schedule`, the events `release.yml` runs on, and
`services` adds `hostkeeper`. `job_workflow_ref` names `set-desired.yml` at the commit both
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
```

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
coordinator is updated to one that does before the pin moves. The workflow
lint (`cargo test -p xtask --test workflows`, `callers.rs`) names that revision, as
`CLIENT_COMMIT` and `CLIENT_RELEASE`, and refuses a caller whose `uses:` commit, version comment,
`hostkeeper_version` or `hostkeeper_commit` is not it. The pin moves in a reviewed pull request
that changes those two values and both callers together, after the allow-list entry names the
new commit (above).

## When it fails

A failure of either job leaves the release published; only the deploy did not happen. Once the
cause is fixed, use **Re-run failed jobs** on the release run: it runs `channel` and
`set-desired` again, and the call gets a new token.

| job and step | what it means | what to do |
|---|---|---|
| `channel`: The release channel's host | Only with `HOSTKEEPER_TAILNET_CLIENT_ID` set: `HOSTKEEPER_RELEASE_CHANNEL` is empty or not `https://<host>[:<port>][/<path>]`. Nothing is joined or set. | Fix the variable, then re-run. |
| `channel`: Join the tailnet | Only with `HOSTKEEPER_TAILNET_CLIENT_ID` set: the runner did not join (the federated identity or its tag, as for the CI client's join), or the channel's host did not answer a `tailscale ping` (it is not on the tailnet under that name, or the ACL does not grant the tag the channel's host). Nothing is set. | Fix the tailnet variables, the channel's name or the ACL, then re-run. |
| `channel`: Wait until the release channel serves the version's index | The channel did not serve `hostkeeper/<X.Y.Z>/index.json` within 30 minutes, or `HOSTKEEPER_RELEASE_CHANNEL` is empty or not `https://`, or the runner cannot reach the channel (`Could not resolve host`: a channel served only on the tailnet, while `HOSTKEEPER_TAILNET_CLIENT_ID` is empty; or the ACL does not grant the tag the channel's port). Nothing is set. | Look at the channel sync's log (`systemctl status hostkeeper-channel-sync.service`, or the sidecar's): a release it refused is listed there. Re-run once it is staged. For a channel the runner cannot reach, set the tailnet variables or fix the ACL. |
| `channel`: The channel serves the published index, byte for byte | The channel holds another index for this version. A release in the channel never changes, so this does not mend itself. Nothing is set. | Find out how the channel came to hold other bytes; fix the channel by hand. |
| `set-desired`: any verification step | The pinned hostkeeper release is not the one that was built ([When it fails](ci-client.md#when-it-fails)). Nothing is set. | Check the pin. |
| `set-desired`: Set the desired version | The coordinator refused the call, and `reason` is its token: `unauthorized` or `forbidden` (the allow-list entry, above), `not_in_channel` (the coordinator does not reach the channel the runner reached, or reads another one), `held_bad`, `below_min_version`, `no_answer` (the coordinator is not reachable: the tailnet variables) ([the full table](ci-client.md#when-it-fails)). | Fix what the token names, then re-run. |

A run that published several releases (each run releases every commit since the last release whose
runs are done, so one run can publish several) sets only the newest:
the others are in the channel, and a host goes straight to the newest. A run whose newest release
is older than one already published sets nothing: `set-desired` is skipped, not failed.

## What it does not do

- It uploads nothing, to the channel or anywhere else.
- It does not wait for the fleet to converge: a roll by stages, each baked before the next, takes
  longer than a release run should hold the release group, and a slow roll is not a failed
  release.
- It does not change the coordinator's allow-list, rollout file, holds or marks.
- It sets only `hostkeeper`. The hello proof run stays a dispatch by hand
  ([docs/hello.md](../hello.md)).
