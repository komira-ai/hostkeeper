# Serving the operator web app

The operator web app in [`ui/`](../ui) is static files. The coordinator serves them from its
private listener, the same listener as the operator API, so the app's calls are same-origin:
there is no cross-origin access, no cookie and no session.

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

Browse to `https://<private-address>/ui/` and paste an operator token when the page asks for one.
The page keeps the token in memory only and sends it on each API call; reloading the page asks for
it again.

## What the pages do for services

- **Rollout**: for each service, the roles it is assigned to, the operator's assignments (a
  removal is shown as one), the hosts it is assigned to, the hosts being uninstalled, and the
  desired version. The same section sets the desired version (a version and, if you have them, the
  text of the release index and its digest; the text is sent as typed), assigns the service to a role, and unassigns it from a role, with a box to
  uninstall it from each host that loses its last assignment. Setting the desired version and
  unassigning with uninstall ask first.
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
  or why none (`no_target`, `no_build_for_host`, `installer_dropped`, or `unheard` while the
  coordinator has read no installer of a daemon that announced `target-v1`). A host whose platform
  `Hosts.List` does not name (one whose only word of the service is that it is a member) is
  `unknown: platform not reported`: the coordinator, which knows it, may pick it an entry. Counts of each pick
  cover every host; the table shows the first 200. A refusal (`NOT_FOUND release_unknown` for a
  version never set with targets) is shown as the API says it.
- **Host**: the services assigned to that host, how each is assigned (to the host or through its
  role), the installed version against the desired one, and the minimum version.
- Each action is one API call, and the Audit page shows it by the token's name. A refused call
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
- The files need no token. The operator API the app calls still needs the operator token on every
  call.

## Where the app comes from

The release's container image
(`hostkeeper-coordinator-<version>-<linux-platform>.image.tar`) copies the `ui/` of its commit to
`/usr/share/hostkeeper-coordinator/ui`, owned by root, files `0644` and directories `0755`
(`packaging/container/Dockerfile`). Before the image is released,
`cargo xtask release check-image-ui` reads it back and refuses it unless that directory holds
exactly the files of `ui/`, byte for byte, with that owner and those modes.

The coordinator's release tarball (`hostkeeper-coordinator-<version>-<linux-platform>.tar.gz`)
does not carry the app; a coordinator run from it serves `/ui/` only with `--ui-dir`.
