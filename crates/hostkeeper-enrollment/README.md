# hostkeeper-enrollment

Everything about enrollment that is a certificate or a file, as a library: the coordinator's
certificate authority and its durable enrollment store. Why they are shaped this way:
`docs/design/hostkeeper-design.md`, sections 4.4 (the CA), 5.7 (enrollment), 6.1 (the host
lifecycle), 8.8 (enrollment races) and 10 (state). This page states what the code holds. No RPC
is served here and no wire message is used; the coordinator converts its messages to this
crate's plain types. The crate depends on no crate of this workspace;
`crates/xtask/tests/layering` keeps it so.

| module | what it holds |
|---|---|
| `ca::profile` | `check_request(csr_der)`: the request profile. One DER PKCS#10 request of at most 4096 bytes; an uncompressed ECDSA P-256 key; a self-signature that verifies as ECDSA P-256 / SHA-256; a subject of exactly one common name `host:<id>` with a valid host id (a daemon sends `host:pending`). Requested extensions are allowed and never copied. |
| `ca::issue` | `Ca::issue_host` (subject `CN=host:<approved id>`, `clientAuth` only, `CA:FALSE`, exactly 7 days from the CA's clock) and `Ca::issue_server` (`serverAuth` only, the names of `ServerNames` asked for: at least one DNS name, then the IP addresses, at most 90 days). A validity above the cap is refused (`validity_over_cap`), never clamped. `Signer` is the seam a separate signer process would implement; `Ca` is the one implementation. |
| `ca::key` | `Ca::init` makes the CA (a P-256 key and its self-signed certificate, 20 years); `Ca::open` loads it. The key is one PKCS#8 file, mode 0600, of the user the coordinator runs as; a key file that is a symbolic link, another user's, or open to group or others is refused, checked on the opened file. |
| `ca::bundle` | `CaBundle`: the CA certificate and its SHA-256 (the `ca_sha256` a daemon config pins), and `host_certificate`, which reads a certificate a caller presents back as the host and key it names when this CA signed it with the subject `host:<id>`. `PollBundle`: what a poll returns. |
| `enroll` | `EnrollmentStore`: `post` (refused `pending_full` while `max_pending` requests wait, 256 unless `with_max_pending` says otherwise), `approve`, `approve_recorded` (the approval with who made it, when and why, refusing a `duplicate_claim` request unless it replaces, checking and consuming a Mac nonce in the same write, and, with a nonce, taking over the host id of the same Mac's key approved before its current MDM enrollment began), `expire` (every waiting request older than a TTL, at a given time, once), `issue`, `verify_poll`, `deny`, `retire`, `report_live`, `note_heartbeat` (the first heartbeat accepted since the host's approval, written once), `issue_mac_nonce` (for a serial and the request it binds to), and `is_denied`, `is_retiring`, `is_duplicate`, `renew_allowed`, `host_role`, `host_role_for` (only for the key the host id is approved for), `pending`, `expired`, `request`, `recorded_approvals`, `bound_macs`, `allocate_serial`. A new request (not a renewal) is a duplicate claim when another request has its machine id, or when it claims a MAC bound to an enrolled host: a universally administered unicast MAC (data or BMC) claimed by the request a host's key came from, for a host that is not denied. `Claims` reads its MACs (`data_macs`, `bmc`, `all_macs`). |
| `validity` | `UnixClock` and `SystemClock`, the only read of the wall clock in the crate (the wall-clock lint allows this file), and the validity window. |
| `fs` | The `Fs` port every file is written through, `RealFs`, and `FaultFs`, which fails a chosen operation for the crash tests. |
| `names` | `HostId` (a DNS label), `Role`, `ServiceName`, `KeyHash` (compared in constant time where an approval checks it), `MacAddress` (six octets read as `aa:bb:..`, `AA-BB-..` or twelve hex digits, written lower-case and colon-separated). |

## The state files

The store lives in one directory the caller names (the coordinator's `--enrollment-dir`). Every
file is one binary protobuf message of `proto/hostkeeper/enrollment/store/v1/store.proto`
(private to this crate), mode 0600, written whole: temporary file, fsync, rename, directory
fsync. Field 1 of every message is `schema` (1). A reader refuses a newer schema, a missing one
and a state it does not know, and ignores fields it does not know.

| file | message | holds |
|---|---|---|
| `requests/<request id>.pb` | `Request` | the CSR, claims (the BMC MAC among them), image, post time, `renewal_of`, state, the approval (with its approver, time, reason and the SHA-256 of the Mac nonce it consumed), the issuance round and its reserved serial, whether a Mac nonce was presented, and when and under which TTL it expired (the state keeps the waiting state it expired in, so a release before expiry reads it as waiting) |
| `issued/<request id>.pb` | `Issued` | the certificate of the request's current round, its serial, host id and end |
| `hosts/<host id>.pb` | `Host` | the key hash (the request id it came from), machine id, role, services, the `duplicate` mark, the time of the first heartbeat accepted since the approval |
| `deny/<host id>.pb` | `Deny` | retiring, retired, or cleared by a named request |
| `serial.pb` | `SerialCounter` | the next serial (the first is 2; 1 is the CA certificate's) |
| `mac-nonces/<nonce SHA-256>.pb` | `MacNonce` | a Mac nonce by its SHA-256 only: the serial and the request it was issued for (no request on a file written before field 6), when, and the request whose approval consumed it |
| `lock` | | the file every mutation `flock`s, under the store's mutex |

The request id is the SHA-256 of the CSR's SubjectPublicKeyInfo, as 64 lower-case hex digits in
file names. It is computed by the store; no caller supplies one. Because a renewal keeps its key,
it keeps its request id: an accepted `renewal_of` after a certificate was issued starts the next
round of the same request, and issuance is once per request id and round.

The CA's two files are paths the caller names (the coordinator's `--ca-key-file`): the key
(PKCS#8 DER, 0600) and the CA certificate (DER, 0644). The certificate is made once and read
back on every start, since every daemon config pins its SHA-256.

## The refusal mapping

Each error that is a refusal names its wire token with `token()`; a failure of the store or of
signing returns `None` and is not a refusal.

| error | variant | token |
|---|---|---|
| `ProfileError` (in `PostError::Profile`) | `BadCsr(_)` | `bad_csr` |
| `ProfileError` (in `PostError::Profile`) | `KeyNotP256` | `key_not_p256` |
| `PostError` | `RenewalNotAllowed` | `renewal_not_allowed` |
| `PostError` | `PendingFull` | `pending_full` |
| `ApproveError` | `UnknownRequest` | `unknown_request` |
| `ApproveError` | `KeyMismatch` | `key_mismatch` |
| `ApproveError` | `NotPending(_)` (also an expired request) | `not_pending` |
| `ApproveError` | `HostIdTaken` | `host_id_taken` |
| `ApproveError` | `Denied` | `denied` |
| `ApproveError` | `RenewalOfOtherHost` | `renewal_of_other_host` |
| `ApproveError` | `DuplicateClaim` | `duplicate_claim` |
| `ApproveError` | `MacNonce(_)` | `nonce_unknown`, `nonce_expired`, `nonce_used`, `nonce_wrong_serial`, `nonce_wrong_request` |
| `IssueError` | `UnknownRequest` | `unknown_request` |
| `IssueError` | `Denied` | `denied` |
| `IssueError` | `NotApproved(_)` (also an expired request) | none: the poll answers "pending" |
| `PollError` | `UnknownRequest` | `unknown_request` |
| `PollError` | `BadSignature` | `bad_signature` |
| `PollError` | `BadNonce(_)` | `bad_signature` |
| `SignError` | `ValidityOverCap { .. }` | `validity_over_cap` |

## Mac nonces

A Mac nonce is good for one hour from its issue (`MAC_NONCE_VALID_SECS`, by the caller's clock)
and for one approval, of the request it was issued for (`issue_mac_nonce` records the request's
key hash; a nonce file written before that field names none and is judged without it).
`approve_recorded` checks it under the store lock (known, unexpired, unused, issued for the serial
the request claims, issued for this request) and writes the approval with the nonce's
SHA-256 in the request file: that one write is the consumption. The nonce's own file is marked
after it; should that mark not land, the store remembers the consumption in memory and `open`
marks the file. A check that does not end in an approval consumes nothing.

## The poll message

A poll is an ECDSA P-256 / SHA-256 signature (ASN.1 DER), by the request's key, over
`hostkeeper-enroll-poll-v1`, 0x00, the request id as 64 lower-case hex characters, 0x00, and the
nonce (16 to 64 bytes). `poll_message` builds it; `EnrollmentStore::verify_poll` checks it. The
nonces are the coordinator's to choose and remember.

## Backup

The sealed offline backup of the CA key and certificate files is an operator step:
[docs/operations/ca-backup.md](../../docs/operations/ca-backup.md).
