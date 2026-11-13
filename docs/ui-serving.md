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
Sign out ends the session on the coordinator.

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
- **Packages** (`Packages` in the page list; `ui/pages/packages.js`). The registry of what the coordinator can deploy.
  A table of the registered packages (`Services.List`): its repository, its minimum version in each host group that has
  one, and the host groups that run it, with *Edit* and *Remove* on each row. *Add package* is a form of a name and the
  repository the package comes from, chosen from the registered repositories (the default, `komira`, preselected when it is
  one); it sends `Services.Put`. *Edit* changes the repository (`Services.SetRepository`) and, for one of the host groups that
  run the package, its minimum version (`Services.SetMinVersion`; empty removes it): a minimum version belongs to a host group,
  so a package no group runs has none and the add form does not ask for one. *Remove* of a package some host group runs is
  refused on the page, naming the groups; for one no group runs it asks first and sends `Services.Remove`, and the API's
  `in_use` refusal (a group listed it since the page loaded) is shown in words. A package is registered when a host group lists
  it or it was added here, so the packages of the farm's groups (`hostkeeper`, `tierstore`, `js-agent`, whatever the groups
  list) are in the table without any step. Below, *Repositories* (`Repositories.List`): each repository's URL, suites,
  components, access (public, or private: read by the coordinator alone), the packages that come from it and its signing keys
  (the last 16 hex characters of each fingerprint), with *Edit* and *Remove*, and a form that adds or changes one
  (`Repositories.Put`). The form asks for a name (not on *Edit*), the URL, the suites, the components, the signing key as a
  file (a binary OpenPGP keyring, `gpg --export`), who reads it, and for a private repository the path of the credential file
  on the coordinator's machine; each field has a one-line hint. The fingerprints are read from the key file in the page,
  not typed, and the package manager (`apt`) is not asked for. *Edit* with no new key file keeps the key held
  (`Repositories.Get`). A repository some package comes from is not removed, and the page names the packages. The reason of each
  write is a fixed sentence; the audit log names the operator. Repositories that packages name and the registry lacks are shown as
  an alert. A refused list is shown inline and the rest of the page still renders.
- **Host groups**: every group the coordinator holds (`HostGroups.List`, every page), the built-in `all` marked as built in,
  with its packages, the policy keys that are set ("default" when none is), its generation, its host count and its desired
  versions. Opening a group (`HostGroups.Get`) shows, per package, the desired and minimum version and the convergence counts,
  and the group's hosts 50 at a time, each a link to its Host page. A host is not moved here but on its Host page (below). The form
  that creates or changes a group takes a name and the packages as a list of checkboxes, one for each registered package
  (`Services.List`, the Packages page's): nothing is typed, the packages are installed in the order ticked, and with none
  registered the form links to the Packages page. A package the group lists that is not registered stays in the list, ticked.
  Saving a group (`HostGroups.Put`) keeps its policy and asks first when a package would be uninstalled. Each group but `all` has
  a *Delete* button (danger style) that asks first and sends `HostGroups.Remove`, which drops the group with its desired versions and
  marks. A group that still has hosts is refused (`not_empty`): the page names its hosts (`HostGroups.Get`, the first 50) and says to
  move them on their Host page first; for a group with no host that an inventory record, a pending approval or the Mac allowlist
  names, it says so. A refused List or Get is shown inline with its code and reason,
  and the rest of the page still renders.
- **Host**: the services that host runs because its group lists them (its Source column reads
  `role <name>`, the name being the host's group), the installed version against the desired one,
  and the minimum version. A **Host group** form moves the host: a dropdown of the groups the
  coordinator holds (`HostGroups.List`, every page) but the built-in `all`, with the host's own
  group selected and, for a host in none, the blank "No group (all alone)". Choosing another group
  and pressing **Move** calls `HostGroups.Move` after a confirmation, with the reason "moved in the
  console" (the form has no reason field); choosing the blank moves the host to no group. A refused List is shown inline and the rest of the page renders.
- **MDM setup** (`MDM setup` in the page list; `docs/operations/mdm-setup.md`, `docs/design/coordinator-mdm.md` section 4.9).
  One plain form and nothing else but a one-line status above it and a *Verify* button below it. The status line is `Mdm.Status`
  in a line: `MDM: configured`, or `MDM: missing push certificate, ABM token` (a part that is there and wrong, such as an expired
  certificate, is named after a semicolon), and `Name:` with the MDM name, read-only. Each field is one labelled input with a one-line
  hint of where its file comes from and a *How do I get this?* link to the section of `docs/operations/mdm-setup.md` (as GitHub renders
  it: the one address the app may link to, which `docLink` in `view.js` enforces) that says how to get it: the push certificate and
  its private key (files, PEM); the Apple Business Manager token (a file: the `.p7m` from ABM, or an existing token as JSON, told by its
  content); the Apple Business Manager private key and certificate (files, PEM, for taking over an existing ABM server); the GoDaddy API
  key and secret (the secret in a password field); and the contact email for Let's Encrypt (prefilled with the stored `acme_contact`, else
  with the signed-in user's name when that is an address). Only while no MDM name is set, one more field takes it (`Mdm.PutName`). A part the
  coordinator holds is shown beside its field as `stored ✓ (expires YYYY-MM-DD)`, never its value (`Mdm.Status`, and `Mdm.ListSecrets` for the
  ABM key and certificate). Next to the token field, only when no ABM key is stored, a link *Download the public certificate to upload to ABM*
  (`Mdm.GetAbmPublicKey`, which makes the key pair at its first call) is for a new server entry in ABM.
  One *Save* sends whatever fields are filled, in this order, and an empty file field keeps what is stored: the name; the push certificate
  with its key in one `Mdm.PutPushCertificate` (the coordinator checks they are a pair; one without the other is refused on the page);
  the ABM key with its certificate in one `Mdm.PutSecret` (kind `abm_key` with `with_certificate`; a certificate alone is kind `abm_cert`);
  the token (`Mdm.PutAbmToken`: `token_json` when the file begins with `{` and is a JSON object, else `token_p7m`, which is not tried when the
  ABM key just failed); the GoDaddy credential (`Mdm.PutSecret`, kind `dns_credential`, provider `godaddy`, the key and the secret) and, when the
  setting `dns_zone` is empty, the zone worked out from the MDM name (its registrable domain: the last two labels, three under a suffix of
  two labels such as `co` and a country code; `Mdm.PutSetting`); and the contact (`acme_contact`) when it was changed or is saved with the credential. A refusal is shown under
  its own field, in words with the API's code and reason, and the other parts are still stored. The ACME directory (production Let's Encrypt
  unless the setting says another), the DNS provider (GoDaddy) and the DNS zone are not on the page; they stay settings of the API.
  A file is read when *Save* is pressed, sent and dropped, never typed, and the GoDaddy secret is emptied whatever the answer is; the page
  shows no value of a secret. The writes need a signed-in user's session: a bearer is refused them. *Verify* (`Mdm.Verify`, with no Mac) lists what
  each part found: the setup, whether the Macs' listener runs and serves a certificate that names the MDM name, and, with an ABM token held, whether
  Apple accepts it. The push certificate request, the reply check, the name step beyond the first name, the TLS upload, the ACME settings and
  the test push of earlier pages are not on the page; their calls are still served.
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
  4 MiB, are served; anything else is `404`. So `ui/gen/client.d.ts`, the TypeScript declarations
  of the generated client `ui/gen/client.js`, is in the image but not served: the app does not
  load it. A path that leaves the directory (`..`, an absolute
  path, their percent-encoded or backslash forms, or a symbolic link to a file outside) is `404`,
  and the file outside is never opened. Hidden files (a name starting with `.`) are `404`.
- Each file is read from disk on each request; nothing is cached in memory.
- Every answer carries `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer` and
  `Content-Security-Policy: default-src 'self'; frame-ancestors 'none'`. No answer of the
  coordinator carries an `Access-Control-*` header.
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
