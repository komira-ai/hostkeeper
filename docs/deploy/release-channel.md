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
transport, which is the web server's: the daemon requires `https`.

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
