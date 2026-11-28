# Scheduled maintenance updates

Status: design; the Linux report of sections 1 and 4.1 is built (`crates/hostkeeper-daemon/src/maintenance_facts.rs`), and on a Mac its macOS facts and due macOS updates, FileVault, the data volume's free space, the Command Line Tools, its Xcodes, its update account and the settings of section 5.8 (`maintenance_macos.rs`, `maintenance_mac.rs`, `maintenance_xcode.rs`, `maintenance_account.rs`, `maintenance_mdm.rs`), as are the maintenance hook's steps, the Linux kinds' `cutover` (`maintenance_linux.rs`) and their `verify` of section 5.6 (`crates/hostkeeper-daemon/src/maintenance_hook/linux.rs`), and the `macos` kind's `cutover`, install and `verify` on the daemon path of section 5.5 (`crates/hostkeeper-daemon/src/maintenance_hook/macos/apply.rs`; with FileVault on, a minor update's install without `--restart` and its authenticated restart, `maintenance_hook/macos/filevault.rs`), and the `xcode` kind's stage, `cutover` and `verify` of sections 2.4 and 5.6 (`crates/hostkeeper-daemon/src/maintenance_hook/xcode/`; no simulator platform is imported), the coordinator's `xcode` due of section 1.1 and the Xcode entries of the `maintain` order (`crates/hostkeeper-coordinator/src/maintenance_xcode.rs`), and the operator runbook of section 8's MU9 for Linux and the Mac (`docs/operations/maintenance.md`, whose list "Not built yet" says what of the Mac path an operator cannot use yet); nothing else. The owner's answers of 2026-11-19 are in: the Mac's update account (section 5.5) and the first-class Linux path
(the one-button update of section 4.2) are decided; for Xcode, section 2.0 makes a push from the operator's own Mac the source (owner's direction, 2026-11-20), with a fetcher Mac as the alternative. Facts about this repository were read on `main` at the commit this document lands on top
of; facts about komira-ai/jobscheduler and komira-ai/kbf were read on their `main` and are marked as theirs. **Built** marks what exists
(with its path), **New** what this design adds, **[V]** a claim to verify before building on it.

The owner's request (2026-11-19): "I want us to get updates regularly but I also don't want us to blow up ongoing work. The ideal scenario
is I get a notification in hostkeeper that updates are needed, and then I schedule updates, which would drain all work on the host, do the
update, and put it back in service."

The owner's answers on this design (2026-11-19): a Linux host that needs a kernel or security update or a restart is shown so, for the
host and the fleet ("kernel update, restart needed"), and **one button** drains it, updates it, reboots it if needed, checks it and puts it
back; the Mac's update account (a dedicated `hkupdate` administrator, section 5.5) goes in a root-only file hostkeeper reads for the update call; Xcode should always be the latest;
the coordinator discovers no OS update itself, and only "a new Xcode exists" is central (section 1); a Linux coordinator cannot be
assumed to download Xcode (section 2.1).

The owner's rulings of 2026-11-19 on the Mac (later the same day):

- **The trigger of a macOS update.** The ideal: the coordinator drains the Mac in js-agent and then has Apple Business's device
  management install the update, since its bootstrap token gives it the volume owner's rights and needs no password. That depends on
  Apple Business offering an API to send the update to one device; it does not today (section 5.5.1). Until it does, the daemon installs
  the update itself with a dedicated local account's password, which is not treated as a secret: no vault, but on standard input only,
  never in an argument and never in a log (section 5.5). Section 5.5.2 says how the coordinator switches to device management when the
  API exists.
- **Xcode in our store.** The `.xip` is kept unmodified in our private store and installed only on our own Macs: never public, never
  offered to anyone else, installed as Apple ships it (its signature checked; nothing repackaged, patched or stripped). `xcode-select`,
  the licence acceptance, the first launch and the platform import configure the installed app; they do not change it (section 2).

## What changes for the operator

**Before.** hostkeeper updates packages (its own, js-agent, the services of a host group) and installs the Command Line Tools on a Mac
whose group lists them (`docs/design/host-prerequisites.md`). The operating system is nobody's job: a Mac's macOS and Xcode, and a Linux
host's kernel and security packages, move when a person logs in and moves them, with no drain, and nothing tells anyone they are due.

**After.**

1. Every daemon reports, in its heartbeat, what its host runs (OS version and build, running and newest installed kernel, the Xcodes in
   `/Applications`) and what is due: macOS updates `softwareupdate` lists, Linux security and kernel updates, a reboot the host already
   needs. For Xcode it reports what is installed, and the coordinator marks a newer release it holds as due (section 1.1).
2. The web UI shows a **notice**: per host ("kernel update, restart needed"; "12 security updates"; "macOS 26.1 to 26.2, restart";
   "Xcode 27.2 available") and for the fleet (a strip on Home: "Updates due on 7 hosts in 2 groups").
3. The operator presses **Update** on a host, or on a group's notice: one button, which starts now with the group's defaults. **Schedule**
   is the same with choices: on a host, a group or a selection, **now** or **a time window** (start and end), and how many hosts of each
   group at once (**a rolling batch of N**, default 1).
4. Each host in turn: downloads and stages the update while it still serves, is **cordoned in js-agent**, **drains** (running jobs end on
   their own; no deadline unless the operator set one), applies the update, reboots if it needs to, is **checked**, and is **uncordoned**.
   The UI shows each host's step, and links the Console (Linux) or Desktop (Mac) page while it reboots.
5. A failure **halts** the run in that group: the failed host stays cordoned (if it was drained), no further host of the group starts,
   and a critical alert stands until the operator acts.

**What does not change.** Package rollouts, their gate and their rules; the hook contract of `prepare` to `cleanup`; hostkeeper's own
updates; js-agent's own cordon route. Nothing updates the OS unless an operator scheduled it: detection only reports.

## 1. What "an update is due" means

**The coordinator discovers no macOS or Linux update itself.** Each host's daemon checks its own host and reports what it finds in its
heartbeat, the due items included; the coordinator only shows the reports. The one signal that is central is "a new Xcode exists"
(section 1.1), because no host can see it: for the `xcode` row below the daemon reports only what is installed, and the coordinator
decides what is due.

The daemon looks at its host every hour, after every start, and after a maintenance step ends, and sooner after a look that failed (Built:
5 minutes after it ended, doubled for each further failure in a row, at most the hour; `maintenance_facts.rs`), as the prerequisites pass does today (Built:
`crates/hostkeeper-daemon/src/prerequisites.rs`, a worker thread that never holds up a heartbeat; `xcode_clt.rs`, which already runs
`softwareupdate`; the bounded program runner `run`/`run_ok` of `login_account.rs`). Each program runs as root with an empty environment, a deadline and capped output, through the same ports. **New:** the
checks below and the report of section 4.1. A package manager's run that finds its lock held is tried again every 2 seconds for at
most the daemon's `--package-lock-wait-secs`, as the hook's stage waits, and fails the look only when the lock is still held then (Built:
`LockWait` in `maintenance_collect.rs`; apt's `DPkg::Lock::Timeout` is not used, since apt 2.4 to 2.8 do not apply it to the lock of
`/var/lib/apt/lists` that `apt-get update` takes; dnf runs with `exit_on_lock`).

| Kind | Platform | What the daemon reads | Due when |
|---|---|---|---|
| `macos` | Mac | `sw_vers` (version, build); `softwareupdate --list` (each label, its version, whether it says `Action: restart`); `--list-full-installers` for a major upgrade | `softwareupdate` lists a macOS update. The daemon reports the newest major upgrade too, always; the coordinator counts it as due only when the group's `macos_major` is true (section 3), never by default |
| `xcode` | Mac | each `/Applications/Xcode*.app/Contents/version.plist` (`CFBundleShortVersionString`, `ProductBuildVersion`); `xcode-select -p` | decided by the coordinator: the newest `xcode_keep` releases its store holds (section 2) include one the host lacks or `xcode-select` is not the group's default |
| `linux` | Linux | apt: `apt-get update`, then a simulated upgrade, split by origin into security and other; dnf: `dnf updateinfo list --security` and `dnf check-update`; apk: `apk version -l '<'`. The kernel: `uname -r` against the newest installed kernel | a security update or a newer kernel package is available (group setting `linux_scope`: `security` by default, or `all`); under `security`, apt counts only packages already installed (section 5.5) |
| `reboot` | Linux | `/run/reboot-required` (apt); `needrestart -b` where installed (its kernel status, and the services running old libraries); `needs-restarting -r` (dnf); the newest kernel under `/lib/modules` against `uname -r` | the host needs a reboot it has not had: a newer kernel installed but not running, or the distribution says so. A run of `reboot` alone has nothing to install: it drains, reboots, checks and puts back |

Packages hostkeeper itself installs must not move in an OS upgrade. apt holds them (`apt-mark hold`, Built:
`docs/design/packages-repositories.md`). dnf and apk have no such hold today, so the maintenance hook excludes them by name (dnf
`--exclude`, apk by pinning each in `/etc/apk/world` as `name=version`) **[V]**, and the due list leaves them out on every platform.
The stage (section 5.2, step 1; Built) never names them to apt or apk (apt's hold stays as the installer placed it) and passes
dnf `--exclude=<name>` for each; the pin in `/etc/apk/world` is the apply's, since the stage changes nothing apk reads.

On a Mac the report also carries what an update needs (section 4.1): FileVault's state (`fdesetup status`, and `fdesetup
supportsauthrestart`), the free space of the data volume (`statvfs` of `/System/Volumes/Data`), the update account's state (present, an
administrator, holding a Secure Token: `sysadminctl -secureTokenStatus`), the Command Line Tools (receipt, files and version), and any
device-management setting of section 5.8 found on the Mac. `softwareupdate` has no JSON output, so each listing is parsed by one function
pinned to fixtures captured from every macOS release the farm runs; a line it does not know is counted in the report, never guessed at.

**Only what Apple offers now can be installed.** A macOS version is installable while the Mac's own `softwareupdate --list` (or
`--list-full-installers`, for a major upgrade) offers it; a superseded one answers "Update not found" (`--fetch-full-installer
--full-installer-version`). So what is due is always what the host itself lists, never a version typed in elsewhere. Apple's public
catalog of OS releases, `gdmf.apple.com/v2/pmv` (fields `ProductVersion`, `Build`, `PostingDate`, `ExpirationDate`, `SupportedDevices`,
read on 2026-11-19), lists several builds of one version for different models and its certificate chain is not publicly trusted; the host's
own list already resolves both, so the coordinator does not read it.

**Apple Business does not see what is due.** Its API (the Apple Business API, `api-business.apple.com`) gives, for a device in its own
built-in management (`GET /v1/mdmDevices/{id}/details`), `osVersion`, `isFileVaultEnabled`, `storageFreeCapacity` and
`lastCheckInDateTime`, and for any device of the organisation (`/v1/orgDevices`) its model and assignment: no available or pending
update. The device-management queries that would (`AvailableOSUpdates`, `OSUpdateStatus`, the declarative software-update status) run
between Apple's management service and the Mac, and the API does not pass them on. So the daemon's `softwareupdate --list`, which needs no
administrator, is the source of truth, as above.

### 1.1 How hostkeeper learns that a new Xcode exists

A host cannot see a new Xcode: `softwareupdate` does not list it. So the **coordinator** reads a public list of Xcode releases once a day
(New: a periodic job `xcode-feed`, claimed by due time like `alerts` and `prune`, Built: `store/postgres/periodic.rs`) and compares it with
what the store holds (section 2) and what each Mac reports installed (section 1).

- **The list:** the Xcode Releases site's `data.json`, a community list, not Apple's. Each entry carries the version (`27.2`), the build
  (`27B52`), whether it is a release, a beta or a release candidate, the macOS it requires, and Apple's download link. The coordinator takes
  only version, build, release type and required macOS from it, never a file. Apple's own developer releases RSS feed names the same
  version and build in each item's title; it is the fallback if the community list goes away (a second parser, written only then). The
  list's address is a flag (`--xcode-feed-url`; empty turns the job off).
- **What it decides:** a release (not a beta or a release candidate) newer than any the store holds is brought in by the source of section 2.0. When no fetcher is configured (`--xcode-fetcher` empty), it raises the warning alert `xcode_release_waiting`, which clears when the store holds the release (`hk xcode add`); with a fetcher, the fetcher fetches it and `xcode_signin_needed` is the alert when it cannot. Once the
  coordinator holds it, a Mac of a group that keeps the newest Xcodes (`xcode_keep`, section 3) and lacks it has `xcode` due, like any
  other due item.
- **Failure:** a fetch of the list that fails is a log line, retried the next day, never an alert.

### 1.2 The Command Line Tools on the build Macs

Each Mac the coordinator lists in the group `mac-build` reports the prerequisite `xcode-command-line-tools` `FAILED` with `softwareupdate
--list offered no Command Line Tools for Xcode`, several failures in a row, and raises `prerequisite_failed` (measured: the
coordinator's host and alert lists, 2026-11-19; nothing else on a Mac was reachable). By the order of `xcode_clt.rs` this means,
inferred and not measured, that on each Mac `pkgutil --pkg-info=com.apple.pkg.CLTools_Executables` exited non-zero (no receipt), the daemon made the marker
`/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress`, `softwareupdate --list` exited 0, and its standard output held no `*
Label: Command Line Tools for Xcode-<version>` line (Built: `xcode_clt.rs`, `install_label`). The cause is not confirmed; the candidates,
and the fact that tells each apart:

| # | Cause | Told by |
|---|---|---|
| C1 | The tools are on disk without their receipt, so `pkgutil` says absent and `softwareupdate` has nothing to offer | `/Library/Developer/CommandLineTools/usr/bin/clang` present, no `com.apple.pkg.CLTools_*` receipt |
| C2 | A full Xcode is installed and selected, and the Mac is offered no separate tools | `xcode-select -p` names an Xcode |
| C3 | A device-management deferral hides them: `softwareupdate.settings` `Deferrals.SystemPeriodInDays` "defer[s] system or non-OS updates ... updates only appear after the specified delay" | the key, in the report of section 1 |
| C4 | Apple's catalog has no tools for this macOS build | the macOS build, and a listing that offers a macOS update and no tools |
| C5 | The daemon's empty environment and closed standard input change what `softwareupdate` prints | the first line of both of its streams |
| C6 | The label's spelling changed, and the parser takes only `Command Line Tools for Xcode-<version>` | every `* Label:` line holding "Command Line Tools" |

**The fix** (New, in `xcode_clt.rs`, the first pull request, MU0 in section 8): the failure reason adds the macOS version and build, the
first line of each stream and every `* Label:` line holding "Command Line Tools", cut to the reason's bound, so the next failure on each
Mac names the cause with no shell; and the look counts the tools `PRESENT` when the receipt is there, or when the compiler file is there
and `xcode-select -p` exits 0 (C1), or when the selected developer directory is an Xcode whose licence is accepted and whose first launch
ran (C2: it carries the same compilers and SDKs). C3 is fixed at its source, the profile (section 5.8); C4 clears with the macOS update
this design brings; C6 is a parser fixture.

## 2. Xcode: where it comes from, and the install

The owner wants the latest Xcode, always, with as few hands as possible, and asked (2026-11-19) for the Apple Account's two-factor
sign-in to be taken out of the path if it can be. Section 2.0 gives the source: a push from the operator's own Mac. Section 2.1 (why
the coordinator cannot download) and 2.4 (the install) hold for any source; 2.2 describes the store, then the fetcher Mac, the
alternative source; 2.3 is the fetcher's sign-in.

### 2.0 The source: a push from the operator's own Mac

**The design (owner's direction, 2026-11-20).** The operator whose own Mac is already signed in to Apple's developer site brings each new
Xcode in with one command there, prompted by the warning alert `xcode_release_waiting` (New: the feed of section 1.1 lists a release the
store lacks).

**The Apple Account** (owner's direction, 2026-11-20) is the organisation's developer account, the one shared engineering account of the
organisation (its address is in the operator runbook, not in this public document), never a person's own, and no extra or dummy account
is made for this. Its two-factor codes go to the owner's phone. The push runs on the owner's own Mac, signed in to that account (with
`xcodes`, or in a browser on Apple's developer site), so a code is needed only when Apple ends that session. hostkeeper never holds the
account or its password: `xcodes` keeps the session in that Mac's Keychain. **The farm Macs need no Apple Account at all**: everything they
do with the archive (2.4: expand it, check Apple's signature, move it into place, accept the licence, run the first launch, `xcode-select`)
runs as root with no sign-in.

```
hk xcode add 27.2             # downloads with xcodes in the operator's own session, then uploads
hk xcode add ~/Downloads/Xcode_27.2.xip
```

`hk xcode add` (named `hk xcode-push` until [xcode-one-action.md](xcode-one-action.md) X3, and still a second name of it for one release) is, like `hk ssh`, not a gateway call: it is a verb of the `hk` program (Rust)
that calls `xcodes`, `pkgutil`, `xip`, `plutil` and `codesign` on the operator's Mac, hashes the archive itself and uploads it over its own HTTPS client
(Built: `crates/hostkeeper-hk/src/xcode_add/` and `src/xcode_push/`; on macOS **[V]**, section 10; off macOS its checks are made by a farm
Mac, `src/xcode_farm/`, xcode-one-action.md section 2.8), and `docs/cli.md`'s list of commands that
are not gateway calls gains it. The steps below are the first form of it; its checks, record, lock and progress are
[xcode-one-action.md](xcode-one-action.md) section 2. In order:

1. **Download**, when given a version: `xcodes download <version>` in the operator's own session (their Keychain, so a two-factor prompt
   only when Apple has ended the session, answered by the person at the keyboard). Given a file (downloaded from Apple's developer site
   in a browser), it takes the file.
2. **Check Apple's signature** on the archive, `pkgutil --check-signature`: it must exit 0 and name a chain that ends at Apple's root,
   signed by Apple itself **[V]**; anything else stops the push and uploads nothing.
3. **Read** the archive's version and build **[V]**, as 2.2 does, and compute its SHA-256 and size.
4. **Upload** it unmodified, streamed, to the coordinator's store: `PUT /v1/xcode/<sha256>?version=27.2&build=27B52` on the operator
   listener, with the operator's token as the bearer in the request's header and nowhere else (never an argument, an environment
   variable or an output line). The coordinator refuses a version and build the feed does not list or the store already holds, refuses a
   body whose SHA-256 is not its path's, and audits the upload.

The Macs then install it from the store in their own drained turn (2.4), where the daemon checks the size, the SHA-256, Apple's signature
(`pkgutil --check-signature`, then `xip --expand`) and the expanded app's `ProductBuildVersion` again. It needs no automatic login and no
Apple session on any farm Mac; the cost is one command by a person per release.

**From a box that is not a Mac (owner's direction, 2026-11-25).** The owner may download the archive in a browser on an operator's Linux
box, where `pkgutil`, `xip` and `plutil` do not exist. `hk xcode add` runs there too (Built,
`crates/hostkeeper-hk/src/xcode_farm/`; xcode-one-action.md section 2.8). The archive reaches the store through the same `PUT`,
with the version from the archive's name and the build the coordinator lists for that release (the alert's). Steps 2 and 3's checks
are then made by a farm Mac the operator names, in one `probe` run that names the stored archive: the Mac fetches it while it
serves, and checks it drained with the stage's own code (`pkgutil --check-signature` with the same rule as step 2, `xip --expand`,
the build, `codesign`). It installs nothing, and `hk` removes an archive that fails from the store. Every Mac's stage still makes
the same checks before it installs anything (2.4, Built), so an archive that was never checked is still never installed.
`docs/operations/mac-proof-owner-session.md` step 1 is this route.

**The alternative: the fetcher Mac (section 2.2).** A farm Mac holds the Apple session and logs its account in at boot; a person signs in
on it when Apple has ended the session. It is kept only for the case where no operator's Mac can push.

**Not used: Apple Business's Apps and Books.** A device-licensed App Store Xcode would have needed no Apple Account, but Xcode is not
offered in our Apple Business's Apps and Books (the owner's check in its console, 2026-11-19), and our own device-management service was
removed (2026-11-14).

**Automating the two-factor step is not an option.** We found no documented way for a program to complete an Apple Account's two-factor
sign-in (read 2026-11-19): the code goes to the account's trusted devices or phone number, for a person to read, and Apple's key-based
developer API (App Store Connect API keys) does not reach the developer downloads. A session made once by a person and reused by a
program (as fastlane's `spaceauth` does) still needs that person whenever Apple ends the session, which is what the push and the fetcher already
have. Reading codes off a phone or an SMS gateway by a program would break when Apple changes its sign-in, puts the second factor where
the first already is (defeating it), and uses Apple's sign-in in a way Apple does not offer to programs.

### 2.1 Can the coordinator download it? No

The coordinator runs on Linux. A `.xip` comes from Apple's developer site over plain HTTPS, but only with an Apple Account session, made by
a sign-in with two-factor authentication. The tool that does this well, `xcodes`, is macOS only (its package declares macOS and it keeps
the password in the macOS Keychain). Linux clients of Apple's developer sign-in exist (fastlane's, in Ruby) but follow undocumented Apple
behaviour that breaks when Apple changes its sign-in, and an Apple Account session on the coordinator would put a second credential on the
fleet's trust root. Expanding the archive needs macOS too (`xip`, and for newer releases Apple's encrypted archive format, section 2.3).
So the coordinator never downloads Xcode.

### 2.2 The Xcode store, and the fetcher Mac (the alternative)

The store below is where every archive goes; the fetcher Mac is used only if no operator's Mac pushes (2.0). One Mac is the **fetcher**, named by a coordinator flag (`--xcode-fetcher <host id>`). It is an ordinary farm Mac in its group; fetching
takes disk and network, not a drain.

1. When section 1.1 finds a new release, the coordinator tells the fetcher in its heartbeat answer (New: a `fetch_xcode` field naming the
   version and build, sent until the fetcher reports it held; section 4.1).
2. The fetcher's daemon runs `xcodes download <version> --directory <dir>` (download only: no expansion, no install) as the Mac's
   administrator account in that account's login session (`launchctl asuser`), where its Keychain and its Apple session are (a launchd
   daemon running as root cannot read a user's login Keychain) **[V]**. The fetcher therefore logs that account in at boot (automatic
   login); with nobody logged in, the fetch reports `signin_needed` with the reason `no_session`. It checks the archive's build against the release (`xip` lists it before it expands)
   **[V]**, computes its SHA-256 and size, and uploads it to the coordinator's **Xcode store** (`--xcode-dir`, files named by SHA-256).
   The store's routes are on the coordinator's host listener, under the host's enrollment certificate like the heartbeat (New):
   `PUT /v1/xcode/<sha256>`, accepted from the fetcher (host certificate) for the release it was told to fetch, or on the operator
   listener (operator token, audited, section 2.0) for a release the feed lists and the store lacks, at most
   `--xcode-max-bytes`; and `GET /v1/xcode/<sha256>`, answered with ranges so a broken copy resumes, only to a Mac of a group with
   `xcode_keep` above 0.
3. Every other Mac of a group that wants it fetches it from the store over the LAN, in the stage step of its own turn (section 5.2), and
   checks size and SHA-256 before it expands it. The fetcher installs its own copy the same way, in its own turn.

The store keeps the newest releases, as many as the largest `xcode_keep` of any group, plus any release a Mac reports installed, and
removes the rest. Nothing
of it is in a public bucket, a package repository or this repository. The owner ruled (2026-11-19) that the archive is kept exactly as
Apple ships it, in this private store, and installed only on our own Macs: never public, never offered to anyone else; nothing in it is
repackaged, patched or stripped. The steps after the install (`xcode-select`, the licence, the first launch, the platform import) configure
the installed app and change none of its files.

### 2.3 The alternative: the Apple Account sign-in on the fetcher

`xcodes` asks for the two-factor code at a terminal; nothing lets a program supply it (an open request upstream). So the one human step
is: **an operator signs in once on the fetcher Mac**, as its administrator account, in a terminal (`xcodes signin`), reached by the Mac's
Desktop page (Built: `ui/pages/desktop.js`) or SSH, with the Apple Account and the code from a trusted device. A security key cannot be
used; the Apple Account needs a code device.

**How often.** `xcodes` reuses its session for later downloads and asks again only when Apple has ended it. Apple publishes no lifetime;
reports range from days to weeks **[V]**, measured on the fetcher once built. A sign-in is needed only when a download is due and the
session has ended, so **at most once per Xcode release** (Apple ships one every few weeks), and only a download waits on it. When a
download fails because the session ended, the coordinator raises the warning alert `xcode_signin_needed` (New), naming the fetcher and
the release; the fetch stays pending and is tried again every hour, so after the operator signs in the next attempt downloads it and the
alert clears. Any other failure (network, a wrong build, disk) is retried later and later, from 30 minutes doubling to a day, like a
failed prerequisite (Built: `prerequisites.rs`). The Apple Account is the organisation's developer
account of section 2.0, never a person's own, and it is never stored by hostkeeper: `xcodes` keeps it in that Mac's Keychain.

### 2.4 The install (in the maintenance hook, section 5.2)

Xcode needs tens of GB free while it installs (the archive, its expansion and the Xcode kept beside it), so stage checks the free space
first and defers when it is short. Nothing reaches `/Applications/Xcode-<version>.app` before Apple's signature and the build are checked.
No step below needs an Apple Account: the daemon runs each as root.

1. **Stage, while the host serves:** fetch the archive from the store, check it, expand it into a staging directory on the same volume
   (`/Applications/.hostkeeper-staging/`) with `xip --expand`, which checks Apple's signature **[V]** (or, for an encrypted archive,
   `archiveutil` as `xcodes` does **[V]**: Apple is reported to ship releases after 27.3 in that format), then check the expanded app's
   `ProductBuildVersion` and its code signature (`codesign --verify`).
   Built (`crates/hostkeeper-daemon/src/maintenance_hook/xcode/stage.rs`): each entry of the order not installed with its build is
   staged in `/Applications/.hostkeeper-staging/<sha256>/`. The free space checked first is, per archive to fetch, what is left of it
   to fetch plus three times its size. The archive is read by `GET /v1/xcode/<sha256>` alone, resumed with a range after a broken
   connection (at most 8 connections) or from a copy an interrupted hook left, and refused before `xip` unless its size and SHA-256
   are the order's. Before `xip`, `pkgutil --check-signature` must report Apple's own signature with a chain ending at Apple Root CA,
   by its SHA-256 fingerprint, the rule of 2.0's step 2 (Built: `xcode/signature.rs`), so the Mac does not rest on `xip`'s check
   **[V]** or on the uploader's, which an archive uploaded from a box with no `pkgutil` never had (2.0); otherwise it defers
   `xcode_archive_not_apple`. The verified app ends at `<sha256>/Xcode.app`, the archive and the expansion removed. An encrypted
   archive is deferred until the Mac proof shows `archiveutil`'s use. Any failure defers and removes the staging directory.
2. **On the drained host:** move it to `/Applications/Xcode-<version>.app` (a rename on one volume), `xcodebuild -license accept`,
   `xcodebuild -runFirstLaunch` (it installs packages every Xcode shares, which is why the host is drained, [V]), then `xcode-select -s` to
   the group's default Xcode.
   Simulator platforms are exported once, on the Mac that downloaded the archive (the fetcher, or the operator's Mac, whose
   `xcode-push` then also uploads them **[V]**), with `xcodebuild -exportPlatform`, kept in the store beside the archive with
   their own SHA-256, and imported on each Mac with `xcodebuild -importPlatform` **[V]**; `-downloadPlatform`, which fetches from Apple
   on every Mac, fails too often to be a step of a run.
3. **Remove** each `Xcode-<version>.app` older than the group's newest `xcode_keep`, on the drained host. An Xcode whose directory is not
   `Xcode-<version>.app` (one a person or Apple Business put there) is reported (`InstalledXcode.foreign`, Built) and never removed.

`<version>` is `CFBundleShortVersionString` (`27.2`, `26.1.1`). A second build under a version the store already holds is refused
(`xcode_version_taken`) and alerted, so on our hosts a version names one build.

### 2.5 Shapes not chosen, or not yet

| Shape | Why not |
|---|---|
| The coordinator downloads | section 2.1 |
| Every Mac signs in and downloads its own | a sign-in with a code on every Mac, every time a session ends |
| A person downloads the `.xip` and uploads it to the store | chosen: section 2.0, `hk xcode-push` |
| Apple Business assigns the App Store's Xcode | not offered in our Apple Business's Apps and Books (section 2.0); it would also update one Xcode in place under running builds |

## 3. The group's settings

A host group's policy (Built: `hostgroups.proto` `RolloutPolicy`) gains an optional `maintenance` table (New, additive; `HostGroups.Put`
and `hk` edit it):

| Key | Default | Meaning |
|---|---|---|
| `xcode_keep` | 0 | how many of the newest Xcode releases the group's Macs keep (2: the latest and the one before); 0 leaves Xcode alone |
| `xcode_default` | `latest` | the one `xcode-select` points at: `latest`, or a version the group keeps |
| `macos_major` | `false` | whether a new major macOS counts as due |
| `linux_scope` | `security` | `security` (security updates and the kernel) or `all` |
| `batch` | 1 | hosts of the group in maintenance at once, unless a run says otherwise |
| `min_serving` | 1 | hosts of the group that must stay serving (section 5.1); a group of one host is taken only when the Schedule ticks "take the group's last host" |
| `drain_deadline_secs` | 0 (none) | how long a drain may wait before the host is put back as it was and the run moves on (section 5.3) |
| `return_secs` | 1200 (Linux), 3600 (Mac) | how long a rebooting host may be silent before `maintenance_not_back` |

## 4. Wire and UI

### 4.1 The report (New)

`HeartbeatRequest` gains `maintenance` (the next free field, optional), announced by the feature `maintenance-v1` and sent only to a
coordinator that advertises it, like `prerequisites` (Built). The daemon names `maintenance-v1` only once a reply advertised it, so the
coordinator offers it in its answer to every gRPC heartbeat that announces features, named or not (Built:
`crates/hostkeeper-coordinator/src/features.rs`, `OFFERED`); the report is kept only from a heartbeat that names it. It carries the host's facts (OS name, version and build, on a Mac its hardware serial, running and newest
installed kernel, the boot id the daemon already reads for its step record, Built: `hostkeeper_steps::ProcBootId` and
`hostkeeper_launchd::SysctlBootId`, and the boot time), the installed Xcodes (version, build, path, `xcode-select`'s, and `foreign` when its directory is not `Xcode-<version>.app`), on a Mac the update facts of section 1 (FileVault, free space, the update
account, the Command Line Tools, device-management conflicts), the due items of
section 1 (kind, from, to, needs reboot, label), when it last looked, how many listing lines no parser knew, why the last look failed (`look_failed`, Built: a failed look keeps the last good report), the state of the maintenance run it holds, and, on the fetcher, the
state of its fetch (release, `downloading`, `held`, `signin_needed` with its reason, or `failed` with its reason, and the next try).
`HeartbeatResponse` gains `fetch_xcode` (optional), sent only to a daemon that announced `maintenance-v1`; an older daemon never sees it. It is bounded under the
heartbeat's 16 KiB and stored for free in the host's last beat (Built: `fleet_hosts.beat`). Proto changes are additive, with an
`additions_maintenance.rs` test both ways (Built pattern: `crates/hostkeeper-proto/tests/additions_prerequisites.rs`).

### 4.2 The UI

- **Home** shows the fleet notice: the warning alert `updates_due` (New), one per group with due hosts ("Updates due on 4 hosts of
  `mac-build`"), linking the Maintenance page. It is a notice, not a page: it never pages, and stands while something is due.
- **Host page** shows the due items in its header ("kernel update, restart needed") and gains an **Updates** tab: the facts, each due
  item, the Xcodes, the last look, and the **Update** and **Schedule** buttons for this host.
- **Update** (one button, on the host page, on a host's row of the Maintenance page and on a group's notice): a run of everything due, on
  that host or every due host of the group, now, with the group's batch and drain settings. It asks once to confirm ("drains, updates,
  restarts if needed, puts back"), takes a reason, and calls `Maintenance.Schedule` like the form.
- **Maintenance page** (New, `ui/pages/maintenance.js`, in the Manage menu): per group, the hosts and what is due; the Xcode store, the release waiting for a push (with the `hk xcode add <version>` line to copy) and, if a fetcher is configured, its state (with "sign in on the fetcher" and a link to its Desktop
page when it needs one); the
  runs, each with its hosts and their step (waiting, staging, draining since, applying, rebooting, checking, putting back, done, deferred,
  halted), refreshed by `ctx.every`. A rebooting host links its **Console** (Linux, Serial over LAN, Built: `ui/pages/serialconsole.js`)
  or **Desktop** (Mac, Built: `ui/pages/desktop.js`; a Mac's screen returns only after it boots, so the page is opened again then).
- **Schedule** (a form on the host and Maintenance pages): what (the kinds that are due, all ticked), where (this host, a group, or the
  hosts ticked), when (**now**, or a **window** with a start and an end in the operator's local time, stored as Unix seconds), **batch**
  (the group's `batch` by default), drain deadline (the group's by default), and a reason. It calls `Maintenance.Schedule`.
- **A run's buttons:** **Cancel** (no new host starts; a host in its turn finishes), and per halted host **Retry** (from the failed step)
  and **Put back** (run the drained services' `resume`, leave the update as far as it got, and let the run go on). Nothing else: a pause is
  a Cancel and a new Schedule.

The operator API is a new service `hostkeeper.operator.v1.Maintenance` (`Schedule`, `List`, `Get`, `Cancel`, `RetryHost`, `PutBack`) and `Xcode` (`List`: the store, the release waiting, and with a fetcher its last attempt and whether it needs a sign-in; `Remove`), each audited, each in `ui/calls.json` and `hk`. The store's upload, `PUT /v1/xcode/<sha256>`, is a route of its own, not a method of these services; each upload is audited.

## 5. The run

### 5.1 Who orders it: the rollout gate (reused)

A run is stored by the coordinator (New: migration with `maintenance_runs` and `maintenance_hosts`, additive, older releases may run)
and decided where packages are: in `decide()`, per heartbeat, in one transaction under the rollout gate (Built:
`crates/hostkeeper-coordinator/src/decide.rs`, `store/decision.rs`). One rule is shared with packages, as it stands: **one thing at a
time on a host** (Built: `busy_elsewhere`). A host with a package order outstanding is not given maintenance, and a host in a run is
given no package order until its run ends there. The other rules are maintenance's own (New), because the package rules (halt,
exclusion, `min_up`) are kept per package and, before the last rule below, counted a cordoned host as up:

- **Serving count:** a host is not taken while fewer than `min_serving + 1` hosts of its group are serving. A host counts as serving when
  it is live and healthy and not between drain and done in a run, so a draining, rebooting or halted host never counts.
- **Window:** before the start or after the end, no host *starts*. A host already started finishes; a drain is never cut by a window.
- **Batch:** at most `batch` hosts of a group are between staging and done (default 1). The coordinator's rule for packages is one update
  at a time on a host and one service rolling at a time; maintenance keeps the first and, per group, adds the batch. Runs of different
  groups go on in parallel, so a run over three groups with batch 2 has up to six hosts in their turn.
- **Order of hosts:** by node id.
- **Skipped:** a host that is held or excluded for any package, a host whose daemon does not announce `maintenance-v1` (shown in the
  Schedule form, never ordered), a host whose group changes while it waits, and,
  for `macos`, a Mac that reports an `mdm_conflict` (5.8), or, while the path is `daemon`, a Mac with `no_update_account` (5.5); the
  `apple-business` path needs no update account.
- **Packages wait, not the reverse** (Built: `crates/hostkeeper-coordinator/src/stages.rs`, `live.rs`, `decide.rs`, `moving.rs`, the
  hosts in their turn kept in the fleet's marks by `holds.rs`): a host in its turn (from the order that starts it until it is done,
  deferred or put back) is told `busy_elsewhere` for every package. One that has not taken its place in a package's plan is left out of
  the hosts the plan counts for its stage, neither failed nor left to order, so a long drain does not hold a stage open; it gets its
  package updates after its turn. One already at the desired version keeps its place and counts as done, and one that failed the
  version stays failed. `min_up` counts it down. A maintenance run does not count against `--live-rollouts`, and a host in its turn
  bakes nothing that makes a rollout live. A host moving into a group is not admitted to a plan while in its turn. `HostGroups.Move` of
  a host in its turn is refused `host_in_maintenance` (New: not built).

The coordinator sends one order, `maintain` (New, a case of `Order.verb`, with an `order_id` and `try` like the others): the run, the
kinds, for `xcode` the store entries (version, build, size, SHA-256) and the versions to remove and the default, the Linux scope, the
drain deadline, and for `macos` the path (`install` or `wait`, section 5.5.2) and whether a major upgrade may be staged and installed
(`Maintain.macos_major`, the group's `macos_major`; false when not sent). The order names no version or label: the host stages what its
own listing offers (section 1, "Only what Apple offers now can be installed").

### 5.2 On the host: drain hooks and the maintenance hook

Per-service logic lives in the service's own hook, not in hostkeeper (AGENTS.md B17), so hostkeeper does not call js-agent itself. Two
changes keep it so:

- **Drain steps (New, hook contract, additive).** A package's manifest may name two more steps of its hook, `[hook] drain` and
  `[hook] resume`, as it names `[hook] boot` today (Built: `crates/hostkeeper-config/src/manifest.rs`). `drain` takes the service out of
  service and returns when it has no work; `resume` puts it back. Both are safe to run twice. A package that names neither is left alone.
  The conformance harness (Built: `hostkeeper-hook`'s `conformance` feature) gains checks for them.
- **The maintenance hook (Built: the command and its skeleton, `crates/hostkeeper-daemon/src/maintenance_hook.rs`; the `cutover`
  of `linux` and `reboot`, `crates/hostkeeper-daemon/src/maintenance_linux.rs`, and their `verify`,
  `crates/hostkeeper-daemon/src/maintenance_hook/linux.rs`; the `prepare` of `linux`, the stage of step 1,
  `crates/hostkeeper-daemon/src/maintenance_hook/linux/`; the other kinds' work and steps New).** The OS work is a hook too, a subcommand of hostkeeper's own binary, `hostkeeper maintenance-hook <prepare|cutover|verify>
  --params <file>`, launched the way the step runner launches a step (Built: `hostkeeper_steps` launchers: a transient systemd unit, a
  launchd job on a Mac, a process group under OpenRC). Its parameters are the `maintain` order's fields as JSON, in a file or on standard
  input (`--params -`), never in an argument. Each kind is the work of a `MaintenanceKind`, found by the host's platform; until a kind's
  work is written its step fails `unsupported_kind`. The stage and the install wait for a held package manager's lock for at most
  `--package-lock-wait-secs` (the daemon's flag, given to the hook too).

The step runner's sequence does not fit as it stands: it runs `rollback` after a failed `prepare`, `cutover` or `verify`, its reboot row
restarts a new release's units, and only `prepare` may declare no bound of its own (Built: `hostkeeper-hook` `next.rs`, `hostkeeper-steps`
`resume.rs` and `runner.rs`). An OS update has no release to restart and cannot be rolled back. So the daemon runs a `maintain` order by a
small sequencer of its own (Built: `crates/hostkeeper-daemon/src/maintenance_run.rs`, the order in `verbs/maintain.rs`), whose position is
kept in a durable, versioned run record beside the step records and fsynced before and after each step, so a crash, a reboot or a daemon
that restarts as a newer release resumes it. The order names no services, so the daemon drains the installed services whose release names
`drain` and `resume` in the order of their names; the group's package order needs a field of the order (additive) and is not built:

| # | Step | What runs | On failure |
|---|---|---|---|
| 1 | stage | maintenance hook `prepare`: check free space (twice the download, at least, from the package manager's own estimate), then download what can be downloaded (`softwareupdate --download`, `softwareupdate --fetch-full-installer` for a major upgrade, `apt-get --download-only` sized by `--print-uris`, `dnf upgrade --downloadonly` sized by `--assumeno`'s total, `apk fetch` into `/var/cache/hostkeeper/maintenance/apk` sized by `apk info --size` **[V]**, the Xcode archive copied from the store and expanded into staging). Built for Linux. Built for `macos` (`crates/hostkeeper-daemon/src/maintenance_hook/macos/stage.rs`), on the `install` path (on `wait` it runs no program): `sw_vers` and the Mac's own `softwareupdate --list` (and `--list-full-installers` only when the order's `macos_major` allows it); with an upgrade allowed and offered, the newest full installer of a higher major alone (the upgrade replaces the system, so no minor update is staged beside it), else every macOS update of the running major by its label; the data volume's free space at least twice the sizes the listings give; then `softwareupdate --download <label>` or `--fetch-full-installer --full-installer-version <version>`. Nothing offered, or a download answering `Update not found`, defers `macos_update_not_listed`; a short disk `free_space_short`, nothing downloaded | deferred: nothing was touched, the host never left service; the run goes on |
| 2 | drain | each installed service's `drain`, in the group's package order, unbounded unless the run sets a deadline (5.3). js-agent's cordons the host and waits for its leases to end | halt (5.4) |
| 3 | apply | maintenance hook `cutover`, which applies each kind of the run once (by the run record), macOS first (section 5.5). When a kind needs a reboot (`Action: restart`, `reboot-required`, a new kernel), the hook records that the kind's reboot is under way, then reboots (on a Mac, section 5.5: `softwareupdate --install ... --restart`; with FileVault on, `--install` then `fdesetup authrestart`; `startosinstall` for a major upgrade; or only a wait when device management installs it; `systemctl reboot`, or OpenRC's `reboot`, on Linux). After the reboot the daemon runs `cutover` again: it reads the record and the changed boot id, records the kind applied once it reads the new version, and goes on with the next kind, never installing the first again. A major upgrade on a Mac with FileVault on is not ordered until MP shows `startosinstall` restarts it authenticated | halt; `maintenance_not_back` when silent past `return_secs` |
| 4 | check | maintenance hook `verify` (section 5.6) | halt |
| 5 | put back | each drained service's `resume`, in reverse order. js-agent's re-reads its facts (the new Xcode set) and then uncordons | halt |
| 6 | done | the run record is closed; the next heartbeat's report shows nothing due of the kinds run | |

No step of this sequence runs `rollback`; a failure stops it where it is.

### 5.3 Drain: no deadline by default

Work is never cut to finish an update (`docs/design/hostkeeper-design.md`, goal 6). A `drain` step runs with no time bound
(`RuntimeMaxSec=infinity`, Built for `prepare`; for `drain` the launchers and `hostkeeper_steps::run_maintenance_step` are Built, and so is the sequencer that calls it, bounded by exactly the deadline when there is one, and reading a deadline that passed as exit 75) unless the run names `drain_deadline_secs`. When a deadline passes, the step exits 75 (deferred, Built
outcome): the daemon runs `resume` for every service it drained, the host is back in service with nothing changed, it is marked `deferred`,
and the run moves on to the next host; a deferred host is offered again when the run's other hosts are done, if the window is still open,
once its group's `defer_backoff` (Built) has passed since it deferred, longer for each deferral of its turn, under a new order id
(`maintain-<run id>-<n>`: a daemon answers an order it already holds with that order's end). Built in the coordinator
(`crates/hostkeeper-coordinator/src/decide_maintenance.rs`), as are the bake of section 5.6 and the ends of section 5.7.
Nothing stops a job.

js-agent's `drain` (New in komira-ai/jobscheduler, section 7) cordons with the host's own credential on the lease server's own-host route
(Built there: `POST /v1/hosts/{id}:cordon`, `GET /v1/hosts/{id}`, `crates/js-agent/src/own_host.rs`) and waits until the host reads idle,
the test its `js-agent cordon` verb already uses (no lease, not releasing, no mark; Built there: `crates/js-agent/src/cordon/`).

### 5.4 Failure: halt, stay cordoned, alert

A step from drain on that fails (exits non-zero other than a drain's deadline, or exceeds its bound) halts the host: no later step runs, so
no `resume` runs and **the host stays cordoned**. The run halts:
no further host starts in that group. The coordinator raises the critical alert `maintenance_halted` (New: an alert kind, Built machinery
in `alerts.rs`, logged once across coordinators), naming the host, the step and the hook's reason, and the Maintenance page shows Retry,
Put back and Cancel (section 4.2).

A cordon must outlive the hook that placed it: the own-host cordon is a lease of at most an hour that lapses unless renewed, and a reboot
or a halt leaves no hook to renew it. So js-agent's `drain` also writes a **hold file** in js-agent's state directory, and js-agent (New),
while the file is there: refuses to start any lease it is offered (so a lease placed between its hello and its cordon goes back to the
server's queue), cordons as soon as its hello has made the host known to the server (the own-host route answers `unknown-host` before
that), and renews the cordon. Only `resume` removes the file. Its precedent in js-agent is the taint marker, a file kept across restarts
(Built there: `crates/js-agent/src/lifecycle/taint.rs`). A halted host therefore stays cordoned across reboots and daemon restarts until an operator acts; a host whose
drain was never asked for is never held.

### 5.5 Apply

- **Linux.** apt: `apt-get install --only-upgrade` of the installed security packages and kernels (`linux_scope = security`; a
  package not yet installed is not due under it and comes in only as a dependency of one of them) or `apt-get dist-upgrade` (`all`),
  non-interactive, keeping existing configuration files; dnf: `dnf upgrade --security`, then a newer kernel by name, or `dnf upgrade`; apk: `apk upgrade`, under `security` the kernel alone (apk names no security origin). The package lock is
  waited for as the installers wait today (Built: `--package-lock-wait-secs`, also a flag of `maintenance-hook`).
- **macOS.** Apple silicon installs a macOS update or upgrade only with a *volume owner's* authorisation: an administrator holding a
  Secure Token. Root alone is not one, and the bootstrap token, which is one, can be redeemed only by a device-management service, not by
  a local daemon. Two paths follow, and one coordinator flag says which is live (section 5.5.2):
  - **The daemon installs it (live today; Built: `crates/hostkeeper-daemon/src/maintenance_hook/macos/apply.rs`,
    where the hook's `cutover` checks the update-account file, FileVault and the listings and keeps its choice in
    `macos-install.json` beside the run record, the hook records the reboot under way, and only then does the kind's own restart run
    the install; with FileVault on, as the FileVault paragraph below says).** A minor update: `softwareupdate --install <label> --agree-to-license --restart --user
    <account> --stdinpass`, with the label `--list` named, after the download step 1 made. A major upgrade (only when the group's
    `macos_major` allows it): step 1 fetches the installer, `softwareupdate --fetch-full-installer --full-installer-version <version>`,
    and step 3 runs that app's `startosinstall --agreetolicense --nointeraction --user <account> --stdinpass`, which restarts the Mac
    itself; the installer app is removed after the check. In both the password goes on the program's standard input.
  - **Device management installs it (when Apple Business can be asked, section 5.5.1).** The coordinator asks Apple Business to install
    the update on this one Mac once the drain step has reported it drained; the daemon's apply step runs no Apple program for `macos` and
    waits for the restart and the new version, then goes on with the next kind. No password is involved.

  **The update account** (owner's ruling, 2026-11-19): a dedicated local account on each Mac, `hkupdate`, an administrator holding a
  Secure Token, hidden (`IsHidden 1`, so it is not the login account the daemon picks for operator keys, Built: `login_account.rs`),
  with no login shell and not in `com.apple.access_ssh`. Its name and password are in a root-only file,
  `/Library/Application Support/hostkeeper/update-account` (root's, mode 0600; the daemon refuses another owner or mode). The password
  is not treated as a secret: no vault, no escrow machinery, the same on every Mac if the operator wants. It is still read only for the
  update call and passed only on the program's standard input (a new runner of the Built `Launchctl` port whose standard input is a pipe;
  the Built one gives `/dev/null`): never in arguments or the program's environment (which stays empty), a log line, a run record, a
  heartbeat or an error, whose reason names the step and the exit code only (Built pattern: `run_ok_named`).

  **Making the account, with its Secure Token.** A Secure Token is granted only by a holder of one, here the administrator made at
  Setup Assistant, once per Mac. The bootstrap's settings (New keys beside the farm values, `docs/design/mac-bootstrap.md`) carry the
  update account's name and password and that administrator's name and password; the bootstrap writes the update-account file (Built:
  `crates/hostkeeper-bootstrap/src/update_account.rs`, the keys `update_account_name`, `update_account_password`, `admin_name` and
  `admin_password`, all four or none), and writes no copy of the administrator's password: the daemon reads the two `admin_` keys from
  the bootstrap's settings source at the start that makes the account (mac-bootstrap.md section 4). The
  daemon, at its next start, makes the account if it is missing: `sysadminctl -addUser hkupdate -password - -admin`, then
  `sysadminctl -secureTokenOn hkupdate -password - -adminUser <admin> -adminPassword -`, the passwords on standard input **[V]** (if
  `sysadminctl` reads `-` only from a terminal, the daemon gives it a pseudo-terminal it owns, never an argument), then hides it. The
  administrator's password is used for that call and not kept. (Built: `crates/hostkeeper-daemon/src/update_account_make.rs`, at every
  start of a Mac's daemon: the passwords on a pipe, since MP-A2 has not run; the hide, `UserShell /usr/bin/false` and the removal from
  `com.apple.access_ssh` right after `-addUser` and before the token call, each only when not so already; the account's password, then
  the administrator's, in the order of the arguments; each `sysadminctl` call followed by a read that must show it took.) The report of section 1 says whether the account exists, is an
  administrator, holds a token and is hidden (Built: `maintenance_account.rs`, the name read from the update-account file); a Mac whose account lacks one is never ordered a macOS update (`no_update_account`, shown in the
  Schedule form).

  **FileVault.** With FileVault on, a restart stops at the pre-boot login until someone types a volume owner's password, unless it is an
  authenticated restart (`fdesetup authrestart`, which needs that password too). The report carries FileVault's state per Mac. On a Mac
  where it is on, the daemon installs without `--restart` and then restarts with `fdesetup authrestart -inputplist`, the account and
  password in a plist on standard input, and the update account must also be a FileVault user (`fdesetup list`) **[V]**; whether
  `startosinstall` makes an authenticated restart by itself is to be shown before a major upgrade runs on such a Mac **[V]**. A restart
  hostkeeper does not make (a power cut, a panic) still stops at the pre-boot login, so the build Macs should run with FileVault off
  (question 4, open). A Mac that reports FileVault on is not ordered `macos` until its update account is a FileVault user and `fdesetup
  supportsauthrestart` says yes. The macOS installer app of a major upgrade is removed by `verify` once it passes.
  (Built in the daemon: `crates/hostkeeper-daemon/src/maintenance_hook/macos/filevault.rs`. The `cutover` asks `fdesetup status`;
  off, nothing more. On, `fdesetup supportsauthrestart` must say `true` and `fdesetup list` must name the update account, and its
  password must go in a plist (UTF-8, no control character but a tab, neither U+FFFE nor U+FFFF), else `filevault_not_ready` before anything is listed; as it is
  for FileVault being turned on or off. The plan keeps `"authrestart": true`; the restart runs `softwareupdate --install <label>
  --agree-to-license --user <account> --stdinpass` and, once it exits 0, `fdesetup authrestart -inputplist`, the plist of `Username`
  and `Password` on its standard input alone, failing `filevault_authrestart_failed` by the step and exit code alone. A major upgrade
  chosen on such a Mac fails `filevault_not_ready` until MP-U8 shows `startosinstall` restarts it authenticated.)

  **Threat model.** The account is a local administrator, so its password is root on that Mac for anyone who learns it, and, if it is
  the same on every Mac, on the fleet. The owner accepted this (not treated as a secret); the file's 0600 mode and the hidden account with
  no shell and no SSH limit who can use it, nothing more. The bootstrap settings that hold it are never committed to a repository. What
  guards the update call is hostkeeper's own authorisation: it runs only for a `maintain` order, which the coordinator sends to an
  enrolled host over mutual TLS, for a run an authenticated operator scheduled (audited).

- **Xcode.** Section 2.4. Xcode alone needs no reboot.

When a run has both `macos` and `xcode`, macOS goes first (a new Xcode can need a newer macOS), then the reboot, then Xcode, in the same
host's turn (step 3).

#### 5.5.1 Can Apple Business install an update on one Mac? Not today

What the ideal path needs is an API call that, for one device, sends the device-management command `ScheduleOSUpdate` or sets the
declaration `com.apple.configuration.softwareupdate.enforcement.specific` (`TargetOSVersion`, `TargetBuildVersion`,
`TargetLocalDateTime`). The Apple Business API's published reference (read on 2026-11-19) has neither:

- no endpoint sends a device-management command of any kind; the device endpoints read (`orgDevices`, `mdmDevices`), assign a device to
  a management service, and read AppleCare and Activation Lock;
- a Configuration can be created or updated by the API only when its type is `CUSTOM_SETTING`, a `.mobileconfig` profile ("Only
  Configurations with type `CUSTOM_SETTING` can be created with the API"). Software-update enforcement is a declaration, not a profile
  payload, and the `SOFTWARE_UPDATE` Configuration type exists but is edited only in Apple Business's own pages;
- Blueprints take devices by API (`Add Devices to a Blueprint`, `Remove Devices from a Blueprint`).

The one route the API leaves: an operator makes, by hand in Apple Business, a Blueprint whose `SOFTWARE_UPDATE` Configuration enforces
the target version, and the coordinator adds the drained Mac to that Blueprint and removes it after. It is not chosen: the target version
is edited by hand for every release, the enforcement's moment is its deadline as Apple applies it, not the drained host's, and whether
that Configuration is the enforcement declaration is not shown **[V]**. The daemon path stays live.

#### 5.5.2 The switch

The coordinator's flag `--macos-update-via` names the live path: `daemon` (the default) or `apple-business`. With `apple-business` the
coordinator also takes `--apple-business-key-file` (the API's OAuth client key, a 0600 file) and, for a host whose drain step reported
drained in a run with `macos`, makes the API call for that host's serial and the target the host listed, audits it, and shows its answer
on the host's turn; a call that fails halts the host's turn like any failed step (section 5.4). The order to the daemon names the path, so
the daemon knows whether to install or to wait. In `wait`, the daemon's apply step waits for `update_requested` in the coordinator's
heartbeat answer (the coordinator learns "drained" from the run record in the host's heartbeat and makes the call then). If the Mac does
not report the target build within `return_secs` of the call, whether it restarted or not, the turn halts with `update_not_applied`
(the critical alert `maintenance_halted`) and the host stays cordoned. The switch needs no change on a Mac,
and the update account stays as the fallback: setting the flag back to `daemon` restores the live path for the next host. The
`apple-business` value is refused until the API call it needs exists and is built.

### 5.6 Check

Before `resume`, the maintenance hook's `verify` checks, and fails naming what it found:

- the host booted again when it had to (the boot id changed);
- what was due of the kinds run is no longer due: `sw_vers` reports the target build; `uname -r` is the newest installed kernel and no
  reboot is needed; each Xcode the order names is at `Xcode-<version>.app` with its build, `xcodebuild -version` runs from it, and its licence is
  accepted; `xcode-select -p` is the group's default;
- every service the host runs is healthy by its own health check (Built: the `health` of each `ServiceBeat`).

The coordinator then waits for the host's next healthy heartbeats, as many as the group's `bake_beats` (Built), before the next host of the
group starts.

### 5.7 How a host's turn ends, and failure modes

| End | Cordoned | Holds a batch place | Run in the group | Then |
|---|---|---|---|---|
| done | no | no | goes on | |
| deferred (stage failed, or the drain deadline passed) | no | no | goes on | offered again after the others, while the window is open |
| halted | yes | yes | halted: no new host | Retry, Put back, or Cancel (the host stays halted and cordoned until Put back) |
| not applied (`apple-business` path: no target build within `return_secs` of the call) | yes | yes | halted | Retry, Put back, or Cancel |
| not back (silent past `return_secs`) | yes (hold file) | yes | halted | its return resumes it; the alert clears when it reports |
| not reached (window ended, or Cancel) | no | no | ended | a new Schedule |

- **Coordinator restart.** Runs and hosts' turns are rows; a host's step is what its daemon's run record says, reported in each heartbeat.
  An order the daemon already holds is recognised by its `order_id` and not started twice (Built: the order and try of every order).
- **Daemon older than `maintenance-v1`.** Never ordered; shown as such in the Schedule form.
- **A host retired mid-run.** Its turn ends; its cordon is js-agent's to lapse with the host.
- **A full disk.** Step 1 checks space before it downloads and defers; it never fills the disk of a serving host.

### 5.8 What device management must not configure

On a Mac hostkeeper updates, Apple Business (or any device management) leaves software updates to the run. It must not set (key names
from Apple's published device-management schema):

- the declaration `com.apple.configuration.softwareupdate.enforcement.specific`: it forces an install and a restart at its deadline, in
  the middle of a job;
- in `com.apple.configuration.softwareupdate.settings`: `Deferrals` (any period hides updates from `softwareupdate --list`, the Command
  Line Tools of section 1.2 included), `AutomaticActions` that install (`InstallOSUpdates`, `InstallSecurityUpdate`), `Beta`
  enrollment;
- the restrictions (`com.apple.applicationaccess`) `forceDelayedSoftwareUpdates`, `forceDelayedMajorSoftwareUpdates`,
  `forceDelayedAppSoftwareUpdates`, `enforcedSoftwareUpdateDelay`, `enforcedSoftwareUpdateMajorOSDeferredInstallDelay`,
  `enforcedSoftwareUpdateMinorOSDeferredInstallDelay`, `enforcedSoftwareUpdateNonOSDeferredInstallDelay` (deprecated in macOS 26,
  removed in 27);
- in `com.apple.SoftwareUpdate`: `CatalogURL`, `AllowPreReleaseInstallation`, `AutomaticallyInstallMacOSUpdates` true,
  `restrict-software-update-require-admin-to-install`;
- an App Store assignment of Xcode (the App Store would update it in place under running builds), reported as `xcode-app-store` when an
  `/Applications/Xcode*.app` holds `Contents/_MASReceipt/receipt`.

These are fixed in Apple Business, at their source. The daemon reports each one it finds on the Mac (`/Library/Managed Preferences`,
and the declarations the Mac reports **[V]**) because the Mac is the only place that shows what reached it; the Schedule form shows a
host with one as `mdm_conflict`, and it is not ordered `macos` while one stands.

Built (`crates/hostkeeper-daemon/src/maintenance_mdm.rs`): the daemon reads the top-level files of `/Library/Managed Preferences`
(`com.apple.applicationaccess.plist` and `com.apple.SoftwareUpdate.plist`, a boolean key reported when true, any other when set) and,
for the declarations, `/var/db/softwareupdate/SoftwareUpdateDDMStatePersistence.plist`, which Apple does not document **[V]**: an
enforced target (`TargetOSVersion` or `TargetBuildVersion` anywhere in it) is reported as the `enforcement.specific` declaration, and
in its `SUCorePersistedStatePolicyFields` / `SUCoreDDMDeclarationGlobalSettings` a key naming a deferral as `Deferrals`,
`automaticallyInstallOSUpdates` or `automaticallyInstallSystemAndSecurityUpdates` as `AutomaticActions`, and a key naming a beta as
`Beta`. A file there that cannot be read fails the look (`look_failed`), never reads as a clean Mac. The Mac proof (MP) shows how each
setting appears on a Mac and corrects these keys.

**The Mac's own setting (decided 2026-11-25).** Without any device management, macOS keeps `AutomaticallyInstallMacOSUpdates` in the
Mac's own `/Library/Preferences/com.apple.SoftwareUpdate.plist`, and sets it to 1 by default; the farm Macs have it so (10.1.2). On, it
installs a macOS update and restarts the Mac without a drain, which this section rules out. It is not a conflict to report, since no
one sets it on purpose: a Mac's daemon turns it off at each start, the way it keeps the Mac awake (Built:
`crates/hostkeeper-daemon/src/auto_install.rs`; `docs/macos.md`, "macOS installs no update by itself"), and `defaults read` of the key
(the file is readable without root) shows it. The other keys of that file (`AutomaticDownload`, `AutomaticCheckEnabled`, `CriticalUpdateInstall`) install no macOS update
and stay as macOS sets them; whether a security response that `CriticalUpdateInstall` lets in ever restarts a Mac is not shown.

## 6. Builds stay correct across an Xcode change

- **Side by side.** A new Xcode is a new directory. An Xcode a build uses is never replaced under it and one is removed only on a drained
  host, when it falls out of the group's newest `xcode_keep`.
- **The Xcode is a platform property, by build.** js-agent advertises the installed Xcodes as the capability `xcode`. Its built frame
  field carries build ids (Built there: `caps.xcode` of `js-agent-wire`, `16C5032a`), while its design names versions (komira-ai/jobscheduler
  `docs/design/capabilities.md`; its step 38 builds the probe). This design asks for the **build**: it names one Xcode exactly (and a second
  build under one version is refused at the store, section 2.4). A consumer's action states the Xcode build it needs as a platform property; a remote-execution action key includes its
  platform properties, so a result built with one Xcode has a different key from the same action with another, and is never served for it.
  js-agent places such an action only on a host that has that Xcode, and selects it per action (`DEVELOPER_DIR`), never through the global
  `xcode-select`.
- **No action without an Xcode.** An action on a Mac that names no Xcode would run against `xcode-select`'s default, which a run can move,
  and its cached result would then be served across the change. So the consumer's table (kbf's, New there) refuses a Mac action whose
  platform properties name no `xcode`, and the change of a group's `xcode_default` is shown in the Schedule form as "moves the default".
- **The facts move only while drained.** js-agent re-reads its facts in `resume`, before it uncordons, so the first lease after a run sees
  the new set.

## 7. Work outside this repository

| Repository | Change | Why |
|---|---|---|
| komira-ai/jobscheduler | js-agent `update-hook drain` and `resume`, and the hold file held across restarts (5.3, 5.4) | the cordon is js-agent's, through its own-host route |
| komira-ai/jobscheduler | the `xcode` capability probe (its capabilities.md step 38), by build id, re-read in `resume` (6) | the Xcode as a platform property |
| komira-ai/kbf | the consumer table maps `xcode` (a build) and refuses a Mac action without it (6); on hosts hostkeeper manages, the Mac profile of `kbf-mac-provision` drops its `xcode` and `default_developer_dir` items, which would remove an Xcode it does not pin | one owner for the Xcodes on a host |

## 8. Implementation pull requests

Each is one pull request in this repository unless the table says otherwise, with its tests and the documents it changes.

| # | Pull request | After |
|---|---|---|
| MU0 | Daemon: the Command Line Tools failure reason and look of section 1.2 (`xcode_clt.rs` alone), so every Mac names its cause | |
| MU1 | Wire: `HeartbeatRequest.maintenance` and `maintenance-v1`, `Order.maintain`, the `maintenance` group policy, the `Maintenance` and `Xcode` operator services, the alert kinds `updates_due`, `xcode_release_waiting`, `maintenance_halted`, `maintenance_not_back` (the heartbeat answer's `fetch_xcode`, the fetcher's report of it and `xcode_signin_needed` only if question 2 chooses the fetcher); `additions_maintenance.rs`; `docs/cli.md` and the UI client regenerated | |
| MU2 | Daemon: the facts and due checks of section 1 on all three platforms (on a Mac with FileVault, free space, the update account, the Command Line Tools, the hardware serial and the `mdm_conflict` keys of 5.8), hourly and after a start, reported; fake programs in tests | MU1 |
| MU3 | Coordinator and UI, read only: the report kept and shown (`Hosts.Get`, the Updates tab), `updates_due`, the Maintenance page's due list | MU2 |
| MU4 | Hook contract: `[hook] drain` and `resume` in the manifest and the release manifest, the step runner running them, the conformance checks | MU1 |
| MU5 | Daemon: the `maintain` order, the run record, the maintenance hook for Linux (apt, dnf, apk, reboot), stage-drain-apply-reboot-check-put back across a reboot | MU2, MU4 |
| MU6 | Coordinator: runs, the migration, `Schedule`, `Cancel`, `RetryHost`, `PutBack`, the rules of 5.1 in `decide()` (serving count, window, batch, packages leaving a host in its turn out of a stage), `maintenance_halted`, `maintenance_not_back` and silence while rebooting, `--macos-update-via` accepting only `daemon` and the order's path field (no `silent_host` before `return_secs`); the Update button, the Schedule form and the run view | MU3, MU5 |
| MU7 | Xcode: the `xcode-feed` job and `xcode_release_waiting`, the store (`--xcode-dir`) and its route for enrolled Macs, `hk xcode-push` and the operator `PUT` route (2.0), the hook's install and removal (2.4); the fetcher (`--xcode-fetcher`, `fetch_xcode`, `xcode_signin_needed`) only if question 2 chooses it | MU6, MU8 (a run with both orders macOS first), MP, J2 and K1 deployed |
| MU8 | macOS: `softwareupdate` and `startosinstall` in the maintenance hook with the update account file and the standard-input runner (5.5), the account and its Secure Token made at start, FileVault's authenticated restart, the bootstrap's keys | MU6, MP |
| MP | The Mac proof, on one drained Mac in a group of its own, each a **[V]** of 5.5, 2.0, 2.4 and 5.8: `xcode-push`'s signature check and version read on a real archive; a minor update and a major upgrade as root from launchd with nobody logged in, each with FileVault off and on; whether an install on macOS 26 or later stops on a step that needs the screen; `sysadminctl` with passwords on a pipe and the Secure Token grant; an Xcode install, a swap and a removal with js-agent probing before and after; how each setting of 5.8 shows on the Mac. Results go into this document | MU5 |
| MU9 | `docs/operations/maintenance.md`: the runbook (the Xcode download, scheduling, a halted host) | MU7, MU8 |
| J1 | komira-ai/jobscheduler: js-agent `drain`, `resume`, the hold file | MU4 |
| J2 | komira-ai/jobscheduler: the `xcode` probe re-read in `resume` | J1 |
| K1 | komira-ai/kbf: `xcode` required on a Mac action; `kbf-mac-provision` without Xcode items on hostkeeper hosts | J2 |

MU1 lands first (interfaces before code). MU2 and MU4 go in parallel; Linux maintenance (MU5, MU6) works end to end before any Mac work,
with J1 deployed: the one-button Linux update (a kernel or security update, or a restart) is the first thing the build delivers. MU8
follows. The `apple-business` value of `--macos-update-via`, its key flag and the API call are not built or scheduled: they are written
when Apple Business offers the call (section 5.5.1), and until then the flag accepts only `daemon`. MU7 waits for J2 and K1, because without them an Xcode change could serve a cached result across Xcodes (section 6), and for the
push of section 2.0; the fetcher is built only if question 2 asks for it.

**Tests the build must prove**, each over fakes (fake programs answering `softwareupdate`, `apt-get`, `dnf`, `apk`, `xip`, `xcodebuild`;
a fake hook; a clock the test sets; injected heartbeats), none timing anything: a due item appears and clears; a host in a package update
is given no maintenance and the reverse; the window starts no host before or after it, and never cuts one that started; a batch of N
never has N + 1 hosts between staging and done in a group; no host is taken when fewer than `min_serving + 1` hosts serve, a draining or
halted host never counting; a group of one is taken only when the Schedule says so; a failed step runs no `resume` and no `rollback`, and
the hold file keeps the host cordoned across a restart; js-agent with a hold file refuses a lease before its cordon is placed; a drain
deadline puts the host back unchanged; a `reboot`-only run (a kernel installed but not running) installs nothing and reboots; Update starts
the same run as a Schedule of everything due, now; the update account file of another owner or mode is refused, and its password reaches the program only on its standard input (the
argument list and the empty environment checked) and never appears in a log line, a run record, a heartbeat or an error (each asserted by
exact equality); a Mac whose update account holds no Secure Token, or that reports an `mdm_conflict`, is never ordered `macos`; with
FileVault on the install runs without `--restart` and then `fdesetup authrestart` with the plist on standard input; a major upgrade
fetches the full installer at stage and runs `startosinstall`; with `--macos-update-via apple-business` the daemon runs no Apple program
for `macos` and waits for the new version, and the value is refused while its API call is not built; the Command Line Tools reason names
the macOS build, both streams' first lines and the tools' label lines, and the look counts tools on disk without a receipt, or a ready
selected Xcode, as present; a full disk defers at stage; a macOS-and-Xcode run reboots after macOS and installs Xcode after the
reboot; `verify` fails on each wrong fact; a host silent past `return_secs` raises `maintenance_not_back` and not before; Put back and
Retry do what section 4.2 says; a host in its turn is left out of a package stage and given no package order; an older daemon is sent no
`maintain` order; a coordinator restart does not start an order twice; the feed's beta and release-candidate entries are never fetched, and
a release is fetched once; a fetch refused for an ended session raises `xcode_signin_needed`, and the next attempt after a sign-in
clears it; a second build under a held version is refused; a Mac takes the archive only when its size and SHA-256 match the store's; `xcode` becomes due on a Mac only once the store holds the
release; the store keeps the largest `xcode_keep` newest releases plus the installed ones and removes the others; the host-certificate `PUT` is
refused from any host but the fetcher and for any release it was not told to fetch; the operator `PUT` is refused with a host
certificate, with no token, and for a version and build the feed does not list; a failed read of the feed is a log line and no alert;
`fetch_xcode` stops once the fetcher reports the release held; `xcode-push` uploads only a release the feed lists and the store lacks,
and a body whose SHA-256 differs from its path is refused and nothing kept; `hk xcode-push` puts the token only in the request's header, in no
argument and no environment variable, and uploads nothing when the signature check fails (Built: `crates/hostkeeper-hk/src/xcode_push/tests.rs`); with no fetcher,
`xcode_release_waiting` stands from the feed's new release until the store holds it.

The Linux path is proven end to end in process (Built: `crates/hostkeeper-daemon/tests/maintenance_linux_e2e.rs`, over
`tests/support_e2e` and `tests/support_linux`): the real coordinator schedules and orders each turn from the hosts' heartbeats, and on each host the real
daemon runs it from its run record through the real maintenance hook's `prepare`, `cutover` and `verify` over fake `apt-get`,
`systemctl`, a fake service that declares `drain` and `resume`, and a fake boot id. It shows the Update button's turn (stage, drain,
install, reboot, check in the new boot, resume, done, nothing due after, `updates_due` cleared), the same turn for a host that has a
`linux` and a `reboot` item due (one reboot), a batch of one over a group of three inside a window, a failed apply that halts the
host cordoned with no resume and no rollback until Put back, a coordinator and a daemon that restart mid-run without starting the
order twice, a `reboot`-only run that installs nothing, and a drain deadline that puts the host back unchanged and defers it. The
`prepare` of `reboot` is the real kind's there (it succeeds and runs nothing: a restart has nothing to download); the `linux`
kind's stage is answered by the test, and its programs are tested in the hook's own crate (`maintenance_hook/linux/tests.rs`,
`stage_tests.rs`), where the hook's `prepare` of `reboot` alone and of `linux` with `reboot` is run too.

The Mac path is proven end to end in process the same way (Built: `crates/hostkeeper-daemon/tests/maintenance_macos_e2e.rs`, over
`tests/support_e2e` and `tests/support_mac`): the real coordinator, with its Xcode store, orders each turn from the heartbeats of Macs
that name the platform `darwin-arm64`, and on each Mac the real daemon runs it from its run record through the real maintenance hook's
`macos` and `xcode` kinds (their stage and their apply) and the real Mac collectors, over fake `sw_vers`, `ioreg`, `softwareupdate`,
`fdesetup`, `pkgutil`, `dscl`, `sysadminctl`, `startosinstall`, `xip`, `codesign`, `xcodebuild` and `xcode-select`, the update
account's file (mode 0600) and a boot id that changes at the Mac's restart. It shows the Update button's turn with FileVault off
(stage with the download, drain, `softwareupdate --install ... --restart` with the password on standard input alone and in no
argument, log line, heartbeat, run record or halt reason, restart, check of the new build, resume, done, nothing due after,
`updates_due` cleared); a Mac whose FileVault was turned on after the report it was ordered on, installed without `--restart` and then
restarted by `fdesetup authrestart -inputplist` with the property list on standard input; a major upgrade that fetches the full
installer at stage, runs `startosinstall` and removes the installer app at the check; a macOS and Xcode turn that restarts after macOS
and only then installs the store's Xcode (fetched, expanded and checked at stage while the Mac serves), selects the order's default
and removes the old Xcode beyond `xcode_keep`; a Mac with no update account, one whose account holds no Secure Token, one with an
`mdm_conflict` and one that reports FileVault on, each skipped for `macos` with its `skipped_because` while the run goes on; a full
disk, and a store archive `xip` cannot expand, each deferring at stage with nothing drained or installed; a failed install and a check
that finds the wrong build, each halting the Mac cordoned with no resume and no rollback until Put back; a Mac silent past its
`return_secs` that the job `alerts` notes `maintenance_not_back`, and not a second before; and a coordinator and a daemon that restart
mid-turn without starting the order twice or installing twice.

## 9. Questions for the owner

1. **How a Mac authorises a macOS update. Decided (2026-11-19):** device management after the drain is the ideal, but Apple Business's
   API cannot send the update to one device (5.5.1); until it can, the daemon installs with the dedicated update account's password on
   standard input (5.5), and `--macos-update-via` switches (5.5.2). Open: nothing.
2. **Xcode: where it comes from. Decided (2026-11-20):** `hk xcode-push` from the operator's own Mac, prompted by
   `xcode_release_waiting` (section 2.0); Apple Business's Apps and Books does not offer Xcode, and the two-factor step cannot be
   automated. The store is decided (2026-11-19): the archive unmodified, private, installed only on our Macs (section 2.2). Open: whether
   the fetcher Mac is wanted as well (it is not built unless asked for).
3. **`linux_scope` default.** Kept: `security` (security updates and the kernel); `all` is one setting away.
4. **FileVault on the build Macs.** Recommended: off (section 5.5, FileVault): with it on, every restart hostkeeper does not make stops
   at the pre-boot login until a person types a password. The report shows each Mac's state before any run.

## 10. Proof results

This section records the Mac proof (MP of section 8): for each **[V]** of sections 2.0, 2.4, 5.5 and 5.8, the command that settles it,
the macOS version it ran on, what it showed and the decision that forces. The commands, in the order they are run, with what to record
and how to undo each, are in `docs/operations/mac-proof.md` (its step ids are cited below). What an operator does with what is built (the Xcode push, the update account, FileVault, the
settings of 5.8, a Mac that halts) is in `docs/operations/maintenance.md`, "The Mac: macOS and Xcode"; a result here that changes
an operator's step changes that page too. A result is written here only when it was
seen on a Mac; what Apple documents, or what a reading of the code shows, is said as such.

### 10.1 What could be shown on 2026-11-20

Read again on 2026-11-24: section 10.1.1 says what has changed since. This section is kept as it was read.

Read from the coordinator's operator API (`Hosts.List` and `Hosts.Get`, as an operator, read only):

- **No farm Mac can be given a shell yet.** All four Macs run hostkeeper 0.1.398, announce `operator-keys-v1`, and report an empty
  operator keys block (`heartbeat.operatorKeys` is `{}`), so `hk ssh` has no key to log in with on any of them. Every step of the proof
  needs a root shell on the Mac, from the owner at the Mac or after the proof group is named in the coordinator's operator-keys files
  (`mac-proof.md` MP-E2 to MP-E4).
- **No Mac is drained or in a group of its own.** All four are in `mac-build`. js-agent is not a hostkeeper package on the Macs
  (`services[js-agent].membership` is `MEMBERSHIP_MISSING`: `kbf-mac-provision` installs it), so moving a Mac to a new group does not take
  it out of builds; only a js-agent cordon does (MP-E6 as it read then; 10.1.1 says what holds now).
- **No Mac reports the maintenance facts yet.** No host of the fleet sends `heartbeat.maintenance`, so the reads already built for 5.5 and
  5.8 (FileVault, `supportsauthrestart`, the update account, the device-management keys) have no answer from a Mac yet. They will come
  without a shell once the Macs and the coordinator run a release with `maintenance-v1`: that answer is the 5.8 read as the daemon makes it
  (root, launchd), which MP-M also needs.
- **The daemon, root under launchd, cannot open `TCC.db`**: its Remote Login probe reports `Operation not permitted`, which looks like
  missing Full Disk Access. Whether `/var/db/softwareupdate/SoftwareUpdateDDMStatePersistence.plist` (5.8) is behind the same protection is not shown;
  if it is, the built read reports `look_failed` on every Mac and never a clean one (MP-M6 decides).
- **The Macs' Command Line Tools differ**: `27.2.0.0.1792721180` on two Macs, `26.6.0.0.1781586589` on the other two (the `prerequisites`
  report). Their macOS versions are not reported yet, so which Mac has a minor update or an older major waiting is not known.

From this repository's release pipeline (the job `macos-device` of `release.yml`, on a GitHub-hosted macOS 15.8.2 VM, build 24H232):
a step run as a launchd job and the daemon under launchd work as root; who was logged in is not recorded there. That VM is not a farm Mac and has
no Secure Token holder, FileVault or update to install, so it settles none of the **[V]** below.

### 10.1.1 The ready state, read on 2026-11-24

Read the same way (`Hosts.List`, `Hosts.Get`, `HostGroups.Get`, and `Ssh.HostKeys` for each Mac, as an operator, read only), against
the ready state of `mac-proof.md` section 1 ("The ready state, in order"):

- **Every Mac reports the maintenance facts.** All four Macs and the coordinator run 0.1.447. Each Mac announces `maintenance-v1` beside
  `operator-keys-v1` and `prerequisites-v1`, so no release or roll is needed for the report. Each Mac's `maintenance`, a field of the
  host beside its heartbeat, reads the same on all four: macOS 27.2.1, build 26B131; FileVault off; `authrestartSupported` true; no
  update account; the Command Line Tools 27.2.0.0.1792721180 with receipt and files (on all four now); nothing `due`; no
  `mdmConflicts`; no Xcode in `/Applications`; `lookFailed` empty. The data volume has about 3.9 TB free on two Macs and about 0.9 TB on
  the other two.
- **The look does not fail for want of Full Disk Access.** `fullDiskAccess` is `FULL_DISK_ACCESS_NOT_GRANTED` on all four, and the Remote
  Login probe still cannot open `TCC.db`, yet every look succeeds. The built read takes only a missing file as absent and fails the
  look on any other error of the open (`maintenance_plist.rs`). So the DDM state file of 5.8 and the two managed-preference files are
  readable to the daemon or absent; which of the two, the heartbeat cannot say, and MP-M6 answers it from a root shell. The Macs run the
  Developer ID signed build (the probe's `codesign` step); the profile of `docs/operations/mac-full-disk-access.md` (its step 2, the
  owner's) is not installed. Nothing in the ready state needs the grant.
- **A Mac's report has no boot id, boot time or running kernel.** Only the Linux collector fills `boot_id`, `boot_unix` and
  `running_kernel` (`maintenance_host.rs`). The five Mac collectors fill none of them, though section 4.1 says the report carries them
  and names `SysctlBootId` for the Mac. A maintenance run is not affected: it reads the boot id from the system (`maintenance_run.rs`).
  An operator reading the report cannot see a Mac's restart in it. To be fixed in the daemon.
  Built since: a sixth Mac collector, `boot` (`maintenance_boot.rs`), fills `boot_id` from `kern.bootsessionuuid` through
  `SysctlBootId` and `boot_unix` from the `sec` of `sysctl -n kern.boottime`; a boot fact that cannot be read leaves its field
  empty and does not fail the look. `running_kernel` stays empty on a Mac, a Linux notion. Shown by tests over a scripted `sysctl`
  only; that a farm Mac reports both, and that they change across a restart, is still for the Mac proof (`mac-proof.md`, MP-E1 and
  MP-E5).
- **No Mac can be given a shell yet, for a reason 10.1 did not name.** The coordinator holds no SSH user CA. `Ssh.HostKeys` for a Mac
  answers `not_configured`, no host announces `ssh-v1`, and none has an `ssh` report. `hk ssh` needs that CA and the SSH operators file
  (MP-E2 (a) and (b)), not the operator-keys file. Every host of the fleet, Linux ones too, reports `operatorKeys` as `{}`: the
  coordinator was given no operator-keys file. The host-prerequisites file is in force (each Mac reports the Command Line Tools present).
- **js-agent is now a hostkeeper package on the Macs.** The Macs' group lists it with its configuration packages (`MEMBERSHIP_PRESENT`,
  healthy). A group move still does not take a Mac out of builds, because the new group lists js-agent too. The job scheduler's operator cordon
  does (MP-E6).
- **No Mac is drained or in a group of its own**, as on 2026-11-20. Which Mac takes the fewest builds is not in hostkeeper's reports;
  the job scheduler's `js usage` answers it (MP-E1).

What is left for the ready state is the operator's and the coordinator deploy owner's: the SSH CA and the files, the cordon, the move
and the pin. None of these installs, updates or removes a package. The SSH CA has the daemon of every host that announces `ssh-v1`
set its sshd up to trust it, the Linux hosts included, and the move has the proof Mac's daemon write the operators' keys
(`mac-proof.md`, "The ready state, in order").

### 10.1.2 The first root reads on a farm Mac, 2026-11-25

Run by an operator with `hk` 0.1.408 against the coordinator at 0.1.462: `hk ssh` to one farm Mac (daemon 0.1.462) as its
administrator, then `sudo -n /bin/bash -s` with each step's commands on standard input. The output was kept on the operator's side;
nothing was written on the Mac. `sw_vers`: macOS 27.2.1, build 26B131, before and after every step. Nobody was logged in: the console
owner was `root`, `who` empty, no `autoLoginUser` and no `/etc/kcpassword`, so MP-E7 had nothing to log out. The Mac took no job
while the reads ran (no js-agent child, `xcodebuild`, `xctest` or `simctl`), but was not cordoned, so only reads were made: no
launchd job, no account, no install. What each read showed, and what it decides:

- **A root shell needs no password (MP-E4).** `sudo -n true` exits 0 for the administrator. The update path does not rely on it,
  as the step says; it is reported to the owner. The administrator's password is still the owner's for the Secure Token grant
  (MP-A3, `-adminPassword -`), not for the shell.
- **The coordinator's reading matches the Mac (MP-E5).** Each pair agrees: `sw_vers` and `osVersion`/`osBuild`; `fdesetup status`
  (`FileVault is Off.`, exit 0) and `filevault` off; `fdesetup supportsauthrestart` and `authrestartSupported`; no update account
  and `updateAccount` empty; `softwareupdate --list` (`No new software available.`, exit 0) and nothing `due`; neither managed
  file nor the DDM state file present, and no `mdmConflicts`. `fdesetup list -extended` and `diskutil apfs listUsers /` show two
  volume owners: the administrator and the MDM Bootstrap Token. Decides: no collector change.
- **`dscl` and `sysadminctl` answer as the collector reads them (MP-A1).** `dscl . -read` of a missing user exits 56 with
  `<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)` on standard error: `RECORD_NOT_FOUND` stays and its **[V]** is dropped.
  `sysadminctl -secureTokenStatus` exits 0 and writes its one line to standard error only, after a time stamp and its process,
  naming the account by its full name (`RealName`), not its short name: `Secure token is ENABLED for user <full name>`. For an
  unknown user it also exits 0, with `Unknown user <name>`. Decides: `TOKEN_ENABLED` and `TOKEN_DISABLED` stay, and both streams
  stay read (`maintenance_account.rs` says which one carries it). The administrator holds a token, so MP-A3's grant can work.
- **Defect: macOS 27 writes a hidden account's flag with a prefix the daemon did not read.** For an account that is hidden,
  `dscl . -read <user> RecordName IsHidden` writes `dsAttrTypeNative:IsHidden: 1` (the same with `IsHidden` alone, and the key is
  `dsAttrTypeNative:IsHidden` in `-plist` output); for one that is not, `No such key: IsHidden` on standard error. The daemon read
  only a line starting `IsHidden:` (`dscl_value` in `login_account.rs`), so it took every hidden account for a visible one. Once
  the update account of 5.5 exists, hidden and an administrator, the login-account rule would have counted two visible
  administrators: `ssh.state` `no_login_user` and the operator keys refused on that Mac, and the account-making start would have
  hidden the account again at every start. Fixed in the same change: `dscl_value` reads a key with or without the
  `dsAttrTypeNative:` prefix, shown by tests over the text read here (`login_account_tests.rs`, `maintenance_account_tests.rs`,
  and the account-making start's fake Mac, which now writes it so).
- **`fdesetup` as root from a shell (MP-A6, the shell half).** With FileVault off, `fdesetup status` writes `FileVault is Off.` and
  `fdesetup supportsauthrestart` writes `true` and a newline, each exit 0 with nothing on standard error. Decides:
  `maintenance_mac.rs` unchanged; `true` with FileVault off means the gate reads the FileVault state first, not this alone, as
  the coordinator's gate does (`crates/hostkeeper-coordinator/src/maintenance_mac.rs`). The same from launchd, and both with
  FileVault on, are still to run.
- **The device-management baseline (MP-M1).** `/Library/Managed Preferences` holds neither `com.apple.applicationaccess.plist`
  nor `com.apple.SoftwareUpdate.plist`, only hostkeeper's configuration file, a privacy (TCC) policy for js-agent and a per-user
  directory; the DDM state file is absent; the Mac is enrolled through Automated Device Enrollment, user approved, and
  `profiles show` works as root. Decides: no change (an absent file adds nothing). Seen beside it: the Mac's own
  `/Library/Preferences/com.apple.SoftwareUpdate.plist`, which no device management wrote, has `AutomaticDownload` and
  `AutomaticallyInstallMacOSUpdates` set to 1 (macOS's defaults), and records 26B131 installed minutes before the Mac's last boot;
  whether macOS installed it by itself or a person started it is not shown. The daemon reads only the managed file (5.8), so a
  Mac set to install macOS updates by itself reports no conflict. Open: whether 5.8 counts that local setting too, since such an
  install restarts the Mac without a drain.
- **The boot facts.** `sysctl -n kern.bootsessionuuid` gives a UUID and `kern.boottime` a `sec` that matches `last reboot`;
  `uname -r` is 27.2.0. The report from 0.1.462 has no `bootId` or `bootUnix`: the boot collector (10.1.1, komira-ai/hostkeeper#556)
  is not in that release. Their comparison waits for the next one.
- **The minor update (MP-U1 to MP-U3) cannot be shown, because none is pending.** `softwareupdate --list` finds nothing, and
  `--list-full-installers` lists 27.2.1 and older only.

### 10.1.3 The owner's steps, shrunk, 2026-11-25

The six steps 10.2 left to the owner were each weighed: does it need the owner, and if not, who does it. Nothing in this subsection
was run on a Mac: each line is a decision and what it rests on, and the runs it leads to are recorded here when they are made. What
is left for the owner is one session, `docs/operations/mac-proof-owner-session.md`.

- **The Xcode archive.** Stays the owner's (the Apple Account's two-factor prompt, 2.0), now in a browser on an operator's Linux box
  (2.0, "From a box that is not a Mac"). The checks `xcode-push` made on the operator's Mac move to a farm Mac: `hk xcode add` on the
  Linux box has the proof Mac check the stored archive in one `probe` run (`pkgutil --check-signature` with the same rule, `xip`, the
  build, `codesign`; xcode-one-action.md section 2.8, Built), and the stage runs the same checks again before `xip` (2.4, Built). So
  MP-X2's positive case is that probe's record, and MP-I1, MP-I3 and MP-I5 are one `xcode` run on the proof Mac, from the store. MP-X1 (paths on the operator's Mac) and MP-X3 to MP-X6 (what the
  operator's `pkgutil` and `xip` say of a damaged archive and its table of contents) are not run on this route: the stage's verdict
  and its deferral reasons are tested over fixtures, and the real archive's run shows the positive case.
- **The administrator's password.** Handed over (a file on the operator's side) but not used: the lead does not put it on a Mac by
  hand. It serves one purpose, the update account's Secure Token (5.5), and how it reaches a Mac is the owner's choice (the session's
  step 4). Built, it is read from the Blueprint's hostkeeper settings, `/Library/Managed Preferences/dev.hostkeeper.config.plist`;
  whether other users of the Mac can read that file is not shown, and a `stat` of it in the administrator's `hk ssh` session, with no `sudo`, shows it **[V]**. A
  build job runs as a user of the Mac, so if they can, neither password goes there.
- **FileVault on, and its pre-boot password.** Off the list: a Mac not reported FileVault off is never ordered `macos`
  (`filevault_not_ready`), and the ruling is FileVault off (question 4). MP-A7, MP-A8, MP-U4, MP-U5 and MP-U8's FileVault-on case
  prove a path no run takes; they come back only if FileVault is wanted on build Macs.
- **Apple Business settings.** Stay the owner's (5.5.1: the API cannot make them), in one sitting on a Blueprint of the proof Mac's
  own, read from the daemon's report (`mdmConflicts`, after a probe run, since the daemon looks again when a turn ends) rather than
  from a root shell, as `xcode-one-action.md` 6.2 has it.
- **A major upgrade.** Nothing to choose: every farm Mac runs macOS 27, the newest major (10.1.1). It stays the owner's choice when
  macOS 28 ships (`macos_major` on one Mac's own group).
- **The Mac's own `AutomaticallyInstallMacOSUpdates` 1.** Decided by the lead, as the rule of 5.8 already implies: off. The daemon
  turns it off at each start (5.8, "The Mac's own setting"; Built), and a `defaults read` of the key, which needs no root, shows it **[V]**.

**A minor macOS update through a run on the proof Mac** (MP-U1 to MP-U3) still cannot be made: none was offered (10.1.2), and no
farm Mac has the update account, which a run needs (`no_update_account`). The account waits for the owner's answer to the session's
step 4.

### 10.1.4 The first probe run on a farm Mac, 2026-11-26

MP-E13 (`mac-proof.md`), one `Maintenance.Schedule` of the kind `probe` for the farm Mac 10.1.2 read, from the operator's side, with no cordon by
hand, no root shell and no launchd harness: run `run-1795667883-1`, reason "Mac proof: launchd probes", scheduled 04:38:03Z. Read back
with `hk maintenance get`, `hk hosts get` and `hk audit list`, read only.

- **The run waited 17 hours, for a reason outside the probe.** It was held at every heartbeat by the serving count of 5.1
  (`min_serving` 1): every Mac of `mac-build` reported its four configuration packages `HEALTH_UNHEALTHY`, so no Mac counted as
  serving, and no group setting could lift it (the host itself must be serving). A configuration package has no run units and no
  health URL, and the daemon sent that "nothing to observe" as unhealthy. Fixed by komira-ai/hostkeeper#592 (the daemon sends such
  a service healthy; `docs/service-observation.md`), released in 0.1.480 at 22:11Z. Once all four Macs ran 0.1.480 the packages
  read healthy, and the run started at the next heartbeat with nothing done by hand.
- **The turn, from the audit log.** `maintenance_start` of the Mac at 22:18:32Z; `maintenance_step` `done` at 22:18:48Z. Its
  stage (nothing), drain (js-agent's `[hook] drain`), apply (the probe), check and put back (js-agent's `resume`) took 16 seconds
  together, inside one heartbeat, so the audit holds no row between start and done: the drain returned at once, so no job was
  running on the Mac to wait for. The run is
  `MAINTENANCE_STATE_DONE`, the host `done` with `applied` `probe`. The Mac's report carries the run record at step `done`, and its
  boot id and boot time (16:53:56Z, 2026-11-23) did not change: nothing restarted. The job service lists all four Macs live after
  the run, so `resume` lifted the cordon (the lead's read at 22:35Z).
- **What the probe read** (probed 22:18:34Z; daemon 0.1.480; macOS 27.2.1, build 26B131), each program exit 0, in the maintenance
  hook's `cutover` launchd job:
  - `console` `root` and `who` empty: nobody was logged in, as 10.1.2 found from a shell. MP-E7 had nothing to do.
  - `id` `uid=0(root) gid=0(wheel) groups=0(wheel),1(daemon),...` (cut at its 128-byte bound) and `managername` `System`: the
    hook's programs run as root in the System domain. **MP-E8's probe shown.**
  - `env` empty: every program the hook runs gets no environment. The eight ran with none.
  - `fdesetup_status` `FileVault is Off.` and `fdesetup_supportsauthrestart` `true`, exit 0, the same as from the root shell
    (10.1.2). **MP-A6 from launchd with FileVault off shown.** Decides: `maintenance_mac.rs` unchanged; `true` with FileVault off,
    so the gate reads the FileVault state first, as built.

### 10.2 Each [V]

Status `not run` means: not yet run on a Mac. The common reasons: no root shell on any farm Mac (10.1, until 10.1.2); no Mac drained in a group
of its own; a step that needs the owner's own hands (an Xcode archive downloaded with the Apple Account, FileVault's password, a setting in
Apple Business); a major upgrade, which cannot be undone, needs the owner's choice of Mac.

| [V] | Section | Settled by | Status | What each result decides |
|---|---|---|---|---|
| `xcode-push` runs only on macOS, with `pkgutil`, `xip` and `plutil` at their paths | 2.0 | MP-X1 | not run: on the route of 10.1.3 the archive comes from an operator's Linux box, and its checks run on the staging Mac instead | paths present: as built (`xcode_push/mod.rs` `Mac::system`); elsewhere: change those paths |
| `pkgutil --check-signature` on a real `.xip` exits 0, `Status: signed Apple Software`, chain ending at `Apple Root CA` with the fingerprint the code holds | 2.0, 2.4 | MP-X2, MP-X3 | not run: needs a real archive (the owner's download, session step 1); the positive case is now the staging Mac's `pkgutil` in an `xcode` run (10.1.3); the negative cases are the stage's tests | as expected: `signature.rs` stands; other status text or root: change `SIGNED_BY_APPLE` / `APPLE_ROOT_CA*`, never widening to a developer-ID status; no signature (an encrypted archive): step 2 needs another check (MP-X6) |
| The archive's version and build are readable | 2.0, 2.2 | MP-X4, MP-X5 | not run | the built read expands the archive and reads `Contents/Info.plist` and `Contents/version.plist`; the copy of `xip`'s manual page read (an older macOS's) documents only `--expand`, not checked against macOS 26, so 2.2's "`xip` lists it before it expands" is unproven and is changed to the expand-then-read if MP-X4 finds none |
| `xip --expand` checks Apple's signature | 2.4 | MP-I1 | not run: an `xcode` run on the proof Mac, after session step 1; the stage no longer rests on it (`pkgutil` first, 2.4) | refuses a damaged or re-signed archive: the stage step relies on it; expands it anyway: stage runs `pkgutil --check-signature` first, as `xcode-push` does |
| An encrypted archive is expanded with `archiveutil` as `xcodes` does | 2.4 | MP-I2, MP-X6 | not run: needs an encrypted release | other tools run `archiveutil extractXcodeAEA <archive>` in the destination directory (read in their sources, not run); the store names every archive `<sha256>.xip` (`xcode_store.rs`), so an encrypted one needs its kind recorded beside it, or the stage step choosing by the file's first bytes |
| `xcodebuild -runFirstLaunch` installs packages every Xcode shares | 2.4 | MP-I3 | not run: the same `xcode` run | the receipts it adds (`pkgutil --pkgs` before and after) say whether the drain is needed for it |
| Simulator platforms by `xcodebuild -exportPlatform` and `-importPlatform`; `xcode-push` uploads them | 2.4 | MP-I4 | not run | a copy of `xcodebuild`'s manual page read (not checked on a Mac) lists neither `-exportPlatform` nor `-importPlatform`; the route other tools use is, as recalled and not checked, `-downloadPlatform <platform> -exportPath <dir>`, then `-importPlatform <file>`. If MP-I4 shows that, 2.4 names it, and the export needs one download from Apple on the pushing Mac |
| An Xcode install, a swap and a removal, js-agent probing before and after | 2.4, MP | MP-I5 | not run: the same `xcode` run, which drains the Mac through js-agent | js-agent's probe reads `/Applications/Xcode*.app/Contents/version.plist` and runs nothing (komira-ai/jobscheduler); what it reports after each step decides whether J2's re-read in `resume` is enough |
| `sysadminctl -addUser` and `-secureTokenOn` take both passwords on standard input (`-`), or need a pseudo-terminal | 5.5 | MP-A2, MP-A3 | not run | on a pipe: the Built stdin runner is enough; only from a terminal: the daemon gives it a pseudo-terminal it owns; a password reaches no argument in either case |
| The Secure Token grant, and the account hidden | 5.5 | MP-A1, MP-A4, MP-A5 | MP-A1 shown (10.1.2): the reads before the account, and a hidden account's `dsAttrTypeNative:IsHidden: 1`, fixed; MP-A4, MP-A5 not run | read three ways (`sysadminctl -secureTokenStatus`, `diskutil apfs listCryptoUsers /`, `fdesetup list`); the built read uses the first (`maintenance_account.rs`) |
| The update account is a FileVault user; `fdesetup authrestart -inputplist` with the plist on standard input | 5.5 | MP-A6 to MP-A8, MP-E13 | MP-A6 with FileVault off shown from a root shell (10.1.2) and from launchd, by MP-E13's probe run (10.1.4): `FileVault is Off.`, `true`, exit 0 both; with FileVault on, not needed: no run orders `macos` on a Mac whose FileVault is not off (10.1.3) | if the account cannot authorise it, Macs with FileVault on are never ordered `macos` (question 4) |
| A minor update as root from launchd, nobody logged in, FileVault off and on | 5.5 | MP-U1 to MP-U5 | not run: no minor update pending on 2026-11-25 (10.1.2) and no update account on any farm Mac (10.1.3); MP-U4 and MP-U5 (FileVault on) not needed (10.1.3) | stops, asks, or does not restart: the apply step changes (MU8); `--restart` with FileVault on stops at the pre-boot login: the install runs without it and `authrestart` follows, as 5.5 says |
| A major upgrade by `startosinstall --nointeraction --user --stdinpass`, FileVault off and on | 5.5 | MP-U6 to MP-U8 | not run: cannot be undone; every farm Mac runs the newest major, so none is possible until macOS 28 (10.1.3); then the owner's choice | as for the minor; one Mac can show only one of the two FileVault cases |
| `startosinstall` restarts authenticated with FileVault on | 5.5 | MP-U8 | not needed: a major upgrade is never ordered with FileVault on (10.1.3) | if it stops at the pre-boot login, major upgrades are ordered only on Macs with FileVault off |
| An install on macOS 26 or later stops on a step that needs the screen | 5.5 | MP-U9 | not run | a pane after the restart that waits for a person: the host does not come back by itself, and `verify` must name it |
| How each setting of 5.8 shows: `/Library/Managed Preferences`, the declarations the Mac reports, the undocumented DDM state file | 5.8 | MP-M1 to MP-M6 | MP-M1 baseline shown (10.1.2): none of the files, no conflict; MP-M2 to MP-M6 not run: the owner makes the settings in Apple Business in one sitting (session steps 2 and 3), read from the daemon's report | each key seen corrects `maintenance_mdm.rs`'s keys; a nested `Deferrals` or `AutomaticActions` dictionary with nothing on in it is reported as on by `is_on`, which MP-M4 checks |
| Apple Business's `SOFTWARE_UPDATE` Configuration is the enforcement declaration | 5.5.1 | MP-M7 | not run: session step 3 (owner's console) | 5.5.1 stays "not chosen" either way; the answer only says whether the Blueprint route would work |

### 10.3 What the proof must allow for

Found while writing the steps, from the code and the jobscheduler repository, not from a Mac:

- **A farm Mac normally has someone logged in.** js-agent's session helper logs in a standing VM user or a lease's user automatically
  (komira-ai/jobscheduler `crates/js-mac-session/src/autologin.rs`), so "nobody logged in" is made and checked for the proof (MP-E7),
  and the apply path is also shown with that user logged in, as a real run would find the Mac.
- **A second visible administrator breaks the operator-keys account rule.** The daemon gives operator keys to the one visible local
  administrator (`login_account.rs`); with two, the host has none. The proof account (`hkproof`, never the production `hkupdate`) is
  hidden in the same step that makes it, and the update account of MU8 must be too (5.5 already hides it; it must not be visible between
  the two calls).
- **A cordon must outlive the shell and the Mac's restarts.** The cordon a shell starts ends with the shell's session. The owner's
  steps use the job scheduler's operator cordon instead (MP-E6): placed from the operator's side, it holds up to seven days a call with
  nothing on the Mac renewing it, over restarts of the Mac and its agent (komira-ai/jobscheduler `docs/runbooks/cordon-a-node.md`).
- **The steps that need no owner run inside a maintenance run, not under a cordon placed by hand.** A run of the kind `probe`
  (`docs/operations/maintenance.md`, "Probe: drain, read, put back") drains the Mac through js-agent's `drain`, runs a fixed list of
  read-only programs in the maintenance hook's launchd job (root, System domain, an empty environment), keeps what they wrote in the
  Mac's report and puts the Mac back with js-agent's `resume`: one operator command, with the drain, the halt and the put back of any
  run. It carries MP-E8's probe and MP-A6's launchd half as MP-E13; `mac-proof.md` section 1 lists every step that needs the Mac out of
  builds and what takes it out for each.
- **`hk ssh` needs the coordinator's SSH user CA, and the new group is named in its files first.** With no CA the `Ssh` service answers
  `not_configured` and no host announces `ssh-v1`; the SSH operators file must list the proof operator for the Mac. The operator-keys file
  is a second route (a plain `ssh` to the Mac's administrator), and moving a Mac into a group it does not list takes those keys away;
  the host-prerequisites file keeps the Command Line Tools check (MP-E2).
