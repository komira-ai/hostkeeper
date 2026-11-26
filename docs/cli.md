# hk

<!-- Generated from the protos: `cargo test -p hostkeeper-hk` fails when this file differs, and writes the current text to `cli.md` in the target directory's `tmp`. Do not edit by hand. -->

`hk` is the operator's command line of hostkeeper's coordinator: one program, generated from the protobuf services of `hostkeeper-proto`, that calls one method of the coordinator's JSON gateway ([gateway.md](gateway.md)) per command, and has the SSH tunnel built in. It is released as `hk-<version>-<platform>.tar.gz`, attested like every other asset ([deploy/verify-a-release.md](deploy/verify-a-release.md)); `hk --version` prints `hk <version>+<first 12 hex digits of the commit>`. The archive holds the one file `hk`, and the program says `hk:` before its diagnostics whatever file name it is run as. Every command, flag and help text below is generated from the descriptors; the host API (`hostkeeper.agent.v<n>`), whose caller is the daemon, is not here.

## Rules

Every command is one method: `hk <service> <method>`, both in kebab case as the protos name them (`Releases.SetDesired` is `releases set-desired`), called as one POST of JSON to the coordinator's gateway over HTTPS. hk is one program: it needs no bash, no curl and no jq, and runs no other program for a gateway call. It puts `hk:` before its diagnostics, whatever file name it is run as.

A request field is a flag named after it (`node_id` is `--node-id`): a scalar takes one value; a bool takes `true` or `false`, so that an absent flag and `false` differ; an enum takes a value's name without its prefix, in lower case, never `unspecified`; a repeated field repeats its flag; a `map<string, string>` repeats `--<field> key=value`. A flag takes its value as `--flag value` or `--flag=value`; a value that starts with `--` only as `--flag=value`.

A message, a oneof, `bytes` and any other map come only through `--request <file|->`: a JSON object, `-` for stdin, which the flags add to. A field named both by a flag and by the request file is a usage error.

A secret field (`(hostkeeper.secret)`) is never a flag: it is read with `--<field>-file <path>` (one trailing newline dropped) or `--<field>-stdin`, and never printed. At most one input reads stdin. No token or secret is ever written to a file or put in an argument list or in the environment of a program hk runs: it is read into memory and sent in the request.

The answer, the gateway's JSON, goes to stdout or to `--response-file`; diagnostics go to stderr. A refusal prints `refused: <reason>` when its reason is a token of `[a-z0-9_]{1,64}`. A call refused `invalid_request` makes one more call, with no token, to the coordinator's `GET /v1/version?at_least=<this hk's release>`: when that says the coordinator is older (`409`, or `404` from a coordinator before the route), hk says so on stderr, with the coordinator's version when it gave one, and the reason it prints is `coordinator_older`; any other answer, or none, leaves `invalid_request`.

Four commands are not gateway calls: the SSH tunnel is built into hk. `hk ssh <host-id> [--identity FILE] [--reason TEXT] [-- ssh args]` gets a one-host SSH certificate and runs ssh to the host through the coordinator's tunnel (a word after `ssh` that is not a method of the service `ssh` is a host id). `hk ssh-proxy <host-id>` is the byte stream for `ProxyCommand hk --endpoint URL --ca-file PATH --token-file PATH ssh-proxy %h` in `~/.ssh/config`. `hk ssh-revoke --operator NAME | --serial N | --host <host-id>` revokes certificates. `hk ssh-status <host-id>` shows the host keys pinned for a host and the certificate held. They need --endpoint, --ca-file and --token-file, take no other credential, keep the key, the certificates and the pins in `$HOME/.hostkeeper/ssh`, and read the token from its file. `hk ssh-known-host` is run by ssh itself, as the `KnownHostsCommand` of `hk ssh`, and not by hand.

`hk xcode add [<version> | <archive.xip>]` is a method of the service `xcode` that is not a gateway call: on macOS only, it brings an Xcode release into the coordinator's Xcode store. With no argument it takes the newest `Xcode_<version>.xip` in `$HOME/Downloads` (or the release waiting in the store, downloaded); given a version it takes that release's archive from `$HOME/Downloads` or downloads it with `xcodes download <version>` in the operator's own session (a two-factor prompt is answered at the terminal); given a file, it takes the file. An archive the store holds ends the run with exit 0; Apple's signature must be on it (`pkgutil --check-signature`: exit 0, `signed Apple Software`, a chain ending at Apple Root CA), its version and build are read from the app `xip --expand` makes (or from the record an earlier run kept), and it is uploaded unmodified and streamed with its progress on stderr: `GET /v1/xcode/<sha256>/upload` says how much of it the store holds, and the rest goes in one `PUT /v1/xcode/<sha256>?version=<version>&build=<build>` with a `Content-Range`, so that a broken connection, an `offset_mismatch` or a `partial_locked` is tried again from the offset the store reports (five tries in all, after waits of 2, 4, 8 and 16 seconds), and a run that stops (`the upload stopped at 7.31 of 11.92 GB; run hk xcode add again to continue`, exit 1) is continued by the next run; a coordinator that cannot resume (its query answers 405) gets the whole archive in one request; a step that fails uploads nothing. It needs --endpoint, --ca-file and --token-file (or the settings file), and takes no other credential; `hk xcode add --help` says each step. `hk xcode-push <version> | <archive.xip>` is a second name for it for one release.

`hk xcode verify [--apple-root FILE] <archive.xip>` is a method of the service `xcode` that is not a gateway call: on macOS only, it runs every check of the Mac proof's operator steps on an Xcode archive (the programs, the archive's kind, Apple's signature and published root, a damaged copy and a file that is no archive, the table of contents, and the expansion with its version and build), writes the record of what it found to `$HOME/.hostkeeper/xcode/<sha256>.json` and to stdout, and exits 0 when its verdict is pass and 1 otherwise. It asks no coordinator; `hk xcode verify --help` says each check.

`hk settings init --endpoint URL --ca-file PATH --token-file PATH` writes the settings file `$HOME/.hostkeeper/hk.toml`, which gives `--endpoint`, `--ca-file` and `--token-file` their default for every command that takes them (a call, the ssh commands and `xcode add`; not `tailnet-wait`, which takes no token). A flag on the command line wins over the file; a flag that neither gives is missing, as without a file. The file is a TOML table of the strings `endpoint`, `ca_file` and `token_file`: an address and two paths, never a token. A key of another name is ignored, with `hk: <path>: unknown key <name> ignored` on stderr. hk refuses a file, or a `$HOME/.hostkeeper`, that is a link, that another user owns or that its group or others may write, naming the path; `--token-stdin` and `--github-oidc-audience` leave the file's token file unused. `hk settings --help` says what init does.

`hk tailnet-wait [--attempts N] [--interval-secs S]` is not a gateway call either: run by a CI job right after it joined the tailnet, it asks the coordinator's `GET /v1/version`, with no token, up to N times (6 by default) with S seconds between two tries (10 by default), until any HTTP answer comes; when none does, it runs `tailscale down`, then `tailscale up`, and asks as many times again. It ends 0 at the first answer, with one line saying which try it was, and 1 when neither round had one, saying how each ended. It needs --endpoint and --ca-file, takes --timeout-secs for each try, and takes no token; `hk tailnet-wait --help` says each step.

## Global flags

| flag | value | default | meaning |
|---|---|---|---|
| `--endpoint` | URL |  | The coordinator's private listener, as https://<host>[:<port>]. Needed for a call, unless the settings file gives it. |
| `--ca-file` | PATH |  | PEM certificates of the CA the coordinator's certificate must chain to; no other root is trusted for the coordinator. Needed for a call, unless the settings file gives it. |
| `--token-file` | PATH |  | Read the bearer token (an operator token) from this file, which only its owner may read; one trailing newline is dropped. The token is never an argument, an environment variable or a line of output. The settings file's `token_file` is the default when no other credential is given. |
| `--token-stdin` |  |  | Read the bearer token from stdin instead. |
| `--github-oidc-audience` | AUDIENCE |  | In a GitHub Actions job with id-token: write: ask the runner for a fresh ID token for this audience and call with it; `::add-mask::<token>` is printed on stdout first. |
| `--response-file` | PATH |  | Write the answer to this file instead of stdout. |
| `--timeout-secs` | SECONDS | 30 | How long to wait to connect, and for the answer, 1 to 999999; a method that holds the call (a `wait_secs` field) adds its hold. |
| `--help` |  |  | Print the help of hk, a service or a method. |
| `--version` |  |  | Print `hk <version>+<commit>`; only before the service, so a method's own `--version` is its field. |

## Exit codes

| code | meaning | gateway codes |
|---|---|---|
| 0 | the call succeeded |  |
| 1 | anything else: no answer, an answer that is not the gateway's, or any other code | CANCELLED, UNKNOWN, DEADLINE_EXCEEDED, NOT_FOUND, ALREADY_EXISTS, RESOURCE_EXHAUSTED, ABORTED, OUT_OF_RANGE, UNIMPLEMENTED, INTERNAL, UNAVAILABLE, DATA_LOSS |
| 2 | a usage error, or the coordinator refused the request: INVALID_ARGUMENT or FAILED_PRECONDITION | INVALID_ARGUMENT, FAILED_PRECONDITION |
| 3 | the coordinator refused the caller: UNAUTHENTICATED or PERMISSION_DENIED | PERMISSION_DENIED, UNAUTHENTICATED |

## Commands

### `alerts`

The service `Alerts` of the package `hostkeeper.operator.v1`. The conditions that need a person.

#### `hk alerts list`

`/hostkeeper.operator.v1.Alerts/List`

Every condition that needs a person now, by kind, then service, host and version. Changes
nothing.

### `api-tokens`

The service `ApiTokens` of the package `hostkeeper.operator.v1`. The tokens a script calls with. A token made here belongs to a user, its owner, and acts as that user: every check of who may do what (the console's operators file among them) reads the owner's name, and the audit records the owner as `by` and the token as `api_token`. A token cannot sign in. Revoking it, or disabling or deleting its owner, refuses it from the next call; deleting the owner also removes their tokens. A token of the tokens file has no owner and acts as the operator it is named for. Each mutating call is audited under the caller's name, with the token as the entry's subject: `api_token_create` and `api_token_revoke`.

#### `hk api-tokens list`

`/hostkeeper.operator.v1.ApiTokens/List`

Every token, by name: the coordinator's tokens file's, which are read-only, and those made
here. Changes nothing. No token and no digest is ever in the answer.

#### `hk api-tokens create`

`/hostkeeper.operator.v1.ApiTokens/Create`

Makes a token of 32 random bytes, `hkt_` and 43 base64url characters, which the answer carries
once: the coordinator keeps its SHA-256 only.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The new token's name. |
| `--owner` | STRING | The user the token belongs to and acts as; absent, the caller (a user, or the owner of the caller's token). A name that is no user's is `unknown_owner`. |

#### `hk api-tokens revoke`

`/hostkeeper.operator.v1.ApiTokens/Revoke`

Removes a token made here: it is refused from then on. A token of the tokens file is removed
by editing the file.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The token's name. |

### `audit`

The service `Audit` of the package `hostkeeper.operator.v1`. The coordinator's audit log.

#### `hk audit list`

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

### `console`

The service `Console` of the package `hostkeeper.operator.v1`. A host's console. One session per host: a second `Open` of a host that has one is refused `console_busy`, and `List` names who holds it. Each call that acts is audited under the caller's name with the host as the entry's node: `console_open`, `console_close`, `power_action` and `boot_device_set`; a refusal of an operator named in the console operators file is audited as `console_denied` with its cause.

#### `hk console open`

`/hostkeeper.operator.v1.Console/Open`

Opens a console session on a host: takes the host's one session slot and writes the audit
line, without contacting the device. The session is then attached by a WebSocket at
`/console/v1/stream/<session>` on the private listener, by the same operator name, within
`expires_in_secs`; an unattached session ends after that.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING | The host's id. |
| `--reason` | STRING | Why the console is opened; the audit line carries it. |

#### `hk console power`

`/hostkeeper.operator.v1.Console/Power`

Reads or changes a Linux server's power through its BMC, after checking that the BMC at the
host's address has the host's BMC MAC. Answered when the BMC has acknowledged the command;
the host's own state follows later (`Hosts.Get`).

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING | The host's id. |
| `--action` | VALUE | The command. One of: status, on, off, soft, cycle, reset. |
| `--reason` | STRING | Why; the audit line carries it. |

#### `hk console set-boot-device`

`/hostkeeper.operator.v1.Console/SetBootDevice`

Sets the device a Linux server boots from through its BMC, after the same MAC check as
`Power`, and answers with the boot flags the BMC holds afterwards, read back.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING | The host's id. |
| `--device` | VALUE | The device to boot from. One of: pxe, disk, bios. |
| `--persistent` | true\|false | Every boot from now on, not only the next one. |
| `--efi` | true\|false | Boot in UEFI mode; a UEFI board may ignore the device otherwise. |
| `--reason` | STRING | Why; the audit line carries it. |

#### `hk console list`

`/hostkeeper.operator.v1.Console/List`

Every open console session, by host id. Changes nothing and is not audited.

#### `hk console close`

`/hostkeeper.operator.v1.Console/Close`

Ends the console session of a host, whoever holds it. The closer's name is in the audit
line.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING | The host whose session ends. |

#### `hk console probe`

`/hostkeeper.operator.v1.Console/Probe`

Reads a Linux server's serial console without typing and says what it saw: opens a session
as `Open` does (the same checks, refusals and `console_open` line), attaches it in the
coordinator, reads until `max_bytes` came, `quiet_secs` passed without a byte or
`wait_secs` passed, then ends it as a stream ends (the payload deactivated, one
`console_close` line). It sends the host no byte, not even a newline. A Mac is `no_bmc_mac`; a
device not reached, or that fails before the host sent a byte, is refused with its cause.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING | The host's id. |
| `--reason` | STRING | Why the console is probed; the `console_open` line carries it. |
| `--wait-secs` | UINT32 | The longest the read lasts, in seconds: 1 to 900 (`invalid_request` otherwise). `hk` waits this long beyond its own timeout. |
| `--quiet-secs` | UINT32 | The read ends once this many seconds pass with no byte from the host, from its start or from the last byte; 0 is the coordinator's `--console-quiet-s`. |
| `--max-bytes` | UINT32 | The read ends once this many bytes came: 0 is 65536; above 1048576 is `invalid_request`. |

### `enrollment`

The service `Enrollment` of the package `hostkeeper.operator.v1`. The hosts waiting for an operator to approve their enrollment.

#### `hk enrollment list`

`/hostkeeper.operator.v1.Enrollment/List`

The pending requests, oldest first, a page at a time; with `unenrolled`, the requests that
expired instead.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most requests to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |
| `--unenrolled` | true\|false | List the requests that expired, whose hosts are unenrolled, in place of the pending ones: each waited for an operator longer than the coordinator's `--enrollment-request-ttl` and is no longer approved. The order and the page tokens are the same. A coordinator before expiry ignores the field and lists the pending requests, each with `state` UNSPECIFIED. |

#### `hk enrollment approve`

`/hostkeeper.operator.v1.Enrollment/Approve`

Approve a pending request: the host gets its certificate at its next `Poll`, with the host
id and host group the approval names. The group is optional: a host approved with none is
enrolled in no group: it is ordered no package until `HostGroups.Move` puts it in one. The
reason is optional too.

| flag | value | meaning |
|---|---|---|
| `--request-id` | STRING |  |
| `--key-sha256` | STRING | The SHA-256 of the pending request's key, as `PendingEnrollment.key_sha256` lists it. |
| `--host-id` | STRING | The host id the certificate names. For a request (not a renewal) that matches exactly one inventory record, empty means that record's host id, and another host id needs `replace`; otherwise it is required. |
| `--role` | STRING | The host's role. From a coordinator that holds host groups, an alias of `host_group` kept for clients that predate it: read as the group's name when `host_group` is empty, and when both are empty the host is enrolled in no group. A coordinator that holds no host groups requires it. |
| `--services` | STRING | Services to assign to the host; may be empty. Deprecated: a coordinator that holds host groups refuses any (`services_retired`). Repeat the flag for more. |
| `--replace` | true\|false | Take the host id from the host that holds it, revoking that host's certificate; also needed to approve a `duplicate_claim` request or a denied host id. It never approves a contested request (`record_contested`). |
| `--reason` | STRING | Why, for the audit log: 1 to 256 bytes with no control character, or empty for none. |
| `--host-group` | STRING | The host group the host is in, which must exist (`unknown_host_group`; a coordinator that does not hold host groups refuses any); empty for none (with `role` empty too, from a coordinator that holds host groups): the host is enrolled in no group until `HostGroups.Move`. With a renewal or `replace`, the group of that host (`host_group_differs`); empty keeps it. |
| `--rack` | STRING | Where the server stands, an optional placement the approval makes in the same step (`docs/design/ui-shell-lab.md`): the lab layout's rack id, which must exist (`unknown_rack`); empty for none, and then `placement_u` and `placement_height_u` are 0 (`placement_invalid`): the host is unplaced until it is placed (`Place` of `Layout`). A coordinator that holds no layout refuses a rack (`unknown_rack`). The placement is of the inventory entry the host id names when there is one, so the server keeps its place across a wipe, else of the host. A refused placement refuses the approval, and nothing changes (`placement_overlap`, `placement_out_of_rack`). |
| `--placement-u` | UINT32 | The lowest U the server occupies, from 1 (the bottom); required with `rack`. |
| `--placement-height-u` | UINT32 | How many U it occupies; required with `rack`. |
| `--placement-sled` | STRING | Or the sled the server stands on (`Layout.Get`'s `sleds`) and its slot from 0 (left out: the lowest free): `rack`, `placement_u` and `placement_height_u` left empty (`placement_invalid` otherwise). A slot another subject holds is `placement_overlap`, a sled the layout lacks `placement_invalid`. With `rack`, `placement_u` and `placement_height_u` instead, the approval makes a sled of that U and height for the server, which goes when the server leaves it. |
| `--placement-slot` | UINT32 |  |

#### `hk enrollment reject`

`/hostkeeper.operator.v1.Enrollment/Reject`

Reject a pending request: it leaves the queue, and its key is refused from then on (its
`Poll` answers `DENIED`, and posting it again changes nothing). A request already rejected
is answered as rejected and audited once.

| flag | value | meaning |
|---|---|---|
| `--request-id` | STRING | The request to reject, as `PendingEnrollment.request_id` lists it. |
| `--reason` | STRING | Why, for the audit log: 1 to 256 bytes with no control character, or empty for none. |

#### `hk enrollment add-record`

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
| `--notes` | STRING | Empty for none; otherwise as `InventoryRecord.notes`. |
| `--approve-until-unix` | UINT64 | The end of the record's window, unix seconds: after now, and within the coordinator's `--record-approve-max` of it. 0 for none given. |
| `--approve-for-secs` | UINT64 | The length of the record's window from now, in seconds: at most the coordinator's `--record-approve-max`. 0 for none given. With neither, the window is the coordinator's `--record-approve-for`; with both, the call is refused `invalid_window`. A record that names no role takes neither, and has no window. |
| `--platform` | VALUE | The platform to install the server as by network boot: `PLATFORM_LINUX_X86_64` or `PLATFORM_LINUX_ARM64` (`linux_x86_64` or `linux_arm64` as a command-line flag); `PLATFORM_UNSPECIFIED` (the flag left out) for none. Any other value is refused `invalid_request`. One of: linux_x86_64, linux_arm64, darwin_arm64. |
| `--host-group` | STRING | The host group the host is to be in, which must exist (`unknown_host_group`; a coordinator that does not hold host groups refuses any); empty for none. A record that names a group has a window as one that names a role does. |

#### `hk enrollment list-records`

`/hostkeeper.operator.v1.Enrollment/ListRecords`

The records, by host id, a page at a time; all of them, or only those a host is enrolled
against, or only the others.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most records to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |
| `--enrolled` | true\|false | Unset for every record; true for only the records a host is enrolled against (`InventoryRecord.enrolled`), false for only the others. |

#### `hk enrollment remove-record`

`/hostkeeper.operator.v1.Enrollment/RemoveRecord`

Remove the record of a host id. An enrolled host keeps its certificate.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING |  |
| `--reason` | STRING |  |

#### `hk enrollment update-record`

`/hostkeeper.operator.v1.Enrollment/UpdateRecord`

Change the data MACs, the BMC MAC and the notes of the record of a host id, and nothing else
of it: its role, host group, serial, platform, window, use and `added_by` stay as they are,
which removing the record and adding it again would not keep. The three fields are set as
given (empty clears the BMC MAC and the notes), checked as `AddRecord` checks them, and the
record must still hold a data MAC or a serial. A MAC that another record holds is refused
`record_conflict`. A pending request that matches the record is approved then, as at
`AddRecord`.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING |  |
| `--data-macs` | STRING | As `AddRecordRequest.data_macs`: the whole list, not additions. Repeat the flag for more. |
| `--bmc-mac` | STRING | As `AddRecordRequest.bmc_mac`. |
| `--notes` | STRING | As `AddRecordRequest.notes`. |

#### `hk enrollment open-record`

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

The service `HostGroups` of the package `hostkeeper.operator.v1`. The host groups: what each group's hosts run, and by which rollout policy. A host is in one group or in none; a host in none is ordered no package (it keeps what it runs). Every group lists `hostkeeper`. There is no built-in group: `all` is a name like any other. Each mutating call takes a reason and answers the audit entry it wrote.

#### `hk host-groups list`

`/hostkeeper.operator.v1.HostGroups/List`

Every group, by name, a page at a time, each with its packages, policy, generation, host
count and, per package, its desired version. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most groups to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |

#### `hk host-groups get`

`/hostkeeper.operator.v1.HostGroups/Get`

One group, with, per package, its desired version, minimum version and convergence, and a
page of its hosts. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--host-group` | STRING | The group. |
| `--limit` | UINT32 | The most hosts to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |

#### `hk host-groups put`

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

#### `hk host-groups remove`

`/hostkeeper.operator.v1.HostGroups/Remove`

Remove a group no host is in or moving into, and that no inventory record, pending approval
or Mac allowlist entry names, with its desired versions and plan positions. Audited
`host_group_remove`.

| flag | value | meaning |
|---|---|---|
| `--host-group` | STRING | The group. |
| `--reason` | STRING | Why; 1 to 256 bytes. |

#### `hk host-groups move`

`/hostkeeper.operator.v1.HostGroups/Move`

Move an enrolled host into another group, in one call: it is in the new group from this
write, and is moving until its packages match the new group's. An empty `host_group` moves
it out of its group into none: it is ordered no package from then on and keeps what it runs;
nothing is uninstalled. Audited `host_move`.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING | The host, by the host id it was enrolled as. |
| `--host-group` | STRING | The group it moves into; empty for none (the host is ordered no package and keeps what it runs). |
| `--reason` | STRING | Why; 1 to 256 bytes. |

### `hosts`

The service `Hosts` of the package `hostkeeper.operator.v1`. The hosts the coordinator has heard from or enrolled, and what an operator decides about them.

#### `hk hosts list`

`/hostkeeper.operator.v1.Hosts/List`

Every host heard from since the coordinator started and, when the coordinator has an
enrollment, every host it enrolled that is not denied, by node id, a page at a time. A host
enrolled and not heard since the coordinator started is listed as not live, with what its
enrollment kept of what it runs (`Host.running`). Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | The most hosts to return; capped at 256, and 0 means the cap. |
| `--page-token` | STRING | Where to go on, as `next_page_token` gave it; empty for the first page. |

#### `hk hosts get`

`/hostkeeper.operator.v1.Hosts/Get`

One host heard from since the coordinator started; `unknown_host` for any other, an enrolled
host not heard since included. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |

#### `hk hosts exclude`

`/hostkeeper.operator.v1.Hosts/Exclude`

Take a host out of a service: its heartbeats do not count for the service and it is not
ordered. A second exclusion of the same host and service replaces the first.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--node-id` | STRING |  |
| `--reason` | STRING |  |
| `--abandon` | true\|false | Also free the update slot the host holds for the service mid-update, for a host that is gone and will not report its update final. False (the default) keeps the slot. |

#### `hk hosts include`

`/hostkeeper.operator.v1.Hosts/Include`

Put an excluded host back. Including a host that is not excluded changes nothing and is not
an error.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--node-id` | STRING |  |

#### `hk hosts set-labels`

`/hostkeeper.operator.v1.Hosts/SetLabels`

Replace a host's labels, which service selectors match and which the host's facts file
carries (its daemon is sent them at its next heartbeat). An empty set removes them.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--labels` | KEY=VALUE | At most 8; each key and value 1 to 32 bytes of `[a-z0-9-]`. Repeat the flag for more. |

#### `hk hosts abort`

`/hostkeeper.operator.v1.Hosts/Abort`

Queue an `abort` order of a service for the host's next heartbeat; it takes no slot. An
abort of a host that runs nothing of the service is accepted and changes nothing.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING |  |
| `--service` | STRING |  |
| `--reason` | STRING |  |

#### `hk hosts retire`

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

#### `hk hosts clear-record`

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

#### `hk hosts retry`

`/hostkeeper.operator.v1.Hosts/Retry`

Set aside the host's failed report of the service, so it is ordered the desired version again
at its next heartbeat, at the next try (see `RetryRequest`). Audited `retry`.

| flag | value | meaning |
|---|---|---|
| `--node-id` | STRING | The host, heard from since the coordinator started. |
| `--service` | STRING | The service whose failed report is set aside; one the coordinator manages. |

### `layout`

The service `Layout` of the package `hostkeeper.operator.v1`. The lab's layout. Every write is audited under the caller's name: `layout_name_put`, `layout_row_add`, `layout_row_remove`, `layout_rack_put`, `layout_rack_remove`, `layout_place`, `layout_unplace`, `layout_device_put` and `layout_device_remove`. A write takes an optional reason; without one the audit says "layout edit".

#### `hk layout get`

`/hostkeeper.operator.v1.Layout/Get`

The whole layout: the cluster's name, every row by number, every rack by row and number,
every placement by rack and lowest U. Changes nothing. The layout is empty on a coordinator
that holds none.

#### `hk layout put-name`

`/hostkeeper.operator.v1.Layout/PutName`

Names the cluster.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | 1 to 32 characters of letters, digits, space, `.`, `_` and `-`. |
| `--reason` | STRING | 0 to 256 bytes, no control character; empty: "layout edit". |

#### `hk layout add-row`

`/hostkeeper.operator.v1.Layout/AddRow`

Adds the next row: its number is one more than the highest row number held (0 for the first).

| flag | value | meaning |
|---|---|---|
| `--reason` | STRING |  |

#### `hk layout remove-row`

`/hostkeeper.operator.v1.Layout/RemoveRow`

Removes a row that holds no rack: refused `in_use` otherwise. A rack is removed only when it
holds no placement, so a row's servers are named by `Get` (the page names them).

| flag | value | meaning |
|---|---|---|
| `--id` | STRING |  |
| `--reason` | STRING |  |

#### `hk layout put-rack`

`/hostkeeper.operator.v1.Layout/PutRack`

Adds a rack to a row (empty `id`): its number is one more than the highest rack number of
that row, 0 for the first. With an `id`, resizes that rack and, when `row` names another row, moves it there
(it takes that row's next number). A rack made shorter than a placement it holds is refused.

| flag | value | meaning |
|---|---|---|
| `--id` | STRING | Empty to add a rack; else the rack to resize or move. |
| `--row` | STRING | The id of the row it stands in (or, with an `id`, is moved to). |
| `--height-u` | UINT32 | 1 to 60; 0 on an add is 42. |
| `--reason` | STRING |  |

#### `hk layout remove-rack`

`/hostkeeper.operator.v1.Layout/RemoveRack`

Removes a rack that holds no placement.

| flag | value | meaning |
|---|---|---|
| `--id` | STRING |  |
| `--reason` | STRING |  |

#### `hk layout place`

`/hostkeeper.operator.v1.Layout/Place`

Puts a server in a rack, or moves it there from where it was: a subject is in at most one
place.

| flag | value | meaning |
|---|---|---|
| `--rack` | STRING | The id of the rack. |
| `--height-u` | UINT32 | How many U it occupies, 1 to the rack's height; required (0 is refused). |
| `--u` | UINT32 | The lowest U it occupies; required (0 is refused). |
| `--reason` | STRING |  |
| `--sled` | STRING | To put the subject on a sled: the sled's id (`Get`'s `sleds`), with `rack`, `u` and `height_u` left empty (a rack and a sled together is `invalid_request`). `slot` is the slot from 0; left out, the lowest free one. A slot another subject holds is `overlap`, one past 7 `invalid_request`, a sled with no free slot `capacity`. With `rack`, `u` and `height_u` instead, a new sled of that U and height is made there and the subject stands on its slot 0 (the sled goes when the subject leaves it and it is empty). |
| `--slot` | UINT32 |  |

Through `--request` only: `subject` (hostkeeper.operator.v1.LayoutSubject).

#### `hk layout unplace`

`/hostkeeper.operator.v1.Layout/Unplace`

Takes a server out of its rack: it is unplaced again.

| flag | value | meaning |
|---|---|---|
| `--reason` | STRING |  |

Through `--request` only: `subject` (hostkeeper.operator.v1.LayoutSubject).

#### `hk layout put-device`

`/hostkeeper.operator.v1.Layout/PutDevice`

Adds a network device (a switch) to the lab (empty `id`: its id is `net.<n>`, one more than
the highest held), or replaces every field of the device `id`. A device is placed like an
inventory entry: `Place` with the subject kind INVENTORY and the device's id.

| flag | value | meaning |
|---|---|---|
| `--id` | STRING | Empty to add; else the device to replace. |
| `--name` | STRING | Every field of the device but `id`. |
| `--role` | VALUE | One of: bmc, data. |
| `--console-host` | STRING |  |
| `--console-device` | STRING |  |
| `--management-mac` | STRING |  |
| `--notes` | STRING |  |
| `--reason` | STRING |  |

#### `hk layout remove-device`

`/hostkeeper.operator.v1.Layout/RemoveDevice`

Removes a network device and its placement, if it has one.

| flag | value | meaning |
|---|---|---|
| `--id` | STRING |  |
| `--reason` | STRING |  |

#### `hk layout put-sled`

`/hostkeeper.operator.v1.Layout/PutSled`

Adds a sled to a rack (empty `id`: its id is `sled-<n>`, one more than the highest held), or
replaces the rack, lowest U, height and label of the sled `id` (a move or a resize): what
stands on a sled moves with it.

| flag | value | meaning |
|---|---|---|
| `--id` | STRING | Empty to add; else the sled to replace. |
| `--rack` | STRING | The id of the rack it stands in (or, with an `id`, is moved to). |
| `--u` | UINT32 | The lowest U it occupies, 1 or more; required. |
| `--height-u` | UINT32 | 1 to 60; required. |
| `--label` | STRING |  |
| `--reason` | STRING |  |
| `--slots` | UINT32 | 1 to 8; 0 on an add is 1, 0 on a replace keeps the sled's count. A count below the slot of an item on the sled is `in_use`, naming it. |

#### `hk layout remove-sled`

`/hostkeeper.operator.v1.Layout/RemoveSled`

Removes a sled that holds nothing: refused `in_use`, naming what stands on it, otherwise.

| flag | value | meaning |
|---|---|---|
| `--id` | STRING |  |
| `--reason` | STRING |  |

### `maintenance`

The service `Maintenance` of the package `hostkeeper.operator.v1`. The maintenance runs. Each call that changes something takes a reason and answers the audit entry it wrote, under the caller's name, with the caller's reason followed by `run <id>`: `maintenance_schedule` (with the host group as the entry's `host_group` when the run names one), `maintenance_cancel`, `maintenance_retry_host` and `maintenance_put_back` (each with the host as the entry's `node_id`). `List`, `Get` and a `Schedule` without `confirm` change nothing and write no audit entry.

#### `hk maintenance schedule`

`/hostkeeper.operator.v1.Maintenance/Schedule`

Schedule a run: of the kinds asked (all that are due, when none is named), on the hosts named
or every host of a host group, now or in a window, with the group's batch and drain deadline
unless the request names its own. The run takes the hosts that have something due of its
kinds; a host it cannot order is listed skipped, with why. Without `confirm` nothing is stored
and the answer is the run as it would be, with no id; it is refused as the stored run would
be.

| flag | value | meaning |
|---|---|---|
| `--kinds` | VALUE | The kinds to apply, each at most once; empty: every kind that is due on each host. `probe` is never due and never taken by an empty list: it is named alone, with `host_ids`, and a run of it holds each host named whose daemon lists `probe` on demand (`MaintenanceReport.on_demand`); `invalid_request` with another kind or with `host_group`. One of: macos, xcode, linux, reboot, probe. Repeat the flag for more. |
| `--host-ids` | STRING | The hosts, 1 to 32, by node id: one host, or a selection. Empty when `host_group` is set. Repeat the flag for more. |
| `--host-group` | STRING | Every host of this host group. Empty when `host_ids` is set. |
| `--window-start-unix` | UINT64 | When the window opens; 0: now. |
| `--window-end-unix` | UINT64 | When the window ends: no host starts its turn after it, and a host in its turn finishes; 0: no end. Set whenever `window_start_unix` is. |
| `--batch` | UINT32 | How many hosts of each group may be in their turn at once, 1 to 10000; absent: the group's `batch`. |
| `--drain-deadline-secs` | UINT64 | How long a host's drain may wait before the host is put back as it was and deferred; 0: no deadline; absent: the group's `drain_deadline_secs`. |
| `--reason` | STRING | Why; the audit entry carries it. Needed with `confirm`; a call without it may leave it out. |
| `--take-last-host` | true\|false | Take every host of a group, the one host of a group of one included; otherwise such a run is refused (`last_host`). |
| `--confirm` | true\|false | Store the run. Without it nothing is stored, no host is ordered, and the answer is the run as it would be. |

#### `hk maintenance list`

`/hostkeeper.operator.v1.Maintenance/List`

The newest runs, newest first, as many as `limit` allows, with no hosts; every run, or those
in the states asked. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--states` | VALUE | Only the runs in these states; empty: every run. One of: scheduled, running, halted, done, cancelled, window_ended. Repeat the flag for more. |
| `--limit` | UINT32 | At most this many of the newest runs. |

#### `hk maintenance get`

`/hostkeeper.operator.v1.Maintenance/Get`

One run, with each of its hosts, by node id: its step, since when, and why it halted,
deferred or was skipped. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--run-id` | STRING | The run's id. |

#### `hk maintenance cancel`

`/hostkeeper.operator.v1.Maintenance/Cancel`

Cancel a run: no host of it starts its turn from now on; a host in its turn finishes it, and
the hosts that had not started end as not reached. A halted host stays halted, and cordoned,
until it is put back: `PutBack` is still taken. Audited `maintenance_cancel`.

| flag | value | meaning |
|---|---|---|
| `--run-id` | STRING | The run's id. |
| `--reason` | STRING | Why; the audit entry carries it. |

#### `hk maintenance retry-host`

`/hostkeeper.operator.v1.Maintenance/RetryHost`

Retry a halted host's turn from the step that failed: its daemon is ordered the run again
(`MAINTAIN_ACTION_RETRY`) with a new try. The order is queued, and sent at each heartbeat of
the host until its daemon reports the turn at another step; the answer is the turn as stored,
still halted. Audited `maintenance_retry_host`.

| flag | value | meaning |
|---|---|---|
| `--run-id` | STRING | The run's id. |
| `--host-id` | STRING | The halted host, by node id. |
| `--reason` | STRING | Why; the audit entry carries it. |

#### `hk maintenance put-back`

`/hostkeeper.operator.v1.Maintenance/PutBack`

Put a halted host back: its daemon runs the drained services' `resume`
(`MAINTAIN_ACTION_PUT_BACK`) and leaves the update as far as it got; the host's turn ends, and
the run goes on in its group. Taken in a run that has ended too (cancelled, or past its
window), for a host still halted there. The order is queued, and sent at each heartbeat of
the host until its daemon reports the turn at another step; the answer is the turn as stored,
still halted. Audited `maintenance_put_back`.

| flag | value | meaning |
|---|---|---|
| `--run-id` | STRING | The run's id. |
| `--host-id` | STRING | The halted host, by node id. |
| `--reason` | STRING | Why; the audit entry carries it. |

### `mdm`

The service `Mdm` of the package `hostkeeper.operator.v1`. No longer served: every method answers UNIMPLEMENTED, the coordinator's MDM having been removed (2026-11-14). The devices of the MDM policy gate, as it was.

#### `hk mdm list-devices`

`/hostkeeper.operator.v1.Mdm/ListDevices`

The devices the gate has a record of, by enrollment id, from the records alone: no device
is asked. A coordinator that runs no gate answers none, with `gate` false. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--limit` | UINT32 | At most this many devices; 0 means the cap. |
| `--page-token` | STRING | Empty for the first page; else the `next_page_token` of the page before. |

#### `hk mdm put-secret`

`/hostkeeper.operator.v1.Mdm/PutSecret`

Seals `material` as the next version of the secret of `kind` and keeps it; the version it
replaces is kept for the `secret_grace_days` setting, so that a wrong upload is undone by
uploading again. Session-only: a call with a bearer token is refused `PERMISSION_DENIED`
`session_required`, so `hk` cannot make it. Audited under the signed-in user's name as
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

#### `hk mdm put-setting`

`/hostkeeper.operator.v1.Mdm/PutSetting`

Sets the setting `name` to `value`, replacing what it was. Session-only, as `PutSecret` is.
Audited as `mdm_setting_set` with the name and the value.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The setting, one `ListSettings` names: else `invalid_request`. |
| `--value` | STRING | The value, in the form the setting takes (a number of days, or a falling list of them such as `60,14`): else `invalid_request`. |

#### `hk mdm list-secrets`

`/hostkeeper.operator.v1.Mdm/ListSecrets`

Every version of every secret the coordinator holds, by kind and version: what is shown of
each, never its value. Changes nothing.

#### `hk mdm list-settings`

`/hostkeeper.operator.v1.Mdm/ListSettings`

Every setting this release knows, with its value and whether that is its default. Changes
nothing.

#### `hk mdm create-push-request`

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

#### `hk mdm put-push-reply`

`/hostkeeper.operator.v1.Mdm/PutPushReply`

Decrypts the reply of mdmcert.download (the attachment of its email, as the mail has it, hex
text, or its raw bytes) with the pending request's encryption key and checks that it is the
signed request for the pending CSR; answers it, for the operator to upload to Apple's push
portal, which returns the certificate for `PutPushCertificate`. Stores nothing. Session-only;
audited as `mdm_push_reply_put`. Refused `no_pending_request`, `reply_undecryptable` (not a
message encrypted to the pending request's key), `reply_not_for_request`, `audit_unavailable`.

Through `--request` only: `reply` (bytes).

#### `hk mdm put-push-certificate`

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

#### `hk mdm status`

`/hostkeeper.operator.v1.Mdm/Status`

Whether the MDM setup is complete (`configured`), and when it is not, which conditions are
missing; the name, its enrollment URL, and what is shown of the push certificate and key, the
ABM token and the TLS certificate and key in use, with the expiry and topic read from their
material. Computed from the records at each call and never stored, so it cannot go stale.
Never returns a value of a secret. Changes nothing.

#### `hk mdm put-name`

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

#### `hk mdm verify`

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

#### `hk mdm put-abm-token`

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

#### `hk mdm get-abm-public-key`

`/hostkeeper.operator.v1.Mdm/GetAbmPublicKey`

The certificate of the ABM server entry's key, in PEM, for the operator to add to the entry
in ABM (the public-key download of the fallback path, when NanoDEP's key cannot be taken
over). The key pair is made when none is held (RSA 2048, stored as `abm_key` and `abm_cert`),
which is why the call is session-only; once made, the same certificate is answered. Audited as
`mdm_abm_cert_get`. The private key is never returned.

#### `hk mdm list-enrollment-requests`

`/hostkeeper.operator.v1.Mdm/ListEnrollmentRequests`

The enrollment requests that wait for an operator (docs/design/coordinator-mdm.md, 4.2): each
is a Mac whose Setup Assistant posted a device blob to `/enroll` that the coordinator could not
verify against Apple's roots, so it was given no challenge. The list is in memory (a restart
empties it and the Mac's next post fills it again). Each entry says whether the ABM table lists
the serial now. Changes nothing.

#### `hk mdm confirm-enrollment`

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

#### `hk releases set-desired`

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

#### `hk releases converge`

`/hostkeeper.operator.v1.Releases/Converge`

How far a service's hosts are from its desired version. Changes nothing. Takes CI's OIDC
token or the operator token.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--host-group` | STRING | The host group. From a coordinator that holds host groups: empty for every group that lists the service and has a desired version of it, converged when all are. |

#### `hk releases force`

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

#### `hk releases unmark-bad`

`/hostkeeper.operator.v1.Releases/UnmarkBad`

Lift a bad mark from a version, so that it may be ordered and CI may set it again.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--version` | STRING |  |
| `--reason` | STRING |  |

#### `hk releases clear-halt`

`/hostkeeper.operator.v1.Releases/ClearHalt`

Lift a service's standing halt, and the halts hosts hold of it of their own.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

#### `hk releases status`

`/hostkeeper.operator.v1.Releases/Status`

Everything the coordinator holds about each service. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | One service; empty for every service the coordinator manages. |

#### `hk releases mark-bad`

`/hostkeeper.operator.v1.Releases/MarkBad`

Mark a version bad by hand: it is never ordered again until it is unmarked. A held pair is
not reached.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--version` | STRING |  |
| `--reason` | STRING |  |

#### `hk releases halt`

`/hostkeeper.operator.v1.Releases/Halt`

Halt a service by hand, as a host's halt report does: no host of it is ordered until the halt
is cleared.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

#### `hk releases wait-converged`

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

#### `hk releases get-release`

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

#### `hk repositories list`

`/hostkeeper.operator.v1.Repositories/List`

Every repository of the registry, by name, without its keyring (`keyring_sha256` names it),
and the registry's generation. Changes nothing.

#### `hk repositories get`

`/hostkeeper.operator.v1.Repositories/Get`

One repository, with its keyring. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The repository's name. |

#### `hk repositories put`

`/hostkeeper.operator.v1.Repositories/Put`

Adds a repository, or replaces the one of its name: a new keyring or new fingerprints are a
key rotation, which reaches each host whose services name it at its next heartbeat. The
registry's generation grows by one. Writes the audit entry `repository_put`.

| flag | value | meaning |
|---|---|---|
| `--reason` | STRING | Why, for the audit entry. |

Through `--request` only: `repository` (hostkeeper.release.v1.Repository).

#### `hk repositories remove`

`/hostkeeper.operator.v1.Repositories/Remove`

Removes a repository no package comes from. The registry's generation grows
by one. Writes the audit entry `repository_remove`.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The repository's name. |
| `--reason` | STRING | Why, for the audit entry. |

#### `hk repositories discover`

`/hostkeeper.operator.v1.Repositories/Discover`

Reads what the repository at a URL serves: its descriptor (`<base_uri>/hostkeeper-repository.json`)
and the signing key it names, over HTTPS, and answers the platforms, the channels, the packages
and the key's fingerprints, computed from the keyring, for the operator to confirm. Changes
nothing (docs/deploy/package-repositories.md, "hostkeeper repository format, version 1").

| flag | value | meaning |
|---|---|---|
| `--base-uri` | STRING | The repository's base URL: `https://`, a host, an optional port and path, with no trailing `/`. |
| `--access` | STRING | `public` (or empty), or `hosts` for a private repository. |
| `--credential-file` | STRING | `hosts` only: the credential file on the coordinator's machine, as `Repository.credential_file`. |

#### `hk repositories add`

`/hostkeeper.operator.v1.Repositories/Add`

Adds the repository at a URL, as `Discover` showed it: reads the descriptor and the key again,
refuses when either differs from what the operator confirmed, and then does what `Put` does
with the entry it builds (same refusals, one audit entry `repository_put`, the registry's
generation up by one).

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The name the repository is registered under: 1 to 32 bytes of `[a-z0-9-]`. |
| `--base-uri` | STRING | As `DiscoverRepositoryRequest.base_uri`. |
| `--access` | STRING | As `DiscoverRepositoryRequest.access`. |
| `--credential-file` | STRING | As `DiscoverRepositoryRequest.credential_file`. |
| `--descriptor-sha256` | STRING | The `descriptor_sha256` the operator confirmed. |
| `--fingerprints` | STRING | The `fingerprints` the operator confirmed: exactly the keyring's. Repeat the flag for more. |
| `--reason` | STRING | Why, for the audit entry. |

### `rollouts`

The service `Rollouts` of the package `hostkeeper.operator.v1`. The update plans of the services: an operator moves a plan on, pauses it and resumes it. Each call is an audit entry under the operator token's name.

#### `hk rollouts advance`

`/hostkeeper.operator.v1.Rollouts/Advance`

Enter the next stage of a plan that waits for an operator (`advance = "manual"`): its done
stage, or none yet for a manual first stage.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |
| `--host-group` | STRING | The host group of the plan. From a coordinator that holds host groups: empty for the one group that lists the service (`ambiguous_host_group` when more do). |

#### `hk rollouts pause`

`/hostkeeper.operator.v1.Rollouts/Pause`

Pause a plan: no host that has not started is ordered; the updates already ordered or
running finish and bake. The pause stands across a new desired version, until `Resume`.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |
| `--host-group` | STRING | The host group of the plan. From a coordinator that holds host groups: empty for the plan of every group that lists the service, in one change. |

#### `hk rollouts resume`

`/hostkeeper.operator.v1.Rollouts/Resume`

Resume a paused plan from where it stands.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The service. |
| `--reason` | STRING | Why; 1 to 256 bytes. |
| `--host-group` | STRING | The host group of the plan. From a coordinator that holds host groups: empty for the one group that lists the service (`ambiguous_host_group` when more do). |

### `services`

The service `Services` of the package `hostkeeper.operator.v1`. The services the coordinator manages, which hosts run them, and the operator's holds.

#### `hk services list`

`/hostkeeper.operator.v1.Services/List`

Every service, with its desired version, the roles and hosts it is assigned to, the
operator's assignments, its minimum versions, the hosts ordered to uninstall it and its holds.

#### `hk services assign`

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

#### `hk services unassign`

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

#### `hk services set-min-version`

`/hostkeeper.operator.v1.Services/SetMinVersion`

Set the version below which the rollout gate orders the service to no host of a role, or to
one host: no update, install or automatic rollback. A role names a host group: the minimum of
the package in that group. A host's own minimum stands before its group's.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |

Through `--request` only: `min_version` (hostkeeper.operator.v1.MinVersion).

#### `hk services set-repository`

`/hostkeeper.operator.v1.Services/SetRepository`

Set the repository of the registry the service's package comes from; empty goes back to the
default, `komira`. The coordinator sends each host the repositories of its packages, and a
release names channels of the package's repository. The registry must hold the repository.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The package. |
| `--repository` | STRING | The registry's name of the repository the package comes from. Empty clears the setting: the package goes back to the default repository, `komira`. |
| `--reason` | STRING | Why, 1 to 256 bytes; recorded in the audit entry. |

#### `hk services put`

`/hostkeeper.operator.v1.Services/Put`

Register a package with the registry of packages before any host group lists it, with the
repository it comes from (empty: the default, `komira`), so the console's Packages page lists
it and a host group can be given it. A package a host group lists is registered already:
`Put` of it sets its repository as `SetRepository` does. Registering one already registered
sets its repository again. Writes the audit entry `service_put`.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The package: 1 to 64 bytes of `[a-z0-9-]`. |
| `--repository` | STRING | The registry's name of the repository the package comes from. Empty: the default repository, `komira`. It must be held by the registry. |
| `--reason` | STRING | Why, 1 to 256 bytes; recorded in the audit entry. |

#### `hk services remove`

`/hostkeeper.operator.v1.Services/Remove`

Take a registered package out of the registry of packages. Refused `in_use` while a host group
lists it (the groups it is listed by are the ones `List` names), and `unknown_service` when it
is not registered. Writes the audit entry `service_remove`.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The package. |
| `--reason` | STRING | Why, 1 to 256 bytes; recorded in the audit entry. |

#### `hk services list-trusted-publishers`

`/hostkeeper.operator.v1.Services/ListTrustedPublishers`

The trusted publishers of a package: the GitHub Actions runs (repository, workflow file, ref
and, if named, environment) whose OIDC token may set its desired version. A CI call sets a
package only when its token matches one of the package's entries. A coordinator's first start
with entries imports those of its `--oidc-config` allow-list for each package that has none
held; the state's own entries win from then on. `unknown_service` for a package the
coordinator does not know.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The package. |

#### `hk services put-trusted-publisher`

`/hostkeeper.operator.v1.Services/PutTrustedPublisher`

Add an entry to a package's list (one already held stays as it is). Takes effect at once.
Writes the audit entry `trusted_publisher_put`.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The package. |
| `--repository` | STRING | The repository, `owner/name`. |
| `--workflow` | STRING | The workflow file: `.github/workflows/<name>.yml` (or `.yaml`). |
| `--ref` | STRING | The branch (`main`) or the full ref (`refs/tags/v1`) the run is on. Empty: `main`. |
| `--environment` | STRING | The environment of the job, if the entry names one; empty: not matched. |
| `--reason` | STRING | Why, 1 to 256 bytes; recorded in the audit entry. |

#### `hk services remove-trusted-publisher`

`/hostkeeper.operator.v1.Services/RemoveTrustedPublisher`

Take an entry out of a package's list, `publisher_unknown` when the package holds none like it.
Takes effect at once; leaving the list empty leaves the package with no CI caller, and a
restart does not bring the allow-list's entries back. Writes the audit entry
`trusted_publisher_remove`.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING | The package. |
| `--repository` | STRING | The repository, `owner/name`. |
| `--workflow` | STRING | The workflow file, as the list shows it. |
| `--ref` | STRING | The branch or the full ref. Empty: `main`. |
| `--environment` | STRING | The environment, as the list shows it; empty: none. |
| `--reason` | STRING | Why, 1 to 256 bytes; recorded in the audit entry. |

#### `hk services hold`

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

#### `hk services unhold`

`/hostkeeper.operator.v1.Services/Unhold`

Release a hold: the pairs it held are ordered to the then-desired version at their next
heartbeat.

| flag | value | meaning |
|---|---|---|
| `--service` | STRING |  |
| `--reason` | STRING |  |

Through `--request` only: `scope` (hostkeeper.operator.v1.HoldScope).

### `ssh`

The service `Ssh` of the package `hostkeeper.operator.v1`. Access to the hosts' sshd through the coordinator. Each call is audited under the caller's name; `SignUserCert` as `ssh_cert_signed`, `Revoke` as `ssh_revoke`, `PinHostKeys` as `ssh_hostkey_pinned`, and `Tunnel` as `ssh_tunnel_open` and `ssh_tunnel_close`.

#### `hk ssh sign-user-cert`

`/hostkeeper.operator.v1.Ssh/SignUserCert`

Signs an OpenSSH user certificate for the caller's public key, good for one host and for
`--ssh-cert-ttl-s` seconds. Its principal is `hk-host-<host id>`, never a value of the
request. No certificate is made when the audit entry cannot be written.

| flag | value | meaning |
|---|---|---|
| `--public-key` | STRING | The caller's public key in `authorized_keys` syntax. |
| `--reason` | STRING | Why, at most 200 printable ASCII characters; stored and audited, never in the certificate. |
| `--host-id` | STRING | The host the certificate is for. |

#### `hk ssh host-keys`

`/hostkeeper.operator.v1.Ssh/HostKeys`

The fingerprints of the sshd host keys pinned for a host: the set an operator pinned from
what the host reported in its heartbeat, with `PinHostKeys`. Changes nothing.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING | The host whose pinned fingerprints are read. |

#### `hk ssh pin-host-keys`

`/hostkeeper.operator.v1.Ssh/PinHostKeys`

Pins, as a set, exactly the host keys the host's last heartbeat reported, for a host, and
answers as `HostKeys` does after. Refused with `not_reported` when `fingerprints` are not, as
a set, what that heartbeat reported. Audited as `ssh_hostkey_pinned`. The operators file must
permit the caller the host, as for `SignUserCert`.

| flag | value | meaning |
|---|---|---|
| `--host-id` | STRING | The host whose host keys are pinned. |
| `--fingerprints` | STRING | The fingerprints to pin, at most 8, each `SHA256:` and 43 base64 characters; exactly the set the host's last heartbeat reported. Repeat the flag for more. |

#### `hk ssh revoke`

`/hostkeeper.operator.v1.Ssh/Revoke`

Revokes the certificates of an operator, or one certificate by serial. The serials reach the
hosts in `SshAccess.revoked_serials`, and the operator's open tunnels close.

| flag | value | meaning |
|---|---|---|
| `--operator` | STRING | The operator whose certificates are revoked; empty when `serial` names one. |
| `--serial` | UINT64 | The certificate serial revoked; 0 when `operator` names one. |

### `users`

The service `Users` of the package `hostkeeper.operator.v1`. The people who sign in with a name and a password. A user is an operator, as a token is: there are no roles. Each mutating call is audited under the caller's name, with the user it concerned as the entry's subject: `user_create`, `user_disable`, `user_enable`, `user_delete` and `password_reset`.

#### `hk users list`

`/hostkeeper.operator.v1.Users/List`

Every user, by name. Changes nothing. No password and no hash is ever in the answer.

#### `hk users create`

`/hostkeeper.operator.v1.Users/Create`

Makes a user with a generated temporary password, which the answer carries once: the
coordinator keeps its hash only. The user must choose a new password at the first sign-in,
and the temporary one stops working an hour after it was made.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The new user's name. |

#### `hk users disable`

`/hostkeeper.operator.v1.Users/Disable`

Stops a user signing in, and ends their sessions at once. The last enabled user cannot be
disabled. Disabling a disabled user changes nothing.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The user's name. |

#### `hk users enable`

`/hostkeeper.operator.v1.Users/Enable`

Lets a disabled user sign in again, with the password they had. Enabling an enabled user
changes nothing.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The user's name. |

#### `hk users reset-password`

`/hostkeeper.operator.v1.Users/ResetPassword`

Gives a user a new temporary password, which the answer carries once, makes them choose
another at the next sign-in, and ends their sessions. Refused for the caller's own name: a
person changes their own password on the coordinator's page.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The user's name. |

#### `hk users delete`

`/hostkeeper.operator.v1.Users/Delete`

Removes a user and ends their sessions. The audit entries that name them stay. The last
enabled user cannot be deleted.

| flag | value | meaning |
|---|---|---|
| `--name` | STRING | The user's name. |

### `xcode`

The service `Xcode` of the package `hostkeeper.operator.v1`. The Xcode store. `Remove` is audited as `xcode_remove` (as `xcode_partial_remove` for a partial upload) under the caller's name, with the caller's reason, and the version, build and SHA-256 as its subject; `List` changes nothing and writes no audit entry.

#### `hk xcode list`

`/hostkeeper.operator.v1.Xcode/List`

The archives the store holds, newest release first, the release of the daily list of Xcode
releases that the store does not hold yet (the one the alert `xcode_release_waiting` names),
how many releases the store keeps, the most bytes one upload may be, and the uploads in
progress (`uploads`: the partial uploads the store keeps, oldest first). Changes nothing and
is not audited. A coordinator started without `--xcode-dir` answers `configured` false.

#### `hk xcode remove`

`/hostkeeper.operator.v1.Xcode/Remove`

Removes one archive from the store, by its SHA-256, and writes the audit entry `xcode_remove`
first: when the entry cannot be written nothing is removed. The version the archive held is
free for an upload of another build afterwards. A SHA-256 the store holds no archive of but
holds as a partial upload (one `List` gives in `uploads`) removes that upload instead, with the
audit entry `xcode_partial_remove` written first the same way, and is refused `partial_locked`
while the upload's lock is fresh; a SHA-256 the store holds neither way is `unknown_archive`.

| flag | value | meaning |
|---|---|---|
| `--sha256` | STRING | The SHA-256 of the archive to remove, 64 lower-case hex characters, as `List` gives it. |
| `--reason` | STRING | Why; the audit entry carries it. 1 to 256 bytes once trimmed, with no control character. |

#### `hk xcode add`

Brings an Xcode release into the coordinator's store from the operator's own Mac, macOS only: `hk xcode add` takes the newest Xcode_<version>.xip in ~/Downloads (or the release waiting, downloaded with xcodes), `hk xcode add 27.2` the archive of that version (downloaded with xcodes when ~/Downloads has none), `hk xcode add <path>` that file. It checks the archive's kind and Apple's signature, reads its version and build from the expanded app (keeping a record that saves the next expansion), asks the store first (an archive it holds ends the run with exit 0), uploads with progress, resuming where the store holds a part of the archive (a run that stops says how far it got, and running it again continues), and ends with the Macs that are due the release and when, as the coordinator's Schedule preview answers it. One run at a time per Mac. Not a gateway call. `hk xcode add --help` says each step; `hk xcode-push` is a second name for one release.

#### `hk xcode verify`

Checks an Xcode archive on the operator's own Mac, macOS only, and writes the record of what it found to ~/.hostkeeper/xcode/<sha256>.json, mode 0600, and to stdout: the programs, the archive's kind, Apple's signature and published root, a damaged copy and a file that is no archive, the archive's table of contents, and the expansion with the version and build read from the app. Not a gateway call: no coordinator is asked. Exits 0 when the verdict is pass and 1 otherwise; `hk xcode verify --help` says each check.

| flag | value | meaning |
|---|---|---|
| `--apple-root` | FILE | Apple's root certificate (DER or PEM), in place of fetching it over HTTPS from Apple's certificate authority page. |
