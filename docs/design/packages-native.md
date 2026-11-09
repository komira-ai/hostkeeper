# Native packages only: hostkeeper's own package format retired, hostkeeper included

Status: the retirement of the format is ruled; the design below is for the maintainers' ruling. Nothing here is built. This is section 10 of
[packages-repositories.md](packages-repositories.md), kept in a file of its own so that one stays under 1000 lines
(rule R12). A bare "section 4.7" is that file's; "10.4" is this file's; packages.md's sections are named with it.

## 10. The end state: no package format of hostkeeper's own

As ruled: hostkeeper's own package format ends. Every service hostkeeper deploys, and hostkeeper itself, is installed
from the platform's native package: a `.deb` from an apt repository on Linux, a signed `.pkg` on macOS. This section is
that revision. Where it disagrees with sections 0 to 9 of packages-repositories.md, with [packages.md](packages.md), with
[packages-lifecycle.md](packages-lifecycle.md), with [packages-tests.md](packages-tests.md) or with
[two-programs.md](two-programs.md) sections 4.1 and 4.4, this section wins once the items of 10.8 that it names are
built. Its tests are packages-tests.md's items A18 to A28.

### 10.1 What we run, and the end state

Today hostkeeper deploys only `hostkeeper-archive` packages, each listed in a release index, served by the release
channel and attested:

- `hostkeeper` itself, on every host of the farm;
- `hello`, the demo service, on every host of the farm;
- `mdm`, the MDM host's service, is built as one (docs/operations/mdm-package.md), whether or not the MDM host runs
  it under hostkeeper yet; the `pxe` package of two-programs.md section 6.1 (N1) is designed as one too, and until it
  ships `hostkeeper-pxe` stays in hostkeeper's `.deb`.

tierstore is next. It runs on the lab's hosts outside hostkeeper today. No host of the farm is a Mac (none is enrolled,
hostkeeper-mdm.md section 2), and none runs Alpine or an RPM-family distribution. The daemon itself is shipped for
Alpine under OpenRC (two-programs.md section 4.6), with its own `.apk` and the required `openrc-device` check.

**The end state.** Two installers, each the format's own tool run by the daemon as root:

| Host | Installer | Entry |
|---|---|---|
| Debian, Ubuntu and their derivatives | `apt` (A7) | A repository entry: `repository`, `package_version`, the required `digest` pin, the manifest (section 2) |
| macOS | `macos-pkg` (A9) | An artifact entry: `locations` naming the testing or stable store of section 4.7, `digest`, `bytes`, the manifest; no attestation (10.3) |
| Alpine | `apk` (A26, A27), a backend of the repository installer: a signed `APKINDEX`, a key of the host's configuration, the pin | As `apt` |
| An RPM-family host | `dnf` (A8, later, section 6) | As `apt` |
| Any other host | None | |

`apk` is in the end state because the daemon already ships for Alpine: without it an Alpine daemon could not update
itself once archives stop, and the `openrc-device` check could not keep its self-update cases (rule T7). It is not yet
designed: A26 is its design pull request, as `dnf` would have, shaped like section 4's `apt`, A27 builds it, and the
deletion (A25) waits for it. `dnf` waits for the first RPM-family host, since no daemon of ours ships for one.

Withdrawn: `hostkeeper-archive` (deleted by A25, after the move of 10.6), `tarball` (never built; taken out of A6) and
`oci-image` (A10 withdrawn: nothing we run ships a container image to a host; the coordinator ships as an image, but it
is not a service hostkeeper deploys, 10.4). A25 takes the three names out of `hostkeeper-release`'s installer set; a
set that names one is then accepted as any unknown installer is, and no host is ever picked for it (packages.md
section 3.4).

**What a release's targets name.** One entry per platform, for every service, `hostkeeper` included:

```json
"targets": {
  "linux-x86_64": { "entries": [
    { "installer": "apt", "repository": "komira-stable", "package_version": "1.6.0-1", "digest": "<64 hex>",
      "manifest": { "package": "tierstore", "run": ["tierstore.service"], "hook": { "timeout_secs": {...} },
                    "installer_bounds": {...}, "min_hostkeeper": "0.1.200" } } ] },
  "linux-arm64":  { "entries": [ { "installer": "apt", ... } ] },
  "macos-arm64":  { "entries": [
    { "installer": "macos-pkg", "locations": [ { "store": "komira-stable", "locator": "tierstore/1.6.0/tierstore-1.6.0.pkg" } ],
      "digest": "<64 hex>", "bytes": "...", "manifest": { "package": "dev.tierstore.server", "run": ["tierstore"], ... } } ] }
}
```

During the move (10.6) a list also holds one `hostkeeper-archive` entry after the native one. A host of a platform or
distribution with no entry it can install shows `no_build_for_host`, as today; none of the farm's does.

### 10.2 Where each piece of today's archive goes

| Today's archive | In a `.deb` | In a `.pkg` | Who acts on it |
|---|---|---|---|
| `bin/<program>` | `/usr/bin/<program>`, or `/usr/lib/<package>/` for a program no one types | `/Library/Application Support/<service>/bin/` | The format's tool installs it; the service runs from where it is installed (sections 4.7, 5) |
| `units/` | `/usr/lib/systemd/system/<unit>`, installed disabled | `/Library/LaunchDaemons/<label>.plist`, `Disabled` true | hostkeeper enables, starts, stops and observes them (section 5) |
| `files/` and `[[files]]` | The package's own files: `/etc/<service>/…` as conffiles, `/usr/share/<package>/…` | Under `/Library/Application Support/<service>/` | The format's tool; removed by the purge or the receipt |
| `[hook] command` | The program `/usr/lib/<package>/hostkeeper-hook` | The program `/Library/Application Support/<service>/hostkeeper-hook` | hostkeeper runs it from the sealed tree, as today (below) |
| `manifest.toml`'s name, version, platform | Control fields `Package`, `Version`, `Architecture` | `PackageInfo`'s `identifier` and `version` | Checked against the order at `stage` (sections 4.3, 4.7) |
| `manifest.toml`'s `min_hostkeeper`, `[units] run`, `[health] url`, `[hook]` bounds and `boot`, `[params]` | The entry's manifest in the call (packages.md section 4.2), as for every entry since A1 | The same | Checked by the coordinator and the daemon (packages.md section 4.4) |
| The archive's Sigstore bundle | The repository's signature and the pin (section 3.3) | The package's signature and the pin (section 4.7) | 10.3 |
| Per-host settings | Not in any package: host setup's, as `hostkeeper.toml` is | The same | The operator |

**The hook at a fixed path.** A package that has a hand-over carries its hook at one path: `usr/lib/<package>/hostkeeper-hook`
in a `.deb` or an `.apk`, `Library/Application Support/<service>/hostkeeper-hook` in a `.pkg`. A package without one
gets the built-in steps (packages-lifecycle.md section 6.3); whether it has one is the file's presence, checked at
`stage`. The entry's `hook.command` must be empty for every native installer, and `hook.boot` is then allowed, refused
at `stage` when the package has no hook (A18 changes both rules of `hostkeeper-release`, which today refuses `boot` with
an empty command; no caller sends a native entry yet). The hook's bounds, `verify_deadline_secs`, `boot`,
`promotion_ceiling_secs` and `cleanup_retries` stay in the manifest's `Hook`. The file is the hook program itself,
called with the step as its first argument: a service whose hook today is its own binary with a verb (`tierstore
update-hook`) ships a two-line wrapper there; tierstore's packaging, in its own repository, adds it. The
daemon runs `<version dir>/tree/<path> <step> [<version dir>] --params <file>` in a transient unit, under the step's
bound: the contract of `crates/hostkeeper-hook` is unchanged (the steps `prepare`, `cutover`, `verify`, `rollback`,
`cleanup`, `uninstall` and `boot`, the exit codes, the last line, the params file). The program comes from the sealed
tree, so `prepare` runs before `activate`, from the version being installed, as today. Why fixed: the caller of
`SetDesired` no longer chooses which program of the package runs as root, nor its arguments. What that caller decides
(packages.md section 8.2) narrows to the params, the bounds, the `run` units and the health URL; the program is part of
the signed bytes, at a path the package's builder chose.

**What replaces `manifest.toml`: nothing hostkeeper-specific in the package but the hook.** The package's identity is
in its own control fields. What hostkeeper needs to run the service is the entry's manifest, the message A1 built,
which the coordinator and the daemon check and every set audits. A small hostkeeper file inside the package was
weighed: it would be signed with the package, but it would be a second place for what the manifest says, a
hostkeeper format again, and one more file every service's packaging writes.

**Maintainer scripts: none.** A service's `.deb` has no `preinst`, `postinst`, `prerm`, `postrm`, `config` or
`triggers` in its control archive, only `control`, `md5sums` and `conffiles`; dpkg needs none of the scripts. A
service's `.pkg` has no `Scripts` in its component and no script in its `Distribution` (the latter refused already,
section 4.7); an `.apk` has no `.pre-install` or other script (A27). What a script would do at install is done by the
unit (`StateDirectory=`, `DynamicUser=`), by the hook's `prepare`, which runs as root, or by host setup. The daemon
refuses a package with a script at `stage`, before anything is installed: `Failed`, `release`, `package_scripts`.

**Where a package's files may go.** Without scripts a package could still act through its files, so a service's
`.deb` (and `.apk`) holds files only under `/usr/bin/`, `/usr/lib/<package>/`, `/usr/share/<package>/`,
`/usr/share/doc/<package>/`, `/etc/<service>/`, and its `run` units in `/usr/lib/systemd/system/` (or its init
scripts in `/etc/init.d/` on Alpine), as section 4.7's rule does for a `.pkg`. Anything else (an enablement link under
`/etc/systemd/system/`, a cron file, a `sudoers.d` file, a generator, an `ld.so.conf.d` file, a unit it does not
`run`) is `manifest_invalid` at `stage`. So nothing of the package's own runs as root at install, only the format's
tool and the triggers of the host's own packages (`man-db`, systemd's reload), as for any install; nothing of the
package can enable or start its units (section 5's install check becomes a check at `stage`); and a Mac's install no
longer runs package code with the network. The one exception is hostkeeper's own package (10.4).

**What the daemon checks before it runs anything**, in order, each before the next reads a byte:

1. **The order**: the entry's shape and kind, the installer listed, `min_hostkeeper`, the manifest's checks
   (packages.md section 4.4), `hook.command` empty.
2. **The bytes**: for `apt`, the suite's signature by the host's keyring, the stanza's `SHA256` equal to the pin and
   its `Size` within bounds, the simulation, the downloaded file hashed equal to the pin (section 4.3); for
   `macos-pkg`, the size and digest equal to the pin, then the signature's leaf among the host's signers (section 4.7).
3. **The identity**: the control fields or `PackageInfo` equal to `package`, the version and the architecture.
4. **No scripts**, as above.
5. **The tree**, extracted with no script run: the core's bounded walk (and `MacVerify` on a Mac); every `run` unit or
   label a file of the tree, every plist `Disabled`; every file within the path rule above (section 4.7's on a Mac); the hook,
   when present, a regular executable file at its path, reached through no link.
6. **The seal**. Only then the hook's `prepare`, from the tree, and `activate`, the format's tool under its bound.

### 10.3 Trust: the key, the signature and the pin; the attestation leaves the host

The repository's key and the pin (`apt`, section 3.3), and the package's signature and the pin (`macos-pkg`, section
4.7), replace the bundle check of archives. **The attestation check goes from the host, and is made once, where
packages are published.**

- Both formats already carry a signature that the format's own tool checks against a key host setup placed, and the
  pin names the bytes CI asked for. The attestation would be a third check on a Mac only, since `apt` has none (section
  3.3): the two platforms would trust differently for no gain.
- The attestation says this organisation's CI built the bytes. That is checked once, before any host can see them:
  the shared release machinery's publish job, the only holder of the repository's signing key and of the `.pkg`
  signing identity, runs on `main` in a protected environment and verifies the attestation of the bytes it is given
  before it signs them (for a `.pkg`, the attestation of the unsigned build; the signed file is attested again before
  its pin is taken, section 4.7) and publishes them to testing. Its negative case, a build with no attestation refused,
  is that machinery's test, in its own repository (rule T15); A23's workflow lint holds hostkeeper's release to calling
  it. A host then trusts the signature that publishing made.
- What a host gives up. Today an attacker must get bytes built on `main` by the trusted workflow, and also set the
  release. After, an attacker who holds the signing key and can set the release (CI's OIDC identity for the service,
  an operator token, or the coordinator) installs bytes `main` never built. With the key alone they cannot choose
  which bytes a host installs (the pin), and with the deploy path alone they cannot get bytes signed. Keeping the key
  in the one publish job, behind the attestation check, is what keeps the two apart.
- What leaves the daemon (A25): `prove.rs`, its `gh attestation verify` call and the trusted root's report
  (`crates/hostkeeper-daemon/src/trusted_root.rs`; `trusted_root_age_secs` and `trusted_root_reason` stay in the
  proto, unsent). The daemon stops reading `release_channel` and `[trusted_signers]`; the loader keeps accepting both,
  since an older binary requires them after a fallback, `hostkeeper-pxe` reads `[trusted_signers]` from the same file,
  and the file is never rewritten. Host setup keeps writing both while a release that requires them may run.
- What stays elsewhere: `hostkeeper-pxe` checks the attestation of `netboot.json` and of the `.deb` it seeds
  (network-install.md), on the hop before any key is on the new host, against `[trusted_signers]`. So the pinned `gh`,
  `packaging/sigstore/trusted_root.json` and its refresh workflow `trusted-root.yml` stay, for it.
- A `macos-pkg` entry carries no attestation, and A18 makes the coordinator refuse one, as it refuses one on a
  repository entry (section 2.2): an attestation nobody checks must not look checked. No daemon lists `macos-pkg` yet
  (A9 is not built), so nothing that shipped changes.

### 10.4 hostkeeper's own update

**The release.** The service `hostkeeper` is set like every service: per platform one native entry, `apt` on Linux
and `macos-pkg` on macOS, and during the move one `hostkeeper-archive` entry after it (10.6). Its package is the
`.deb` and the `.pkg` that bootstrap a host today, published by the shared release machinery to the repository's
testing suite and the testing store, and promoted to stable, as every service's is. A18 changes `hostkeeper-release`'s
rule for the service from "one `hostkeeper-archive` entry" to "one native entry, and at most one `hostkeeper-archive`
entry after it"; A25 drops the archive half.

**The self-update through it** (A19). The hand-over of two-programs.md section 4.3 is unchanged:

1. `stage` the version as any `apt` or `macos-pkg` version (10.2). The new binary is `tree/usr/bin/hostkeeper` of a
   `.deb`, `tree/usr/local/bin/hostkeeper` of a `.pkg`: the path the daemon's `RELEASE_BINARY` names instead of
   today's `bin/hostkeeper`.
2. Judge it by its floor: the entry's `min_hostkeeper`, which the release builder takes from `OLDEST_READER`, and the
   backward rule, as today (two-programs.md section 4.2).
3. No hook: hostkeeper's package has none, and its steps are the built-in ones it runs today (section 4.4 there).
4. `follow_on` copies the binary to `bin/versions/<N>`, links `next`, writes the wait and exits 0; the restart and the
   hand-over's three steps follow.
5. `N`, once it has confirmed itself, runs `activate` of its own version: `apt-get install hostkeeper=<V>` and the
   hold, or `installer -pkg`, and records it. Only now do the package database and `/usr/bin/hostkeeper` change. The
   record keeps the `activate` pending until it is done, so a crash between the confirm and the end of `activate` is
   resumed by `N`'s next start. The verdict `Succeeded` is reported after `activate`. A deferred `activate`
   (`package_manager_busy`, section 4.5) stays pending and is tried again on the deferral's schedule, as any deferred
   step is. A failed one is `Failed` with section 4.5's class and reason (`dpkg_error`, `installer_error`); `N` keeps
   running, since it has proved it starts, and is not rolled back, and it reports hostkeeper's drift (the package
   database still at the old version). The rollout gate stops on it, and a `ForceRollback` hands back to the old
   version, whose package is still the installed one. The record gains one optional field, the pending `activate`,
   which an older reader reads as absent, as `next` and `starts` were (two-programs.md section 4.3); no older binary
   ever meets it, since only `N` writes it, after its confirm.

**Why `activate` comes after the confirm.** The unit starts `/usr/bin/hostkeeper` (below). If dpkg replaced it before
`N` proved it starts, an `N` that crashes before its hand-over code would be started for ever, with nothing left to
fall back to. After the confirm, the binary the package installs is one that has started on this host. This keeps the
invariant of two-programs.md section 4.1, that the unit starts a binary that ran, while the package database names the
version that runs.

**One unit.** The `.deb`'s `/usr/lib/systemd/system/hostkeeper.service` (`ExecStart=/usr/bin/hostkeeper daemon`,
which seeds and execs `current`, two-programs.md section 2.1) is the only unit. The archive's package unit
(`units/hostkeeper.service`, linked into `/etc/systemd/system/`) is taken away by the move (10.6) and deleted with the
archive. two-programs.md section 10 had the package unit start `current` for good because `/usr/bin/hostkeeper` was as
old as the host's last `.deb`; from A19 it is the last version that confirmed. On a Mac the plist already starts
`/usr/local/bin/hostkeeper` (two-programs.md section 4.1).

**hostkeeper's own package keeps its scripts**, the one exception to 10.2's rule. It is also the bootstrap package,
installed by dpkg or by MDM before any daemon runs, so it makes its directories and enables its own unit or job. Its
scripts are the texts the packaging tests hold today, with one change: the `.pkg`'s postinstall bootstraps the
job only when launchd does not have it loaded, so an `installer` the daemon itself runs never boots the daemon out
in the middle of its update. That change lands in A19 itself, so that `rollback_below_installer`'s row for A19 also
keeps a Mac from installing an older `.pkg` whose postinstall boots the job out. The plist's text then takes effect
at the next boot; the packaging tests hold it unchanged, and a release that changes it says so in its notes. The
`.deb`'s postinst already enables the unit only on a first install and never starts or restarts it, and its prerm
acts only on `remove`; every shipped `.deb` has those texts (A19's device test upgrades from the `.deb` of the release
X1 pins). The daemon allows scripts only when both the service and the package are `hostkeeper`; the repository's
signature, or the package's, is their trust, as for every byte. Its file paths are its own package's list
(packaging/linux/nfpm.yaml, docs/macos.md), held by its packaging tests, not the rule of 10.2 for a service.

**The bootstrap install is adopted.** A host set up from the `.deb` or the `.pkg` holds `hostkeeper` in its package
database with no record. A daemon with a native installer that finds, at its start, its own package at the version
`current` runs, and no `hostkeeper-archive` version of `hostkeeper` on the host, records it as installed by that
installer, and sets the hold (`apt-mark hold`), so nothing but hostkeeper replaces `/usr/bin/hostkeeper` from then
on; that is not drift. Its next update is an `update` through the installer. A host that runs `hostkeeper` from
archives is moved instead (10.6).

**The coordinator** is not a service hostkeeper deploys. It ships as its container image and its packages, published
by the shared release machinery as tierstore's are, and its deployment updates it. Nothing of it changes here but
`--release-channel`, which its deployment stops passing at 10.5 and A25 deletes.

### 10.5 The release channel's end

The channel and its sync are interim, as ruled. The channel serves archives and nothing else, and it is needed until
the last archive is ordered: a daemon before `target-v1` fetches through nothing else, and a `target-v1` daemon with no
`stores.toml` defines only the `channel` store. So, exactly:

1. Until A24, the channel is filled by `channel-sync` and `release.yml` waits for it (its `channel` job), as today.
2. A24 is the first release of every service set with native entries alone, with no archive entry and no index. It
   takes from `release.yml`'s `channel` job the wait for the channel, the comparison of its index and the index
   outputs, and keeps the job's `targets` and `newest` steps, renamed `targets`, which `set-desired` needs. It is set
   only when stage 9's condition holds (10.6).
3. Once A24's release has converged on every host, the operator stops the sync's timer (or its compose sidecar),
   removes `--release-channel` from the coordinator's deployment, and deletes the channel's tree. From then nothing
   reads it: no order names an archive, a rollback takes the host's sealed copy and fetches nothing, and a `Force` to a
   version no longer on a host stages it from the repository or the store.
4. A25 and A28 delete the code, the docs and the workflow steps (10.7). Two other readers move first: X1's rig
   (`self_update_across_releases`) runs the pinned previous release against a loopback channel, and its pin moves to
   a release from A19 on with A24, so it installs native entries; and the MDM gate's pkg allowlist (hostkeeper-mdm.md
   section 7.1) names a `.pkg` from A19's release on, so a Mac enrolled after stage 9 gets a daemon that can update.

### 10.6 The move of the farm

The stages replace packages.md section 9.2's stages 5 and 6 (A11 and A12). A stage starts when the previous one holds
on every host. A service moves only once its own release lists a native entry: that entry is added to `hello`'s
release at stage 7 and to `mdm`'s and `hostkeeper`'s at stage 8, each by its own pull request, so neither host setup
nor an automatic release moves a service before its stage.

| Stage | What | Starts when | Still answered | Back out |
|---|---|---|---|---|
| 5 | Daemons with the native installers (A6, A7; A9 on a Mac; A27 on Alpine), the move and the read of stage 9 (A18, A19); the native packages built (A20) | A4's release runs on every host | Every release lists archive entries alone, ordered as today; a daemon before `target-v1` fetches through the channel | Set the previous release; nothing has moved |
| 6 | Host setup places the repository entries and the key on each host (A17's runbook, A22) and restarts its daemon, which then announces `apt` | Stage 5 on every host | A host not yet set up keeps its archives | Remove the entries and restart: the host stops announcing `apt` |
| 7 | `hello`'s release lists its native entry before its archive one (A21), and `hello` moves through its rollout plan, the canary of the move | Stage 6 on at least one host | A host not yet set up is picked the archive entry | Set `hello`'s release without the native entry; the move's rollback (below) |
| 8 | `mdm`'s release (A21) and then `hostkeeper`'s (A23) list their native entry before the archive one, and they move the same way | `hello` moved on every host | The same | The same |
| 9 | Archives stop (A24): every release set with native entries alone; the channel ends (10.5) | Both of stage 9's counts are zero (below) | Nothing of the archive | Set a release with archive entries again: the code is still there |
| 10 | Deletion (A25, A28) | Stage 9's release converged on every host, and A27 built | Nothing | A rollback of `hostkeeper` below A25 is allowed: A19 to A24 read native versions |

**Stage 9's condition** is two counts over every host the registry holds, whether or not it heartbeats: hosts that
run any service from `hostkeeper-archive`, and hosts whose daemon is older than A19's release (a daemon before
`target-v1` among them). `Converge` cannot give them: it counts one service's version, and a host picked nothing does
not hold its convergence. A18 adds the read: each host's services with their installer, an additive field of `Host` in
`Hosts.List` beside the daemon's version it shows today. The runbook (A22) gives the two `hkctl` queries, and A24's
pull request shows both at zero. A host that keeps either count above zero is named by them; the operator brings it
up, or removes it from the fleet and installs it afresh (network-install.md). So no host is left on a daemon that
nothing can update.

After stage 9 a host set up from a `.deb`, `.pkg` or `.apk` older than A19's release has a daemon that can install
nothing a release lists: it shows `no_build_for_host` for `hostkeeper`, and is set up again from the current package.
The runbooks (enrollment, network install, the MDM kit) install the newest release's.

**tierstore never ships as an archive.** Its first deploy under hostkeeper is its `apt` entry, on hosts that announce
`apt` (from stage 6), and its release lists `apt` alone. A host not yet set up shows `no_build_for_host` for it and
keeps running tierstore as it does today, outside hostkeeper. So tierstore's move waits for A7 and A17, not for stage 9.
Its hand-over (host-updates.md section 4.1) starts the new version in its other slot from the new files while the old
process runs on from its own, so it needs no old file at its path and fits `apt` (section 5); a service whose hand-over
does is not one we run.

**The move of one service on one host** (A18, A19):

- **The pick.** A host that runs the service from `hostkeeper-archive`, and whose daemon lists the installer of a
  native entry of its platform's list, is picked that native entry. This is the one exception to packages.md section
  3.3's first rule ("a host takes the entry of the installer it runs"). A host that lists no native installer of the
  list is picked the archive entry, and an old daemon is too.
- **The daemon.** The one installer change it accepts is from `hostkeeper-archive` to a native installer. Back, or
  between native installers, is `installer_changed`, as today. The move is an update: `stage` the native version, the
  hook's `prepare` from its tree, then `activate`, which first takes away the archive version's placed units and files
  (listed in `placed.json`; a placed unit in `/etc/systemd/system/` would hide the package's unit of the same name),
  then installs the package and enables its units; then `cutover`, `verify` and `cleanup`. The archive version stays
  on the host as `previous` until the next update's `retire`.
- **The move's rollback**, automatic or a `ForceRollback`, before or after the move settled: `activate` the archive
  version again. It purges the native package (for a service; for `hostkeeper`, the package stays at the version apt
  installed, since purging it would remove the unit and the binary the host starts), places the archive's paths
  again, and records `hostkeeper-archive`. The coordinator orders it with the release's archive entry, which A18 sends
  for a rollback of a host whose installer is native to a release with no native entry. The daemon takes it from its
  sealed copy and fetches nothing. Once the next update has retired the archive version, the move cannot be undone.
- **hostkeeper's move** is 10.4's self-update with the move's `activate`. After `N` confirms, it installs its `.deb`
  (an upgrade of the bootstrap package dpkg still lists at the host's first version), enables the `.deb`'s unit, takes
  away the archive's package unit from `/etc/systemd/system/` and reloads systemd. The running daemon is not
  restarted; its next start runs `/usr/bin/hostkeeper` (`N`), which execs `current` (`N`). A move that fails before
  the confirm is a failed self-update: the old binary goes on from archives, unchanged.
- **A rollback of the daemon below the change** is refused before anything switches: to a release before A7 on a host
  that holds an `apt` version, and to one before A19 on a host where `hostkeeper` itself moved, both
  `rollback_below_installer` (packages.md section 9.1; A19 adds its row to the compiled table). A hand-over that fails
  runs the old binary, which is the one that ran the move, so it reads everything on the host.
- **Old daemons.** A daemon before `target-v1` is ordered the archive entry through the channel, as today, until it
  updates. Stage 9's second count waits for it.

### 10.7 What is deleted at the end (A25, A28)

- **Crates.** `hostkeeper-unpack` (its `MacVerify` moves to `hostkeeper-install`, for `macos-pkg`'s `stage`).
  `hostkeeper-package` shrinks to what is still used: its `net`, `ports`, `bundle`, `der` and `gh` modules, which
  `hostkeeper-pxe` uses to check what it seeds, and its fetch by digest (`stores.rs`, `entry.rs`, `stage.rs`), which
  `macos-pkg` uses, without the proof; `index.rs`, `fetch.rs` and `prove.rs` go, and its `Platform` copy gives way to
  `hostkeeper-config`'s `platform.rs`. Every release index parser goes: `hostkeeper-config`'s `index.rs`,
  `hostkeeper-package`'s and xtask's writer (`release/index.rs`), after their users move (the coordinator's
  `releases.rs`, xtask's `release/targets.rs`). `hostkeeper-config`'s `manifest.rs` (the `manifest.toml` loader) and
  `channel_trust.rs` go; its `daemon.rs` keeps `release_channel` and `[trusted_signers]` (10.3).
  `hostkeeper-placed`'s archive placement (`placer.rs`, `lint.rs`, the relink) goes; its record of enablement links
  and `installed` stays. A25 carries the supersession check of rule B22 for each crate it deletes or shrinks.
- **Daemon and coordinator code.** The `hostkeeper-archive` installer, the channel fetch of `real_packages.rs`, the
  switch and relink of `place.rs`, the attestation proof and the trusted root's report (10.3). At its start a daemon
  from A25 deletes the archive version directory of a service it runs natively (the move's `previous`), and nothing
  else. The coordinator's `channel.rs`, `channel_https.rs`, `release_index.rs`, the `--release-channel` flag, the
  archive rules of `picks.rs` and the orders by index for old daemons; the JSON route stays frozen and carries no order.
  A stored release with archive entries or an index still loads, its archive entries never picked.
  `hostkeeper-release`'s installer set loses `hostkeeper-archive`, `tarball` and `oci-image` (10.1).
- **Proto: nothing removed.** `package_digest`, `release_index`, `release_index_digest`, `index_digest`,
  `GATE_REASON_NO_PACKAGE_FOR_PLATFORM`, `Place` and `Container` stay. A set with `package_digest` or `release_index`
  is still accepted and orders nothing, so the hosts show `no_build_for_host` and `Converge` says not converged: an old
  CI client's call fails loudly in its wait. That, the bootstrap floor and the withdrawn installer names are the
  waivers of rule B8 this ruling makes (10.9).
- **xtask.** The `release package` verb with its archive builders (`package.rs`, `package/hello.rs`,
  `package/mdm.rs`), while `package/linux_check`, `package/nfpm_stage` and `package/elf` stay for `check-linux-pkg` and
  `nfpm-stage`; `release index`, `channel-stage`, `channel-sync` and its ports; the archive entries of `release
  targets`; and the archive and index asset names of the release contract and of `release check`.
- **Docs.** `docs/package-format.md`, `docs/deploy/release-channel.md` and `docs/deploy/channel-sync/`; the archive
  and channel parts of `docs/deploy/ci-client.md`, `service-caller.md`, `github-actions.md` and
  `verify-a-release.md`, `docs/hello.md`, `docs/macos.md`, `docs/operations/mdm-package.md` (its paths under
  `packages/mdm/current/` move to the `.deb`'s, A20) and `docs/operations/self-update-after-bootstrap.md`; the channel
  and attestation statements of network-install.md and hostkeeper-mdm.md; hostkeeper-design.md sections 4.7 and 5.5
  and two-programs.md sections 4.1, 4.4 and 4.6 become pointers to this file.
- **Workflows.** `artifacts.yml`'s `release package` steps (hostkeeper, `hello`, `hello` fail-ready, `mdm`), its
  `release index` step and their bundles; `release.yml`'s channel wait (A24, 10.5); `set-desired.yml`'s
  `package_digest`, `release_index` and `release_index_digest` inputs, in a new revision of the client (a caller pinned
  to an older one keeps them); `hello-desired.yml`'s archive downloads. `trusted-root.yml` stays (10.3).
- **Tests.** Those of the deleted code: `hostkeeper-unpack`'s; `hostkeeper-package`'s fetch, index and fixture tests;
  the daemon's `real_packages.rs`, `index_digest.rs`, `install.rs`, `placed_update.rs`, `trusted_root.rs`,
  `trusted_root_beat.rs` and `support_archive/`; the coordinator's `release_channel.rs` and `channel_set_tests.rs`;
  xtask's `channel_sync.rs`, `package_launch.rs` and the index tests; the layering rows of the deleted crates. The
  device tests that run archives today are not deleted, and keep every case (rule T7): `tierstore_shaped.rs`,
  `hello_rig.rs` and the self-update tests are re-pointed at `.deb` and `.pkg` fixtures by A19 to A21, the rollout
  tests at `hello`'s fail-ready `.deb` (A20), and `openrc_device.rs` and `selfupdate_openrc.rs` at an `.apk` fixture
  repository by A27.

### 10.8 The build items, re-scoped

| # | Item, one pull request | After | In parallel with |
|---|---|---|---|
| A6 | The installer trait, the version directory (`target.json`, `tree/`), the walk, built-in steps, `installed` and its tombstone, `ServiceBeat.installer`, `installer_changed` with the move's exception (10.6). `tarball`, `place`, the gzip and xz decoders and the free layout are taken out | A4 | A18, A20 |
| A7 | The repository installer and `apt` (section 8), plus the hook at its fixed path, the refusal of scripts and the path rule (10.2) | A6, A16 | A18, A20 |
| A9 | `macos-pkg` (section 4.7) without the attestation check, plus the hook's fixed path and the refusal of scripts | A6, A7, M11 | A17's Linux half, A18 to A24 on Linux |
| A17 | Host setup places the repository entries and keys, and on a Mac the stores and signers (section 8) | A7, A9 | A19 |
| A18 | Coordinator and `hostkeeper-release`: the move's pick, the rollback across the move, `hostkeeper`'s list rule (10.4), `hook.command` empty and `boot` allowed for native entries, no attestation on a `macos-pkg` entry, each host's services and installers in `Hosts.List` | A16 | A6, A7, A20 |
| A19 | Daemon: `hostkeeper`'s self-update through a native installer (10.4), the bootstrap install adopted and held, the move and its rollback (10.6), scripts allowed in `hostkeeper`'s package alone, the `.pkg`'s postinstall that bootstraps only a job not loaded, the `rollback_below_installer` row | A7, A18 (A9 for the Mac half) | A20, A17 |
| A20 | Packaging: `hello` and its fail-ready build as a `.deb` and a `.pkg`, `mdm` as a `.deb` (its paths and units moved, docs/operations/mdm-package.md), each with its hook at the fixed path and no script | none | A6 to A19 |
| A21 | `hello`'s (stage 7) and then `mdm`'s (stage 8) release: the packages to the repository's testing suite and the testing store through the shared release machinery; `release targets` lists the native entry before the archive one; `hello-desired.yml` the same | A18 released, A20, the project's repository | A19 |
| A22 | The runbook of the move: per host the repository entries, the restart and the check that `apt` is announced; the stages' conditions and stage 9's two queries | A17, A18 | A19 to A21 |
| A23 | hostkeeper's own release (stage 8): its `.deb` and `.pkg` through the release machinery, the native entry before the archive one | A19 released, A21 | none |
| A24 | Stage 9: every release with native entries alone; `release.yml` without the channel wait and the index (10.5); X1's pin moved to a release from A19 on | Stage 9's counts zero; A9 built, or else `macos-arm64` left out of every list while no Mac is enrolled (its hosts would show `no_target`) | none |
| A25 | Deletion in the daemon, the coordinator and the crates (10.7) | A24 converged, A27 | A28 |
| A26 | The design of the `apk` backend for Alpine, a pull request of design only, like section 4 for `apt` | A7 | A18 to A24 |
| A27 | The `apk` backend, the Alpine package published to an apk repository through the release machinery, and the OpenRC device tests on it | A26 | A18 to A24 |
| A28 | Deletion in xtask, the workflows and the docs (10.7) | A24 converged | A25 |

Withdrawn: A10 (`oci-image`), A11 and A12 (replaced by stages 9 and 10), as A13 and A14 were. A5 is built; A21 and A23
change what its steps send. A8 (`dnf`) stays later. two-programs.md's S3 is independent of these items: whichever of
S3 and A19 lands second carries the other's change to the self-update code.

### 10.9 Decisions

| Question | Decision | Why |
|---|---|---|
| Installers in the end state | `apt` and `macos-pkg`, and `apk` for the Alpine daemon already shipped; `dnf` when an RPM host is run | Those are the hosts a daemon of ours ships for; a backend no daemon needs is a speculative port (rule B20) |
| `tarball` | Withdrawn, never built | Every host a daemon ships for has a native installer |
| `oci-image` (A10) | Withdrawn | No service we run ships an image to a host; the coordinator's image is not a hostkeeper service |
| What replaces `manifest.toml` | The package's control fields for its identity, the entry's manifest for the rest; nothing hostkeeper-specific in the package but the hook | The manifest is built, checked twice and audited; a file in the package would be a second place and a hostkeeper format again |
| The hook | The program at a fixed path of the package, run from the sealed tree with today's contract; `hook.command` empty, `boot` allowed | The caller of `SetDesired` no longer picks a root program or its arguments |
| Maintainer scripts and paths | None in a service's package, refused at `stage` (`package_scripts`); files only under its own paths; hostkeeper's own package keeps its bootstrap scripts | dpkg, apk and `installer` need none; with none, only the format's tool runs at install and nothing enables or starts the units but hostkeeper |
| The attestation | Leaves the host; the publish job that alone holds the signing keys checks it before it signs; a `macos-pkg` entry carries none | The key and the pin bind the bytes on every platform; one trust model; the loss is stated in 10.3 |
| `release_channel` and `[trusted_signers]` | Still accepted by the loader; the daemon stops reading them | An older binary requires them after a fallback, `hostkeeper-pxe` reads the signers, and `hostkeeper.toml` is never rewritten |
| `hostkeeper-package` | Shrunk, not deleted: pxe's attestation check and `macos-pkg`'s fetch by digest stay | Both still have users; moving them would only rename them |
| hostkeeper's own release | One native entry per platform, through the same installers | One way to install anything |
| When hostkeeper's package is installed | After the new binary confirms, by that binary; a failure there leaves it running and reports drift | The unit starts `/usr/bin/hostkeeper`; it must always be a binary that started |
| The unit | The `.deb`'s, starting `/usr/bin/hostkeeper` | The packaged binary is now the last confirmed one; one unit instead of two |
| The bootstrap install | Adopted and held at start when no archive version of `hostkeeper` is on the host | It is hostkeeper's own install, not drift, and nothing else may replace it |
| How a host moves | The pick prefers a native entry over the archive one it runs; the daemon accepts that one installer change | No uninstall: `hostkeeper` cannot uninstall itself, and a service keeps running |
| What orders the moves | A service's release gains its native entry at its stage; host setup decides which hosts can move | Neither host setup nor an automatic release moves a service early; `hello` is the canary |
| The move's rollback | To the archive version kept as `previous`, from its sealed copy, until the next update retires it | Rollback below the change works with the channel and the network down |
| Rollback of the daemon below the change | `rollback_below_installer` | The daemon knows what its disk holds |
| Stage 9's condition | Two counts over every registered host, from a new field of `Hosts.List` | `Converge` counts one service and ignores unpicked hosts; a stranded host must be named |
| The channel's end | After A24's release, the first with no archive entry, has converged on every host | It serves archives only; before that, some order may still name one |
| tierstore | Never an archive; its first deploy under hostkeeper is `apt` | An archive would be built only to be deleted, and its hand-over fits `apt` |
| Rule B8 | Three waivers, made by the ruling that ends the format: the bootstrap floor rises to A19's release; a set with `package_digest` or `release_index` orders nothing; `hostkeeper-archive`, `tarball` and `oci-image` leave the installer set. No proto field is removed or renumbered | The format cannot end otherwise. The B8 tests in both directions: X1 runs the previous release on a host holding native versions and the move's records (A19, A24), and a daemon from A25 starts on a host holding an archive `previous` (A25) |
| A host that cannot move | Named by stage 9's counts; brought up, or removed and installed afresh | No host is left on a daemon nothing can update |
