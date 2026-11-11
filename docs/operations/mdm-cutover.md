# Cutover to the coordinator's own MDM

This is the runbook of task CM10 of [coordinator-mdm.md](../design/coordinator-mdm.md) (section 7 there
says why and in what order; [operator-login.md](../design/operator-login.md) says how the first user
signs in). It moves the farm's Macs from the old MDM stack (the `hostkeeper-mdm-*` units: Caddy,
NanoHUB, the SCEP server, NanoDEP) to the MDM that is built into the coordinator. One operator runs
it, once, on the dev box. Every step that changes the machine is the held script
`mdm_cutover.sh`, a held script of the owner's deployment repository (dry run by default) or the owner's own
action in the coordinator's web app, an Apple portal or at a Mac. Nothing here changes a running
service until the owner runs the script with `--apply`.

This page describes the code of the pull requests named in section 1. A flag marked "open" is not in
any release yet; check each row against the release notes of the version you deploy before step 3.

## 1. Wait for the code, then for a release

The cutover starts only when every pull request below is merged and a release that contains them is
published (the coordinator's image and the macOS pkg are assets of the release).

| Part | Pull requests | State when this page was written |
|---|---|---|
| Operator login: the first-admin file, sessions, sign-in routes, Users page | #301, #302, #303, #311, #318 merged; #316, #317, #319, #320, #321, #323 open | open |
| MDM server: queue and listener (CM11a #331), pkg route (CM11c #332), SCEP library (CM12b #313, merged) and its routes `/enroll` and `/scep` (CM12c #337), APNs and push certificate (CM7 #329), ABM client (CM13 #328) | see the numbers | open |
| Setup: secrets sealed in the store (CM20 #324), `Mdm.Status` and `Mdm.PutName` (CM21 #327), setup page (CM22a #330, CM22b #333), `Mdm.Verify` (CM11b #335), ACME for the name (CM17 #334), tables (CM6 #312), named installs (CM8 #325), baseline (#290) | see the numbers | open |
| Approval from the ABM table (CM9), the TLS step of the page (CM22c), the package proxy's routes (PP2 #336) | not all have a pull request yet | to come |

Do not start before CM9 is merged if the Macs are to be approved without the serial allowlist; with
the allowlist (this runbook keeps it, section 3) CM9 is not needed.

Check before going on:

1. `gh pr list --repo komira-ai/hostkeeper --state open --search "CM"` shows none of the rows above.
2. `gh release view --repo komira-ai/hostkeeper` shows a release newer than the last merge of them.
   Its version is `X.Y.Z` in the rest of this page.
3. The release's coordinator must accept the flags of section 3 (`--mdm-listen` is the one that
   decides); an older one refuses to start with it, and the deploy puts the old container back.

## 2. What the cutover changes, and what it does not

| Before | After |
|---|---|
| Caddy holds port 443 on the LAN address and routes to NanoHUB, SCEP and the pkg directory | the coordinator's MDM listener holds port 443 on the same address |
| NanoHUB, NanoDEP, the SCEP server and `lego` run as units of the host | none of them runs; the coordinator is the only process |
| The coordinator's gate calls NanoHUB over loopback and receives its webhook | the gate queues its commands in the coordinator, and the Macs check in to it directly |
| Push certificate, ABM key and token, SCEP CA and the TLS certificate are files in `/var/lib/hostkeeper-mdm` | the push certificate, the ABM key and token and the DNS credential are uploaded in the web app and kept sealed in the coordinator's store; the SCEP CA and the certificate are made by the coordinator |
| Operators use the tokens file | the owner signs in as a user; the tokens file stays for CI |

Not changed: the coordinator's state and files volumes (never deleted by any run), the host CA and
every host's identity, the heartbeat and private listeners, the release channel, Apple's side (the
ABM server `js-mdm`, its default assignment, the push topic), and the old stack's state, units and
settings (they are kept for the back-out in section 8).

## 3. The flags

The held script builds the new coordinator's flags from the flags file of the present deploy (the
file the dev box's deploys are made with, `--old-args`). It removes three flags, adds the ones
marked "set" and leaves the others as they are. Flags marked "default" are not passed; the coordinator uses the
default.

Removed from the argv (shipped flags, still accepted by a coordinator that has no `--mdm-listen`):

| Flag | With `--mdm-listen` |
|---|---|
| `--mdm-api-url`, `--mdm-api-key-file`, `--mdm-webhook-listen` | not read; the gate's commands go to the coordinator's own queue and no webhook is served. The held script drops them so the deploy does not carry a NanoHUB key. CM14a later accepts and ignores them for a release, CM14c removes them |

Kept as they are (shipped): `--mac-serial-allowlist`, `--mdm-profile-identifier`,
`--mdm-daemon-config`, `--mdm-pkg-allowlist` (read once into the setting `installs`, #325, when that
setting was never set), `--mdm-host`, `--lan-listen`, `--tokens-file`, `--oidc-config`, the TLS and
client CA flags and the store flags.

New flags, with the pull request that adds each and its default:

| Flag | Pull request | The cutover | Default and meaning |
|---|---|---|---|
| `--mdm-listen IP:PORT` | open, #331 (CM11a) | set: the LAN address of the dev box, port 443 | none (no MDM listener). One interface address, no wildcard; needs the CA key flag and the enrollment store. It answers nothing until the setup holds the certificate of the MDM name |
| `--mdm-secrets-key-file FILE` | open, #324 (CM20) | set by `coordinator_docker.sh` to `mdm-secrets.key` in the coordinator's state directory (the state volume) | none (no MDM secret kept; `Mdm.PutSecret` is refused). The coordinator makes the file (mode 0600) when it is absent and the store holds no sealed secret; a missing or wrong key with sealed secrets refuses the start |
| `--mdm-pkg-dir DIR` | open, #332 (CM11c) | set: the host's pkg directory, mounted read-only (the old stack's, which holds the macOS pkg) | none (`/pkg/` answers 404). Files `<sha256>.pkg`, served for the digests the setting `installs` names. Needs `--mdm-listen` |
| `--mdm-command-timeout-s N` | open, #331 | default | 86400: how long an unanswered command waits before it is queued and pushed again |
| `--mdm-queue-sweep-s N` | open, #331 | default | 60: how often the queue looks for due commands and pushes |
| `--mdm-baseline-file FILE` | open, #290 (and #325) | not set (no baseline yet) | none. A TOML file of `[[group]]` rows: Screen Sharing and profiles per host group; needs `--mdm-profile-identifier` and `--mac-serial-allowlist` |
| `--mdm-baseline-check-s N` | open, #290 | default | 21600: how often a Mac's baseline is checked. Needs `--mdm-baseline-file` |
| `--initial-admin-file FILE` | open, #317 (L5) | set by `coordinator_docker.sh` with `--initial-admin NAME`: `initial-admin.toml` in the state directory, written by the script with a one-hour random password it prints once | none. The coordinator makes the user from the file at start and at each `--tls-reload-secs`, requires the user to change the password at first sign-in, and deletes the file when it has expired. One of `--tokens-file`, `--operator-token-file` or this flag is required |
| `--initial-password-secs N` | open, #317 | default | 3600 (1 to 3600): the longest validity the file may ask for |
| `--password-memory-kib`, `--password-iterations`, `--password-parallelism` | open, #317 | default | 19456, 2, 1: the Argon2id cost of a password hash |
| `--password-min-length N` | open, #317 | default | 12 (1 to 256) |
| `--session-idle-secs`, `--session-max-secs`, `--session-max-count` | open, #319 (L6b, the routes that use them; the limits are in #318, merged) | default | 1800, 43200, 256 |
| `--login-max-failures`, `--login-lockout-secs`, `--login-max-tracked` | open, #319 (limiter in #318, merged) | default | 5, 900, 4096 |
| `--login-max-hashes N` | open, #319 | default | 2: how many password hashes run at once |
| `--proxy-cache-dir DIR` | open, #326 (PP1) | not set: the package proxy is not part of the cutover | none (no cache). A directory of the coordinator's own for the proxy's cache (its size limit is a setting); `GET /pkg/<sha256>.pkg` uses it, when an `installs` entry has a `url` and the pkg is not in `--mdm-pkg-dir` (PP3): the pkg is read from the `url` once, checked against its digest and kept; the apt routes are #336 (PP2) |

There is no flag for ACME or DNS. The MDM name, the certificate (by ACME DNS-01 through GoDaddy, #334 CM17, or
uploaded) and the GoDaddy credential are setup data entered in the web app in step 5, and so are
the thresholds of the status panel. The other MDM tunables of the design are settings of the setup
page too. No further flag is added by the heads of #312 and #324 to #337, or by the CM9 branch (read from the
flag structs of those branches: `config.rs`, `mdm.rs`, `initial_admin.rs`); #335, #334, #333 and #337
add rpcs and routes, not flags.

Port 443 is below 1024 and the coordinator's container has no capability. Measured on the dev box on
2026-11-11, with the same helper image and options the deploy script uses: docker refuses a
per-container `net.ipv4.ip_unprivileged_port_start` when the container uses the host's network
(which it does, so that the coordinator binds the host's addresses itself), and `--cap-add
NET_BIND_SERVICE` does not let uid 65532 bind port 443, because docker sets no ambient capability
for a non-root user. So the cutover script writes the **host's** sysctl
`net.ipv4.ip_unprivileged_port_start=443` as a drop-in file under `/etc/sysctl.d` and applies it. That answers item 17 of section 12
of the design and the Q4 note of section 4.1. It has a cost to know: any local user, not only the
coordinator, may then bind ports 443 and up. The alternative is to give the container its own network
namespace and publish the ports, which changes how every listener is bound; it was not chosen. The
back-out removes the drop-in and restores the value the script recorded.

How the coordinator is deployed matters for later deploys. The dev box's coordinator is a plain
docker container made by `coordinator_docker.sh`. The script writes the new flags to a new file next
to the old one; **every later deploy must use the new file**, or the next deploy replaces the
container with one that has no MDM listener. The release workflow's deploy job (#322) does not touch this
container: it updates only the image of an existing Docker Swarm service, by digest, and passes no
flag, so a coordinator that is later moved to Swarm takes these flags and mounts from its service
definition.

## 4. Step by step

Do the steps in this order. Each says what must be true before the next.

### Step (a): wait

Section 1. Also read the dry run of step (c) with no `--apply` at least once; it prints the same
steps and checks that apply would run, and changes nothing.

### Step (b) and (c): stop the old stack, deploy the coordinator

One command does both, in this order, and stops at the first failure (exit 3 names what was done):

```
bash mdm_cutover.sh --old-args OLD_ARGS_FILE --version X.Y.Z --initial-admin NAME [--mdm-listen IP[:PORT]]
bash mdm_cutover.sh --old-args OLD_ARGS_FILE --version X.Y.Z --initial-admin NAME [--mdm-listen IP[:PORT]] --apply
```

The first form is the dry run. `OLD_ARGS_FILE` is the file of the flags the running coordinator was deployed with (it holds the
flags `coordinator_docker.sh` takes, among them the download directory and the helper image, so the script takes neither again);
its name ends in `.args`. `--mdm-listen` defaults to the address of the old flags' `--lan-listen`
with port 443. `NAME` is the owner's user name (1 to 32 characters of a-z, 0-9 and `-`). `--state-dir` (default: the directory
of the old flags file) holds the script's record and the new flags file; `--sysctl-dir` defaults to `/etc/sysctl.d`.

What apply does:

0. Writes the record `mdm_cutover.record` in the state directory: each old unit's state, the sysctl's old value and whether the
   script made its file. It holds no secret.
1. (b) Stops and disables the old units `hostkeeper-mdm-proxy.service`,
   `hostkeeper-mdm-proxy-reload.path`, `hostkeeper-mdm-cert.timer`, `hostkeeper-mdm-nanohub.service`,
   `hostkeeper-mdm-scep.service`, `hostkeeper-mdm-depsyncer.service` and `hostkeeper-mdm-depserver.service`, in the order
   cert.timer, proxy-reload.path, proxy, depsyncer, nanohub, scep, depserver; each must then be inactive. Port 443 on the LAN
   address must be free afterwards, or the script puts everything back. Their
   unit files and the state under `/var/lib/hostkeeper-mdm`, the settings
   `/etc/hostkeeper-mdm.conf` and the files under `/etc/hostkeeper-mdm` are not touched.
   Until the coordinator serves port 443 no Mac can reach an MDM server; `m3ultra-1` retries.
2. (c) Writes the sysctl drop-in of section 3 (`/etc/sysctl.d/61-hostkeeper-coordinator-mdm.conf`, only when the host's value is
   above 443) and applies it.
3. Writes the new flags file beside the old one (`coord.gate.args` becomes `coord.gate.mdm.args`); every later deploy must use it.
4. (c) Runs `coordinator_docker.sh apply` with the new flags. That script verifies the release's image and the
   macOS pkg against their attestations, replaces the container (a new container beside the old, the old
   kept until the new is ready), mounts the pkg directory read-only, passes
   `--mdm-secrets-key-file` and `--initial-admin-file`, and prints the first user's one-hour
   password once, on the terminal (the held script never sees it). **Copy the password now**; it is in no other file and no later
   output. If any step fails, the script starts the old units again as the record has them, removes the sysctl file it made and sets
   the value back, moves the new flags file to `.failed` and the record to `.backed-out`, and exits 3; the coordinator is what
   `coordinator_docker.sh` left (it keeps the old container when the new one is not ready).

The coordinator's start line names the MDM listener. Until the certificate of the MDM name is stored the
listener is bound and answers no TLS handshake; that is expected, step (d) fixes it.

### Step (d): sign in, change the password, set MDM up

Open the coordinator's web app on its private address (the one the owner uses today) and:

1. Sign in as `NAME` with the printed password. The app requires a new password at once (12
   characters or more). The one-hour file is deleted by the coordinator when it has expired; if
   you missed the hour, run `coordinator_docker.sh apply` with the words of the new flags file and `--reset-admin NAME` for a new one.
2. Open the MDM setup page. It says *not configured* and lists what is missing. Upload, in this
   order, what is below. Everything is checked at entry and is audited under your name; no key is
   shown again after upload. The files are root-only on the box: read them with `sudo`, copy to a
   file you own with mode 0600 in your own directory, upload, then delete your copy with the scratch
   removal tool. Never paste a key into a chat or a terminal log.

| Step of the page | What to upload or enter | Where it is today (by path; to confirm on the box before this step) |
|---|---|---|
| 1b' Push certificate and key | the PEM pair of the push certificate. The page checks that the key matches and that the topic is the one the enrolled Mac has | NanoHUB's storage: `/var/lib/hostkeeper-mdm/nanohub/mdm/push_cert/<2 hex>/<2 hex>/<topic>.pem` and `<topic>.key`, the topic being the one in `/var/lib/hostkeeper-mdm/push-topic`. Root-only. To confirm: that the `.pem` holds the leaf only or a chain the page accepts |
| 2a ABM key pair | NanoDEP's key and certificate for the server `js-mdm` (a takeover: no change in ABM, no device moves) | `/var/lib/hostkeeper-mdm/dep/dep_names/key.js-mdm` and `cert.js-mdm` (and `consumer_key.js-mdm`, `consumer_secret.js-mdm`). To confirm: the page takes PEM of the key and certificate as they are there |
| 2b ABM server token | the `.p7m` of the token, which the page decrypts with the key of step 2a | NanoDEP keeps the token's parts (`access_token.js-mdm`, `access_secret.js-mdm`, `consumer_*`) decrypted, not the `.p7m`. To confirm on the box: the original `.p7m` is the file ABM gave when the token was made; if it was not kept, take the fallback: step 2a' (the page makes a new key and shows its public certificate), register it in ABM for the `js-mdm` entry, download the new token and upload that. Whether ABM accepts a replaced key for the entry is not verified (design section 12, item 16). The token expires 2027-10-31 |
| 3 MDM name | the name in `MDM_DNS_NAME` (written `mdm.example.com` below) | `MDM_DNS_NAME` in `/etc/hostkeeper-mdm.conf`. It resolves to the LAN address of the dev box. Refused once any Mac has enrolled |
| 4 Certificate: GoDaddy credential, ACME directory, contact | the GoDaddy API key and secret; the ACME directory (Let's Encrypt production is the default; choose staging for the first try, because production rate-limits failures); contact `MDM_ACME_EMAIL` of the settings file | the key and secret are in `/etc/hostkeeper-mdm/dns.env` (root-only). Open it with `sudo` and copy the two values by hand into the page. Do not print the file in a terminal that is logged. Or upload a certificate and key for the name, from `/var/lib/hostkeeper-mdm/lego/certificates/` (`mdm.internal.komira.dev.crt`, `.key`), which is valid until 2027-02-05, as a fallback |

3. When the status panel says *configured* (the push certificate present and not expired, the ABM
   token accepted with a first sync done, the name set, a certificate for the name stored), take
   the backup of the secrets key before anything else:

   ```
   umask 077
   docker run --rm --mount type=volume,src=hostkeeper-coordinator-state,dst=/state,readonly --user 65532:65532 HELPER_IMAGE cat /state/mdm-secrets.key > SECRETS_KEY_COPY
   ```

   Keep `SECRETS_KEY_COPY` apart from the database's backups. **If the key file is lost, the coordinator
   refuses to start with sealed secrets it cannot open; the remedy is a new key, which makes a new MDM CA, so every Mac
   re-enrolls (an erase), and the push certificate, the ABM token and the DNS credential are uploaded again.**

### Step (e): verify

1. Run `Verify` on the setup page (#335, `Mdm.Verify`): it reads the setup back, asks Apple about the ABM
   token and sends a test push to the farm Mac that is still enrolled in the old stack. Each line
   must pass. A push to `m3ultra-1` proves the push certificate (the Mac is not yet talking to the
   coordinator).
2. From a LAN machine: `curl -sSI https://mdm.example.com/enroll` (with the name of step 3) answers over TLS with a
   certificate that a stock client trusts (`curl` needs no `-k`). A TLS failure here means the certificate step did not finish.
3. The status shows the ABM last-sync time, no last error, and the serials ABM lists. If `js-mdm`
   lists none, the devices are not assigned yet (step (g)).
4. The coordinator's heartbeat and private listeners are live and the hosts are live: the hosts list of the web
   app shows the farm's hosts as before.

Do not go on while any of these fails; use the back-out (step (h)), which is still free.

### Step (f): `m3ultra-1`, serial P24LH9H6JT

It is enrolled in the old stack, with an identity of the old SCEP CA. Decision B: it re-enrolls. A
takeover of its identity is not offered, because it would have to trust the old CA and could not be
rolled back (design section 7).

1. In ABM, confirm the serial is assigned to `js-mdm` (the coordinator's sync assigns the ADE
   profile "Komira MDM" to the devices it lists, once the setup is configured).
2. Erase the Mac (System Settings, Erase All Content and Settings, or the coordinator's erase from the
   Devices page once it lists the Mac; the old stack's NanoHUB is stopped, so use the Mac's own erase).
   On the same wired LAN, Setup Assistant fetches the enrollment from the MDM name.
3. Check in the web app: the Devices page lists the Mac, enrolled and supervised, with host id
   `m3ultra-1-p24lh9h6jt`; the audit shows `mdm_install_sent` and the nonce push; the host appears
   approved with no operator (by the serial allowlist, or by the ABM table once CM9 has landed). The
   old host `mac-1` stays as a stale host.
4. **From the Mac's first `TokenUpdate` to the coordinator, the back-out is no longer free** (step (h)).

### Step (g): `m3ultra-2` and the boxed Macs

Before the owner touches a Mac:

1. Get its serial (box label or ABM). Add a `[[mac]]` entry to the allowlist file the deploy names
   (until CM9 approval from the ABM table is released), and redeploy with the new flags file (the
   coordinator reads the allowlist only at start).
2. In ABM, set the default server in Device Management Settings to `js-mdm`, and assign the serial
   to `js-mdm`. A Mac that was not bought through the org's Apple account is added with Apple Configurator, from another Mac, at
   its Setup Assistant.
3. **Check that the ADE profile is assigned to the serial before pressing Activate Mac**: the setup
   page's device list (after the coordinator's sync) shows the profile, not "not accessible".
4. At the Mac: wired Ethernet on the LAN, whose DHCP DNS resolves the MDM name; power on. Setup Assistant takes the
   enrollment and skips its panes. Check as in step (f), item 3, and that the Mac then installs hostkeeper's pkg (fetched from
   `https://<name>/pkg/<sha256>.pkg`) and its daemon posts to the coordinator.

### Step (h): back-out, and what is deleted later

```
bash mdm_cutover.sh --back-out --old-args OLD_ARGS_FILE
bash mdm_cutover.sh --back-out --old-args OLD_ARGS_FILE --apply
```

Apply starts and enables the NanoHUB side of the old stack the record names (depserver, scep, nanohub, depsyncer), redeploys the
coordinator with the old flags file (same script, same volumes; the old container spec returns, which frees port 443), starts and
enables the proxy, proxy-reload.path and cert.timer, removes the sysctl drop-in the script made and restores the value it recorded,
and moves the record to `mdm_cutover.record.backed-out`. It deletes nothing else: the new flags file, the coordinator's secrets, the
users, the MDM CA and the device records stay.

The limit: the back-out is free only until the first Mac's first `TokenUpdate` reaches the
coordinator. After it, NanoHUB's copy of that Mac is stale. The way back then is to erase the Mac and
enroll it into the old stack again. The old stack's state is untouched either way, so that is
possible; it is the Mac that pays.

After a week with every Mac green, the deletions of the design follow, each its own pull request:
CM14a (the NanoHUB client and webhook; the three NanoHUB flags accepted and ignored with a
logged deprecation for one release), CM14b (the kit, its pins, the `mdm` deb and the kit tarball),
CM14c (the three flags and the allowlist flag, one release after CM14a and CM9). The old units and the
host's files are removed by the owner's own held script, not by any pull request, and only after that week.
