# hostkeeper-package

Steps 1 and 2 of fetching a package ([docs/design/hostkeeper-design.md](../../docs/design/hostkeeper-design.md),
section 5.5): download the package an order names from the release channel, and prove that a
trusted signer built it. A version of a service is one package per OS and CPU family, listed in a
release index; the order names only the service and the version, and the daemon takes the package
of its own platform. The result is a `VerifiedArchive`, a staging file whose size and SHA-256 are
the index's and which is attested; nothing in this crate opens, decompresses or unpacks it.

```
{release_channel}/{S}/{V}/index.json
{release_channel}/{S}/{V}/{S}-{V}-{platform}.tar.zst
{release_channel}/{S}/{V}/{S}-{V}-{platform}.tar.zst.sigstore.json
```

The platforms are exactly `linux-x86_64`, `linux-arm64` and `macos-arm64`; the daemon knows its
own from the target it was compiled for (`Platform::current`).

| module | what it holds |
|---|---|
| `index` | `Platform`, `ReleaseIndex` (read strictly: exactly its fields, schema 1, the service and version asked for, each platform once and in order, at least one), `index_url` and `package_url` (the one place each is built). Held here under the names the shared `hostkeeper-config` types take, until those land |
| `fetch` | `fetch_package`: reads the index, takes the entry for the host's platform, downloads that package into the staging file `packages/<service>/.fetch-<pid>-<n>` (created exclusively, never through a link, mode 0600), byte counting and SHA-256 in the pass that writes it, checked against the entry's size and SHA-256, the size cap and the deadline. `remove_orphan_fetches`, which the daemon calls at start and the fetch calls first |
| `prove` | `prove`: downloads the attestation bundle, reads the commit and ref it names, asks the `Verifier`, and returns the `VerifiedArchive`, which has no other constructor |
| `ports` | `Fetcher` (one GET into a `Sink`, bounded by a `Limit`) and `Verifier` (one attestation, offline), and the data that crosses them |
| `net` | the real `Fetcher`: the adapter's rules over a `Transport`, and `UreqTransport` (rustls, HTTPS only, no redirect, no proxy, no cookies, no credentials, the roots the daemon gives it) |
| `gh` | the real `Verifier`: the pinned `gh attestation verify`, offline, ported from kbf's `GhCheck`; `SystemRunner` runs it and kills it at the deadline, `OwnedBy` checks `gh` and the trusted root before each run |

## Failures

Every refusal is an outcome, a class and a reason token, and removes the staging files. `Deferred`,
class `environment`, means nothing changed on the host and the coordinator retries the order;
`Failure`, class `release`, means the package is bad.

| token | outcome, class | when |
|---|---|---|
| `index_invalid` | `Failure`, `release` | the release index is over 64 KiB, is not JSON, has a field too many or too few, another schema, service or version, a file name or digest that is not the package's form, no package, or a platform listed twice or out of order |
| `index_digest_mismatch` | `Failure`, `release` | the order names the SHA-256 of the release index and the index received hashes to another; checked before the index is parsed, and nothing after it is fetched |
| `no_package_for_platform` | `Failure`, `release` | the index lists no package for the host's platform |
| `package_digest_mismatch` | `Failure`, `release` | the entry's size is over `max_package_bytes`, the announced length or the bytes received pass the entry's size (the transfer stops at once), or the bytes received are fewer than it or do not hash to its SHA-256 |
| `fetch_failed` | `Deferred`, `environment` | the index or the package: no connection, a status other than 2xx (a 404 included: the channel may not have the version yet), a body cut short, `fetch_timeout_secs` passed, or a staging file that cannot be made, written or read for a reason other than a full disk |
| `no_space` | `Deferred`, `environment` | the disk is full while the index, the package or the bundle is written. Nothing else is removed to make room |
| `attestation_unavailable` | `Deferred`, `environment` | the bundle cannot be downloaded, or the verifier cannot run, is not trusted, gives no verdict or runs past `attestation_timeout_secs` |
| `not_attested` | `Failure`, `release` | the bundle is over 1 MiB, is not a bundle, names no commit or no ref, or does not list the package's SHA-256 (the verifier is not run), or the verifier rejects it |

The fourth refusal of the release layout, `package_manifest_mismatch` (the archive's manifest names
another service, version or platform than asked for), belongs to the manifest step after unpacking;
`VerifiedArchive::platform` gives that step the platform to compare.

A token is only ever added: `outcome_tokens_are_the_documented_set` checks that this table and the
crate's `REASONS` hold the same set, and that no token that shipped is gone.

## Where the attestation comes from, and what is checked

The index is a lookup, not a trust anchor, and is not signed. The package must have the size and
SHA-256 the index gives, be attested by the trusted signer, and name in its manifest the service,
version and platform asked for. So a tampered index can at most cause a refusal, or the install of
another build that the trusted signer attested for the same service, version and platform (one who
controls the channel can swap the index entry, the archive and its bundle together for another such
build); never anything unattested, and never another service, version or platform. Nothing on the
host pins one build of a version: an order names only the service and the version.

The packages directory (`/var/lib/hostkeeper/packages`) is root's: the daemon creates it root-owned
and closed to group and others. A service directory under it that group or others can write is
refused (`fetch_failed`). `gh` and the Sigstore trusted root are installed in root-owned
directories; the verifier checks each file and its directory before every run.

The bundle is fetched from the same release channel, at the package's URL with `.sigstore.json`
appended, through the same bounded downloader: at most 1 MiB, within `attestation_timeout_secs`,
which bounds the whole proof (not `fetch_timeout_secs`). The suffix is one constant.

The design asks for a "source commit equal to the package's", and an order carries no commit. The
package's commit is read as the commit the attestation's own provenance records for the digest
(the one `gitCommit` of its `resolvedDependencies`), and the verifier is asked to verify with
exactly that value (`--source-digest`), so a bundle that lies about its commit fails. The ref is
read from the bundle's certificate, matched against the config's `refs` globs (a `*` stays within
one `/`-separated component), and passed to the verifier (`--source-ref`); a ref no glob matches
is rejected without running the verifier.

`gh` is asked for the configured organisation (`--owner`), for the configured workflow in any
repository of it (`--cert-identity-regex`, the config values escaped), for the GitHub Actions
issuer, an SLSA provenance v1, and hosted runners when `hosted_runner` is true.

Two rulings of the design apply:

- The org-level signer has no binding between repository and service (Q12, option (a)): any
  repository of the organisation may sign a package for any service, and the attested repository
  is never compared with the service name.
- Nothing on the host refuses an older attested package (Q13, option (a)): the downgrade bound is
  the coordinator's `min_version`. No version is compared with anything here, and no floor is
  kept.
