# Operator SSH keys

Status: built. The coordinator side is `crates/hostkeeper-coordinator/src/operator_keys.rs`, the
daemon side `crates/hostkeeper-daemon/src/operator_keys.rs` (with `authorized_keys.rs`,
`login_account.rs` and `mac_ssh.rs`), the rule both read keys by
`crates/hostkeeper-wire/src/operator_keys.rs`, and the wire `HeartbeatResponse.operator_keys` and
`HeartbeatRequest.operator_keys` of `hostkeeper.agent.v1` (feature `operator-keys-v1`).

The owner's direction of 2026-11-17: after four Macs were reinstalled, nobody could log in to them
over SSH until someone typed each Mac's password into `ssh-copy-id`. hostkeeper, the one root
daemon on every host, now puts the operators' public keys on every host it runs on, Linux and
macOS, and keeps them there.

## 1. What changes

**Before.** An operator's key reached a host's `authorized_keys` by hand (`ssh-copy-id`, with the
account's password), or, on a Linux node, through the network install's seed
(`--ssh-authorized-keys` of `hostkeeper-pxe`, once, at install). A reinstalled Mac had no key and
Remote Login off.

**After.** The coordinator is given one file, `--operator-keys-file`, that lists the operator
public keys of each host group. Every daemon that runs on a host of a group writes that group's
keys into a marked block of the host's login account's `authorized_keys`, at the first heartbeat
after the host is approved and whenever the file changes, and puts the block back when it is
edited or removed by hand. On a Mac it also turns Remote Login on and lets the account use SSH.
Keys added by hand outside the block stay as they are.

**What does not change.** No password, no account name and no private key is stored anywhere: the
file holds public keys only, and the daemon finds the account itself (section 4). sshd's own
configuration is not edited (no `sshd_config` line, no `PasswordAuthentication` change), host keys
are not touched, and on Linux sshd is not started or enabled (openssh-server is a package of the
host group like any other). The SSH certificate design (`ssh-through-coordinator.md`) is unchanged
except for its rule that hostkeeper never writes `authorized_keys`, which now excepts this block.

## 2. The coordinator's file

```toml
# One table per host group; a group that is not listed gets no keys.
[host-groups."build"]
keys = [
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... ops laptop",
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... ops desktop",
]

[host-groups."mac-build"]
keys = ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... ops laptop"]
```

- Each key is one `authorized_keys` line, `<type> <base64> [comment]`, of a type `ssh-keygen`
  writes (`ssh-ed25519`, `ecdsa-sha2-nistp256/384/521`, the two security-key types, `ssh-rsa`). A
  line with options before its type (`command=...`, `from=...`) is refused: a key here gives a
  plain login. White space is made single.
- At most 64 keys per group, each at most 4096 bytes, none twice (same type and blob). A group
  name follows the host group rule. The file is at most 256 KiB and holds nothing but
  `host-groups`.
- A refusal names the group and the key's place in its list, never the key's text.
- The file is not a secret (public keys) and needs no particular mode. It is read at start (a
  refused file stops the start, naming the flag) and again at every tick of `--tls-reload-secs`
  (60 s by default): a changed file is in use from that tick on; a file that cannot be read or is
  refused then leaves the keys in use and is logged as a warning.

Without the flag the coordinator sends no keys, and every host keeps whatever its file holds.

## 3. The wire

`operator-keys-v1` is announced by every daemon of this release and advertised by every
coordinator of it. A coordinator that was given the file answers a daemon that announced it with
the host's set (`OperatorKeys`: the keys and their SHA-256 digest, `hostkeeper_wire::operator_keys::digest`),
until the daemon's report (`OperatorKeysReport`) holds that digest in force or refused. The set is
the host group's, or the empty set when the host is in no group or its group is not in the file:
so a group taken out of the file, or a host moved to a group without keys, has its block taken
away. A daemon that does not announce the feature (every release before this one) is sent
nothing, whatever its group's keys
(`crates/hostkeeper-coordinator/src/grpc/tests/operator_keys.rs`; `tests/move_v0_1_221.rs` holds that
a v0.1.221 daemon, which announces no such feature, reads every answer with no field it does not know); the JSON route of kbf's agents never carries it. The fields are
additive (`crates/hostkeeper-proto/tests/additions_operator_keys.rs`).

The report is sent in every heartbeat: the digest of the set the block holds as the daemon read
it for that heartbeat, the last set refused with the reason, and why the last try failed. The
coordinator keeps the last report of each host in memory and shows it in the host's heartbeat
(`hk hosts get --node-id <host>`: `host.heartbeat.operatorKeys`).

## 4. The daemon

**Which account.** No account name is configured. On Linux (systemd and OpenRC) it is `root`,
read from `/etc/passwd` for its home and group. On macOS it is the one local administrator
account: uid 501 or more, a member of `admin`, not hidden, its name not starting with `_`, read
with `dscl`. A Mac with none or with several is refused (`no local administrator account ...`,
`<n> local administrator accounts ...`) and nothing is written; the reason names no account.

**The block.** `<home>/.ssh/authorized_keys` holds, once:

```
# BEGIN hostkeeper operator keys: hostkeeper replaces this block; add keys by hand outside it
ssh-ed25519 AAAA... ops laptop
# END hostkeeper operator keys
```

The daemon replaces what is between the two lines with the set, keeps every other line byte for
byte and in place, appends the block at the end the first time, and removes the two lines and
what is between them for the empty set (a file with no block and the empty set is left alone, and
no file is made). A file with a begin line and no end, an end with no begin, or two begin lines is
not written: the daemon does not guess which lines are its own.

**Ownership and modes.** `.ssh` is made mode 0700, owned by the account, when it is missing; one
that exists must be a directory (not a link) owned by the account, and loses group and other write
if it has them. `authorized_keys`, when it exists, must be a regular file (not a link). It is
written whole, as a new file in `.ssh` owned by the account's uid and group, mode 0600, synced and
renamed over the old one, so sshd never reads half a file. A file that already holds the set with
the right owner and mode is not written.

**On a Mac, when the set is not empty.** Remote Login: `launchctl print system/com.openssh.sshd`;
when the service is not loaded, `launchctl enable system/com.openssh.sshd`, `launchctl bootstrap
system /System/Library/LaunchDaemons/ssh.plist` and `launchctl print` again, which must succeed
(the steps of `ssh-through-coordinator.md` section 3, measured on the build Macs; `systemsetup`,
which needs Full Disk Access, is never run, and Remote Login is never turned off). Then the SSH
access group: when `com.apple.access_ssh` exists (Remote Login limited to some users) and the
account is not a member, `dseditgroup -o edit -a <account> -t user com.apple.access_ssh`. This
answers that design's Q5 for this account and this group only.

**Refused and failed.** A set whose digest is not its keys', a set that breaks a rule of section
2, or a Mac with no single administrator account is *refused*: the block keeps what it had, the
report names the set and why, and the set is not tried again until another is sent or the daemon
starts again. A file, a directory or a command that fails is a *failure*: the report's digest is
empty and says why, so the coordinator sends the set again at the next heartbeat and the daemon
tries again.

**Kept in sync.** The daemon reads the block at each heartbeat. A block edited or removed by hand,
or a file whose owner or mode changed, reports an empty digest; the coordinator sends the set
again and the daemon puts it back. After a daemon start the report is empty until the first set
arrives, so every start applies the set once (and, on a Mac, checks Remote Login again).

## 5. A reinstalled Mac, from device management to SSH, with no password

1. The Mac is erased and set up again; Apple Business's device management enrolls it and its
   Blueprint installs the bootstrap pkg (`docs/operations/mac-bootstrap.md`).
2. The bootstrap installs hostkeeper's signed pkg; its postinstall loads the daemon under launchd,
   as root.
3. The daemon makes a new key and posts its enrollment request. A Mac that was never enrolled and
   has an open inventory record for its serial is approved by the record with no step at all. A
   reinstalled Mac's host id is still held by its old key, so it needs one approval with
   `--replace true` (a click on the Enrollment page or one `hk enrollment approve`), on the
   operator's own machine; nothing is typed on the Mac, and no password anywhere
   (`docs/operations/enrollment.md`, "The Mac path").
4. The daemon takes its certificate at its next poll and heartbeats. The coordinator answers its
   first heartbeat with the keys of the Mac's host group.
5. The daemon finds the one administrator account, writes the block into its
   `~/.ssh/authorized_keys` (0600, owned by the account), turns Remote Login on with `launchctl`,
   and adds the account to `com.apple.access_ssh` if that group limits SSH. Its next heartbeat
   reports the digest.
6. `ssh <account>@<mac>` with one of the operator keys logs in.

## 6. Tests

The ordinary tests (no root) cover the rules with fakes and scratch directories: the file's
parsing and reloading, the coordinator's decision when to send, the report's rules, the block's
merge, the file's ownership and modes (with the test's own uid), the Linux and macOS account rules
over a scripted `dscl`, and the macOS steps over a scripted `launchctl`, `dscl` and `dseditgroup`.
Run as root: `crates/hostkeeper-privileged/tests/operator_keys_root.rs` in `privileged.yml` and
`pr-device.yml` writes the block of an account that is not root's (owner and mode as sshd wants
them); `device_mac.rs` in the macOS beta stage of `release.yml` runs the real `dscl` and
`launchctl` steps on a Mac as root under the account rule.
