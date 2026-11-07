# hostkeeper ctl

<!-- Generated from the protos: `cargo test -p hostkeeper-ctl --test docs` fails when this file differs, and writes the current text beside its test binary. Do not edit by hand. -->

`hostkeeper ctl` calls one RPC of the coordinator per command. Every command, flag and help text below is generated from the protobuf descriptors of `hostkeeper-proto`; the services of a host's admin socket (packages `hostkeeper.local.v<n>`) are not here: `hostkeeper admin` calls them on the host, as [docs/admin.md](admin.md) describes.

## Rules

Every command is one RPC: `hostkeeper ctl <service> <rpc>`, both in kebab case as the protos name them (`Releases.SetDesired` is `releases set-desired`).

A request field is a flag named after it (`node_id` is `--node-id`): a scalar takes one value; a bool takes `true` or `false`, so that an absent flag and `false` differ; an enum takes a value's name without its prefix, in lower case, never `unspecified`; a repeated field repeats its flag; a `map<string, string>` repeats `--<field> key=value`.

A message, a oneof, `bytes` and any other map come only through `--request <file|->`: proto3 canonical JSON, `-` for stdin. A field named both by a flag and by the request file is a usage error.

A secret field (`(hostkeeper.secret)`) is never a flag: it is read with `--<field>-file <path>` (one trailing newline dropped) or `--<field>-stdin`, and never printed. At most one input reads stdin.

The response goes to stdout; diagnostics go to stderr. A refusal prints `refused: <token>`.

## Global flags

| flag | value | default | meaning |
|---|---|---|---|
| `--endpoint` | URL |  | The coordinator's listener, as https://<host>[:<port>]: the private listener for the operator services, the heartbeat listener for `coordinator`. Needed for a call. |
| `--ca-file` | PATH |  | PEM certificates of the CA the server's certificate must chain to; no other root is trusted. Needed for a call. |
| `--token-file` | PATH |  | Read the bearer token (an operator token, or CI's OIDC token) from this file; one trailing newline is dropped. The token is never a flag value nor an environment variable, and is never printed. |
| `--token-stdin` |  |  | Read the bearer token from stdin instead. |
| `--client-cert-file` | PATH |  | A PEM client certificate, for the heartbeat listener's mutual TLS; with --client-key-file. |
| `--client-key-file` | PATH |  | The PEM private key of --client-cert-file. |
| `--transport` | grpc\|gateway | grpc | gRPC, or the HTTP/JSON gateway of the private listener (POST /<package>.<Service>/<Method>). |
| `--output` | json\|text | text | json: the response as proto3 canonical JSON. text: `field: value` lines, nested values as indented JSON. |
| `--timeout-secs` | SECONDS | 30 | How long to wait to connect, and for the server; at least 1. |

## Exit codes

| code | meaning | gRPC codes |
|---|---|---|
| 0 | the call succeeded | OK |
| 1 | anything else: the transport, INTERNAL, UNAVAILABLE, a malformed reply | CANCELLED, UNKNOWN, DEADLINE_EXCEEDED, NOT_FOUND, ALREADY_EXISTS, RESOURCE_EXHAUSTED, ABORTED, OUT_OF_RANGE, UNIMPLEMENTED, INTERNAL, UNAVAILABLE, DATA_LOSS |
| 2 | a usage error, or the server refused the request: INVALID_ARGUMENT or FAILED_PRECONDITION | INVALID_ARGUMENT, FAILED_PRECONDITION |
| 3 | the server refused the caller: UNAUTHENTICATED or PERMISSION_DENIED | PERMISSION_DENIED, UNAUTHENTICATED |

## Commands

### `alerts`

The service `Alerts` of the package `hostkeeper.operator.v1`. The conditions that need a person.

#### `hostkeeper ctl alerts list`

`/hostkeeper.operator.v1.Alerts/List`

Every condition that needs a person now, by kind, then service, host and version. Changes
nothing.

### `audit`

The service `Audit` of the package `hostkeeper.operator.v1`. The coordinator's audit log.

#### `hostkeeper ctl audit list`

`/hostkeeper.operator.v1.Audit/List`

The entries after `since_seq` that match every filter set, newest last. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--since-seq` | UINT64 | Only entries with a greater `seq`; 0 for the oldest the log keeps. |
| `--limit` | UINT32 | The most entries to return; capped at 256, and 0 means the cap. |
| `--actor` | STRING | Only entries by this actor, by the token of `Actor` (`operator`, `ci`, `coordinator`). |
| `--action` | STRING | Only entries of this action (`AuditEntry.action`). |
| `--service` | STRING |  |
| `--node-id` | STRING |  |
| `--by-name` | STRING | Only entries made under this name (`AuditEntry.by_name`), as a token is named: 1 to 32 bytes of `[a-z0-9-]`. A name no entry has lists none. |
| `--newest` | true\|false | The newest `limit` matching entries after `since_seq`, oldest first, in place of the oldest; `next_seq` goes on from the newest of them. |

### `coordinator`

The service `Coordinator` of the package `hostkeeper.agent.v1`.

#### `hostkeeper ctl coordinator heartbeat`

`/hostkeeper.agent.v1.Coordinator/Heartbeat`

What the host runs and how it is, and the coordinator's answer: at most one order.

| flag | value | meaning |
|---|---|---|
| `--platform` | VALUE | The platform the host's releases are built for. |
| `--features` | STRING, repeated | What this agent can use of the coordinator's side, by feature name (`deferred-v1`, `halt-v1`, `forced-v1`, which announces that the daemon takes the `Force` verb, `install-v1`, which announces that it takes the `Install` and `Uninstall` verbs, `abort-v1`, which announces that it takes the `Abort` verb, `clear-record-v1`, which announces that it takes the `ClearRecord` verb, and `verdict-acks-v1`, which asks for `HeartbeatResponse.acks`): at most 16, each 1 to 32 bytes of `[a-z0-9-]`, none twice. An agent that lists every service it runs in `services` announces `heartbeat-v2`; without it the coordinator counts the host as unmanaged for every service but the legacy one, rather than as missing them. |
| `--sent-unix` | UINT64 | The host's clock when it made the heartbeat, in seconds since the epoch: what the coordinator holds against its own clock to count the heartbeats of a host whose clock is off. Absent from an agent that predates it. |
| `--cert-not-after-unix` | UINT64 | The last second the host's current certificate is valid, in seconds since the epoch. Absent from an agent that predates it and from a host that holds no certificate. |
| `--last-renew-error` | STRING | Why the host's last step of enrollment or renewal failed, as the host words it, at most 256 bytes (a reader cuts a longer one at a character boundary); absent once a step succeeds, and from an agent that predates it. |

Through `--request` only: `agent` (hostkeeper.agent.v1.AgentInfo), `services` (repeated hostkeeper.agent.v1.ServiceBeat).

#### `hostkeeper ctl coordinator enroll`

`/hostkeeper.agent.v1.Coordinator/Enroll`

Ask for a certificate: a CSR, the host's claims and its image. No client certificate. The
request is pending until an operator approves it; nothing is installed meanwhile.

| flag | value | meaning |
|---|---|---|
| `--image` | STRING | The boot image or installer the daemon came from, as the image names itself. |
| `--renewal-of` | STRING | The host id of a host whose certificate expired while it was offline, posting again with the same key; empty for a first enrollment. |
| `--mac-nonce-file` | PATH | secret `mac_nonce`, from a file. The one-time nonce MDM gave a Mac (one hour, one use); empty on any other host. |
| `--mac-nonce-stdin` | | secret `mac_nonce`, from stdin |

Through `--request` only: `csr_der` (bytes), `claims` (hostkeeper.agent.v1.Claims).

#### `hostkeeper ctl coordinator poll`

`/hostkeeper.agent.v1.Coordinator/Poll`

Ask whether a request was approved, and take its certificate, by the challenge the header
describes. No client certificate.

| flag | value | meaning |
|---|---|---|
| `--request-id` | STRING | The request's id, as `EnrollResponse` gave it. |

Through `--request` only: `nonce` (bytes), `signature` (bytes).

#### `hostkeeper ctl coordinator renew`

`/hostkeeper.agent.v1.Coordinator/Renew`

Get a new certificate for the key the host holds, over mutual TLS with its current one.

Through `--request` only: `csr_der` (bytes).

### `enrollment`

The service `Enrollment` of the package `hostkeeper.operator.v1`. The hosts waiting for an operator to approve their enrollment.

#### `hostkeeper ctl enrollment list`

`/hostkeeper.operator.v1.Enrollment/List`

The pending requests, oldest first, a page at a time; with `unenrolled`, the requests that
expired instead.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most requests to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |
| `--unenrolled` | true\|false | List the requests that expired, whose hosts are unenrolled, in place of the pending ones: each waited for an operator longer than the coordinator's `--enrollment-request-ttl` and is no longer approved. The order and the page tokens are the same. A coordinator before expiry ignores the field and lists the pending requests, each with `state` UNSPECIFIED. |

#### `hostkeeper ctl enrollment approve`

`/hostkeeper.operator.v1.Enrollment/Approve`

Approve a pending request: the host gets its certificate at its next `Poll`, with the host
id, role and services the approval names.

| flag | value | meaning |
|---|---|---|
| `--request-id` | STRING |  |
| `--key-sha256` | STRING | The SHA-256 of the pending request's key, as `PendingEnrollment.key_sha256` lists it. |
| `--host-id` | STRING | The host id the certificate names. For a request (not a renewal) that matches exactly one inventory record, empty means that record's host id, and another host id needs `replace`; otherwise it is required. |
| `--role` | STRING |  |
| `--services` | STRING, repeated | Services to assign to the host; may be empty. |
| `--replace` | true\|false | Take the host id from the host that holds it, revoking that host's certificate; also needed to approve a `duplicate_claim` request or a denied host id. It never approves a contested request (`record_contested`). |
| `--reason` | STRING |  |

#### `hostkeeper ctl enrollment reject`

`/hostkeeper.operator.v1.Enrollment/Reject`

Reject a pending request: it leaves the queue, and its key is refused from then on (its
`Poll` answers `DENIED`, and posting it again changes nothing). A request already rejected
is answered as rejected and audited once.

| flag | value | meaning |
|---|---|---|
| `--request-id` | STRING | The request to reject, as `PendingEnrollment.request_id` lists it. |
| `--reason` | STRING | Why, for the audit log: 1 to 256 bytes with no control character, or empty for none. |

#### `hostkeeper ctl enrollment add-record`

`/hostkeeper.operator.v1.Enrollment/AddRecord`

Record a server the operator built, before it first boots: its host id, optional role, data
MACs, BMC MAC, serial, notes and optional platform. A record for the host id, or a MAC or
serial another record holds, is refused; remove the record first.

A record that names a role is the approval of the host's enrollment: its window opens now
and stays open until `approve_until_unix`, for `approve_for_secs`, or for the coordinator's
`--record-approve-for` (24 hours unless set), and never longer than its
`--record-approve-max` (7 days unless set). While the window is open and the record unused,
a pending request (not a renewal) that matches it exactly and no other record, that no
other pending request contests, and that is not a `duplicate_claim`, is approved by the
coordinator itself, as the record's host id and role, bound to the SHA-256 of the request's
key: no `Approve` is needed. The approval uses the record (`InventoryRecord.state` USED),
which approves nothing again. Exactly: by one of its data MACs among the claimed addresses
and, when the record holds a BMC MAC, that BMC MAC claimed; a Mac by its serial. Every
other request waits for `Approve` as before. A record with no role has no window.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING |  |
| `--role` | STRING | Empty for none. |
| `--data-macs` | STRING, repeated | At least one, or a `serial`. |
| `--bmc-mac` | STRING | Empty for none. |
| `--serial` | STRING | Empty for none. |
| `--notes` | STRING | Empty for none. |
| `--approve-until-unix` | UINT64 | The end of the record's window, unix seconds: after now, and within the coordinator's `--record-approve-max` of it. 0 for none given. |
| `--approve-for-secs` | UINT64 | The length of the record's window from now, in seconds: at most the coordinator's `--record-approve-max`. 0 for none given. With neither, the window is the coordinator's `--record-approve-for`; with both, the call is refused `invalid_window`. A record that names no role takes neither, and has no window. |
| `--platform` | VALUE | The platform to install the server as by network boot: `PLATFORM_LINUX_X86_64` or `PLATFORM_LINUX_ARM64` (`linux_x86_64` or `linux_arm64` as a flag of `hostkeeper ctl`); `PLATFORM_UNSPECIFIED` (the flag left out) for none. Any other value is refused `invalid_request`. |

#### `hostkeeper ctl enrollment list-records`

`/hostkeeper.operator.v1.Enrollment/ListRecords`

The records, by host id, a page at a time; all of them, or only those a host is enrolled
against, or only the others.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most records to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |
| `--enrolled` | true\|false | Unset for every record; true for only the records a host is enrolled against (`InventoryRecord.enrolled`), false for only the others. |

#### `hostkeeper ctl enrollment remove-record`

`/hostkeeper.operator.v1.Enrollment/RemoveRecord`

Remove the record of a host id. An enrolled host keeps its certificate.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING |  |
| `--reason` | STRING |  |

#### `hostkeeper ctl enrollment open-record`

`/hostkeeper.operator.v1.Enrollment/OpenRecord`

Open the window of a record again, one whose window ended or that a release before windows
added, or move the end of one still open: as `AddRecord` opens it, from now. A record an
approval used is refused `record_used`: remove it and add it again. A pending request that
matches the record is approved then, as at `AddRecord`.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING |  |
| `--approve-until-unix` | UINT64 | As `AddRecordRequest.approve_until_unix`. |
| `--approve-for-secs` | UINT64 | As `AddRecordRequest.approve_for_secs`. |
| `--reason` | STRING | Why, for the audit log: 1 to 256 bytes with no control character, or empty for none. |

### `hosts`

The service `Hosts` of the package `hostkeeper.operator.v1`. The hosts the coordinator has heard from, and what an operator decides about them.

#### `hostkeeper ctl hosts list`

`/hostkeeper.operator.v1.Hosts/List`

Every host heard from since the coordinator started, by node id, a page at a time. Changes
nothing.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most hosts to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |

#### `hostkeeper ctl hosts get`

`/hostkeeper.operator.v1.Hosts/Get`

One host heard from since the coordinator started; `unknown_host` for any other. Changes
nothing.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |

#### `hostkeeper ctl hosts exclude`

`/hostkeeper.operator.v1.Hosts/Exclude`

Take a host out of a service: its heartbeats do not count for the service and it is not
ordered. A second exclusion of the same host and service replaces the first.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--node-id` | STRING |  |
| `--reason` | STRING |  |
| `--abandon` | true\|false | Also free the update slot the host holds for the service mid-update, for a host that is gone and will not report its update final. False (the default) keeps the slot. |

#### `hostkeeper ctl hosts include`

`/hostkeeper.operator.v1.Hosts/Include`

Put an excluded host back. Including a host that is not excluded changes nothing and is not
an error.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--node-id` | STRING |  |

#### `hostkeeper ctl hosts set-labels`

`/hostkeeper.operator.v1.Hosts/SetLabels`

Replace a host's labels, which service selectors match. An empty set removes them.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--labels` | KEY=VALUE, repeated | At most 8; each key and value 1 to 32 bytes of `[a-z0-9-]`. |

#### `hostkeeper ctl hosts abort`

`/hostkeeper.operator.v1.Hosts/Abort`

Queue an `abort` order of a service for the host's next heartbeat; it takes no slot. An
abort of a host that runs nothing of the service is accepted and changes nothing.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--service` | STRING |  |
| `--reason` | STRING |  |

#### `hostkeeper ctl hosts retire`

`/hostkeeper.operator.v1.Hosts/Retire`

Retire an enrolled host: a deny row is written, so its heartbeats and its `Renew` are
refused `denied` at once, and every entry of the host's own in the services' assignments is
dropped: an assignment or exclusion of the host, its minimum version, and an uninstall it was
ordered (which is cancelled); a role's entries stay. An update in flight on it keeps its
slot until the host reports the update's record final; a host with none frees it now. The
host is sent no `uninstall`. Retiring a host already retired is answered as retired and
audited once.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING | The host id the host was enrolled as. |
| `--reason` | STRING | Why, for the audit log: 1 to 256 bytes with no control character, or empty for none. |

#### `hostkeeper ctl hosts clear-record`

`/hostkeeper.operator.v1.Hosts/ClearRecord`

Order the host to set aside its step record of the service when the record does not read
(the `clear_record` verb of `hostkeeper.agent.v1`), at the host's next heartbeat; it takes no
slot and is sent once. Only for a host whose daemon announced `clear-record-v1` in its last
heartbeat; to any other it is refused `daemon_too_old` and nothing is ordered. The daemon's
verdict comes back in a heartbeat like any order's. Audited `record_cleared`.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING | The host, heard from since the coordinator started. |
| `--service` | STRING | The service whose step record the host sets aside; one the coordinator manages. |

### `mdm`

The service `Mdm` of the package `hostkeeper.operator.v1`. The devices of the MDM policy gate.

#### `hostkeeper ctl mdm list-devices`

`/hostkeeper.operator.v1.Mdm/ListDevices`

The devices the gate has a record of, by enrollment id, from the records alone: no device
is asked. A coordinator that runs no gate answers none, with `gate` false. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | At most this many devices; 0 means the cap. |
| `--page-token` | STRING | Empty for the first page; else the `next_page_token` of the page before. |

### `releases`

The service `Releases` of the package `hostkeeper.operator.v1`. The desired version of each service, how far the hosts are, and the operator's overrides.

#### `hostkeeper ctl releases set-desired`

`/hostkeeper.operator.v1.Releases/SetDesired`

Set a service's desired state. The coordinator moves its hosts there by the service's update
plan, stage by stage (`Rollouts`), a role's `max_updating` at a time. Takes
CI's OIDC token (the services its allow-list entry names) or the operator token (any service
the coordinator manages, audited as `set_desired` under the token's name).

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--if-generation` | UINT64 | Write only if the service's generation is this one now; otherwise refused with `generation_conflict`, and nothing changes. Absent: write whatever the generation is. |
| `--write-id` | STRING | The writer's own id for this write, kept with it and returned by `Converge`, so a writer that lost the answer can tell whether its write landed: 1 to 128 visible ASCII characters. Absent: none. |
| `--package-digest` | STRING | The digest of the package CI registers for `desired.version` (64 lower-case hex characters), the one package every host is ordered whatever its platform: for a fleet of one platform. Refused with `release_index` (`index_invalid`). |
| `--release-index` | STRING | The release index of `desired.version` (`index.json`, design section 5.5), byte for byte: per platform, the package file, its size and its SHA-256. The coordinator checks that its SHA-256 is `release_index_digest` (else `index_digest_mismatch`) and that it is the index of `service` at `desired.version` (else `index_invalid`), keeps the digest of each platform's package, and orders each host the package of its own platform. Comes with `release_index_digest`. |
| `--release-index-digest` | STRING | The SHA-256 of `release_index` (64 lower-case hex characters), as CI's release job wrote it down when it published the index: the release this desired state names. |

Through `--request` only: `desired` (hostkeeper.operator.v1.DesiredState).

#### `hostkeeper ctl releases converge`

`/hostkeeper.operator.v1.Releases/Converge`

How far a service's hosts are from its desired version. Changes nothing. Takes CI's OIDC
token or the operator token.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |

#### `hostkeeper ctl releases force`

`/hostkeeper.operator.v1.Releases/Force`

Switch a service to a version on the targeted hosts at once, with no hook (see
`ForceRequest`).

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--version` | STRING |  |
| `--reason` | STRING |  |
| `--include-held` | true\|false | Order the held pairs among the targets too, for this call only. |

Through `--request` only: `fleet` (hostkeeper.operator.v1.FleetTarget, in a oneof), `role` (string, in a oneof), `hosts` (hostkeeper.operator.v1.HostList, in a oneof).

#### `hostkeeper ctl releases unmark-bad`

`/hostkeeper.operator.v1.Releases/UnmarkBad`

Lift a bad mark from a version, so that it may be ordered and CI may set it again.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--version` | STRING |  |
| `--reason` | STRING |  |

#### `hostkeeper ctl releases clear-halt`

`/hostkeeper.operator.v1.Releases/ClearHalt`

Lift a service's standing halt.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

#### `hostkeeper ctl releases status`

`/hostkeeper.operator.v1.Releases/Status`

Everything the coordinator holds about each service. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | One service; empty for every service the coordinator manages. |

#### `hostkeeper ctl releases mark-bad`

`/hostkeeper.operator.v1.Releases/MarkBad`

Mark a version bad by hand: it is never ordered again until it is unmarked. A held pair is
not reached.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--version` | STRING |  |
| `--reason` | STRING |  |

#### `hostkeeper ctl releases halt`

`/hostkeeper.operator.v1.Releases/Halt`

Halt a service by hand, as a host's halt report does: no host of it is ordered until the halt
is cleared.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

### `rollouts`

The service `Rollouts` of the package `hostkeeper.operator.v1`. The update plans of the services: an operator moves a plan on, pauses it and resumes it. Each
call is an audit entry under the operator token's name.

#### `hostkeeper ctl rollouts advance`

`/hostkeeper.operator.v1.Rollouts/Advance`

Enter the next stage of a plan that waits for an operator (`advance = "manual"`): its done
stage, or none yet for a manual first stage.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |

#### `hostkeeper ctl rollouts pause`

`/hostkeeper.operator.v1.Rollouts/Pause`

Pause a plan: no host that has not started is ordered; the updates already ordered or
running finish and bake. The pause stands across a new desired version, until `Resume`.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |

#### `hostkeeper ctl rollouts resume`

`/hostkeeper.operator.v1.Rollouts/Resume`

Resume a paused plan from where it stands.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |

### `services`

The service `Services` of the package `hostkeeper.operator.v1`. The services the coordinator manages, which hosts run them, and the operator's holds.

#### `hostkeeper ctl services list`

`/hostkeeper.operator.v1.Services/List`

Every service, with its desired version, the roles and hosts it is assigned to, the
operator's assignments, its minimum versions, the hosts ordered to uninstall it and its holds.

#### `hostkeeper ctl services assign`

`/hostkeeper.operator.v1.Services/Assign`

Assign a service to a host or a role: a host that does not run it is ordered to install it
at its next heartbeat, unless a hold stands. Assigning a service already assigned changes
nothing.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--service` | STRING |  |
| `--role` | STRING | A role to assign the service to instead of one host: every host of the role, those that join it later included. Exactly one of `node_id` and `role` is set. |

#### `hostkeeper ctl services unassign`

`/hostkeeper.operator.v1.Services/Unassign`

Take a service off a host or a role. With `uninstall`, each host that no longer has any
assignment of it is ordered to uninstall it; without, the coordinator stops managing it
there and the host keeps running it.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--service` | STRING |  |
| `--uninstall` | true\|false | Order the host to uninstall the service; false (the default) leaves it running there, unmanaged. |
| `--role` | STRING | A role to take the service off instead of one host. Exactly one of `node_id` and `role` is set. |

#### `hostkeeper ctl services set-min-version`

`/hostkeeper.operator.v1.Services/SetMinVersion`

Set the version below which the rollout gate orders the service to no host of a role, or to
one host: no update, install or automatic rollback. A host's own minimum stands before its
role's, and neither goes below the rollout file's.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |

Through `--request` only: `min_version` (hostkeeper.operator.v1.MinVersion).

#### `hostkeeper ctl services hold`

`/hostkeeper.operator.v1.Services/Hold`

Hold a service on a host, a role or the whole fleet where it is: the coordinator orders the
held pairs nothing (no update, install, rollback, uninstall or force) until the hold is
released. A hold carries no version, takes no slot, survives a coordinator restart, covers
hosts that later join a held role, and the host never learns of it. An update already
running finishes. Holding a pair already held under the same scope replaces the reason.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

Through `--request` only: `scope` (hostkeeper.operator.v1.HoldScope).

#### `hostkeeper ctl services unhold`

`/hostkeeper.operator.v1.Services/Unhold`

Release a hold: the pairs it held are ordered to the then-desired version at their next
heartbeat.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

Through `--request` only: `scope` (hostkeeper.operator.v1.HoldScope).
