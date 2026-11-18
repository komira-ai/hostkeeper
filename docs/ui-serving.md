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
(`/v1/ssh/terminal`, `docs/operations/ssh-ca.md`), which `default-src 'self'` allows.

## The frame and the lab

After sign-in the app opens on **Home**, the lab (`docs/design/ui-shell-lab.md`). The frame around every page is a side menu with
the "hostkeeper" wordmark on top (a click goes Home), the pages (Host groups, Packages, Inventory and Enrollment) and, at the bottom, **Settings**
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
  and a table of every host. **Edit layout** shows the controls: rename the cluster, add a row (the next number), add a rack to a
  row (its height from a list, 42 U by default), resize, move to another row or remove a rack, remove a row, and move a server by
  picking it up (or dragging it) and putting it on a free U of a rack, or taking it out of its rack. A row or a rack that still
  holds servers is not removed, and the page names them. A refusal is told in words. An empty lab says so and offers "Add a row".
  Calls: `Hosts.List`, `Releases.Status`, `Alerts.List`, `Layout.Get`, `Enrollment.ListRecords`; on a button `Layout.PutName`,
  `AddRow`, `RemoveRow`, `PutRack`, `RemoveRack`, `Place` and `Unplace`.
- **Enrollment** (approval): besides the host group, the Height (1U to 4U, 1U by default), Row, Rack and U position of the server.
  All four are choices from the layout, the U position only among the free slots of the rack for that height; Row's first choice,
  Unplaced, places nothing. The placement is sent with the approval, which is refused whole when the slot is taken.
- **Host** (Placement): where the host stands, and the same four choices to place or move it (Place, Move) and Unplace.

## What the pages do for services

- **Rollout**: for each service, what `Services.List` says of it (the hosts it runs on, the hosts being uninstalled; the
  assigned roles and operator assignments it also lists are empty, since a group places it) and the desired version. The same section sets the
  desired version (a version and, if you have them, the text of the release index and its digest;
  the text is sent as typed). Setting the desired version asks first. It still shows the Assign and
  Unassign buttons, but a coordinator places by host group alone, so it refuses both with
  `placed_by_host_group`, and the page shows that refusal as it is; put a package in a group with
  `hkctl host-groups put` instead.
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
- **Remote SSH** (a tab of the Host page, `ui/sshpanel.js`; design:
  [SSH through the coordinator](design/ssh-through-coordinator.md) section 13): the tab says whether the host's SSH host keys are
  pinned (`Ssh.HostKeys`, read with the page) and lists them. When none is pinned, or the host reports others than the pinned ones, and
  its last heartbeat reported its keys, it lists the reported keys and **Pin these host keys** asks first, then sends
  `Ssh.PinHostKeys` with exactly those keys. **Open a terminal** opens one socket to `/v1/ssh/terminal?host=<host id>` of the page's own
  origin (over TLS), sends `{csrf, cols, rows}` first (the session's CSRF token and a terminal of 120 by 36), then keystrokes and
  pastes as binary messages; the host's output comes back as binary messages and is drawn by the app's own terminal (`ui/terminal.js`,
  `ui/terminal_render.js`, `ui/terminal_keys.js`; no third-party code), into elements classed by `ui/styles/terminal.css`, so no inline
  style or script is used and the policy above stays as it is. The coordinator's `status` messages show how far it has got and its last
  message, `closed`, gives the cause, which the tab shows in words (every refusal of `SshRefusal`, every close cause of a tunnel,
  `no_pin`, `host_key_mismatch`, `auth_refused`, `shell_refused`, `bad_request` and `exit`); a socket that closes without it says so.
  **Close** closes the socket. The terminal has no resize (it is always 120 by 36) and keeps the last 200 lines that scrolled off.
  A proxy in front of the private listener must pass WebSocket upgrades on that path.
- **Users** (`Users.List`; `Users.Create`, `Disable`, `Enable`, `ResetPassword` and `Delete` on a button): a table
  of the users with whether each is enabled, must change its password, who made it and when, its last sign-in and the
  second its temporary password stops working. Create takes a name; Create and Reset password show the temporary
  password under the table once, until "I have copied it" is pressed or the page is left (it is held in the page's memory
  only and is not in the status line, so a reload of the page or the list never shows it again). Disable, Reset password
  and Delete ask to confirm first, as each ends the user's sessions. The coordinator refuses to disable or delete the last
  enabled user and to reset the password of the user who asks; the page shows the refusal as the API gives it.
- **API tokens** (`ApiTokens.List`; `Create` and `Revoke` on a button): each token's name, source, maker and time. Create
  takes a name and shows the token once, as Users does a password. Revoke asks first and is offered for the tokens of the
  store only: a token of the tokens file is marked read-only, and the page says it is removed by editing that file.
- Each action is one API call, and the Audit page shows it by the signed-in user's name or the token's name. A refused call
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
  `Content-Security-Policy: default-src 'self'; frame-ancestors 'none'`. No answer of the
  coordinator carries an `Access-Control-*` header.
- The one socket: the app opens a socket in exactly one place, `ui/main.js`, which builds `ctx.openSocket(path, handlers)` from the
  page's own host and hands it to the pages as it hands them `fetch`. The scan of the sources
  (`crates/hostkeeper-ui-tests/src/scan.rs`) refuses `WebSocket` in every other file and a `ws://` or `wss://` address in every file, and holds
  `main.js` to one `WebSocket(` call.
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
