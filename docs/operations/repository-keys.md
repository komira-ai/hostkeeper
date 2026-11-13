# Repository entries and keys: placing and rotating them

A host's trust in the packages it installs is set in one place per kind of host
([packages-repositories.md](../design/packages-repositories.md) sections 3.2 and 4.7). No order
names a key:

- **A Linux host** installs a service's `.deb` with `apt` from a channel of the project's package
  repository. It trusts the keyring that its entries in `/etc/hostkeeper/repositories/` name, and
  its daemon writes those entries and that keyring from the coordinator's registry of repositories
  (`Repositories`): each heartbeat reply carries the repositories the host's services name when the
  host does not hold them yet.
- **A Mac** installs a service's signed `.pkg` with `macos-pkg`. It fetches the `.pkg` from a store
  of `/etc/hostkeeper/stores.toml`, and trusts a `.pkg` only when its leaf certificate is one of the
  signers in `/etc/hostkeeper/macos-pkg.toml`. On a Mac with MDM the coordinator's gate carries
  those files in the config profile, from files its operator gives it. A Mac never reads the
  registry; a `.pkg` of a private repository is fetched from the implicit store `coordinator`,
  over the Mac's mutual-TLS connection to the coordinator.

The organisation's own repository, `komira`, is built into the coordinator: its keyring and the two fingerprints
that are the root of trust of every host's apt for it (`CB09CF205649494A92DFC22C70D1F2A25906CB22`, the primary key,
and `F767235D83B0410CFAF4779E8738AD1BE061EEDD`, its signing subkey) are in the coordinator's code
(`crates/hostkeeper-coordinator/src/builtin.rs` and `builtin/komira-archive-keyring.gpg`), so a fresh
coordinator needs no repository setup. Any other repository is added by its URL and the operator confirms the
fingerprints the coordinator computes from its key
([package-repositories.md](../deploy/package-repositories.md#how-the-key-is-confirmed)). Rotating the
`komira` key is a coordinator release with the new keyring and pin, or, at once and without a release, a `Put` of
an entry named `komira` with the new keyring (which replaces the built-in one; removing that entry brings the
built-in one back). `Add` of the name `komira` with any other key is refused (`builtin`).

So a key rotation is one change on the coordinator for every Linux host, ahead of the
repository's own change, and a change of the coordinator's MDM files for managed Macs.

## Where each host gets them

| Host | Files | Placed by |
|---|---|---|
| Linux | `/etc/hostkeeper/repositories/` | the daemon, from the coordinator's registry (`hkctl repositories put`), at the first heartbeat after the registry holds every repository the host's services name; it adopts files already there when they are byte for byte the set, and replaces them otherwise |
| Linux, before it reaches the coordinator | `/etc/hostkeeper/repositories/` | only for a host that must install from the repository first: `repositories = "<dir>"` in the boot server's `/etc/pxe/pxe.toml` ([network-install.md](network-install.md)) or the enrollment runbook's step ([enrollment.md](enrollment.md#bootstrap-a-linux-node)); the registry's set adopts or replaces them |
| Mac with MDM | `stores.toml`, `macos-pkg.toml` | the coordinator's `--mdm-stores` and `--mdm-macos-pkg` files. The gate carries them in the config profile as `dev.hostkeeper.stores` and `dev.hostkeeper.macos-pkg`, and the daemon's start writes each file from them |
| Mac without MDM | `stores.toml`, `macos-pkg.toml` | the operator, by hand, root:wheel 0644 |

The coordinator checks each of its two MDM files with the daemon's own loaders when it starts, and
refuses to start on a file that a Mac would refuse. A store that names a credential file is refused
too: a profile carries no secret.

On a Mac, the managed preferences are the source of the two files. A file that differs from them
is replaced at the daemon's next start. A Mac without the two domains keeps the files its operator
wrote. The gate sends the config profile again to an enrolled Mac on its allowlist once each time
its settings change, approved Macs included. It counts a change as delivered when the Mac
acknowledges the profile. A send that the Mac answers with an error is tried again after the gate's
resend delay. `hostkeeper.toml` is still written only when it is absent, so a profile sent again
never changes it.

## Put the repository in the registry

The registry entry `komira` gives every Linux host its two entries, `komira-testing.sources` and
`komira-stable.sources`, and the keyring `komira.gpg` they name. Write the request as JSON, the
keyring as base64 of the binary keyring (`base64 -w0 komira-archive-keyring.gpg`), and every
fingerprint the keyring holds that may sign the suites, primary key or subkey, as `gpg
--with-colons --show-keys --with-subkey-fingerprints` prints them on its `fpr` lines:

```json
{ "repository": { "name": "komira", "kind": "apt", "uri": "https://packages.komira.ai/apt",
    "suites": ["testing", "stable"], "components": ["main"],
    "fingerprints": ["<40 hex, primary key>", "<40 hex, signing subkey>"],
    "keyring": "<base64>" },
  "reason": "the project's package repository" }
```

Then `hkctl repositories put --request komira.json`. The coordinator refuses a keyring that lacks
a listed fingerprint, holds a key none of whose fingerprints is listed, or holds a secret key
(`keyring_invalid`), and changes nothing. `hkctl repositories list` shows each repository, the
services that name it, and `unregistered`: the repositories services name that the registry lacks.
A host one of whose services names one of those is sent nothing. `hkctl hosts get` shows each
host's `repositories`: `current` once the host holds the set, and `refused` with the daemon's
reason when it refused it (the host keeps its set in force).

A host whose directory holds what the held setup or the boot server's seed wrote for the same URI and
keyring adopts it as it is; a host whose files differ gets the registry's set in their place, and
any other entry in the directory goes. No step restarts a daemon.

## Put a private repository in the registry

A private repository (`"access": "hosts"`) holds a site's configuration packages, one suite per
host group ([package-repositories.md](../deploy/package-repositories.md#private-repositories-a-host-groups-configuration)).
Put it the same way, with two more fields: `"access": "hosts"` and `"credential_file"`, the
absolute path on the coordinator's machine of the file that holds the `Authorization` header value
the coordinator sends the origin. Write that file first, as the coordinator's user, mode 0600, one
line:

```sh
install -m 0600 /dev/null /etc/hostkeeper-coordinator/lab.credential
printf 'Bearer %s\n' "$TOKEN" > /etc/hostkeeper-coordinator/lab.credential
```

The coordinator refuses the entry `credential_unusable` when the file is absent, a link, not the
coordinator user's, of a mode other than 0600 or 0400, empty, more than 4 KiB, or not one line of
printable ASCII, and
refuses a public entry that names a file (`invalid_request`). Its `suites` are the host groups it
serves. Each Linux host whose services name it and whose daemon announces
`private-repository-v1` is sent it with its own group as the one suite, and fills its local mirror
from the coordinator. To change the credential, write the file again: the coordinator reads it at
every fetch from the origin, and nothing restarts. A host's `repositories` in `hkctl hosts get`
shows the set as for any entry; a key rotation is the same as below.

## Rotate the repository's signing key (Linux)

The repository's owner publishes the next key ahead of time and signs the suites with both keys
during an overlap ([package-repositories.md](../deploy/package-repositories.md)).

1. **Before the overlap ends**, make a keyring that holds both keys. A binary OpenPGP keyring is
   the keys concatenated:
   `cat komira-archive-keyring.gpg komira-archive-keyring-next.gpg > komira-archive-keyring.both.gpg`.
   Check that it lists both: `gpg --show-keys komira-archive-keyring.both.gpg`.
2. **Put the repository** with that keyring and the fingerprints of both keys, as above. Each Linux
   host whose services name it writes the new keyring at its next heartbeat.
3. **Check** every host before the owner stops signing with the old key: `hkctl hosts list` shows
   each host's `repositories` with `current` true. A host still on the old keyring alone fails each
   `stage` from then on with `signature`, and the rollout gate stops on it.
4. **After the overlap**, once the suites are signed with the new key alone, put the repository
   with a keyring that holds the new key alone and its fingerprints, and check again.

The `repositories` directory of the boot server's `/etc/pxe/pxe.toml` still seeds the keyring it was given; replace it there
too (and restart `pxe.service`) so a host installed from then on starts with the new key.

## Rotate the `.pkg` signing identity (Mac)

The release machinery signs each service's `.pkg` with one identity. Its leaf certificate's
SHA-256 is the signer that a Mac checks.

1. Read the new leaf's fingerprint from a `.pkg` signed with it: `pkgutil --check-signature
   <file>.pkg`. The leaf is the first certificate of the printed chain, and its fingerprint is the
   lines after `SHA256 Fingerprint:`. Join those lines and drop all white space; the case does not
   matter.
2. **Before the release machinery signs with it**, add it, so that the signers file holds the old
   and the new fingerprint:

   ```toml
   signers = ["<64 hex, old leaf>", "<64 hex, new leaf>"]
   ```

   - **Macs with MDM.** Write this to the coordinator's `--mdm-macos-pkg` file and restart the
     coordinator. It refuses to start on a file that the Mac's loader would refuse. The gate then
     sends the changed profile to each enrolled Mac on its allowlist, and each Mac's daemon writes
     `/etc/hostkeeper/macos-pkg.toml` at its next start. Restart it on each Mac to take it now:
     `launchctl kickstart -k system/dev.hostkeeper.hostkeeper`.
   - **Macs without MDM.** Write the file as root, root:wheel 0644, and restart the daemon the
     same way.
3. **Check** each Mac: `/etc/hostkeeper/macos-pkg.toml` names both, and the daemon is running. A
   daemon that refused the file does not start, and its log names the rule that the file breaks.
4. **After the last release signed with the old identity** is no longer installed or rolled back
   to, remove the old fingerprint the same way. A rollback installs the kept `.pkg` without checking
   its signature again, so removing the old signer does not block a rollback.

A signers file names one to four signers. Keep two only during an overlap.

## Change a Mac's channel stores

The stores work the same way as the signers: through the coordinator's `--mdm-stores` file on a
Mac with MDM, or by hand on a Mac without it. The two channels are https stores under the
names that a release's `macos-pkg` entries use as their locations:

```toml
[[store]]
name = "komira-testing"
kind = "https"
base = "https://packages.komira.ai/pkg/"

[[store]]
name = "komira-stable"
kind = "https"
base = "https://packages.komira.ai/pkg/"
```

A Mac whose `stores.toml` lacks a store that an entry names answers `no_store`.
