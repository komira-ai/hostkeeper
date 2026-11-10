# hostkeeper-package

Step 1 of fetching a package ([docs/design/hostkeeper-design.md](../../docs/design/hostkeeper-design.md),
section 5.5) for an entry that is pinned by its digest, the host's stores, and the checks of an
attestation. `fetch_pinned` downloads the artifact an order's entry names from the host's stores
into a root-only staging file and refuses bytes whose size or SHA-256 are not the entry's; nothing
in this crate opens, decompresses or unpacks it.

The daemon stages a package its installer names from the host's package manager
(`hostkeeper-install`), and uses this crate for `stores` (a Mac's start renders `stores.toml` from
managed preferences), `entry::fetch_pinned` (the artifact of a `macos-pkg` entry, whose package the
installer checks the signature of) and, at its start, for `remove_orphan_fetches`, which removes
the staging files a fetch left. `hostkeeper-pxe` uses `bundle`, `net`, `gh` and `ports`, and
`hostkeeper-mdm-gate` uses `stores`.

| module | what it holds |
|---|---|
| `entry` | `fetch_pinned`: the fetch of an order's `Target` by its pin: the mirrors, then the entry's locations, each source with its own deadline and the credential and redirects of its store; the first bytes with the entry's size and SHA-256 win, counted and hashed in the pass that writes them into the staging file `packages/<service>/.fetch-<pid>-<n>` (created exclusively, never through a link, mode 0600). Returns a `Pinned` and the stores that gave other bytes |
| `stage` | the staging file and the one download loop that fills it; `remove_orphan_fetches`, which `fetch_pinned` calls first and the daemon's start calls for each service |
| `bundle` | the attestation bundle read for what the verifier is asked to confirm (the one commit of the provenance, the certificate's source ref, the subjects' SHA-256), before anything has verified it; `hostkeeper-pxe fetch` reads bundles with it |
| `ports` | `Fetcher` (one GET into a `Sink`, bounded by a `Limit`) and `Verifier` (one attestation, offline), and the data that crosses them |
| `net` | the real `Fetcher`: the adapter's rules over a `Transport`, and `UreqTransport` (rustls, HTTPS only, no redirect, no proxy, no cookies, the credential only when a store's request gives it, the roots the daemon gives it; `UreqTransport::following_redirects` follows a bounded number of `https://` redirects, for `hostkeeper-pxe fetch`'s release assets); `NetFetcher::get_request` follows a store's redirects itself (`https://` only, the credential dropped at another origin) and answers an OCI `Bearer` challenge on the store's own origin |
| `stores` | the stores of `/etc/hostkeeper/stores.toml` (`Stores`, `Store`, `load`): kind `https` or `oci`, base, redirects, mirror, repository, and a credential file read once and never shown by `Debug`; the URL of a blob in each kind; every defect of the file is a `StoresError` that names the store and the rule |
| `gh` | the real `Verifier`: the pinned `gh attestation verify`, offline, ported from kbf's `GhCheck`; `SystemRunner` runs it and kills it at the deadline, `OwnedBy` checks `gh` and the trusted root before each run |

## Failures

Every refusal is an outcome, a class and a reason token, and removes the staging file. `Deferred`,
class `environment`, means nothing changed on the host and the coordinator retries the order;
`Failure`, class `release`, means the package is bad. A token that no code reports any more stays
in the table, marked as not reported: tokens are only ever added.

| token | outcome, class | when |
|---|---|---|
| `index_invalid` | `Failure`, `release` | not reported any more: the release index is no longer read |
| `index_digest_mismatch` | `Failure`, `release` | not reported any more: the release index is no longer read |
| `no_package_for_platform` | `Failure`, `release` | not reported any more: the release index is no longer read |
| `package_digest_mismatch` | `Failure`, `release` | the entry's size is 0 or over `max_package_bytes`, its digest is not a SHA-256, or a location gave bytes other than its size and SHA-256 (the announced length or the bytes received pass the size and the transfer stops at once, or they are fewer or do not hash to it) |
| `fetch_failed` | `Deferred`, `environment` | no connection, a status other than 2xx (a 404 included), a body cut short, `fetch_timeout_secs` passed, a service directory that is a link, is not a directory or is writable by group or others, or a staging file that cannot be made, written or read for a reason other than a full disk |
| `no_space` | `Deferred`, `environment` | the disk is full while the package is written. Nothing else is removed to make room |
| `attestation_unavailable` | `Deferred`, `environment` | not reported any more: the attestation of a package is no longer proven by a fetch |
| `not_attested` | `Failure`, `release` | not reported any more: the attestation of a package is no longer proven by a fetch |
| `no_store` | `Failure`, `environment` | the host defines no mirror, and none of the locations names a store it defines |
| `store_digest_mismatch` | `Deferred`, `environment` | only mirrors answered, and every one gave bytes other than the entry's size and SHA-256 |

`outcome_tokens_are_the_documented_set` checks that this table and the crate's `REASONS` hold the
same set, and that no token that shipped is gone.

## What is checked of an attestation

`hostkeeper-pxe fetch` checks the bundle of a release asset with `bundle` and `gh`; the daemon's
fetch of a pinned artifact checks no attestation.

The packages directory (`/var/lib/hostkeeper/packages`) is root's: the daemon creates it root-owned
and closed to group and others. A service directory under it that group or others can write is
refused (`fetch_failed`). `gh` and the Sigstore trusted root are installed in root-owned
directories; the verifier checks each file and its directory before every run.

The design asks for a "source commit equal to the package's", and an asset carries no commit. The
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
