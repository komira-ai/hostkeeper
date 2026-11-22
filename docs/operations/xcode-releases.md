# Xcode releases

The coordinator reads the list of Xcode releases once a day (`--xcode-feed-url`; an empty value turns
the reading off) and keeps it. A failed read is a log line and another try the next day, never an alert.

With `--xcode-dir` it also keeps a private store of the Xcode archives themselves. This page says what
the store holds, how an archive gets into it (`hk xcode-push`) and out of it, and what an operator
sees.

## The store

`--xcode-dir` names a directory that already exists (the coordinator does not make it, and does not
start when it is missing or is a file, naming the flag). With several coordinators on one database,
every instance mounts the same directory. Without the flag there is no store: the two routes below
answer 404 `not_found`, `Xcode.List` answers that no store is configured, and `Xcode.Remove` is refused
`no_store`.

The archive is kept exactly as Apple ships it: the `.xip` as it was uploaded, never repackaged, unpacked
or changed. Only a host of a group whose `xcode_keep` is at least 1 reads it (the route below answers such a host
and nobody else); it is not served to anyone else.

Each archive is two files in the directory, both mode 0600:

- `<sha256>.xip`, the bytes, named by their SHA-256;
- `<sha256>.json`, the record: the version, the build, the size, the name of the token or user that
  uploaded it, and when (seconds since the epoch).

An upload is written to `.upload-<sha256>-<token>-<n>` first (`<token>` is random bytes the store draws when it opens, so two
coordinators that mount the directory never make one name) and becomes an archive only when its
SHA-256 is the name it was uploaded under; the record is written before the archive is renamed into
place, so an archive is held only while both files exist. A start removes the `.upload-` files whose last write is more than 24 hours old (a stopped coordinator's
left uploads); a newer one may be an upload another coordinator of the directory has in progress, and stays. A record that cannot be read is left out of the list and logged.

`--xcode-max-bytes` (default 21474836480, 20 GiB; at least 1) is the most bytes of one archive.

## Pushing a release from your Mac: `hk xcode-push`

When the alert `xcode_release_waiting` names a release, an operator brings it in with one command on
their own Mac, the one signed in to the organisation's Apple developer account:

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> xcode-push 27.2
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> xcode-push ~/Downloads/Xcode_27.2.xip
```

It runs on macOS only (anywhere else it says so and stops) and needs `xcodes` on the `PATH` for the
first form. In order, and a step that fails uploads nothing:

1. Given a version, `xcodes download <version> --directory ~/Downloads/hk-xcode-push/<version>`, in
   your own session: `xcodes` uses the Apple sign-in kept in your Keychain, and asks for a two-factor
   code at the terminal only when Apple has ended that session. The directory must then hold exactly
   one `.xip` (the archive is kept there afterwards). Given a path, `hk` takes that file, downloaded
   in a browser from Apple's developer site.
2. `pkgutil --check-signature` on the archive must exit 0, give the status `signed Apple Software`
   (Apple's own signature, not a developer's) and a certificate chain that ends at Apple Root CA,
   matched by its SHA-256 fingerprint.
3. `hk` computes the archive's SHA-256 and size, then reads its version (`CFBundleShortVersionString`)
   and build (`ProductBuildVersion`) with `plutil` from the app that `xip --expand` makes in a directory
   beside the archive, `.hk-xcode-push-expansion`, which it removes afterwards (and first, if an
   interrupted push left it). That takes as much free space as an installed Xcode, on the archive's
   volume, and some minutes. Given a version, the archive's must be it.
4. The archive is uploaded unmodified and streamed with the `PUT` below, with your token from
   `--token-file` as the bearer: the token is never an argument, an environment variable of a program
   `hk` runs, or a line it prints. The request says `Expect: 100-continue`, so a refusal the
   coordinator makes before the body (`not_listed`, `already_held`, `not_kept`, `unauthorized`) comes
   back before the archive is sent.

`--timeout-secs` bounds the connection and the wait for the coordinator's `100 Continue`; once the
archive is sent, its answer is waited for 30 minutes at least, as long as the coordinator may take to
sync, audit and store it. The answer, the archive stored, is printed. A refusal prints `refused:
<error>` (the table below) and exits 3 for 401 and 403, 2 for another 4xx and 1 otherwise; an upload cut
off before its answer keeps nothing in the store and is pushed again. `--token-stdin` is not taken: the
terminal is `xcodes`'s, for the two-factor prompt.

## Pushing an archive: `PUT /v1/xcode/<sha256>?version=<version>&build=<build>`

The route is on the private listener, the one operators use. The caller sends an operator token as the
bearer (an operator token or a user's API token; CI's token is refused) and the archive as the body; the
path names the SHA-256 of the body as 64 lowercase hex characters, and the query holds exactly `version`
(digits and dots, `27.2`) and `build` (letters, digits and dots, at most 32, `27B52`), each once.

The checks run in this order, and a refusal stores nothing. Each answer is JSON, `{"error":"<code>"}`:

| status | `error` | when |
|---|---|---|
| 405 | `method_not_allowed` | the method is not PUT |
| 403 | `forbidden` | the connection presented a client certificate (a host never uploads); checked before the token is read |
| 401 | `unauthorized` | no operator token, or one that is not an operator's |
| 400 | `bad_request` | the path is not a SHA-256, the query is not exactly `version` and `build`, either is malformed, the body ends in an error, or the body stalls for longer than the connection's patience |
| 413 | `too_large` | `Content-Length` is above `--xcode-max-bytes`, or the body turns out to be (the upload stops there) |
| 409 | `not_listed` | the list of releases holds no release with this version and build: betas and release candidates are not stored |
| 409 | `already_held` | the store holds this release, or an archive of these bytes |
| 409 | `not_kept` | the store would remove this release at once: it is older than the releases the store keeps (below) and no host has it installed; the body is not read |
| 409 | `xcode_version_taken` | the store holds another build of this version (below) |
| 409 | `upload_in_progress` | another upload of this version is running |
| 400 | `digest_mismatch` | the SHA-256 of the body is not the one in the path |
| 503 | `audit_unavailable` | the audit entry could not be written, so nothing was stored |
| 500 | `store_failed` | the list of releases or the directory could not be read or written |

A stored archive answers 201 with `{"sha256", "version", "build", "size"}`. The upload is checked against
the store again, under the store's lock, once its bytes are in, so two uploads that race cannot both be
stored. The audit entry is written before the archive is put in place: an archive is never stored without
its entry.

A version names one build on our hosts. A second build under a version the store holds is refused
`xcode_version_taken` and raises the alert below, until the held archive of that version is removed.

## Reading an archive: `GET /v1/xcode/<sha256>`

The route is on the heartbeat listener, the one the hosts use. Only a host whose group keeps Xcode
(`xcode_keep` of the group's maintenance policy, at least 1) reads it; the caller is identified by its
client certificate as for the repository routes.

| status | `error` | when |
|---|---|---|
| 405 | `method_not_allowed` | the method is not GET |
| 403 | `forbidden` | no client certificate, a certificate that names no host, or a host whose group keeps no Xcode |
| 403 | `denied` | the host is not admitted |
| 404 | `not_found` | the path is not a SHA-256, or the store does not hold it |
| 500 | `store_failed` | the record or the file could not be read |

The answer is the file, streamed in chunks and never read whole, with `Content-Type:
application/octet-stream`, `Accept-Ranges: bytes`, `ETag: "<sha256>"` and the file's own size as
`Content-Length`. A download that stops can be resumed with `Range`:

- one range of `bytes=<first>-<last>`, `bytes=<first>-` or `bytes=-<count>` (decimal, no spaces) is
  answered 206 with `Content-Range: bytes <first>-<last>/<size>`;
- a range that starts at or past the end, a suffix of 0, or any range of an empty file is answered 416
  with `Content-Range: bytes */<size>` and no body;
- several ranges, a unit other than `bytes`, or a header that does not parse is ignored and the whole
  file is answered 200;
- an `If-Range` that is not the archive's ETag also ignores the range, so a changed file is read from the
  start.

## What stays: the prune

The store keeps the newest releases, as many as the largest `xcode_keep` of any host group's maintenance
policy (and at least one), and removes the rest, newest first, each removal audited before it is made. A release older than those the store keeps
is refused `not_kept` when it is pushed, before its body is read and again under the store's lock
before it is stored, so the release just pushed always stays. A
release a host reports installed is never removed; the prune does not read the Xcodes the Macs report
yet (they decide only what is due on a Mac and what its run removes, below), so today none is held
back by that rule. The prune runs after each stored upload and in the daily read of the list.
Each archive it removes is audited first, as `xcode_remove` by `coordinator`, with the reason `pruned:
Xcode <version> (<build>), sha256 <sha256>`.

## When a Mac is due an Xcode

A Mac's daemon reports the Xcodes installed in `/Applications` and the one `xcode-select` names, and
lists no Xcode due: the coordinator decides it, from the Mac's group and the store. In a group whose
`xcode_keep` is 0, nothing of Xcode is ever due and no run changes an Xcode. Otherwise the group keeps
the newest `xcode_keep` releases the store holds, and `xcode` is due on its Mac when one of them is not
installed in its own directory (`Xcode-<version>.app` with that build: a copy elsewhere, such as an
App Store `Xcode.app`, does not count), or when `xcode-select` does not name the group's
`xcode_default` (`latest`, the newest kept release, or a kept version; a version the store keeps no
release of leaves `xcode-select` as it is). So a newer Xcode becomes due only once it is pushed to
the store.

The host page shows it among the due items, from the version `xcode-select` names to the release
missing (or the default), with that release's build; the Mac counts toward the alert `updates_due` of
its group, and a Schedule or Update with `xcode` takes it. The `maintain` order of its turn carries
the kept releases (version, build, size and SHA-256, newest first), the versions to remove (each
installed in its own directory and older than every kept release; never one in another directory)
and the default; a turn with both macOS and Xcode is ordered macOS first. While the store's directory
cannot be read, no Mac is shown due an Xcode and a turn with `xcode` waits.

## The audit entries

- `xcode_put`: an upload was stored. The entry names the operator and carries the reason `Xcode
  <version> (<build>), sha256 <sha256>, <size> bytes`.
- `xcode_remove`: an archive was removed, by an operator's `Xcode.Remove` (the operator's reason) or by
  the prune (as above).

`Xcode.List` writes no entry.

## Seeing and removing archives

`hk xcode list` lists the archives, newest release first, with each one's SHA-256, version, build,
size, who pushed it and when; whether a store is configured; how many newest releases it keeps (the
largest `xcode_keep`, at least 1); the most bytes of an archive; and the release the alert
`xcode_release_waiting` names, if any.

`hk xcode remove --sha256 <sha256> --reason <text>` removes an archive: the reason is required, 1 to
256 bytes, with no control character. It is audited as `xcode_remove` before the files go. A CI token
cannot call either. The refusals are `unauthorized`, `forbidden`, `invalid_request` (a SHA-256 that is
not 64 lowercase hex characters, or a bad reason), `no_store`, `unknown_archive`, `store_failed` and
`audit_unavailable`.

Removing the held archive of a version is what clears `xcode_version_taken` for it: the other build can
then be pushed.

## The alert `xcode_release_waiting`

The coordinator raises the warning `xcode_release_waiting` while the list holds a release (not a beta,
not a release candidate) newer than any release its Xcode store holds. The message names the release's
version and build and the macOS it requires, and the alert's `version` is the release's version. There
is one alert, for the newest such release. See it with `hk alerts list`.

It clears when the store holds that release or a newer one. What to do: push the release with
`hk xcode-push <version>` (above), from a Mac signed in to the Apple developer account. Without `--xcode-dir` the store
holds nothing, so the alert stands while the list holds a release.

## The alert `xcode_version_taken`

The coordinator raises the warning `xcode_version_taken` for each version whose second build was
refused. The alert's `version` is the version; the message names the build the store holds and the build
that was refused. It is the coordinator's memory, like a failed canary's: it stands until the held archive
of that version is removed (`hk xcode remove`) and a restart forgets it. To switch builds, remove the
held archive, then push the other build.
