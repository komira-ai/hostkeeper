# Serving the operator web app

The operator web app in [`ui/`](../ui) is static files. The coordinator serves them from its
private listener, the same listener as the operator API, so the app's calls are same-origin:
there is no cross-origin access. The static files set no cookie and read none; a person's session
is made by the sign-in routes of the operator API (see
[the operator login design](design/operator-login.md)).

## The directory the coordinator serves

The coordinator's container image holds the app at `/usr/share/hostkeeper-coordinator/ui`, and
`hostkeeper-coordinator serve` serves that directory when it is started without `--ui-dir`. A
deploy of the image needs no flag for it.

To serve another directory, give `serve` the directory that holds the app's files, beside its
other flags:

```sh
hostkeeper-coordinator serve --private-listen <private-address> --ui-dir <path-to-ui> ...
```

- With `--ui-dir`, that directory is served, whatever is at the default path. It must exist and
  be a directory when the coordinator starts; otherwise the start is refused with a message naming
  `--ui-dir` (it does not fall back to the default).
- Without `--ui-dir`, `/usr/share/hostkeeper-coordinator/ui` is served when it is a directory (a
  symbolic link to one included) when the coordinator starts. When nothing is there, or something
  other than a directory, the coordinator starts and serves no file: `/ui/` is `404`.

The flag is the only way to set it: no environment variable is read.

## Open the app

Browse to `https://<private-address>/ui/` and sign in with your user name and password. The
browser keeps the session in a cookie the page cannot read; the page keeps only the session's CSRF
token, in memory, and sends it in the `X-HK-CSRF` header of each API call. Reloading the page asks
the coordinator for the session again, so a reload does not sign you out. A first sign-in, or a
sign-in with a temporary password an admin set, shows only the form for choosing a new password.
Sign out ends the session on the coordinator. The host page's Remote SSH terminal speaks to the coordinator over one WebSocket to the same origin
(`/v1/ssh/terminal`, `docs/operations/ssh-ca.md`), and so does the Console page (`/console/v1/stream/<session>`), which
`connect-src 'self'` allows. Signing out, or leaving the page, closes either.

## The frame and the lab

After sign-in the app opens on **Home**, the lab (`docs/design/ui-shell-lab.md`). The frame around every page is a side menu with
the "hostkeeper" wordmark on top (a click goes Home), the pages (Host groups, Packages, Inventory, Enrollment and Maintenance) and, at the bottom, **Settings**
(one page with the tabs Users, API tokens and Audit), the theme (the browser's own, light or dark; kept in memory only),
Sign out and a button that collapses the menu to icons; on a narrow screen it is a drawer opened by the top bar's Menu button.
The top bar says where you are and who is signed in. The menu's icons are small inline SVG drawings (`icon` in `ui/view.js`; no image or font is loaded). Banners come in three kinds: `error` (red), `warning` (amber) and `notice` (blue, for what is true and needs no action); an empty one shows nothing. The pages that moved keep their old route: `fleet` is Home, and Release, Rollout and Holds still open
by their route and are not in the menu.

- **Home**: the lab. The floor plan is a band for each row of the cluster with a footprint for each rack (its number, its U in use
  and a dot for each state of its servers: healthy, updating, offline, pending); a click on a row opens its racks as front
  elevations side by side in a strip that scrolls (wheel, drag, arrow keys), each server drawn over its U with a light for its
  state and its name, a blank panel over a free run of U; a click on a server opens its host page, and hover says its name, group,
  version and state. A click on a rack's head opens that rack with its servers in a table. Breadcrumbs lead back
  (`Lab > Row > Rack > Host`). Below the floor plan: the servers not placed yet (the Unplaced list), the alerts of `Alerts.List`
  and a table of every host. Under the alerts, the degraded hosts: how many, and each reason with the hosts that have it (most
  hosts first), each host a link to its page. A live host is degraded when it has a reason: an alert of `Alerts.List` that names
  it and says it is not healthy (a halt, a duplicate identity, a failed canary, a silent host, a trusted root, a failed
  prerequisite; an operator's hold, an abandon or an approval that waits is no reason), or a service of it halted, unhealthy or
  whose last update failed; it is drawn `failed`, and its row says why under Why. When `Alerts.List` fails, the count is of the
  hosts' own reports, and says so. **Edit layout** shows the controls: rename the cluster, add a row (the next number), add a rack to a
  row (its height from a list, 42 U by default), resize, move to another row or remove a rack, remove a row, and move a server by
  picking it up (or dragging it) and putting it on a free U of a rack, or taking it out of its rack. A row or a rack that still
  holds servers is not removed, and the page names them. A refusal is told in words. An empty lab says so and offers "Add a row".
  Above the alerts, the updates notice: when `Alerts.List` answers `updates_due` alerts (one per host group with hosts that have
  updates due, "Updates due on 4 hosts of mac-build"), it shows each one's words with an **Update** button (one `Maintenance.Schedule` call for the group, with the one
  Reason field of the notice, after one confirmation) and an **Open Maintenance** button that opens
  the Maintenance page; these alerts are not repeated in the alerts strip, and with none the notice is not shown.
  Calls: `Hosts.List`, `Releases.Status`, `Alerts.List`, `Layout.Get`, `Enrollment.ListRecords`; on a button `Layout.PutName`,
  `AddRow`, `RemoveRow`, `PutRack`, `RemoveRack`, `Place` and `Unplace`; on the notice's Update `Maintenance.Schedule`.
- **Enrollment** (approval): besides the host group, the Height (1U to 4U, 1U by default), Row, Rack and U position of the server.
  All four are choices from the layout, the U position only among the free slots of the rack for that height; Row's first choice,
  Unplaced, places nothing. The placement is sent with the approval, which is refused whole when the slot is taken.
- **Host** (Placement): where the host stands, and the same four choices to place or move it (Place, Move) and Unplace.
- **Host** (why it is not healthy): under the header, every reason the host is not healthy in plain words, with the time it
  began (by the coordinator's clock for an alert, the host's for a failed update) and the one action that fixes it: **Clear halt**
  (`Releases.ClearHalt` of the service, with a reason typed once for the section; it lifts the service's halt on every host, and
  the question says so), **Retry update** (`Hosts.Retry` of the host and the service; the host is ordered the version again
  when its update plan and holds let it), or words saying why there is no button (a
  failed prerequisite, which the host's daemon tries again by itself; a service that reports itself unhealthy; another alert).
  For a js-agent taint (a halt report with the reason `tainted`) it shows what the agent said and the hook's last log lines; the
  hook tells hostkeeper no check and no run id, so they read "not reported to hostkeeper", and since hostkeeper has no action
  that clears a taint, the page says to clear it on the host first. `Alerts.List` is the page's last read; when it fails, the
  page says so and shows the reasons the host's own report gives.
- **Host** (Updates; `ui/updates.js`): the header shows what the host's maintenance report (`Host.maintenance` of `Hosts.Get`)
  lists as due, a short phrase each: "kernel update, restart needed" (a Linux kernel package, or a newer kernel installed and not
  running), "restart needed" (another reboot the host needs), "12 security updates", "3 other updates", "macOS 26.1 to 26.2,
  restart", "Xcode 27.2 available", and "and 4 more" for the items the report left out; a look the coordinator calls stale
  (`maintenance_stale`) shows "stale look" with its age, since an empty list then does not mean nothing is due. A host with
  nothing due and a fresh look, or with no report, shows nothing there. The **Updates** tab shows the report: the operating
  system and its build, the running and newest installed kernel, when the host booted, the last look and how old it is, each due
  item (kind, from, to, whether it needs a restart, whether it is a security update, label), and on a Mac what an update needs
  (FileVault, authrestart, free space, the update account, the Command Line Tools, device-management conflicts) and the Xcodes
  installed. For a host with a report the tab also has an **Update** button with a Reason field (one `Maintenance.Schedule` call for
  the host, after one confirmation: everything due, now, the group's batch) and the **Schedule** form (`ui/schedule.js`): which
  kinds, which hosts, now or in a window (typed in the operator's time zone, shown in UTC), batch, drain deadline, whether to take
  the group's last host (at most 32 hosts when they are ticked one by one), a reason, a **Check** that sends the request without `confirm` and shows which hosts would be taken and
  which are left out and why (a second Check names the hosts ticked again), and **Schedule**, which stores the run. `ui/updates.js` itself makes no call.
- **Maintenance** (`ui/pages/maintenance.js`): the fleet's count ("Updates due on 7 hosts in 2 groups"), then each host group
  (in name order, Unassigned last) with its hosts, those with something due first, each with what is due, its last look and how
  old it is (", stale" for a stale one), and a link to its Host page; a host whose daemon sends no report says "no report". It
  reads `Hosts.List` (every page) when opened and again every 60 seconds; a read again that fails keeps the last answer and
  says so. After the fleet's count comes the runs (`ui/runs.js`): `Maintenance.List` (the last 10) and `Maintenance.Get` of each
  run that is not done, read when opened, every 10 seconds (the page is drawn again only when the runs changed) and after a stored run; a list that fails is said there and the rest of
  the page stays. Each run shows its state, scope, window, batch, reason and its hosts with each one's step, with a Console or
  Desktop link while it restarts (the Console link opens the host's Console page), and **Cancel** (a run not ended), **Retry** and **Put back** (a halted host), each with a reason
  and one confirmation, one call per click. Each group's card has an **Update** button on each due host's row, a Reason field,
  **Update the group** (not for Unassigned) and **Schedule...**, which opens the Schedule form for the group's hosts after reading
  the group's maintenance policy with `HostGroups.Get`. A refusal of `Maintenance.Schedule`, `Cancel`, `RetryHost` or `PutBack`
  that the operator can act on is said in words (an Update or Schedule on the host page, this page or Home's notice, and
  Cancel, Retry and Put back here): nothing due (`nothing_due`), the group's last host (`last_host`, with the way to take it:
  the Schedule form's "Take the group's last host"), a host already in a run, a window that has ended, an unknown host, group or run, a host not in the run, a run that
  has ended, a host no longer halted, and an audit entry that could not be written; any other refusal is shown as its code
  and token. In the Schedule form, a window field left empty is said to be "not set".
- **Xcode** (`ui/pages/xcode.js`, beside Maintenance in the Manage menu; design: [adding an Xcode in one
  action](design/xcode-one-action.md) section 4): what the Xcode store holds and what each Mac has. It reads `Xcode.List`,
  `Hosts.List` and `HostGroups.List` (every page of both) and `Maintenance.List` (the last 10 runs) when opened and again
  every 60 seconds; a read again that fails keeps the last answer and says so, and a run list that fails is said while the
  rest of the page stays. The sections are: the release that waits ("Xcode 27.3 (27C21) is out and not in the store; it
  needs macOS 27.1", from `Xcode.List`'s `waiting`) with the **Add Xcode** button (with none waiting the page says the store
  holds the newest release, and the button still opens the panel); the uploads in progress (`Xcode.List`'s `uploads`: who
  started each, "7.31 of 11.92 GB, last write 2 minutes ago", by the page's clock); the store, each archive with its version,
  build, size in decimal GB, who uploaded it, when and how many Macs have it installed in its own directory with its build,
  and a Reason field and **Remove**, which says which Macs still have the archive due, asks once and calls `Xcode.Remove`
  with the reason; and the Macs, one row each (a host whose heartbeat says Mac or whose report has a Mac's facts) in group
  order, Unassigned last: its group, the group's `xcode_keep` and `xcode_default`, each Xcode installed (version, build,
  selected, foreign), what is due, its device-management state ("nothing that blocks updates", or the `mdmConflicts` its
  report lists) and, while a scheduled, running or halted run names the Mac or its group, a link to the Maintenance page
  (saying so when the run's kinds do not include `xcode`). What is due is the coordinator's rule (`maintenance_xcode.rs`):
  the newest `xcode_keep` releases of the store are kept; the kept releases a Mac lacks (not installed in
  `Xcode-<version>.app` with the same build) are due, newest first ("due: 27.3, then 27.2"); when none is missing the
  default (`latest`, or the named version if kept) is due to be selected when `xcode-select` does not name it; a group that
  keeps 0, or a Mac in no group, is left out. **Add Xcode** opens a panel with the command to run on the operator's Mac
  (`hk xcode add <waiting version>`, or `hk xcode add` with none waiting) and the first-time line (`hk settings init
  --endpoint <this page's origin> --ca-file <ca.pem> --token-file <token-file>`), each with a **Copy** button that writes
  the line to the clipboard (`ctx.copy`, which `main.js` gives the pages from `navigator.clipboard`); where the browser
  refuses, the line is selected and the panel says to copy it by hand. While the panel is open the page reads `Xcode.List`
  every 10 seconds, shows the upload's progress, then "Stored. Due on 3 Macs of mac-build." with a link to the Maintenance
  page. The page starts no run: Update and Schedule stay on the Maintenance page.

## What the pages do for services

- **Rollout**: for each service, what `Services.List` says of it (the hosts it runs on, the hosts being uninstalled; the
  assigned roles and operator assignments it also lists are empty, since a group places it) and the desired version. The same section sets the
  desired version (a version and, if you have them, the text of the release index and its digest;
  the text is sent as typed). Setting the desired version asks first. It still shows the Assign and
  Unassign buttons, but a coordinator places by host group alone, so it refuses both with
  `placed_by_host_group`, and the page shows that refusal as it is; put a package in a group with
  `hk host-groups put` instead.
  The section also shows the service's update plan from `Releases.Status`: the stage sizes with
  their percents (a stage an operator enters is marked manual) against the eligible hosts, the
  stage the plan is in ("2 of 3"), why it waits (and for which stage, or that an operator paused
  it, who and why), the hosts that hold update slots (several can at once), and a table of the
  stages with their hosts and each host's state. Its buttons call `Rollouts.Advance` (offered
  only while the plan waits for a manual advance), `Rollouts.Pause` (while it is not paused) and
  `Rollouts.Resume` (while it is paused), each with the reason typed in the section and a
  confirmation first.
- **Release**: type a service and a version to see the release it was set with, as
  `Releases.GetRelease` answers it: the digest of the release index, and for each platform its
  entries in the order set (installer, digest, size, locations, attestation digest, package and
  oldest daemon). An installer name is shown as it is, whatever it is. Below them, each host of
  the service with the entry it is picked by the coordinator's rule
  ([packages design](design/packages.md), section 3.3), worked out from `Hosts.List`: the entry of
  the installer it runs the service with, else the first entry whose installer its daemon lists;
  or why none, in words (`no_target` as "No build for this platform", `no_build_for_host` as "No build for this
  host", `installer_dropped` as "Installer missing", `unheard` as "Installer not reported", while the
  coordinator has read no installer of a daemon that announced `target-v1`); "Runs with" is the installer the host's
  heartbeat names, the archive when it names none, and "Not installed" when the service is not running. A host whose platform
  `Hosts.List` does not name (one whose only word of the service is that it is a member) is
  "Unknown (platform not reported)": the coordinator, which knows it, may pick it an entry. Counts of each pick
  cover every host; the table shows the first 200. A refusal (`NOT_FOUND release_unknown` for a
  version never set with targets) is shown as the API says it.
- **Packages** (`Packages` in the page list; `ui/pages/packages.js`). The repositories packages are installed from and the
  registry of what the coordinator can deploy, as two plain lists, *Repositories* first (a package comes from one) and then
  *Packages*. Each list is a table with an *Add repository* or *Add package* button in its header and *Edit* and *Remove* on
  each row. No form is shown by default: a button, or a row's *Edit*, opens its form under its table with *Save* and *Cancel*
  (one form at a time; a save draws the page again with every form closed). *Add package* is disabled, with the hint "Add a
  repository first", while no repository is registered. A refused list is shown inline and the rest of the page still renders.
  - *Repositories* (`Repositories.List`): each repository's name, its URL and how many packages use it. The form (`Repositories.Put`)
    asks for a name (not on *Edit*), the URL, the suites, the components, the signing key as a file (a binary OpenPGP keyring,
    `gpg --export`), who reads it, and for a private repository the path of the credential file on the coordinator's machine;
    each field has a one-line hint. The fingerprints are read from the key file in the page, not typed, and the package manager
    (`apt`) is not asked for. *Edit* names the keys held (the last 16 hex characters of each fingerprint) and, with no new key
    file, keeps the key held (`Repositories.Get`). *Remove* of a repository some package comes from is refused on the page,
    naming the packages; the API's `in_use` refusal is shown in words. Repositories that packages name and the registry lacks are
    shown as an alert.
  - *Packages* (`Services.List`): each package's name, its repository and the host groups that run it. The form is a name and the
    repository the package comes from, chosen from the registered repositories (the default, `komira`, preselected when it is
    one); it sends `Services.Put`. *Edit* names the minimum versions set and changes the repository (`Services.SetRepository`)
    and, for one of the host groups that run the package, its minimum version (`Services.SetMinVersion`; empty removes it): a
    minimum version belongs to a host group, so a package no group runs has none and the add form does not ask for one.
    *Remove* of a package some host group runs is refused on the page, naming the groups; for one no group runs it asks first
    and sends `Services.Remove`, and the API's `in_use` refusal (a group listed it since the page loaded) is shown in words. A
    package is registered when a host group lists it or it was added here, so the packages of the farm's groups (`hostkeeper`,
    `tierstore`, `js-agent`, whatever the groups list) are in the table without any step. The name is plain text until the
    router has a page for a package.

  The reason of each write is a fixed sentence; the audit log names the operator.
- **Host groups**: every group the coordinator holds (`HostGroups.List`, every page; there is no built-in group),
  with its packages, the policy keys that are set ("default" when none is), its generation, its host count and its desired
  versions. Opening a group (`HostGroups.Get`) shows, per package, the desired and minimum version and the convergence counts,
  and the group's hosts 50 at a time, each a link to its Host page. A host is not moved here but on its Host page (below). The form
  that creates or changes a group takes a name and the packages as a list of checkboxes, one for each registered package
  (`Services.List`, the Packages page's): nothing is typed, the packages are installed in the order ticked, and with none
  registered the form links to the Packages page. A package the group lists that is not registered stays in the list, ticked.
  Saving a group (`HostGroups.Put`) keeps its policy and asks first when a package would be uninstalled. The page says that every group must list `hostkeeper` (a group that omits it is refused, `hostkeeper_not_listed`). Each group has
  a *Delete* button (danger style) that asks first and sends `HostGroups.Remove`, which drops the group with its desired versions and
  marks. A group that still has hosts is refused (`not_empty`): the page names its hosts (`HostGroups.Get`, the first 50) and says to
  move them on their Host page first; for a group with no host that an inventory record, a pending approval or the Mac allowlist
  names, it says so. A refused List or Get is shown inline with its code and reason,
  and the rest of the page still renders.
- **Host**: the services that host runs because its group lists them (its Source column reads
  `role <name>`, the name being the host's group), the installed version against the desired one,
  and the minimum version. A **Host group** form moves the host: a dropdown of the groups the
  coordinator holds (`HostGroups.List`, every page), with the host's own
  group selected and, for a host in none, the blank "Unassigned" (a host in no group is ordered no package). Pages label a host with no group "Unassigned". Choosing another group
  and pressing **Move** calls `HostGroups.Move` after a confirmation, with the reason "moved in the
  console" (the form has no reason field); choosing the blank moves the host to no group. A refused List is shown inline and the rest of the page renders.
  The Overview (`ui/serialbaseline.js`) shows a "Serial console" row, the port and the state of the daemon's serial console
  baseline from the last heartbeat (`heartbeat.serialConsole`; no row when the heartbeat carries none), and a warning while the state
  is reboot pending (the kernel's console moves to the port at the host's next reboot; hostkeeper does not reboot the host) or failed.
- **Remote SSH** (a tab of the Host page, `ui/sshpanel.js`; design:
  [SSH through the coordinator](design/ssh-through-coordinator.md) section 13): the tab says whether the host's SSH host keys are
  pinned (`Ssh.HostKeys`, read with the page) and lists them. When none is pinned, or the host reports others than the pinned ones, and
  its last heartbeat reported its keys, it lists the reported keys and **Pin these host keys** asks first, then sends
  `Ssh.PinHostKeys` with exactly those keys. **Open a terminal** opens one socket to `/v1/ssh/terminal?host=<host id>` of the page's own
  origin (over TLS), sends `{csrf, cols, rows}` first (the session's CSRF token and a terminal of 120 by 36), then keystrokes and
  pastes as binary messages; the host's output comes back as binary messages and is drawn by the app's own terminal (`ui/terminal.js`,
  `ui/terminal_render.js`, `ui/terminal_keys.js`; no third-party code), into elements classed by `ui/styles/terminal.css`, so no inline
  style or script is used. The coordinator's `status` messages show how far it has got and its last
  message, `closed`, gives the cause, which the tab shows in words (every refusal of `SshRefusal`, every close cause of a tunnel,
  `no_pin`, `host_key_mismatch`, `auth_refused`, `shell_refused`, `bad_request` and `exit`); a socket that closes without it says so.
  **Close** closes the socket. The terminal has no resize (it is always 120 by 36) and keeps the last 200 lines that scrolled off.
  A proxy in front of the private listener must pass WebSocket upgrades on that path.
- **Console** (`ui/pages/serialconsole.js`, reached from the Console link of Home's list of servers on the row of each host whose
  last heartbeat says Linux; design: [console design](design/console-broker.md)): a Linux server's serial port (COM2, `ttyS1`)
  through its BMC's Serial over LAN. **Open the console** calls `Console.Open` with the host and the operator's reason (required,
  1 to 256 bytes, no control character), then opens one socket to `/console/v1/stream/<session>`, offering the subprotocols
  `hostkeeper.console.v1` and `csrf.<the session's CSRF token>` (the browser sends its sign-in as for every call; the coordinator
  answers with the first name alone), and hands that socket, open, to the vendored terminal (xterm.js with its attach addon,
  `ui/vendor/`), which writes the host's bytes and sends what the operator types; the page sends one carriage return so that a
  waiting login prompt is drawn again. When no byte has come for the session's `quietSecs` (the coordinator's
  `--console-quiet-s`, 10 by default), the page says "No output yet: the host's serial port must be ttyS1." A close is said in
  words from its reason token (every close code of the console design's section 12); a close with none says the coordinator
  refused the connection, or that it was lost. **Close** closes the socket. The page also shows the host page's Power and boot
  section and every open session (`Console.List`), each with a **Close** that asks first and calls `Console.Close`. A session
  that is not serial (a Mac's screen) is closed at once.
- **Desktop** (`ui/pages/desktop.js`, reached from the Desktop link of Home's list of servers on the row of each host whose last
  heartbeat says `PLATFORM_DARWIN_ARM64`; design: [console design](design/console-broker.md) sections 3 and 11.2): a Mac's screen
  through the coordinator. The page names who holds the Mac's console (`Console.List`), with a **Close it** that asks first and
  calls `Console.Close`. **Open the desktop** calls `Console.Open` with the host and a reason (as the Console page; a refusal is said
  in the Console page's words, but `no_address`, which for a Mac names the console targets file and the data MACs of the Mac's
  inventory record, and `console_busy`, which says to open the page again to see who holds it), opens one socket
  to `/console/v1/stream/<session>` with the same two subprotocols, and in the socket's `open` handler hands it, open, to the vendored
  noVNC viewer (`ui/screen.js`, `ctx.viewScreen`), which is never given an address. When the Mac asks for its login the page shows a
  form for what it asks and hands what is typed to the viewer alone, keeping no copy of the password. A close is said in the Console
  page's words; a refusal of the login by the Mac stays said when the viewer then closes the socket itself. **Close**, leaving the
  page and signing out close it, and each end empties the screen box. An answer that is not a Mac's screen gives the session back
  with `Console.Close`. The Mac's Power buttons are not on the page yet.
- **Users** (`Users.List`; `Users.Create`, `Disable`, `Enable`, `ResetPassword` and `Delete` on a button): a table
  of the users with whether each is enabled, must change its password, who made it and when, its last sign-in and the
  second its temporary password stops working. Create takes a name; Create and Reset password show the temporary
  password under the table once, until "I have copied it" is pressed or the page is left (it is held in the page's memory
  only and is not in the status line, so a reload of the page or the list never shows it again). Disable, Reset password
  and Delete ask to confirm first, as each ends the user's sessions. The coordinator refuses to disable or delete the last
  enabled user and to reset the password of the user who asks; the page shows the refusal as the API gives it.
- **API tokens** (`ApiTokens.List`; `Create` and `Revoke` on a button): each token's name, source, owner, maker and time. Create
  takes a name, makes a token owned by the signed-in user and shows it once, as Users does a password. Revoke asks first and is offered for the tokens of the
  store only: a token of the tokens file is marked read-only, and the page says it is removed by editing that file.
- Each action is one API call, and the Audit page shows it by the signed-in user's name, by an API token's owner with the token
  (the entry's `api_token`) in its Token column, or by a tokens file token's name; a change made without an API token has `-` there. A refused call
  shows the API's code and reason as they are.

## What is served

- `GET` and `HEAD` of `/ui/` and of every path below it, on the private listener only. The
  heartbeat listener never serves these files.
- `/ui/` is the directory's `index.html`, and a path ending in `/` is the `index.html` of that
  directory.
- Only files ending in `.html`, `.css`, `.js`, `.json`, `.svg`, `.png` or `.ico`, of at most
  4 MiB, are served; anything else is `404`. So the `.d.ts` files of `ui/gen/` (`client.d.ts`, the TypeScript
  declarations of the generated client `ui/gen/client.js`, and one file per proto package it
  imports types from) are in the image but not served: the app does not load them. A path that leaves the directory (`..`, an absolute
  path, their percent-encoded or backslash forms, or a symbolic link to a file outside) is `404`,
  and the file outside is never opened. Hidden files (a name starting with `.`) are `404`.
- Each file is read from disk on each request; nothing is cached in memory.
- Every answer carries `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer` and
  `Content-Security-Policy: default-src 'self'; connect-src 'self'; frame-ancestors 'none'; style-src 'self' 'unsafe-inline'; img-src 'self' data:`.
  Script comes only from the app's own origin: no inline script and no `eval`. `connect-src 'self'` lets the page reach its own
  origin, a `wss:` socket to its own host and port included. Inline style and `data:` images are for the console's vendored
  libraries (the Console page's terminal, xterm.js, draws its cells with inline style, the remote desktop its cursor and JPEG rectangles as `data:`
  images; [console design](design/console-broker.md), section 8.2); the app's own files still use neither inline style nor
  inline script. No answer of the coordinator carries an `Access-Control-*` header.
- The one socket: the app opens one kind of WebSocket, to its own origin, in exactly one place, `ui/main.js`, which builds
  `ctx.openSocket(path, handlers, protocols)` and hands it to the pages as it hands them `fetch`; `ctx.openTerminal(element,
  opened)` (`ui/terminal_port.js`, handed over there too) hands the opened socket to the vendored terminal's attach addon, so no
  library is given an address. Its address comes from
  `socketAddress` (`ui/socket_address.js`): the page's own host, over TLS when the page is, and a path of one of two fixed shapes, the
  Remote SSH terminal's (`/v1/ssh/terminal?host=<host id>`) and a console session's stream
  (`/console/v1/stream/<session id of 64 lower-case hex digits>`); any other path is refused before a socket is made. The scan of the
  sources (`crates/hostkeeper-ui-tests/src/scan.rs`) refuses `WebSocket` in every other file of the app and a `ws://` or `wss://`
  address in every file, and holds `main.js` to one `WebSocket(` call, whose arguments are `socketAddress(window.location, path)`, alone or followed by
  `protocols` (the subprotocols the page offers).
- Vendored libraries: third-party code lives under `ui/vendor/`, each file pinned by its SHA-256 in
  `crates/hostkeeper-ui-tests/vendor.sha256` (`sha256sum` lines, run in `ui/`), so a change to a vendored library is a change to
  that file; the scan fails for a vendored file with no pin or another digest, and for a pin with no file. It holds xterm.js 5.5.0
  and its attach addon 0.11.0 (both MIT, unchanged from their npm packages, each with its licence file), and noVNC 1.7.0
  (`ui/vendor/novnc/`, MPL-2.0, with its bundled pako, MIT): `core/rfb.js` and the modules it imports, unchanged but for
  `core/websock.js`, whose `open` (the library's one `new WebSocket(`) throws instead, marked in the file as changed; its
  `LICENSE.txt`, `AUTHORS` and licence texts are the package's own. The app's 1000-line limit is for its own files. The directory is exempt
  from the scan's needles (minified code names licence URLs and `console.`), and is still refused, by the scan, inline script and
  handlers made from text, and any `WebSocket(` call: the libraries are handed the app's one socket, already open.
- The files need no sign-in. The operator API the app calls still needs a session (with its CSRF
  header) or an operator token on every call.

## Where the app comes from

The release's container image
(`hostkeeper-coordinator-<version>-<linux-platform>.image.tar`) copies the `ui/` of its commit to
`/usr/share/hostkeeper-coordinator/ui`, owned by root, files `0644` and directories `0755`
(`packaging/container/Dockerfile`). Before the image is released,
`cargo xtask release check-image-ui` reads it back and refuses it unless that directory holds
exactly the files of `ui/`, byte for byte, with that owner and those modes.

The coordinator's release tarball (`hostkeeper-coordinator-<version>-<linux-platform>.tar.gz`)
does not carry the app; a coordinator run from it serves `/ui/` only with `--ui-dir`.
