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

The handlers of `Enroll`, `Poll` and `Renew` are built here. The heartbeat listener serves them
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
| `--lan-listen <address:port>` | A third listener, on the farm LAN, for network-installed hosts: it serves what the heartbeat listener serves, with its TLS and its checks (below), and the server certificate names its IP address. The address of one interface: a wildcard address stops the start. Requires `--ca-key-file`. |
| `--max-pending-enrollments <n>` | How many requests may wait for an operator at once, 1 or more; 256 when not given. A new request over it is refused `pending_full`; no waiting request is dropped for it, and a host posting its waiting request again is still answered. Requires `--ca-key-file`. |
| `--host-cert-validity-secs <n>` | How long each host certificate the CA issues or renews is valid, in seconds: 60 to 604800 (7 days, the CA's cap), 604800 when not given. The daemon renews at half life. A certificate already issued keeps its validity; the next issuance or renewal takes the new one. Requires `--ca-key-file`. |
| `--enrollment-request-ttl <time>` | How long a request waits for an operator before it expires: digits and one unit, `s`, `m`, `h` or `d` (`90m`, `72h`), at least one second; `72h` when not given. Requires `--ca-key-file`. |
| `--record-approve-for <time>` | The approval window of a new inventory record that names a host group (or a role) when `Enrollment.AddRecord` names none: digits and one unit, `s`, `m`, `h` or `d` (`90m`, `24h`), at least one second, no longer than `--record-approve-max`; `24h` when not given. While it is open a pending request that matches the record exactly is approved by the coordinator itself. Needs no `--enrollment-dir`. |
| `--record-approve-max <time>` | The longest approval window a record may be given, by `Enrollment.AddRecord` and `Enrollment.OpenRecord` alike; a longer one is refused `invalid_window`; `7d` when not given. A `--record-approve-for` above it stops the start. Needs no `--enrollment-dir`. |
| `--mac-serial-allowlist <file>` | The Mac serial numbers a Mac may be approved for by its MDM nonce, with the host id prefix and host group of each (below). Without it no Mac is approved by its nonce. Requires `--ca-key-file`. |
| `--mdm-api-url <url>` | NanoHUB's API, on loopback: `http://<loopback address>:<port>`, as `http://127.0.0.1:9004`; a name (even `localhost`), another address, `https://`, a path or a query stops the start. With it the coordinator runs the MDM policy gate (below). Requires the two below and enrollment (`--ca-key-file`, with `--enrollment-dir` or `--database-dsn-file`); the gate's device records are kept with the enrollment store, in `--enrollment-dir` or in the database's table `mdm_devices`. |
| `--mdm-api-key-file <file>` | NanoHUB's API key (NanoHUB's `-api-key`): one key of visible ASCII, in a file of mode 0600 or 0400 owned by the coordinator's user; a link, another mode or owner, or an empty file stops the start, and the message names the flag, never the path or the key. Requires `--mdm-api-url`. |
| `--mdm-webhook-listen <addr>` | Where the gate serves NanoHUB's webhook: a loopback address and port, as `127.0.0.1:9100`; another address stops the start. Requires `--mdm-api-url`. |
| `--mdm-profile-identifier <id>` | The farm's MDM profile identifier, as the kit's settings say it (`com.komira.hostkeeper.mdm`): two to sixteen labels of letters, digits and `-`, at most 200 characters; anything else stops the start. With it and `--mdm-daemon-config` the gate runs its enrollment side (below): it installs the config profile `<id>.config` and pushes the nonce profile `<id>.nonce`. Requires `--mdm-api-url` and `--mdm-daemon-config`. |
| `--mdm-daemon-config <file>` | The daemon config the config profile carries: a `hostkeeper.toml` of the farm (`coordinator`, `ca_sha256`, `release_channel`, `trusted_signers`, optional `bounds`). It is read at start and judged by the daemon's own loader; a file that does not read, or that the loader refuses, stops the start with the loader's reason. It holds no secret. Requires `--mdm-profile-identifier`. |
| `--mdm-stores <file>` | Optional: the `stores.toml` of the farm's Macs, carried in the config profile as the domain `dev.hostkeeper.stores`, which a Mac's daemon writes to `/etc/hostkeeper/stores.toml`, replacing a file that differs. It is read at start and judged by the daemon's own stores loader; a file the loader refuses, or a store that names a credential file (a profile carries no secret), stops the start. Requires `--mdm-profile-identifier`. |
| `--mdm-macos-pkg <file>` | Optional: the `macos-pkg.toml` of the farm's Macs, the signers of the `macos-pkg` installer, carried as the domain `dev.hostkeeper.macos-pkg` and written to `/etc/hostkeeper/macos-pkg.toml` the same way. Judged at start by the installer's own signers loader. Requires `--mdm-profile-identifier`. Rotation: [repository-keys.md](operations/repository-keys.md). |

The CA is made once. Every daemon config pins the SHA-256 of its certificate (`ca_sha256`); the
start logs it at info as `ca_sha256`. The server certificate names the DNS names of
`--server-name` and then, with `--lan-listen`, its IP address (an IPv4 address written as IPv6 is
named as IPv4), so a daemon may dial the LAN listener by address. It is made again when its file is
missing, does not start with a certificate, has less than a third of its 90 days left (60 days
after it was made), or names other names than these (a name or the address added, removed or
changed; their order is not compared): at the start, and while the coordinator runs, at each look of the reload tick
(`--tls-reload-secs`, 60 seconds when not given), before the tick reloads the TLS files, so the
heartbeat listener serves the new certificate from that same tick. A restart is not needed. The
new file, with a new key and a serial from the store, is written whole: a temporary file
`<file>.tmp` beside it (mode 0600), fsync, rename over the old one, fsync of the directory. New
`--server-name` values or a new `--lan-listen` take effect at the restart that gives them; delete
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
`enrollment_serial` and `mac_nonces`, one column per field of the messages below, and an approval
(the host, the request, the deny row cleared, the Mac nonce consumed) is one transaction. The
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
| `mac-nonces/<nonce SHA-256>.pb` | a Mac nonce by its SHA-256 only: its serial, the request it was issued for (absent on a nonce stored before that field), when it was issued, the request that consumed it |
| `mdm-devices/<enrollment id>.pb` | with the MDM gate, its record of one device (below), with the SHA-256 of the record, so a torn file is refused; on the database store, the row of the device in `mdm_devices`, holding the same bytes |
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
with its claims, its `key_sha256`, `renewal_of`, whether it is a `duplicate_claim` and whether a
Mac nonce was presented, its match against the host inventory (with the one record's host id,
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
naming neither, or a `role` beside `host_group` that is another name, is `invalid_role`) and a
reason. It names no services (`services_retired`): the host's group decides what it runs. A request (not a renewal) that matches exactly one record of
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
| `invalid_host_id`, `invalid_role`, `invalid_service` | that field breaks its rule; a service the coordinator does not manage; a renewal named under another host id; no host id for a request that matches no single record; no host group nor role named; a role beside `host_group` that is another name |
| `services_retired` | `services` named on a coordinator that holds host groups |
| `unknown_host_group` | a host group, or a role read as one, that the coordinator does not hold; any `host_group` on a coordinator that holds none |
| `host_group_differs` | a renewal, or a `replace`, naming another group than the host it renews or replaces is in |
| `unknown_request` | no request has the id |
| `key_mismatch` | the key hash is not the request's |
| `record_mismatch` | the request matches one inventory record, the host id is another, and `replace` is not set |
| `record_contested` | another pending request matches an inventory record the request matches; `replace` does not set it aside |
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

The MDM policy gate gives each Mac a one-time nonce through `Enrollment::issue_mac_nonce`: 32
random bytes, of which the store keeps the SHA-256
only, for one serial and the one pending request the gate saw (its key hash), good for one hour
and one approval. A Mac's `Enroll` carries it (`mac_nonce`): the daemon reads it from the managed
preferences the MDM installed and posts its pending request once more with it
([operations/enrollment.md](operations/enrollment.md#the-mac-path)). The answer to that post is
`ENROLL_STATE_UNSPECIFIED` when the nonce approved the request, as it is for any request no longer
pending, and the daemon's next `Poll` brings the certificate. The request is approved, as the
allowlist entry of its serial says and by the approver `mdm-nonce`, only when the serial it claims
is on `--mac-serial-allowlist`, the gate says the device is enrolled and supervised, and the nonce
is known, unexpired, unused, was issued for that serial and was issued for that request. A nonce
stored before the request field names no request and is judged without it. The nonce is consumed
in the approval's own write. Any condition false leaves the request pending for an operator and is
audited `mac_auto_approve_declined` (`device_not_via_dep` only when the coordinator is set to require it, `Enrollment::with_mac_require_dep`; off by default) with one of `serial_not_allowed`, `device_state_unavailable`,
`device_not_enrolled`, `device_not_supervised`, `device_not_via_dep`, `nonce_unknown`, `nonce_expired`, `nonce_used`,
`nonce_wrong_serial`, `nonce_wrong_request`; such an attempt does not use the nonce. Another key
that presents the nonce, a local user's who read it first for instance, is `nonce_wrong_request`.

The approval puts the Mac in its allowlist entry's host group, audited `enroll_approve` by
`mdm-nonce` naming the group, and assigns nothing: the group decides what the Mac runs. A Mac
wiped and taking its host id over is held to its host's group as a `replace` is: one moved to
another group than its entry names is refused `host_group_differs` and waits for an operator.

A Mac wiped after its approval comes back with a new key while its host id is held by the old
one. Its nonce approval takes the host id over, audited `enroll_replace`, when the gate says the
device's current MDM enrollment began after the old key's approval and the old key's own request
claimed the same serial; the old key's certificate then names no host, as after an operator's
`replace`. Otherwise it is refused as an operator's approval of a taken host id is
(`enroll_approve_refused`, `host_id_taken`), and a denied host id is never taken over (`denied`).

A coordinator without the gate (no `--mdm-api-url`) knows no device, so it approves no Mac by its
nonce (`device_state_unavailable`); with it, the gate's records say whether the device is enrolled
and supervised and when its current enrollment began. No operator token may be named `mdm-nonce`,
`inventory` or `mdm-gate`.

```toml
[[mac]]
serial = "C02XK1ABJG5J"
host_id_prefix = "mac-"     # the host id is the prefix and the serial in lower case
host_group = "build"        # a group the coordinator holds
```

A file that does not read, a serial listed twice, an entry that names no `host_group`, or a
serial, host id or group that breaks its rule stops the start. An entry written before groups
names `role` instead, read as the group's name (with both, they must be one name), and may name
`services`. A coordinator that holds a host group at its start refuses an entry that names
`services` (`--mac-serial-allowlist: the entry of serial <serial> names services; ...`) or a
group it does not hold (`--mac-serial-allowlist names host group <name>, which the coordinator
does not hold`), and `HostGroups.Remove` refuses a group an entry names (`not_empty`). A
coordinator that holds no group approves the Mac as its entry's role and assigns its
`services`, one `assign` entry per service by `mdm-nonce`, each a service the rollout file
defines, or the start stops; should it come to hold groups while it runs, an entry with
`services`, or naming a group it does not hold, is refused as an operator's approval would be
(`services_retired`, `unknown_host_group`) until the file is fixed and the coordinator started
again.

## The MDM policy gate

With `--mdm-api-url`, `--mdm-api-key-file` and `--mdm-webhook-listen` the coordinator runs the
device side of the MDM policy gate, the crate `hostkeeper-mdm-gate`
([docs/design/hostkeeper-mdm.md](design/hostkeeper-mdm.md) sections 3 and 6.1). It is linked in:
the enrollment asks it about a serial as a library call. NanoHUB 0.2.0 runs on the same host and
is started with `-webhook-url http://<--mdm-webhook-listen>/webhook`.

- **What it hears.** NanoHUB posts each check-in (`mdm.Authenticate`, `mdm.TokenUpdate`,
  `mdm.CheckOut`) and each command result (`mdm.Connect`) of a device channel to `POST /webhook`.
  The listener answers `200` once the event is recorded or passed over, `400` for a body that is no
  event, `404` and `405` for another path or method, `413` over 4 MiB, and `500` when the record
  cannot be written; NanoHUB logs any answer but `200` and does not send the event again.
- **What it sends.** At a device's first `TokenUpdate` of an enrollment it queues
  `DeviceInformation` (`SerialNumber`, `UDID`, `IsSupervised`) and `SecurityInfo` through NanoHUB's
  API (`PUT /api/v1/nanomdm/enqueue/<id>`, basic authentication as `nanohub`). A command the device
  has not answered is never sent again, so a Mac that is off piles nothing up; a query NanoHUB did
  not queue, or never confirmed it queued (the coordinator stopped while asking), is sent again at
  the device's next event, and one the device answered with an error at its next `TokenUpdate`. The gate's code sends only five command types (`DeviceInformation`,
  `SecurityInfo`, `InstallProfile`, `RemoveProfile`, `InstallEnterpriseApplication`); any other is
  refused before NanoHUB is called. This bounds the code, not a holder of the key.
- **What it believes.** A result is read only when its command UUID is one the gate sent that
  device and the device has not answered (NanoHUB's webhook carries no signature). A device is
  enrolled from its first `TokenUpdate` until a `CheckOut`; an `Authenticate` begins a new
  enrollment and forgets the serial and supervision the device gave in the last. The serial is the
  one its `DeviceInformation` answer gave, never the `Authenticate`'s alone, and the device is
  supervised only when that answer says `IsSupervised` true. `SecurityInfo`'s `EnrolledViaDEP` is carried in the device's status
  (`enrolled_via_dep`, `None` until the answer) and decides only where the coordinator requires it
  (`device_not_via_dep`); `UserApprovedEnrollment` is shown and decides nothing. Both are what the Mac says of itself: evidence, not an identity.
- **What it says.** The enrollment's question about a serial is answered from the records, never by
  asking the device: a serial no record has, or one two enrollments that have not checked out
  have, is `device_state_unavailable`; a checked-out device is `device_not_enrolled`; an
  unsupervised one `device_not_supervised`.
- **Its records.** One file per device under `mdm-devices/` (above), or with `--database-dsn-file`
  one row per device of the table `mdm_devices`, written before the gate acts on the event, so a restart loses nothing and a result of a command sent before the restart is
  still matched. A record that does not read stops the start. `Mdm.ListDevices` of the operator
  API (`hkctl mdm list-devices`, [docs/cli.md](cli.md)) lists them; it asks no device.

- **Its enrollment side.** With `--mdm-profile-identifier` and `--mdm-daemon-config` the gate
  also acts for each device that is enrolled, has a serial from its `DeviceInformation` answer
  that no other live enrollment has, and whose serial is on `--mac-serial-allowlist` (design
  sections 4 and 5). It looks at each webhook event of a device, at each request posted to the
  coordinator, and at every device once a minute; one profile command a look.
  - Once a request claiming the serial was approved in this enrollment (at or after its first
    `TokenUpdate`), it sends no nonce and no pkg to the Mac in this enrollment, and removes the
    nonce profile it pushed in it (`RemoveProfile`, once). Only a changed config profile (below)
    still reaches it.
  - It installs the config profile, `<id>.config`, supervised or not: one
    `com.apple.ManagedClient.preferences` payload forcing the keys of `--mdm-daemon-config` in
    the domain `dev.hostkeeper.config`, and, when given, those of `--mdm-stores` in
    `dev.hostkeeper.stores` and of `--mdm-macos-pkg` in `dev.hostkeeper.macos-pkg`. It records
    the SHA-256 of the settings with the command, and counts them delivered once the Mac
    acknowledges it. It sends the profile again, approved Mac or not, once whenever the settings
    it would send now differ from those delivered (a record with no digest counts as different).
    One the Mac answered with an error is sent again an hour after the answer, never sooner.
  - It issues a nonce (`Enrollment::issue_mac_nonce`) only when the device is supervised, no
    nonce issued for the serial is still good (3600 seconds from its issue, and no approval of
    the serial recorded since, which consumed it), and exactly one
    request is a candidate: waiting for an operator, not a duplicate claim, claiming the serial
    and a platform UUID equal to the device's UDID (letter case aside), posted at or after the
    first `TokenUpdate` of the current enrollment. Two candidates, a local reader of the
    managed preferences for instance, get no nonce: the operator decides. The nonce is bound to
    the candidate and pushed in `<id>.nonce` (domain `dev.hostkeeper.enrollment`, keys `nonce`
    and `request_id`); each push NanoHUB queued is audited `mdm_nonce_pushed` by `mdm-gate`,
    with the serial, the request and the push count. A push the Mac answered with an error is
    pushed again, with the same nonce, a minute after the answer; an expired nonce is replaced
    by a new one for the same request.
  - The record says when a nonce was issued and for which request, and is written before the
    nonce is issued; the nonce itself is only in the enrollment store (by its SHA-256) and in
    the gate's memory, and no byte of it is logged or audited. After a restart a failed push is
    not pushed again: a new nonce is issued once the old one expires.

  The daemon posts the nonce back with its request ([docs/operations/enrollment.md](operations/enrollment.md)),
  and the request is approved as above.

- **Its install of the pkg.** With `--mdm-pkg-allowlist` and `--mdm-host` as well, the gate
  installs the hostkeeper pkg on each Mac it acts on (design sections 7.1 and 5.4). The
  allowlist is a TOML file, a regular file of root's that its group and others cannot write,
  read at start (a defect stops it; a new entry takes a restart):

  ```toml
  [[pkg]]
  sha256  = "<the pkg's SHA-256, 64 lower-case hex digits>"
  version = "0.1.66"
  path    = "/var/lib/hostkeeper-mdm-pkg/<sha256>.pkg"   # in the MDM kit's pkg directory
  ```

  The pkg is the release's `hostkeeper-<version>-macos-arm64.pkg` as it is: the release signs
  it with the Developer ID Installer identity before it sums and attests it, so its `sha256` is
  its line of the release's `SHA256SUMS`, and the file goes into the pkg directory unchanged,
  named by that digest ([macos.md](macos.md), "The installer's signature").
  A path is absolute and named `<sha256>.pkg`, a digest is listed once and so are a version's
  release numbers, and a list with no entry is refused. The gate sends the newest entry: an
  `InstallEnterpriseApplication` with an inline manifest, one `software-package` asset whose
  `url` is `https://<--mdm-host>/pkg/<sha256>.pkg` and whose `sha256` is the digest the Mac
  checks, and the metadata `bundle-identifier` `dev.hostkeeper.hostkeeper`, `bundle-version`,
  `kind` `software` and `title`. It hashes the file before every send and sends nothing when the
  bytes are not the listed digest (or the file is missing), keeping the reason as the device's
  last error.
  - It sends the pkg once the Mac has acknowledged the config profile, supervised or not, while
    no request is a candidate (above) and no request claiming the serial was approved in this
    enrollment.
  - The Mac answers only that it accepted the command, never the install's end. So the gate
    sends it again an hour after the Mac's last answer (`Acknowledged` or an error) while those
    conditions hold. It never sends it again while the last send is unanswered (a Mac that is
    off), and sends at the next look one NanoHUB did not queue.
  - Each send NanoHUB queued is audited `mdm_install_sent` by `mdm-gate`, with the serial, the
    digest, the version and the send's count. `hkctl mdm list-devices` shows the count
    (`install_sends`), the last send (`install_last_sent_unix`) and its digest
    (`install_sha256`). A new enrollment starts the count again.

  Without the two flags the gate sends no pkg, and the pkg is installed by hand.

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
`Enrollment.Approve` (the coordinator's gRPC serving). The MDM policy gate's install of the pkg. Neither transport carries a machine id in a heartbeat yet, so the duplicate mark (two live
daemons on one host id) is only set when a caller of `Coordinator::heartbeat_from` passes one.
