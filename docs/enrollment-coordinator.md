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

The CA is made once. Every daemon config pins the SHA-256 of its certificate (`ca_sha256`); the
start logs it at info as `ca_sha256`. At each start the server certificate is made again when
its file is missing, does not start with a certificate, or has less than a third of its 90 days
left; delete the file to make one sooner (for new `--server-name` values, say). Nothing renews it
while the coordinator runs: restart it within the certificate's validity.

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
| `hosts/<host id>.pb` | an approved host: its key's hash, machine id, role, services, the duplicate mark |
| `deny/<host id>.pb` | a denied host: retiring, retired, or cleared by a later approval |
| `serial.pb` | the next certificate serial |
| `lock` | the lock every change takes |

A request id is the SHA-256 of the request key's SubjectPublicKeyInfo, as 64 lower-case hex
characters. The poll nonces are kept in memory only: after a restart a daemon starts its poll
again with an unsigned one.

## Backup

Back up the CA key file and its `.cert` once, right after the CA is made, sealed and offline:
[docs/operations/ca-backup.md](operations/ca-backup.md). Rotating the CA is not built.

## What a host is told

Once a transport carries the calls, a refusal is a gRPC status whose message is one token of `CoordinatorRefusal`
(`crates/hostkeeper-proto/proto/hostkeeper/agent/v1/agent.proto`):

| token | when |
|---|---|
| `forbidden` | `Heartbeat` or `Renew` with no client certificate, or one that names no host |
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
the coordinator forgets the host, which frees the one live update slot, and marks the deny row
final; from then on its heartbeats are refused before they are read.

## Not here yet

The wire transport of `Enroll`, `Poll` and `Renew` (the coordinator's gRPC serving).
The operator's `Enrollment.List` and `Enrollment.Approve`, the cap on pending requests
(`pending_full`) and the Mac nonce are built separately; until then the pending queue is unbounded
and a request is approved by the store's `approve`. Neither transport carries a machine id in a
heartbeat yet, so the duplicate mark (two live daemons on one host id) is only set when a caller
of `Coordinator::heartbeat_from` passes one.
