# The SSH user CA and who may sign

What the coordinator does with `--ssh-ca-secret-file` set (build item C2 of
[ssh-through-coordinator.md](../design/ssh-through-coordinator.md)): it signs short-lived OpenSSH user
certificates for the operators the owner lists, one host each, and revokes them. Nothing here reaches a
host yet: the daemon's sshd setup (C3, C3m), the tunnel (C4), `hkctl ssh` (C5) and the heartbeat plumbing
that tells hosts the CA's public key and the revoked serials (C6) are other changes. Until C6, an owner who
wants a host to trust the CA adds its public key to that host's `TrustedUserCAKeys` by hand.

The feature is off without `--ssh-ca-secret-file`: the coordinator signs nothing and `Ssh` answers
`not_configured`.

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

## What the database holds (migration `0020`, `may-run`)

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

## Not in this change

The audit lines are in `ssh_audit` only: `Audit.List` and the audit file do not show them yet. The line for a
caller the coordinator does not know (a failed token) is not written. `Ssh.HostKeys` and `Ssh.Tunnel` answer
`not_configured`.
