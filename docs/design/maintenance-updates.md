# Scheduled maintenance updates

Status: design. No code. The owner's answers of 2026-11-19 are in: the Mac's update account (section 5.5) and the first-class Linux path
(the one-button update of section 4.2) are decided; **Xcode is on hold** (sections 1.1 and 2 are options until it is decided). Facts about this repository were read on `main` at the commit this document lands on top
of; facts about komira-ai/jobscheduler and komira-ai/kbf were read on their `main` and are marked as theirs. **Built** marks what exists
(with its path), **New** what this design adds, **[V]** a claim to verify before building on it.

The owner's request (2026-11-19): "I want us to get updates regularly but I also don't want us to blow up ongoing work. The ideal scenario
is I get a notification in hostkeeper that updates are needed, and then I schedule updates, which would drain all work on the host, do the
update, and put it back in service."

The owner's answers on this design (2026-11-19): a Linux host that needs a kernel or security update or a restart is shown so, for the
host and the fleet ("kernel update, restart needed"), and **one button** drains it, updates it, reboots it if needed, checks it and puts it
back; the Mac's administrator account goes in a root-only file hostkeeper reads for the update call; Xcode should always be the latest,
and how it is fetched is being researched.

## What changes for the operator

**Before.** hostkeeper updates packages (its own, js-agent, the services of a host group) and installs the Command Line Tools on a Mac
whose group lists them (`docs/design/host-prerequisites.md`). The operating system is nobody's job: a Mac's macOS and Xcode, and a Linux
host's kernel and security packages, move when a person logs in and moves them, with no drain, and nothing tells anyone they are due.

**After.**

1. Every daemon reports, in its heartbeat, what its host runs (OS version and build, running and newest installed kernel, the Xcodes in
   `/Applications`) and what is due: macOS updates `softwareupdate` lists, Linux security and kernel updates, a reboot the host already
   needs, and an Xcode its group wants that it lacks.
2. The web UI shows a **notice**: per host ("kernel update, restart needed"; "12 security updates"; "macOS 26.1 to 26.2, restart";
   "Xcode 26.2 wanted") and for the fleet (a strip on Home: "Updates due on 7 hosts in 2 groups").
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

The daemon looks at its host every hour, after every start, and after a maintenance step ends, as the prerequisites pass does today (Built:
`crates/hostkeeper-daemon/src/prerequisites.rs`, a worker thread that never holds up a heartbeat; `xcode_clt.rs`, which already runs
`softwareupdate`; the bounded program runner `run`/`run_ok` of `login_account.rs`). Each program runs as root with an empty environment, a deadline and capped output, through the same ports. **New:** the
checks below and the report of section 4.1.

| Kind | Platform | What the daemon reads | Due when |
|---|---|---|---|
| `macos` | Mac | `sw_vers` (version, build); `softwareupdate --list` (each label, its version, whether it says `Action: restart`) | `softwareupdate` lists a macOS update. A major upgrade is listed but never due by default (group setting `macos_major`, section 3) |
| `xcode` | Mac | each `/Applications/Xcode*.app/Contents/version.plist` (`CFBundleShortVersionString`, `ProductBuildVersion`); `xcode-select -p` | the group's wanted Xcodes (section 2) name one the host lacks, or the default the group names is not `xcode-select`'s |
| `linux` | Linux | apt: `apt-get update`, then a simulated upgrade, split by origin into security and other; dnf: `dnf updateinfo list --security` and `dnf check-update`; apk: `apk version -l '<'`. The kernel: `uname -r` against the newest installed kernel | a security update or a newer kernel package is available (group setting `linux_scope`: `security` by default, or `all`) |
| `reboot` | Linux | `/run/reboot-required` (apt); `needrestart -b` where installed (its kernel status, and the services running old libraries); `needs-restarting -r` (dnf); the newest kernel under `/lib/modules` against `uname -r` | the host needs a reboot it has not had: a newer kernel installed but not running, or the distribution says so. A run of `reboot` alone has nothing to install: it drains, reboots, checks and puts back |

Packages hostkeeper itself installs must not move in an OS upgrade. apt holds them (`apt-mark hold`, Built:
`docs/design/packages-repositories.md`). dnf and apk have no such hold today, so the maintenance hook excludes them by name (dnf
`--exclude`, apk by pinning each in `/etc/apk/world` as `name=version`) **[V]**, and the due list leaves them out on every platform.

### 1.1 How hostkeeper learns that a new Xcode exists (on hold)

This section and section 2 describe one option, kept until the owner decides how Xcode is fetched (section 9). The owner wants the latest
Xcode, always kept up to date, without hostkeeper vending the `.xip` itself; the options are in section 2.4. The rest of the design does
not depend on which is chosen: whatever brings a new Xcode, it is installed in a host's turn, drained, side by side (section 6).

A host cannot see a new Xcode: `softwareupdate` does not list it, and the App Store's copy is not the one we install (section 2.2). So the
**coordinator** reads Apple's developer releases feed (`https://developer.apple.com/news/releases/rss/releases.rss`) once a day (New:
a periodic job `xcode-feed`, claimed by due time like `alerts` and `prune`, Built: `store/postgres/periodic.rs`). A release of Xcode
newer than every entry of the Xcode store (section 2.1) raises the warning alert `xcode_release` ("Xcode 26.2 (17C52) released; download
it"). That is the notice; it does not make an update due. The feed's URL is a flag (`--xcode-feed-url`, empty turns the job off), and a
failed fetch is a log line, retried the next day, never an alert. Beta and release-candidate entries are not offered.

## 2. Xcode: options for the download and the install (on hold)

### 2.1 Option A: a person downloads, the coordinator stores

A `.xip` is downloaded from Apple's developer site by a signed-in Apple Account, with two-factor sign-in. Automating that sign-in means
keeping an Apple Account's password and second factor on a machine, and is unreliable (tools that try, such as `xcodes`, break on
two-factor changes). So the one human step is: **an operator downloads the `.xip` once per release and adds it to the coordinator's Xcode
store** with `hkctl xcode add <file>` (or the UI's upload on the Maintenance page). The coordinator reads its version and build from the
archive's signed `Info.plist` [V], stores it by SHA-256 under `--xcode-dir`, and lists it (`Xcode.List`: version, build, size, SHA-256,
who added it). Nothing else in the flow needs a person.

Daemons fetch a `.xip` from the coordinator over their mutual-TLS channel, only for a host whose group wants it, and check the size and
SHA-256 the order names before `xip --expand`, which checks Apple's signature. The `.xip` is never in a public bucket, a package
repository or this repository.

**Licence (Question 2).** Xcode's licence lets an organisation install "a reasonable number of copies" on Apple computers it controls, and
forbids hosting the software "on any website or server" and making it "available over a network where they could be run or used by
multiple computers at the same time". A coordinator that serves the `.xip` only to its own enrolled Macs, for installation, is the
narrowest reading we can build; whether it is allowed is counsel's call. Without that sign-off the fallback is the same flow with the
store on one farm Mac's read-only share, or a person copying the `.xip` to each Mac.

### 2.2 Option B: Apple Business's device-assigned App Store Xcode

Apple Business can assign the App Store's Xcode to devices (device licences, no Apple Account on the Mac), and its built-in device
management installs it. It gives the latest Xcode with no person, which is what the owner asked for; its limits, as read so far:

- **One Xcode, the newest.** The App Store offers only the current release, so a version cannot be pinned, an older one cannot be
  installed again, and two cannot sit side by side.
- **In place, at `/Applications/Xcode.app`.** An App Store update replaces the Xcode under running builds, and its moment is Apple
  Business's, not a drained host's.
- **No hook for us.** The coordinator has no MDM since its removal (`docs/design/coordinator-mdm.md`); Apple Business's settings are edited
  by hand in Apple Business, so hostkeeper could neither time the install nor see its result except by reading the disk afterwards.

A way around the first two **[V]**: keep App Store updates off for Xcode, let Apple Business install the newest into its fixed path in `/Applications`, and in
the host's drained turn copy it to `Xcode-<version>.app`. Whether Apple Business then installs it again, and whether a copy keeps working
after the App Store's copy moves on, is unverified.

### 2.3 The install (in the maintenance hook, section 5.2)

1. **Stage, while the host serves:** fetch the `.xip`, check it, expand it into a staging directory on the same volume
   (`/Applications/.hostkeeper-staging/`), and check the expanded app's `ProductBuildVersion` against the order.
2. **On the drained host:** move it to `/Applications/Xcode-<version>.app` (a rename on one volume), `xcodebuild -license accept`,
   `xcodebuild -runFirstLaunch` (it installs packages every Xcode shares, which is why the host is drained, [V]), then `xcode-select -s` to
   the group's default Xcode, if the group names one.
3. **Remove** each `Xcode-<version>.app` the group no longer wants, on the drained host. An Xcode whose directory is not `Xcode-<version>.app`
   (one a person or Apple Business put there) is reported and never removed.

`<version>` is `CFBundleShortVersionString` (`26.2`, `26.1.1`). Two builds with one version are refused when added to the store
(`xcode_version_taken`), so on our hosts a version names one build.

### 2.4 The choice

| Option | Latest, unattended | Pins a version, side by side | A person | Open |
|---|---|---|---|---|
| A. `.xip` into the coordinator's store | no: a person per release | yes | one download per release | the licence on serving the `.xip` |
| B. Apple Business, App Store | yes | only with the copy of 2.2 | none | the copy, and timing to a drained host |
| C. A tool signed in with an Apple Account (`xcodes` and the like) | yes | yes | none, until its sign-in breaks | an Apple Account's password and second factor kept on a machine |

Which one, or a mix, is the owner's decision after the research the owner asked for (section 9, question 2).

## 3. The group's settings

A host group's policy (Built: `hostgroups.proto` `RolloutPolicy`) gains an optional `maintenance` table (New, additive; `HostGroups.Put`
and `hkctl` edit it):

| Key | Default | Meaning |
|---|---|---|
| `xcode` | empty | the Xcode versions the group's Macs keep, each one in the store |
| `xcode_default` | none | the one `xcode-select` points at; unset leaves `xcode-select` alone |
| `macos_major` | `false` | whether a new major macOS counts as due |
| `linux_scope` | `security` | `security` (security updates and the kernel) or `all` |
| `batch` | 1 | hosts of the group in maintenance at once, unless a run says otherwise |
| `min_serving` | 1 | hosts of the group that must stay serving (section 5.1); a group of one host is taken only when the Schedule ticks "take the group's last host" |
| `drain_deadline_secs` | 0 (none) | how long a drain may wait before the host is put back as it was and the run moves on (section 5.3) |
| `return_secs` | 1200 (Linux), 3600 (Mac) | how long a rebooting host may be silent before `maintenance_not_back` |

## 4. Wire and UI

### 4.1 The report (New)

`HeartbeatRequest` gains `maintenance` (the next free field, optional), announced by the feature `maintenance-v1` and sent only to a
coordinator that advertises it, like `prerequisites` (Built). It carries the host's facts (OS name, version and build, running and newest
installed kernel, the boot id the daemon already reads for its step record, Built: `hostkeeper_steps::ProcBootId` and
`hostkeeper_launchd::SysctlBootId`, and the boot time), the installed Xcodes (version, build, path, `xcode-select`'s), the due items of
section 1 (kind, from, to, needs reboot, label), when it last looked, and the state of the maintenance run it holds. It is bounded under the
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
- **Maintenance page** (New, `ui/pages/maintenance.js`, in the Manage menu): per group, the hosts and what is due; the Xcode store; the
  runs, each with its hosts and their step (waiting, staging, draining since, applying, rebooting, checking, putting back, done, deferred,
  halted), refreshed by `ctx.every`. A rebooting host links its **Console** (Linux, Serial over LAN, Built: `ui/pages/serialconsole.js`)
  or **Desktop** (Mac, Built: `ui/pages/desktop.js`; a Mac's screen returns only after it boots, so the page is opened again then).
- **Schedule** (a form on the host and Maintenance pages): what (the kinds that are due, all ticked), where (this host, a group, or the
  hosts ticked), when (**now**, or a **window** with a start and an end in the operator's local time, stored as Unix seconds), **batch**
  (the group's `batch` by default), drain deadline (the group's by default), and a reason. It calls `Maintenance.Schedule`.
- **A run's buttons:** **Cancel** (no new host starts; a host in its turn finishes), and per halted host **Retry** (from the failed step)
  and **Put back** (run the drained services' `resume`, leave the update as far as it got, and let the run go on). Nothing else: a pause is
  a Cancel and a new Schedule.

The operator API is a new service `hostkeeper.operator.v1.Maintenance` (`Schedule`, `List`, `Get`, `Cancel`, `RetryHost`, `PutBack`) and `Xcode` (`Add`, `List`, `Remove`), each audited, each in `ui/calls.json` and `hkctl`.

## 5. The run

### 5.1 Who orders it: the rollout gate (reused)

A run is stored by the coordinator (New: migration with `maintenance_runs` and `maintenance_hosts`, additive, older releases may run)
and decided where packages are: in `decide()`, per heartbeat, in one transaction under the rollout gate (Built:
`crates/hostkeeper-coordinator/src/decide.rs`, `store/decision.rs`). One rule is shared with packages, as it stands: **one thing at a
time on a host** (Built: `busy_elsewhere`). A host with a package order outstanding is not given maintenance, and a host in a run is
given no package order until its run ends there. The other rules are maintenance's own (New), because the package rules (halt,
exclusion, `min_up`) are kept per package and count a cordoned host as up:

- **Serving count:** a host is not taken while fewer than `min_serving + 1` hosts of its group are serving. A host counts as serving when
  it is live and healthy and not between drain and done in a run, so a draining, rebooting or halted host never counts.
- **Window:** before the start or after the end, no host *starts*. A host already started finishes; a drain is never cut by a window.
- **Batch:** at most `batch` hosts of a group are between staging and done (default 1). The coordinator's rule for packages is one update
  at a time on a host and one service rolling at a time; maintenance keeps the first and, per group, adds the batch. Runs of different
  groups go on in parallel, so a run over three groups with batch 2 has up to six hosts in their turn.
- **Order of hosts:** by node id.
- **Skipped:** a host that is held or excluded for any package, a host whose daemon does not announce `maintenance-v1` (shown in the
  Schedule form, never ordered), and a host whose group changes while it waits.
- **Packages wait, not the reverse:** a host between drain and done is left out of the hosts a package plan counts for its stage, so a long
  drain does not hold a stage open; it gets its package updates after its turn. A maintenance run does not count against
  `--live-rollouts`. `HostGroups.Move` of a host in its turn is refused `host_in_maintenance`.

The coordinator sends one order, `maintain` (New, a case of `Order.verb`, with an `order_id` and `try` like the others): the run, the
kinds, for `xcode` the store entries (version, build, size, SHA-256) and the versions to remove and the default, the Linux scope, and the
drain deadline.

### 5.2 On the host: drain hooks and the maintenance hook

Per-service logic lives in the service's own hook, not in hostkeeper (AGENTS.md B17), so hostkeeper does not call js-agent itself. Two
changes keep it so:

- **Drain steps (New, hook contract, additive).** A package's manifest may name two more steps of its hook, `[hook] drain` and
  `[hook] resume`, as it names `[hook] boot` today (Built: `crates/hostkeeper-config/src/manifest.rs`). `drain` takes the service out of
  service and returns when it has no work; `resume` puts it back. Both are safe to run twice. A package that names neither is left alone.
  The conformance harness (Built: `hostkeeper-hook`'s `conformance` feature) gains checks for them.
- **The maintenance hook (New).** The OS work is a hook too, a subcommand of hostkeeper's own binary, `hostkeeper maintenance-hook
  <step>`, launched the way the step runner launches a step (Built: `hostkeeper_steps` launchers: a transient systemd unit, a launchd job on a
  Mac, a process group under OpenRC).

The step runner's sequence does not fit as it stands: it runs `rollback` after a failed `prepare`, `cutover` or `verify`, its reboot row
restarts a new release's units, and only `prepare` may run unbounded (Built: `hostkeeper-hook` `next.rs`, `hostkeeper-steps`
`resume.rs` and `runner.rs`). An OS update has no release to restart and cannot be rolled back. So the daemon runs a `maintain` order by a
small sequencer of its own (New), whose position is kept in a durable, versioned run record beside the step records and fsynced before and
after each step, so a crash, a reboot or a daemon that restarts as a newer release resumes it:

| # | Step | What runs | On failure |
|---|---|---|---|
| 1 | stage | maintenance hook `prepare`: check free space (twice the download, at least), then download what can be downloaded (`softwareupdate --download`, `apt-get --download-only`, `dnf --downloadonly`, the `.xip` expanded into staging) | deferred: nothing was touched, the host never left service; the run goes on |
| 2 | drain | each installed service's `drain`, in the group's package order, unbounded unless the run sets a deadline (5.3). js-agent's cordons the host and waits for its leases to end | halt (5.4) |
| 3 | apply | maintenance hook `cutover <kind>`, once per kind of the run, macOS first (section 5.5). When a kind needs a reboot (`Action: restart`, `reboot-required`, a new kernel), the hook records the kind as applied, then reboots (`softwareupdate --install --restart` does both on a Mac; `systemctl reboot`, or OpenRC's `reboot`, on Linux). After the reboot the daemon reads the record and the changed boot id and goes on with the next kind | halt; `maintenance_not_back` when silent past `return_secs` |
| 4 | check | maintenance hook `verify` (section 5.6) | halt |
| 5 | put back | each drained service's `resume`, in reverse order. js-agent's re-reads its facts (the new Xcode set) and then uncordons | halt |
| 6 | done | the run record is closed; the next heartbeat's report shows nothing due of the kinds run | |

No step of this sequence runs `rollback`; a failure stops it where it is.

### 5.3 Drain: no deadline by default

Work is never cut to finish an update (`docs/design/hostkeeper-design.md`, goal 6). A `drain` step runs with no time bound
(`RuntimeMaxSec=infinity`, Built for `prepare`, extended to `drain`, New) unless the run names `drain_deadline_secs`. When a deadline passes, the step exits 75 (deferred, Built
outcome): the daemon runs `resume` for every service it drained, the host is back in service with nothing changed, it is marked `deferred`,
and the run moves on to the next host; a deferred host is offered again when the run's other hosts are done, if the window is still open.
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

- **Linux.** apt: `apt-get install --only-upgrade` of the security packages and the kernel (`linux_scope = security`) or `apt-get
  dist-upgrade` (`all`), non-interactive, keeping existing configuration files; dnf: `dnf upgrade --security` or `dnf upgrade`; apk:
  `apk upgrade`. The package lock is waited for as the installers wait today (Built: `--package-lock-wait-secs`).
- **macOS.** `softwareupdate --install <label> --restart --user <account> --stdinpass`, with the label `--list` named, after the download
  step 1 made. On Apple silicon an OS update needs an administrator's authorisation, and the owner decided how it is given (2026-11-19):
  the Mac's administrator account, its name and password, is in a root-only file, `/Library/Application Support/hostkeeper/update-account`
  (root's, mode 0600; the daemon refuses another owner or mode). The bootstrap writes it from its settings (New keys beside the farm
  values, `docs/design/mac-bootstrap.md`), or a person does. The daemon reads it only for this call and passes the password on the
  program's standard input: never in arguments, a log line, a heartbeat or an error.
  **Threat model.** The account and password are the same on every Mac and are not meant to be a secret; the owner said so. What guards
  the update is hostkeeper's own authorisation: the call runs only for a `maintain` order, which the coordinator sends to an enrolled host
  over mutual TLS, for a run an authenticated operator scheduled (audited). Reading the file gains a local user nothing they could not be
  told.
- **Xcode.** Section 2.3. Xcode alone needs no reboot.

When a run has both `macos` and `xcode`, macOS goes first (a new Xcode can need a newer macOS), then the reboot, then Xcode, in the same
host's turn (step 3).

### 5.6 Check

Before `resume`, the maintenance hook's `verify` checks, and fails naming what it found:

- the host booted again when it had to (the boot id changed);
- what was due of the kinds run is no longer due: `sw_vers` reports the target build; `uname -r` is the newest installed kernel and no
  reboot is needed; each wanted Xcode is at `Xcode-<version>.app` with its build, `xcodebuild -version` runs from it, and its licence is
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
| not back (silent past `return_secs`) | yes (hold file) | yes | halted | its return resumes it; the alert clears when it reports |
| not reached (window ended, or Cancel) | no | no | ended | a new Schedule |

- **Coordinator restart.** Runs and hosts' turns are rows; a host's step is what its daemon's run record says, reported in each heartbeat.
  An order the daemon already holds is recognised by its `order_id` and not started twice (Built: the order and try of every order).
- **Daemon older than `maintenance-v1`.** Never ordered; shown as such in the Schedule form.
- **A host retired mid-run.** Its turn ends; its cordon is js-agent's to lapse with the host.
- **A full disk.** Step 1 checks space before it downloads and defers; it never fills the disk of a serving host.

## 6. Builds stay correct across an Xcode change

- **Side by side.** A new Xcode is a new directory. An Xcode a build uses is never replaced under it, and one is removed only on a drained
  host whose group no longer wants it.
- **The Xcode is a platform property, by build.** js-agent advertises the installed Xcodes as the capability `xcode`. Its built frame
  field carries build ids (Built there: `caps.xcode` of `js-agent-wire`, `16C5032a`), while its design names versions (komira-ai/jobscheduler
  `docs/design/capabilities.md`; its step 38 builds the probe). This design asks for the **build**: it names one Xcode exactly, including
  a re-release under one version. A consumer's action states the Xcode build it needs as a platform property; a remote-execution action key includes its
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
| MU1 | Wire: `HeartbeatRequest.maintenance` and `maintenance-v1`, `Order.maintain`, the `maintenance` group policy, the `Maintenance` and `Xcode` operator services, the alert kinds `updates_due`, `xcode_release`, `maintenance_halted`, `maintenance_not_back`; `additions_maintenance.rs`; `docs/cli.md` and the UI client regenerated | |
| MU2 | Daemon: the facts and due checks of section 1 on all three platforms, hourly and after a start, reported; fake programs in tests | MU1 |
| MU3 | Coordinator and UI, read only: the report kept and shown (`Hosts.Get`, the Updates tab), `updates_due`, the Maintenance page's due list | MU2 |
| MU4 | Hook contract: `[hook] drain` and `resume` in the manifest and the release manifest, the step runner running them, the conformance checks | MU1 |
| MU5 | Daemon: the `maintain` order, the run record, the maintenance hook for Linux (apt, dnf, apk, reboot), stage-drain-apply-reboot-check-put back across a reboot | MU2, MU4 |
| MU6 | Coordinator: runs, the migration, `Schedule`, `Cancel`, `RetryHost`, `PutBack`, the rules of 5.1 in `decide()` (serving count, window, batch, packages leaving a host in its turn out of a stage), `maintenance_halted`, `maintenance_not_back` and silence while rebooting (no `silent_host` before `return_secs`); the Update button, the Schedule form and the run view | MU3, MU5 |
| MU7 | Xcode, by the option the owner chooses (2.4): for option A the store, the fetch route, the hook's install and removal (2.3), the `xcode-feed` job and `xcode_release` | MU6, J2 and K1 deployed, question 2 |
| MU8 | macOS: `softwareupdate` in the maintenance hook with the update account file (5.5), and the bootstrap's keys that write it | MU6 |
| MU9 | `docs/operations/maintenance.md`: the runbook (the Xcode download, scheduling, a halted host) | MU7, MU8 |
| J1 | komira-ai/jobscheduler: js-agent `drain`, `resume`, the hold file | MU4 |
| J2 | komira-ai/jobscheduler: the `xcode` probe re-read in `resume` | J1 |
| K1 | komira-ai/kbf: `xcode` required on a Mac action; `kbf-mac-provision` without Xcode items on hostkeeper hosts | J2 |

MU1 lands first (interfaces before code). MU2 and MU4 go in parallel; Linux maintenance (MU5, MU6) works end to end before any Mac work,
with J1 deployed: the one-button Linux update (a kernel or security update, or a restart) is the first thing the build delivers. MU8
follows. MU7 waits for J2 and K1, because without them an Xcode change could serve a cached result across Xcodes (section 6), and for the
owner's Xcode decision.

**Tests the build must prove**, each over fakes (fake programs answering `softwareupdate`, `apt-get`, `dnf`, `apk`, `xip`, `xcodebuild`;
a fake hook; a clock the test sets; injected heartbeats), none timing anything: a due item appears and clears; a host in a package update
is given no maintenance and the reverse; the window starts no host before or after it, and never cuts one that started; a batch of N
never has N + 1 hosts between staging and done in a group; no host is taken when fewer than `min_serving + 1` hosts serve, a draining or
halted host never counting; a group of one is taken only when the Schedule says so; a failed step runs no `resume` and no `rollback`, and
the hold file keeps the host cordoned across a restart; js-agent with a hold file refuses a lease before its cordon is placed; a drain
deadline puts the host back unchanged; a `reboot`-only run (a kernel installed but not running) installs nothing and reboots; Update starts
the same run as a Schedule of everything due, now; the update account file of another owner or mode is refused, and its password never
appears in an argument or a log line (asserted by exact equality); a full disk defers at stage; a macOS-and-Xcode run reboots after macOS and installs Xcode after the
reboot; `verify` fails on each wrong fact; a host silent past `return_secs` raises `maintenance_not_back` and not before; Put back and
Retry do what section 4.2 says; a host in its turn is left out of a package stage and given no package order; an older daemon is sent no
`maintain` order; a coordinator restart does not start an order twice.

## 9. Questions for the owner

1. **How a Mac authorises a macOS update. Decided (2026-11-19): (a)**, with the threat model of section 5.5. The question as asked: on Apple silicon `softwareupdate` installs an OS update only with a volume owner's password
   (`--user`, `--stdinpass`), unless a device management service with the bootstrap token orders it. Options: (a) each Mac keeps its
   administrator's password in a root-only file that hostkeeper reads only for that call, written once per Mac by a person; (b) Apple
   Business's built-in management enforces the target version (a setting made by hand in Apple Business, at Apple's moment, not the
   drained host's) **[V]** whether it can; (c) macOS stays manual and hostkeeper only drains and puts back around it. **Recommendation:
   (a)**, the only one hostkeeper can time to a drained host.
2. **How Xcode is fetched: on hold.** The owner wants the latest Xcode, always kept up to date, and does not see why hostkeeper should
   vend the `.xip`. The options are in section 2.4; the answer follows research into what CI providers, device management services and
   tools such as `xcodes` do. MU7 waits for it.
3. **`linux_scope` default.** Kept: `security` (security updates and the kernel); `all` is one setting away.
