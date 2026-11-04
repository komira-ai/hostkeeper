# hostkeeper-config

The two files hostkeeper reads to decide what a host runs, each with a strict loader. Why they
are shaped this way: `docs/design/hostkeeper-design.md`, sections 4.1 (the daemon config) and 4.7
(the package). This page states what the code holds. Nothing calls the loaders yet; the daemon
and the package fetch do.

| module | what it holds |
|---|---|
| `daemon` | `DaemonConfig::load(path, trusted_uid)` reads `/etc/hostkeeper/hostkeeper.toml`: a regular file, not a link, owned by `trusted_uid` (root on a host) and not writable by the group or others, checked on the open file. `DaemonConfig::parse(text)` reads the text alone. |
| `manifest` | `Manifest::load(version_dir, expected, running)` reads `<version_dir>/manifest.toml` (a regular file, not a link, at most 64 KiB) and applies the load rules of design 4.7 for the order `expected`, against the running daemon's version. |
| `trust` | `read_trusted` and `check_dir`: the owner and mode check of a file or directory the host trusts. |
| `names` | The grammars: service names (`A-Za-z0-9_-`, 1 to 64 bytes) and versions (1 to 128 bytes of `A-Za-z0-9._+-`, no leading dot) of design 11.8, absolute and relative paths, and the unit names of a package. |

The crate depends on no crate of this workspace; `crates/xtask/tests/layering` keeps it so.

## The daemon config

Format exactly as design 4.1: `coordinator` and `release_channel` (`https://` URLs),
`ca_sha256` (64 hex digits), `[trusted_signers]` (`organisation`, `workflow`, `refs`,
`hosted_runner`) and `[bounds]`. Every key is required except those of `[bounds]`, which default
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
4. `name` and `version` equal the order's (`NameMismatch`, `VersionMismatch`);
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
