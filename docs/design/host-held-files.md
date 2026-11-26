# Host-held files: a service's per-host secret and instance file

Status: design. Nothing of it is built. It answers komira-ai/hostkeeper#288, which asks hostkeeper to place a
service's host-held secret file and instance file at enrollment approval and on rotation, over the daemon's mutual TLS channel.
It builds on the host identity ([host-identity.md](host-identity.md); Built: `crates/hostkeeper-daemon/src/host_identity.rs`),
the per-host pushes of the heartbeat answer (Built: `HeartbeatResponse` fields 5 to 9 in
`crates/hostkeeper-proto/proto/hostkeeper/agent/v1/agent.proto`; the SSH access of
[ssh-through-coordinator.md](ssh-through-coordinator.md) is the closest one) and the packages' rule on secrets
([hostgroups.md](hostgroups.md) section 6.5). Facts about this repository were read on `main` at the commit this document lands
on, and facts about jobscheduler on its `main` of the same day. **Built** marks what exists (with its path), **New** what this
design adds. Section 12 lists the decisions the owner is asked to make.

## 1. Summary

The issue's one consumer is js-agent, which needed `/etc/js-agent/token` (a one-time join token) and `/etc/js-agent/instance`
(`PROVIDER:INSTANCE_ID`) on every host. Since the issue was written, both halves of a better answer have merged:

- hostkeeper keeps a copy of the host's own certificate and key where a service may read it, at fixed paths, renewed in place
  (komira-ai/hostkeeper#431, komira-ai/hostkeeper#457; [host-identity.md](host-identity.md), whose section 4 already names komira-ai/hostkeeper#288);
- jobscheduler's lease server accepts that certificate as the host's credential (`--agent-client-ca`), and js-agent presents it
  (`--host-cert`, `--host-key`) and then reads no token and no instance file (komira-ai/jobscheduler#922), on Linux and on a Mac.

So **a host hostkeeper enrolls needs neither file, and no secret has to travel to it for js-agent.** The token and the instance
file stay only for a node hostkeeper does not manage (a node the job service's scaler launches with a token in its launch
data), which hostkeeper never sees.

This design therefore has two parts:

- **Track A (section 4), recommended now:** the small gaps left on the certificate path. These are a PEM copy of hostkeeper's CA
  for a relying server, the operator page that ties the steps together, and the doc lines komira-ai/hostkeeper#288 asked for. There is no new wire,
  store or secret.
- **Track B (sections 5 to 10), held:** the general path the issue describes, specified in full so that it can be built the day
  a service needs a per-host secret that the host certificate cannot replace. No such service exists today (section 3), and
  AGENTS.md B20 asks against building an abstraction with no concrete use. Section 12 asks the owner whether to hold it, build it, or
  close it.

## 2. What the host has today (Built)

| File | Owner and mode | Written by | Who reads it |
|---|---|---|---|
| `/var/lib/hostkeeper/identity/host.crt` | `root:hostkeeper-identity`, 0644 | the daemon, after every enrollment step | a service that speaks for the host |
| `/var/lib/hostkeeper/identity/host.key` | `root:hostkeeper-identity`, 0640 (0600 and group `root` when the group cannot be made) | the daemon, key before certificate | the same |
| `/var/lib/hostkeeper/facts.json` | root, 0644 | the daemon, from `HeartbeatResponse.facts` | any service: host id, group, platform, labels; never a secret |

The certificate (`crates/hostkeeper-enrollment/src/ca/issue.rs`, `host_params`) is an end-entity certificate with subject
`CN=host:<id>`, two subject alternative names (the URI `urn:hostkeeper:host-role:host` and exactly one DNS name, the host id), key
usage `digitalSignature` and extended key usage `clientAuth` alone. It lives 7 days and is renewed at half its life with the same
key. jobscheduler's rule (`crates/js-lease-server/src/host_cert.rs`) is: an end-entity certificate with exactly one DNS SAN, a
valid host id, and the extended key usage `clientAuth` and nothing else; URI SANs and the CN are not read. hostkeeper's
certificate passes it.

js-agent runs as root on both platforms: `js-agent-crun.service` has no `User=`, and the Mac's plist runs it as root. So it reads
the 0640 key without joining the group, and the key passes its rule: after links a regular file, owned by root, mode 0600, 0400,
0640 or 0440. Its update hook's `prepare`, given a flags file that names both paths, checks only those two files and no longer
defers with `join_token_missing` for a token (jobscheduler `crates/js-agent/src/update_hook/step/prepare.rs`, `join_token`).

What the host group's config package carries for js-agent is its flags file, which names the two paths and
`--host-id-from-facts=/var/lib/hostkeeper/facts.json`, and the lease server's CA for the agent to pin (`/etc/js-agent/ca.pem`).
Neither is a secret, and both are the same on every host of the group, so they belong in a package
([hostgroups.md](hostgroups.md) section 6.5).

## 3. What is left of komira-ai/hostkeeper#288, item by item

| komira-ai/hostkeeper#288 asks | Where it stands |
|---|---|
| place `/etc/js-agent/token` at approval | Not needed on an enrolled host: the certificate is the credential from the first heartbeat after approval. |
| place `/etc/js-agent/instance` | Not needed: the instance binds a one-time token, and a certificate login has no token. |
| place them again on rotation | The certificate renews itself every 3.5 days with no operator step; on-demand renewal is komira-ai/hostkeeper#466. |
| never in a package, an order, argv or a log | Holds: the key is made on the host and never leaves it; the coordinator never sees it. |
| the hook keeps deciding readiness | Holds: `prepare` defers until the flags file's two files pass its check. |
| package installation unchanged | Holds. |
| a line in hostgroups.md section 6.5 and its secrets row | Added by this change: the host's credential toward a service is its certificate, in no package. |
| jobscheduler's runbook replaces "Place the files" | Done there already: its section 3A, "A node that hostkeeper manages needs no join token". |

**Other consumers.** We searched this repository's docs and code for a service that needs a per-host secret placed by hand:
`secret`, `token`, `by hand`, `/etc/<service>/` paths. Besides js-agent there is none. The tierstore example in hostgroups.md
section 6.5 reads its secret from the site's secret store by reference, and that store's own credential is per host or per
service. A service that trusts hostkeeper's CA can use the host certificate for that credential too. We did not search the other
services' own repositories.

What would make track B needed is a service whose server cannot be configured to trust hostkeeper's CA (a third-party agent with
its own enrollment token, for example) and that is run on enrolled hosts. That is the consumer section 12 asks the owner about.

## 4. Track A: the certificate path, finished (New)

Three small changes, none of which adds a wire field, a store or a secret.

**A1. A PEM copy of the CA for a relying server.** jobscheduler's `--agent-client-ca` reads a PEM file of CA certificates. The
operator has the enrollment CA's certificate as `ca.key.cert`, which is DER (docs/operations/ca-backup.md), and the hosts have it
as PEM only inside the root-only pki directory of the enroller's store (`crates/hostkeeper-enroll/src/store.rs`,
`gen-<n>/ca.pem`). No document tells the operator to convert it. The CA's certificate is public: every host already receives it
in `PollResponse.ca_bundle_pem`. Three shapes are possible, from least to most new code:

- **a documented command** (recommended): the operator page of A2 gives
  `openssl x509 -inform der -in ca.key.cert -out hostkeeper-ca.pem` and a check that the SHA-256 of `ca.key.cert` is the
  `ca_sha256` the daemons pin. There is no code;
- `Enrollment.GetCa` (operator token, read-only): it answers the CA bundle in PEM and its SHA-256, and
  `hk enrollment get-ca --out ca.pem` writes it. It is an additive RPC, a public interface change;
- the daemon also writes `ca.pem` (0644) into `/var/lib/hostkeeper/identity/`, for a service on the host that must verify a
  peer host's certificate. No service on a host needs that today.

Section 12 recommends the documented command (AGENTS.md B20): one relying server needs the file once, and again only if the
CA changes, which is not built (`crates/hostkeeper-enrollment/src/ca/issue.rs`). `GetCa` waits for a second relying server or
for CA rotation.

**A2. The operator page.** A section of docs/operations/enrollment.md, "A service that joins by the host's certificate", says the
whole path in order:

1. Give the relying server hostkeeper's CA as PEM (A1).
2. Put the two paths in the service's configuration through the host group's config package.
3. Approve the host.
4. Let the service's hook defer until the copy is there.

It also says what to do when a host's certificate predates the DNS name (it renews at the daemon's first step;
host-identity.md section 3) and when the server must stop trusting a host: retire it, and its certificate expires within 7 days
(`HOST_VALIDITY_SECS`); until then the relying server's own refusal stops it, for jobscheduler the host ids of its deny file
(`deny_hosts_file`, which refuses and evicts them; jobscheduler `docs/design/configuration.md`).

**A3. The design lines komira-ai/hostkeeper#288 asked for.** This change adds to hostgroups.md section 6.5 and its secrets row, and to
host-identity.md section 4, the sentence that a host's own credential toward a service is its host certificate, kept in the
identity copy and in no package. These lines describe what is built, so they land now. Track B, if built, adds its own line
there in the pull request that builds it.

A1 as a documented command, A2 and A3 are text, with no tests. If the owner picks `GetCa` instead, its tests are: the RPC
answers the CA the coordinator holds as PEM, with the SHA-256 of the DER; a CI token is refused, as for every `Enrollment` call;
`hk enrollment get-ca --out` refuses an existing file; and the PEM it writes parses back to the DER of `ca.key.cert` (asserted
by equality).

## 5. Track B: what is placed, and from where (New, held)

The rest of this document is the general path, for a service that needs a per-host file the certificate cannot replace. js-agent
is used as the worked example because its two files are the ones komira-ai/hostkeeper#288 names, even though it no longer needs them.

### 5.1 A host file

A **host file** is one file on one host, owned by one service, whose content comes from the coordinator. It is named by three
things: the host id, the service, and the file's name. Its path is always `/etc/<service>/<name>`:

- `<service>` is a service the coordinator has a release of, by the service name rule (1 to 64 bytes of `[a-z0-9-]`,
  `hostkeeper_wire::is_service_name`).
- `<name>` is 1 to 64 bytes of `[a-z0-9._-]`, not starting with `.`.
- There is no free path. The daemon writes nothing outside `/etc/<service>/`, so a caller cannot use this path to replace a
  root-owned file elsewhere (`/etc/sudoers`, a unit file, the daemon's own config).
- The owner is `root:root` (gid 0, `wheel` on a Mac) and the mode is 0600. The directory `/etc/<service>` is made 0700, root's,
  if it is missing. An existing one must already be root's with mode 0700, as js-agent's package makes `/etc/js-agent`. So a
  service named like a system directory (`ssh`, `systemd`, which are 0755) is refused, not written into.
- These rules guard against a mistake, not against an operator: an operator token can already order a release whose hook runs
  as root on the host. They keep a typo, or a service named like a system directory, from replacing a file the system owns.

komira-ai/hostkeeper#288 asks for the owner "taken from the service's manifest". The manifest (`Manifest`, `release.proto`) has no owner field, and
the only consumer runs as root and requires a root-owned file. A non-root owner would need a manifest field and an account
lookup the daemon does not have. So v1 has no owner field; section 12 lists this decision.

A host file holds at most 4096 bytes. A host has at most 8 host files. Both limits keep a heartbeat answer far below its 1 MiB
gRPC cap (`GRPC_REPLY_MAX`) and fit a token, a short key or an instance line.

### 5.2 Where the content comes from

Two sources are possible. The design builds the first and describes the second only so the owner can reject it with the facts
in hand.

**Source 1, operator-supplied (built by track B).** An operator, or CI acting for one, puts the content:

```
js join-token mint --server https://js.example.com:8443 --token-file ./client-token \
    --provider farm --instance-id node-5 --out ./node-5.token
hk host-files put --host-id node-5 --service js-agent --name token \
    --content-file ./node-5.token --reason "join node-5"
printf 'farm:node-5\n' | hk host-files put --host-id node-5 --service js-agent --name instance \
    --content-stdin --reason "join node-5"
```

`content` is a `(hostkeeper.secret)` field, so the generated `hk` never takes it as a flag. It is read from `--content-file`
or `--content-stdin` (docs/cli.md) and never printed. That keeps it out of every argv, as komira-ai/hostkeeper#288 requires.

**One canonical form.** The coordinator is the one place that normalizes content. At `Put` it removes every trailing newline
and refuses what is left if it is empty (`empty`). The canonical content has no trailing newline. Its digest, the SHA-256 of
the canonical bytes, is the digest every audit line, answer, order and report uses. The CLI's own drop of one newline from
`--content-file` changes nothing, since the coordinator strips the rest. The daemon writes the canonical bytes and exactly one
newline (section 7.2).

**Source 2, coordinator-minted (not built; for the owner's decision).** The coordinator would call the job service itself at
approval:

- The call: `POST https://<js server>/v1/agent/join-tokens`, `Authorization: Bearer <client token>`, body
  `{"provider": "<pool>", "instance_id": "<host id>", "ttl_seconds": 1800}`.
- The answer: `201 {"token": "<64 hex digits>", "expires_at_ms": <ms>}` with `Cache-Control: no-store`. The token is shown once,
  and the server keeps only its digest (jobscheduler `crates/js-lease-server/src/join_api.rs`).
- Timing: mint when the host's first heartbeat after approval arrives, put the token as the host file `token` and the instance as
  `instance`, and send both in that answer. A token unused within its TTL (30 minutes by default, at most 24 hours) is minted
  again. A second mint for the same instance replaces the unused token.

It is not recommended, for five reasons:

1. The mint route takes the job service's client token, the same bearer that takes leases ("whoever may take leases ... is who
   may admit the machines"). The coordinator would hold a credential that can run jobs on the farm, which it does not hold today.
2. It is per-service logic in the coordinator (a jobscheduler client, its pools, its error codes). hostkeeper keeps a
   service's logic in that service's hook (host-updates.md), and a coordinator that knows one job service's API is the
   speculative port AGENTS.md B20 asks against.
3. It is a second secret store: the client token is a new coordinator secret file, in every instance.
4. A lost answer is unrecoverable: a token swapped for a host credential is gone, and the host id stays bound (`host-bound`), so
   the host needs a new instance id. Source 1 has the same failure, but there an operator sees it and acts.
5. Every enrolled host can use the certificate instead (section 2).

If the owner wants it anyway, it is one more producer of the same host file. The coordinator puts the minted content through the
same store write as source 1, audited as `host_file_put` by `minted:js-agent`, and nothing on the wire or the host changes.

### 5.3 At approval and on rotation

- **At approval.** A host file may be put before the host is approved, for a host id that an inventory record names or that a
  is enrolled. It is stored and waits. The first heartbeat after the approval (`first_heartbeat_unix`, Built) is
  the first one that can be answered with it, so the file reaches the host at approval with no new field on `ApproveRequest` and
  no change to the inventory record's auto-approval. A put for a host id that is neither enrolled nor named by a record is
  refused `host_unknown`, and one for a service the coordinator has no release of is refused `service_unknown`.
- **Placing twice changes nothing.** A put of the same content (the same digest) is answered `unchanged`, writes no audit
  line and sends nothing, while the host's last report holds that digest as `placed`, or while that content is still waiting
  to be sent. Otherwise it is a new put: if the host reported the file missing, or held other content after delivery (deleted
  by hand, a host imaged again under the same id), the same content is stored and sent again. The coordinator kept only the
  digest, so the operator gives the content again.
- **Rotation.** One rule, the daemon's: **it never replaces a file whose content differs from the order's unless the order says
  `rotate`** (section 7.2). The coordinator applies the same rule before anything is sent:
  - a put whose content differs from what the host's last report holds is refused `held_differs` unless it says `rotate: true`;
  - `rotate: true` is allowed on a first put, which is how a hand-placed file (the case komira-ai/hostkeeper#288 starts from) is taken over;
  - a put that changes content still waiting to be sent, before any report holds it, replaces the waiting content without
    `rotate`, since the host has not acted on it.

  So a file an operator edited by hand on the host is never replaced silently: the daemon reports `differs`, and only a put that
  says `rotate` replaces it.
- **Removal.** `hk host-files remove` drops the coordinator's record and its content. It does not delete the file on the host:
  deleting a service's file is that service's uninstall, and a delete over the wire is a second, destructive verb that no
  consumer has asked for. Retiring a host drops all its records.

## 6. The coordinator (New, held)

### 6.1 The store, and how long a secret is kept

The coordinator holds secrets only as files outside its store: the CA key (`--ca-key-file`), the SSH CA secret file, the token
file and the database DSN. Nothing in Postgres or the ops file holds a secret's plaintext or sealed value today (the MDM sealed
store of komira-ai/hostkeeper#324 was removed with MDM).
Postgres is the one source of truth for every instance ([coordinator-multi-instance.md](coordinator-multi-instance.md)), so a
host file must live there to be sent by whichever instance answers the heartbeat.

The record, a new table `host_files` (and a file-store equivalent beside the ops file for a coordinator without `--database`):

| Column | Holds |
|---|---|
| `host_id`, `service`, `name` | the key |
| `sha256` | the SHA-256 of the content, as hex |
| `generation` | the audit sequence number of the put that set it |
| `rotate` | whether that put asked for a rotation |
| `sealed` | the content sealed with the coordinator's host-file key (below); emptied once the host reports it holds the `sha256` |
| `held_sha256`, `state`, `reported_unix` | the host's last report (section 7.3) |

The content is sealed with XChaCha20-Poly1305 under a 256-bit key read from `--host-files-key-file`. That is a secret file of the
coordinator's user, mode 0600 or 0400, read with `hostkeeper_wire::secret::read_secret` like `--database-dsn-file`. Under Swarm
it is a stack secret, identical in every instance by construction. The associated data is `host_id`, `service`, `name` and
`generation`, so a sealed value copied to another row fails to open. Without the flag the `HostFiles` service answers
`not_configured` and the feature is not advertised. The coordinator runs exactly as today.

**Rotating the key.** The key file holds one or two keys, one per line, like the SSH CA secret during its rotation. The coordinator
seals with the first and opens with either. To rotate, put the new key first and the old one second, restart every instance, then
remove the old line once `hk host-files list` shows no content waiting that predates the change. Content that was sealed under a
key no longer in the file cannot be opened. It is reported `sealed_unreadable` in `List`, is not sent, and is given again by a
put.

**The content is kept only until the host has it.** When a heartbeat reports the file held with the record's `sha256`, the
coordinator empties `sealed` in the same write that records the report. From then on it keeps the digest, not the secret. A
database backup taken after delivery holds no secret. One taken between the put and the delivery holds it sealed, under a key
the backup does not hold.

### 6.2 The operator API

A new service, `hostkeeper.operator.v1.HostFiles`, operator token only (a CI token is refused, as for `Enrollment`):

- `Put(host_id, service, name, content [(hostkeeper.secret)], rotate, reason)` answers `{generation, sha256, outcome}`, where the
  outcome is `stored`, `unchanged` or `rotation_stored`. Refusals: `host_unknown`, `service_unknown`, `name_invalid`, `empty`,
  `too_large`, `too_many_files`, `held_differs`, `low_entropy` (decision 7), `not_configured`, `audit_unavailable`,
  `store_failed`.
- `List(host_id?)` answers each record's key, `sha256`, `generation`, whether content is still waiting, and the host's last
  state. It never answers content: no call does.
- `Remove(host_id, service, name, reason)`.

The generated `hk host-files put|list|remove` follows. The console gets no page in v1; `hk host-files list` and the host's audit
are where an operator looks.

### 6.3 The audit, and what no line holds

Every put, rotation and removal is audited, and refused `audit_unavailable` when its line cannot be written (as for every
mutating call). The line's `reason` is, for example:

`service js-agent; file /etc/js-agent/token; sha256 9f2c…e1; generation 4412; join node-5`

The report that the host placed it is audited `host_file_placed` with the same file, digest and generation, and a refusal on the
host is audited `host_file_refused` with its state. No audit line, log line, error, `Debug` output or answer holds the content:

- the content's type has no `Display`, and its `Debug` prints `content(withheld)`;
- errors name the host, the file and the step, never a byte of the content;
- the key file's errors name the flag, never its path or bytes.

A SHA-256 in an audit line reveals nothing of a 256-bit random token. For a low-entropy secret (a short password) it would allow
an offline guess, so section 12 asks whether v1 refuses (`low_entropy`) canonical content under 16 bytes unless it is an
instance line: one line matching `^[a-z0-9-]+:[A-Za-z0-9._-]+$`, `PROVIDER:INSTANCE_ID`, which is not secret. The issue asked
for the plain digest.

### 6.4 The heartbeat answer

The coordinator answers `host_files` (section 7.1) to a daemon that announced `host-files-v1`, over gRPC, while the host is not
retiring. Each file whose record has content and whose last report does not hold its `sha256` is sent in every answer until a
report holds it or refuses it. This is the SSH access's rule (`ssh_access_for`, `crates/hostkeeper-coordinator/src/ssh_heartbeat.rs`):
whatever instance answers sends the same thing, and a restart costs one resend.

## 7. The wire and the daemon (New, held)

### 7.1 Additive fields (AGENTS.md B8)

In `agent.proto`, every field optional and every number new:

```proto
// HeartbeatResponse
optional HostFiles host_files = 10;   // sent to an agent that announced `host-files-v1`, until it reports each file held

message HostFiles {
  repeated HostFile files = 1;        // at most 8
}
message HostFile {
  string service = 1;                 // the directory: /etc/<service>
  string name = 2;                    // the file: [a-z0-9._-], 1 to 64 bytes, not starting with "."
  bytes content = 3 [(hostkeeper.secret) = true];  // at most 4096 bytes
  string sha256 = 4;                  // SHA-256 of content, hex; content is canonical: not empty, no trailing newline
  uint64 generation = 5;
  bool rotate = 6;                    // replace a file whose content differs from the last one placed
}

// HeartbeatRequest
optional HostFilesReport host_files = 21;

message HostFilesReport {
  repeated HostFileState files = 1;   // one per file the coordinator sent or the daemon placed; at most 8
}
message HostFileState {
  string service = 1;
  string name = 2;
  string held_sha256 = 3;             // what the file holds now; empty when there is none
  uint64 generation = 4;              // the last generation the daemon acted on
  HostFileOutcome outcome = 5;
  string detail = 6;                  // at most 128 bytes; never content
}
enum HostFileOutcome {
  HOST_FILE_OUTCOME_UNSPECIFIED = 0;
  HOST_FILE_OUTCOME_PLACED = 1;       // written, or already held with the same content
  HOST_FILE_OUTCOME_DIFFERS = 2;      // a different file is there and the order did not say rotate
  HOST_FILE_OUTCOME_NOT_A_FILE = 3;   // the path or its directory is a link or not a regular file
  HOST_FILE_OUTCOME_WRITE_FAILED = 4;
  HOST_FILE_OUTCOME_BAD_ORDER = 5;    // a name, size or digest that breaks the rules above
}
```

The report holds digests and outcomes only. At its largest (8 files, each with a 64-byte service, a 64-byte name, a 64-digit
digest and a 128-byte detail) it is about 2.6 KiB, against the 16 KiB request cap (`MAX_HEARTBEAT_BYTES`). It is counted and
never dropped, as the SSH report is (`crates/hostkeeper-link/src/beat.rs`; B5 checks this before relying on it).

**The feature name.** `host-files-v1` is added to the coordinator's `FEATURES` and to `OFFERED`, offered only while
`--host-files-key-file` is given. A daemon with every port already names more than 16 features, so it names `host-files-v1`
only after a reply advertised it (the `ssh-v1` rule, `MAX_FEATURES_BEFORE_SSH`). The name joins the pinned list in
`crates/hostkeeper-proto/tests/features.rs`.

**The JSON route** carries no host files, as it carries no per-host material. A daemon stuck on JSON never gets them, and the
service's hook keeps deferring, which is the visible symptom (section 9).

### 7.2 Writing the file

A new sink, `HostFilesSink`, in `Ports`, on Linux (systemd and OpenRC) and on a Mac. It applies a `HostFiles` only when the
reply advertised `host-files-v1`, and says nothing back: the next report does. For each file, in order:

1. Check the order: service and name grammar, size, content not empty and not ending in a newline, and `sha256` equal to the
   SHA-256 of `content`. A failure reports `bad_order`.
2. Open the base directory: `/etc` on Linux, `/private/etc` on a Mac (section 8). Then open `<service>` below it with
   `O_NOFOLLOW | O_DIRECTORY`. If `<service>` is missing, make it 0700, root's. If it is a
   link, not a directory, or not root's with mode 0700, report `not_a_file` and write nothing.
3. Read the target with `O_NOFOLLOW`. Its digest is the SHA-256 of its bytes with exactly one trailing newline removed, if it
   has one. If it is absent, write it. If it holds the same content, set its owner and mode to
   `root:root` 0600 if they differ (a fix, never a content change) and report `placed`. If it holds other content, replace it
   only when `rotate` is true; otherwise report `differs` and leave it. If it is a link or not a regular file, report
   `not_a_file` and leave it.
4. Write as `host_identity::replace` does (Built): a temporary `.<name>.tmp` made with `O_EXCL | O_NOFOLLOW` and mode 0600,
   `fchown` and `fchmod` before any byte, the content and one newline, `fsync`, `renameat` over the
   target, `fsync` of the directory. A crash leaves the old file or the new one, never half of one, and the next step removes
   a temporary left behind. A failure of any of these calls reports `write_failed`, naming the call.

The newline rule exists because `--content-file` drops one trailing newline and the files komira-ai/hostkeeper#288 names are one line each. One
digest rule follows from it, used everywhere: a file's digest is the SHA-256 of its bytes with one trailing newline removed.
The coordinator records that digest of the content it was given, the daemon compares the file on disk by it in step 3, and
`held_sha256` reports it. A file whose content must not end in a newline cannot be a host file in v1; no consumer needs one.

The daemon keeps, in its state directory (`host-files.json`, root's, 0600), the service, name and last generation of each file
it was sent, never the content. That list is what it reports on: after a restart it still reports every file it was sent, by
reading each from disk, so the coordinator sees a file deleted by hand as `held_sha256` empty. It drops the order from memory once the write returns. No log line, error or report holds content. Its log
lines are like `host file /etc/js-agent/token placed, sha256 9f2c…e1, generation 4412`.

### 7.3 Readiness is the service's

hostkeeper adds no ordering between placing a host file and installing a package. A service whose hook needs a file defers in
`prepare` until the file is there, as js-agent's does with `join_token_missing`. hostkeeper retries a deferred install on its
backoff and holds no slot for it (host-updates.md section 3.8, `Deferred`). A file that arrives while the hook waits is picked up at the next
try. The package, its installer and its system packages (`crun`, `uidmap`) are unchanged.

## 8. Platforms

- **Linux (systemd, OpenRC).** The daemon runs as root and writes `/etc/<service>/`.
- **macOS.** The daemon runs as root under launchd. `/etc` is a link to `/private/etc` made by the system, so the base
  directory on a Mac is `/private/etc`, opened as it is. `O_NOFOLLOW` applies to `<service>` and the file below it, never to
  the base. The owner is uid 0 and gid 0 (`wheel`).
  `mac_files::replace` (Built) hard-wires 0644 and cannot be used. The writer of section 7.2 is the one used on both platforms.

## 9. Failure modes

| What happens | What is seen | What recovers it |
|---|---|---|
| The heartbeat answer with the file is lost | the next answer sends it again | nothing to do: it is sent until reported |
| The daemon crashes mid-write | the old file or none, and a `.tmp` left | the next step removes the `.tmp`; the next answer resends |
| The coordinator restarts or another instance answers | the record and its sealed content are in Postgres | nothing to do |
| The key file is missing after a restart | `HostFiles` answers `not_configured`; content waiting is not sent | restore the key; content already delivered needs no key |
| Content was sealed under a key no longer in the key file | `List` shows `sealed_unreadable`; it is not sent | a put of the content again |
| A delivered file is deleted on the host, or the host is imaged again under its id | the report holds `held_sha256` empty; the coordinator has only the digest | a put of the same content, which is stored and sent again (section 5.3) |
| A file on the host differs (edited by hand) | report `differs`, audited `host_file_refused`; `hk host-files list` shows it | `put --rotate`, or fix the host's file |
| `/etc/<service>` or the file is a link, or the directory is not root's 0700 | report `not_a_file` | an operator fixes the path, or the put names the right service |
| The order's digest does not match its content | report `bad_order` | a coordinator bug; nothing is written |
| The daemon is on the JSON route | no file; the hook defers | the gRPC route returning |
| An older daemon | it does not announce `host-files-v1`; nothing is sent | updating the daemon |
| An older coordinator | it advertises nothing; the daemon never names the feature | updating the coordinator |
| A one-time token expires before the service uses it | the service's own refusal (for js-agent, `join-token-expired`) | a new mint and `put --rotate` |
| A one-time token was used, then the agent lost its credential | the job service refuses the host id (`host-bound`) | a new instance id, as in jobscheduler's runbook; out of hostkeeper's hands |
| The host is retired | records dropped; the coordinator stops sending | none |

## 10. Tests (for track B)

All hermetic: the daemon's writer runs against a scratch directory standing in for `/etc`, with the owner set to the test
process's own uid and gid, as `host_identity`'s tests do. The coordinator runs in process against the store fakes and, in the
Postgres job, a real database. Nothing reads a clock: deadlines come from the injected clock.

- **Proto** (`crates/hostkeeper-proto/tests/additions_host_files.rs`): the messages are absent from the baseline. A round trip
  keeps every field. The field numbers are pinned. A reader built from the baseline keeps the new fields as unknown. `content`
  carries `(hostkeeper.secret)`.
- **Two-way compatibility** (`crates/hostkeeper-coordinator/tests/host_files_wire.rs`, beside `ssh_wire.rs`, against the
  previous release's `.proto` files copied under `tests/fixtures/<release>/proto`): the previous release reads a heartbeat answer
  with `host_files` and a request with the report, and keeps the rest; this build reads the previous release's messages with
  the fields absent.
- **Daemon writer** (`crates/hostkeeper-daemon/src/host_files_tests.rs`), each asserting the file's bytes, owner, mode, the
  directory's mode, and the exact report:
  - absent → placed;
  - same content → placed with no write, the inode unchanged;
  - same content with mode 0644 → mode fixed to 0600, content untouched;
  - other content, no rotate → `differs`, file unchanged;
  - other content, rotate → replaced;
  - the target a link, the directory a link, the target a directory, the directory mode 0755 or another owner → `not_a_file`,
    nothing followed or written;
  - a digest that does not match, a name with `/`, `..` or a leading `.`, 4097 bytes, a ninth file → `bad_order`;
  - a crash at each step of the write (the `WriteStep` hook of `files.rs`) → the old file or none, and the `.tmp` gone after the
    next step;
  - a file on disk with one trailing newline, with none, and with two → the first two match the canonical content; the third
    reports `differs`;
  - `/etc/<service>` missing → made 0700, root's; a failure of each call of the write (`openat`, `fchown`, `write`, `fsync`,
    `renameat`, through the writer's port) → `write_failed` naming it, and the old file kept;
  - after a restart with `host-files.json` naming a file deleted by hand → reported with `held_sha256` empty.
- **No content anywhere** (exact equality, AGENTS.md T13): the daemon's log lines, its `Debug` of an order, its errors and its
  report for a run with content `secret-bytes-0123456789abcdef` are asserted whole, and none contains that text. The same holds
  on the coordinator for the audit line, the `List` answer, every refusal and the `Debug` of a `Put` request.
- **Coordinator** (`crates/hostkeeper-coordinator/src/host_files_tests.rs`):
  - put → sent in each answer until a report holds the digest, then not sent and the sealed column empty;
  - content `x`, `x\n` and `x\n\n` → the same canonical content and digest; content of only newlines → `empty`;
  - a second put of the same content → `unchanged`, no audit line; the same content after the host reported the file missing →
    stored and sent again;
  - other content without rotate → `held_differs`; `rotate` on a first put → stored; a change of content still waiting →
    replaced without `rotate`;
  - a service with no release → `service_unknown`; with decision 7, a 15-byte secret → `low_entropy` and `farm:node-5` → stored;
  - a key file with two keys → content sealed under the second opens; content sealed under a removed key → `sealed_unreadable`,
    not sent;
  - with rotate → sent with `rotate`;
  - a host not enrolled and not named by a record → `host_unknown`;
  - a CI token → refused;
  - no key file → `not_configured` and no `host-files-v1` offered;
  - sealed content moved to another row → fails to open and is not sent;
  - a retiring host → nothing sent;
  - an audit failure → the put not made.
- **Feature negotiation** (`crates/hostkeeper-link/tests/heartbeat/host_files.rs`): the daemon's shipped `Heartbeater` names
  `host-files-v1` only after a reply advertised it. It never names more than 16 features before that. It applies an answer's
  files only when advertised. The report of the next heartbeat carries the outcome.
- **End to end** (`crates/hostkeeper-coordinator/src/grpc/tests/host_files_e2e.rs`, the shape of `ssh_offer.rs`): from the first
  heartbeat after approval, through the real `Coordinator.Heartbeat`, to the file placed in the scratch base directory and the record's
  content emptied.
- **As root** (`crates/hostkeeper-privileged`, feature `privileged`): the writer on a real `/etc/<service>` under systemd,
  OpenRC and launchd, the last on a Mac's `/private/etc` in `release.yml`'s `macos-device` job. The file is `root:root` 0600 and
  the directory 0700. These tests count toward no coverage gate.

**Why this list is complete.** What the daemon does depends on seven inputs:
- whether the order is valid;
- the directory (missing, root's 0700, another mode or owner, a link);
- what is at the path (none, a regular file, a link, something else);
- whether the content matches, by the digest rule;
- whether the order says rotate;
- which call of the write fails, or where a crash interrupts;
- whether the daemon restarted since it was sent the file.

Each value of each input is a case in the writer's list. The inputs act one after another (an invalid order or a bad directory
stops before the path is read), so a case per value covers every outcome without the full cross product.

The coordinator's outcome depends on:
- the key file (none, one key, two keys, a removed key);
- the host's state (unknown, named by a record, enrolled, retiring);
- the service (with or without a release);
- the content (canonical, with newlines, empty, short);
- what the last report holds (nothing, the digest, other content, the file missing);
- whether content is waiting;
- the rotate flag;
- the caller's token.

Each value is a case. The secrecy claims are asserted on every output channel: log, error, `Debug`, report,
audit and answer.

## 11. Build plan

Each step is one pull request. Interfaces land before the code that uses them (AGENTS.md P7).

**Track A (recommended now):**

1. **A1 and A2, the operator page** in docs/operations/enrollment.md: the CA as PEM by the documented command, and the whole
   path of section 4, with a cross-reference from host-identity.md. Text only. If the owner picks `GetCa` (decision 3), it is
   a pull request before this one, with the tests of section 4.

(A3, the hostgroups.md and host-identity.md lines, lands with this design.)

**Track B (only if the owner decides to build it; section 12):**

1. **B1, the protos.** The messages and fields of section 7.1, the feature name in the pinned list, the additions test and the
   two-way test. No behaviour.
2. **B2, the store.** The `host_files` table (one additive migration), the file-store equivalent, sealing under
   `--host-files-key-file` (one or two keys), and the store tests. It adds the `chacha20poly1305` crate (RustCrypto, MIT or
   Apache-2.0) once in the root `Cargo.toml` (AGENTS.md R5, R6).
3. **B3, the operator API.** `HostFiles.Put`, `List` and `Remove`, the generated `hk host-files`, the audit lines, and the
   no-content tests.
4. **B4, the coordinator's heartbeat.** `host-files-v1` in `FEATURES` and `OFFERED`, sending until reported, emptying the sealed
   content, and the coordinator tests.
5. **B5, the daemon.** `HostFilesSink`, the writer, `host-files.json`, the report, the link and end-to-end tests, and the
   privileged tests.
6. **B6, the docs.** An operations section for host files, the track B line in hostgroups.md section 6.5, and the
   jobscheduler runbook's step for a host that needs one.

## 12. Decisions for the owner

1. **Is komira-ai/hostkeeper#288 answered for js-agent by the host certificate?** Recommended: yes. Close komira-ai/hostkeeper#288 for js-agent, linking this document.
   An enrolled host, Linux or Mac, needs no token or instance file. The token path stays only for hosts hostkeeper never sees.
2. **Track B: hold, build, or drop?** Recommended: hold it as designed, with no code, until a service on enrolled hosts needs a
   per-host secret that its server cannot replace with the host certificate (AGENTS.md B20). Building it now adds a secret
   store, a key file and a wire field that nothing uses.
3. **A1's shape: a documented `openssl` command** (recommended), `Enrollment.GetCa`, or a `ca.pem` in the identity directory.
   The command needs no code, and one relying server needs the file once. `GetCa` is an additive operator RPC, worth it once a
   second relying server or CA rotation exists. The third writes a file no service on a host reads today.

If track B is built:

4. **Source: operator-supplied only** (recommended), not coordinator-minted (section 5.2). Minting would give the coordinator a
   credential that takes leases on the job service, plus per-service logic.
5. **The owner of a host file is `root:root` 0600, fixed,** not a manifest field (recommended), until a service that runs as
   another account needs one. komira-ai/hostkeeper#288 asked for the owner from the manifest, and the manifest has no such field.
6. **The coordinator keeps the content sealed only until the host has it** (recommended), under a key file. The alternatives are
   plaintext in Postgres, or keeping nothing at rest; the second fails with more than one coordinator instance.
7. **Low-entropy content.** The audit's plain SHA-256 is what komira-ai/hostkeeper#288 asked for. Should v1 refuse (`low_entropy`) content under 16 bytes that is
   not an instance line, so a short password is never digested where an audit reader can guess it? Recommended: yes.
8. **No delete over the wire** (recommended): removal on the coordinator does not delete the file on the host.

## 13. Decided here

Choices this design makes that follow from the issue, the built code or the rules, recorded so a reviewer can check them:

- **The path is `/etc/<service>/<name>`, never free.** A free path in an order would let an operator call replace any root
  file. The files komira-ai/hostkeeper#288 names are of this form.
- **Content is put by its own call, not by a field of `ApproveRequest`.** One way to do it serves an operator's approval and an
  inventory record's auto-approval alike, and the first heartbeat after approval is already the delivery point.
- **Sent until reported, like SSH access.** This is the existing shape for per-host material, it is the same from every
  coordinator instance, and it needs no delivery queue.
- **gRPC only.** Every per-host push already is. The JSON route is the frozen route of an older release.
- **The daemon fixes a wrong mode on a matching file** but never replaces differing content without `rotate`. A wrong mode
  is hostkeeper's to correct, and a different content is someone's decision.
