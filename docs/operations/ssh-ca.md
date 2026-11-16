# The SSH user CA, who may sign, and the tunnels

What the coordinator does with `--ssh-ca-secret-file` set (build items C2 and C4 of
[ssh-through-coordinator.md](../design/ssh-through-coordinator.md)): it signs short-lived OpenSSH user
certificates for the operators the owner lists, one host each, revokes them, and carries an operator's ssh
connection to a host's sshd through its private listener (the `Ssh/Tunnel` stream, below). The clients of the
tunnel (`hkctl ssh` and `hkctl ssh-proxy`, C5), the daemon's sshd setup and its report of the host keys (C3,
C3m) and the heartbeat plumbing that tells hosts the CA's public key and the revoked serials (C6) are other
changes. Until C6, an owner who wants a host to trust the CA adds its public key to that host's
`TrustedUserCAKeys` by hand.

The feature is off without `--ssh-ca-secret-file`: the coordinator signs nothing, holds no tunnel and `Ssh`
answers `not_configured`.

## Make the CA key (once, on the owner's machine)

```
hostkeeper-coordinator ssh-ca init --out ./fleet-ca
```

It writes `./fleet-ca/ssh_user_ca` (an unencrypted OpenSSH Ed25519 private key, mode 0600; `ssh-keygen -y -f`
reads it) and prints the public key line and its fingerprint. It never replaces a key that is there. It is
run by hand, once, because N coordinator instances that each made a key at start would hold N different CAs.

Load the file as a Swarm secret, read-only in every instance, and keep the offline copy as
[ca-backup.md](ca-backup.md) keeps the other CA's:

```
docker secret create hostkeeper-ssh-ca ./fleet-ca/ssh_user_ca
```

The coordinator reads it with the rule of every secret file: a regular file owned by its user, mode 0600 or
0400. The key is never in the database and never on one task's disk.

The control-plane class (the NUC) has a CA of its own so that a stolen coordinator is not root there. Run
`ssh-ca init` again into another directory for it, and either give the coordinator that key too
(`--ssh-cp-ca-secret-file`) or leave it out and sign for the control-plane hosts yourself:

```
hostkeeper-coordinator ssh-ca sign --ca-key-file ./cp-ca/ssh_user_ca --public-key-file ~/.ssh/id_ed25519.pub \
    --host-id nuc --operator alice --ttl-s 3600
```

It prints the certificate line (and nothing of the key). Its serial is random in the high half of the numbers,
where the coordinator's sequence does not come.

## Who may sign: the operators file

`--ssh-operators-file` is required with the CA. Default deny: an operator it does not list, or a host outside
the lists of the operator's entry, is refused `not_permitted`, and the refusal is an audit line.

```toml
[[operator]]
name = "alice"                      # the name of the operator's token
classes = ["fleet", "mac-build"]    # host classes (fleet, control-plane) or host groups
hosts = ["node-07"]                 # host ids
```

The file is read at start (a file that does not parse refuses the start) and again at every call, so removing
an entry takes effect with the next call. A file that cannot be read at a call lists no one. An operator the file
lists may also `Revoke`. CI's tokens are never operators.

## The certificate

One host, one principal: `hk-host-<host id>`, never a value of the request. Key id `<operator>/<serial>`.
Valid from a minute before the database's time to `--ssh-cert-ttl-s` (default 3600, 60 to 28800) after it.
The one extension is `permit-pty`: no agent, port or X11 forwarding and no `~/.ssh/rc`. Keys: `ssh-ed25519`,
`ecdsa-sha2-nistp256`, or RSA of at least 3072 bits. The reason (at most 200 printable ASCII characters) is
stored and audited and never in the certificate. The control-plane class is the hosts named by
`--ssh-control-plane-host`; every other host is `fleet`.

## Rotation

Swarm secrets are immutable. Make the next key, create a new secret holding both key files one after the other
(`current` first, `next` second), and update the service to read it. Hosts must trust both before anything is
signed with `next`; then `--ssh-signing-key next` in a later service update. Removing a leaked key is a new
secret without it.

## What the database holds (migration `0019`, `may-run`)

- `ssh_cert`: one row per certificate, written before the certificate exists, in the transaction that writes
  its audit line. If either write fails there is no certificate.
- `ssh_revocation`: one row per revoked serial.
- `ssh_audit`: the audit of the SSH calls (`ssh_cert_signed`, `ssh_revoke`, `ssh_denied`), each row holding
  the SHA-256 of the row before it and of its own content (`prev_hash`, `hash`; the first row's `prev_hash` is
  32 zero bytes), written under one advisory lock, so a deleted or edited row breaks every row after it. That
  is tamper evidence for the database, not proof against a coordinator that holds the key.
- the sequence `ssh_cert_serial`, which gives the serial.

Any instance signs; the times of every row are the database's clock. These tables are not part of `export`
(the file store has none of them) or of `backup`.

To check the chain by hand, recompute each row's hash with the SHA-256 of `prev_hash`, then each of
`at_unix` (decimal), `by_name`, `action`, `subject` (empty when null) and `detail`, each field preceded by its
length as 8 big-endian bytes.

## Tunnels

`Ssh/Tunnel` is a gRPC stream on the private listener (the operator's credential, as every `Ssh` call): the
first frame is `open{host_id, cert_serial}`, the answer's first frame is `opened{login_user}`, and from then
`data` frames of at most 16 KiB carry the bytes of one TCP connection to the host's sshd, both ways, until a
`close{cause}` ends it. `hkctl ssh <host>` signs a certificate, then runs `ssh` with `ProxyCommand` set to
`hkctl ssh-proxy`, which speaks this stream with stdin and stdout as the bytes; any client that speaks the
stream does the same.

**What a tunnel is allowed.** The checks run in this order and a refusal is a status before any frame: the
caller is an operator (not CI); the SSH CA and tunnels are configured (`not_configured`); the instance is not
draining (`draining`); the first frame is an `open` with a host id; the operators file lists the caller
(`not_permitted`, audited); the host is enrolled (`unknown_host`, audited) and the file permits the operator
the host (`not_permitted`, audited); the instance holds fewer than `--ssh-max-tunnels` (`too_many_tunnels`);
then, in one write that also writes the `ssh_tunnel_open` line: the serial is a certificate of this operator
for this host, unexpired and not revoked (`bad_serial`), the host has an address (`no_address`) and was heard
within `--ssh-live-s` (`host_not_live`), and the operator holds fewer than `--ssh-max-tunnels-per-operator`
tunnels over every instance and none to this host already (`too_many_tunnels`); last, the connection to the
host is made within `--ssh-dial-timeout-s` (`dial_failed`, and the row the book made is closed with the cause
`error`). A book that cannot be written is `audit_unavailable` and nothing is opened.

**Liveness and the address.** A host is live when its last heartbeat was within `--ssh-live-s` by the
database's clock. The coordinator never takes an address from a host: the address it dials is the TCP peer
address of the host's last heartbeat, written to `ssh_host` by the instance that served the heartbeat, and
the port is always 22. A heartbeat that arrives on a connection with no address says nothing; a host with
none is refused `no_address`. A proxy that forwards the heartbeat listener's connections (a load balancer,
Envoy) without preserving the client address makes the peer address the proxy's: the coordinator then dials
the proxy's address on port 22, which is refused (`dial_failed`) or reaches another machine. The defence is
the host key, not the address: `ssh` checks the host key it was given before it shows any credential, so a
dial that lands on another machine ends in a host key mismatch and the certificate is never presented. A host
heard from an address other than the one before writes an `ssh_address_changed` line.

**Host keys.** A host's sshd host keys are pinned by an operator, never taken from the host being reached. The
daemon reports the fingerprints of its sshd host keys in its heartbeat; the coordinator holds the last set
reported. `Ssh/PinHostKeys` (`hkctl ssh pin-host-keys`) pins a set of at most 8 fingerprints (`SHA256:` and 43
base64 characters each) and is refused `not_reported` when it is not, as a set, exactly the set of the host's
last heartbeat (so an operator pins what the host says now, a step a person takes, not a script). It needs the
operators file to permit the operator the host, as signing does, and is audited as `ssh_hostkey_pinned`
(a refusal `not_reported` is an `ssh_denied` line). `Ssh/HostKeys` answers the pinned set and a state:
`no_pin` (nothing pinned: the client refuses to connect), `pinned`, or `changed` (the host reports a set that
is not the pinned one; the pin stays what it was until an operator pins the new one, and the first report of
the change writes an `ssh_hostkey_changed` line).

**Limits.**

- Frames carry at most 16 KiB. The coordinator reads from the host only after it has a place to put the frame
  (a queue of 4 frames towards the client), so a client that reads nothing makes the host wait in its own
  socket; nothing is buffered without bound. A frame from the client over 16 KiB ends the tunnel `error`.
- A client `close` ends only the client's input: the write side of the connection to the host is shut down,
  and the host's answer is relayed until the host closes (what `scp` and `rsync` need).
- `--ssh-idle-s` counts bytes in either direction (an ssh `ServerAliveInterval` keeps a quiet tunnel open);
  `--ssh-max-session-s` is the longest a tunnel lives. The private listener sends HTTP/2 PINGs itself, which
  keep the path warm and do not count as bytes; there is no flag for it. Whatever proxy sits in front of the
  private listener must allow a stream idle for `--ssh-idle-s` and open for `--ssh-max-session-s`.
- One tunnel per operator per host, `--ssh-max-tunnels-per-operator` per operator over every instance (counted
  in the database), `--ssh-max-tunnels` per instance (counted by the instance).

**Closing.** Whatever ends a tunnel sends the client `close{cause}` and writes the `ssh_tunnel_close` line with
the cause, the bytes each way and the seconds open. The causes: `eof` (the client's stream ended), `idle`,
`max_session`, `host_closed`, `error` (a stream or socket failed, or the client broke the protocol),
`coordinator_stopping`, and the four the poll gives:

- `token_revoked`: the operator no longer has a token (neither in `--tokens-file` nor among the API tokens);
- `not_permitted`: the operators file no longer lists the operator, no longer permits the host, or the host is
  no longer enrolled;
- `revoked`: the certificate the tunnel was opened with was revoked (`Ssh/Revoke`);
- `instance_lost`: the book no longer holds the tunnel, because another instance closed it as lost.

Each instance polls every `--ssh-revoke-poll-s` (default 10), so a revocation closes the tunnels it concerns
within that and one poll's work. At the same poll it refreshes its tunnels' `last_seen` in the database, and
closes the rows of other instances that have not been refreshed for three polls with `instance_lost`, which
is how the tunnels of an instance that was killed leave the table. A poll that cannot read the book is logged
and tried again; it closes nothing.

**Drain.** When the coordinator takes SIGTERM or SIGINT it stops taking tunnels (`draining`, which the client
may retry once and reach another instance), keeps serving, and lets its tunnels run for up to `--ssh-drain-s`
(default 600; 0 closes them at once); the health listener says `draining` meanwhile. Tunnels that are still
open then are closed with `coordinator_stopping` and the process ends when their lines are written. A transfer
longer than the drain is cut: `rsync` resumes, `scp` does not. **The stop grace period of the container must be
at least `--ssh-drain-s`**, or the runtime kills the process in the middle of the drain and the tunnels left
are closed as `instance_lost` by another instance's poll. `docs/deploy/coordinator/compose.yaml` sets
`stop_grace_period: 11m` (the default drain, 600 s, and a minute to write the closes). A Swarm service whose
spec is kept outside this repository needs the same by hand: `--stop-grace-period 11m` on `docker service
create` or `update`, or `stop_grace_period` in its stack file, and a larger value whenever `--ssh-drain-s` is
raised.

## Flags of the tunnel

Each needs `--ssh-ca-secret-file`.

| Flag | Default | Range | Meaning |
|---|---|---|---|
| `--ssh-live-s` | 120 | 10 to 3600 | how recent a host's heartbeat must be (`host_not_live`) |
| `--ssh-idle-s` | 900 | 10 to 86400 | a tunnel with no byte either way this long is closed `idle` |
| `--ssh-max-session-s` | 28800 | 60 to 86400 | longest a tunnel lives (`max_session`) |
| `--ssh-dial-timeout-s` | 5 | 1 to 60 | the connection to the host's sshd |
| `--ssh-max-tunnels` | 16 | 1 to 1024 | open tunnels of one instance |
| `--ssh-max-tunnels-per-operator` | 4 | 1 to 64 | open tunnels of one operator, over every instance |
| `--ssh-revoke-poll-s` | 10 | 1 to 300 | how often an instance looks for tunnels that must close |
| `--ssh-drain-s` | 600 | 0 to 3600 | how long a stopping instance lets its tunnels run; the stop grace must be at least this |

## What the database holds (migration `0019`, `may-run`)

- `ssh_cert`: one row per certificate, written before the certificate exists, in the transaction that writes
  its audit line. If either write fails there is no certificate.
- `ssh_revocation`: one row per revoked serial.
- `ssh_audit`: the audit of the SSH calls (`ssh_cert_signed`, `ssh_revoke`, `ssh_denied`, and from the
  tunnels `ssh_tunnel_open`, `ssh_tunnel_close`, `ssh_hostkey_pinned`, `ssh_hostkey_changed`,
  `ssh_address_changed`), each row holding the SHA-256 of the row before it and of its own content
  (`prev_hash`, `hash`; the first row's `prev_hash` is 32 zero bytes), written under one advisory lock, so a
  deleted or edited row breaks every row after it. That is tamper evidence for the database, not proof
  against a coordinator that holds the key.
- the sequence `ssh_cert_serial`, which gives the serial.

Any instance signs; the times of every row are the database's clock. These tables are not part of `export`
(the file store has none of them) or of `backup`.

## What the database holds for tunnels (migration `0020`, `may-run`)

- `ssh_host`: per host, the address of its last heartbeat, the time it was heard (only ever raised) and the
  host keys it reported.
- `ssh_host_key_pin`: per host, the pinned fingerprints, who pinned them and when.
- `ssh_tunnel`: one row per open tunnel (operator, host, certificate serial, instance, address, opened and
  last-seen times), deleted when the tunnel closes; a unique index makes it one open tunnel per operator per
  host over every instance. The audit keeps the history.

To check the chain by hand, recompute each row's hash with the SHA-256 of `prev_hash`, then each of
`at_unix` (decimal), `by_name`, `action`, `subject` (empty when null) and `detail`, each field preceded by its
length as 8 big-endian bytes.

## Not in this change

The audit lines are in `ssh_audit` only: `Audit.List` and the audit file do not show them yet. The line for a
caller the coordinator does not know (a failed token) is not written. A host's keys are pinned by an operator
from what its heartbeat reported; keys carried by the enrollment request are not read yet.
