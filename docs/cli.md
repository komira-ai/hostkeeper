# hkctl

<!-- Generated from the protos: `cargo test -p hostkeeper-hkctl --test docs` fails when this file differs, and writes the current text beside its test binary. Do not edit by hand. -->

`hkctl` is the command line of hostkeeper's coordinator: one bash script, generated from the protobuf services of `hostkeeper-proto`, that calls one method of the coordinator's JSON gateway ([gateway.md](gateway.md)) per command. It is released as the asset `hkctl-<version>`, attested like every other asset ([deploy/verify-a-release.md](deploy/verify-a-release.md)); `hkctl --version` prints `hkctl <version>+<first 12 hex digits of the commit>`. Every command, flag and help text below is generated from the descriptors; the host API (`hostkeeper.agent.v<n>`), whose caller is the daemon, is not here.

## Rules

Every command is one method: `hkctl <service> <method>`, both in kebab case as the protos name them (`Releases.SetDesired` is `releases set-desired`), called as one POST of JSON to the coordinator's gateway with curl. hkctl needs bash 3.2 or later and curl 7.55 or later, and no jq.

A request field is a flag named after it (`node_id` is `--node-id`): a scalar takes one value; a bool takes `true` or `false`, so that an absent flag and `false` differ; an enum takes a value's name without its prefix, in lower case, never `unspecified`; a repeated field repeats its flag; a `map<string, string>` repeats `--<field> key=value`. A flag takes its value as `--flag value` or `--flag=value`; a value that starts with `--` only as `--flag=value`.

A message, a oneof, `bytes` and any other map come only through `--request <file|->`: a JSON object, `-` for stdin, which the flags add to. A field named both by a flag and by the request file is a usage error.

A secret field (`(hostkeeper.secret)`) is never a flag: it is read with `--<field>-file <path>` (one trailing newline dropped) or `--<field>-stdin`, and never printed. At most one input reads stdin. No token or secret is ever written to a file, put in an argument list or in a child's environment: curl reads each through a pipe.

The answer, the gateway's JSON, goes to stdout or to `--response-file`; diagnostics go to stderr. A refusal prints `refused: <reason>` when its reason is a token of `[a-z0-9_]{1,64}`. A call refused `invalid_request` makes one more call, with no token, to the coordinator's `GET /v1/version?at_least=<this hkctl's release>`: when that says the coordinator is older (`409`, or `404` from a coordinator before the route), hkctl says so on stderr, with the coordinator's version when it gave one, and the reason it prints is `coordinator_older`; any other answer, or none, leaves `invalid_request`.

## Global flags

| flag | value | default | meaning |
|---|---|---|---|
| `--endpoint` | URL |  | The coordinator's private listener, as https://<host>[:<port>]. Needed for a call. |
| `--ca-file` | PATH |  | PEM certificates of the CA the coordinator's certificate must chain to, given to curl as `--cacert` in place of its default bundle (a curl built with a default CA directory also reads that directory). Needed for a call. |
| `--token-file` | PATH |  | Read the bearer token (an operator token) from this file, which only its owner may read; one trailing newline is dropped. The token is never an argument, an environment variable or a line of output. |
| `--token-stdin` |  |  | Read the bearer token from stdin instead. |
| `--github-oidc-audience` | AUDIENCE |  | In a GitHub Actions job with id-token: write: ask the runner for a fresh ID token for this audience and call with it; `::add-mask::<token>` is printed on stdout first. |
| `--response-file` | PATH |  | Write the answer to this file instead of stdout. |
| `--timeout-secs` | SECONDS | 30 | How long to wait to connect, and for the answer, 1 to 999999; a method that holds the call (a `wait_secs` field) adds its hold. |
| `--help` |  |  | Print the help of hkctl, a service or a method. |
| `--version` |  |  | Print `hkctl <version>+<commit>`; only before the service, so a method's own `--version` is its field. |

## Exit codes

| code | meaning | gateway codes |
|---|---|---|
| 0 | the call succeeded |  |
| 1 | anything else: no answer (curl failed), an answer that is not the gateway's, or any other code | CANCELLED, UNKNOWN, DEADLINE_EXCEEDED, NOT_FOUND, ALREADY_EXISTS, RESOURCE_EXHAUSTED, ABORTED, OUT_OF_RANGE, UNIMPLEMENTED, INTERNAL, UNAVAILABLE, DATA_LOSS |
| 2 | a usage error, or the coordinator refused the request: INVALID_ARGUMENT or FAILED_PRECONDITION | INVALID_ARGUMENT, FAILED_PRECONDITION |
| 3 | the coordinator refused the caller: UNAUTHENTICATED or PERMISSION_DENIED | PERMISSION_DENIED, UNAUTHENTICATED |

## Commands

### `alerts`

The service `Alerts` of the package `hostkeeper.operator.v1`. The conditions that need a person.

#### `hkctl alerts list`

`/hostkeeper.operator.v1.Alerts/List`

Every condition that needs a person now, by kind, then service, host and version. Changes
nothing.

### `api-tokens`

The service `ApiTokens` of the package `hostkeeper.operator.v1`. The tokens a script calls with. A token acts as the operator it is named for and cannot sign in. Each mutating call is audited under the caller's name, with the token as the entry's subject: `api_token_create` and `api_token_revoke`.

#### `hkctl api-tokens list`

`/hostkeeper.operator.v1.ApiTokens/List`

Every token, by name: the coordinator's tokens file's, which are read-only, and those made
here. Changes nothing. No token and no digest is ever in the answer.

#### `hkctl api-tokens create`

`/hostkeeper.operator.v1.ApiTokens/Create`

Makes a token of 32 random bytes, `hkt_` and 43 base64url characters, which the answer carries
once: the coordinator keeps its SHA-256 only.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The new token's name. |

#### `hkctl api-tokens revoke`

`/hostkeeper.operator.v1.ApiTokens/Revoke`

Removes a token made here: it is refused from then on. A token of the tokens file is removed
by editing the file.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The token's name. |

### `audit`

The service `Audit` of the package `hostkeeper.operator.v1`. The coordinator's audit log.

#### `hkctl audit list`

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
| `--host-group` | STRING | Only entries about this host group (`AuditEntry.host_group`); a coordinator that does not hold host groups lists none. |

### `enrollment`

The service `Enrollment` of the package `hostkeeper.operator.v1`. The hosts waiting for an operator to approve their enrollment.

#### `hkctl enrollment list`

`/hostkeeper.operator.v1.Enrollment/List`

The pending requests, oldest first, a page at a time; with `unenrolled`, the requests that
expired instead.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most requests to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |
| `--unenrolled` | true\|false | List the requests that expired, whose hosts are unenrolled, in place of the pending ones: each waited for an operator longer than the coordinator's `--enrollment-request-ttl` and is no longer approved. The order and the page tokens are the same. A coordinator before expiry ignores the field and lists the pending requests, each with `state` UNSPECIFIED. |

#### `hkctl enrollment approve`

`/hostkeeper.operator.v1.Enrollment/Approve`

Approve a pending request: the host gets its certificate at its next `Poll`, with the host
id and host group the approval names. The group is optional: a host approved with none is
enrolled in no specific group: it is in the built-in group `all` alone, and runs the packages
`all` lists until `HostGroups.Move` puts it in one. The reason is optional too.

| flag | value | meaning |
|---|---|---|
| `--request-id` | STRING |  |
| `--key-sha256` | STRING | The SHA-256 of the pending request's key, as `PendingEnrollment.key_sha256` lists it. |
| `--host-id` | STRING | The host id the certificate names. For a request (not a renewal) that matches exactly one inventory record, empty means that record's host id, and another host id needs `replace`; otherwise it is required. |
| `--role` | STRING | The host's role. From a coordinator that holds host groups, an alias of `host_group` kept for clients that predate it: read as the group's name when `host_group` is empty, and when both are empty the host is enrolled in no group. A coordinator that holds no host groups requires it. |
| `--services` | STRING | Services to assign to the host; may be empty. Deprecated: a coordinator that holds host groups refuses any (`services_retired`). Repeat the flag for more. |
| `--replace` | true\|false | Take the host id from the host that holds it, revoking that host's certificate; also needed to approve a `duplicate_claim` request or a denied host id. It never approves a contested request (`record_contested`). |
| `--reason` | STRING | Why, for the audit log: 1 to 256 bytes with no control character, or empty for none. |
| `--host-group` | STRING | The host group the host is in, which must exist (`unknown_host_group`; a coordinator that does not hold host groups refuses any); empty for none (with `role` empty too, from a coordinator that holds host groups): the host is enrolled in no specific group (in `all` alone) until `HostGroups.Move`. `all`, which every host is in, is no group a request names (`unknown_host_group`). With a renewal or `replace`, the group of that host (`host_group_differs`); empty keeps it. |

#### `hkctl enrollment reject`

`/hostkeeper.operator.v1.Enrollment/Reject`

Reject a pending request: it leaves the queue, and its key is refused from then on (its
`Poll` answers `DENIED`, and posting it again changes nothing). A request already rejected
is answered as rejected and audited once.

| flag | value | meaning |
|---|---|---|
| `--request-id` | STRING | The request to reject, as `PendingEnrollment.request_id` lists it. |
| `--reason` | STRING | Why, for the audit log: 1 to 256 bytes with no control character, or empty for none. |

#### `hkctl enrollment add-record`

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
| `--role` | STRING | Empty for none; from a coordinator that holds host groups, read as the group's name when `host_group` is empty. |
| `--data-macs` | STRING | At least one, or a `serial`. Repeat the flag for more. |
| `--bmc-mac` | STRING | Empty for none. |
| `--serial` | STRING | Empty for none. |
| `--notes` | STRING | Empty for none. |
| `--approve-until-unix` | UINT64 | The end of the record's window, unix seconds: after now, and within the coordinator's `--record-approve-max` of it. 0 for none given. |
| `--approve-for-secs` | UINT64 | The length of the record's window from now, in seconds: at most the coordinator's `--record-approve-max`. 0 for none given. With neither, the window is the coordinator's `--record-approve-for`; with both, the call is refused `invalid_window`. A record that names no role takes neither, and has no window. |
| `--platform` | VALUE | The platform to install the server as by network boot: `PLATFORM_LINUX_X86_64` or `PLATFORM_LINUX_ARM64` (`linux_x86_64` or `linux_arm64` as a command-line flag); `PLATFORM_UNSPECIFIED` (the flag left out) for none. Any other value is refused `invalid_request`. One of: linux_x86_64, linux_arm64, darwin_arm64. |
| `--host-group` | STRING | The host group the host is to be in, which must exist (`unknown_host_group`; a coordinator that does not hold host groups refuses any); empty for none. A record that names a group has a window as one that names a role does. |

#### `hkctl enrollment list-records`

`/hostkeeper.operator.v1.Enrollment/ListRecords`

The records, by host id, a page at a time; all of them, or only those a host is enrolled
against, or only the others.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most records to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |
| `--enrolled` | true\|false | Unset for every record; true for only the records a host is enrolled against (`InventoryRecord.enrolled`), false for only the others. |

#### `hkctl enrollment remove-record`

`/hostkeeper.operator.v1.Enrollment/RemoveRecord`

Remove the record of a host id. An enrolled host keeps its certificate.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING |  |
| `--reason` | STRING |  |

#### `hkctl enrollment open-record`

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

### `host-groups`

The service `HostGroups` of the package `hostkeeper.operator.v1`. The host groups: what each group's hosts run, and by which rollout policy. Every enrolled host is in the built-in group `all`, which always exists, and may be in one specific group besides; it runs the packages `all` lists, then those its specific group lists that `all` does not. Each mutating call takes a reason and answers the audit entry it wrote.

#### `hkctl host-groups list`

`/hostkeeper.operator.v1.HostGroups/List`

Every group, by name, a page at a time, each with its packages, policy, generation, host
count and, per package, its desired version. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most groups to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |

#### `hkctl host-groups get`

`/hostkeeper.operator.v1.HostGroups/Get`

One group, with, per package, its desired version, minimum version and convergence, and a
page of its hosts. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--host-group` | STRING | The group. |
| `--limit` | UINT32 | The most hosts to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |

#### `hkctl host-groups put`

`/hostkeeper.operator.v1.HostGroups/Put`

Create a group, or replace one whole. A package taken out of the list is uninstalled from
the group's hosts; one added is installed once the group has a desired version for it. A
package's minimum version in the group is not part of the body: `Put` neither reads nor
resets it (`Services.SetMinVersion` writes it). Audited `host_group_put`.

| flag | value | meaning |
|---|---|---|
| `--if-generation` | UINT64 | The generation of the group this replaces, as `List` or `Get` answered it. Absent to create a group; a replacement without it is refused `generation_mismatch`. |
| `--reason` | STRING | Why; 1 to 256 bytes. |

Through `--request` only: `host_group` (hostkeeper.operator.v1.HostGroup).

#### `hkctl host-groups remove`

`/hostkeeper.operator.v1.HostGroups/Remove`

Remove a group no host is in or moving into, and that no inventory record, pending approval
or Mac allowlist entry names, with its desired versions and plan positions. Audited
`host_group_remove`.

| flag | value | meaning |
|---|---|---|
| `--host-group` | STRING | The group. |
| `--reason` | STRING | Why; 1 to 256 bytes. |

#### `hkctl host-groups move`

`/hostkeeper.operator.v1.HostGroups/Move`

Move an enrolled host into another group, in one call: it is in the new group from this
write, and is moving until its packages match the new group's. An empty `host_group` moves
it out of its specific group into none: it then runs the packages of `all` alone, and what
its old group listed and `all` does not is uninstalled. Audited `host_move`.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING | The host, by the host id it was enrolled as. |
| `--host-group` | STRING | The group it moves into; empty for none (the host runs the packages of `all` alone). |
| `--reason` | STRING | Why; 1 to 256 bytes. |

### `hosts`

The service `Hosts` of the package `hostkeeper.operator.v1`. The hosts the coordinator has heard from or enrolled, and what an operator decides about them.

#### `hkctl hosts list`

`/hostkeeper.operator.v1.Hosts/List`

Every host heard from since the coordinator started and, when the coordinator has an
enrollment, every host it enrolled that is not denied, by node id, a page at a time. A host
enrolled and not heard since the coordinator started is listed as not live, with what its
enrollment kept of what it runs (`Host.running`). Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most hosts to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |

#### `hkctl hosts get`

`/hostkeeper.operator.v1.Hosts/Get`

One host heard from since the coordinator started; `unknown_host` for any other, an enrolled
host not heard since included. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |

#### `hkctl hosts exclude`

`/hostkeeper.operator.v1.Hosts/Exclude`

Take a host out of a service: its heartbeats do not count for the service and it is not
ordered. A second exclusion of the same host and service replaces the first.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--node-id` | STRING |  |
| `--reason` | STRING |  |
| `--abandon` | true\|false | Also free the update slot the host holds for the service mid-update, for a host that is gone and will not report its update final. False (the default) keeps the slot. |

#### `hkctl hosts include`

`/hostkeeper.operator.v1.Hosts/Include`

Put an excluded host back. Including a host that is not excluded changes nothing and is not
an error.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--node-id` | STRING |  |

#### `hkctl hosts set-labels`

`/hostkeeper.operator.v1.Hosts/SetLabels`

Replace a host's labels, which service selectors match and which the host's facts file
carries (its daemon is sent them at its next heartbeat). An empty set removes them.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--labels` | KEY=VALUE | At most 8; each key and value 1 to 32 bytes of `[a-z0-9-]`. Repeat the flag for more. |

#### `hkctl hosts abort`

`/hostkeeper.operator.v1.Hosts/Abort`

Queue an `abort` order of a service for the host's next heartbeat; it takes no slot. An
abort of a host that runs nothing of the service is accepted and changes nothing.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--service` | STRING |  |
| `--reason` | STRING |  |

#### `hkctl hosts retire`

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

#### `hkctl hosts clear-record`

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

#### `hkctl hosts retry`

`/hostkeeper.operator.v1.Hosts/Retry`

Set aside the host's failed report of the service, so it is ordered the desired version again
at its next heartbeat, at the next try (see `RetryRequest`). Audited `retry`.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING | The host, heard from since the coordinator started. |
| `--service` | STRING | The service whose failed report is set aside; one the coordinator manages. |

### `mdm`

The service `Mdm` of the package `hostkeeper.operator.v1`. The devices of the MDM policy gate.

#### `hkctl mdm list-devices`

`/hostkeeper.operator.v1.Mdm/ListDevices`

The devices the gate has a record of, by enrollment id, from the records alone: no device
is asked. A coordinator that runs no gate answers none, with `gate` false. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | At most this many devices; 0 means the cap. |
| `--page-token` | STRING | Empty for the first page; else the `next_page_token` of the page before. |

#### `hkctl mdm put-secret`

`/hostkeeper.operator.v1.Mdm/PutSecret`

Seals `material` as the next version of the secret of `kind` and keeps it; the version it
replaces is kept for the `secret_grace_days` setting, so that a wrong upload is undone by
uploading again. Session-only: a call with a bearer token is refused `PERMISSION_DENIED`
`session_required`, so `hkctl` cannot make it. Audited under the signed-in user's name as
`mdm_push_cert_put`, `mdm_abm_token_put`, `mdm_tls_put`, `mdm_acme_credential_put` or
`mdm_secret_put` by the kind, with the kind, the fingerprint and the expiry, and never the
material; a refusal after the caller is known is audited with its token. Answers what
`ListSecrets` shows of the version made. Refused `secrets_unavailable` when the coordinator
holds no master key.

| flag | value | meaning |
|---|---|---|
| `--kind` | STRING | The kind of secret: `push_key`, `push_cert`, `push_encryption_key`, `push_pending_key`, `abm_key`, `abm_cert`, `abm_token`, `mdm_ca_key`, `acme_account_key`, `dns_credential`, `tls_key` or `tls_cert`; else `invalid_request`. |
| `--not-after-unix` | INT64 | When the value stops being good, when it says so; shown by `ListSecrets` and the status. |

Through `--request` only: `material` (bytes), `with_certificate` (bytes).

#### `hkctl mdm put-setting`

`/hostkeeper.operator.v1.Mdm/PutSetting`

Sets the setting `name` to `value`, replacing what it was. Session-only, as `PutSecret` is.
Audited as `mdm_setting_set` with the name and the value.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The setting, one `ListSettings` names: else `invalid_request`. |
| `--value` | STRING | The value, in the form the setting takes (a number of days, or a falling list of them such as `60,14`): else `invalid_request`. |

#### `hkctl mdm list-secrets`

`/hostkeeper.operator.v1.Mdm/ListSecrets`

Every version of every secret the coordinator holds, by kind and version: what is shown of
each, never its value. Changes nothing.

#### `hkctl mdm list-settings`

`/hostkeeper.operator.v1.Mdm/ListSettings`

Every setting this release knows, with its value and whether that is its default. Changes
nothing.

#### `hkctl mdm create-push-request`

`/hostkeeper.operator.v1.Mdm/CreatePushRequest`

Makes a push certificate request: an RSA 2048 key, a CSR over it with the `email` in its
subject, and a second key and self-signed certificate that mdmcert.download encrypts its
reply to. Both keys are sealed and kept; a request pending before is replaced. With `submit`
the coordinator posts the CSR and the encryption certificate to mdmcert.download, which
**emails** the signed request to `email`: the reply is not in this answer, the operator
uploads the attachment with `PutPushReply`. Without `submit` nothing is sent, and the CSR is
for a vendor certificate or a signer of the operator's own. Session-only; audited as
`mdm_push_csr`. Refused `invalid_request` (the email is not one address),
`secrets_unavailable`, `mdmcert_unavailable` (the service could not be reached or did not
take it; the request stays pending), `audit_unavailable`, `store_failed`.

| flag | value | meaning |
|---|---|---|
| `--email` | STRING | The address the request is made for: it is in the CSR's subject, and mdmcert.download emails its reply to it. One plain address (`local@domain`), up to 254 bytes; else `invalid_request`. |
| `--submit` | true\|false | Whether the coordinator posts the request to mdmcert.download. |

#### `hkctl mdm put-push-reply`

`/hostkeeper.operator.v1.Mdm/PutPushReply`

Decrypts the reply of mdmcert.download (the attachment of its email, as the mail has it, hex
text, or its raw bytes) with the pending request's encryption key and checks that it is the
signed request for the pending CSR; answers it, for the operator to upload to Apple's push
portal, which returns the certificate for `PutPushCertificate`. Stores nothing. Session-only;
audited as `mdm_push_reply_put`. Refused `no_pending_request`, `reply_undecryptable` (not a
message encrypted to the pending request's key), `reply_not_for_request`, `audit_unavailable`.

Through `--request` only: `reply` (bytes).

#### `hkctl mdm put-push-certificate`

`/hostkeeper.operator.v1.Mdm/PutPushCertificate`

Stores a push certificate. Without `key_pem` the certificate must match the pending request's
key (what Apple returned for the request); with it, the pair is stored as it is (the
migration of an existing certificate and key; PKCS#1 `BEGIN RSA PRIVATE KEY` and PKCS#8 are
both read). It must match its key, be unexpired, carry a topic that starts
`com.apple.mgmt.External.`, and, when a certificate is already in use, the same topic.
Session-only; audited as `mdm_push_cert_put` with the topic and the expiry, never the key.
Refused `certificate_invalid`, `key_invalid`, `key_mismatch`, `certificate_expired`,
`topic_changed`, `no_pending_request`, `secrets_unavailable`, `audit_unavailable`,
`store_failed`.

Through `--request` only: `certificate_pem` (bytes), `key_pem` (bytes).

#### `hkctl mdm status`

`/hostkeeper.operator.v1.Mdm/Status`

Whether the MDM setup is complete (`configured`), and when it is not, which conditions are
missing; the name, its enrollment URL, and what is shown of the push certificate and key, the
ABM token and the TLS certificate and key in use, with the expiry and topic read from their
material. Computed from the records at each call and never stored, so it cannot go stale.
Never returns a value of a secret. Changes nothing.

#### `hkctl mdm put-name`

`/hostkeeper.operator.v1.Mdm/PutName`

Sets the MDM name, the host name the Macs enroll at (`https://<name>/enroll`). Session-only,
as `PutSecret` is. Audited as `mdm_name_set` with the name. The first name is always taken
(Macs that enrolled before this setup hold theirs); once a name is set and a Mac has enrolled,
another is refused `name_locked`, and the same name is taken again. A coordinator started with
`--mdm-host` stores that host as the name at start (audited as `mdm_name_set` by `server config`), and
at a later start its `--mdm-host` replaces a different stored name while no Mac has enrolled; once one
has, a different `--mdm-host` is refused at start.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The name: a DNS name of two or more labels of letters, digits and `-`, each 1 to 63 long and not starting or ending with `-`, at most 253 in all, whose last label is not all digits; kept in lower case. Else `invalid_request`. |

#### `hkctl mdm verify`

`/hostkeeper.operator.v1.Mdm/Verify`

Reads the setup back and proves its parts work, as a button of the setup page: the status as
`Status` computes it; whether the Macs' listener runs and which names the certificate it serves
names; with an ABM token held, whether Apple accepts it (`GET /account` of the ABM service,
which answers only an accepted token); and, when `enrollment_id` names an enrolled Mac, one
push to it through the sender the queue uses, which proves the push certificate, its topic and
the environment. The push is a plain push with no command behind it: the Mac that gets it
connects, is idle, and nothing is queued or recorded for it. What APNs, or the connection to
it, answered is one class of `MdmPushResult`, so that a wrong topic, a dead token and a
certificate of the wrong environment are told apart. Session-only; audited as `mdm_verify`
with the enrollment id, before the push is sent. A failed check is in the answer, not a
refusal. Refused `unauthorized`, `forbidden`, `invalid_request` (the enrollment id is not
one), `audit_unavailable`, `store_failed`.

| flag | value | meaning |
|---|---|---|
| `--enrollment-id` | STRING | The enrollment id of the enrolled Mac to send the test push to (as `ListDevices` lists it); empty sends none and checks the rest. |

#### `hkctl mdm put-abm-token`

`/hostkeeper.operator.v1.Mdm/PutAbmToken`

Takes the ABM server token (docs/design/coordinator-mdm.md, 4.5): either the JSON of its four
OAuth parts and expiry, which is what a takeover of NanoDEP's token uploads, or the `.p7m` file
ABM hands over, opened with the `abm_key` the coordinator holds. The token is proved to
Apple before it is kept (a session is opened with it and `GET /account` is read), so that one
that is not good is refused and nothing is stored. Session-only, as `PutSecret` is, and
audited as `mdm_abm_token_put` with the fingerprint and the expiry, never the token. Refused
`abm_unavailable` when this coordinator has no ABM client, `secrets_unavailable` with no
master key, `invalid_request` for a token that is not five parts or a request that gives both
forms or neither, `abm_key_missing` for a `.p7m` with no `abm_key` held, `abm_token_unreadable`
for a `.p7m` that does not open with it, `abm_credential_refused` and `abm_clock_refused`
for the two faults Apple names on the session, `abm_unreachable` when no answer came.

Through `--request` only: `token_json` (bytes), `token_p7m` (bytes).

#### `hkctl mdm get-abm-public-key`

`/hostkeeper.operator.v1.Mdm/GetAbmPublicKey`

The certificate of the ABM server entry's key, in PEM, for the operator to add to the entry
in ABM (the public-key download of the fallback path, when NanoDEP's key cannot be taken
over). The key pair is made when none is held (RSA 2048, stored as `abm_key` and `abm_cert`),
which is why the call is session-only; once made, the same certificate is answered. Audited as
`mdm_abm_cert_get`. The private key is never returned.

#### `hkctl mdm list-enrollment-requests`

`/hostkeeper.operator.v1.Mdm/ListEnrollmentRequests`

The enrollment requests that wait for an operator (docs/design/coordinator-mdm.md, 4.2): each
is a Mac whose Setup Assistant posted a device blob to `/enroll` that the coordinator could not
verify against Apple's roots, so it was given no challenge. The list is in memory (a restart
empties it and the Mac's next post fills it again). Each entry says whether the ABM table lists
the serial now. Changes nothing.

#### `hkctl mdm confirm-enrollment`

`/hostkeeper.operator.v1.Mdm/ConfirmEnrollment`

Confirms the waiting request of `serial`: the next post of that serial from the UDID that
waited is given a challenge, once, for `challenge_ttl_s` seconds. The ABM table is read again
here, and a serial it does not list (or lists as removed) is refused `serial_not_in_abm`. A
confirmation says that the operator saw this Mac enroll; it cannot be made for a serial that
is not waiting (`no_pending_enrollment`). Session-only; audited as `mdm_enroll_confirmed`.
Refused also `enrollment_unavailable` when this coordinator has no MDM listener, `store_failed`.

| flag | value | meaning |
|---|---|---|
| `--serial` | STRING | The serial, 1 to 64 ASCII letters and digits. |

### `releases`

The service `Releases` of the package `hostkeeper.operator.v1`. The desired version of each service, how far the hosts are, and the operator's overrides.

#### `hkctl releases set-desired`

`/hostkeeper.operator.v1.Releases/SetDesired`

Set a service's desired state. The coordinator moves its hosts there by the service's update
plan, stage by stage (`Rollouts`), a role's `max_updating` at a time. Takes
CI's OIDC token (the services its allow-list entry names) or the operator token (any service
the coordinator manages, audited as `set_desired` under the token's name; a set with
`targets` is audited by CI too). Its request is at most 256 KiB; every other method's 4 KiB.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--if-generation` | UINT64 | Write only if the service's generation is this one now; otherwise refused with `generation_conflict`, and nothing changes. Absent: write whatever the generation is. |
| `--write-id` | STRING | The writer's own id for this write, kept with it and returned by `Converge`, so a writer that lost the answer can tell whether its write landed: 1 to 128 visible ASCII characters. Absent: none. |
| `--package-digest` | STRING | The digest of the package CI registers for `desired.version` (64 lower-case hex characters), the one package every host is ordered whatever its platform: for a fleet of one platform. Refused with `release_index` or with `targets` (`index_invalid`). |
| `--release-index` | STRING | The release index of `desired.version` (`index.json`, design section 5.5), byte for byte: per platform, the package file, its size and its SHA-256. The coordinator checks that its SHA-256 is `release_index_digest` (else `index_digest_mismatch`) and that it is the index of `service` at `desired.version` (else `index_invalid`), keeps the digest of each platform's package, and orders each host the package of its own platform. Comes with `release_index_digest`. |
| `--release-index-digest` | STRING | The SHA-256 of `release_index` (64 lower-case hex characters), as CI's release job wrote it down when it published the index: the release this desired state names. |
| `--host-group` | STRING | The host group to set the version in. From a coordinator that holds host groups: the group must list the service (`not_in_host_group`); empty: in each group that lists the service (each group a token limited to groups may deploy to), one deployment and one audit entry per group, in one change; `if_generation` is then taken only when one group lists it (`ambiguous_host_group`). |

Through `--request` only: `desired` (hostkeeper.operator.v1.DesiredState), `targets` (map).

#### `hkctl releases converge`

`/hostkeeper.operator.v1.Releases/Converge`

How far a service's hosts are from its desired version. Changes nothing. Takes CI's OIDC
token or the operator token.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--host-group` | STRING | The host group. From a coordinator that holds host groups: empty for every group that lists the service and has a desired version of it, converged when all are. |

#### `hkctl releases force`

`/hostkeeper.operator.v1.Releases/Force`

Switch a service to a version on the targeted hosts at once, with no hook (see
`ForceRequest`).

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--version` | STRING |  |
| `--reason` | STRING |  |
| `--include-held` | true\|false | Order the held pairs among the targets too, for this call only. |
| `--host-group` | STRING | The host group whose desired version the force sets. From a coordinator that holds host groups: `hosts` must be in it (`unknown_target`), `fleet` is the whole group, and a `role` must name it; empty: one force per group, in one change (the groups of the `hosts`, or each group that lists the service, or the `role`'s). |

Through `--request` only: `fleet` (hostkeeper.operator.v1.FleetTarget, in a oneof), `role` (string, in a oneof), `hosts` (hostkeeper.operator.v1.HostList, in a oneof).

#### `hkctl releases unmark-bad`

`/hostkeeper.operator.v1.Releases/UnmarkBad`

Lift a bad mark from a version, so that it may be ordered and CI may set it again.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--version` | STRING |  |
| `--reason` | STRING |  |

#### `hkctl releases clear-halt`

`/hostkeeper.operator.v1.Releases/ClearHalt`

Lift a service's standing halt.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

#### `hkctl releases status`

`/hostkeeper.operator.v1.Releases/Status`

Everything the coordinator holds about each service. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | One service; empty for every service the coordinator manages. |

#### `hkctl releases mark-bad`

`/hostkeeper.operator.v1.Releases/MarkBad`

Mark a version bad by hand: it is never ordered again until it is unmarked. A held pair is
not reached.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--version` | STRING |  |
| `--reason` | STRING |  |

#### `hkctl releases halt`

`/hostkeeper.operator.v1.Releases/Halt`

Halt a service by hand, as a host's halt report does: no host of it is ordered until the halt
is cleared.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

#### `hkctl releases wait-converged`

`/hostkeeper.operator.v1.Releases/WaitConverged`

Hold the call until a service's hosts have converged on a version, or the wait ends another
way (see `WaitOutcome`), for at most `wait_secs`. The coordinator answers at once when the
wait is over already, and otherwise after the change that ends it. Changes nothing. Takes
CI's OIDC token or the operator token, checked once, when the call is admitted.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service; empty names the legacy service, as for `Converge`. |
| `--version` | STRING | The version the caller set; any other desired version ends the wait as SUPERSEDED. |
| `--wait-secs` | UINT32 | How long to hold the call, 1 to 1800 seconds. |
| `--host-group` | STRING | The host group. From a coordinator that holds host groups: empty for every group that lists the service and has a desired version of it, converged when all are (a group with no hosts is converged). |

#### `hkctl releases get-release`

`/hostkeeper.operator.v1.Releases/GetRelease`

The release a version of a service was set with: its targets, as set, and the digest of the
release index that came with them. Changes nothing. Takes CI's OIDC token or the operator
token, and answers either for any service.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service; empty names the legacy service, as for `Converge`. |
| `--version` | STRING | The version, as it was set. |
| `--host-group` | STRING | The host group. From a coordinator that holds host groups: a named group must list the service (`not_in_host_group`); empty for none. |

### `repositories`

The service `Repositories` of the package `hostkeeper.operator.v1`. The coordinator's registry of package repositories.

#### `hkctl repositories list`

`/hostkeeper.operator.v1.Repositories/List`

Every repository of the registry, by name, without its keyring (`keyring_sha256` names it),
and the registry's generation. Changes nothing.

#### `hkctl repositories get`

`/hostkeeper.operator.v1.Repositories/Get`

One repository, with its keyring. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The repository's name. |

#### `hkctl repositories put`

`/hostkeeper.operator.v1.Repositories/Put`

Adds a repository, or replaces the one of its name: a new keyring or new fingerprints are a
key rotation, which reaches each host whose services name it at its next heartbeat. The
registry's generation grows by one. Writes the audit entry `repository_put`.

| flag | value | meaning |
|---|---|---|
| `--reason` | STRING | Why, for the audit entry. |

Through `--request` only: `repository` (hostkeeper.release.v1.Repository).

#### `hkctl repositories remove`

`/hostkeeper.operator.v1.Repositories/Remove`

Removes a repository no package comes from. The registry's generation grows
by one. Writes the audit entry `repository_remove`.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The repository's name. |
| `--reason` | STRING | Why, for the audit entry. |

### `rollouts`

The service `Rollouts` of the package `hostkeeper.operator.v1`. The update plans of the services: an operator moves a plan on, pauses it and resumes it. Each call is an audit entry under the operator token's name.

#### `hkctl rollouts advance`

`/hostkeeper.operator.v1.Rollouts/Advance`

Enter the next stage of a plan that waits for an operator (`advance = "manual"`): its done
stage, or none yet for a manual first stage.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |
| `--host-group` | STRING | The host group of the plan. From a coordinator that holds host groups: empty for the one group that lists the service (`ambiguous_host_group` when more do). |

#### `hkctl rollouts pause`

`/hostkeeper.operator.v1.Rollouts/Pause`

Pause a plan: no host that has not started is ordered; the updates already ordered or
running finish and bake. The pause stands across a new desired version, until `Resume`.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |
| `--host-group` | STRING | The host group of the plan. From a coordinator that holds host groups: empty for the plan of every group that lists the service, in one change. |

#### `hkctl rollouts resume`

`/hostkeeper.operator.v1.Rollouts/Resume`

Resume a paused plan from where it stands.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |
| `--host-group` | STRING | The host group of the plan. From a coordinator that holds host groups: empty for the one group that lists the service (`ambiguous_host_group` when more do). |

### `services`

The service `Services` of the package `hostkeeper.operator.v1`. The services the coordinator manages, which hosts run them, and the operator's holds.

#### `hkctl services list`

`/hostkeeper.operator.v1.Services/List`

Every service, with its desired version, the roles and hosts it is assigned to, the
operator's assignments, its minimum versions, the hosts ordered to uninstall it and its holds.

#### `hkctl services assign`

`/hostkeeper.operator.v1.Services/Assign`

Assign a service to a host or a role: a host that does not run it is ordered to install it
at its next heartbeat, unless a hold stands. Assigning a service already assigned changes
nothing. Deprecated: a coordinator that holds host groups refuses it `placed_by_host_group`,
since `HostGroups.Put` and `HostGroups.Move` place every package.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--service` | STRING |  |
| `--role` | STRING | A role to assign the service to instead of one host: every host of the role, those that join it later included. Exactly one of `node_id` and `role` is set. |

#### `hkctl services unassign`

`/hostkeeper.operator.v1.Services/Unassign`

Take a service off a host or a role. With `uninstall`, each host that no longer has any
assignment of it is ordered to uninstall it; without, the coordinator stops managing it
there and the host keeps running it. Deprecated: refused `placed_by_host_group` as `Assign`
is.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--service` | STRING |  |
| `--uninstall` | true\|false | Order the host to uninstall the service; false (the default) leaves it running there, unmanaged. |
| `--role` | STRING | A role to take the service off instead of one host. Exactly one of `node_id` and `role` is set. |

#### `hkctl services set-min-version`

`/hostkeeper.operator.v1.Services/SetMinVersion`

Set the version below which the rollout gate orders the service to no host of a role, or to
one host: no update, install or automatic rollback. A role names a host group: the minimum of
the package in that group. A host's own minimum stands before its group's.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |

Through `--request` only: `min_version` (hostkeeper.operator.v1.MinVersion).

#### `hkctl services set-repository`

`/hostkeeper.operator.v1.Services/SetRepository`

Set the repository of the registry the service's package comes from; empty goes back to the
default, `komira`. The coordinator sends each host the repositories of its packages, and a
release names channels of the package's repository. The registry must hold the repository.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The package. |
| `--repository` | STRING | The registry's name of the repository the package comes from. Empty clears the setting: the package goes back to the default repository, `komira`. |
| `--reason` | STRING | Why, 1 to 256 bytes; recorded in the audit entry. |

#### `hkctl services hold`

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

#### `hkctl services unhold`

`/hostkeeper.operator.v1.Services/Unhold`

Release a hold: the pairs it held are ordered to the then-desired version at their next
heartbeat.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

Through `--request` only: `scope` (hostkeeper.operator.v1.HoldScope).

### `users`

The service `Users` of the package `hostkeeper.operator.v1`. The people who sign in with a name and a password. A user is an operator, as a token is: there are no roles. Each mutating call is audited under the caller's name, with the user it concerned as the entry's subject: `user_create`, `user_disable`, `user_enable`, `user_delete` and `password_reset`.

#### `hkctl users list`

`/hostkeeper.operator.v1.Users/List`

Every user, by name. Changes nothing. No password and no hash is ever in the answer.

#### `hkctl users create`

`/hostkeeper.operator.v1.Users/Create`

Makes a user with a generated temporary password, which the answer carries once: the
coordinator keeps its hash only. The user must choose a new password at the first sign-in,
and the temporary one stops working an hour after it was made.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The new user's name. |

#### `hkctl users disable`

`/hostkeeper.operator.v1.Users/Disable`

Stops a user signing in, and ends their sessions at once. The last enabled user cannot be
disabled. Disabling a disabled user changes nothing.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The user's name. |

#### `hkctl users enable`

`/hostkeeper.operator.v1.Users/Enable`

Lets a disabled user sign in again, with the password they had. Enabling an enabled user
changes nothing.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The user's name. |

#### `hkctl users reset-password`

`/hostkeeper.operator.v1.Users/ResetPassword`

Gives a user a new temporary password, which the answer carries once, makes them choose
another at the next sign-in, and ends their sessions. Refused for the caller's own name: a
person changes their own password on the coordinator's page.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The user's name. |

#### `hkctl users delete`

`/hostkeeper.operator.v1.Users/Delete`

Removes a user and ends their sessions. The audit entries that name them stay. The last
enabled user cannot be deleted.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The user's name. |
