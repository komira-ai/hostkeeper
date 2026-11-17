# Operator SSH keys on every host

How the operators' SSH public keys get onto every host hostkeeper runs on, and stay there
([design/operator-keys.md](../design/operator-keys.md)). The coordinator holds one file of public
keys per host group; each host's daemon writes its group's keys into a marked block of the login
account's `authorized_keys`. Nobody types a password, and no account name or password is stored.

## Give the coordinator the keys

Write the file, one table per host group, the keys as `ssh-keygen` prints them
(`cat ~/.ssh/id_ed25519.pub`). Give each key a comment that says whose it is; an e-mail address
works but is not needed.

```toml
[host-groups."build"]
keys = [
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... ops laptop",
]

[host-groups."mac-build"]
keys = [
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... ops laptop",
]
```

Name it in the coordinator's flags (`operator-keys-file = "/etc/hostkeeper-coordinator/operator-keys.toml"`
in `serve.toml`, or `--operator-keys-file` on its command line) and restart it once. From then on
the coordinator reads the file again at every tick of `--tls-reload-secs` (60 s by default): edit it
in place and every host of a changed group has the new keys within a heartbeat after that tick.
Public keys are not secret; the file needs no particular mode. At start a file that breaks a rule
stops the coordinator with a message naming `--operator-keys-file`, the group and the key's place
in its list; on a later tick it is logged as a warning and the keys in use stay.

Rules: at most 64 keys per group, each one line of a key type `ssh-keygen` writes, with no options
before the type (`from=...`, `command=...` are refused), none twice. A group the file does not list
gets the empty set, and so does a host in no group.

## What each host does

| Host | Account | Steps |
|---|---|---|
| Linux (systemd or OpenRC) | `root` | writes the block of `/root/.ssh/authorized_keys` (or root's home from `/etc/passwd`) |
| Mac | the one local administrator account (uid 501 or more, in `admin`, not hidden) | writes the block of `~/.ssh/authorized_keys`; when the group has keys, turns Remote Login on with `launchctl` and adds the account to `com.apple.access_ssh` when that group limits SSH |

The block is the lines from `# BEGIN hostkeeper operator keys` to `# END hostkeeper operator keys`.
Keys you add by hand go outside it and are never touched. A key removed from the file is removed
from the block of every host of its group; a group taken out of the file has its block removed. The
daemon reads the block at each heartbeat and puts it back when someone edits or removes it.

On Linux the daemon does not install, start or enable sshd: openssh-server is a package of the
host's group like any other.

## Check a host

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts get --node-id <id>
```

`host.heartbeat.operatorKeys` is the daemon's last report:

| You see | It means |
|---|---|
| `digest` equal to the set's | the block holds the group's keys, owned by the account, mode 0600 (on a Mac, Remote Login was on and the account allowed SSH when the set was put in force; a daemon start checks them again) |
| `digest` empty, `failed` set | the last try failed (a file, a directory or a command); the coordinator sends the set again at every heartbeat and the daemon tries again |
| `refused` | the daemon will not take this set: `reason` names the key's place and the rule, or says the Mac has no single local administrator account (none, or several); fix the file or the Mac, and the next set (or a restart of the daemon) tries again |
| no `operatorKeys` | the daemon predates the feature, or the heartbeat came over the JSON route |

The daemon's log says each step: `operator keys <digest>: <n> keys in force ...`, `Remote Login
turned on`, `the login account added to com.apple.access_ssh`, and every refusal and failure.

## A reinstalled Mac

A Mac erased and set up again gets hostkeeper from its Blueprint ([mac-bootstrap.md](mac-bootstrap.md)),
enrolls, and needs one approval with `--replace true` because its old key still holds its host id
([enrollment.md](enrollment.md), "The Mac path"). That approval is the only step, and it is done on
the operator's machine. At the Mac's first heartbeat after it, its daemon writes the keys of the
Mac's group, turns Remote Login on and lets the administrator account use SSH: `ssh
<account>@<mac>` then logs in with an operator key. A new Mac with an open inventory record for its
serial needs no step at all.
