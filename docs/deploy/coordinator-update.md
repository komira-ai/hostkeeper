# Updating the coordinator's container by itself

The machine that runs the coordinator's container installs every published release of hostkeeper
on its own. A systemd timer on that machine runs `hostkeeper-coordinator-update` every five minutes;
the program, a release asset of its own, downloads the release's image, checks it as [verify-a-release.md](verify-a-release.md) says,
loads it, and replaces the container, keeping the old one until the new one is ready. A release
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
   `komira-ai/hostkeeper`.
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
   (a file already there is kept and checked again). The tarball must be the sum `SHA256SUMS`
   lists, and its attestation must verify online and from its bundle as made by
   `komira-ai/hostkeeper`'s `artifacts.yml` on `refs/heads/main` (GitHub-hosted) for the release's
   commit. Nothing is loaded otherwise.
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

## Install

On the machine that runs the coordinator's container:

1. Install the program from a release, never from a build on this machine. `gh` (2.102 or newer)
   and `curl` must be on the `PATH`. `<V>` is the release, `X.Y.Z`; `<P>` is `linux-x86_64` or
   `linux-arm64`, the machine's platform; `<commit>` is the release's commit, as
   [verify-a-release.md](verify-a-release.md) step 1 finds it. In an empty directory:

   ```sh
   gh release download v<V> --repo komira-ai/hostkeeper \
     --pattern hostkeeper-coordinator-update-<V>-<P>.tar.gz --pattern SHA256SUMS
   grep ' hostkeeper-coordinator-update-<V>-<P>.tar.gz$' SHA256SUMS | sha256sum --check
   gh attestation verify hostkeeper-coordinator-update-<V>-<P>.tar.gz \
     --repo komira-ai/hostkeeper \
     --signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml \
     --source-ref refs/heads/main --source-digest <commit> --deny-self-hosted-runners
   tar -xzf hostkeeper-coordinator-update-<V>-<P>.tar.gz hostkeeper-coordinator-update
   install -o root -g root -m 0755 hostkeeper-coordinator-update /usr/local/libexec/hostkeeper/
   /usr/local/libexec/hostkeeper/hostkeeper-coordinator-update --version
   ```

   The attestation is checked as the program checks the image: made by `komira-ai/hostkeeper`'s
   `artifacts.yml` on `refs/heads/main`, on a GitHub-hosted runner, for the release's commit. The
   last line must print `hostkeeper-coordinator-update <V>+<first 12 hex digits of the commit>`.
   Stop at the first command that fails.
2. Create the user and join the group `docker`:

   ```sh
   useradd --system --home-dir /var/lib/hostkeeper-coordinator-update --shell /usr/sbin/nologin \
     --groups docker hostkeeper-coordinator-update
   ```

3. Put the CA the coordinator's private listener's certificate chains to at
   `/etc/hostkeeper/coordinator-ca.pem` (mode 0644, owned by root).
4. Write `/etc/hostkeeper/coordinator-update.env` (mode 0600, owned by root) with one line,
   `GH_TOKEN=<token>`: a GitHub token that reads public repositories and nothing else. It is `gh`'s
   credential; the program reads no environment variable.
5. Copy the two files of `coordinator/` to `/etc/systemd/system/`, set `--coordinator-url` in the
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

The program does not update itself: it keeps the version installed in step 1 while it rolls the
container forward. To take a newer one, run step 1 again with that release; the timer's next run
uses it.

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
whose SHA-256 is the one in `SHA256SUMS`. A compromised GitHub token of the kind in the env file
can read public data only.

## What a refusal means

| Refusal | What to do |
|---|---|
| `no container <name> on this machine: deploy the coordinator first` | The program rolls a running coordinator forward; it never deploys one. Deploy it as [coordinator/README.md](coordinator/README.md) says, or fix `--container`. |
| `container <name> is not running` | It never starts a stopped container. Start it (`docker start <name>`) or find out why it stopped. |
| `container <name>-next exists` | An earlier run was cut short, between creating the new container and the end of the swap. Read its logs (`docker logs <name>-next`) and `docker ps -a` to see which container serves; remove `<name>-next` (`docker rm -f <name>-next`) only when `<name>` runs. If `<name>` is gone and `<name>-next` ran the new version, `docker rename <name>-next <name>`. |
| `the image of <name> is not in docker` | The old container's image was deleted, so what the image supplied cannot be told from what the container sets. Deploy by hand. |
| `<file> says v<V> did not become ready ...` | See "A failed version". |
| `<tar> (online)` or `(offline)` lines: no attestation, signed by another workflow, another ref or commit | The image is not the release's attested one. Nothing was loaded. Look at the release; run `gh attestation verify` as [verify-a-release.md](verify-a-release.md) says. |
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
