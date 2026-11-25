# The daemon's sshd setup (Linux)

What a Linux host's daemon does with the SSH access the coordinator sends (`ssh-v1`,
`docs/design/ssh-through-coordinator.md` sections 3 and 6). Nothing in it runs until the coordinator
sends a record (`HeartbeatResponse.ssh`) in a reply that advertises `ssh-v1`. A daemon names `ssh-v1`
only once a reply advertised it; a coordinator that holds an SSH user CA (`--ssh-ca-secret-file`) advertises
it in its answer to every gRPC heartbeat that announces features, named or not, so a daemon names it at its
next heartbeat and is sent the record in that answer. A coordinator without the CA offers it to none and
sends no record. A Mac's daemon announces `ssh-v1` too and runs the same steps over Remote Login; what is different there is in `docs/macos.md`, "Remote Login for ssh through the coordinator", and adds the states `no_include` and `blocked_by_access_group`.

## What the daemon writes

Only these files, by their fixed names; no flag and no per-host file:

| File | Holds |
|---|---|
| `/etc/ssh/sshd_config.d/10-hostkeeper.conf` | `TrustedUserCAKeys`, `AuthorizedPrincipalsFile` (`principals.%u`) and `RevokedKeys`; with `password_auth = off` also `PasswordAuthentication no` and `KbdInteractiveAuthentication no`. No `Match` block. |
| `/etc/ssh/hostkeeper/user_ca.pub` | the record's CA public keys, one to two, `type base64` per line |
| `/etc/ssh/hostkeeper/principals.<login user>` | the record's principal (`hk-host-<host id>`); the principals file of any other account in that directory is removed |
| `/etc/ssh/hostkeeper/revoked` | the record's revoked serials as a binary key revocation list made with `ssh-keygen -k` (an empty file for none) |

Files are written through a temporary file and a rename, and only when their bytes differ, so a
record applied again writes nothing and reloads nothing. A file of the setup that is changed or
removed by hand is put back at the next heartbeat. The record is checked first: a login user or
principal outside `[A-Za-z0-9._-]`, a CA key that is not `type base64` of a known type, or more than
two keys is refused as `config_invalid` before anything is written.

## The lockout rule, as built

The daemon adds trust and removes none. It starts a stopped `ssh.service` or `sshd.service` and
enables a disabled one (it never stops, restarts, masks or unmasks one, and under an active
`ssh.socket` it changes no unit). It never writes `authorized_keys`, `Port`, `ListenAddress`,
`AllowUsers`, `AllowGroups`, `DenyUsers`, `DenyGroups`, `PermitRootLogin`, `PubkeyAuthentication`
or `AuthenticationMethods`, and never edits `sshd_config` or any other file of sshd's.

1. Before any change it reads `sshd -T -C user=<u>,host=localhost,addr=127.0.0.1` for the login user
   and for every account whose home has a non-empty `~/.ssh/authorized_keys`.
2. It stops with `blocked_by_allow_list` (the keyword in `detail`) when the allow or deny lists
   exclude the login user, and with `cert_login_not_possible` when `pubkeyauthentication` is not
   `yes`, when `authenticationmethods` has no alternative of `publickey` alone, or when the login
   user is `root` and `permitrootlogin` is `no` or `forced-commands-only`.
3. It writes the drop-in to `/etc/ssh/hostkeeper/candidate.conf` (a path no configuration
   includes) and runs `sshd -t -f` on it; a failure is `config_invalid`, and nothing is installed.
4. It installs the files, reloads sshd (`systemctl reload`) when a file changed and until a
   record is applied after the daemon starts, reads the effective configuration again and a banner from
   sshd's first listen address (an all-addresses address is read as the loopback). Under an active
   `ssh.socket` with `Accept=no` (Debian's and Ubuntu's default) the first connection starts the
   service the socket triggers, which serves every later connection with the configuration it
   read when it started: that service is reloaded when it runs. With `Accept=yes` (an sshd per
   connection), or while the triggered service does not run, there is nothing to reload. `sshd -T`
   reads the files on disk, not what the listening sshd holds, so it cannot tell the two apart.
5. It then requires, for the login user, the same conditions as in 2 and that the effective
   `trustedusercakeys`, `authorizedprincipalsfile` and `revokedkeys` are the drop-in's (otherwise
   `shadowed_by_stock`, with the keyword; a stock `sshd_config` that includes `sshd_config.d` after
   its own lines wins over the drop-in); and for every other recorded account the same settings as
   before, except the two password keywords.
6. Any failure after step 3 puts every file back as it was (written, or removed where there was
   none) and reloads sshd again; the report is `reload_failed` (no banner, a reload that failed, an
   account whose settings changed), `shadowed_by_stock`, `cert_login_not_possible` or
   `config_invalid`.
7. Passwords stay as found unless the record says `off`. Then, if any account with a login shell and
   a usable password hash has no non-empty `authorized_keys` (the login user excepted), nothing is
   written and the report is `would_lock_out`. An unreadable shadow file counts as a password.
8. A record with `enabled = false` removes the files, reloads sshd and reads the banner; sshd is
   left running. A failure puts the files back (`reload_failed`).

A record is taken up when its generation differs from the one last taken up, or the files of an
applied record changed. A refused one is not tried again until the next generation or a restart of
the daemon.

## What the heartbeat says

`HeartbeatRequest.ssh` (`SshReport`): `applied_generation` (the generation last taken up,
whatever it came to), `state` (`applied`, `disabled`, `blocked_by_allow_list`, `sshd_missing`,
`no_include`, `config_invalid`, `reload_failed`, `would_lock_out`, `no_login_user`,
`cert_login_not_possible`, `shadowed_by_stock`, `blocked_by_access_group`, `unsupported`), `host_key_fingerprints` (`SHA256:` of the files
`/etc/ssh/ssh_host_*_key.pub`) and `detail` (the keyword or class that failed, never an account name
or a secret). `sshd_missing` is a host with neither unit, `unsupported` one with no `systemctl` or
with a masked unit, and on a Mac `password_auth = off`. The daemon sends nothing before it has taken up a first record.

## Not built here

A real sshd runs in the root tests (`crates/hostkeeper-privileged/tests/ssh_root.rs` on Linux, the
certificate test of `device_mac.rs` on a Mac); every other test scripts its port (`SshHost`). The
coordinator's side of the heartbeat is in [ssh-ca.md](ssh-ca.md) ("What hosts are sent"). The host key
fingerprints of the enrollment request are not read yet.
