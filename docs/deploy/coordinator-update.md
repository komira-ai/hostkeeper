# Updating the coordinator's container by itself

The machine that runs the coordinator's container installs every published release of hostkeeper
on its own. A systemd timer on that machine runs `hostkeeper-coordinator-update` every five minutes;
the program, a release asset of its own, downloads the release's image, checks it as [verify-a-release.md](verify-a-release.md) says,
loads it, and replaces the container, keeping the old one until the new one is ready. It needs no
GitHub token: `komira-ai/hostkeeper` is public, so the program reads the release list and the
release assets at GitHub's public URLs, and checks each attestation offline, from the release's
bundle, with a pinned `gh` and the Sigstore trusted root, as the daemon's verifier does. A release
that sets the hosts waits until the coordinator answers `GET /v1/version` with that release's
version, so the hosts are set only by a coordinator that knows the release's requests.

| File | What it is |
|---|---|
| `coordinator/hostkeeper-coordinator-update.service` | One run of the program, as a oneshot, by a dedicated user. |
| `coordinator/hostkeeper-coordinator-update.timer` | Two minutes after boot, then five minutes after each run ends. |

## What a run does

Each step is one line of the report (the journal); a refusal fails the unit with its reason and
changes nothing.

1. The target version: `--target-version`, else the newest published (not draft) release `vX.Y.Z` of
   `komira-ai/hostkeeper`, from `GET https://api.github.com/repos/komira-ai/hostkeeper/releases?per_page=100`
   with no token, once a run. The list and its `ETag` are kept in the download directory
   (`releases.json`, `releases.etag`), and the next run asks with `If-None-Match`: GitHub's
   `304 Not Modified` is answered from the kept list, with nothing downloaded again.
2. The container (`--container`, default `hostkeeper-coordinator`) must exist and run. A container
   named `<name>-next` must not exist.
3. The running version: the coordinator's `GET <url>/v1/version`, or, for a coordinator too old to
   have that route (it answers 404), the version in its image's `hostkeeper-coordinator:<version>`
   tag. A running version at least the target ends the run: "up to date", nothing changed. A tag
   that names no version is an unknown version, and the update goes on. A `/v1/version` that
   gives no answer the program can read (a wrong `--ca-file`, the listener down) refuses the run with
   nothing changed: the same request judges the new container, so going on would stop a
   coordinator that may be up to date and then roll it back.
4. A file `failed-v<target>` in the download directory refuses the version (see "A failed
   version").
5. The image tarball, its bundle and `SHA256SUMS` are downloaded to `<download dir>/v<target>/`
   from `https://github.com/komira-ai/hostkeeper/releases/download/v<target>/<name>` (following
   its redirects, `https://` only; a file already there is kept and checked again). The tarball
   must be the sum `SHA256SUMS` lists, and its attestation must verify offline from its bundle as
   made by `komira-ai/hostkeeper`'s `artifacts.yml` on `refs/heads/main` (GitHub-hosted) for the
   release's commit: the pinned `gh` (`--gh`) runs `gh attestation verify <tarball> --bundle
   <bundle> --custom-trusted-root <--trusted-root> --repo komira-ai/hostkeeper --signer-workflow
   komira-ai/hostkeeper/.github/workflows/artifacts.yml --source-ref refs/heads/main
   --source-digest <commit> --deny-self-hosted-runners` (with the issuer and predicate the
   daemon names), with no token, no environment of the program's and a proxy that refuses every
   connection, in `<download dir>/.gh`; and what it verified is checked again. `gh` and the
   trusted root must be owned by root (or by the unit's user) and writable by no one else, as
   must their directories. Nothing is loaded otherwise.
6. The tarball's manifest digest and config digest are read from it.
7. `docker load` of the tarball, then the id of `hostkeeper-coordinator:<target>` must be one of
   the two digests (the containerd image store reports the manifest's, the classic store the
   config's): what docker loaded is the verified image.
8. The new container's configuration is the old container's: its command line, mounts, binds,
   volumes, restart policy, published ports and network. What the old image itself supplied
   (entrypoint, default command, user, working directory, stop signal, health check, the image's
   environment and labels) is dropped so the new image's own applies. A volume the old image made
   by `VOLUME` is mounted again by name. A user network keeps its aliases and fixed addresses (the
   Engine API of version 1.43 attaches one network at creation: a container on several is
   refused by docker at the create, with nothing changed).
9. `<name>-next` is created, the old container is stopped (`--stop-timeout-secs`), the new one
   started and checked up to `--ready-checks` times, `--check-every-secs` apart: it must run, not
   be restarting, have restarted 0 times, and the coordinator must answer
   `GET /v1/version?at_least=<target>` with 200. Then the old container is deleted (its volumes
   stay) and the new one is renamed to the old name. Otherwise the new container's last 50 log
   lines go in the failure, it is stopped and deleted, the old container is started again and
   checked, and the version is marked failed. A docker call that fails while the new container is checked counts as
   a check that is not ready, so it ends in the same roll back.

Only a successful update removes old downloads, keeping the newest two `v*` directories and the
one it just installed.

GitHub limits requests without a token by address (60 an hour to its API). A run asks for the
list once, and the timer runs every five minutes. When GitHub answers that the limit is used up
(`429`, or `403` with `x-ratelimit-remaining: 0` or a `Retry-After`), to the list or to a
download, the run stops there, changes nothing, reports `GitHub's limit of requests without a
token is used up (...): nothing changed; the next run tries again` and exits 0: the next run (the
next timer tick) tries again, and keeps what was already downloaded.

## Install

On the machine that runs the coordinator's container:

1. Install the program, the pinned `gh` and the Sigstore trusted root from a release, never from
   a build on this machine, with no token: everything is public. `curl` must be on the `PATH`,
   and the program trusts GitHub's certificates by the CA bundle file
   `/etc/ssl/certs/ca-certificates.crt` (Debian and Ubuntu; `--ca-bundle` names another), never
   by an environment variable.
   `<V>` is the release, `X.Y.Z`; `<P>` is `linux-x86_64` or `linux-arm64`, the machine's
   platform, and `<A>` is `amd64` or `arm64` for it; `<commit>` is the full commit you pin the
   release to (`$PIN` in [verify-a-release.md](verify-a-release.md)); `<GV>` and `<GH-SHA256>`
   are the `gh` version and the SHA-256 of its archive for `<A>` that `.github/workflows/artifacts.yml`
   pins at `<commit>` (`GH_VERSION`, and `gh-sha256` of the build matrix's row for `<P>`). In an
   empty directory:

   ```sh
   get() { curl --proto =https --proto-redir =https --tlsv1.2 --fail --silent --show-error --location "$@"; }
   for f in hostkeeper-coordinator-update-<V>-<P>.tar.gz hostkeeper-coordinator-update-<V>-<P>.tar.gz.sigstore.json SHA256SUMS; do
     get --remote-name "https://github.com/komira-ai/hostkeeper/releases/download/v<V>/$f"
   done
   grep ' hostkeeper-coordinator-update-<V>-<P>.tar.gz$' SHA256SUMS | sha256sum --check
   get --output gh.tar.gz "https://github.com/cli/cli/releases/download/v<GV>/gh_<GV>_linux_<A>.tar.gz"
   echo '<GH-SHA256>  gh.tar.gz' | sha256sum --check
   tar -xzf gh.tar.gz --strip-components=2 gh_<GV>_linux_<A>/bin/gh
   get --output trusted_root.json "https://raw.githubusercontent.com/komira-ai/hostkeeper/<commit>/packaging/sigstore/trusted_root.json"
   mkdir gh-home
   env -i PATH=/usr/bin:/bin HOME="$PWD/gh-home" GH_CONFIG_DIR="$PWD/gh-home/config" GH_PROMPT_DISABLED=1 \
     HTTPS_PROXY=http://127.0.0.1:9 HTTP_PROXY=http://127.0.0.1:9 ALL_PROXY=http://127.0.0.1:9 \
     ./gh attestation verify hostkeeper-coordinator-update-<V>-<P>.tar.gz \
     --bundle hostkeeper-coordinator-update-<V>-<P>.tar.gz.sigstore.json \
     --custom-trusted-root trusted_root.json --repo komira-ai/hostkeeper \
     --signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml \
     --source-ref refs/heads/main --source-digest <commit> --deny-self-hosted-runners
   tar -xzf hostkeeper-coordinator-update-<V>-<P>.tar.gz hostkeeper-coordinator-update
   install -d -o root -g root -m 0755 /usr/local/libexec/hostkeeper /usr/local/lib/hostkeeper
   install -o root -g root -m 0755 hostkeeper-coordinator-update gh /usr/local/libexec/hostkeeper/
   install -o root -g root -m 0644 trusted_root.json /usr/local/lib/hostkeeper/
   /usr/local/libexec/hostkeeper/hostkeeper-coordinator-update --version
   ```

   The attestation is checked as the program checks the image: offline, from the bundle, made by
   `komira-ai/hostkeeper`'s `artifacts.yml` on `refs/heads/main`, on a GitHub-hosted runner, for
   the release's commit. The trusted root is the one the release's commit carries (the daemon's
   packages carry the same file). The last line must print `hostkeeper-coordinator-update
   <V>+<first 12 hex digits of the commit>`. Stop at the first command that fails.
2. Create the user and join the group `docker`:

   ```sh
   useradd --system --home-dir /var/lib/hostkeeper-coordinator-update --shell /usr/sbin/nologin \
     --groups docker hostkeeper-coordinator-update
   ```

3. Put the CA the coordinator's private listener's certificate chains to at
   `/etc/hostkeeper/coordinator-ca.pem` (mode 0644, owned by root).
4. Copy the two files of `coordinator/` to `/etc/systemd/system/`, set `--coordinator-url` in the
   service to the coordinator's private listener, and `--container` if the container has another
   name.

Turn on:

```sh
systemctl daemon-reload
systemctl enable --now hostkeeper-coordinator-update.timer
```

Watch: `systemctl list-timers hostkeeper-coordinator-update.timer`, then
`journalctl -u hostkeeper-coordinator-update.service`. To update at once:
`systemctl start hostkeeper-coordinator-update.service`.

Turn off: `systemctl disable --now hostkeeper-coordinator-update.timer`. Nothing else changes; the
container keeps running the version it has.

A machine set up before the program was a release asset (it ran `xtask coordinator-update`, built
on that machine): do step 1, copy `coordinator/hostkeeper-coordinator-update.service` again (its
`ExecStart` now starts `/usr/local/libexec/hostkeeper/hostkeeper-coordinator-update`, whose flag
for a target release is `--target-version`), set `--coordinator-url` as in step 4, run
`systemctl daemon-reload`, and remove `/usr/local/libexec/hostkeeper/xtask` unless the database
backup timer of [coordinator/README.md](coordinator/README.md) still runs it.

A machine set up when the program needed a token (a unit with
`EnvironmentFile=/etc/hostkeeper/coordinator-update.env`, `gh` on the `PATH` logged in with it):
do step 1 with this release, copy `coordinator/hostkeeper-coordinator-update.service` again (it
reads no environment file), set `--coordinator-url` as in step 4, run `systemctl daemon-reload`,
and remove `/etc/hostkeeper/coordinator-update.env`; then revoke the token. An older program
under the new unit, with no token, cannot read the release list: install the program of this
release or a newer one first.

The program does not update itself: it keeps the version installed in step 1 while it rolls the
container forward. To take a newer one, run step 1 again with that release; the timer's next run
uses it. The same goes for the trusted root: when Sigstore rotates a key, the releases signed
after it are refused (see the refusal table) until step 1 is run again with a release whose
commit carries the new root.

A release whose job `coordinator` fails with `no coordinator updater reported this release (is
the timer installed?)` waited 30 minutes and the coordinator never answered that it runs the
release: the timer is not installed or not enabled, or its runs fail (`systemctl status` and the
journal of the service say why), or the version is marked failed (see "A failed version").

## The docker group

Membership of `docker` is root on the machine: whoever can talk to the Docker socket can start a
container with the host's root mounted. So the unit's user is a dedicated one that owns nothing
else and logs in nowhere, the sandbox of the unit (`ProtectSystem=strict`, one writable directory,
`NoNewPrivileges`) limits what the program itself can touch, and the program only ever loads an image that
the release signer, which every daemon of the farm trusts, attested for the release's commit and
whose SHA-256 is the one in `SHA256SUMS`. The unit holds no credential: it reads only public
URLs, and checks attestations offline.

## What a refusal means

| Refusal | What to do |
|---|---|
| `no container <name> on this machine: deploy the coordinator first` | The program rolls a running coordinator forward; it never deploys one. Deploy it as [coordinator/README.md](coordinator/README.md) says, or fix `--container`. |
| `container <name> is not running` | It never starts a stopped container. Start it (`docker start <name>`) or find out why it stopped. |
| `container <name>-next exists` | An earlier run was cut short, between creating the new container and the end of the swap. Read its logs (`docker logs <name>-next`) and `docker ps -a` to see which container serves; remove `<name>-next` (`docker rm -f <name>-next`) only when `<name>` runs. If `<name>` is gone and `<name>-next` ran the new version, `docker rename <name>-next <name>`. |
| `the image of <name> is not in docker` | The old container's image was deleted, so what the image supplied cannot be told from what the container sets. Deploy by hand. |
| `<file> says v<V> did not become ready ...` | See "A failed version". |
| `<tar> (offline): ...` lines: no attestation, signed by another workflow, another ref or commit, `Error: verifying with issuer ...` | The image is not the release's attested one, or the trusted root is older than the key that signed it. Nothing was loaded. Look at the release; run `gh attestation verify` as [verify-a-release.md](verify-a-release.md) says. If it verifies there, the trusted root is stale: run step 1 of "Install" again with the release. |
| `<tar> (offline): gh attestation verify (offline): --gh is not trusted: ...` (or `--trusted-root`) | `gh` or the trusted root is not owned by root (or the unit's user), or someone else can write it or its directory. Nothing was run. Install them again as step 1 of "Install" says. |
| `GitHub's limit of requests without a token is used up (...): nothing changed; the next run tries again` (exit 0) | Not a refusal: another program behind the same address spent GitHub's limit. The next run tries again; nothing to do unless it lasts. |
| `GET https://api.github.com/...: HTTP <status>` or `download <name> of v<V>: GET ...: HTTP <status>` | GitHub did not serve the list or the asset. Nothing changed. The next run tries again; look at GitHub's status if it lasts. |
| `<tar>: its SHA-256 is ..., SHA256SUMS says ...` | The download is not the release's file (a cut-short or altered copy). Remove `<download dir>/v<V>/` and let the next run download it again. |
| `the image docker loaded is not the verified one` | After `docker load`, the id of `hostkeeper-coordinator:<V>` is neither digest of the tarball. Nothing was created. Remove the image (`docker rmi hostkeeper-coordinator:<V>`) and look at the docker version and image store. |
| `<name> could not be stopped` | `<name>-next` was deleted and `<name>` left as it was. Look at `docker ps`. |
| `... <name> IS DOWN` | The new version did not become ready and the old container could not be started again. Start it (`docker start <old id>`, given in the message), or deploy by hand. |
| `<name>-next runs v<V> and is ready, but the old container ... could not be deleted` | The new container serves. Delete the old one and rename the new one as the message says. |

## A failed version

When the new container does not become ready within the checks, the program stops and deletes it,
starts the old container again, writes `failed-v<V>` in the download directory with the reason,
and fails. The failure has the new container's last 50 log lines. Later runs refuse `v<V>` and
change nothing; a newer release is tried normally. To try `v<V>` again (after the cause is fixed,
for example a config change the release needs), remove the marker:

```sh
rm /var/lib/hostkeeper-coordinator-update/failed-v<V>
systemctl start hostkeeper-coordinator-update.service
```

## A compose deployment

A coordinator deployed with the compose file of [coordinator/README.md](coordinator/README.md) is
a container like any other, and the program replaces it. The new container keeps the compose labels
of the old one, so `docker compose` still sees it as the service. The compose file's `image:` line
still names the old version: update it (version and digest) before the next `docker compose up`,
or compose will recreate the container from the old image.

## On Docker Swarm

A coordinator that runs as a Docker Swarm service is not updated by the timer above: nothing on
its machine runs `hostkeeper-coordinator-update`. Its release run updates it instead. The job
`deploy` of [release.yml](../../.github/workflows/release.yml) calls the reusable deploy workflow
of `komira-ai/docker-swarm-updater` with an OIDC token, naming the release's version, the digest
of its `linux/amd64` image manifest and the download URLs of its image tarball and bundle, and the
job `coordinator` then waits until the coordinator answers that version, as it does for the timer
(`GET /v1/version`). The job `deploy` replaces the timer on such a machine; the two are not run
together on one coordinator.

The deploy is off until the repository variable `CP_DEPLOY_ENDPOINT` is not empty. Empty or unset,
the jobs `deploy-digest` and `deploy` are skipped, and the coordinator is updated as before, by
whatever updates it (the timer, or an operator). Before setting the variable an admin creates the
environment `controlplane` (deployment branches `main` only, no required reviewer, no wait timer)
and the variables `CP_DEPLOY_AUDIENCE`, `CP_DEPLOY_TAILSCALE_CLIENT_ID` and
`CP_DEPLOY_TAILSCALE_AUDIENCE`; [github-actions.md](github-actions.md#deploying-the-coordinator-to-docker-swarm)
has the whole list and what each job does.

The failure text of the job `coordinator` still says `is the timer installed?` after a 409 or a
404; on a Swarm coordinator the timer is not the cause, so read the log of the job `deploy`
instead.

## Why the machine pulls

The incident: a release rolled every host to new versions while the coordinator's container stayed
on an old build until an operator updated it by hand, and the release's `set-desired` was refused
because the old coordinator did not know the release's new request fields. The options weighed:

- **(a) The coordinator's machine managed by hostkeeper as a service.** Not taken.
  [packages-native.md](../design/packages-native.md) sections 10.4 and 10.9 rule that the
  coordinator is not a service hostkeeper deploys; a service's package may carry no maintainer
  scripts, and the coordinator's package has them; the `oci-image` kind of target is withdrawn;
  and the coordinator would order its own restart.
- **(b) A self-update that the release calls.** Taken in its pull form. CI cannot reach docker on
  that machine and should not: that would be a path from CI to a write on a machine (rule S3 of
  AGENTS.md). So the machine's own timer installs every attested release, and the release only
  reads `GET /v1/version` of the coordinator and waits there before it sets the hosts.
