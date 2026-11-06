# Stage a release into the release channel

The daemon fetches a package from the daemon config's `release_channel`, under this layout
(design section 5.5):

```
{release_channel}/S/V/index.json
{release_channel}/S/V/S-V-{platform}.tar.zst
{release_channel}/S/V/S-V-{platform}.tar.zst.sigstore.json
```

A GitHub release does not have that layout: its assets sit side by side in one list, and its
index has an asset name of its own (`index.json`, `hello-index.json`,
`hello-fail-ready-index.json`). `cargo xtask release channel-stage` lays one version of one
service out as the daemon reads it, in a plain directory that any static file origin or
object-store sync can serve:

```sh
cargo xtask release channel-stage \
  --service tierstore --version 1.5.0 \
  --from <directory of the release's assets> \
  --out <root of the channel's tree>
```

| Flag | Meaning |
|---|---|
| `--service` | The service, in the daemon's grammar for a service name |
| `--version` | The version, in the daemon's grammar for a version (`+` allowed: `1.4.0+g1a2b3c`) |
| `--from` | The directory holding `S-V-{platform}.tar.zst` and its `.sigstore.json` for each platform |
| `--index` | The release index of the version; `index.json` in `--from` unless named. Staged as `S/V/index.json` whatever its asset name |
| `--require-platforms` | The platforms the index must list a package for, comma-separated; all three unless named |
| `--out` | The root of the tree; `S/V/` is made under it |

It copies files only. It uploads nothing, holds no credentials, and has no opinion on the
transport, which is the web server's: the daemon requires `https`. To stage each new release
as it is published, run `channel-sync` on the channel's host (see
[Sync new releases into the channel](#sync-new-releases-into-the-channel)); it stages through
this same code.

## What it checks

Nothing is written unless every check passes, and a refusal lists every problem it found:

- the index is at most 64 KiB, reads as the index of S at V by the daemon's own parser, and is in
  canonical form (keys sorted, packages sorted by platform, no white space, no final line break);
- it lists a package for every platform of `--require-platforms`. A release without its
  `macos-arm64` package would leave every Mac with `no_package_for_platform`, so leaving a
  platform out takes a flag, for example `--require-platforms linux-x86_64,linux-arm64` for a
  service built for Linux only;
- each package it lists is in `--from` with the size and SHA-256 the index gives it;
- each package has its bundle beside it, at most 1 MiB, whose statement lists the package's
  SHA-256. The bundle's signature is not checked here: check a hostkeeper release with
  `cargo xtask release check` first (see [verify-a-release.md](verify-a-release.md)), and the
  daemon checks every package against its `[trusted_signers]` before it unpacks anything;
- `--from` holds no package of S at V that the index leaves out;
- every file name is at most 255 bytes.

## Releases never change

A release in the channel is immutable. A file already staged with the same bytes is left as it
is, so staging the same release again writes nothing, and a stage that was cut short is finished
by running it again. A different file at a staged path (a rebuilt package under a version that is
already in the channel) is refused, and nothing is written.

Each file is copied to a temporary name (`.channel-stage-*`) in the version's directory, checked
again, and linked to its own name, which never replaces a file; the temporary name is then
removed. The packages and bundles are written before the index, so a host that finds the index
finds its packages. A temporary file left by a stage that was killed can be deleted.

## Serving the tree

Serve `--out` so that `{release_channel}/S/V/...` maps to `<out>/S/V/...`, path for path. A
version with `+` is staged with the `+` in its directory and file names, and the daemon asks for
it with a literal `+` in the URL path: an origin that reads `+` in a path as a space answers 404,
which the daemon reports as `fetch_failed` and retries.

## Sync new releases into the channel

A release published in CI does not reach the channel by itself: no workflow can push into the
host that serves it. `cargo xtask release channel-sync` pulls instead. It runs on that host,
reads each repository's published releases from GitHub, and stages every new one with
`channel-stage`:

```sh
cargo xtask release channel-sync \
  --repo komira-ai/hostkeeper --repo komira-ai/tierstore \
  --channel-dir <root of the channel's tree>
```

| Flag | Meaning |
|---|---|
| `--repo` | A repository whose releases are synced, `<owner>/<name>`. Repeat it, or list several after it; they are synced in that order. A repository named twice is refused |
| `--channel-dir` | The root of the channel's tree, the `--out` of `channel-stage`. It must exist |
| `--since` | A tag `v<X.Y.Z>`: releases older than it are skipped (it is synced) |
| `--every-secs` | Sync again this many seconds after each run ends, and never exit (the sidecar). Without it the verb runs once |

It needs `gh` on the `PATH`, with a GitHub token that can read public repositories and nothing
else (`GH_TOKEN`, gh's own credential). It reads only the 100 newest releases of each repository,
leaves drafts out, and skips a release whose tag is not `v<X.Y.Z>` or that carries no release
index (a release of binaries only).

For each release, oldest version first:

1. It downloads the release's indexes (`index.json` and each `<name>-index.json`) and compares
   each service version they name with the channel. An index must name the service its
   repository is named for (`komira-ai/tierstore` ships `tierstore`; a hostkeeper release also
   ships `hello`), and only one index of a release may name a version; otherwise the version is
   refused. A version whose `S/V/index.json` holds the
   same bytes, with every package it lists at its size and every bundle beside it, is already
   there: nothing more is downloaded. A version whose `S/V/index.json` holds other bytes is
   refused, since a release in the channel never changes; it is left as it is.
2. If any version is new, or was cut short (`S/V/` without its index, or missing a file), it
   downloads every other asset and checks the release. A hostkeeper release is checked by
   `cargo xtask release check` itself: every asset, `SHA256SUMS`, the indexes, and every
   attestation online and from its bundle. A release of another repository is checked by its
   `SHA256SUMS` (it lists every asset but itself and the bundles, each at its SHA-256) and the
   attestation of every asset with a bundle, online and from the bundle, as signed by that
   repository's own `.github/workflows/artifacts.yml` on `refs/heads/main` at the release's
   commit. A download that fails or a check that refuses stages nothing of the release.
3. It stages each new version with `channel-stage`, which requires a package for every platform.
   A new version is staged in a work directory inside the channel directory and renamed to
   `S/V/` whole, so a host never fetches a version that is half there. A version that was cut
   short is finished in place, which writes only what is missing.

The work directory is `.channel-sync-<pid>-<n>/` in the channel directory, removed after each
release; each run starts by removing any such directory a run that was killed left behind. A run with nothing new downloads
only the indexes and changes nothing. A run that refuses anything still syncs everything else,
prints a `refused` line for each refusal, and then fails (exit 1), so the timer or the
container's log shows it; the next run tries again.

Two runs must not overlap: the timer below starts a run only after the last one ended, and the
sidecar runs one loop. Packages already in the channel are compared by size only, not hashed on
every run: the daemon checks each package's SHA-256 against the index as it fetches it.

Two ways to run it, both in [`channel-sync/`](channel-sync/):

- on a host with systemd, [`hostkeeper-channel-sync.service`](channel-sync/hostkeeper-channel-sync.service)
  runs it once and [`hostkeeper-channel-sync.timer`](channel-sync/hostkeeper-channel-sync.timer)
  starts it ten minutes after each run ends;
- in a docker deployment, [`compose.yaml`](channel-sync/compose.yaml) runs it as a sidecar
  with `--every-secs 600`, writing into the volume the channel's web server serves.

### The order of a release

1. The service's release workflow publishes a signed GitHub release.
2. `channel-sync` stages it into the channel (within one timer period).
3. Only then is the desired version set ([ci-client.md](ci-client.md)). A version set before the
   channel holds it leaves every host failing its fetch (`fetch_failed`) and retrying until it
   arrives.
