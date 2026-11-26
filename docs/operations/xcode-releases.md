# Xcode releases

The coordinator reads the list of Xcode releases once a day (`--xcode-feed-url`; an empty value turns
the reading off) and keeps it. A failed read is a log line and another try the next day, never an alert.

With `--xcode-dir` it also keeps a private store of the Xcode archives themselves. This page says what
the store holds, how an archive gets into it (`hk xcode add`) and out of it, and what an operator
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

An upload sent in parts (below) is kept between its requests as three more names in the directory, each
mode 0600: `.partial-<sha256>`, the bytes received; `.partial-<sha256>.json`, the version, build and
size it was started with and the name of the token or user whose request started it; and, while a request
is writing it, `.partial-<sha256>.lock`. A partial upload that has not been written for 24 hours is removed
with its record and its lock, by the start of a coordinator and by the daily `xcode-feed` job, each
removal a log line (`a lapsed file of the Xcode store was removed`, naming the file) and no audit entry,
since nothing of it was ever stored. The same sweep removes `.upload-` files that old.

## Adding a release from your Mac: `hk xcode add`

When the alert `xcode_release_waiting` names a release, an operator brings it in with one command on
their own Mac, the one signed in to the organisation's Apple developer account:

```
hk xcode add 27.2
hk xcode add ~/Downloads/Xcode_27.2.xip
hk xcode add
```

The coordinator's address, the CA and the token file come from the settings file
`~/.hostkeeper/hk.toml` (`hk settings init`, [cli.md](../cli.md)); the global flags `--endpoint`,
`--ca-file` and `--token-file` override it. `hk xcode-push <version> | <path>` is a second name of the
same command for one release: it says `hk xcode-push is now hk xcode add` on stderr and then does what
`hk xcode add` does.

It runs on macOS only (anywhere else it says so and stops). One run at a time per Mac: the lock
`~/.hostkeeper/xcode/.lock` holds the running process's id, and a second run exits 1 naming it and
does not wait; a lock whose process is gone is taken over. In order, and a step that fails uploads
nothing:

1. **The archive.** A path: that file. A version: the newest file named for it in `~/Downloads` or in
   `~/Downloads/hk-xcode-push/<version>`, else `xcodes download <version> --directory
   ~/Downloads/hk-xcode-push/<version>` in your own session: `xcodes` uses the Apple sign-in kept in your
   Keychain, and asks for a two-factor code at the terminal only when Apple has ended that session. A
   missing `xcodes` is said with the two ways out: install it, or download the archive in a browser
   from Apple's developer downloads page and run `hk xcode add` again. Nothing given: the file directly
   in `~/Downloads` named for the highest version, then the newest; with none, the release the store
   waits for (as if its version were given), and when none waits, `the store holds the newest release
   (27.2, 27B52); nothing to add` and exit 0. `hk` picks a file up on its own only when its name is
   `Xcode`, `_` or `-`, a version and `.xip` (`Xcode_27.2.xip`, `Xcode-26.1.1.xip`); a trailing `.0` of
   a version does not count (`27.2` is `Xcode_27.2.0.xip`). Any other name is taken only as a path.
2. **The kind**, from the first four bytes: `xar!` (a `.xip`) goes on; `AA01` (Apple Archive) and
   `AEA1` (Apple Encrypted Archive) stop with `archive_kind_unsupported: <kind>` (exit 1), since the
   farm Macs defer them; anything else is `not_an_xip` (exit 2).
3. **The SHA-256 and size**, read once in 8 MiB blocks; on a terminal the progress is one line
   rewritten in place.
4. **The store is asked** (`Xcode.List`). An archive of these bytes already held prints `already in the
   store: Xcode 27.2 (27B52)` and ends with exit 0, running nothing else; with a version given and the
   held archive another version, `version_mismatch` (exit 1). With nothing given, a newer release
   waiting is named with the line that adds it.
5. `pkgutil --check-signature` on the archive must exit 0, give the status `signed Apple Software`
   (Apple's own signature, not a developer's) and a certificate chain that ends at Apple Root CA,
   matched by its SHA-256 fingerprint.
6. **The version and build.** `~/.hostkeeper/xcode/<sha256>.json` is used, and the expansion skipped,
   when it is the user's own with mode 0600, names this SHA-256 and size, has the verdict `pass` and
   holds a version and a build; any other record is ignored and replaced after a fresh expansion.
   Otherwise the archive's volume must have three times its size free (`free_space_short`, naming both
   numbers), then `xip --expand` makes the app in `.hk-xcode-push-expansion` beside the archive
   (removed afterwards, and first if an interrupted run left it), `plutil` reads
   `CFBundleShortVersionString` and `ProductBuildVersion`, `codesign --verify --deep --strict` must
   pass on the app, and the record `hk-xcode-add-v1` is written. A passing record of `hk xcode verify`
   is never replaced. Given a version, the archive's must be it (`version_mismatch`).
7. **The store is asked again** with the release known. The same release from other bytes, or another
   build of the version, stops the run naming the line `hk xcode remove --sha256 <sha256> --reason
   <why>` that removes the held archive first, if the new bytes are the ones to keep.
8. **The upload resumes.** `hk` asks the store what it holds of these bytes, `GET /v1/xcode/<sha256>/upload`
   (below), with your token as the bearer: `none` starts at byte 0, `partial` continues from the bytes the
   store keeps (an earlier run's, from this Mac or another, within 24 hours), `held` means the archive is
   stored already and nothing is sent. The rest of the archive is sent in one `PUT` with `Content-Range:
   bytes <first>-<last>/<size>` (below), unmodified and streamed from the file (the bytes that were hashed,
   no more): one request and not parts, because a request that is cut off keeps what the store received
   of it, so a cut costs only the bytes in flight. The request says `Expect: 100-continue`, so a refusal
   the coordinator makes before the body comes back before the archive is sent. The token is never an
   argument, an environment variable of a program `hk` runs, or a line it prints. The progress goes to
   stderr (one line rewritten in place on a terminal, otherwise one line per tenth of the archive; a
   resumed run says `continuing at 4.10 of 11.92 GB` and counts its rate from there);
   `stored: Xcode 27.2 (27B52), sha256 <sha256>, <n> bytes` goes to stdout.
   - A connection that breaks, a `409 offset_mismatch` (its answer carries the offset to go on from) and
     a `409 partial_locked` (another upload of these bytes holds the store's lock, not yet stale) are
     tried again from the offset the store then reports (after `partial_locked` and a broken connection,
     by asking again; after `offset_mismatch`, from its `received`), five tries in all, each after a
     wait that doubles from 2 seconds (2, 4, 8, 16). A `202` (a part kept, bytes still due) goes on at
     once. If the archive is found stored by the next query (the answer to the last request was lost),
     the run ends with `stored:` as if the answer had come.
   - Then `hk` stops with `the upload stopped at 7.31 of 11.92 GB; run hk xcode add again to continue`
     (exit 1), the offset being what the store held at the last query. The store keeps the partial
     upload for 24 hours: run `hk xcode add` again, from any Mac with the same archive, and it continues.
   - Any other refusal of the `PUT` or of the query ends the run, said in words, then `refused:
     <code>`, with the exit code of its status (3 for 401 and 403, 2 for other 4xx, among them
     `partial_differs`, whose words say to remove the partial with `hk xcode remove`, 1 for the rest).
   - A coordinator without the query (it answers the `GET` `405 method_not_allowed`) cannot resume: `hk`
     says so and sends the whole archive in one `PUT` with no `Content-Range`, as before; a cut of that
     request keeps nothing. A `404` is a coordinator with no Xcode store (no `--xcode-dir`): `hk` says so
     and exits 1.
9. **The Macs.** After `stored:` and after `already in the store:`, `hk` reads every page of `Hosts.List`
   and `HostGroups.List`, the runs that have not ended (`Maintenance.List`, then `Maintenance.Get` of
   each) and says on stdout which Macs (hosts of the platform `darwin-arm64`) are due the release, by
   group with its `xcode_keep` and batch, by the rule the coordinator decides `xcode` by: the group
   keeps the release (it is among the newest `xcode_keep` releases the store holds) and the Mac lacks it
   in `Xcode-<version>.app` with that build, or has it while `xcode-select` names another and it is the
   group's default (`installed, not selected: due to be selected`). A Mac that lacks a newer release
   too is `due: 27.3, then 27.2 in the same turn`. A Mac of a group that keeps no Xcode or only newer
   releases is `left out` with that reason, and a Mac that has the release as the group wants is
   `installed`. A Mac in an open run that takes `xcode` and has not started its turn is `in run <id>,
   window <start> to <end>` (UTC) or `in run <id>, now`: the coordinator reads the store when the turn
   starts. A Mac in a run that does not take `xcode` is `in run <id>, which does not take Xcode:
   schedule after it ends`, because a run's kinds are fixed when it is scheduled. For each group with a
   due Mac that no run takes, `Maintenance.Schedule` is asked with the kind `xcode`, the reason `Xcode
   <version>` and no `confirm`, which stores nothing: `hk` prints the hosts a run would take and its
   batch, the hosts it would skip with the coordinator's reason, a refusal of the whole run with its
   reason (for `last_host`, the line with `--take-last-host true`), and, for a run it would take, the
   line `no run takes it yet. Press Update on the Maintenance page, or:` and then the line that
   stores the run, `hk maintenance schedule --host-group <group> --kinds xcode --reason "Xcode
   <version>" --confirm true`. `hk xcode add` never stores the run: an install drains a build Mac. A call of the summary that
   fails does not undo the upload: stderr says `summary unavailable: <why>; the archive is stored` and
   names `hk hosts list` and `hk maintenance list`, and the exit is 0. With no Mac known, stdout has
   nothing more and stderr says so.

`--timeout-secs` bounds the connection and the wait for the coordinator's `100 Continue`; once the
archive is sent, its answer is waited for 30 minutes at least, as long as the coordinator may take to
sync, audit and store it. A refusal of the store is said in words with what to do, then `refused:
<error>` (the table below); the exit code is 3 for `unauthorized` and `forbidden`, 2 for another 4xx
(`not_listed`, `not_kept`, `already_held`, `xcode_version_taken`, `upload_in_progress`,
`too_large`, `digest_mismatch`) and 1 otherwise. A coordinator with no store (no `--xcode-dir`) is exit
1. An upload cut off before its answer keeps nothing in the store and is run again. `--token-stdin` is
not taken: the terminal is `xcodes`'s, for the two-factor prompt.

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
| 409 | `offset_mismatch` | a part (below) does not start at the byte the partial upload holds; the answer also has `"received"`, the bytes held |
| 409 | `partial_differs` | a part says another size, version or build than the partial upload it continues |
| 409 | `partial_locked` | another writer, of this coordinator or of another over the same directory, holds the partial upload's lock |
| 400 | `digest_mismatch` | the SHA-256 of the body is not the one in the path |
| 503 | `audit_unavailable` | the audit entry could not be written, so nothing was stored |
| 500 | `store_failed` | the list of releases or the directory could not be read or written |

A stored archive answers 201 with `{"sha256", "version", "build", "size"}`. The upload is checked against
the store again, under the store's lock, once its bytes are in, so two uploads that race cannot both be
stored. The audit entry is written before the archive is put in place: an archive is never stored without
its entry.

A version names one build on our hosts. A second build under a version the store holds is refused
`xcode_version_taken` and raises the alert below, until the held archive of that version is removed.

## Sending an archive in parts: `Content-Range`

A `PUT` that has no `Content-Range` is the one request above, and behaves as it always did. A `PUT` that
has `Content-Range: bytes <first>-<last>/<size>` sends one part of the archive, so that an upload that was
cut off continues from the last byte the coordinator holds instead of from the first. The caller sends a
part for each request, each starting where the last ended; the path and the query are those of the
whole archive.

Every check of the table above runs first, in its order, with these differences: `<size>` is the size
that `too_large` is judged by (`--xcode-max-bytes`), since the body is only a part of the archive;
`Content-Range` must be one range of decimal numbers with `first <= last < size`, and a `Content-Range`
that is not, or that comes twice, is `bad_request`; and `Content-Length` must be present and equal
`<last> - <first> + 1`, else `bad_request`. Then:

1. Under the partial upload's lock (below), a partial upload of this SHA-256 that was started with
   another size, version or build is `409 partial_differs`.
2. `<first>` must be the number of bytes the partial upload holds, 0 when there is none: else `409
   {"error":"offset_mismatch","received":<bytes held>}`, and the caller goes on from `received`. No partial
   upload is made for a first part that does not start at 0.
3. The body is appended to `.partial-<sha256>` and synced. While bytes are still due the answer is `202`
   with `{"received":<bytes held>}`. A body that ends short of its range, fails, stalls for longer than the
   connection's patience or has more bytes than its range is `400 bad_request`; a request that is cut off
   has nobody to answer. Either way the bytes it wrote stay, synced, and the next part continues after
   them.
4. When the last byte is in, the whole file is read once and hashed, the lock refreshed before each
   block (a writer that finds its lock taken stops with `409 partial_locked` and commits nothing). A
   SHA-256 other than the path's is `400 digest_mismatch`, and the partial upload is removed, so the next
   try starts from byte 0. Else the archive is committed as a whole upload is: the checks again under the
   store's lock, the `xcode_put` audit entry first, the record, the rename, the prune, and `201` with the
   same body, and the partial upload is removed. If the commit is refused or fails (`503
   audit_unavailable`, `500 store_failed`, a check of the commit), the answer is that refusal and the
   partial upload stays, with all its bytes.
5. A partial upload that holds all `<size>` bytes takes no body: a request that names any part of it
   (`hk` sends the last byte again) skips step 2, and the file is hashed and committed as in step 4.

Any operator may continue a partial upload: the bytes are named by their SHA-256 and checked whole before
anything is stored, so a partial upload written by someone else cannot make the store hold other bytes
than the path names; the worst it can do is fail the final hash. The partial upload's record names who
started it, and the `xcode_put` entry names the operator whose request completed it.

**The lock.** One request writes a partial upload at a time. The writer holds
`.partial-<sha256>.lock`, which names the writer and the second, by the coordinator's clock, of its last
refresh, and refreshes it once for each second in which it writes a block. A lock is made so that of
two writers one makes it and the other is refused `409 partial_locked` (a second request of the same
coordinator for the same archive is refused the same way). A lock not refreshed for 30 seconds, or
whose text is not a lock's, is stale: the next writer moves it out of the lock's name, and when it is
the text it judged stale removes it and makes its own (a lock made since is put back and that writer is
refused `partial_locked`), and a writer whose lock was taken that way finds it at its next refresh and
stops with `partial_locked`. The lock's file is
made by writing its text to `.partial-<sha256>.lock.<writer>` (created exclusively) and linking that
to the lock's name, which fails when the name is taken; so the directory every coordinator mounts must
have an atomic exclusive create and `link(2)`, as a local file system and NFS do.
Of two writers that both found a lock stale only one takes it. What a writer wrote under a lock it
lost cannot store wrong bytes, because the whole file's SHA-256 is checked before the archive is
stored. The coordinators judge a lock
by their own clocks, so those clocks must agree to within a few seconds.

## Asking what the store holds: `GET /v1/xcode/<sha256>/upload`

On the private listener, with the checks of the `PUT` in their order (`403 forbidden` for a client
certificate, `401 unauthorized` for no operator token or CI's, `400 bad_request` for a path that is not a
SHA-256 and `/upload`). It reads and changes nothing. The answer is `200` with

```
{"state": "held" | "partial" | "none", "received": <bytes>, "size": <bytes>}
```

`held`: the store holds an archive of these bytes (`received` and `size` are its size). `partial`: it
keeps a partial upload of them (`received` is the bytes held, `size` the size the upload was started
with). `none`: neither, and both numbers are 0. A store that cannot be read is `500 store_failed`. A
coordinator without this route answers a `GET` of the path `405 method_not_allowed`, as for any other `GET`
of `/v1/xcode/` on the private listener.

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
release a host reports installed is never removed, nor refused `not_kept`: the prune reads the Xcodes
each Mac's last report names (by version and build, in `Xcode-<version>.app` or in another directory,
such as `Xcode.app`), and keeps each of them however old. The prune runs after each stored upload and in the daily read of the list.
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
- `xcode_partial_remove`: a partial upload was removed by an operator's `Xcode.Remove` of its SHA-256
  (the operator's reason; the subject is `Xcode <version> (<build>) <sha256>`). A partial upload that
  lapses writes no entry.

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

**Uploads in progress.** `Xcode.List` also answers the partial uploads, the one written longest ago
first: each one's SHA-256, version, build, the bytes received, the size, who started it and the time of
its last write. `hk xcode remove --sha256 <sha256> --reason <text>` of a SHA-256 the store holds only as a
partial upload removes the partial upload (and its record), audited `xcode_partial_remove` before the
files go. It is refused `partial_locked` while a writer holds the lock, that is, while the lock has been
refreshed within the last 30 seconds; a lock older than that is taken over and the removal is made.
`unknown_archive` still means that the store holds neither an archive nor a partial upload of it.

## The alert `xcode_release_waiting`

The coordinator raises the warning `xcode_release_waiting` while the list holds a release (not a beta,
not a release candidate) newer than any release its Xcode store holds. The message names the release's
version and build and the macOS it requires, and the alert's `version` is the release's version. There
is one alert, for the newest such release. See it with `hk alerts list`.

It clears when the store holds that release or a newer one. What to do: push the release with
`hk xcode add <version>` (above), from a Mac signed in to the Apple developer account. Without `--xcode-dir` the store
holds nothing, so the alert stands while the list holds a release.

## The alert `xcode_version_taken`

The coordinator raises the warning `xcode_version_taken` for each version whose second build was
refused. The alert's `version` is the version; the message names the build the store holds and the build
that was refused. It is the coordinator's memory, like a failed canary's: it stands until the held archive
of that version is removed (`hk xcode remove`) and a restart forgets it. To switch builds, remove the
held archive, then push the other build.
