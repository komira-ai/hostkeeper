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
   the run published and its release index, `index.json`, from the files `verify` checked. It
   reads `<channel>/hostkeeper/<X.Y.Z>/index.json` every 40 seconds, a 404 included, until the
   channel serves it, for at most 30 minutes. The served index must then be the published one,
   byte for byte. Last, it lists the published releases: when a higher version than the run's is
   published already (a late run of an older commit, released after a newer one), the next step
   is skipped, so that the fleet is never moved back. The job reads the repository
   (`contents: read`) and holds no other token right.
5. **Set the desired version** (job `set-desired` of `release.yml`). The job calls the CI client
   ([set-desired.yml](ci-client.md)) once: it verifies the pinned hostkeeper release, joins the
   tailnet when the job is configured to, requests an OIDC token, and calls
   `releases set-desired` for the service `hostkeeper` at `<X.Y.Z>`. The call names the release
   by its index and the index's SHA-256, so each host is ordered the package of its own platform.
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
| `HOSTKEEPER_RELEASE_CHANNEL` | The base URL of the release channel, `https://<host>[/<path>]`, the same as the daemon config's `release_channel` and the coordinator's `--release-channel`. A GitHub-hosted runner must reach it. |
| `HOSTKEEPER_COORDINATOR_URL` | The coordinator's private listener, `https://<host>[:<port>]` (shared with `hello-desired.yml`). |
| `HOSTKEEPER_COORDINATOR_CA` | The PEM certificates of the CA the coordinator's certificate chains to (shared). |
| `HOSTKEEPER_CI_AUDIENCE` | The audience of the OIDC token, as the allow-list entry names it (shared). |
| `HOSTKEEPER_TAILNET_CLIENT_ID` | The client ID of the tailnet's federated identity. Empty: the job does not join the tailnet, and the coordinator must be reachable from the runner some other way. |
| `HOSTKEEPER_TAILNET_AUDIENCE` | The audience of that federated identity. |
| `HOSTKEEPER_TAILNET_COORDINATOR` | The coordinator's name on the tailnet, which the join pings until it answers. |

The tailnet, its tag `tag:hk-ci`, its ACL and its federation are set up as for any caller of the
CI client ([Reaching the coordinator](ci-client.md#reaching-the-coordinator)). For this
repository, the federation's subject is
`repo:komira-ai/hostkeeper:ref:refs/heads/main`.

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

**The allow-list entry.** `release.yml` calls the CI client as `hello-desired.yml` does, so the
entry for this repository ([The allow-list entry](ci-client.md#the-allow-list-entry)) admits it
with three changes: `job_workflow_ref` adds `set-desired.yml` at the commit `release.yml` pins
(listed once when it is the same commit `hello-desired.yml` pins), `event_names` adds
`workflow_run` and `schedule`, the events `release.yml` runs on, and `services` adds `hostkeeper`:

```toml
[[remote_operator]]
issuer = "https://token.actions.githubusercontent.com"
audience = "hostkeeper-ci"
repository_id = "123456789"            # gh api repos/komira-ai/hostkeeper --jq .id
repository = "komira-ai/hostkeeper"
job_workflow_ref = [
  "komira-ai/hostkeeper/.github/workflows/set-desired.yml@<the commit hello-desired.yml pins>",
  "komira-ai/hostkeeper/.github/workflows/set-desired.yml@<the commit release.yml pins>",
]
refs = ["refs/heads/main"]
event_names = ["workflow_dispatch", "workflow_run", "schedule"]
runner_environment = "github-hosted"
services = ["hello", "hostkeeper"]
```

Restart the coordinator after changing the file: it reads the allow-list when it starts.

## Pinning the client

`release.yml` pins the CI client like any caller ([Pinning hostkeeper](ci-client.md#pinning-hostkeeper)):
the `uses:` line names a commit of this repository with its release as the comment, and
`hostkeeper_version` and `hostkeeper_commit` name that same release. The program that holds the
OIDC token is a published release, verified before it runs, never the commit being released. The
pin moves in a reviewed pull request, together with the allow-list entry's `job_workflow_ref`;
the workflow lint refuses a `hostkeeper_commit` that is not the `uses:` commit.

## When it fails

A failure of either job leaves the release published; only the deploy did not happen. Once the
cause is fixed, use **Re-run failed jobs** on the release run: it runs `channel` and
`set-desired` again, and the call gets a new token.

| job and step | what it means | what to do |
|---|---|---|
| `channel`: Wait until the release channel serves the version's index | The channel did not serve `hostkeeper/<X.Y.Z>/index.json` within 30 minutes, or `HOSTKEEPER_RELEASE_CHANNEL` is empty or not `https://`. Nothing is set. | Look at the channel sync's log (`systemctl status hostkeeper-channel-sync.service`, or the sidecar's): a release it refused is listed there. Re-run once it is staged. |
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
