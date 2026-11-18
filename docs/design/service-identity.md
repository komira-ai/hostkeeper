# Service identity: an 8-hour certificate for a service hostkeeper installs

Status: built by the change that adds this file. It is the E11 item of the hostkeeper design
(service identity, in place of a service's hand-placed join token), written for the code as it is
now: one root daemon per host, native packages whose manifest travels in the release entry, host
groups, and the coordinator's in-process CA.

## 1. What it does

A service that needs to prove to another service which host it runs on (js-agent to
js-lease-server) asks for it in its release entry's manifest:

```json
"manifest": { "package": "js-agent", "identity": true, "identityAccount": "root", ... }
```

Then, on every host that runs that entry, while the host's group lists the service:

1. the daemon makes a P-256 key for the service on the host (the key never leaves it);
2. it asks the coordinator for a certificate for that key, over its own mutual TLS
   (`hostkeeper.agent.v1.Coordinator/IssueServiceCertificate`);
3. the coordinator's CA signs a certificate that names the service and the host and is good for
   client authentication only, for 8 hours;
4. the daemon checks it and writes the key, the certificate and the CA bundle into the
   service's identity directory, owned by the service's account, mode `0600`;
5. it asks again at half life (4 hours), with the same key.

A relying party (js-lease-server) trusts hostkeeper's CA by its own flag, reads the service and
host from the certificate, and refuses a host the coordinator denies (`GET /v1/deny`).

Nothing secret is in any package or release entry: the key is made on the host, and the manifest
only says that the service wants an identity and which account owns it (hostgroups.md section 6.5
stays true).

## 2. The certificate

| field | value |
|---|---|
| issuer | the coordinator's enrollment CA (the one that signs host certificates) |
| subject | one CN, `service:<name>` (never `host:<id>`, so no reader of host certificates takes it for one) |
| SAN | URI `urn:hostkeeper:service:<name>`, and DNS name `<host id>` |
| EKU | `clientAuth` only |
| key usage | `digitalSignature` |
| basic constraints | not a CA |
| lifetime | 8 hours from the coordinator's clock when it signs |
| key | the P-256 key of the CSR, nothing else copied from the CSR |

A service certificate is refused everywhere a host certificate is needed: on `Heartbeat`, `Renew`,
`IssueServiceCertificate`, the repository and console routes. It authenticates only a call of
`ListDeny` (section 5).

## 3. Who gets one

The coordinator signs only when all of these hold, checked in this order:

1. The caller's client certificate is a current host certificate of an enrolled host
   (`forbidden` otherwise).
2. The host is not denied (`denied`): an operator has not retired it.
3. The service name is well formed (`invalid_service`).
4. The CSR is a P-256 request signed by its own key (`bad_csr`, `key_not_p256`).
5. The service is assigned to the host: its host group lists it and it is not being uninstalled
   from the host (`service_not_assigned`).
6. The entry the coordinator would order the host for the service's desired version in the
   host's group has `identity = true` in its manifest (`service_not_assigned` too: an entry
   without `identity` is not assigned an identity).
7. The host's assurance allows it (`identity_not_approved`):
   - **operator**: the host's certificate came from a request an operator approved by hand
     (`Enrollment.Approve`, the UI's Approve). Allowed.
   - **auto**: the host was approved by the coordinator itself, by an inventory record
     (`enrollment.auto_approved`). Allowed only after an operator approved this host's key for
     identity with `HostIdentity.Approve` (UI: the host page's "Approve identity"), naming the
     SHA-256 of the key the host holds now. A new key (a replace, a re-enrollment) needs a new
     approval; a host whose approval is not found, or names no approver (an approval written by a release
     that did not record who approved), counts as auto: only an approver that is not empty and
     not `inventory` makes a host `operator`.

Each issuance takes a serial from the same counter as host certificates, and writes one audit
entry, `service_identity_issued`, naming the host, the service and the serial. A refusal changes
nothing.

## 4. On the host

- **Which services.** Every service the daemon has installed (a `current` version) whose
  entry's manifest says `identity = true`, that is not being uninstalled. The daemon reads it from
  the sealed entry of `current`.
- **Where.** `<identity dir>/<service>/` (`--identity-dir`, default `/var/lib/hostkeeper/identity`,
  the same on macOS), which holds `key.pem` (PKCS#8), `cert.pem` and `ca.pem`. The service's own
  directories are the hook's, as before: the daemon never writes into them. A service is pointed
  at its files by its own configuration (js-agent: a flag naming that directory).
- **Ownership.** The identity root is `root:root 0755`. Each service's directory is owned by root
  with the service account's group, mode `0750`, so the account reads its files but cannot create,
  rename or replace any; root never follows a link it did not make there. Each file is owned by
  the account, mode `0600`. The account is `identityAccount` from the manifest, `root` when absent.
  It is looked up on the host by name (`/etc/passwd` on Linux, the directory service on macOS); an
  account the host does not have is reported and nothing is written. A file in a service's
  directory owned by another user than the account (the account changed) counts as absent: it is
  neither read nor trusted, and the pass makes a new key, asks for a certificate for it and
  installs both, owned by the new account. A link is still refused, and so is a file that is not
  regular or is too large.
- **Writing.** Each file is written to a new temporary file in the same directory, owned and moded
  before any byte is written, synced, and renamed over the old one; the directory is synced. A
  reader sees the old file or the new one, never half of one. The key is written before the
  certificate is asked for, so a restart keeps it and asks again for the same key.
- **The checks before a certificate is installed.** It parses (one PEM block of DER, or DER); it
  is signed by the CA bundle the host trusts the coordinator by; its EKU is `clientAuth` alone;
  it names `urn:hostkeeper:service:<service>` and the host's own id; its key is the key the daemon
  holds for the service; `notBefore <= now < notAfter` on the daemon's clock; and its `notBefore`
  is later than the installed certificate's. A certificate that fails any is not installed, and
  the reason is reported.
- **When.** The daemon checks every service once a minute: a service with no certificate, one
  past half its life, or one that expired is asked for. A refusal is retried after 5 minutes.
- **Reported.** Every gRPC heartbeat of a daemon that keeps service identities carries, per
  service with `identity`, the installed certificate's `notAfter` and the last error
  (`HeartbeatRequest.service_identities`, read whatever features the heartbeat announces: a
  heartbeat's list of features holds at most 16 names, which a Linux daemon already fills, so no
  feature is announced for it, and a daemon that predates the field leaves it empty); `Hosts.Get`
  shows it in the host's heartbeat. The report names at most 16 services, the first 16 by name,
  since the coordinator drops a longer list whole; the daemon keeps the certificates of every
  service all the same.
- **Stopping.** The coordinator refuses a host that is retired, a service that is unassigned or
  being uninstalled, and an auto host without approval: the daemon keeps what it holds, which
  expires within 8 hours, and keeps asking. Uninstalling a service removes its identity
  directory, after the hook's `uninstall` step and before the service is forgotten. After each
  pass the daemon also removes the identity directory of every service it does not have
  installed (no `current` version, or an uninstall under way) and any directory no service
  owns, reading what is installed after the pass, so a pass that began before an uninstall
  leaves no key behind; when it cannot read what is installed it removes nothing. An installed
  service whose entry no longer asks for an identity keeps what it holds.

## 5. The deny list for relying parties

`GET /v1/deny` on the coordinator's heartbeat listener, and the same answer as the gRPC call
`hostkeeper.agent.v1.Coordinator/ListDeny`: every host id with a deny row that is retiring or
retired, sorted, as JSON `{"hostIds":[...]}`. The caller needs a client certificate that is a
current host or service certificate of a host that is not denied; nothing else is answered. A
retire is listed at once: the list is read from the deny rows the coordinator holds.

## 6. Compatibility

- Every change is additive. `Manifest.identity` (10) and `Manifest.identity_account` (11) are new
  optional fields; a daemon before this change ignores them and installs the service with no
  identity, and an entry without them asks for none. A coordinator before this change answers the
  new RPCs `UNIMPLEMENTED`; the daemon reports that and keeps going.
- The coordinator's store gains `Host.identity_approval` (field 14), kept by the file store in the
  host's file and by Postgres in a side table (`enrollment_host_identity`, migration `0020`, which
  an older release may run on: it never reads it, and its writes of a host row do not touch it). A
  row for another key than the host's counts as absent, so a row an older release left behind
  never approves a new key.

## 7. What this replaces

The join token js-agent reads from `/etc/js-agent/token`, placed by hand on each host
(komira-ai/hostkeeper#288 asked hostkeeper to place it). A hostkeeper-managed host gets its
service certificate instead, and js-lease-server accepts it (the jobscheduler side). Hosts launched
by a scaler can keep join tokens until they run hostkeeper.

## 8. Decisions this note takes where the earlier design text assumed code that is gone

- The earlier text puts `identity` in an extension record shipped beside a declaration, and
  makes it a right covered by an operator-approved declaration digest. Neither exists now: the
  manifest travels in the release entry, which the coordinator and the daemon check and every set
  of which is audited. `identity` is a manifest field, and the gate is the audited release plus the
  host's assurance.
- The earlier text has a separate root part that installs files an unprivileged agent publishes
  in a handoff directory. The daemon is one root process now, so it asks and installs itself; the
  checks the handoff installation made are the checks of section 4.
- The earlier text writes into "the service's state directory, owned by the service account".
  The manifest names no state directory and no account, and the daemon never touches a service's
  own directories, so the files go to hostkeeper's per-service identity directory, and the
  account is a new optional manifest field.
- The `operator` and `auto` assurance levels were cut with batch tokens. The only automatic
  approval left is by an inventory record, so that is `auto`; the assurance is read from the
  approval of the host's current key, not stored again.
- `approve-identity` is `HostIdentity.Approve` of the operator API (`POST
  /hostkeeper.operator.v1.HostIdentity/Approve` on the gateway; `hkctl host-identity approve`):
  the operator API has no REST paths, and `operator.proto` is over 1000 lines, so the service is
  its own file.
