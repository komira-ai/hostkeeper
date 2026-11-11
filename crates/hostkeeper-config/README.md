# hostkeeper-config

The daemon config hostkeeper reads to decide what a host trusts, with a strict loader, the
manifest of an installed version, and the grammars of names and paths. Why they are shaped this
way: `docs/design/hostkeeper-design.md`, sections 4.1 (the daemon config), 4.7 (the package) and
5.5 (the platforms). This page states what the code holds.

| module | what it holds |
|---|---|
| `daemon` | `DaemonConfig::load(path, trusted_uid)` reads `/etc/hostkeeper/hostkeeper.toml`: a regular file, not a link, owned by `trusted_uid` (root on a host) and not writable by the group or others, checked on the open file. `DaemonConfig::parse(text)` reads the text alone. |
| `manifest` | The manifest of an installed version, its refusals and the rules a release manifest shares with it. |
| `platform` | `Platform`: `linux-x86_64`, `linux-arm64` or `macos-arm64`, a closed set read from exactly those spellings; `Platform::current()` is the platform of the compile target (any other target does not compile). |
| `trust` | `read_trusted` and `check_dir`: the owner and mode check of a file or directory the host trusts. |
| `names` | The grammars: service names and versions of design 11.8 (1 to 255 bytes of the POSIX portable filename characters `A-Za-z0-9._-`, with `+` added for a version, not beginning with `-` or `.`), absolute and relative paths, and the unit names of a package. |

The crate depends on no crate of this workspace; `crates/xtask/tests/layering` keeps it so.

## The daemon config

Format exactly as design 4.1: `coordinator` and `release_channel` (`https://` URLs),
`ca_sha256` (64 hex digits), `[trusted_signers]` (`organisation`, `workflow`, `refs`,
`hosted_runner`; `TrustedSigners::allows_ref` matches a ref against its globs, `*` within one
`/`-separated part) and `[bounds]`. Every key is required except `release_channel` and those of `[bounds]`, which default
to the design's example (`Bounds::default`). A missing `[trusted_signers]` is refused: there is
no unsigned mode. There is no list of services and no per-service key. The daemon reads neither
`release_channel` nor `[trusted_signers]` any more; the loader still requires `[trusted_signers]` and
accepts `release_channel` present or absent, so a config written for an earlier release keeps loading.
The generated configs keep writing `release_channel` for one release more, because a daemon of the
previous release refuses a config without it.

Refused, each as a `ConfigError`: a key no table knows (`UnknownKey`, naming the key and its
table; `""` is the top level), a missing or ill-typed value (`Invalid`, naming the dotted key), a
bound of 0, `max_unpacked_bytes` below `max_package_bytes`, a `fetch_timeout_secs` whose product
with 256 KiB/s does not fit in 64 bits, and a `max_package_bytes` that cannot be fetched at
256 KiB/s within `fetch_timeout_secs` (`FetchBoundTooSmall`).

## The manifest

The manifest of an installed version is the value the daemon builds from the version's sealed
`target.json` (`hostkeeper_daemon::target`): `Manifest`, `Hook`, `Timeouts`, `FileEntry`, and
`Expected`, the order it is read for. No `manifest.toml` is read.

`ManifestRefusal` says why a version is refused (`Unreadable`, `Invalid` naming the dotted key,
`DaemonTooOld` when `min_hostkeeper` is above the running daemon). `class()` and `reason()` say how
a step record reports a refusal: `DaemonTooOld` is `Environment` with `daemon_too_old` (the host
must update hostkeeper first), every other refusal `Release` with `manifest_mismatch` (design 5.5).
`HostkeeperVersion` is `major.minor.patch`, three whole numbers without leading zeros or a suffix.

The rules a release manifest (crate `hostkeeper-release`) shares with it: `check_health_url`
(`http://` to a loopback literal, `127.0.0.1` or `[::1]`, design 5.8; a `{port}` in the port's
place needs every run unit to be an instance whose name is a port), `is_boot_step` (a lowercase
name, `a-z0-9_`, 1 to 64, that is none of the five update steps or `uninstall`) and
`REFUSED_PREFIXES` (the directories a placed file's path may not be at or below, compared on
whole components).
