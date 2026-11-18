# Enrollment at the coordinator

With `--enrollment-dir` the coordinator is the hosts' certificate authority: a host with no
certificate asks for one (`Enroll`), an operator approves it, and the host takes its 7-day
certificate (`Poll`) and renews it while it is valid (`Renew`). Why it is shaped this way:
`docs/design/hostkeeper-design.md`, sections 4.4, 5.7, 6.1 and 8.8. The certificate authority and
the store are the crate `hostkeeper-enrollment`; the coordinator's side is the module
`crates/hostkeeper-coordinator/src/enrollment/`.

A host certificate names its host as `CN=host:<id>` and carries one subjectAltName, the URI of
the fixed host role `host`: the coordinator's `--host-role-uri-prefix` (by default
`urn:hostkeeper:host-role:`) followed by `host`, so `urn:hostkeeper:host-role:host`. That role
means nothing and is the same for every host: the coordinator reads who a host is from its host
id and its key, and which host group it is in from its enrollment record
([docs/design/hostgroups.md](design/hostgroups.md) section 1.2), never from its certificate. A
certificate issued by a release before names no host role and keeps working until it is renewed,
read for its host id alone; its renewal carries the URI.

The handlers of `Enroll`, `Poll`, `Renew`, `IssueServiceCertificate` and `ListDeny` are built here. The heartbeat listener serves them
as gRPC calls of `hostkeeper.agent.v1.Coordinator` (`crates/hostkeeper-coordinator/src/grpc/`),
with no client certificate asked of `Enroll` and `Poll`. With `--lan-listen`, a third listener on
the farm LAN serves the same, to network-installed hosts, which have no tailnet
(`docs/design/network-install.md` section 7).

## Flags

| flag | what it is |
|---|---|
| `--enrollment-dir <dir>` | The enrollment store of the file store. Requires the three below. With `--database-dsn-file` it is refused: the enrollment store is the database's ([coordinator-state.md](operations/coordinator-state.md#the-database-store)). |
| `--ca-key-file <file>` | The CA's key: PKCS#8, mode 0600, owned by the coordinator's user. A key file open to group or others, another user's, or a symbolic link stops the start. Its certificate is beside it, `<file>.cert`. Turns enrollment on; requires `--enrollment-dir` or `--database-dsn-file`, and `--server-cert-file` and `--server-name`. |
| `--ca-init` | Make the CA when `--ca-key-file` does not exist. Without it a missing CA stops the start. |
| `--server-cert-file <file>` | The coordinator's server certificate, the CA certificate and the server key, one PEM file (mode 0600) the coordinator writes. The heartbeat listener serves it. |
| `--server-name <name>` | A DNS name the server certificate names, as daemons dial the coordinator; repeat it for each. |
| `--lan-listen <address:port>` | A third listener, on the farm LAN, for network-installed hosts: it serves what the heartbeat listener serves, with its TLS and its checks (below), and the server certificate names its IP address. The address of one interface: a wildcard address stops the start unless `--lan-listen-wildcard` is given with `--lan-address`. Requires `--ca-key-file`. |
| `--lan-listen-wildcard` | Take a wildcard `--lan-listen` (`0.0.0.0` or `[::]`): the LAN listener listens on every interface of the coordinator's network namespace, as a Swarm task's does with its port published on the host in `mode=host`. Off unless given; an address of one interface is taken as without it. Requires `--lan-listen` and `--lan-address`. |
| `--lan-address <ip>` | The address daemons dial the LAN listener at, the host's LAN address: the server certificate names it in place of the address of `--lan-listen`, which with a wildcard is none a daemon dials. Not a wildcard address (the start stops). Requires `--lan-listen-wildcard`; without the wildcard the certificate names the address of `--lan-listen` as before. |
| `--max-pending-enrollments <n>` | How many requests may wait for an operator at once, 1 or more; 256 when not given. A new request over it is refused `pending_full`; no waiting request is dropped for it, and a host posting its waiting request again is still answered. Requires `--ca-key-file`. |
| `--host-cert-validity-secs <n>` | How long each host certificate the CA issues or renews is valid, in seconds: 60 to 604800 (7 days, the CA's cap), 604800 when not given. The daemon renews at half life. A certificate already issued keeps its validity; the next issuance or renewal takes the new one. Requires `--ca-key-file`. |
| `--enrollment-request-ttl <time>` | How long a request waits for an operator before it expires: digits and one unit, `s`, `m`, `h` or `d` (`90m`, `72h`), at least one second; `72h` when not given. Requires `--ca-key-file`. |
| `--record-approve-for <time>` | The approval window of a new inventory record that names a host group (or a role) when `Enrollment.AddRecord` names none: digits and one unit, `s`, `m`, `h` or `d` (`90m`, `24h`), at least one second, no longer than `--record-approve-max`; `24h` when not given. While it is open a pending request that matches the record exactly is approved by the coordinator itself. Needs no `--enrollment-dir`. |
| `--record-approve-max <time>` | The longest approval window a record may be given, by `Enrollment.AddRecord` and `Enrollment.OpenRecord` alike; a longer one is refused `invalid_window`; `7d` when not given. A `--record-approve-for` above it stops the start. Needs no `--enrollment-dir`. |
| `--lan-name <dns-name>` | A DNS name daemons dial the LAN listener by: the server certificate names each one given, beside its IP address. Repeatable. Lowercase letters, digits, `-` and `.`, at most 253 bytes, each label 1 to 63 and not starting or ending with `-`; a wildcard, an IP address or a trailing dot stops the start. A name given with `--server-name` too is named once. Requires `--lan-listen`, with or without `--lan-listen-wildcard`. |

The CA is made once. Every daemon config pins the SHA-256 of its certificate (`ca_sha256`); the
start logs it at info as `ca_sha256`. The server certificate names the DNS names of
`--server-name` and each `--lan-name`, and then, with `--lan-listen`, its IP address (`--lan-address` when the listener is a wildcard; an IPv4 address written as IPv6 is
named as IPv4), so a daemon may dial the LAN listener by address. It is made again when its file is
missing, does not start with a certificate, has less than a third of its 90 days left (60 days
after it was made), or names other names than these (a name or the address added, removed or
changed; their order is not compared): at the start, and while the coordinator runs, at each look of the reload tick
(`--tls-reload-secs`, 60 seconds when not given), before the tick reloads the TLS files, so the
heartbeat listener serves the new certificate from that same tick. A restart is not needed. The
new file, with a new key and a serial from the store, is written whole: a temporary file
`<file>.tmp` beside it (mode 0600), fsync, rename over the old one, fsync of the directory. New
`--server-name` or `--lan-name` values or a new `--lan-listen` take effect at the restart that gives them; delete
the file to have one made sooner for any other reason: the next tick makes it.

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

The LAN listener of `--lan-listen` is a second listener of the heartbeat surface: the same TLS
source (the server certificate, a client certificate optional), the same calls and the same
checks. It answers `Enroll` and `Poll` to a caller without a certificate, `Heartbeat`, `Renew`
and the JSON heartbeat route to a host certificate only, and never the operator API (a gRPC call
of it is `UNIMPLEMENTED`), its JSON gateway, the private listener's JSON routes or the UI's files
(404 `not_found`). The start line names it after the other two, as `lan=<address:port>`.

## State files

On the database store (`--database-dsn-file`) the enrollment store is the tables
`enrollment_requests`, `enrollment_issued`, `enrollment_hosts`, `enrollment_deny`,
`enrollment_serial` and `mac_nonces` (which nothing writes now), one column per field of the messages below, and an approval
(the host, the request, the deny row cleared) is one transaction. The
store's lock is its in-process mutex; the database's advisory lock keeps other coordinators off.
The heartbeat's reads (a host's key, role and host group, its deny row, its duplicate mark) come from a copy of
the hosts and the deny rows in memory, loaded at start and changed after each commit, so a
heartbeat runs no query; the first heartbeat time and the duplicate mark are each written once.
An enrollment change's audit row is the coordinator's second transaction, as its second write is
on the files: when it fails, the change stands, the failure is logged, and the next start audits
it.

Under `--enrollment-dir`, every file one binary protobuf message with a schema number, written
whole (temporary file, fsync, rename, directory fsync), mode 0600:

| file | holds |
|---|---|
| `requests/<request id>.pb` | a request: the CSR, the claims, the image, its state, its approval |
| `issued/<request id>.pb` | the certificate a request was issued |
| `hosts/<host id>.pb` | an approved host: its key's hash, machine id, role, services, host group (and, while a move runs, the group it moves from), the duplicate mark, when its first heartbeat since the approval was accepted, and when the first that reported a service the coordinator manages for it serving was |
| `deny/<host id>.pb` | a denied host: retiring, retired, or cleared by a later approval |
| `serial.pb` | the next certificate serial |
| `mac-nonces/<nonce SHA-256>.pb` | a Mac nonce of an earlier release, by its SHA-256 only. Nothing writes it now; the directory and the table `mac_nonces` stay, so a store of an earlier release reads |
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
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment list --limit 50
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment approve --request-id <id> --key-sha256 <key-sha256> \
    --host-id web-17 --host-group web --reason "racked in row 4"
```

`List` answers the waiting requests oldest first (the time the coordinator first received each,
then its id), a page at a time (`limit` at most 256; `next_page_token` until the last page), each
with its claims, its `key_sha256`, `renewal_of`, whether it is a `duplicate_claim`, its match against the host inventory (with the one record's host id,
host group, serial and BMC MAC; `record_role` carries the group's name too), and `contested_by`: the ids of the other pending requests that match a
record it matches, looked for among every pending request, not the page. It never answers a CSR
or a nonce. With `unenrolled` it answers, in the same order and pages, the
requests that expired in their place (Request expiry, below).

Automatic approval: a pending request that matches exactly one open, unused record naming a host
group the coordinator holds, with no other pending request contesting that record, is approved by
the coordinator itself, in that group, while the record's window is open (`--record-approve-for`,
`--record-approve-max`), once. The audit entry is `enrollment.auto_approved`, naming the group. Any other match only fills in the form. See
[operations/enrollment.md](operations/enrollment.md).

`Approve` names the request, the SHA-256 of its key as listed (compared whole, in constant time;
another request's key hash is `key_mismatch`), the host id, the host group (by `host_group`, or by
`role` read as the group's name; it must be a group the coordinator holds, `unknown_host_group`;
naming neither enrolls the host in no group (unassigned: it is ordered no package, and keeps what it runs, until `HostGroups.Move`); a `role` beside `host_group` that is another name is `invalid_role`) and, optionally, a
reason (1 to 256 bytes; the audit entry names the approver and the group either way). It names no services (`services_retired`): the host's group decides what it runs. A request (not a renewal) that matches exactly one record of
the host inventory is approved as that record's host id: an empty host id is it, and the answer's
`host_id` names it; another host id is `record_mismatch` unless `replace` is set. Any other request
names its host id. Before that, a request that another pending request contests (they match one
record) is `record_contested`, `replace` or not, until the operator rejects all but one. Under the store's lock it is refused when the request is not
pending, is a `duplicate_claim`, or names a host id another key holds or an operator denied,
unless `replace` is set; a renewal, or a `replace` of a host that is not denied, that names another
group than that host's is refused `host_group_differs` (a host changes group only by
`HostGroups.Move`). One write of the request file records the approval: host id, host group (as
the role too), the operator's token name, the time and the reason; the host record carries the
group from that write, so `HostGroups.Remove` counts the host before its first `Poll`. The group
is checked and the approval written under the fleet's lock, which `HostGroups.Remove` checks
under, as are a record's `AddRecord` and an approval by a record, so no host or record lands in a
group being removed. The
certificate is made at the host's next `Poll`, never answered here. With `replace`, the host id's
earlier key no longer names the host: a certificate of it is not admitted at the heartbeat, and
it cannot renew.

Each approval is audited `enroll_approve` (`enroll_replace` with `replace`) under the operator's
token name, with the request id, the group (`group web`, and the entry's `host_group`) and the
claims; each refused one
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

## Service identity

`IssueServiceCertificate` signs an 8-hour client certificate for a service a host runs, and
`ListDeny` (and `GET /v1/deny`) lists the hosts the coordinator denies, for a relying party; the
design is `docs/design/service-identity.md` and the operator's side is
[docs/operations/enrollment.md](operations/enrollment.md#service-identity). The handlers are in
`enrollment/identity.rs` (the store, the CA and the audit), `service_identity.rs` (is the service
the host's, and does its entry say `identity`) and `operator_host_identity.rs` (`HostIdentity.Get`
and `Approve`); the daemon's report is kept by `service_identity_report.rs`.

The checks of `IssueServiceCertificate` are made in this order, and the caller is told the first
that fails:

1. the client certificate is a current host certificate of an enrolled host (`forbidden`): a
   service certificate is none;
2. the host is not denied (`denied`);
3. the service is a service name (`invalid_service`);
4. the CSR is a P-256 request signed by its own key (`bad_csr`, `key_not_p256`): its subject is
   not looked at, and nothing but its key is copied;
5. the service is assigned to the host (`service_not_assigned`): in group mode, the host's group
   lists it; in role mode, the rollout file and the operator's assignments give it to the host's
   role; and no uninstall of it is ordered on the host. A host in no group is assigned nothing;
6. the entry the coordinator would pick for the host at the service's desired version has
   `identity` in its manifest (`service_not_assigned` too). A host whose daemon was not heard
   since the coordinator started is picked nothing until its next heartbeat;
7. the host's assurance allows it (`identity_not_approved`): approved by an operator, or its
   current key approved with `HostIdentity.Approve`.

It then takes a serial (`allocate_serial`, the counter of the host certificates), signs and writes
the audit entry `service_identity_issued` (under `coordinator`: the host as node, the service,
`serial <n>`; never the CSR) before it answers; a serial or an entry that cannot be written is
`store_failed` (UNAVAILABLE) and nothing is answered. A refusal changes nothing and is logged at
info level with the host and the service.

`ListDeny` needs a client certificate that is a current host certificate or a service certificate
of this CA that is valid now (`notBefore <= now < notAfter` by the coordinator's clock), of a host
that is not denied; the deny rows are read as they are, so a retire is listed at once.

The heartbeat's `service_identities` is read only over gRPC from a daemon that announced
`service-identity-v1` (a feature the coordinator advertises): at most 16 services, each a service
name, none twice, each `last_error` cut to 256 bytes at a character boundary. The last good list is
kept in memory and shown by `Hosts.Get` and `Hosts.List` in the host's heartbeat; a list that
breaks a rule is counted as a dropped fact `service_identities` and the last one is kept; an empty
list from a daemon that announces the feature clears it.

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
| `invalid_host_id`, `invalid_role`, `invalid_service` | that field breaks its rule; a service the coordinator does not manage; a renewal named under another host id; no host id for a request that matches no single record; a role beside `host_group` that is another name; no role, from a coordinator that holds no host groups |
| `services_retired` | `services` named on a coordinator that holds host groups |
| `unknown_host_group` | a host group, or a role read as one, that the coordinator does not hold; any `host_group` on a coordinator that holds none |
| `host_group_differs` | a renewal, or a `replace`, naming another group than the host it renews or replaces is in |
| `unknown_request` | no request has the id |
| `key_mismatch` | the key hash is not the request's |
| `record_mismatch` | the request matches one inventory record, the host id is another, and `replace` is not set |
| `record_contested` | another pending request matches an inventory record the request matches; `replace` does not set it aside |
| `not_pending` | the request was approved, issued or denied already, or it expired |
| `duplicate_claim` | the request is a duplicate claim and `replace` is not set |
| `host_id_taken` | another key holds the host id and `replace` is not set |
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

## A Mac

A Mac enrolls as any host does: its daemon posts `Enroll` with the claims of its serial number and
platform UUID (`mac_serial`, `platform_uuid`), and the request waits for an operator or for an
inventory record that matches it (`Enrollment.AddRecord`, by serial; see Approving a request). A
Mac's host group is the one its approval or its record names. There is no approval by a nonce and
no MDM at the coordinator: the coordinator's own MDM, which gave a Mac a one-time nonce and
approved it by an allowlist of serials, was removed (2026-11-14), and Macs come to the coordinator
through the bootstrap package of Apple Business device management
(`docs/operations/mac-bootstrap.md`, `docs/design/mac-bootstrap.md`). A serial a Mac claims is its
own word, as any claim is: what approves it is an operator's click or a record's window.

An `Enroll` that carries a `mac_nonce` (a daemon of an earlier release does, when its managed
preferences hold one) is read as any other: the field is ignored, the request is pending, and
`mac_nonce_presented` of the pending list is false. The audit lines of the earlier release that
approved by a nonce (`by` `mdm-nonce`, `mdm-gate`) stay in the audit file and still read; no
operator token may be named so.

## Backup

Back up the CA key file and its `.cert` once, right after the CA is made, sealed and offline:
[docs/operations/ca-backup.md](operations/ca-backup.md). Rotating the CA is not built. The enrollment
store is backed up with the rest of the coordinator's state: by `hostkeeper-coordinator backup` on
the file store, and with the database on the database store:
[docs/operations/coordinator-state.md](operations/coordinator-state.md).

## What a host is told

Once a transport carries the calls, a refusal is a gRPC status whose message is one token of `CoordinatorRefusal`
(`crates/hostkeeper-proto/proto/hostkeeper/agent/v1/agent.proto`):

| token | when |
|---|---|
| `forbidden` | `Heartbeat`, `Renew`, `IssueServiceCertificate` or `ListDeny` with no client certificate, or one that names no host (a service certificate is none, but for `ListDeny`) |
| `pending_full` | an `Enroll` of a new request while `--max-pending-enrollments` requests wait |
| `bad_csr` | the CSR is over 4096 bytes, does not parse, is not signed by its own key, or its subject is not `host:<id>` |
| `key_not_p256` | the CSR's key is not an ECDSA P-256 key |
| `renewal_not_allowed` | an `Enroll` whose `renewal_of` names an unknown or denied host or one certified for another key; a `Renew` for another key, by a host whose key was replaced, or by a host marked duplicate |
| `unknown_request` | a `Poll` for a request id the coordinator holds no request for |
| `bad_signature` | a signed `Poll` whose nonce was not given for the request, was already answered, or whose signature does not verify by the request's key |
| `denied` | a `Heartbeat`, `Renew`, `IssueServiceCertificate` or `ListDeny` by a host an operator denied, with a certificate not yet expired |
| `service_not_assigned`, `identity_not_approved`, `invalid_service` | an `IssueServiceCertificate`, as the section above says |
| `store_failed` | an `IssueServiceCertificate` whose serial or audit entry could not be written (UNAVAILABLE): nothing was issued |

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
`Enrollment.Approve` (the coordinator's gRPC serving). Neither transport carries a machine id in a heartbeat yet, so the duplicate mark (two live
daemons on one host id) is only set when a caller of `Coordinator::heartbeat_from` passes one.
