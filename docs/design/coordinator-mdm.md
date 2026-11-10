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
| Processes on the MDM host | The coordinator, and five more: `nanohub`, `depserver`, `depsyncer`, `scepserver`, Caddy with `lego` | The coordinator (and `lego` on a timer until CM17, 4.1) |
| Installed from | the coordinator image, the `mdm` deb or the `hostkeeper-mdm` kit tarball, and four pinned Go tools | the coordinator image |
| A new Mac | its serial is added to ABM, to the coordinator's serial allowlist (a redeploy) and the ADE profile is assigned by hand before the Mac first reaches Activate | the Mac is bought on the org account; nothing else |
| Which Macs are trusted | the serials in `--mac-serial-allowlist` | the serials Apple's ABM lists for the org's MDM server, plus supervision and the nonce; an inventory record may name the Mac |
| Name, group | per allowlist entry | from the Mac's inventory record by serial, else the default rule (section 4.6) |
| Screen Sharing, device name, Tailscale, extra profiles | by hand, or by a hand-made NanoHUB API call | the group baseline, applied at enrollment and re-applied (section 5.3) |
| Push certificate, ABM token | uploaded into NanoHUB and NanoDEP storage | held by the coordinator; the operator renews them with `hkctl mdm` |

What does not change: the Mac sees the same two things it sees today, an ADE profile that points at `https://mdm.example.com/enroll`
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
| 12 Tasks M2 to M5, M6a, M6b, M7 | Superseded by section 11 here. M8's NanoHUB client and webhook are replaced (CM2, CM11a, CM14a) |
| 11, 13 | Stand; Q-numbers carry over |

## 2. What exists today

Facts, with the file that says them. The rest of this document is about the difference.

- **The stack** (the farm's MDM host, as run, from the audit of the go-live and its runbook): NanoHUB (NanoMDM, NanoCMD and KMFDDM behind one
  JSON API and a webhook), `depserver` and `depsyncer` (NanoDEP), `scepserver`, Caddy terminating TLS on the LAN address with a
  `lego` certificate for `mdm.example.com`. The coordinator talks to NanoHUB on loopback and receives its webhook on a loopback
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
| SCEP server and the MDM identity CA | `scepserver` | three request types (`GetCACaps`, `GetCACert`, `PKIOperation`) with a signed and enveloped reply, written here because no Rust SCEP server exists (4.2) |
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
`http::Surface::Mdm`, TLS from `--mdm-tls-cert-file` and `--mdm-tls-key-file` with the certificate of `mdm.example.com` issued by a
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
putting a private CA in it is a second trust path to maintain, and the name `mdm.example.com` already has a public-CA certificate
today. The DNS record of hostkeeper-mdm.md section 9 stays, pointing at the new listener's address.

**TLS client-certificate rule.** `/checkin` and `/mdm` must receive the Mac's identity while `/enroll` and `/scep` must accept a client
with none, so the verifier is client-certificate-optional (rustls `allow_unauthenticated`) and each route checks for itself: `/checkin`
and `/mdm` refuse a request with neither a trusted certificate nor a valid `Mdm-Signature`, the other two ignore the certificate. A rustls
verifier sends root hints only for the issuers it is given, and a macOS client may decline to offer an identity that no hint names, so the
hint list holds the MDM CA (and any issuer the migration trusts). Verify with the farm Mac that it offers the identity (section 12, item 10).

**Is the `/enroll` response signed?** For ADE, Apple's text may require the returned enrollment profile to be CMS-signed. If so the signing
key is a second use of the public TLS key that the coordinator reads, and the signing is a task in CM12c. Not established; section 12, item 11.
The capture that decides it is the same farm-Mac capture as for the device blob (CM12a).

**An unknown enrollment.** What a Mac does with `401`, `403` or `410` from `/mdm` is not the same: some are read by the MDM client as
"this server no longer manages me" and remove management. The status for an enrollment the table does not know is chosen from Apple's
protocol text before CM11 (a `410` is not used until that text says what it does), and CM11 tests that no Mac-side state is lost on it
(a fake client keeps its profile). Section 12, item 12.

**Certificate renewal.** v1 reads the two files and reloads them on the coordinator's existing reload tick, the way it reloads
`--server-cert-file` (**Built** for that file; Designed for this one). Something must write the files: `lego` on a timer does today, and
the first cut may keep that one unit and no Caddy. This is the one place the "no separate unit" goal is not met at first, and the
document says so rather than hiding it. Task CM17 moves renewal in: DNS-01 through `instant-acme` (TLS-ALPN-01 needs the Let's Encrypt
validator to reach a LAN address, so it cannot be used for this name), with the provider hook the `lego` setting uses now.

### 4.2 Device identity: SCEP, issued by its own CA, one identity per serial

**Decision.** SCEP (RFC 8894) served by the coordinator, with a new MDM identity CA that has its own RSA 2048 key at
`--mdm-dir/mdm-ca.key`. The identity's subject names the serial: `CN=<serial>`, `O=<org>` and an `OU=hostkeeper-mdm`, so there is no
shared empty DN. The challenge is per request: `POST /enroll` verifies the device-information blob, finds the serial in the ABM table,
makes a 32-byte challenge valid for one hour, and builds the enrollment profile with that challenge. The challenge is bound to the
serial, the blob's UDID and the ABM record's `profile_status` (`pushed`). `PKIOperation` is **idempotent**: consumption is keyed on the
challenge and the hash of the CSR's public key, in the transaction that records the issued certificate. A repeat with the same challenge,
key and `transactionID` (a client whose CertRep was lost) gets the stored certificate again; a different key presenting a consumed challenge
is refused.

**Why SCEP and not the alternatives.**

| Option | For | Against |
|---|---|---|
| SCEP (decided) | `com.apple.security.scep` since macOS 10.7; what the live stack runs and proved | no Rust SCEP server exists, so it is written here: `GetCACaps`, `GetCACert`, and a `PKIOperation` that opens a CMS envelope (RSA key transport, DES-EDE3 or AES-CBC content) and returns a signed and enveloped `CertRep`, on `rsa`, `cms`, `des`, `aes` and `cbc` (the RSA advisory is not a blocking one under R6); a CA of its own |
| Per-device PKCS#12 in the enrollment profile | no SCEP code; P-256 possible | the CA generates the private key and ships it inside a profile; whether macOS accepts P-256 as the MDM identity is not known. Verify. Rejected for the key exposure |
| ACME with `Attest` and `HardwareBound` | no shared secret; the certificate is bound to Apple-silicon hardware and the serial; the schema gives `com.apple.security.acme` since macOS 13.1 | no Rust ACME server crate was found; and whether `IdentityCertificateUUID` of `com.apple.mdm` accepts an ACME payload during ADE is not stated by Apple's schema, which says only that it "may also point to a SCEP payload". Verify on the farm Mac. Follow-up CM16 |

**Why a CA apart from the host CA.** The host CA is P-256 only (`ca/profile.rs` refuses other key types), its certificates are
admitted on the mTLS listeners, and every daemon pins it. An MDM identity is RSA, ships to a Mac before it is approved, and must never be
accepted by the heartbeat listener. A second key is additive and changes no pin.

**Verification of the enrolling device, and what happens when it cannot be checked.** The ADE POST to `/enroll` carries a PKCS#7 signed by
the device's Apple-issued identity. The coordinator verifies it against Apple's root and takes the serial and UDID from it, so a client on
the LAN cannot ask for a challenge for a serial it does not own. The blob carries no freshness, so a captured one can be replayed; the
challenge being bound to the UDID and to a serial whose ABM `profile_status` is `pushed` limits what a replay buys, and the nonce (4.6) is
the gate after it. Rules, which fail closed:

- Task **CM12a** first captures the real POST from the farm Mac and proves that the chain verifies. It comes before any `/enroll` code.
- If it cannot be verified, `/enroll` **issues no challenge on its own**: the request waits as an operator click (the same place a
  `no_group` request waits), because a serial is not a secret and "the serial is in the ABM table" alone would let a LAN peer who knows one
  obtain an identity.
- A `/enroll` for a serial that already has a live approved enrollment gets a challenge only when the blob verifies or an operator
  confirms, and a new `Authenticate` never supersedes a live enrollment silently: the supersession is audited and, when the blob did not
  verify, held for the operator.
- ACME with attestation (CM16) is the real fix for the weaker case, since the certificate is then bound to the hardware. 9.2 says so.

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

- One push per wake. A push is "outstanding" for an enrollment until **any request from that enrollment reaches `/mdm`** (the Mac is
  awake and draining its whole queue on one connection, so a command enqueued ten minutes later gets a new push at once).
  `--mdm-push-retry-s` (default 3600) is only a floor for repeating a push to a Mac that has not connected since the last one. `NotNow` keeps
  the command and does not re-push inside the floor.
- Each push sets `apns-priority: 10` and an `apns-expiration` of `--mdm-push-expiry-s` (default 86400, never 0, because 0 drops the push
  when the Mac is asleep). Both are flags with defaults.
- A `410` marks the token invalid and stops pushing it; the next `TokenUpdate` replaces it. A `403` raises the push-certificate alert (the
  certificate is expired or revoked). `429` and `503` retry with the window as the floor.
- A command that waits without a push-capable token waits for the Mac's next connection. That a Mac polls on any schedule is **not**
  established by anything read (section 12, item 13); the design does not depend on it, and the push is the wake.

### 4.4 The push certificate: where it comes from and how it renews

The push certificate is the only dependency Apple does not let the coordinator remove, and this document does not hide that.

**Facts established from Apple's page and the MicroMDM guide.** A push certificate is created at identity.apple.com from a CSR that must
be signed by an Apple "MDM vendor CSR signing certificate", which the Account Holder of a Developer Program account requests from Apple.
Community signers (the community mdmcert service) exist and the MicroMDM guide labels them for testing and development. ABM does not issue push
certificates. It lasts one year, and it is renewed with the same Apple Account that created it and the same topic. The org's certificate
has the topic `com.apple.mgmt.External.d1801ae1-3ef7-483e-9f6d-4979e2ebfc09`; the audit infers expiry about 2027-11-07 and did not read it.

**Decision.**

1. The coordinator **carries over** the existing certificate and key (section 7), so no new certificate is needed for the migration and the
   topic in every enrollment never changes.
2. The coordinator owns the lifecycle from then on: `hkctl mdm push-csr` creates a key and a CSR in `--mdm-dir` and returns the CSR.
   The vendor's signing step takes that CSR and returns a **plist** (the CSR, the vendor certificate chain and a signature over the
   CSR) that is uploaded to identity.apple.com, which returns the PEM certificate; `push-csr` therefore also writes the CSR in the form
   the vendor tool takes, and the exact input and output are read off the vendor tool at Q1, not assumed here.
   `hkctl mdm push-cert put` accepts the PEM Apple returns, checks that its topic equals the current topic on a renewal, and swaps it in
   atomically; `Mdm.Status` reports topic and expiry. The coordinator's existing alert path raises an alert at `--mdm-push-alert-days`
   (default 60) before expiry and again at 14.
3. **Signing the CSR is outside the coordinator**, by whoever holds the vendor certificate. This needs the owner's answer (Q1): which route
   produced the current certificate. If the vendor route, the vendor certificate exists and the yearly renewal is a 10-minute task. If
   the community mdmcert service, the renewal route is the one the owner's account used and its availability must be confirmed before 2027-09.

### 4.5 ABM sync, in process

**Decision.** A task in the coordinator, single-flight under the store's lease, every `--mdm-abm-sync-s` (default 300). It speaks to
`https://mdmenrollment.apple.com` with OAuth 1.0a (HMAC-SHA1, the four-part token), exchanging it (`GET /session`) for an `X-ADM-Auth-Session` that it
caches and refreshes. Every call carries `X-Server-Protocol-Version`; a `429` is retried after its `Retry-After`, and an auth-session
error refreshes the session once. These protocol details rest on NanoDEP's source and are unverified (section 12, item 1). HMAC-SHA1 is about sixty lines on `ring`; no OAuth crate is needed.

- **Token.** ABM hands over an encrypted `.p7m`. The coordinator generates the RSA key and self-signed certificate for the ABM server
  (`hkctl mdm abm-cert` returns the certificate to upload in ABM), decrypts the returned file with `cms` and `rsa`, and keeps the four
  parts in `--mdm-dir/abm-token.json` (0600). The carried-over token is copied from NanoDEP's storage by the owner's script instead (section 7). Expiry is read
  from the token and alerted at `--mdm-abm-alert-days` (default 60). The live token expires 2027-10-31.
- **Sync.** `POST /devices/sync` with the stored cursor, new serials upserted into `abm_devices`, devices that ABM reports as removed marked
  so, then the assignment call for every serial that has no profile assigned: `POST /profile/devices` as Apple's current text has it, falling
  back to `PUT` on a `4xx` (NanoDEP uses `PUT`, for the depsim simulator, and says so in its source). A first run, an invalid cursor or one older than seven
  days does a full fetch (`POST /server/devices`), which also returns Macs already assigned. NanoDEP's assigner is the pattern.
- **The ADE profile's UUID** is the column `abm_state.profile_uuid`. At first run it is seeded from the carried-over
  `--mdm-dir/abm-profile-uuid` (section 7: the held script reads NanoDEP's stored assigner UUID for the DEP name, which is unverified,
  section 12 item 1), so no redefine is needed and no assigned device changes profile. If it is absent, the coordinator defines the profile
  and stores the new UUID; devices already assigned to the old UUID keep it, and it names the same `url`, so they still enroll.
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

`mac_auto_approve` keeps its order and its nine decline tokens (**Built**: the four `device_*`/`serial_*` tokens and `device_state_unavailable` in `crates/hostkeeper-coordinator/src/enrollment/mac_nonce.rs`, the five `nonce_*` tokens in `crates/hostkeeper-enrollment/src/enroll/mac_nonce.rs`). Only
its first lookup changes, and these are added, additively: `serial_not_in_abm`, `device_not_via_dep`, `no_group`. The tokens form the family of the built ones (`device_*` for what a device reports, `serial_*` for the list, `nonce_*` for the nonce), and a test pins the full list of twelve. The conditions for approval
are all of:

1. the serial is in the ABM table (Apple's attestation of ownership; a stronger claim than "a Mac says it is supervised");
2. the MDM enrollment is the current one for that serial, supervised per `DeviceInformation` `IsSupervised`, and `SecurityInfo` says
   `EnrolledViaDEP` (the gate records both today; **CM3** passes `enrolled_via_dep` through `DeviceStatus`). Both are what the Mac says about itself, so they are evidence only after the identity was issued under the fail-closed rule of 4.2;
3. a group resolves;
4. the nonce, as today: single-use, one hour, bound to the request and the serial, which proves the pkg installed and the daemon runs.

**Name from inventory.** The baseline's `Settings` command sets `DeviceName` to the host id and `HostName` to the same lower-case DNS-safe
string. A Mac without an inventory record gets the default rule's id. Changing the inventory's `host_id` changes the baseline target and
the next re-apply (5.3) sends the new name. The schema's `Settings` entry for macOS is device channel and not user-enrollment, and
Apple's text says `DeviceName` is available on supervised devices only; supervised is what this design assumes anyway.

**The old allowlist** stays one release as an optional override that wins for the serials it lists (B8: no format is removed), and is
deleted in CM14c. A deployment that holds no host groups keeps the allowlist as its only source, because services come only from groups or
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
own Team ID, to be read off the pkg), the pkg's download URL, and above all how an unattended Mac joins the tailnet**. The mechanism is documented:
Tailscale's MDM keys page (kb/1315/mdm-keys) lists an `AuthKey` system policy for macOS (added in 1.74) that logs a managed device in
without interaction unless it is already logged in, recommends a one-off key and warns about keeping keys in an MDM. It is a
managed-preferences payload, not a new Tailscale feature. The design: a per-device one-off, pre-authorized, tagged key with a one-hour
expiry, minted by the coordinator through Tailscale's API with an OAuth client, delivered through the `AuthKey` policy in the per-device
profile the gate already manages, and removed once the Mac appears on the tailnet. This puts a secret in a profile, which
hostkeeper-mdm.md and the baseline design forbid for the daemon's config, so it is an **explicit exception**, bounded by the one-hour
expiry and one use (Q3 asks the owner to accept it). The OAuth client secret is read from a file named by `--mdm-tailscale-oauth-file`
(S1, S2: a flag, never the environment), and a test asserts that neither it nor a minted key appears in a log line, an audit record or
any listing. What stays unread: the Team ID and the pkg URL, which are read off the signed pkg. Task CM18, with a run on the farm Mac.

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

**Where the secrets live.** The push certificate and key, the MDM CA key and the ABM token are files in `--mdm-dir` of **one** node. In v1
the MDM listener, the sync and the push sender are single-node: the coordinator refuses to start with any `--mdm-*` flag when the store's
lease is held by another coordinator that has the MDM flags (the lease record names whether it does), so a second node cannot half-run with
a stale push certificate or a different CA. A second node for MDM would need those secrets in the store, encrypted with a key from a file
flag; that is task CM19 and not designed here.

## 5. How each operation works

The budgets count requests, as measured by the structure of the handlers (what each one calls) and not by a run. They are **designed**; the
times are targets to be measured under CM11a and CM12c, not measurements. A store request is one round trip to Postgres.

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
   old stack is wiped and enrolled through ADE (section 7), which also exercises the real path. A Mac that is enrolled but unknown to the table and presents a valid identity gets a
   answer chosen in 4.1 (not a status that tells the client to drop management); whether the client then re-runs `Authenticate` is
   unverified, and if it does not, only re-enrollment recovers the Mac, which is why section 7 wipes and enrolls the one Mac.
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
| A Mac installs the enrollment profile by hand, unsupervised | `IsSupervised` is false: declined `device_not_supervised`; the serial must also be in the ABM table, and a manual install has no ADE record |
| A client on the LAN posts `/enroll` for someone's serial | the blob must verify against Apple (4.2); no challenge otherwise |
| `PKIOperation` replayed by an attacker, or retried by the Mac after a lost `CertRep` | consumption is keyed on the challenge and the CSR key hash (4.2): the same key and `transactionID` gets the stored certificate, a different key is refused; tests for both |
| Identity certificate's serial differs from `Authenticate.SerialNumber` | refused, audited `mdm_serial_mismatch` |
| Two coordinators on one Postgres | MDM is single-node in v1 (4.4): a second coordinator with MDM flags refuses to start while the lease holder has them; the gate's saves run under the lease (**Built**) |
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

**The plan: carry over two secrets and wipe the one Mac.** Reading NanoMDM's storage (MySQL, Postgres or files, chosen in a unit file)
would add drivers and a schema coupling to a Go tool that is being deleted, and its code would have to reach 99 percent with fakes. A
transparent takeover would also have to trust the old SCEP CA (every identity it ever signs stays valid), depends on that
certificate's renewal, and cannot be rolled back once the Mac sends a new `TokenUpdate` to the coordinator (NanoMDM's copy is then stale).
So there is no `import-nanohub` verb and no NanoMDM reader. What is carried over is the minimum that must not change:

| Item | Where it is | How |
|---|---|---|
| Push certificate and key | NanoHUB's storage (the audit: "inside NanoHUB storage") | PEM pair copied to `--mdm-dir/push.pem` and `push.key`; the topic is checked equal to the one the Mac was enrolled under (it need not, since the Mac is wiped, but a different topic is a different Apple certificate and is refused) |
| ABM server token | NanoDEP's storage, in its four parts | the token JSON written to `abm-token.json`; `GET /account` proves it |
| ADE profile UUID | NanoDEP's storage under the DEP name (unverified, section 12 item 1) | written to `abm-profile-uuid`; absent means the coordinator defines the profile (4.5) |

The two files and the UUID are copied by a **held script** of the owner's (S3): dry-run by default, never forcing, `bash -n` clean,
printing no secret. It is not part of any task in this repository and has no Rust code or fake to cover.

**Order.**

1. Ship the coordinator with the MDM listener off (no flag). Nothing changes.
2. The owner's script copies the three items; the coordinator starts with `--mdm-listen` on a spare port, and `mdm verify` (CM11b)
   proves the push certificate by a push to the farm Mac, which still talks to the old stack.
3. Cut over: stop Caddy and NanoHUB (leave their units installed), start `--mdm-listen` on the name's address and port 443. How a process
   gets port 443 on the LAN address inside the coordinator's container (the held script grants `CAP_NET_BIND_SERVICE`, runs host
   networking, or the listener moves to a high port with a Caddy-free redirect) is Q4.
4. Erase `m3ultra-1` in ABM-assigned state; it enrolls through 5.1. Confirm `Mdm.ListDevices` shows it and a `DeviceInformation` round
   trip returns through APNs.
5. **Rollback holds only until the Mac's first `TokenUpdate` reaches the coordinator.** Before it, stop `--mdm-listen` and start Caddy
   and NanoHUB (their storage is untouched). After it, the way back is to wipe the Mac and enroll it into the old stack again.
6. After a week of green, CM14a and CM14b delete the code and the artifacts; the host's units are removed by the owner's held script.

A real transparent takeover is not offered. If the owner later wants one, it needs a hash-pinned identity (the table already stores
the certificate hash) and the gate's `mdm_devices` record seeded by the import (lesson 3 again); it is future work, not a task.

The coordinator's container is created by a held script of komira (the audit's account of it). That script loses its NanoHUB checks and
gains the new mounts and flags; it is a held script by rule S3 and is not part of any task here.

## 8. What is deleted

| What | When |
|---|---|
| `crates/hostkeeper-mdm-gate/src/nanohub.rs` (`NanoHub`, `ApiUrl`, `ApiKey`), `webhook.rs` and their `MdmError` variants | CM14a (replaced by the in-process queue from CM11) |
| The flags `--mdm-api-url`, `--mdm-api-key-file`, `--mdm-webhook-listen` | CM14a **accepts and ignores them with a logged deprecation for one release**; the release after removes them, as for the allowlist (B8: a deployed unit file that still passes them must start) |
| `crates/hostkeeper-mdm` (the kit), `crates/hostkeeper-mdm-core` (after `AdeProfile` and the enrollment plist move), `crates/hostkeeper-mdm-step`, `pins.txt`, the Caddyfile and unit templates, the `mdm` deb (`cargo xtask release deb --service mdm`), the kit tarball, and their lines in `artifacts.yml`, the workflow lint, the coverage baselines and the README table | CM14b |
| The dev-dependencies on those crates (`hostkeeper-daemon`, `hostkeeper-privileged`, `xtask`'s `mdm-fetch` and `mdm-signing-test`) | CM14b, each ported to the new crate's fakes first |
| `--mac-serial-allowlist` | a release after CM9, with a deprecation note in the release text; B8 forbids removing it in one step |
| `--mdm-pkg-allowlist` | after CM8, replaced by `--mdm-installs-file`, which reads the old file |
| The `/pkg` route in Caddy, the Caddy and `lego` units | not in this repository; they are the host's, removed by the owner's script after step 6 of section 7 |

**Supersession check (B22).** What each deleted piece did and where the duty goes.

| Deleted | What it did | Owner after |
|---|---|---|
| `hostkeeper-mdm-gate` `nanohub.rs`, `webhook.rs` | sent commands to NanoHUB's API, received its unsigned webhook | the in-process queue and `Gate::handle(Event)` (CM2, CM11a); fake MDM in the gate's tests |
| `hostkeeper-mdm` (kit) | rendered the Caddyfile, units and install of the stack; `mdm verify` of the stack | none needed for the Caddyfile and units (no stack); `mdm verify` returns in CM11b |
| `hostkeeper-mdm-core` | `AdeProfile`, the enrollment plist, shared types | `hostkeeper-mdm-server` (CM5), which receives both renderers first |
| `hostkeeper-mdm-step` | the install steps of the kit | none (the kit is gone); the coordinator's units are the owner's |
| `xtask mdm-fetch`, `mdm-signing-test` | fetched the NanoHUB release pins; tested signed-artifact checks | `mdm-fetch`: none (no external binary); `mdm-signing-test`: ported to the server crate's fakes before deletion |
| workflow-lint, coverage-baseline, `artifacts.yml` and README entries for the above | listed the crates, the deb and the tarball | removed in the same PR as each crate, so the lint and coverage gates stay green (T3: no floor is lowered, a deleted crate has no floor) |

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
table) and the nonce are the gates that do not depend on that key. If the blob cannot be verified (4.2), `/enroll` issues nothing
without an operator, so the weak case is closed by an operator click and not by the ABM table alone. Even with the blob verified it carries
no freshness, so a captured one can be replayed; the UDID and `pushed` bindings and the nonce limit that, and ACME with attestation (CM16)
is the real fix. Two secrets are Tailscale's: the OAuth client secret (a file) and the one-off key that goes into a profile (4.7).

The RSA decryption in the SCEP envelope is the operation the published RSA timing advisory is about. The private key is internal, the
listener is on the LAN, a challenge is required before any work is done, and `PKIOperation` is rate limited per source. The advisory
does not block a PR under R6; this note is the reason to keep CM12c's limits.

## 10. Tests and model checks

- **Fakes behind ports**: a clock, an APNs sender, an ABM transport, a store, an Apple root for `/enroll`. No network, no DNS, no fixed
  port (T12); no sleeps or elapsed-time assertions, the clock is injected (T10).
- **Wire fixtures** from Apple's machine-readable schemas (apple/device-management, a licensed subset checked in with its notice):
  each check-in message and each command result, plus a negative case per message (T13).
- **Model checks first** (CM4), before CM11a and CM12b are written, because both are state machines:
  - *the command queue*: enqueue, claim, `Acknowledged`, `Error`, `NotNow`, timeout and re-send, a replaced enrollment, a duplicated
    `Idle`. Invariants: a command is delivered to at most one connection at a time; a result is matched only to a command in `Sent`; a
    `NotNow` command is never lost; an acknowledged command is never re-sent.
  - *enrollment*: `/enroll`, SCEP, `Authenticate`, `TokenUpdate`, `CheckOut`, re-enrollment of one serial. Invariants: a challenge is
    consumed at most once; a serial has one live enrollment; a superseded certificate is never accepted.
  - *later*: DDM's sync token (CM15).
- **SCEP tests** (CM12b): a lost `CertRep` retried with the same key and `transactionID` gets the stored certificate; a different key
  reusing a consumed challenge is refused; `/enroll` for a serial with a live enrollment issues nothing without verification or an
  operator. **Listener tests** (CM11a): `/enroll` accepts no client certificate while `/checkin` refuses without one; an unknown
  enrollment keeps the fake Mac's state. **Push tests**: a command after an answered push is pushed at once; `apns-priority` and
  `apns-expiration` are set.
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
| CM3 | `DeviceStatus` carries `enrolled_via_dep`; decline token `device_not_via_dep`; docs | none | S |
| CM4 | Model checks of the command queue and of enrollment (section 10), with the invariants as the tests' names | CM1 | S |
| CM12a | Spike on the farm Mac: capture the `/enroll` POST and prove the Apple chain verifies; read whether the `/enroll` response must be signed; run a SCEP exchange against a library fixture; read which client-auth mode the live profile uses. Output: a note and fixtures, decides 4.2 | CM1 | S |
| CM5 | `hostkeeper-mdm-server`: plist wire types for check-in and results, with Apple-schema fixtures and negative cases | CM1 | M |
| CM6 | Migration 0009, the file-store families, conformance tests for both stores, `abm_devices` (with `abm_state.profile_uuid`) and `mdm_enrollments` first | CM4 | M |
| CM7 | APNs sender (priority, expiry, event-based outstanding), push certificate state, `hkctl mdm push-csr`, `push-cert put`, `Mdm.Status`, expiry alerts | CM5 | M |
| CM8 | Named installs (`--mdm-installs-file`), `Settings` and the device name in the baseline, the pinned command list of eight | CM2 | M |
| CM11a | MDM listener: `/checkin`, `/mdm`, the optional-client-certificate verifier with per-route checks and the hint list, the in-process queue replacing `NanoHub`, signature or certificate checks, the unknown-enrollment answer | CM2, CM5, CM6, CM7 | L |
| CM11b | `mdm verify` (reads back the state, sends a push) | CM11a | S |
| CM11c | `GET /pkg/<sha256>.pkg`, streamed and digest-checked | CM11a, CM8 | S |
| CM13 | ABM client: OAuth session, token decrypt, sync, assign (`POST`, `PUT` fallback), ADE profile definition, `hkctl mdm abm-cert` and token upload | CM5, CM6 | L |
| CM12b | SCEP library: `GetCACaps`, `GetCACert`, `PKIOperation`, signed and enveloped `CertRep`, idempotent consumption, fixtures and the farm Mac | CM12a, CM5 | L |
| CM12c | `/enroll` wiring: the verified blob, the per-serial challenge, the MDM CA, renewal alert, fail-closed rules | CM11a, CM12b, CM6, CM13 | M |
| CM9 | Approval from the ABM table, inventory name and group, `--mdm-default-group`, the three new tokens (family pinned), the allowlist as override | CM3, CM6, CM13 | M |
| CM10 | The cutover runbook (the three carried items, the 443 question, the wipe, the rollback limit); the held script is the owner's | CM7, CM11a, CM12c, CM13 | S |
| CM17 | In-process renewal of the MDM name's public certificate (DNS-01 with `instant-acme`), or an explicit owner of `lego`, before CM14b so "one process" is true | CM11a | M |
| CM14a | Accept and ignore the NanoHUB client flags with a deprecation; delete the NanoHUB client and webhook | CM11a and a week of green on the farm | S |
| CM14b | Delete the kit crates, the pins, the `mdm` deb and kit tarball and their workflow lines, with the supersession table of section 8 | CM14a, CM17 | M |
| CM14c | Remove the three flags and the allowlist flags | one release after CM14a and CM9 | S |
| CM15 | DDM: declaration store, sync token, status ingest, model-checked first | CM11a | L |
| CM16 | ACME identity with attestation, behind the identity port, after a farm-Mac test of `IdentityCertificateUUID` | CM12c | M |
| CM18 | Tailscale join: the `AuthKey` policy, the per-device key, the system-extension and VPN profiles, the OAuth file flag | Q3, a farm run | M |
| CM19 | MDM secrets in the store, encrypted, for a second node | CM12c | M |

CM2, CM3, CM4 and CM12a can start at once. **Interim milestone**: CM2, CM5, CM6, CM7 and CM11a move only the commands path, with Caddy
still routing `/enroll` and `/scep` to the old stack: the gate then runs on the in-process queue and the webhook is not needed, long before
the identity work lands. The critical path is CM4, CM6, CM11a, CM12b, CM12c, CM13, CM9, CM10, CM17, CM14a, CM14b. CM13 can run beside CM11a
and CM12b. Build while measuring: the farm-Mac checks in section 12 run alongside CM5 to CM7, not before them.

## 12. Open questions and what is not verified

Questions for the owner.

- **Q1** Which route created the current push certificate (an Apple vendor signing certificate, or the community mdmcert service)? Renewal in 2027
  depends on it (4.4).
- **Q2** Is ABM's default device assignment set for Macs on the org tenant (4.5)? Its menu path differs between sources; confirm in the tenant.
- **Q3** Do you accept the explicit exception to the no-secrets-in-profiles rule (4.7): a one-off, one-hour, tagged Tailscale key in the
  per-device profile, through the documented `AuthKey` policy?
- **Q4** How does the coordinator's process get port 443 on the LAN address inside its container (a capability, host networking, or a
  higher port) (7)?

Not verified. A claim in this document that depends on one of these is marked, and none is stated as fact elsewhere.

1. Apple's own text for APNs, Device Assignment and the MDM protocol reference was not reachable; the endpoint details rest on the
   Apple schema, NanoDEP and `a2` source.
2. Whether `IdentityCertificateUUID` accepts an ACME identity during ADE (CM16).
3. Whether macOS renews a SCEP identity inside the MDM payload on its own (4.2).
4. Whether the signature of the ADE POST can be verified against Apple's root with what the Mac sends (4.2).
5. Whether the live enrollment profile sets `SignMessage` or relies on the TLS client certificate; the listener accepts either (4.1) and
   CM12a reads which.
6. Tailscale's Team ID and the pkg's URL, both read off the signed pkg (4.7).
7. Whether a Mac that is enrolled but unknown to the table re-runs `Authenticate` on a refusal (section 6, lesson 3).
8. Whether the community mdmcert service is operating today and whether Apple grants the vendor CSR signing certificate to an organization that
   manages only its own fleet.
9. The licence of KMFDDM, to copy its declaration store design for CM15.
10. Whether a macOS client offers its identity given the root hints of 4.1 (CM11a).
11. Whether the ADE `/enroll` response must be CMS-signed (4.1; CM12a).
12. What the MDM client does on `401`, `403` and `410` from `/mdm` (4.1; before CM11a).
13. Whether a Mac polls on any schedule of its own (4.3); the design does not depend on it.

## 13. Future work

- DDM as the baseline's status channel (CM15), then retire the `ProfileList` poll.
- ACME with hardware attestation as the identity (CM16): a certificate bound to a real Apple-silicon Mac and its serial.
- In-process certificate renewal for the MDM name (CM17), which finishes "one process".
- Account-driven enrollment for non-ADE Macs, once there is a Mac that is not in ABM (the old route B2 of the go-live).
- `EraseDevice` and `DeviceLock` as audited operator verbs behind a confirmation the console design defines; not built here because
  nothing asks for them yet.
