# hostkeeper-config

The two files hostkeeper reads to decide what a host runs, each with a strict loader, and the
release index that says which package of a version is the host's. Why they are shaped this way:
`docs/design/hostkeeper-design.md`, sections 4.1 (the daemon config), 4.7 (the package) and 5.5
(the release index and the platforms). This page states what the code holds. Nothing calls the
loaders yet; the daemon and the package fetch do.

| module | what it holds |
|---|---|
| `daemon` | `DaemonConfig::load(path, trusted_uid)` reads `/etc/hostkeeper/hostkeeper.toml`: a regular file, not a link, owned by `trusted_uid` (root on a host) and not writable by the group or others, checked on the open file. `DaemonConfig::parse(text)` reads the text alone. |
| `channel_trust` | `ChannelTrust::parse(text)` reads the trust list of `release channel-sync`: one `[[service]]` per service the release channel stages, its `name`, `repository` (`Repository`, `<owner>/<name>`) and signer, whose `workflow`, `refs` and `hosted_runner` are read by the daemon config's code into its `TrustedSigners` (`organisation` is the repository's owner). |
| `manifest` | `Manifest::load(version_dir, expected, running)` reads `<version_dir>/manifest.toml` (a regular file, not a link, at most 64 KiB) and applies the load rules of design 4.7 for the order `expected`, against the running daemon's version. |
| `platform` | `Platform`: `linux-x86_64`, `linux-arm64` or `macos-arm64`, a closed set read from exactly those spellings; `Platform::current()` is the platform of the compile target (any other target does not compile). |
| `index` | `ReleaseIndex`: `index.json` of a version in the release channel, read by a strict parser (`ReleaseIndex::parse(text, service, version)`), written in canonical form (`to_json`), and `entry_for(platform)`; the channel paths `index_path`, `package_path` and `attestation_path`. |
| `trust` | `read_trusted` and `check_dir`: the owner and mode check of a file or directory the host trusts. |
| `names` | The grammars: service names and versions of design 11.8 (1 to 255 bytes of the POSIX portable filename characters `A-Za-z0-9._-`, with `+` added for a version, not beginning with `-` or `.`), absolute and relative paths, and the unit names of a package. |

The crate depends on no crate of this workspace; `crates/xtask/tests/layering` keeps it so.

## The daemon config

Format exactly as design 4.1: `coordinator` and `release_channel` (`https://` URLs),
`ca_sha256` (64 hex digits), `[trusted_signers]` (`organisation`, `workflow`, `refs`,
`hosted_runner`; `TrustedSigners::allows_ref` matches a ref against its globs, `*` within one
`/`-separated part) and `[bounds]`. Every key is required except those of `[bounds]`, which default
to the design's example (`Bounds::default`). A missing `[trusted_signers]` is refused: there is
no unsigned mode. There is no list of services and no per-service key.

Refused, each as a `ConfigError`: a key no table knows (`UnknownKey`, naming the key and its
table; `""` is the top level), a missing or ill-typed value (`Invalid`, naming the dotted key), a
bound of 0, `max_unpacked_bytes` below `max_package_bytes`, a `fetch_timeout_secs` whose product
with 256 KiB/s does not fit in 64 bits, and a `max_package_bytes` that cannot be fetched at
256 KiB/s within `fetch_timeout_secs` (`FetchBoundTooSmall`).

## The manifest

Format as design 4.7. The checks, in this order, so that a malformed manifest is never reported
as one the daemon is too old for:

1. the file: a regular file, not a link, at most 64 KiB, UTF-8;
2. the syntax, then `schema = 1` (any other schema is refused naming it);
3. every key against the tables that may hold it (`[params]` takes any key);
4. `name` and `version` equal the order's (`NameMismatch`, `VersionMismatch`), and `platform`
   is one of `linux-x86_64`, `linux-arm64` and `macos-arm64` and equals the order's
   (`PlatformMismatch`; the daemon's order names its own platform, `Platform::current()`);
5. `[hook]`: `command` is a program relative to the version directory; `boot` is absent or a
   lowercase name (`a-z0-9_`, 1 to 64) that is none of the five update steps or `uninstall`;
   `prepare = 0` is unbounded and every other step bound is 1 or more; `uninstall` is optional
   (absent means the package has no uninstall step); `verify_deadline_secs` is at least `verify`
   plus `rollback`; `promotion_ceiling_secs` is optional and not 0; `cleanup_retries` defaults
   to 3;
6. every file in `units/` is the service's (`<svc>`, or `<svc>-<more>`, as a `.service`,
   `.socket` or `.timer`, plain or a template); each `[units] run` entry is one of them or an
   instance of a template among them;
7. `[health] url` is `http://` to a loopback literal (`127.0.0.1` or `[::1]`, design 5.8); a
   `{port}` in the port's place needs every run unit to be an instance whose name is a port (with no run unit there is
   nothing to probe, and the URL loads);
8. each `[[files]] source` is `files/...`, present as a regular file or a directory with no link
   on the way; each `path` is absolute, in the path grammar, not at or below `/etc/hostkeeper`,
   `/etc/updater`, `/var/lib/hostkeeper`, `/var/lib/updater`, `/etc/systemd/system`, `/usr`,
   `/boot` or `/var/lib` (whole components: `/usr2/x` is fine), and no two entries share one;
9. `[params]` holds scalars only and no `updater` or `updater.*` key;
10. `min_hostkeeper` is `major.minor.patch`; above the running daemon it is `DaemonTooOld`.

`ManifestRefusal::class()` and `reason()` say how a step record reports a refusal:
`DaemonTooOld` is `Environment` with `daemon_too_old` (the host must update hostkeeper first),
every other refusal `Release` with `manifest_mismatch` (design 5.5).

Not judged here: the ownership of the version directory and the archive shape (the package
fetch), and placing units and files, a path another package owns and the contents of unit files
(the linking of placed paths).

## The release index

Format as design 5.5: `{"packages":[{"bytes":N,"file":"S-V-<platform>.tar.zst","platform":"<platform>","sha256":"<64 hex>"}],"schema":1,"service":"S","version":"V"}`.
The writer sorts the keys and the packages (by platform) and writes no white space; the parser
takes any layout and key order. Refused, each an `IndexError` with the reason `index_invalid`: text
that is not one JSON document; an unknown, missing or repeated key or a value of the wrong type
(`Syntax`); a schema other than 1, judged before the other keys (`Schema`); another service or
version than the one asked for; no package; a platform outside the three; a platform listed twice
or out of order; a `file` other than `S-V-<platform>.tar.zst`; a `sha256` that is not 64 hex
digits. `entry_for` a platform the index does not list is `NoPackageForPlatform`, reason
`no_package_for_platform`.

The channel paths, relative to `release_channel`: `index_path` is `S/V/index.json`,
`package_path` is `S/V/S-V-<platform>.tar.zst`, `attestation_path` is that path with
`.sigstore.json` added.
