# Coordinator MDM: the MDM server inside the coordinator

Status: design, for the maintainers' comments and the owner's. Nothing in this document is built unless it is tagged **Built** with a
path. It is a living document: when a task below lands, its row changes from **Designed** to **Built** with the path, and the text of the
decision stays.

The owner's direction, in their words: "it's a better model that we just package MDM in the coordinator and then we are in full
control." The target is this path and no other step:

> buy a Mac on the org's Apple account, power it on, it enrolls, it gets hostkeeper, it talks to the coordinator, it receives its
> deployments.

No separate MDM stack for an operator to run, no per-serial list to redeploy, no Mac setting applied by hand.

This document supersedes [hostkeeper-mdm.md](hostkeeper-mdm.md) in part. Section 1.3 says which parts. Section 4 of that document (the
daemon's config profile), section 5 (the nonce) and section 7 (the pkg, its signature and its delivery by digest) stand. The gate, its
device record and its decline tokens stand. What goes is the third-party server stack, the kit that installs it, and the loopback hop
between the stack and the gate.

## 1. What an operator sees

### 1.1 Before and after

| | Today (**Built**, as run on the farm) | After this design |
|---|---|---|
| Processes on the MDM host | The coordinator, and five more: `nanohub`, `depserver`, `depsyncer`, `scepserver`, Caddy with `lego` | The coordinator |
| Installed from | the coordinator image, the `mdm` deb or the `hostkeeper-mdm` kit tarball, and four pinned Go tools | the coordinator image |
| A new Mac | its serial is added to ABM, to the coordinator's serial allowlist (a redeploy) and the ADE profile is assigned by hand before the Mac first reaches Activate | the Mac is bought on the org account; nothing else |
| Which Macs are trusted | the serials in `--mac-serial-allowlist` | the serials Apple's ABM lists for the org's MDM server, plus supervision and the nonce; an inventory record may name the Mac |
| Name, group | per allowlist entry | from the Mac's inventory record by serial, else the default rule (section 4.6) |
| Screen Sharing, device name, Tailscale, extra profiles | by hand, or by a hand-made NanoHUB API call | the group baseline, applied at enrollment and re-applied (section 5.3) |
| Push certificate, ABM token | uploaded into NanoHUB and NanoDEP storage | held by the coordinator; the operator renews them with `hkctl mdm` |

What does not change: the Mac sees the same two things it sees today, an ADE profile that points at `https://mdm.internal.komira.dev/enroll`
and an MDM server on that name. The daemon's enrollment, its nonce, its approval record and its host certificate are unchanged. The
operator API and `hkctl` gain verbs; they lose none.

### 1.2 Where this came from

The stack in section 2 was proven live on `m3ultra-1` (serial `P24LH9H6JT`): supervised enrollment, `DeviceInformation`,
`EnableRemoteDesktop` and `Settings` (`DeviceName`, `HostName`) all worked through NanoHUB's API. Six things went wrong or were learnt on
the way (section 6 handles each by a mechanism). The owner's conclusion was that the operational shape, five foreign processes and the
gate on the far side of a loopback webhook, is the problem, so the server moves into the process that already owns the inventory, the
groups, the CA, the store and the operator API. The related designs are the console design (hostkeeper#284), whose baseline reconciler is
reused here, and the baseline pull request (hostkeeper#290).

### 1.3 What this supersedes in hostkeeper-mdm.md

| Section of hostkeeper-mdm.md | Status after this document |
|---|---|
| 2 State today: the rows for the server stack, TLS, ABM token, ADE profile, MDM kit | Superseded: those pieces are replaced (sections 3 to 5 here). The gate, coordinator-side and daemon-side rows stand |
| 3 Components: the rows "The MDM server stack" and "`hostkeeper-mdm` kit"; the paragraph that keeps NanoHUB's API on loopback | Superseded. "The gate is linked into the coordinator" stands and is extended: the queue it enqueues into is in-process. "What a compromise gets" is amended in section 9.2 |
| 4 Where the daemon config comes from | Stands |
| 5 The nonce | Stands. Only the source of "enrolled and supervised" changes (6.1 below) |
| 6.1 How the gate knows "enrolled and supervised" | Superseded in its source (the webhook becomes a call); its rules stand. 6.2 and 6.3 stand with section 6 here |
| 7 The pkg | Stands; 7.1's Caddy `/pkg` route becomes the coordinator's route (4.7 here) |
| 8 Migrating the live stack | Superseded by section 7 here. The `migrate --from` verb of the kit is not used again |
| 9 DNS for the MDM host | The record stands; the parts about Caddy's bind and `lego` are superseded by 4.1 |
| 10 Tests | The kit's tests go with the kit; the gate's stand |
| 12 Tasks M2 to M5, M6a, M6b, M7 | Superseded by section 11 here. M8's NanoHUB client and webhook are replaced (CM2, CM11, CM14) |
| 11, 13 | Stand; Q-numbers carry over |

## 2. What exists today

Facts, with the file that says them. The rest of this document is about the difference.

- **The stack** (the farm's MDM host, as run, from the audit of the go-live and its runbook): NanoHUB (NanoMDM, NanoCMD and KMFDDM behind one
  JSON API and a webhook), `depserver` and `depsyncer` (NanoDEP), `scepserver`, Caddy terminating TLS on the LAN address with a
  `lego` certificate for `mdm.internal.komira.dev`. The coordinator talks to NanoHUB on loopback and receives its webhook on a loopback
  listener.
- **In this repository** (**Built**): `crates/hostkeeper-mdm-gate` (the NanoHUB client, the webhook, a durable record per Mac, the config
  profile, the nonce, the pkg push), `crates/hostkeeper-mdm-core` (profile and unit rendering, the pins), `crates/hostkeeper-mdm` (the
  install kit), `crates/hostkeeper-mdm-step`, and in the coordinator `src/mdm.rs`, `src/operator_mdm.rs`, `src/enrollment/mac_nonce.rs`,
  `src/enrollment/gate_port.rs`, `src/enrollment/device_state.rs`. The three kit crates together are roughly 12,000 lines of Rust whose
  only job is to install, pin and operate third-party Go binaries (measured with `wc -l` over `src/`: core 3,067, kit 6,833, step 2,096).
- **The coordinator's listeners** (**Built**): heartbeat, private, health, and `--lan-listen`, which serves the heartbeat surface on the
  LAN (`crates/hostkeeper-coordinator/src/config.rs`, `listen.rs`).
- **The coordinator's store** (**Built**): Postgres with `mdm_devices` (migration 0005, `older-releases: may-run`) or the file store;
  inventory records with a unique serial (coordinator-storage.md); host groups (hostgroups.md).
- **What is not there**: any SCEP, ACME, APNs, ABM or plist-protocol code, and any RSA or CMS crate in the workspace.

## 3. Components

![Components: the MDM server inside the coordinator](diagrams/coordinator-mdm-components.svg)

The diagram boxes exactly what hostkeeper runs: one process. Everything that moves into it, and why it needs no process of its own:

| Piece | Replaces | Why it is not a process |
|---|---|---|
| MDM listener: `/enroll`, `/scep`, `/checkin`, `/mdm`, `/pkg/<sha256>.pkg` | Caddy's routes, NanoHUB's endpoints, `scepserver`, the pkg route | five request handlers over `hyper`, which the coordinator already runs |
| Check-in and queue | NanoMDM | a table of enrollments and a table of commands |
| SCEP server and the MDM identity CA | `scepserver` | one request type (`PKIOperation`) over a CA the coordinator already knows how to run |
| APNs sender | NanoMDM's push | one HTTP/2 POST per wake, on `hyper` and `rustls`, which the workspace already has |
| ABM client and sync | `depserver`, `depsyncer` | one timer and six HTTPS calls |
| Gate | the same gate, one hop closer | called by the queue; no NanoHUB client, no webhook |

A new crate, `hostkeeper-mdm-server`, holds the protocol: the wire types, the queue, the SCEP request, the APNs and ABM clients. It
reaches the world only through ports (a store, a clock, an APNs sender, an ABM transport), each with a production implementation and a
fake, so the tests of section 10 are hermetic and deterministic (T10, T12). The coordinator links it, as it links the gate.

## 4. Decisions

Each has the reason and what it costs. "Verify" marks a claim that rests on the Apple schema or on source reading and not on a run; section
12 collects them.

### 4.1 Which endpoints, on which listener

**Decision.** A new fourth listener, `--mdm-listen <ip:port>` (a LAN interface address, as `--lan-listen` is checked: no wildcard), a new
`http::Surface::Mdm`, TLS from `--mdm-tls-cert-file` and `--mdm-tls-key-file` with the certificate of `mdm.internal.komira.dev` issued by a
public CA. Routes:

| Route | Who calls it | Authentication |
|---|---|---|
| `GET` and `POST /enroll` | Setup Assistant (the ADE profile's `url`) | none at TLS; the POST body is the Apple-signed device-information blob, checked (4.2) |
| `/scep` | the Mac's SCEP client, from the enrollment profile | a one-time challenge bound to the serial |
| `PUT /checkin` | the MDM client | a client certificate of the MDM CA, or the `Mdm-Signature` header; either way the certificate's hash must be the one stored for the enrollment |
| `PUT /mdm` | the MDM client | the same |
| `GET /pkg/<sha256>.pkg` | the MDM client, following `InstallEnterpriseApplication` | the digest is the capability: only files named in the installs file |

**Why a listener of its own and not routes on the heartbeat or LAN listener.** The heartbeat surface admits any client certificate of the
host CA and is pinned by every daemon's `ca_sha256`. A Mac at Setup Assistant holds no certificate and trusts only public roots, so its
listener needs a public-CA server certificate and a different client-trust rule. Separating the listeners keeps the host CA's meaning
unchanged (B8) and keeps the Mac-facing surface to five routes. `--lan-listen` stays for the daemons. Lesson 4 is this decision.

**Why public-CA TLS.** A freshly erased Mac trusts the system roots only. The ADE profile's `anchor_certs` is empty today and stays so;
putting a private CA in it is a second trust path to maintain, and the name `mdm.internal.komira.dev` already has a public-CA certificate
today. The DNS record of hostkeeper-mdm.md section 9 stays, pointing at the new listener's address.

**Certificate renewal.** v1 reads the two files and reloads them on the coordinator's existing reload tick, the way it reloads
`--server-cert-file` (**Built** for that file; Designed for this one). Something must write the files: `lego` on a timer does today, and
the first cut may keep that one unit and no Caddy. This is the one place the "no separate unit" goal is not met at first, and the
document says so rather than hiding it. Task CM17 moves renewal in: DNS-01 through `instant-acme` (TLS-ALPN-01 needs the Let's Encrypt
validator to reach a LAN address, so it cannot be used for this name), with the provider hook the `lego` setting uses now.

### 4.2 Device identity: SCEP, issued by its own CA, one identity per serial

**Decision.** SCEP (RFC 8894) served by the coordinator, with a new MDM identity CA that has its own RSA 2048 key at
`--mdm-dir/mdm-ca.key`. The identity's subject names the serial: `CN=<serial>`, `O=<org>` and an `OU=hostkeeper-mdm`, so there is no
shared empty DN. The challenge is per request: `POST /enroll` verifies the device-information blob, finds the serial in the ABM table,
makes a 32-byte single-use challenge valid for one hour, and builds the enrollment profile with that challenge. `PKIOperation` consumes
the challenge in the same transaction that records the issued certificate.

**Why SCEP and not the alternatives.**

| Option | For | Against |
|---|---|---|
| SCEP (decided) | `com.apple.security.scep` since macOS 10.7; what the live stack runs and proved; one request type | needs RSA and CMS in the workspace (`rsa`, `cms`; the RSA advisory is not a blocking one under R6); a CA of its own |
| Per-device PKCS#12 in the enrollment profile | no SCEP code; P-256 possible | the CA generates the private key and ships it inside a profile; whether macOS accepts P-256 as the MDM identity is not known. Verify. Rejected for the key exposure |
| ACME with `Attest` and `HardwareBound` | no shared secret; the certificate is bound to Apple-silicon hardware and the serial; the schema gives `com.apple.security.acme` since macOS 13.1 | no Rust ACME server crate was found; and whether `IdentityCertificateUUID` of `com.apple.mdm` accepts an ACME payload during ADE is not stated by Apple's schema, which says only that it "may also point to a SCEP payload". Verify on the farm Mac. Follow-up CM16 |

**Why a CA apart from the host CA.** The host CA is P-256 only (`ca/profile.rs` refuses other key types), its certificates are
admitted on the mTLS listeners, and every daemon pins it. An MDM identity is RSA, ships to a Mac before it is approved, and must never be
accepted by the heartbeat listener. A second key is additive and changes no pin.

**Verification of the enrolling device.** The ADE POST to `/enroll` carries a PKCS#7 signed by the device's Apple-issued identity. The
coordinator verifies it against Apple's root and takes the serial and UDID from it, so a client on the LAN cannot ask for a challenge for a
serial it does not own. Verify against a capture from the farm Mac; if the signature cannot be checked, `/enroll` does not issue a challenge
and the design falls back to a pre-registered serial only (the ABM table), which is weaker, and this document says so in 9.2.

**Renewal.** Identity validity is `--mdm-identity-days`, default 1095 (a tunable, with its default from the start). At `--mdm-renew-alert-days`
(default 90) before expiry, the coordinator raises an alert listing the serials, through the path the push certificate uses (4.8). Whether
macOS renews a SCEP identity inside the MDM payload by itself is not established by anything read. Verify on the farm Mac. If it does not,
renewal is an `InstallProfile` of a fresh enrollment payload for the same topic (Apple's schema says the topic must never change on
update, and it does not), which is one more baseline item. Lesson 2 is this subject rule.

### 4.3 APNs push

**Decision.** An in-process HTTP/2 client on `hyper` and `rustls` (both already in the workspace, `ring` provider), with the MDM push
certificate and key from `--mdm-dir/push.pem` and `push.key` as the TLS client identity. `POST https://api.push.apple.com/3/device/<hex token>`
with `apns-topic` the topic of the certificate, `apns-push-type: mdm`, body `{"mdm":"<PushMagic>"}`. One connection is kept and reused.
Verify the header and status details against Apple's documentation, which could not be fetched while this was written; they rest on
Apple's schema, the `a2` crate's source and NanoMDM's behaviour.

**Why not the `a2` crate.** It pins rustls 0.22 and `hyper-rustls` 0.26, a second TLS stack next to the workspace's rustls 0.23, and its
certificate path needs OpenSSL. The in-house client is a few hundred lines with a fake behind the port.

**Rules.**

- One push per wake, and at most one outstanding per enrollment inside `--mdm-push-retry-s` (default 3600): the Mac, once woken, drains
  its whole queue on one connection, so more pushes only cost requests. `NotNow` keeps the command and does not re-push inside the window.
- A `410` marks the token invalid and stops pushing it; the next `TokenUpdate` replaces it. A `403` raises the push-certificate alert (the
  certificate is expired or revoked). `429` and `503` retry with the window as the floor.
- A command that waits without a push-capable token waits for the Mac's own `Idle`, which a Mac sends after a boot, and when a user is
  logged in, on its own schedule.

### 4.4 The push certificate: where it comes from and how it renews

The push certificate is the only dependency Apple does not let the coordinator remove, and this document does not hide that.

**Facts established from Apple's page and the MicroMDM guide.** A push certificate is created at identity.apple.com from a CSR that must
be signed by an Apple "MDM vendor CSR signing certificate", which the Account Holder of a Developer Program account requests from Apple.
Community signers (mdmcert.download) exist and the MicroMDM guide labels them for testing and development. ABM does not issue push
certificates. It lasts one year, and it is renewed with the same Apple Account that created it and the same topic. The org's certificate
has the topic `com.apple.mgmt.External.d1801ae1-3ef7-483e-9f6d-4979e2ebfc09`; the audit infers expiry about 2027-11-07 and did not read it.

**Decision.**

1. The coordinator **carries over** the existing certificate and key (section 7), so no new certificate is needed for the migration and the
   topic in every enrollment never changes.
2. The coordinator owns the lifecycle from then on: `hkctl mdm push-csr` creates a key and a CSR in `--mdm-dir` and returns the CSR;
   `hkctl mdm push-cert put` accepts the PEM Apple returns, checks that its topic equals the current topic on a renewal, and swaps it in
   atomically; `Mdm.Status` reports topic and expiry. The coordinator's existing alert path raises an alert at `--mdm-push-alert-days`
   (default 60) before expiry and again at 14.
3. **Signing the CSR is outside the coordinator**, by whoever holds the vendor certificate. This needs the owner's answer (Q1): which route
   produced the current certificate. If the vendor route, the vendor certificate exists and the yearly renewal is a 10-minute task. If
   mdmcert.download, the renewal route is the one the owner's account used and its availability must be confirmed before 2027-09.

### 4.5 ABM sync, in process

**Decision.** A task in the coordinator, single-flight under the store's lease, every `--mdm-abm-sync-s` (default 300). It speaks to
`https://mdmenrollment.apple.com` with OAuth 1.0a (HMAC-SHA1, the four-part token), exchanging it for an `X-ADM-Auth-Session` that it
caches and refreshes. HMAC-SHA1 is about sixty lines on `ring`; no OAuth crate is needed.

- **Token.** ABM hands over an encrypted `.p7m`. The coordinator generates the RSA key and self-signed certificate for the ABM server
  (`hkctl mdm abm-cert` returns the certificate to upload in ABM), decrypts the returned file with `cms` and `rsa`, and keeps the four
  parts in `--mdm-dir/abm-token.json` (0600). The migrated token is read from NanoDEP's storage instead (section 7). Expiry is read
  from the token and alerted at `--mdm-abm-alert-days` (default 60). The live token expires 2027-10-31.
- **Sync.** `POST /devices/sync` with the stored cursor, new serials upserted into `abm_devices`, devices that ABM reports as removed marked
  so, then `PUT /profile/devices` for every serial that has no profile assigned. A first run, an invalid cursor or one older than seven
  days does a full fetch (`POST /server/devices`), which also returns Macs already assigned. NanoDEP's assigner is the pattern.
- **The ADE profile** is defined at start if its stored digest differs from the rendered one, from the existing `AdeProfile` renderer
  (**Built**, `crates/hostkeeper-mdm-core/src/profile.rs`, moved into the new crate), `url` being `https://<--mdm-host>/enroll`.
- **ABM's own default device assignment** (Mac devices of a purchase go to our MDM server without a click) is set once in the ABM
  console; that is Apple's side and its menu path is for the owner to confirm in the tenant (Q2). With it, a purchase reaches our list with
  no operator action; without it, the operator assigns in ABM, which is the same one click they do today.

**Cost.** One external request every five minutes when nothing changed, one more per page of changes, one per profile assignment batch.
That is 288 requests a day idle. The interval is a tunable so the cost and the window below can be traded.

### 4.6 Auto-approval of supervised ABM Macs, and how a host gets its name, role and group

The allowlist did three jobs: it said "this serial is ours", it gave a host id, and it gave a group. Each now has a source.

| Job | Today | After |
|---|---|---|
| "ours" | serial is a `[[mac]]` entry | serial is in `abm_devices` for our ABM token, not marked removed |
| host id | `host_id_prefix` plus the lower-cased serial | the inventory record for the serial, if any (`host_id`); else `--mdm-host-id-prefix` plus the lower-cased serial |
| group | the entry's `host_group` or `role` | the inventory record's `role` read as the group (as the allowlist's `role` is today); else `--mdm-default-group`; when neither exists the request is declined `no_group` and waits for an operator |

`mac_auto_approve` keeps its order and its nine decline tokens (**Built**, `crates/hostkeeper-enrollment/src/enroll/mac_nonce.rs`). Only
its first lookup changes, and these are added, additively: `serial_not_in_abm`, `not_via_dep`, `no_group`. The conditions for approval
are all of:

1. the serial is in the ABM table (Apple's attestation of ownership; a stronger claim than "a Mac says it is supervised");
2. the MDM enrollment is the current one for that serial, supervised per `DeviceInformation` `IsSupervised`, and `SecurityInfo` says
   `EnrolledViaDEP` (the gate records both today; **CM3** passes `enrolled_via_dep` through `DeviceStatus`);
3. a group resolves;
4. the nonce, as today: single-use, one hour, bound to the request and the serial, which proves the pkg installed and the daemon runs.

**Name from inventory.** The baseline's `Settings` command sets `DeviceName` to the host id and `HostName` to the same lower-case DNS-safe
string. A Mac without an inventory record gets the default rule's id. Changing the inventory's `host_id` changes the baseline target and
the next re-apply (5.3) sends the new name. The schema's `Settings` entry for macOS is device channel and not user-enrollment, and
Apple's text says `DeviceName` is available on supervised devices only; supervised is what this design assumes anyway.

**The old allowlist** stays one release as an optional override that wins for the serials it lists (B8: no format is removed), and is
deleted in CM14. A deployment that holds no host groups keeps the allowlist as its only source, because services come only from groups or
from allowlist entries.

**What the owner gives up.** Every Mac in the org's ABM that reaches the MDM server is eligible. The safeguard is `--mdm-default-group`:
leave it unset and a Mac with no inventory record is declined `no_group` and waits for an operator, which is today's behaviour for an
unlisted Mac, one click instead of a redeploy.

### 4.7 The hostkeeper pkg, Tailscale and extra profiles

**Pkg delivery** is `InstallEnterpriseApplication` with the inline manifest the gate sends today (**Built**,
`crates/hostkeeper-mdm-gate/src/pkg.rs`), whose `url` is `https://<--mdm-host>/pkg/<sha256>.pkg`, now served by the MDM listener from
`--mdm-pkg-dir`, streamed with a `Content-Length` and the digest checked, never buffered whole (the pattern of
`repository_serve.rs`). The manifest stays inline: the command schema offers `ManifestURL` too, but an inline manifest needs no second
fetch and no second route. The pkg allowlist file becomes `--mdm-installs-file`, a list of named installs (`[[install]] name,
bundle_id, sha256, version, path`). `pkg.rs` hard-codes `dev.hostkeeper.hostkeeper`; CM8 makes it a list, so a second pkg needs no
code. Additive: the old single-pkg file is read as one install named `hostkeeper`.

**Tailscale** (owner direction: the Mac reaches the tailnet-only release channel). It is a second install plus two profiles from the
baseline: a `com.apple.system-extension-policy` payload allowing the Tailscale-signed extension (the standalone variant needs it) and a
`com.apple.vpn.managed` payload that pre-installs the VPN configuration, from Tailscale's MDM page. Open and **not decided from
anything read**: the Team ID (Tailscale's page and a vendor forum thread give two different values; the only authority is the signed pkg's
own Team ID, to be read off the pkg), the pkg's download URL, and above all **how an unattended Mac joins the tailnet**. Nothing read
says Tailscale takes an auth key from a profile. The recommended mechanism, for the owner's decision (Q3): a per-device one-off
pre-authorized, tagged auth key with a short expiry, minted by the coordinator through Tailscale's API with an OAuth client, delivered to
the Mac in the per-device profile that the gate already manages and removed once the Mac appears on the tailnet. This puts a secret in a
profile, which hostkeeper-mdm.md and the baseline design forbid for the daemon's config, so it is an explicit exception, bounded by a
one-hour expiry and one use. Task CM18, blocked on Q3 and on a run on the farm Mac.

**The ordering that makes lesson 5 harmless.** The hostkeeper pkg carries the daemon binary (**Built**, `packaged.rs` seeds it), so the
first daemon runs and enrolls over the LAN listener with no tailnet and no release channel. The tailnet is needed only for what the
daemon later fetches. The coordinator installs hostkeeper first and Tailscale second, so a Mac with no tailnet still ends up approved
and visible.

**Extra profiles** are baseline items (the baseline pull request's `Item::Profile`): a profile file per group, installed by
`InstallProfile`, checked by `ProfileList`, re-sent on drift.

### 4.8 Commands or DDM for the baseline

**Decision.** Commands now, DDM later.

- **Commands** are proven live and each needed item has one: `InstallProfile`, `InstallEnterpriseApplication`, `Settings`
  (`DeviceName`, `HostName`), `EnableRemoteDesktop`, `ProfileList`, `DeviceInformation`, `SecurityInfo`. `EnableRemoteDesktop` is
  supervised-only, device channel, macOS 10.14.4 and up, and turns Remote Desktop on for all users with observe and control; its
  declarative cousin, `com.apple.configuration.screensharing.host.settings`, sets limits only and has no enable switch, so even under DDM
  this stays a command.
- **DDM** adds a server declaration store and a sync token, and the Mac reports status itself, which would replace the `ProfileList`
  poll. It also offers `com.apple.configuration.package` (macOS 26 and up, supervised) for the pkg. It is a second state machine, so it
  is a later task (CM15) that is model-checked first. The farm Macs run macOS 27, so nothing blocks it later.

The gate's command list grows from five to eight types; the test that pins it (**Built**, `the_gate_enqueues_only_its_five_command_types`
in `crates/hostkeeper-mdm-gate/tests/gate.rs`) changes with CM8 and states the new list.

## 5. How each operation works

The budgets count requests, as measured by the structure of the handlers (what each one calls) and not by a run. They are **designed**; the
times are targets to be measured under CM11 and CM12, not measurements. A store request is one round trip to Postgres.

### 5.1 ADE enrollment end to end

![ADE enrollment end to end](diagrams/coordinator-mdm-ade-enrollment.svg)

| Step | Store requests | External requests | Target server time |
|---|---|---|---|
| ABM sync finds the serial and assigns the profile | 2 (cursor, upsert) | 2 to Apple | background |
| `POST /enroll` | 1 write (challenge) | 0 | under 50 ms plus the store |
| SCEP: capabilities, CA certificate, `PKIOperation` | 1 write (consume challenge, record identity) | 0 | `PKIOperation` is one RSA 2048 private operation plus a signing, under 100 ms |
| `Authenticate` and `TokenUpdate` | 1 write each (upsert, supersede) | 0 | under 50 ms plus the store |
| gate enqueues, the first push | 2 (record, command) | 1 APNs | background |
| each command drain | 1 claim, 1 result write, 1 gate record write | 0 | under 50 ms plus the store |

The sequence is the same for a Mac the ABM already listed and one that was added a minute ago, with one constraint that no software
here can remove: the ADE profile must be assigned before the Mac first reaches Activate (6.1).

### 5.2 A command round trip with push

![A command round trip with push](diagrams/coordinator-mdm-command-round-trip.svg)

Per command: 5 store requests (gate record, command insert, claim, result, gate record) and 1 APNs request when no push is outstanding;
a burst of N commands costs N times 4 plus 1 of each. The claim is one statement, `UPDATE ... WHERE id = (SELECT ... FOR UPDATE SKIP LOCKED)
RETURNING`, so two connections of one Mac cannot take the same command. The gate writes its record before the enqueue and matches a result
only to a command UUID it sent (**Built**, `gate.rs`); the in-process source removes the unsigned webhook that rule defended against, and
the rule stays as depth.

Failure modes: APNs down, the command waits and the next `Idle` or a later push drains it; the Mac `NotNow`, the command is kept
and not re-pushed inside the window; the Mac `Error`, the result is final and the gate decides whether to re-send under its own rule; a
coordinator restart mid-command, the row is `Sent` and is re-sent on the Mac's next `Idle` because a `Sent` row whose result never came is
returned to the queue after `--mdm-command-timeout-s` (default 86400).

### 5.3 Baseline re-apply

![Baseline re-apply](diagrams/coordinator-mdm-baseline-reapply.svg)

The reconciler is the baseline pull request's (**Built** on that branch, not on `main`: `crates/hostkeeper-mdm-gate/src/baseline.rs`), per
host group: remote desktop, profiles, now also the device name and the installs. A tick (`--mdm-baseline-check-s`, default 21600) or a
baseline-digest change enqueues `ProfileList` and, when the name is unknown, `DeviceInformation`; only a difference enqueues a fix. A
quiet Mac costs 2 commands and 1 push per six hours.

### 5.4 A wiped Mac re-enrolling

![A wiped Mac re-enrolling](diagrams/coordinator-mdm-wiped-mac.svg)

A serial has one live enrollment. The new `Authenticate` marks the old row replaced and its certificate hash is no longer accepted. The
gate's record is reset by `Event::Authenticate` (**Built** behaviour of the gate), and the daemon's re-enrollment replaces the old host id
under the existing rule (**Built**: `enrolled_since_unix` newer than the old approval and the same serial). No allowlist redeploy and no
operator action are involved.

### 5.5 ABM sync

![ABM sync](diagrams/coordinator-mdm-abm-sync.svg)

## 6. Edge cases, with the mechanism that handles each

**The six live lessons.**

1. **A Mac must be assigned in ABM and get the ADE profile before it reaches Activate Mac, else setup never asks again.** Mechanism: the
   sync assigns the profile to every serial that has none, at most one interval after ABM lists it, and ABM's default assignment (4.5)
   puts a purchase on our server with no click. Remaining window: a Mac powered on inside that interval, and a Mac that is already past
   Activate. Narrowing: the interval is a tunable and the assign call is one request. For a Mac that missed it, the recovery (assign, then
   erase so it activates again) is the operator's and is confirmed on the farm Mac before this is documented as working. The coordinator
   raises an alert for a serial that is listed and has no profile after two intervals.
2. **SCEP `-allowrenew 14` with a shared empty subject DN refused every Mac after the first ("DN already exists").** Mechanism: the subject
   names the serial, so no two Macs collide; there is no depot uniqueness rule, because the identity store is keyed by serial and
   certificate serial number; a re-enrollment of one serial supersedes its old row and does not depend on the old certificate being
   refused or renewed (5.4).
3. **The gate learned devices only from webhook events, so a Mac enrolled before the gate was on was invisible.** Mechanism: the coordinator's
   `mdm_enrollments` table is the source and the ABM table seeds known serials (the first sync is a full fetch). The one Mac enrolled in the
   old stack comes across by import (section 7). A Mac that is enrolled but unknown to the table and presents a valid identity gets a
   `401`-class answer that makes its MDM client re-run `Authenticate` (**verify**: whether the client does; if not, only re-enrollment
   recovers it, which is why section 7 imports).
4. **The coordinator listened only on the tailnet, so a wiped Mac needed a new LAN listener.** Mechanism: `--mdm-listen` on the LAN
   address (4.1), a start-time error if it is a wildcard or not an interface's address.
5. **The release channel is tailnet-only.** Mechanism: the first daemon needs no channel (the pkg carries it), the pkg is served by the MDM
   listener, and Tailscale is installed second (4.7).
6. **Supervised enrollment, `DeviceInformation`, `EnableRemoteDesktop` and `Settings` proven by hand.** Mechanism: each becomes a typed
   command the gate can enqueue and the fake MDM in the tests can answer; the pinned list of command types is the audit of what the
   gate may send.

**Others.**

| Case | Mechanism |
|---|---|
| A Mac installs the enrollment profile by hand, unsupervised | `IsSupervised` is false: declined `not_supervised`; the serial must also be in the ABM table, and a manual install has no ADE record |
| A client on the LAN posts `/enroll` for someone's serial | the blob must verify against Apple (4.2); no challenge otherwise |
| `PKIOperation` replayed | the challenge is consumed in the transaction that records the identity |
| Identity certificate's serial differs from `Authenticate.SerialNumber` | refused, audited `mdm_serial_mismatch` |
| Two coordinators on one Postgres | the sync and the push senders run only under the store's lease (**Built** for the gate's saves) |
| The push certificate expires | `403` from APNs raises the alert; commands queue; the Macs still `Idle` on their own, so nothing is lost, only delayed |
| The ABM token expires | alert at 60 days; the sync fails closed with a named error; enrolled Macs are unaffected |
| ABM removes a device (disowned) | its `abm_devices` row is marked removed; it is not auto-approved again; its existing approval is the operator's to revoke |
| The ABM cursor is invalid or older than seven days | a full fetch |
| The coordinator is down while a Mac is at Setup Assistant | the Mac retries; no state is lost; nothing is queued |
| Time on the Mac is wrong at first boot | the certificate checks use the Mac's clock; Setup Assistant sets it from the network first |
| An older coordinator release reads the new tables | migration 0009 declares `older-releases: may-run` (new tables only, no column of an old table changes) |

## 7. Migrating from the running stack

The live state is one enrolled Mac (`m3ultra-1`, serial `P24LH9H6JT`), the push certificate, the ABM token, the ADE profile "Komira
MDM" and the DNS name.

**What is carried over**, each read read-only from the stack's own storage by a one-shot `hostkeeper-coordinator mdm import-nanohub`
verb (dry-run by default, `--apply` to act, never forcing):

| Item | Where it is | How |
|---|---|---|
| Push certificate and key | NanoHUB's storage (the audit: "inside NanoHUB storage"; the unit's configured backend is read from its unit file) | PEM pair written to `--mdm-dir`; topic checked equal to the one in the enrollment profile |
| ABM server token | NanoDEP's storage, already decrypted into its four parts | written to `abm-token.json`; `GET /account` proves it before the verb finishes |
| ADE profile | at Apple | not copied; the coordinator re-renders the same profile and compares the digest, redefining only if it differs |
| The enrolled Mac's enrollment | NanoMDM's tables: UDID, token, PushMagic, unlock and bootstrap tokens, certificate hash | upserted into `mdm_enrollments` |
| The old SCEP CA certificate | the `scepserver` depot | `--mdm-legacy-ca-file`, trusted for client certificates only, so the enrolled Mac's identity validates |

**Transparent takeover is the plan, re-enrollment is the fallback.** The enrolled Mac keeps its topic (same push certificate), its
ServerURL and CheckInURL (same name), and its identity (issued by the old CA, which the coordinator now trusts). The cutover changes who
answers on `mdm.internal.komira.dev`; the Mac notices nothing. The reasons to prefer it to a wipe: it costs the farm Mac no downtime, and
it is the test of lesson 3, a Mac enrolled before the coordinator MDM existed. The reason it might fail: the import is faithful only if
the stored token and PushMagic are the current ones. If the first push after cutover does not wake the Mac, wiping it and enrolling it
through ADE (which section 5.1 does for every later Mac anyway) is the fallback, and costs one wipe.

**Order.**

1. Ship the coordinator with the MDM listener off (no flag). Nothing changes.
2. `import-nanohub` dry-run; read the report; `--apply`. The old stack is untouched, because the import only reads.
3. Start `--mdm-listen` on a spare port with the fake APNs off and the real push certificate on, and run `mdm verify` (CM11): the
   imported state is read back and a push to the farm Mac is sent. The Mac still talks to the old stack, so nothing is at risk.
4. Cut over: stop Caddy and NanoHUB (leave their units installed), start `--mdm-listen` on the name's address and port 443. Confirm
   `Mdm.ListDevices` shows the Mac and a `DeviceInformation` round trip comes back through APNs.
5. Rollback at any time before CM14: stop `--mdm-listen`, start Caddy and NanoHUB. The old stack's storage is exactly as it was, except
   for commands acknowledged in between, which is one Mac's.
6. After a week of green, CM14 deletes the code and the artifacts; the host's units are removed by the owner's held script, not by this
   repository.

The coordinator's container is created by a held script of komira (the audit's account of it). That script loses its NanoHUB checks and
gains the new mounts and flags; it is a held script by rule S3 and is not part of any task here.

## 8. What is deleted

| What | When |
|---|---|
| `crates/hostkeeper-mdm-gate/src/nanohub.rs` (`NanoHub`, `ApiUrl`, `ApiKey`), `webhook.rs`, the flags `--mdm-api-url`, `--mdm-api-key-file`, `--mdm-webhook-listen` and their `MdmError` variants | CM14a (replaced by the in-process queue from CM11) |
| `crates/hostkeeper-mdm` (the kit), `crates/hostkeeper-mdm-core` (after `AdeProfile` and the enrollment plist move), `crates/hostkeeper-mdm-step`, `pins.txt`, the Caddyfile and unit templates, the `mdm` deb (`cargo xtask release deb --service mdm`), the kit tarball, and their lines in `artifacts.yml`, the workflow lint, the coverage baselines and the README table | CM14b |
| The dev-dependencies on those crates (`hostkeeper-daemon`, `hostkeeper-privileged`, `xtask`'s `mdm-fetch` and `mdm-signing-test`) | CM14b, each ported to the new crate's fakes first |
| `--mac-serial-allowlist` | a release after CM9, with a deprecation note in the release text; B8 forbids removing it in one step |
| `--mdm-pkg-allowlist` | after CM8, replaced by `--mdm-installs-file`, which reads the old file |
| The `/pkg` route in Caddy, the Caddy and `lego` units | not in this repository; they are the host's, removed by the owner's script after step 6 of section 7 |

## 9. Compatibility, secrets, security

### 9.1 Compatibility (B8) and formats

- Migration 0009 adds tables only: `mdm_enrollments`, `mdm_commands`, `mdm_identities`, `abm_devices`, `abm_state`, each with a file-store
  family, because both backends exist today. Its header is `older-releases: may-run`. The device record of the gate, `mdm_devices`,
  gains no required field.
- New flags only; every existing flag keeps its meaning until the one that is retired, and that retirement is a release later.
- New proto messages and rpcs on `Mdm` (push CSR and certificate, ABM token and certificate, status, ABM device list, queued-command list,
  import); `ListDevices` is unchanged. The previous release's `hkctl` runs against a new coordinator and a new `hkctl` against an old
  coordinator gets `UNIMPLEMENTED` for the new verbs. Each task names the test that runs the previous release on new data and the
  reverse (to build, in each PR).
- The host CA, `ca_sha256` and every daemon config are untouched.

### 9.2 What a compromise gets, and the new secrets

hostkeeper-mdm.md already says whoever holds the NanoHUB key holds root on the Macs. Built in, the coordinator's own process holds that
capability: it can install any pkg a Mac accepts and erase a Mac. It now also holds three secrets: the APNs push key, the ABM token and key,
and the MDM CA key. All three are files in `--mdm-dir` (mode 0600, owner the coordinator's user, no symlink, named by one flag, rules S1
and S2; no environment variable reads configuration), and the gate's pinned list of command types is the bound on what the code can
send. A stolen MDM CA key lets its holder sign identities the coordinator would accept, so `/enroll` (the Apple-signed blob and the ABM
table) and the nonce are the gates that do not depend on that key. If the blob cannot be verified (4.2), the ABM table and the nonce are
the only gates, and the document says that auto-approval is then weaker than it is today for a serial an attacker knows.

The RSA decryption in the SCEP envelope is the operation the published RSA timing advisory is about. The private key is internal, the
listener is on the LAN, a challenge is required before any work is done, and `PKIOperation` is rate limited per source. The advisory
does not block a PR under R6; this note is the reason to keep CM12's limits.

## 10. Tests and model checks

- **Fakes behind ports**: a clock, an APNs sender, an ABM transport, a store, an Apple root for `/enroll`. No network, no DNS, no fixed
  port (T12); no sleeps or elapsed-time assertions, the clock is injected (T10).
- **Wire fixtures** from Apple's machine-readable schemas (apple/device-management, a licensed subset checked in with its notice):
  each check-in message and each command result, plus a negative case per message (T13).
- **Model checks first** (CM4), before CM11 and CM12 are written, because both are state machines:
  - *the command queue*: enqueue, claim, `Acknowledged`, `Error`, `NotNow`, timeout and re-send, a replaced enrollment, a duplicated
    `Idle`. Invariants: a command is delivered to at most one connection at a time; a result is matched only to a command in `Sent`; a
    `NotNow` command is never lost; an acknowledged command is never re-sent.
  - *enrollment*: `/enroll`, SCEP, `Authenticate`, `TokenUpdate`, `CheckOut`, re-enrollment of one serial. Invariants: a challenge is
    consumed at most once; a serial has one live enrollment; a superseded certificate is never accepted.
  - *later*: DDM's sync token (CM15).
- **Behaviour tests** named in each PR with the behaviour each proves (T8), including the gate's `Event` tests moved to `Gate::handle`,
  the approval conditions of 4.6 one decline token per case, and an end-to-end test against the fake Mac that runs 5.1.
- **Coverage** 99 percent line and branch per crate (T1). **B9**: no macOS job is needed; the new code is platform-independent and
  tested on Linux. A real-Mac run is the owner's held checklist.

## 11. Tasks, one pull request each

Size: S under a day, M a few days, L a week or more. "After" lists prerequisites; the order is smallest first among the tasks whose
prerequisites are done. Each task updates the docs its change makes stale. Status is **Designed** unless noted.

| # | Task | After | Size |
|---|---|---|---|
| CM1 | This design, linked from hostkeeper-mdm.md and the design index | none | S |
| CM2 | Seam: `Gate::handle(Event)`; the webhook becomes a thin adapter over it (a refactor only, P7) | none | S |
| CM3 | `DeviceStatus` carries `enrolled_via_dep`; decline token `not_via_dep`; docs | none | S |
| CM4 | Model checks of the command queue and of enrollment (section 10), with the invariants as the tests' names | CM1 | S |
| CM5 | `hostkeeper-mdm-server`: plist wire types for check-in and results, with Apple-schema fixtures and negative cases | CM1 | M |
| CM6 | Migration 0009, the file-store families, conformance tests for both stores, `abm_devices` and `mdm_enrollments` first | CM4 | M |
| CM7 | APNs sender, push certificate state, `hkctl mdm push-csr`, `push-cert put`, `Mdm.Status`, expiry alerts | CM5 | M |
| CM8 | Named installs (`--mdm-installs-file`), `Settings` and the device name in the baseline, the pinned command list of eight | CM2 | M |
| CM9 | Approval from the ABM table, inventory name and group, `--mdm-default-group`, the three new tokens, the allowlist as override | CM3, CM6, CM13 | M |
| CM10 | `mdm import-nanohub` (dry-run default) and the cutover and rollback runbook | CM6, CM11 | M |
| CM11 | MDM listener: `/checkin`, `/mdm`, `/pkg`, the in-process queue replacing `NanoHub`, signature or client-certificate checks, `mdm verify` | CM2, CM5, CM6, CM7 | L |
| CM12 | SCEP, the MDM identity CA, `/enroll` with the Apple-signed blob and per-serial challenge, renewal alert | CM11 | L |
| CM13 | ABM client: OAuth session, token decrypt, sync, assign, ADE profile definition, `hkctl mdm abm-cert` and token upload | CM5, CM6 | L |
| CM14a | Delete the NanoHUB client, webhook and their flags | CM11 and a week of green on the farm | S |
| CM14b | Delete the kit crates, the pins, the `mdm` deb and kit tarball and their workflow lines | CM14a | M |
| CM15 | DDM: declaration store, sync token, status ingest, model-checked first | CM11 | L |
| CM16 | ACME identity with attestation, behind the identity port, after a farm-Mac test of `IdentityCertificateUUID` | CM12 | M |
| CM17 | In-process renewal of the MDM name's public certificate (DNS-01 with `instant-acme`) | CM11 | M |
| CM18 | Tailscale join (Q3): per-device key, the system-extension and VPN profiles | owner's Q3, a farm run | M |

CM2, CM3 and CM4 can start at once. The critical path is CM4, CM6, CM11, CM12, CM13, CM9, CM10, CM14a, CM14b. CM13 can run beside CM11
and CM12. Build while measuring: the farm-Mac checks in section 12 run alongside CM5 to CM7, not before them.

## 12. Open questions and what is not verified

Questions for the owner.

- **Q1** Which route created the current push certificate (an Apple vendor signing certificate, or mdmcert.download)? Renewal in 2027
  depends on it (4.4).
- **Q2** Is ABM's default device assignment set for Macs on the org tenant (4.5)? Its menu path differs between sources; confirm in the tenant.
- **Q3** How should an unattended Mac join the tailnet (4.7)? Recommended: a one-off, one-hour, tagged key minted by the coordinator.

Not verified. A claim in this document that depends on one of these is marked, and none is stated as fact elsewhere.

1. Apple's own text for APNs, Device Assignment and the MDM protocol reference was not reachable; the endpoint details rest on the
   Apple schema, NanoDEP and `a2` source.
2. Whether `IdentityCertificateUUID` accepts an ACME identity during ADE (CM16).
3. Whether macOS renews a SCEP identity inside the MDM payload on its own (4.2).
4. Whether the signature of the ADE POST can be verified against Apple's root with what the Mac sends (4.2).
5. Whether the live enrollment profile sets `SignMessage` or relies on the TLS client certificate; the listener accepts either (4.1) and the
   import reads which.
6. Tailscale's Team ID, the pkg's URL and any auth-key mechanism (4.7).
7. Whether a Mac that is enrolled but unknown to the table re-runs `Authenticate` on a refusal (section 6, lesson 3).
8. Whether `mdmcert.download` is operating today and whether Apple grants the vendor CSR signing certificate to an organization that
   manages only its own fleet.
9. The licence of KMFDDM, to copy its declaration store design for CM15.

## 13. Future work

- DDM as the baseline's status channel (CM15), then retire the `ProfileList` poll.
- ACME with hardware attestation as the identity (CM16): a certificate bound to a real Apple-silicon Mac and its serial.
- In-process certificate renewal for the MDM name (CM17), which finishes "one process".
- Account-driven enrollment for non-ADE Macs, once there is a Mac that is not in ABM (the old route B2 of the go-live).
- `EraseDevice` and `DeviceLock` as audited operator verbs behind a confirmation the console design defines; not built here because
  nothing asks for them yet.
