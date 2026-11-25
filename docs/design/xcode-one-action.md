# Adding an Xcode in one action

Status: design. Built so far: `hk xcode verify` and its record (section 5, X2), as a hand-written method of the generated
service `xcode` (section 2.7 last paragraph); `hk xcode add` (X3, X4): sections 2.2 to 2.4 with resume, 2.6 and 2.7, and the
summary of the Macs (2.5, in `xcode_add/summary/`), in `crates/hostkeeper-hk/src/xcode_add/` and `xcode_push/resume.rs`; the
coordinator's side of resume (section 3: the query, `Content-Range`, the partial, its lock, removal, lapse, `Xcode.List.uploads`,
`crates/hostkeeper-coordinator/src/xcode_put_range.rs` and `xcode_partial.rs`); and the console's Xcode page (X5,
`ui/pages/xcode.js`). It changes how an operator brings a new Xcode into the store of
[maintenance-updates.md](maintenance-updates.md) section 2.0 (Built: `hk xcode-push`, `crates/hostkeeper-hk/src/xcode_push/`;
the store and its route, `crates/hostkeeper-coordinator/src/xcode_put.rs` and `xcode_store.rs`), and how the Mac proof's
operator-side steps are run ([mac-proof.md](../operations/mac-proof.md) MP-X1 to MP-X6). Facts about this repository were read on
`main` at the commit this document lands on. **Built** marks what exists (with its path), **New** what this design adds, **[V]** a
claim to check before building on it.

The owner's direction (2026-11-25), after downloading an Xcode archive by hand: there should be a simple command for this, or a
button in the UI; expecting people to run a list of steps by hand is a poor experience. Apple lets only a signed-in developer
account download the `.xip`, so the download happens on an operator's own Mac, never on a farm Mac or the coordinator
(maintenance-updates.md section 2.1). That does not change.

## 1. What changes for the operator

**Before.** To add Xcode 27.2, an operator, on their own Mac:

1. downloads `Xcode_27.2.xip` in a browser, or has `hk xcode-push 27.2` download it with `xcodes`;
2. types `hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> xcode-push ~/Downloads/Xcode_27.2.xip`,
   all three flags every time, since `hk` has no settings file;
3. waits with no progress shown while `hk` checks the signature, hashes the archive, expands it (minutes, about an installed
   Xcode's free space) and uploads 10 GB or more in one request. If the upload is cut off, the store keeps nothing, so the next
   try starts from the first byte and expands the archive again;
4. reads a JSON line, then works out from `hk xcode list`, `hk hosts list` and the group policies which Macs will install it.
   They install it only once someone schedules a run;
5. for the Mac proof, runs MP-X1 to MP-X6 line by line (about forty shell lines) and copies the outputs into a record by hand.

A second try of a release the store already holds expands the archive again and then exits 2 with `refused: already_held`.

**After.**

```
$ hk xcode add
hk: ~/Downloads/Xcode_27.2.xip: the newest Xcode archive in ~/Downloads
hk: sha256 3f1c…e09a, 11.92 GB; the store holds no archive of these bytes
hk: Apple's signature: signed Apple Software, its chain ending at Apple Root CA
hk: Xcode 27.2 (27B52), read from the expanded app; record kept in ~/.hostkeeper/xcode/3f1c…e09a.json
hk: the coordinator lists 27.2 (27B52) as a release, and will take it
hk: uploading 4.10 of 11.92 GB (34%)
…
stored: Xcode 27.2 (27B52), sha256 3f1c…e09a, 11920523904 bytes
Xcode 27.2 is now due on 3 Macs:
  mac-build (xcode_keep 2, batch 1): mac-01, mac-02, mac-03
    no run takes it yet. Press Update on the Maintenance page, or:
    hk maintenance schedule --host-group mac-build --kinds xcode --reason "Xcode 27.2" --confirm true
  left out: mac-04 (group mac-proof keeps no Xcode)
```

- **One command, no flags.** `hk xcode add` with no argument takes the newest `Xcode_*.xip` in `~/Downloads`; `hk xcode add 27.2`
  downloads that release with `xcodes` when no archive of it is in `~/Downloads`; `hk xcode add <path>` takes that file. The
  coordinator's address, CA and token file come from a settings file written once (section 2.1), so the flags are needed only to
  override it.
- **Progress, and resume.** The upload shows its progress. A cut-off upload continues from the last byte the coordinator
  holds, by itself up to five times and on the next run after that, with no new expansion (sections 2.4 and 3).
- **Safe to run again.** It hashes the archive first and asks the store. An archive the store already holds ends at once with exit
  0 and the same summary of Macs. A release whose checks already passed reuses the record and skips the expansion (section 2.3).
- **Which Macs, and when.** It ends with the Macs that now have it due, by group, as the coordinator's own Schedule preview
  answers it (`Maintenance.Schedule` without `confirm` stores nothing). It also says whether a run will take it or which line
  starts one (section 2.5).
- **The console** gains an **Xcode** page in the Manage menu: the store, every Mac's Xcodes and what is due, and **Add Xcode**, which
  shows the one command to copy, made for the release waiting (section 4).
- **The proof's operator steps** become one command, `hk xcode verify <archive>`, which runs every check of MP-X1 to MP-X6 and
  writes the record the proof needs (section 5).

**What does not change.** The Apple Account and its two-factor sign-in stay on the operator's Mac, in `xcodes` and the Keychain.
hostkeeper never holds them. The store's archives, their records, its prune and its refusals stay as they are; the store's
directory gains only the partial-upload files of section 3. The checks a farm Mac makes when it stages an archive (size,
SHA-256, `xip --expand`, build, `codesign`) stay too. So do the `GET` route the Macs read from, the maintenance runs and the
rule that nothing installs Xcode until an operator starts a run. `hk xcode list` also prints the uploads in progress, and
`hk xcode remove --sha256` also removes a partial upload of that SHA-256 (section 3); neither changes otherwise.

## 2. `hk xcode add`

New, in `crates/hostkeeper-hk`. It replaces `hk xcode-push` (section 2.7). Like `xcode-push`, it is a verb of the program, not a
gateway call. It runs the Mac's own programs through the Built `Mac` value (`xcode_push/mod.rs`, today `xcodes`, `pkgutil`,
`xip` and `plutil`). That value gains `codesign`, `spctl`, `xar`, `sw_vers`, `uname`, `cp` and the optional `archiveutil`, `aa`
and `aea`, each by its Apple path in `Mac::system`, and tests replace each with the Built fake tool. Its gateway calls
(`Xcode.List`, `Hosts.List`, `HostGroups.List`, `Maintenance.List`, `Maintenance.Get`, and `Maintenance.Schedule` without
`confirm`) go through the Built transport. It runs on macOS only, as today.

```
hk xcode add [<version> | <path to .xip>]
```

### 2.1 The settings file: no flags to type (New)

`hk` reads `$HOME/.hostkeeper/hk.toml` when it exists, for every verb, not only this one:

```toml
endpoint = "https://<private-listener>"
ca_file = "<absolute path of ca.pem>"
token_file = "<absolute path of the token file>"
```

- Each key is the default of the global flag of the same name (`--endpoint`, `--ca-file`, `--token-file`). A flag given on the
  command line wins. A key the file does not set leaves the flag required as today. The file holds paths and an address, never a
  token.
- **It must be the user's own and not writable by group or others, and so must `~/.hostkeeper`.** A file someone else could
  change, or a directory where someone else could put another file in its place, could point the token at another endpoint.
  `hk` refuses such a file or directory, and a link, naming the path. These checks of owner, link and directory are new code
  beside the Built mode check of a token file (`call::private`).
- **The directories are made private.** Any verb that writes under `~/.hostkeeper` (`settings init`, `xcode add`,
  `xcode verify`) makes `~/.hostkeeper` and `~/.hostkeeper/xcode/` with mode 0700 when they are missing, through one helper
  that X1 lands and X2 and X3 reuse, whatever the umask. An existing directory is checked as above and never has its mode
  changed. Every file `hk` writes there (the settings, the records, the lock) is made with mode 0600.
- **An unknown key is ignored**, with `hk: <path>: unknown key <name> ignored` on stderr. A later `hk` may add a key, and an
  older `hk` on the same Mac must still read the file (AGENTS.md B8); the warning keeps a misspelled key from passing unseen.
- `hk settings init --endpoint … --ca-file … --token-file …` writes it (mode 0600, refusing to overwrite an existing file). It
  checks the three values the way a call does, then makes one `Xcode.List` call to show they work.
- Configuration stays in flags and a file, never in an environment variable (AGENTS.md S2).

This is useful beyond Xcode. `ProxyCommand hk ssh-proxy %h` becomes possible too. It lands as its own pull request (section 8,
X1).

### 2.2 Which archive

**The name an archive is found by.** `hk` picks a file up on its own only when its name is `Xcode`, then `_` or `-`, then a
version (decimal numbers joined by dots), then `.xip`, and nothing else: `Xcode_27.2.xip`, `Xcode-26.1.1.xip`. A name with
anything more (`Xcode_27.3_Release_Candidate.xip`, `Xcode-27.2.0+27B52.xip`, a beta) is never picked up on its own, only when
given as a path. When names are compared with a version, trailing `.0` parts do not count (`27.2` matches `Xcode_27.2.0.xip`).
The name is only a hint: the version read from the expanded app (2.3 step 5) decides.

In order, the first that applies:

1. **A path given**: that file, whatever its name, as today.
2. **A version given** (`27.2`): a file named for it in `~/Downloads` or in `~/Downloads/hk-xcode-push/<version>/` (where
   `xcode-push` downloads), the newest by modification time when there are several. With none,
   `xcodes download <version> --directory ~/Downloads/hk-xcode-push/<version>`, as today. A missing `xcodes` is said with the
   two ways out: install it, or download the archive in a browser from Apple's developer downloads page and run `hk xcode add`
   again (exit 1).
3. **Nothing given**: the file directly in `~/Downloads` named for the highest version (numbers compared part by part), then
   the newest by modification time. When there is none, `hk` reads `Xcode.List`. If a release is waiting
   (`xcode_release_waiting`), it goes on as if that version were given (step 2). If none is waiting, it says `the store holds
   the newest release (27.2, 27B52); nothing to add` and exits 0. When the file it took turns out to be held already (2.3 step 3)
   and a newer release is waiting, the summary ends with `Xcode 27.3 is waiting: hk xcode add 27.3`.

**One run at a time.** `hk` holds an exclusive lock, `~/.hostkeeper/xcode/.lock`, from the first step to the last, since two runs
on one Mac would share the expansion directory and the records. The lock is an exclusive `flock(2)` on that file, which the system lets go when the run's process ends however it ends, so there is
no stale lock and no process id to be taken for another program's; the file itself stays. The run that holds it writes its process
id into the file, which is only what a second run names when it exits 1, and the second never waits.

### 2.3 The checks, and the record that saves the expansion

In this order. A step that fails uploads nothing and names what failed, as `xcode-push` does today:

1. **The archive's kind**: its first four bytes. `xar!` (a `.xip`) goes on. `AA01` (Apple Archive) and `AEA1` (Apple Encrypted
   Archive) stop with `archive_kind_unsupported: <kind>`, so a format the farm Macs would defer (`xcode_archive_encrypted`) is
   never stored. Any other first bytes stop with `not_an_xip`. This is MP-X6's question asked of every archive.
2. **SHA-256 and size**, hashed once, read in 8 MiB blocks, with progress on a terminal ("hashing 6.2 of 11.9 GB").
3. **Ask the store** (`Xcode.List`). If it holds an archive of this SHA-256: when a version was given and the held archive is
   another version, `hk` stops with `version_mismatch: asked 27.3, these bytes are Xcode 27.2 (27B52) in the store`. Otherwise
   it prints `already in the store: Xcode 27.2 (27B52)`, goes to section 2.5's summary and exits 0. Nothing else runs. This is
   what makes a second run cheap.
4. **Apple's signature**: `pkgutil --check-signature`, the Built verdict (status `signed Apple Software`, chain ending at Apple Root
   CA by its SHA-256 fingerprint, `signature.rs`). It runs on every run, since it takes seconds and the file may have changed
   since its record.
5. **Version and build.** If `~/.hostkeeper/xcode/<sha256>.json` exists, is the user's own with mode 0600, names this SHA-256
   and size, has `verdict` `pass` and holds a version and a build, they are used and the expansion is skipped. Any other record
   is ignored and replaced after a fresh expansion. Before expanding, `hk` checks the free space: the archive's volume must
   have three times the archive's size free, the margin the farm Mac's stage uses (Built: `stage.rs`); **[V]** MP-X5 measures
   the real need. Short of that, it stops with `free_space_short`, naming both numbers. Then it expands the archive as today
   (`xip --expand` beside the archive, one `.app`, `plutil` for `CFBundleShortVersionString` and `ProductBuildVersion`), runs
   `codesign --verify --deep --strict` on the app (New here; the farm Mac already does), removes the expansion and writes its
   record (section 5.2). Given a version, the archive's must be it (`version_mismatch`).
6. **The same release from other bytes.** With the version and build known, `hk` compares them with `Xcode.List` again. If the
   store holds this release from other bytes, it stops: `the store holds Xcode 27.2 (27B52) from other bytes (sha256 …); if
   these are the ones to keep, hk xcode remove --sha256 … --reason … first`. If it holds another build of this version, it
   stops naming the build held and the same `hk xcode remove` line (the store would refuse `xcode_version_taken`).
7. **Ask the coordinator before the body**: the upload's `Expect: 100-continue` (Built) brings any refusal of the store
   (`not_listed`, `not_kept`, `already_held`, `xcode_version_taken`, `upload_in_progress`, `unauthorized`) back before a byte is
   sent, each said in words with what to do.

The record is a cache of a check this program made itself, under the user's own home and keyed by the bytes' SHA-256, which
step 2 has just recomputed. Trusting it does not trust the file name or anyone else's file.

### 2.4 The upload: progress and resume

- **Progress**: on a terminal, one line rewritten in place ("uploading 4.10 of 11.92 GB (34%), 85 MB/s"). Otherwise one line per
  tenth of the archive, so a log stays short. Progress goes to stderr. Stdout keeps the one answer line (`stored: …`) and the
  summary.
- **Resume** (needs the coordinator change of section 3): before the body, `hk` asks the coordinator how much of this SHA-256 it
  already holds (`GET /v1/xcode/<sha256>/upload`) and sends from there with `Content-Range`. A broken connection, an
  `offset_mismatch` (whose answer carries the right offset) or a `partial_locked` (another writer's lock not yet stale) is tried
  again from the offset the coordinator then reports, five times in all, each after a wait that doubles from 2 seconds. Then
  `hk` stops with `the upload stopped at 7.31 of 11.92 GB; run hk xcode add again to continue` (exit 1). The coordinator keeps
  the partial upload for 24 hours (section 3), so the next run, even from another terminal or another operator's Mac with the
  same archive, continues it. A partial that holds every byte (the coordinator stopped after the last block and before the
  commit) is finished by sending the archive's last byte again, `Content-Range: bytes <size-1>-<size-1>/<size>`; the coordinator
  then hashes what it holds and commits it (section 3). The waits come from an injected clock in tests (T10).
- **An older coordinator** has no such query. Its `/v1/xcode/` route on the private listener takes `PUT` alone and answers the
  `GET` `405 method_not_allowed` (Built: `xcode_put.rs`, the method is its first check). On a 405, `hk` sends the whole archive in
  one request, as `xcode-push` does today, and says that this coordinator cannot resume. A 404 means the coordinator has no
  Xcode store (no `--xcode-dir`), and `hk` stops saying so.
- The token stays in the request's `Authorization` header alone, never an argument, an environment variable of a program `hk`
  runs, or an output line (Built rule, `xcode_push/mod.rs`).

### 2.5 Which Macs will install it, and when

After the store answers `201` (or 2.3 step 3 found the archive held), `hk` reads every page of `Hosts.List`, `HostGroups.List`
(each group's `xcode_keep`, `xcode_default` and `batch`; a host's group is its `host_group`), and the open runs
(`Maintenance.List`, then `Maintenance.Get` of each open run, which answers its hosts and each host's kinds). Then, for each Mac
(a host of platform `darwin-arm64` with a maintenance report):

1. **Will it install this release?** Yes when its group keeps this release (it is among the group's newest `xcode_keep` releases
   the store holds) and the Mac does not have it installed in `Xcode-<version>.app` with this build. The coordinator's due has
   a second half (Built: `maintenance_xcode.rs`, `plan`, `missing.or(unselected)`): a Mac that has it installed is still due
   when this release is the group's default (`xcode_default`, `latest` naming the newest kept) and `xcode-select` names
   another; `hk` says `installed, not selected: due to be selected`. The Mac's `xcode` due item names only one release, the
   newest it lacks, so after adding an older release while a newer one is held, the item names the newer, and `hk` says
   `due: 27.3, then 27.2 in the same turn`. A Mac of a group whose `xcode_keep` is 0, or that keeps only newer releases, is
   `left out` with that reason. A Mac that has it installed and selected as the group wants is `installed`.
2. **When?** A Mac in an open run whose host entry has `xcode` among its kinds and whose turn has not started gets the release in
   that turn, because the coordinator reads the store when the turn starts (Built: `service_maintain.rs`, the store read before
   `decide_maintenance::decide`): `in run <id>, window <start> to <end>` or `in run <id>, now`. A Mac in an open run without
   `xcode` among its kinds does not: a run's kinds per host are fixed when it is scheduled (Built: `maintenance_service.rs`,
   `turn_of`). `hk` says `in run <id>, which does not take Xcode: schedule after it ends`, and prints no schedule line for that
   Mac.
3. **The run to start.** For each group with Macs from step 1 that no run takes, `Maintenance.Schedule` with `kinds = [xcode]`,
   the group, the reason `Xcode <version>` and no `confirm`. It stores nothing and orders no host (Built). It answers the hosts
   the run would take and those it would skip with the coordinator's reason. `hk` prints the hosts taken, the group's batch, the
   hosts skipped with their reasons, and the exact command that stores the run (the same request with `--confirm true`). A
   refusal of the whole run is printed with the coordinator's text and, for `last_host`, the flag that answers it
   (`--take-last-host true`).

So "when" is: in the run that already takes the Mac's Xcode, or when an operator stores the run printed. Nothing installs on its
own, as the maintenance design rules (an install drains a build Mac). `hk xcode add` does not store the run itself; section 9
asks the owner whether a flag should.

**A summary that cannot be made does not undo the upload.** If any call of the summary fails, `hk` prints `summary unavailable:
<why>; the archive is stored` on stderr, and the `hk hosts list` and `hk maintenance list` lines to read it by hand, and still
exits 0.

### 2.6 Exit codes and refusals

| Exit | When |
|---|---|
| 0 | stored; or the store already holds these bytes; or nothing given and nothing to add |
| 1 | a check failed (`archive_kind_unsupported`, `version_mismatch`, `free_space_short`, the signature, the expansion, `codesign`, the same release from other bytes, another build of the version), a program could not run, another run holds the lock, the upload stopped, the coordinator has no store |
| 2 | usage (`not_an_xip` among it: the given file is no archive), or a refusal of the store: `not_listed`, `not_kept`, `already_held`, `xcode_version_taken`, `upload_in_progress`, `partial_differs`, `too_large`, `digest_mismatch` |
| 3 | `unauthorized` or `forbidden` |

These are the Built codes of `xcode-push` and `hk`, with one change: the same bytes already held is now 0, because the operator
asked for the store to hold this archive, and it does. `already_held` from the store (another archive of this release) can now
arrive only in a race, since step 6 found none before the upload. It stays 2. `offset_mismatch` and `partial_locked` are handled
inside `hk` (2.4) and never end a run by themselves.

### 2.7 `hk xcode-push`

`hk xcode-push <version> | <path>` keeps working for one minor release as a second name of `hk xcode add` with the same
argument. It writes one line to stderr, `hk xcode-push is now hk xcode add`, and is then removed. It does not stay as a second
way to do the same thing. The same pull request (T19) changes everything that names `xcode-push`: its help golden,
`docs/cli.md`, `docs/operations/xcode-releases.md`, `docs/operations/maintenance.md`, `docs/gateway.md`, the alert text of
`xcode_release_waiting`, the maintenance design's section 2.0, and the check of runbook commands in
`crates/xtask/tests/enrollment_doc/`. The last of these learns `xcode add` in X3 and drops `xcode-push` in X7.

`hk xcode add` and `hk xcode verify` are hand-written methods of the generated `xcode` service in `hk`'s dispatch (Built:
`dispatch.rs`, which today looks only the generated methods up). `hk xcode --help` and the generated `docs/cli.md` list them
beside `list` and `remove`, so the doc generator (`doc.rs`) learns the hand-written methods of a service. That change lands
with the first of them, `verify` (X2).

## 3. The coordinator: an upload that resumes (New)

Today an upload is `.upload-<sha256>-<token>-<n>` and is deleted whenever its request ends without being stored (Built:
`xcode_put.rs`, the `Partial` guard). The change is additive. A request without `Content-Range` behaves exactly as now, and
today's tests of the route stay as they are.

- **`GET /v1/xcode/<sha256>/upload`** on the private listener, operator bearer only (the same checks as the `PUT`: no client
  certificate, no CI token): `{"state": "held" | "partial" | "none", "received": <bytes>, "size": <bytes or 0>}`.
- **`PUT /v1/xcode/<sha256>?version=…&build=…` with `Content-Range: bytes <first>-<last>/<size>`**: every check of today runs
  first, in today's order. `<size>` is the size those checks use (`too_large` against `--xcode-max-bytes`), and
  `Content-Length` must equal `<last> - <first> + 1` (else `400 bad_request`). Then:
  - the partial upload of this SHA-256 is one file, `.partial-<sha256>` (0600), with a record beside it,
    `.partial-<sha256>.json`, of the version, build, size and the operator who started it;
  - `<first>` must equal the bytes the partial already holds (0 for a new one), else `409 {"error": "offset_mismatch",
    "received": <n>}`. A size, version or build other than the partial's is `409 partial_differs`;
  - the bytes are appended and synced. A part accepted while bytes are still due answers `202 {"received": <n>}`. A request cut
    off keeps what it wrote, synced, and has nobody to answer;
  - when the last byte is in, the whole file is hashed again in one read (the hash of a prefix written by an earlier request is
    not kept across requests, so nothing trusts a state it did not compute). It must equal the path's SHA-256, else
    `400 digest_mismatch` and the partial is removed. Then the Built commit runs as today: the checks again under the store's
    lock, the `xcode_put` audit entry first, the record, the rename, the prune, and `201` as today.
- **Who may continue a partial.** Any operator: the bytes are named by their SHA-256 and checked whole before anything is
  stored, so a partial written by someone else cannot make the store hold other bytes than the path names. The worst a
  wrong partial does is fail the final hash, which removes it. The audit entry names the operator whose request completed it,
  and the partial's record names who started it.
- **One writer.** Today the reservation of a version is held in memory (Built: `store.reserve`). With several coordinators on
  one directory, a resumed request may reach another instance. So the partial's writer holds an exclusive lock file,
  `.partial-<sha256>.lock`, made with `O_CREAT|O_EXCL`, holding the instance's token and refreshed with every block written.
  A second writer gets `409 partial_locked`, a code of its own so that `upload_in_progress` keeps its one Built meaning. A
  lock not refreshed for 30 seconds (three times the listener's `CLIENT_PATIENCE` of 10 seconds, Built: `listen.rs`, past
  which a stalled body is already cut off) is stale and is taken over. **[V]** That `O_EXCL` is atomic on the shared filesystem
  the deployment mounts. The pull request that builds it (X4) names the filesystem and the test. If it is not atomic there,
  this design comes back to the owner before X4 lands.
- **Removing a partial.** `Xcode.Remove` (`hk xcode remove --sha256 … --reason …`) of a SHA-256 the store holds only as a
  partial removes the partial, audited `xcode_partial_remove` with the operator's reason (New audit action). It is refused
  `partial_locked` while the lock is fresh.
- **Lapse.** A partial not written for 24 hours is removed by the daily `xcode-feed` job and at start (Built: the start's sweep
  of `.upload-` files older than 24 hours, extended to `.partial-`), each removal logged. A lapse is not audited, since a
  partial was never stored.
- `Xcode.List` gains `repeated XcodeUpload uploads` (New, additive: SHA-256, version, build, received, size, who started it,
  last write). The console shows an upload in progress, and `hk xcode list` prints it.
- `XcodeRefusal` gains `partial_locked` (New, additive) for `Remove`. The `PUT` and query codes are listed in
  `docs/operations/xcode-releases.md` in the same pull request.

## 4. The console's Xcode page (New)

`ui/pages/xcode.js`, in the Manage menu beside Maintenance. It calls `Xcode.List` and `Xcode.Remove` (Built and served
to a signed-in session by the gateway, `docs/gateway.md`; not yet in `ui/calls.json`), `Hosts.List` (every page),
`HostGroups.List` (each group's `xcode_keep` and `xcode_default`) and `Maintenance.List` (the link to a Mac's run). It reads them
when it opens and again every 60 seconds through `ctx.every`, like the Maintenance page.

- **Waiting.** When `xcode_release_waiting` stands: "Xcode 27.3 (27C21) is out and not in the store; it needs macOS 27.1" and
  the **Add Xcode** button.
- **Uploads.** Each partial upload: who started it, how far ("7.31 of 11.92 GB, last write 2 minutes ago").
- **The store.** Each archive: version, build, size, who pushed it, when, how many Macs have it installed. **Remove** takes a
  reason, as `hk xcode remove` does, and says first which Macs still have it due.
- **The Macs.** One row per Mac: its group, its group's `xcode_keep` and `xcode_default`, each Xcode installed (version, build,
  selected, foreign), what is due by the rule of 2.5 step 1, and its device-management state (section 6.2). A Mac whose turn
  is in a run links the run on the Maintenance page. The Update and Schedule buttons stay on the Maintenance page, so there is
  one place that starts runs.

### 4.1 What **Add Xcode** does: it shows the command to copy

The browser cannot download from Apple for the operator, so the button has to hand the work to the operator's Mac. The three
candidates:

| | What it needs | Why chosen or not |
|---|---|---|
| **A command to copy** | `hk` installed and its settings file (2.1) | **Chosen.** All the checking stays in `hk` on the Mac, where Apple's tools are. Nothing new is trusted. Nothing new is served. It works from any browser, even when the browser is not on the operator's Mac. |
| A link of a custom `hk:` scheme (`xcode/add?version=…`) | an app bundle that registers the scheme with Launch Services | Not chosen. `hk` is a bare binary in a tarball (Built: `docs/design/hk.md` section 5) and nothing registers a scheme, so this needs a new signed and notarized `.app` in every release. Any web page could then send the operator's `hk` a link that starts a multi-gigabyte download and an upload with their token. It saves one paste. |
| A drop zone that streams the `.xip` to the store | the `PUT` route taking a browser session, and the coordinator judging the archive | Not chosen. The Linux coordinator cannot run `pkgutil` or `xip` (maintenance-updates.md 2.1). The version and build would come from the browser, which means trusting the file name, or the coordinator would need a new parser of Apple's archive signature on the fleet's trust root. A browser upload of 12 GB has no resume across a closed tab. The `PUT` route would have to accept the session cookie and its CSRF check on a stream (Built: it takes a bearer token alone). |

The button opens a panel:

```
Add Xcode 27.3 (27C21)

On the Mac signed in to the Apple developer account, in Terminal:

    hk xcode add 27.3                                   [Copy]

It downloads 27.3 with xcodes (or takes ~/Downloads/Xcode_27.3.xip), checks Apple's
signature and the build, and uploads it here. This page shows the upload as it arrives.

First time on this Mac: install hk (docs/deploy/verify-a-release.md), then
    hk settings init --endpoint <this console's address> --ca-file <ca.pem> --token-file <token-file>   [Copy]
```

- The first line is made from the waiting release. With none waiting, the panel shows `hk xcode add` (the newest archive in
  `~/Downloads`) and says the store holds the newest release.
- The address in the `settings init` line is the page's own origin, read at run time as `ctx.origin`, a string the entry module
  (`main.js`) gives pages from `location.origin`, as it gives them the clock and the local offset, and page tests give a fake. So no
  address is written in the app's sources (the UI scan refuses literal `https://` addresses, Built: `scan.rs`).
- **Copy** writes the line with the Clipboard API through `ctx.copy`, a function the entry module gives pages, as it gives
  `fetch`, so the page tests use a fake. Where the clipboard is refused (a page not in a secure context), the line is selected
  for a manual copy and the panel says so.
- While the panel is open, the page reads `Xcode.List` every 10 seconds (`ctx.every`). It shows the upload's progress from
  `uploads`, then "Stored. Due on 3 Macs of mac-build", with a link to the Maintenance page.

## 5. The proof's operator steps: `hk xcode verify` (New)

### 5.1 One command for MP-X1 to MP-X6

```
hk xcode verify ~/Downloads/Xcode_27.2.xip
```

It runs on the operator's Mac as the operator's user. It reads the archive only, makes nothing outside a scratch directory
beside the archive (removed after) and `~/.hostkeeper/xcode/`, and calls no coordinator. It fetches one file: Apple's published
root certificate, over HTTPS from Apple's certificate authority page, unless `--apple-root <file.cer>` gives it. Before each
expansion it checks the free space as `hk xcode add` does (`free_space_short`). Each runbook step becomes one part of it:

| Step | What it settled | `hk xcode verify` records |
|---|---|---|
| MP-X1 | Apple's three programs are where `Mac::system` says, `xcodes` optional, refusal off macOS | `sw_vers` and `uname -sm`; for each program `Mac::system` names, whether its path is a file (a missing one fails, named); `xcodes version` when present; the first lines of `xip --help`. Off macOS it refuses, as the Built test `off_macos_it_refuses_with_a_clear_message` asserts |
| MP-X2 | the signature verdict on a real archive | `pkgutil --check-signature`'s exit, stdout and stderr apart, the status, every chain entry in order and the last fingerprint; the SHA-256 fingerprint and subject of Apple's published root, read by `hk` from the certificate, beside the code's constant, and whether they are equal |
| MP-X3 | a damaged archive fails | a copy-on-write clone of the archive beside it (`/bin/cp -c`; on APFS no data is copied) with one byte changed at its middle, and a 16-byte file that is no archive: for each, `pkgutil`'s exit and output and `xip --expand`'s exit and output. When `/Library/Developer/CommandLineTools/Packages/*.pkg` exists, `pkgutil`'s output on it (a package that is not `signed Apple Software`). A volume that cannot clone gives `tamper: not run, the volume cannot clone`, never a silent skip |
| MP-X4 | whether the version can be read without expanding | `xip --list`'s exit and output; `xar -tf`; the whole table of contents (`xar --dump-toc`); the `Metadata` member extracted (`xar -xf … Metadata`), its kind (`file`) and its text as a property list or raw; whether any member name or text names a version or build. For the record only: the expanded app stays the authority |
| MP-X5 | the expansion and the version read | free space before and after; the archive's size; `xip --expand`'s exit and everything it printed; the seconds taken (the program's own clock, for the record, never a check); the number of `.app` entries and their names; the `Contents/` listing; whether `Contents/version.plist` exists; each `plutil`'s exit and value; `codesign --verify --deep --strict` and `spctl --assess` |
| MP-X6 | the archive's format | the first four bytes and the kind (`xar!`, `AA01`, `AEA1`); which of `archiveutil`, `aa` and `aea` exist |

**The verdict.** `pass` when: the kind is `xar!`; the signature passes the Built verdict and the root's published fingerprint
equals the code's constant; `xip --expand` refuses the damaged clone and the non-archive (`pkgutil`'s answer on them is recorded
and not required, since MP-X3 allows that `pkgutil` might accept a damaged archive and leave the check to `xip`); and the
expansion gives one app with a version and a build and `codesign` passes. A volume that cannot clone still gives `pass`, with
`tamper: not run`. Otherwise `fail: <the first that failed>`. It exits 0 on `pass` and 1 otherwise, and writes the record in
both cases.

### 5.2 The record

`~/.hostkeeper/xcode/<sha256>.json` (mode 0600), and the same JSON on stdout:

```
{"record": "hk-xcode-verify-v1", "hk": "<hk version>", "macos": {…}, "programs": {…},
 "archive": {"name", "size", "sha256", "kind"}, "signature": {"exit", "stdout", "stderr", "status", "chain", "apple_root"},
 "tamper": {…}, "toc": {…},
 "expansion": {"free_before", "free_after", "exit", "output", "seconds", "apps", "contents", "version", "build", "codesign", "spctl"},
 "verdict": "pass" | "fail: <what>"}
```

`hk xcode add` writes a record of its own kind at the same path, `{"record": "hk-xcode-add-v1", "hk", "archive", "signature":
{"status"}, "expansion": {"version", "build", "codesign"}, "verdict"}`, with only what it computed. `hk xcode verify` replaces
any record. `hk xcode add` never replaces a `verify` record that passed. A reader ignores keys it does not know, so either
record can grow additively. `hk xcode add` reads only `archive`, `verdict` and the `version` and `build` of `expansion`, and
uses a record only when `verdict` is `pass` (2.3 step 5).

The proof's results (maintenance-updates.md section 10) cite the record. Its whole JSON goes in the pull request that records
them, with the archive's name, never the archive. `mac-proof.md` section 2 shrinks to "run `hk xcode verify` on the newest
archive and on one older release, and attach both records". Its "Decides" lines become the code's own behaviour and its
tests: a different status wording, root name or fingerprint layout fails the verdict, by fixture.

### 5.3 What stays a test, and what a person still runs once

Everything `hk xcode add` and `hk xcode verify` decide is tested in this repository over fake programs (section 7), on Linux, in
the PR check. The one thing a test here cannot do is run Apple's programs on a real Xcode archive. Apple's licence does not let
the archive be stored outside our private store or fetched without the developer account's sign-in, and the per-PR checks run on
Linux alone (AGENTS.md B9). So `hk xcode verify` on a real archive is run by a person, once per new Xcode format or macOS major
release, as one command whose output is the record. When the real output differs from a fixture (a new `pkgutil` layout), the
record's text becomes the new fixture in the same pull request as the code that reads it.

## 6. The other steps that need the owner today

### 6.1 The update account's password: handed over once (New)

Today it reaches a Mac in the bootstrap's settings (Built: the Blueprint's managed preferences or `bootstrap.toml`, four keys,
`crates/hostkeeper-bootstrap/src/update_account.rs`), or, on a Mac enrolled before the keys, through a root shell and a
`sudo sh -c` line per Mac (`docs/operations/maintenance.md`, "A Mac enrolled before the keys").

**After:** the operator hands it to the coordinator once, and every Mac that lacks the file gets it.

- `hk mac update-account set --name hkupdate --reason <text>` asks for the password twice on the terminal with no echo, never
  as an argument. It calls `MacAccounts.SetUpdateAccount`, a new operator service (operators only; a CI token is refused),
  audited `update_account_set` with the operator's reason and never the password.
- **Stored**: in the coordinator's database, one row: the name, the password, a generation number and who set it. No API
  answers the password: `MacAccounts.Get` answers the name, the generation, who set it and when. The owner ruled that the
  password is not treated as a secret (maintenance-updates.md 5.5), so it is stored without a vault. It is kept out of every
  log, audit entry, error and answer, as the daemon already keeps it (each asserted by exact equality in the tests).
- **Delivered**: a Mac's daemon announces `update-account-v1` and reports `update_account.file_present` (New, additive
  `MacUpdateFacts` field). To a Mac that reports the file absent, the coordinator sends `HeartbeatResponse.update_account`
  (New, optional) over the heartbeat's mutual TLS. The daemon writes the Built file (root, 0600, through the Built checked
  writer) and makes the account at once, as at a start (Built: `update_account_make.rs`). An existing file is never replaced,
  as the bootstrap never replaces it. So the bootstrap's keys and the hand-written file still win, and changing the password
  stays the hand step it is today (out of this design).
- **What still needs a person, once per Blueprint**: the Secure Token is granted only by a holder of one, the administrator
  made at Setup Assistant (maintenance-updates.md 5.5). Its name and password stay in the Blueprint's managed preferences
  (`admin_name`, `admin_password`), never in the coordinator: that password is the Mac's own administrator's, and the
  coordinator holding every Mac's administrator password would be a larger change than this design should make.
- **Threat model.** Whoever reads the coordinator's database learns the update account's password, which is root on every Mac
  that uses it. The database already holds what lets an operator order maintenance on every host. Still, this is a new reach
  (the password works at a Mac's login, beyond hostkeeper's authorisation), and it is the owner's call (section 9, question 2).
  The alternative, which needs no change: keep the four keys in the Blueprint, and write the file by hand on the Macs enrolled
  before them.

### 6.2 Apple Business settings: checked from the Macs' own facts (Built, shown in one place)

The daemon already reads every setting of maintenance-updates.md 5.8 from the Mac (`/Library/Managed Preferences` and the
software-update declaration state, Built: `maintenance_mdm.rs`) and reports each it finds as `mdm_conflicts`. The coordinator
leaves such a Mac out of `macos` (Built: `maintenance_mac.rs`). The host page's Updates tab already shows them
("Device-management conflicts", Built: `ui/updates.js`). What changes:

- The Xcode page's Mac table shows the same state for every Mac in one column, "nothing that blocks updates" or the keys found.
  So the check MP-E9 step 5 asks the owner to make in Apple Business by hand becomes a read of the Macs' facts in the console.
- The proof's MP-M2 to MP-M7 still need the owner to set each setting in Apple Business once, since its API cannot edit a
  `SOFTWARE_UPDATE` Configuration (maintenance-updates.md 5.5.1). The root-shell reads are replaced by the report: after each
  setting, the operator reads `hk hosts get --node-id <mac>` (`maintenance.mac.mdmConflicts`) at the next look, within an hour,
  or at once after restarting the daemon.
- A file there that exists and cannot be read fails the look (`look_failed` starting `mdm: `), and the Mac keeps its last
  report; it never reads as a clean Mac (Built). Whether the declaration state needs Full Disk Access is still to be shown
  (MP-M6); on the farm Macs, read without it, the looks succeed (maintenance-updates.md 10.1.1).
- Not chosen: reading Apple Business's API from the coordinator. It needs a new key on the coordinator, and it shows what a
  Blueprint asks for, not what reached the Mac. The Mac's facts are the truth the run depends on.

### 6.3 What stays human, and why

| Step | Why a program cannot do it |
|---|---|
| The Apple Account's two-factor code, when Apple has ended `xcodes`'s session | Apple offers programs no way to complete it. The code goes to a trusted device for a person (maintenance-updates.md 2.0). At most once per release, at the operator's own terminal. |
| FileVault's pre-boot password, after any restart hostkeeper did not make (a power cut, a panic), and turning FileVault on or off | The volume's key is unlocked only by a person typing a FileVault user's password at the Mac, by design. hostkeeper's own restarts can be authenticated (Built: `filevault.rs`), and others cannot. The ruling stays: FileVault off on build Macs. |
| Choosing to make a major macOS upgrade, and which Mac takes it first | It cannot be undone. The owner sets `macos_major` on a group, a group of one first. Once chosen, the run is the same one button. |
| Changing a setting in Apple Business (5.8), installing the Full Disk Access profile | Apple Business's API cannot edit those Configurations (5.5.1). The Mac's facts only show whether they are right (6.2). |
| The Setup Assistant administrator's password, once per Blueprint | Only a Secure Token holder can grant one (6.1). |
| Watching the screen during the proof's first installs (MP-E9 step 3) | A dialog that waits for a person is exactly what the proof looks for, and no program on the Mac reports it. It is done once, in the proof, not per release. |

## 7. Tests

All in Rust, in the crate of the code, over fake programs (the Built `hk-fake-mac-tool`, `crates/hostkeeper-hk/tests/bin/`), a
fake store and gateway on loopback port 0, scratch directories and an injected clock. None times anything (T10, T12). New tests
go to new files (`tests/cli/xcode_add.rs`, `tests/cli/xcode_verify.rs`, `tests/cli/settings.rs`), since `tests/cli/xcode_push.rs`
is at 800 lines (R12).

- **Finding the archive**: no argument takes the file named for the highest version, then the newest, among `Xcode_*.xip` and
  `Xcode-*.xip`, and never a name with a tag (`_Release_Candidate`, `+build`); a version finds its file in either place, with
  `27.2` matching `27.2.0`, and runs no `xcodes`; with none, it runs `xcodes download` once; with no `xcodes`, it stops naming
  both ways out; nothing given and nothing waiting exits 0 saying so; nothing given and a release waiting downloads that one; a
  second run while one holds the lock exits 1 naming it.
- **The checks**: an `AA01` or `AEA1` archive uploads nothing (`archive_kind_unsupported`) and other first bytes give
  `not_an_xip`; an archive the store holds runs no `pkgutil` and no `xip` and exits 0; the same bytes held as another version
  when a version was given give `version_mismatch`; a record that passed skips `xip`, and a record of another SHA-256, size,
  owner or mode, or with `verdict` `fail`, does not and is replaced; `add` never replaces a `verify` record that passed; short
  free space stops before `xip` (`free_space_short`); `codesign` refusing uploads nothing; the same release from other bytes,
  and another build of the version, stop before the upload naming the `hk xcode remove` line; every refusal of the store is
  said and exits as section 2.6 lists.
- **Resume**: a connection cut at a byte (the fake store closes) continues from the store's `received` with `Content-Range`,
  and the bytes the store ends with equal the archive; `offset_mismatch` and `partial_locked` are retried from the offset
  reported; five failures stop with the offset said and exit 1, and the next run continues; an older store (`405` on the query)
  gets one whole `PUT`; a store answering `404` ends with "no Xcode store"; the waits between tries are read from the injected
  clock.
- **Which Macs**: from a fake gateway's `Hosts.List` (two pages), `HostGroups.List`, `Maintenance.List`, `Maintenance.Get` and
  `Maintenance.Schedule` answers, the summary names: the Macs that will install it, by group; a Mac whose due item names a newer
  release, as `due: 27.3, then 27.2`; a Mac with it installed but not selected while it is the group's default, as due to be
  selected; a Mac in an open run with `xcode` among its kinds, by run; a Mac in an open run without
  `xcode`, with no schedule line; skipped hosts with the coordinator's reason; Macs of an `xcode_keep 0` group as left out; and
  the exact schedule line. `Maintenance.Schedule` is called without `confirm`, asserted on the request. A failing summary call
  still exits 0 after `stored:`, with `summary unavailable` said.
- **The token**: in no argument, environment or output line of any program run, through resume and the summary calls (extends
  the Built test).
- **Settings file**: a flag wins over the file; a key absent from both is a usage error; a file or `~/.hostkeeper` writable by
  group or others, of another owner, or a link is refused naming it; an unknown key is ignored with the warning, its exact text
  asserted; `settings init` refuses to overwrite; under a umask of 000 the first `hk xcode add` (flags only) makes
  `~/.hostkeeper` and `~/.hostkeeper/xcode/` 0700 and its lock 0600.
- **`hk xcode verify`**: each row of section 5.1 over fakes; the verdict's cases (a published root fingerprint unlike the
  constant fails; `pkgutil` accepting the damaged clone while `xip` refuses it passes; `xip` accepting it fails; a volume that
  cannot clone passes with `tamper: not run`); a real `pkgutil` output captured once as a fixture (its text only); the record's
  JSON compared by exact equality; a failing check still writes the record with `verdict` naming it.
- **Coordinator** (`crates/hostkeeper-coordinator`): the upload query answers `none`, `partial` and `held`; a `Content-Range` at
  the wrong offset is `offset_mismatch` with `received`; a `Content-Length` other than the range is `bad_request`; a partial of
  another size, version or build is `partial_differs`; a part with bytes still due answers `202`; two writers of one partial
  (two store instances over one scratch directory) give `partial_locked` to the second; a lock not refreshed for 30 seconds of
  the injected clock is taken over; the last part's whole-file hash must match, else the partial is gone and nothing is stored;
  `Xcode.Remove` of a partial removes it, audited, and is refused while it is locked; a lapsed partial is removed and logged; a
  request without `Content-Range` behaves as the Built tests assert (they stay unchanged); `Xcode.List.uploads` shows a partial;
  a turn of an open run whose kinds for the Mac include `xcode`, starting after an archive is stored, orders it, and a turn
  whose kinds do not include `xcode` does not.
- **Wire**: `additions_xcode_upload.rs` and `additions_update_account.rs`, both ways between the previous release's messages and
  the new ones (the Built pattern of `crates/hostkeeper-proto/tests/additions_prerequisites.rs`).
- **Update account**: the password appears in no log line, audit entry, answer or error (exact equality); `MacAccounts.Get`
  never answers it; it is sent only to a Mac reporting the file absent and announcing `update-account-v1`; the daemon writes
  the file 0600 root and never replaces one; a CI token is refused.
- **Console** (`crates/hostkeeper-ui-tests`): the page's waiting line and **Add Xcode** panel from a fake `Xcode.List`; the copy
  line made from the waiting release and `ctx.origin`; `ctx.copy` called with exactly that line, and the fallback when it is
  refused; the upload's progress, then the stored state; the Mac table's due column by the rule of 2.5 from fake `Hosts.List`
  and `HostGroups.List`; Remove sends the reason; `ui/calls.json` regenerated with `xcode.list`, `xcode.remove`,
  `hostGroups.list` and `maintenance.list` for the page.

## 8. Implementation pull requests

| # | Pull request | After |
|---|---|---|
| X1 | `hk` settings file and `hk settings init` (2.1); `docs/cli.md`, `docs/design/hk.md` | |
| X2 | `hk xcode verify` and its record (5); `dispatch.rs` and `doc.rs` learn hand-written methods of a generated service (2.7); `mac-proof.md` section 2 rewritten to run it | |
| X3 | `hk xcode add`: archive finding, kind check, store-first, record reuse, `codesign`, progress, the Macs summary; `xcode-push` as a second name for one release; every doc and check that names it (2.7) | X1, X2 |
| X4 | Wire and coordinator: the upload query, `Content-Range`, the partial, its lock, removal and lapse, `Xcode.List.uploads`, `partial_locked`; `hk` resume; `docs/operations/xcode-releases.md` (the query, `Content-Range`, the new codes, the lapse) | X3 |
| X5 | Console: the Xcode page, **Add Xcode**, `ctx.copy` and `ctx.origin`, the device-management column | X4 |
| X6a | Update account, wire and coordinator (6.1): `MacAccounts`, `update_account_set`, the heartbeat field, `additions_update_account.rs` | owner's answer to question 2 |
| X6b | Update account, daemon: the fact `file_present`, the write and the making on delivery; `docs/operations/maintenance.md` | X6a |
| X7 | Remove `hk xcode-push`, and its name from the runbook check | one minor release after X3 |

X2 can land first: it needs nothing else, and it lets the proof run now. X1 and X2 go in parallel.

## 9. Questions for the owner

1. **Should `hk xcode add` start the run too?** Proposed: no. It prints the line that stores the run, because an install drains
   build Macs and should be a separate act. Alternative: `--update --reason <text>` stores the run of every due group right after
   the upload (with the groups' batch and drain settings).
2. **The update account's password in the coordinator (6.1).** Proposed: yes, as described, with its threat model. Alternative:
   no change (Blueprint keys, hand-written file on older Macs).
3. **The settings file (2.1)** changes `hk` for every verb, not just Xcode. Proposed: yes, as its own pull request.
