# The `mdm` service package

The MDM server stack that enrolls the farm's Macs (NanoHUB, the SCEP server, NanoDEP and Caddy,
with lego for the TLS certificate) is a service package, `mdm`, that hostkeeper installs on one
Linux host like any other service ([design: two programs, section 6.2](../design/two-programs.md)).
Assigning `mdm` to a host installs it there; unassigning it uninstalls it. The package is a `.deb`,
`mdm_<version>_amd64.deb` (`cargo xtask release deb --service mdm`), which hostkeeper installs with `apt`
([design: native packages only](../design/packages-native.md), sections 10.2 and 10.6). It carries
the pinned servers, its own program `mdm` and its units; the release builder fetched the
pinned archives and checked them against `crates/hostkeeper-mdm-core/pins.txt` when it built
the package, so nothing is downloaded on the MDM host.

The `hostkeeper-mdm` kit (its `fetch`, `install`, `tls` and `verify` verbs) still ships until the
package replaces it on the farm's MDM host; its `migrate` verb moves the old kit's stack. Do not
install both on one host: they use the same ports, and the package's `prepare` refuses a port
another unit holds.

## What the package holds

The package is built for `linux-x86_64` only: the pinned tools are linux-amd64 builds. It has no
maintainer script: dpkg runs nothing of the package's own, and nothing but hostkeeper and the hook
enables or starts a unit. Each release ships it as an attested asset and publishes it to the `testing` suite
of the project's apt repository ([package-repositories.md](../deploy/package-repositories.md));
`cargo xtask release targets --service mdm` lists its `apt` entry (repository `komira-testing`).

| Path the `.deb` installs | What it is |
|---|---|
| `/usr/lib/mdm/nanohub`, `depserver`, `depsyncer`, `scepserver`, `caddy`, `lego` | The pinned upstream binaries. |
| `/usr/lib/mdm/mdm` | The package's program. |
| `/usr/lib/mdm/hostkeeper-hook` | The hook: two lines that run `mdm update-hook` beside it with the arguments the daemon gives. |
| `/usr/lib/systemd/system/mdm-*.service`, `mdm-cert.timer` | The units below, each starting its program from `/usr/lib/mdm/`, installed disabled. |

The hook's bounds, the run units, the health URL and the params are not in the `.deb`: they are the manifest of the
release's entry, as for every native package. The daemon runs the hook from the version it staged, as
`<version dir>/tree/usr/lib/mdm/hostkeeper-hook`, so `prepare` checks the staged `scepserver` and `lego` in
`<version dir>/tree/usr/lib/mdm/`. The program knows which build it is from the path it runs as: run as
`/usr/lib/mdm/mdm` (or the staged tree's), it is the `.deb`'s.

| Unit | What it does |
|---|---|
| `mdm-scep.service`, `mdm-nanohub.service`, `mdm-depserver.service` | The run units: started at install and observed by the daemon. |
| `mdm-proxy.service` | The HTTPS proxy; it starts only once lego's certificate is there. |
| `mdm-depsyncer.service` | NanoDEP's syncer; `mdm dep define-profile` enables it. |
| `mdm-cert.service` | Orders the first certificate, while lego has none; then restarts the proxy (which starts it). |
| `mdm-cert-renew.service`, `mdm-cert.timer` | Renews the certificate within 30 days of its end (`lego renew --days 30`; earlier runs order nothing), once a day by the timer, then restarts the proxy if it runs. It wants `mdm-cert.service` first, so the timer also retries a first certificate that failed. Both jobs wait for the DNS credentials. |

## Before the first install

Write two root-owned files on the MDM host.

1. **The settings**, `/etc/mdm/settings`, a file of root's that no other user may write. The keys are the kit's
   (`crates/hostkeeper-mdm-core/config.example` documents each), except that the package fixes
   these, and the file may leave them out or set exactly these values; any other value is refused:

   | Key | The package's value |
   |---|---|
   | `MDM_USER` | `mdm` |
   | `MDM_STATE` | `/var/lib/mdm` |
   | `MDM_ETC` | `/etc/mdm` |
   | `MDM_BIN` | `/var/lib/hostkeeper/packages/mdm/current/bin`; `/usr/lib/mdm` for the `.deb` |
   | `MDM_PKG_DIR` | `/var/lib/mdm-pkg` |
   | `MDM_UNIT_PREFIX` | `mdm` |
   | `MDM_SLICE` | `mdm.slice` |
   | `MDM_NANOHUB_PORT`, `MDM_SCEP_PORT`, `MDM_DEP_PORT` | `9004`, `9005`, `9001` |

   The ACME keys (`MDM_ACME_*`) are required: the package always carries the certificate jobs.
   Put no secret in this file.
2. **The DNS provider's credentials**, `/etc/mdm/dns.env` (mode 0600, root's), holding the
   variables lego reads for `MDM_ACME_DNS_PROVIDER` (the list is in `config.example`). It may also
   be written after the install: until it is there the certificate jobs do not run and the
   proxy waits.

Then assign `mdm` to the host (`hkctl services assign --service mdm --node-id <id>`; see
[assignments.md](assignments.md)).

## What each step of the hook does

The daemon runs `bin/mdm update-hook <step>` as root (from the `.deb`, its `hostkeeper-hook`, which runs
`mdm update-hook <step>`), as for every hooked service
([docs/conformance.md](../conformance.md) states the contract). Every step refuses a params
file the contract's reader refuses, or one whose `settings` is not `/etc/mdm/settings`, and
`prepare` refuses a release argument that is not the params' `updater.staged_release`.

- **`prepare`** checks the host as the kit's `install` did (root, x86-64, `MDM_BIND` carried by an
  interface, the ports free or held by the package's own units, the SCEP depot empty or holding a
  whole CA, no link at the state, settings or pkg directory), the staged release's `scepserver`
  and `lego` (by their staged paths: the package's binary directory runs through the daemon's
  link `current`, so it is not checked for a link), and `dns.env` when it is there. Then it makes, each only when the host differs: the user `mdm`,
  `/var/lib/mdm` and its subdirectories, `/etc/mdm`, `/var/lib/mdm-pkg`, the three secrets (each
  made once, never shown), the SCEP CA unless the depot holds one, the Caddyfile
  `/etc/mdm/Caddyfile`, and one drop-in `/etc/systemd/system/<unit>.d/settings.conf` for each
  unit that needs a site value: the slice's caps, the proxy's LAN, depsyncer's token name and the
  two certificate jobs' ACME settings. It starts nothing. What those last files held is kept in the
  hook's record first.
- **`cutover`** reloads systemd, enables the proxy and the timer, restarts the run units and the
  timer, restarts the proxy when it has its certificate and depsyncer when it runs, and starts the
  first certificate job when there is no certificate yet and `dns.env` is there.
- **`verify`** checks that the run units are active and each listens on its loopback port only,
  that nothing listens on port 80, the modes and owners of the state, settings and pkg
  directories and of the secrets, and that the Caddyfile and drop-ins are as the settings render
  them; with the certificate there, also the proxy and the timer, and the proxy on
  `MDM_BIND:443` only. A service that is not up yet makes the daemon ask again; any other failed
  check fails the update. The certificate's remaining validity, `dns.env` and the enrollment
  profile are printed in the step's log, not judged.
- **`rollback`** puts back the files `prepare` changed and restarts what runs on the version the
  daemon linked back; on a first install it stops the units and removes the links it made.
- **`uninstall`** (on unassign) stops every unit of the package, removes the links the hook and
  `dep define-profile` made, and moves the Caddyfile and the drop-ins to
  `/var/lib/mdm/removed/<unix seconds>/`. The state (database, SCEP CA, DEP token, TLS key and
  certificate), the secrets, the pkg directory and the user stay.

## The Apple-facing steps

Run as root on the MDM host, with the package's program and the same settings file. Each prints
its plan and changes nothing without `--apply`, as the kit's verbs of the same names do.

```sh
mdm=/var/lib/hostkeeper/packages/mdm/current/bin/mdm   # from the .deb: mdm=/usr/lib/mdm/mdm
$mdm push-cert --settings /etc/mdm/settings --cert /srv/apple/push.pem --key /srv/apple/push.key --apply
$mdm push-cert --settings /etc/mdm/settings --profile-only --apply
$mdm dep get-cert --settings /etc/mdm/settings --out /srv/apple/nanodep.crt --apply
$mdm dep upload-token --settings /etc/mdm/settings --token /srv/apple/token.p7m --apply
$mdm dep define-profile --settings /etc/mdm/settings --apply
$mdm dep assign --settings /etc/mdm/settings --serial <serial> --apply
```

Every file is named by an absolute path. Keep the push certificate's key, the token and the
certificates outside every git work tree.
