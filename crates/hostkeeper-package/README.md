# hostkeeper-package

Steps 1 and 2 of fetching a package ([docs/design/hostkeeper-design.md](../../docs/design/hostkeeper-design.md),
section 5.5): download the archive an order names from the release channel, and prove that a
trusted signer built it. The result is a `VerifiedArchive`, a staging file whose bytes hash to the
order's digest and are attested; nothing in this crate opens, decompresses or unpacks it.

| module | what it holds |
|---|---|
| `fetch` | `fetch_package`: the URL `<release_channel>/<service>/<service>-<version>.tar.zst` (`package_url`, the one place it is built), the staging file `packages/<service>/.fetch-<pid>-<n>` (created exclusively, never through a link, mode 0600), byte counting and SHA-256 in the pass that writes it, the size cap and the deadline. `remove_orphan_fetches`, which the daemon calls at start and the fetch calls first |
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
| `digest_mismatch` | `Failure`, `release` | the announced length or the bytes received pass `max_package_bytes` (the transfer stops at once), or the bytes do not hash to the order's digest |
| `fetch_failed` | `Deferred`, `environment` | no connection, a status other than 2xx (a 404 included: the channel may not have the version yet), a body cut short, `fetch_timeout_secs` passed, or a staging file that cannot be made or written for a reason other than a full disk |
| `no_space` | `Deferred`, `environment` | the disk is full while the archive or the bundle is written. Nothing else is removed to make room |
| `attestation_unavailable` | `Deferred`, `environment` | the bundle cannot be downloaded, or the verifier cannot run, is not trusted, gives no verdict or runs past `attestation_timeout_secs` |
| `not_attested` | `Failure`, `release` | the bundle is over 1 MiB, is not a bundle, names no commit or no ref, or does not list the archive's digest (the verifier is not run), or the verifier rejects it |

A token is only ever added: `outcome_tokens_are_the_documented_set` checks that this table and the
crate's `REASONS` hold the same set, and that no token that shipped is gone.

## Where the attestation comes from, and what is checked

The bundle is fetched from the same release channel, at the archive's URL with `.sigstore.json`
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
