# Enrollment at the coordinator

With `--enrollment-dir` the coordinator is the hosts' certificate authority: a host with no
certificate asks for one (`Enroll`), an operator approves it, and the host takes its 7-day
certificate (`Poll`) and renews it while it is valid (`Renew`). Why it is shaped this way:
`docs/design/hostkeeper-design.md`, sections 4.4, 5.7, 6.1 and 8.8. The certificate authority and
the store are the crate `hostkeeper-enrollment`; the coordinator's side is the module
`crates/hostkeeper-coordinator/src/enrollment/`.

The handlers of `Enroll`, `Poll` and `Renew` are built here. The heartbeat listener serves them
as gRPC calls of `hostkeeper.agent.v1.Coordinator` (`crates/hostkeeper-coordinator/src/grpc/`),
with no client certificate asked of `Enroll` and `Poll`.

## Flags

| flag | what it is |
|---|---|
| `--enrollment-dir <dir>` | The enrollment store. Turns enrollment on; requires the three below. |
| `--ca-key-file <file>` | The CA's key: PKCS#8, mode 0600, owned by the coordinator's user. A key file open to group or others, another user's, or a symbolic link stops the start. Its certificate is beside it, `<file>.cert`. |
| `--ca-init` | Make the CA when `--ca-key-file` does not exist. Without it a missing CA stops the start. |
| `--server-cert-file <file>` | The coordinator's server certificate, the CA certificate and the server key, one PEM file (mode 0600) the coordinator writes. The heartbeat listener serves it. |
| `--server-name <name>` | A DNS name the server certificate names, as daemons dial the coordinator; repeat it for each. |
| `--max-pending-enrollments <n>` | How many requests may wait for an operator at once, 1 or more; 256 when not given. A new request over it is refused `pending_full`; no waiting request is dropped for it, and a host posting its waiting request again is still answered. Requires `--enrollment-dir`. |
| `--enrollment-request-ttl <time>` | How long a request waits for an operator before it expires: digits and one unit, `s`, `m`, `h` or `d` (`90m`, `72h`), at least one second; `72h` when not given. Requires `--enrollment-dir`. |
| `--mac-serial-allowlist <file>` | The Mac serial numbers a Mac may be approved for by its MDM nonce, with the host id prefix, role and services of each (below). Without it no Mac is approved by its nonce. Requires `--enrollment-dir`. |

The CA is made once. Every daemon config pins the SHA-256 of its certificate (`ca_sha256`); the
start logs it at info as `ca_sha256`. The server certificate is made again when its file is
missing, does not start with a certificate, or has less than a third of its 90 days left (60 days
after it was made): at the start, and while the coordinator runs, at each look of the reload tick
(`--tls-reload-secs`, 60 seconds when not given), before the tick reloads the TLS files, so the
heartbeat listener serves the new certificate from that same tick. A restart is not needed. The
new file, with a new key and a serial from the store, is written whole: a temporary file
`<file>.tmp` beside it (mode 0600), fsync, rename over the old one, fsync of the directory. Delete
the file to have one made sooner (for new `--server-name` values, say): the next tick makes it.

A renewal that fails (the file cannot be read, or the new one cannot be made or written) changes
nothing that is served: the temporary file is removed, the old file stays as it was, and the
listener keeps serving the old certificate. The failure is logged at error level ("cannot renew
the coordinator's server certificate") and shown in `GET /v1/status`, which with enrollment has

```json
"server_certificate": { "not_after": 1807776000, "renewal_error": "No space left on device (os error 28)" }
```

`not_after` is the end of the certificate in the file, in unix seconds, as the last tick that
read it found it; `renewal_error` is why the last tick failed, and `null` once a tick finds the
certificate not due or renews it. Each tick tries again. Without `--enrollment-dir` the status
has no `server_certificate`. `Releases.Status` over gRPC does not carry it.

With enrollment the heartbeat listener lets a client without a certificate through the TLS
handshake, and admits a client certificate of the coordinator's CA as well as one that chains to
`--client-ca`. Each call then decides: `Enroll` and `Poll` answer a caller without a certificate;
`Heartbeat` and `Renew` refuse it (`forbidden`) before reading its request. Today the heartbeat
route is the one route such a connection reaches, and it answers 403 `forbidden`. Without
`--enrollment-dir` nothing changes: the listener requires a certificate of `--client-ca`.

## State files

Under `--enrollment-dir`, every file one binary protobuf message with a schema number, written
whole (temporary file, fsync, rename, directory fsync), mode 0600:

| file | holds |
|---|---|
| `requests/<request id>.pb` | a request: the CSR, the claims, the image, its state, its approval |
| `issued/<request id>.pb` | the certificate a request was issued |
| `hosts/<host id>.pb` | an approved host: its key's hash, machine id, role, services, the duplicate mark, when its first heartbeat since the approval was accepted |
| `deny/<host id>.pb` | a denied host: retiring, retired, or cleared by a later approval |
| `serial.pb` | the next certificate serial |
| `mac-nonces/<nonce SHA-256>.pb` | a Mac nonce by its SHA-256 only: its serial, the request it was issued for (absent on a nonce stored before that field), when it was issued, the request that consumed it |
| `lock` | the lock every change takes |

A request id is the SHA-256 of the request key's SubjectPublicKeyInfo, as 64 lower-case hex
characters. The poll nonces are kept in memory only: after a restart a daemon starts its poll
again with an unsigned one.

## Approving a request: `Enrollment.List` and `Enrollment.Approve`

The operator's two calls of `hostkeeper.operator.v1.Enrollment` are built as plain functions over
the protobuf messages (`Coordinator::enrollment_list`, `Coordinator::enrollment_approve`); the
coordinator's gRPC serving registers them. Both take the operator token only: a CI token is
`forbidden` and no token or any other `unauthorized`, before the request is looked at.

```
hostkeeper ctl enrollment list --limit 50
hostkeeper ctl enrollment approve --request-id <id> --key-sha256 <key_sha256 as listed> \
    --host-id web-17 --role web --services api --reason "racked in row 4"
```

`List` answers the waiting requests oldest first (the time the coordinator first received each,
then its id), a page at a time (`limit` at most 256; `next_page_token` until the last page), each
with its claims, its `key_sha256`, `renewal_of`, whether it is a `duplicate_claim` and whether a
Mac nonce was presented. It never answers a CSR or a nonce. With `unenrolled` it answers, in the same order and pages, the
requests that expired in their place (Request expiry, below).

`Approve` names the request, the SHA-256 of its key as listed (compared whole, in constant time;
another request's key hash is `key_mismatch`), the host id, the role, the services (each one the
coordinator manages) and a reason. A request (not a renewal) that matches exactly one record of
the host inventory is approved as that record's host id: an empty host id is it, and the answer's
`host_id` names it; another host id is `record_mismatch` unless `replace` is set. Any other request
names its host id. Under the store's lock it is refused when the request is not
pending, is a `duplicate_claim`, or names a host id another key holds or an operator denied,
unless `replace` is set. One write of the request file records the approval: host id, role,
services (the host's first assignment), the operator's token name, the time and the reason. The
certificate is made at the host's next `Poll`, never answered here. With `replace`, the host id's
earlier key no longer names the host: a certificate of it is not admitted at the heartbeat, and
it cannot renew.

Each approval is audited `enroll_approve` (`enroll_replace` with `replace`) under the operator's
token name, with the request id, role, services and the claims; each refused one
`enroll_approve_refused` with its token, and nothing else changes. The audit log is a second file,
so its entry is written right after the approval; an approval whose entry did not land (a crash,
or an audit file that takes no lines) is audited when the coordinator next starts. An approval is
therefore never refused `audit_unavailable`: the request file is its durable record. Once the audit
file's retention has deleted segments, an approval older than the oldest line kept is not audited
again.

The coordinator's own enrollment changes are audited under `coordinator`: a new request
(`enroll_requested`, with its request id, round and `pending` or `duplicate_claim`, and the host
it renews if any), a certificate's first issue at a `Poll` (`enroll_issued`, once per request and
round, with the serial), a `Renew` (`cert_renewed`, with the serial) and a request that expired
(`request_expired`, with its request id, round and the TTL, and the host it renews if any). A
repost of a request, a later poll of an issued one and a refused call write nothing. No reason
holds a CSR, a key, a nonce or a signature.

## Request expiry

A request no operator approves within `--enrollment-request-ttl` of the time the coordinator
first received it expires (design 6.1: `pending` -> `unenrolled`). The coordinator looks at every
reload tick (`--tls-reload-secs`), at the time its wall clock reads, so a request expires at most
one tick after its TTL; one that has waited exactly the TTL still waits. A duplicate claim expires
like any waiting request; an approved, issued or denied one never does. Under the store's lock the
request is marked expired, once, and audited `request_expired`; an expiry whose entry did not land
is audited when the coordinator next starts, as an approval is.

An expired request is unenrolled: `Approve` refuses it `not_pending`, it no longer counts toward
`--max-pending-enrollments`, and `List` with `unenrolled` lists it (with `state` `UNENROLLED`,
`unenrolled_reason` `request_expired`, `unenrolled_unix` and `request_ttl_secs`), where the plain
`List` no longer does. Its host's `Poll` is still answered pending: no wire answer tells a daemon
its request expired, and a daemon posts its request once per start. When the host posts the same
key again (its daemon restarted), the request starts over: pending, first seen at that post,
audited `enroll_requested`, and checked for a duplicate claim like a new one. An expired request's
machine id makes no other request a duplicate claim.

The request file keeps the waiting state it expired in and adds the expiry's time and TTL, fields
a release before expiry ignores: after a downgrade such a release reads the request as waiting
again, as it did before it expired.

| token | when |
|---|---|
| `unauthorized` | no bearer, or not an operator token |
| `forbidden` | a CI token |
| `invalid_request` | a request id, key hash, reason or page token that breaks its rule |
| `invalid_host_id`, `invalid_role`, `invalid_service` | that field breaks its rule; a service the coordinator does not manage; a renewal named under another host id; no host id for a request that matches no single record |
| `unknown_request` | no request has the id |
| `key_mismatch` | the key hash is not the request's |
| `record_mismatch` | the request matches one inventory record, the host id is another, and `replace` is not set |
| `not_pending` | the request was approved, issued or denied already, or it expired |
| `duplicate_claim` | the request is a duplicate claim and `replace` is not set |
| `host_id_taken` | another key holds the host id and `replace` is not set (a Mac's nonce approval takes its own host id over without it: [A Mac approved by its nonce](#a-mac-approved-by-its-nonce)) |
| `denied` | the host id is denied and `replace` is not set |
| `store_failed` | the store could not be read or written; nothing changed |

`Reject` names a pending request and, optionally, a reason. Under the store's lock the request is
marked denied (its file is kept): it leaves the queue and no longer counts toward the pending cap,
its `Poll` is answered `DENIED`, a repost of its key changes nothing, and an approval of it is
`not_pending`. A pending request has no host id, so no deny row names it: its key is the identity
refused. The first rejection is audited `enroll_reject` with the request id, the claims and the
reason, about the host a renewal renews; a repeat answers success and audits nothing. A request
that expired undecided can be rejected too, which keeps its key from waiting again at its next
post. A request approved or issued, or denied after its approval, is `not_pending`; an unknown id
`unknown_request`. As for an approval, the audit entry is written right after the request file,
and one that cannot be written is logged.

`Hosts.Retire` (of the `Hosts` service) names an enrolled host id. Under the fleet's lock it
writes the host's deny row (retiring), drops the host's own entries from the operator's service
assignments with the `host_retire` audit entry in one write of the ops file, and frees the host's
update slot when no update or order of it is in flight, making the row final; otherwise the
host's final report does that, as below. The host's entries are its assignment or exclusion,
its minimum version and an uninstall it was ordered, in each service; a role's stay. The deny
row is the first write, so a retirement cut short by a failed ops write leaves the host refused.
A repeat of a host already denied is finished unless the audit log holds its retirement (the
newest of its approvals and retirements is a `host_retire`): its entries are dropped and it is
audited then. A repeat of a finished retirement audits nothing. A host id the store never approved
is `unknown_host`.

## A Mac approved by its nonce

The MDM policy gate (its own component, not in this coordinator) gives each Mac a one-time nonce
through `Enrollment::issue_mac_nonce`: 32 random bytes, of which the store keeps the SHA-256
only, for one serial and the one pending request the gate saw (its key hash), good for one hour
and one approval. A Mac's `Enroll` carries it (`mac_nonce`). The request is approved, as the
allowlist entry of its serial says and by the approver `mdm-nonce`, only when the serial it claims
is on `--mac-serial-allowlist`, the gate says the device is enrolled and supervised, and the nonce
is known, unexpired, unused, was issued for that serial and was issued for that request. A nonce
stored before the request field names no request and is judged without it. The nonce is consumed
in the approval's own write. Any condition false leaves the request pending for an operator and is
audited `mac_auto_approve_declined` with one of `serial_not_allowed`, `device_state_unavailable`,
`device_not_enrolled`, `device_not_supervised`, `nonce_unknown`, `nonce_expired`, `nonce_used`,
`nonce_wrong_serial`, `nonce_wrong_request`; such an attempt does not use the nonce. Another key
that presents the nonce, a local user's who read it first for instance, is `nonce_wrong_request`.

The approval assigns the allowlist entry's services to the host as an operator's approval assigns
the services it names: one `assign` entry per service, by `mdm-nonce`. An assignment that cannot
be made is logged and leaves the approval standing.

A Mac wiped after its approval comes back with a new key while its host id is held by the old
one. Its nonce approval takes the host id over, audited `enroll_replace`, when the gate says the
device's current MDM enrollment began after the old key's approval and the old key's own request
claimed the same serial; the old key's certificate then names no host, as after an operator's
`replace`. Otherwise it is refused as an operator's approval of a taken host id is
(`enroll_approve_refused`, `host_id_taken`), and a denied host id is never taken over (`denied`).

This coordinator has no gate: it knows no device, so it approves no Mac by its nonce
(`device_state_unavailable`). No operator token may be named `mdm-nonce`.

```toml
[[mac]]
serial = "C02XK1ABJG5J"
host_id_prefix = "mac-"     # the host id is the prefix and the serial in lower case
role = "build"
services = ["builder"]      # services the rollout file defines
```

A file that does not read, a serial listed twice, or a serial, host id, role or service that
breaks its rule or names a service the rollout file does not define stops the start.

## Backup

Back up the CA key file and its `.cert` once, right after the CA is made, sealed and offline:
[docs/operations/ca-backup.md](operations/ca-backup.md). Rotating the CA is not built. The enrollment
store is backed up with the rest of the coordinator's state, by `hostkeeper-coordinator backup`:
[docs/operations/coordinator-state.md](operations/coordinator-state.md).

## What a host is told

Once a transport carries the calls, a refusal is a gRPC status whose message is one token of `CoordinatorRefusal`
(`crates/hostkeeper-proto/proto/hostkeeper/agent/v1/agent.proto`):

| token | when |
|---|---|
| `forbidden` | `Heartbeat` or `Renew` with no client certificate, or one that names no host |
| `pending_full` | an `Enroll` of a new request while `--max-pending-enrollments` requests wait |
| `bad_csr` | the CSR is over 4096 bytes, does not parse, is not signed by its own key, or its subject is not `host:<id>` |
| `key_not_p256` | the CSR's key is not an ECDSA P-256 key |
| `renewal_not_allowed` | an `Enroll` whose `renewal_of` names an unknown or denied host or one certified for another key; a `Renew` for another key, by a host whose key was replaced, or by a host marked duplicate |
| `unknown_request` | a `Poll` for a request id the coordinator holds no request for |
| `bad_signature` | a signed `Poll` whose nonce was not given for the request, was already answered, or whose signature does not verify by the request's key |
| `denied` | a `Heartbeat` or `Renew` by a host an operator denied, with a certificate not yet expired |

A `Poll` before approval is answered `PENDING`, not refused; a `Poll` of a denied request is
answered `DENIED`. A request that cannot be read or written, or a failure to sign, is the gRPC
status `INTERNAL` with no token. The JSON heartbeat route answers a denied host 403 with
`{"error":"denied"}`.

A denied host whose update was in flight is still heard: its heartbeat is read for its final
report and refused `denied`. When the report shows no update in flight and no order outstanding,
the coordinator forgets the host, which frees its update slot, and marks the deny row
final; from then on its heartbeats are refused before they are read.

## Not here yet

The wire transport of `Enroll`, `Poll` and `Renew`, and of `Enrollment.List` and
`Enrollment.Approve` (the coordinator's gRPC serving). The MDM policy gate and its `DeviceState`.
Neither transport carries a machine id in a heartbeat yet, so the duplicate mark (two live
daemons on one host id) is only set when a caller of `Coordinator::heartbeat_from` passes one.
