# Deploying the coordinator to the control plane

The control plane runs the coordinator as the Docker Swarm service `hostkeeper-coordinator`, kept up
to date by [docker-swarm-updater](https://github.com/komira-ai/docker-swarm-updater). Once the
repository variables below are set, every run of [release.yml](../../.github/workflows/release.yml)
that publishes a release deploys the newest release of the run to that service. Until then the two
jobs are skipped and a release run ends as it did without them.

## The two jobs

1. **`cp-deploy-digest`** reads the repository and nothing else. After every publish and image push
   of the run (it waits for `newest`), it takes the newest release's files from the run's artifact
   `release-<tag>`, the files `verify` checked, and checks them against `SHA256SUMS` once more. Then
   `cargo xtask release image-digest` outputs:
   - `digest`: the digest of the one manifest the linux-x86_64 image tarball's `index.json` names
     (`hostkeeper-coordinator-<X.Y.Z>-linux-x86_64.image.tar`), checked to be a linux/amd64 image
     held in the tarball at that digest;
   - `image-url` and `bundle-url`: the release's download addresses of that tarball and of its
     bundle (`<tarball>.sigstore.json`), which must be among the files.
2. **`cp-deploy`** calls docker-swarm-updater's reusable workflow `deploy.yml`, pinned to the full
   commit the updater's caller list names, with `id-token: write` and no other right, and no secret.
   It passes the service `hostkeeper-coordinator`, the release's version `X.Y.Z`, that digest and
   those addresses, and the endpoint, audience and tailnet settings from the repository variables.
   It runs in this repository's environment `controlplane` (main only, no required reviewer). It
   passes no signer, so the updater checks the bundle against the default one: this repository's
   `.github/workflows/artifacts.yml` on `refs/heads/main`, the workflow that attests every release
   file.

The called workflow joins the tailnet, sends the deploy with an OIDC token minted for the
audience, and asks for its status every 10 seconds until the deploy is final or 60 minutes have
passed. The job passes when the updater reports the deploy `converged`, and fails otherwise. The
updater checks the token's claims, fetches the tarball and the bundle, verifies the bundle
offline against the signer, checks that the tarball's manifest is the digest it was given, and
then updates the service by digest; Swarm rolls back an update whose task does not become ready.
The release stays published whatever the deploy says. hostkeeper's own deploy waits for this one:
the job `coordinator` of `release.yml` ([github-actions.md](github-actions.md), "Wait for the
coordinator") needs `cp-deploy`, and while both variables below are set it runs only once
`cp-deploy` passed, so it never asks the coordinator this deploy is replacing; when `cp-deploy`
failed or was cancelled, the wait and `set-desired` are skipped and the hosts' desired version is
not set.

The workflow lint (`crates/xtask/tests/workflows/cp_deploy.rs`) holds both jobs to this shape:
their `if:`, needs, rights, steps, outputs, the pinned workflow and every input.

## The repository variables

| Variable | What it holds |
|---|---|
| `CP_DEPLOY_ENDPOINT` | The deploy front's address, `http[s]://host[:port]`. Empty: both jobs are skipped. |
| `CP_DEPLOY_AUDIENCE` | The audience every OIDC token is minted for, the deploy front's. |
| `CP_DEPLOY_TAILSCALE_CLIENT_ID` | The Tailscale workload-identity client the runner joins the tailnet with. |
| `CP_DEPLOY_TAILSCALE_AUDIENCE` | That client's audience. |
| `CP_DEPLOY_TAILSCALE_TAGS` | The tags the runner's tailnet node takes, comma-separated, ones that client can issue. Empty: both jobs are skipped, since the join would fail. |

To turn the deploy off, empty `CP_DEPLOY_ENDPOINT`.

## Re-runs, older releases and failures

- A re-run of `cp-deploy` for a release the service already runs is answered with that deploy's
  record, and passes.
- The updater refuses an image signed before the one the service runs (`older_than_running`): a
  late run, or a re-run, of an older release fails its `cp-deploy` and never moves the service
  back. Moving it back is an operator's step on the updater.
- A run that publishes several releases deploys only the newest; the older ones are passed over.
- A deploy that does not converge (rolled back, paused, refused, or still running after 60 minutes)
  fails the job with the updater's error. Read the deploy's record there, fix the cause, and re-run
  the failed jobs; a deploy still running goes on after the job gives up. A digest Swarm rolled
  back, or that failed its checks, is refused on a re-run (`previously_failed`) until an operator
  clears it on the updater.
