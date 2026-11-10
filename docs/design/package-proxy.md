# Package proxy: the coordinator serves packages to the lab LAN

Status: design, for the maintainers' comments and the owner's. Nothing here is built unless it is tagged **Built** with a path. It is a
living document: when a task below lands, its row changes from **Designed** to **Built** with the path, and the text of the decision
stays. It is the second half of [coordinator-mdm.md](coordinator-mdm.md) (decision of 2026-11-10), which has the listener and the setup
this builds on.

The owner's direction: kill the tailnet-only download server; hosts and Macs pull packages over the public internet, and the coordinator
offers a caching pass-through on the lab LAN so that Macs, wiped hosts and bulk updates use the lab network (a 5G uplink against 25 to
100 Gbit inside), with no object-store cache for now.

## 1. What changes for an operator

| | Today | After this design |
|---|---|---|
| The download server | a tailnet-only nginx container whose tree a sync job filled from each release | none. The container, its sync timer and its tree are removed (section 6) |
| Where a Linux host's apt reads packages | the host's repository entry, whose URL is whatever the group names | `https://<mdm name>/apt/<repo>/`, the coordinator, which reads the public repository once and keeps it |
| Where a Mac gets the hostkeeper pkg | the MDM host's `/pkg/<sha256>.pkg`, from a directory the host provides (**Built** as a design in coordinator-mdm.md 4.7) | the same URL; the coordinator fetches the file from its public origin the first time and keeps it |
| What the operator sets | a directory of pkg files, a channel's address in every daemon config | a cache directory flag and, in the setup page, the origins (section 3) |

What does not change: no client trusts the coordinator for package contents. apt checks the signed `Release` file against the key in the
host's keyring, the daemon checks every package's digest and attestation against its own trusted signers, and the Mac checks the pkg's
signature before it expands it. The proxy can only serve bytes or refuse to.

## 2. What exists today, and what does not

Facts, with the file that says them (paths under `crates/`).

- **Built, repository side.** `hostkeeper-coordinator/src/repository_serve.rs` serves `GET /repositories/<name>/dists/<group>/<rest>` and
  `.../pool/<group>/<rest>` for a **private** repository, to a host that presents its client certificate, streaming the origin's body
  with its `Content-Length` and parsing and signing nothing. It does not cache, serves no public repository (404), and requires the
  certificate. Its origin client is `repository_origin.rs`: one HTTPS GET per read, 30 seconds to connect and receive headers, 30 seconds
  idle per body frame, no redirect followed.
- **Built, client side.** `hostkeeper-install/src/repository/` writes `/etc/hostkeeper/repositories/<name>.sources` with `URIs:` an
  `https://` or `file:/` URI, `Signed-By:` a keyring beside it, and refuses every apt option that weakens a check. apt trusts the system
  roots, **not** the host CA that daemons pin. The consequence drives section 3: the proxy's name must carry a public-CA certificate.
- **Built, Mac side.** `hostkeeper-install/src/macos_pkg/` checks the pkg's signature before it expands it, and
  `hostkeeper-mdm-gate/src/pkg.rs` names the pkg by SHA-256 in the manifest it sends.
- **Built, the channel is gone from the repository.** The archive format, the release indexes, `cargo xtask release channel-sync` and the
  coordinator's `--release-channel*` flags were deleted by #262 and #267 (test `the_release_channel_flags_are_gone` in
  `hostkeeper-coordinator/tests/config.rs`). No daemon fetches from `release_channel`; the loader still requires the key
  (`hostkeeper-config/src/daemon.rs`), so every host config still carries one.
- **Not built, and not evidenced as done.** The operator's step in packages-native.md 10.5 item 3 (stop the sync, delete the channel's
  tree), which is a step on the farm and not in this repository, and A25 (deleting the last readers). Section 6 owns the first.
- **Not there:** any cache, any route for a public repository, any by-digest pkg route that fetches from an origin.

## 3. Components and where it listens

![Components: the coordinator holds the package proxy and its cache](diagrams/coordinator-mdm-components.svg)

The diagram is the one of coordinator-mdm.md; the proxy is the box at the bottom of the coordinator and the origins are on its right.
Everything hostkeeper runs is the one process.

**Decision: the proxy's routes are on the MDM listener** (`--mdm-listen`, port 443, the public-CA certificate of the MDM name, which the
coordinator obtains and renews itself, coordinator-mdm.md 4.10).

- **Why that listener.** apt, which trusts only public roots (above), and a freshly erased Mac, which trusts only public roots too, can
  both reach it with no new trust. The heartbeat and LAN listeners use the host CA and would need a second trust path on every host.
- **What it costs, and what it does not.** The proxy needs a name and a public-CA certificate for it, and nothing else of MDM: no push
  certificate, no ABM token. The setup's TLS step (coordinator-mdm.md 4.10, an uploaded certificate or the in-process ACME DNS-01 with the
  GoDaddy credential) is separate from the push and ABM steps, and each route family is served when its own prerequisites are set: `/apt`
  and `/pkg` when the name and certificate exist, `/enroll`, `/scep`, `/checkin` and `/mdm` when MDM is *configured*. So a Linux-only
  farm sets the name and the certificate up and never uploads an Apple credential; the status shows the proxy line apart from the MDM line.
  The listener's flag keeps the name `--mdm-listen`, since it ships first for MDM and a name once shipped is a lock-in.
- **The proxy is a choice per group, not a requirement.** The owner's direction is that hosts and Macs pull over the public internet and
  the proxy is the LAN pass-through, so a host-group may keep the public URL (`packages.komira.ai`) in its repository entry and never touch
  the proxy; pointing a group at the proxy is a per-group URL choice made where the repository entry is made. If a group is pointed at the
  proxy and the coordinator is down, or its certificate has expired, those hosts' `apt update` fails until it is back or the group is
  repointed. A deb822 stanza that lists two URIs is, as I read apt, two sources and not a fallback, so no second URI is written; the failure
  mode is accepted and listed in section 5 (not verified, section 11 item 4).
- **Admission.** No client certificate is asked. The routes serve public bytes (a public repository, a pkg whose digest the operator
  named), a wiped host before its first enrollment and a Mac before its identity are both admitted, and the listener is bound to the LAN
  interface address only. The guard against an open proxy is the route shape, not the caller: the origin is chosen by the repository
  name or the digest, never by anything in the request.

| Route | Answers | Origin |
|---|---|---|
| `GET /apt/<repo>/dists/<suite>/<rest>` | an index (`Release`, `InRelease`, `Packages*`, `Sources*`) | the repository's base URL, the same path |
| `GET /apt/<repo>/pool/<rest>` | a package file | the same |
| `GET /pkg/<sha256>.pkg` | the pkg whose digest an `installs` entry **of the current manifest** names; any other digest is 404 with no origin request | the entry's `url`, if it has one; else only the file in `--mdm-pkg-dir` |
| `HEAD` on the routes above | the headers of the same answer, no body, from the cache or one origin `HEAD` | as above |
| anything else, or any method but `GET` and `HEAD` | 404 or 405 | none |

apt sends conditional and range requests, so both are part of the routes and not left to chance. `If-None-Match` and `If-Modified-Since`
are answered `304` from the cache's validator when it still matches, with no origin request inside the index TTL; `Range` on a cached file
is served as `206` from the file, and on a miss a range request is passed through to the origin without being stored (a partial body is
never committed), after which a full request fills the cache.

`<repo>` must be a name in the registry (**Built**, `hostkeeper-coordinator/src/registry.rs`) marked public, with a base URL; the proxy's
settings add the origin list (section 4). The path passes the locator grammar `repository_serve.rs` already uses
(`hostkeeper_release::grammar::check_locator`: no empty, `.` or `..` segment, no `%`).

## 4. Decisions

![Package proxy: index, pool file and pkg by digest](diagrams/package-proxy-fetch.svg)

**What is cached, and for how long.**

- *Indexes* (`Release`, `InRelease`, `Packages*`, `Sources*`, anything under `dists/`) change in place, so an entry is served without
  asking the origin while its age is under `proxy_index_ttl_s` (default 300), and after that revalidated with a conditional GET
  (`If-None-Match` or `If-Modified-Since`); a `304` renews the entry. A host that runs `apt update` just after a release therefore sees it
  within one TTL.
- *Pool files and pkgs* are immutable under their name (a version, a digest), so they are kept until evicted.
- *The cache* is a directory flag, `--proxy-cache-dir` (a directory the host provides; unset means the proxy answers 404 to the routes
  above, which is the state before a deployment opts in). Files are written to `tmp/` and renamed into `objects/` in the same
  filesystem, named by the SHA-256 of the key (origin and path); a sidecar holds the origin's validator, the fetch time and the length.
  Leftover `tmp/` files are removed at start.
- *Eviction* is least recently used when a commit would pass `proxy_cache_max_bytes` (default 20 GiB), deleting only committed objects.
  Every number here is a setting of the setup page with its default from the start (coordinator-mdm.md 4.9): `proxy_index_ttl_s`,
  `proxy_cache_max_bytes`, `proxy_fetches` (concurrent origin fetches, default 8), `proxy_redirects` (default 3), `proxy_wait_s`.

**One origin fetch per key.** A miss starts one fetch. The first asker gets the body streamed while it is written; a second asker for the
same key (a rack of hosts running the same update) waits at most `proxy_wait_s` (default 10) for the commit and then reads the file; a
waiter whose wait ends first is given a direct pass-through of its own, uncached, so one slow fill cannot stall fifty hosts for the whole
fill, and if the fetch failed the next waiter starts its own. A direct pass-through counts against `proxy_fetches`. This is what turns
fifty hosts into one pull over the uplink in the usual case and bounds the worst one. Nothing is held whole in memory, and no size cap is put on
a file, the pattern of `repository_serve.rs`.

**Redirects.** GitHub releases answer with a redirect to a storage host, and `repository_origin.rs` follows none, so the proxy's origin
client follows up to `proxy_redirects` redirects, https only, and never to a host named by the client. The pkg digest check below is what
makes that safe.

**Digest rule for the pkg.** The coordinator hashes the body while it streams, and the file is renamed into `objects/` only if the
SHA-256 equals the one in the path; on a mismatch the client gets `502 origin_failed` and nothing is kept. A pkg digest is the capability,
so only digests an `installs` entry names are fetched at all. This is a second check, not the first: the Mac still verifies the pkg's
signature, and the daemon still verifies what it fetches.

**Failure of the cache is not failure of the answer.** If the disk is full or a write fails, the body is still streamed from the origin and
nothing is stored; `Status` counts the pass-throughs. If the origin is down and the entry is in the cache, an immutable entry is served; an
index older than its TTL is not (a stale index could hide a release), the client gets `502 origin_failed`.

**Where the origins come from.** A public repository's base URL (the registry entry, or the setting `proxy_origins` when the registry holds
none), and for a pkg, the `url` of its `installs` entry. Which origin holds the signed macOS pkg today is Q9.

**Host configuration.** A group's repository entry names `https://<mdm name>/apt/<repo>/` as its URI. The daemon config's `release_channel`
key stays required by the loader (above); until a loader that does not read it has shipped for a release, every config sets it to
`https://<mdm name>/`, so no config names the dead server. Dropping the requirement is additive for readers (a new loader accepts both) and
is task PP5.

## 5. Edge cases, with the mechanism

| Case | Mechanism |
|---|---|
| A wiped host's first `apt update`, before any enrollment | the routes need no certificate; the name is public-CA, so apt trusts it |
| The coordinator is down or the proxy's certificate has expired | `apt update` fails for the groups pointed at the proxy; groups on the public URL are unaffected; the operator repoints a group (one entry) or restores the coordinator; the certificate's expiry is alerted by 4.10 |
| `apt` asks with `Range` or `If-Modified-Since`, or sends `HEAD` | served from the cache as `206`, `304` or headers only (section 3); a partial origin body is never stored |
| One slow fill and fifty waiters | each waiter waits `proxy_wait_s` at most, then is passed through directly; the fill still commits |
| An arbitrary digest asked of `/pkg/` | 404, and no origin request is made, since only digests of the current manifest's `installs` are fetched |
| The origin serves a poisoned or truncated file | apt rejects a bad hash or signature; the daemon rejects a bad digest; the coordinator rejects a pkg whose digest differs from its path; a truncated body (length short) is not committed |
| A client disconnects mid-fill | the fill continues to its end (bounded by the 30 s idle deadline per frame) so the next asker hits; a fill that fails leaves only a `tmp/` file, removed at start |
| Two coordinators on one cache directory | not supported: the proxy is part of the single-node MDM listener (coordinator-mdm.md 4.9); the directory is the coordinator's |
| A request for an arbitrary URL, or a `..` segment | the route shape and the locator grammar refuse it with 404 or 400; nothing is fetched |
| The origin is slow | 30 s to headers and 30 s per frame, as `repository_origin.rs` has them; waiters share one fetch; `proxy_fetches` caps concurrent origin pulls and the next request gets `503` |
| A release is published and the index is cached | within `proxy_index_ttl_s` a host may miss it; the rollout waits for the next tick, which the coordinator controls |
| The cache directory is wiped | the next requests refill it; nothing in the store refers to a cached file |
| An index and a pool file from different times | apt re-reads `Release` and refuses a `Packages` file whose hash differs; the next `apt update` heals it |

## 6. Retiring the download server

The repository side is done (section 2). What remains is on the farm, in the komira repository's held scripts, which are the owner's and
are held scripts under S3 (dry-run default, never forcing):

The order matters, because packages-native.md 10.5 and its stage table say the channel is needed until stage 9 has converged: a daemon
before `target-v1` fetches its archive through it, an older binary requires `release_channel` after a fallback, and a rollback uses it.

1. **Confirm stage 9's two counts are zero**, by the two `hkctl` queries of
   [docs/operations/move-to-native.md](../operations/move-to-native.md) (stage 9): no registered host below the `target-v1` daemon, and no
   host whose desired release still lists an archive entry. The repository's own reading of the daemon is only part of the evidence (the
   loader reads `release_channel` and the daemon does not); the farm's configs and any older daemons are what the counts cover.
2. **Point `release_channel` in every host config at the proxy's name.** This needs no new code (the loader still requires the key, so
   PP5 is not a blocker) and no host reads it for anything else.
3. **Repoint repositories and Mac profiles** to the proxy (per group, section 3), then check one `apt update` on a host and one pkg pull
   by a Mac through the proxy before going on.
4. **Stop the sync's timer first, and keep the channel container for one release**, then remove the container, its tree and its
   certificate, and the held script that wrote its configuration.
5. Remove the stale statements: packages-native.md 10.5 and 10.7 still read as future, and `hostkeeper-design.md` still draws the release
   channel in its components diagram (task PP4).

**Rollback.** Until step 4 is done: restart the container from its systemd unit, which is left installed (as the MDM design leaves Caddy's),
and repoint the groups' entries and `release_channel` back. After step 4 the rollback is a redeploy from the held script, which is why the
container waits one release.

**Status: Designed.** Nothing here is evidenced as done; the steps are the owner's.

## 7. Compatibility (B8)

New flag `--proxy-cache-dir`; new settings and routes; new optional `installs` field `url`. No shipped flag, format or wire field changes.
`--mdm-pkg-dir` keeps meaning "files the host provides". The previous release's daemons and Macs fetch from the same URLs as before.
An older coordinator does not have the routes: a host pointed at it gets `404` and keeps the sealed copy of what it has installed.

## 8. Tests (to build with each task)

- A fake origin and a fake clock: an index inside its TTL is not refetched; after it, one conditional GET, and a `304` renews it
  (T10: the clock is injected).
- Fifty concurrent askers of one file cause one origin fetch; a failed fetch lets the next waiter fetch.
- A pkg whose bytes differ from its path's digest is not stored and answers 502; an equal one is stored and a second request is a hit.
- A request outside the route shape, a path with `..`, a repository not in the registry, a non-GET: each refused with its status and no
  origin request. An arbitrary digest asked of `/pkg/` is 404 with no origin request; a digest of the current manifest's `installs` is
  fetched.
- `Range`, `If-None-Match`, `If-Modified-Since` and `HEAD` are answered as in section 3; a range miss stores nothing.
- Fifty askers with a fill that never finishes: each is passed through after `proxy_wait_s` on the fake clock.
- A full cache evicts the least recently used committed object only; a disk write failure still streams the answer.
- A redirect to http, or one past `proxy_redirects`, is refused.
- Hermetic: no network, no fixed port, the cache directory is a scratch directory (T12).

## 9. Tasks, one pull request each

| # | Task | After | Size |
|---|---|---|---|
| PP1 | The cache: directory layout, atomic commit, LRU eviction, single-flight, with the settings and their defaults | coordinator-mdm.md CM20 | M |
| PP2 | `GET /apt/<repo>/...` on the MDM listener: public-repository routes, index TTL and revalidation, redirect-following origin client | PP1, CM11a | M |
| PP3 | `GET /pkg/<sha256>.pkg` pulls through from the `installs` entry's `url`, digest-checked before commit | PP1, CM11c | S |
| PP4 | Docs: the packages-native.md and hostkeeper-design.md statements about the channel | none | S |
| PP5 | The loader no longer requires `release_channel` (additive read), then configs stop carrying it one release later | PP2 | S |
| PP6 | Farm: retire the download server and repoint hosts and Macs (held scripts, the owner's), in the order of section 6 | PP2, PP3, and stage 9's counts at zero (packages-native.md A24 converged) | S |

## 10. Open question

- **Q9** Which origin holds the signed macOS pkg: GitHub releases or packages.komira.ai? The proxy takes either (an `installs` entry's
  `url`); the answer decides which one the release workflow must publish to.

## 11. Not verified

1. Where the registry entry of a public repository keeps its base URL, or whether `proxy_origins` is needed (read `registry.rs` before PP2).
2. That apt on the farm's hosts accepts the proxy's certificate with system roots alone (the farm's hosts have the public roots; confirm with
   one host before PP6).
3. That a GitHub release asset's redirect chain stays within https and three hops.
4. That apt treats two URIs of one deb822 stanza as two sources and not as a fallback (section 3); check with one host before PP6.
5. Whether the proxy's routes can be served with the name and certificate alone while the MDM routes still answer 503 (section 3); the
   listener's route-family switch is read in CM11a.
