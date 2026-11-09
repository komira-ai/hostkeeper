# Repository entries and keys: placing and rotating them

A host's trust in the packages it installs is part of its host configuration, placed by host setup
and never sent by the coordinator ([packages-repositories.md](../design/packages-repositories.md)
sections 3.2 and 4.7):

- **A Linux host** installs a service's `.deb` with `apt` from a channel of the project's package
  repository. It trusts the keyring that its entries in `/etc/hostkeeper/repositories/` name.
- **A Mac** installs a service's signed `.pkg` with `macos-pkg`. It fetches the `.pkg` from a store
  of `/etc/hostkeeper/stores.toml`, and trusts a `.pkg` only when its leaf certificate is one of the
  signers in `/etc/hostkeeper/macos-pkg.toml`.

hostkeeper never changes these files, and no order can name a key. So a key rotation is host setup's
work, on every host, ahead of the repository's own change.

## Where each host gets them

| Host | Files | Placed by |
|---|---|---|
| Linux, installed by network | `/etc/hostkeeper/repositories/` | `hostkeeper-pxe serve --repositories <dir>`, which checks the directory with the daemon's own loader and writes it through the seed ([network-install.md](network-install.md)) |
| Linux, set up by hand | `/etc/hostkeeper/repositories/` | the enrollment runbook's step ([enrollment.md](enrollment.md#bootstrap-a-linux-node)) |
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

## Rotate the repository's signing key (Linux)

The repository's owner publishes the next key ahead of time and signs the suites with both keys
during an overlap ([package-repositories.md](../deploy/package-repositories.md)). No step below
restarts a daemon: apt reads the keyring at every `apt-get update`.

1. **Before the overlap ends**, make a keyring that holds both keys. A binary OpenPGP keyring is
   the keys concatenated:
   `cat komira-archive-keyring.gpg komira-archive-keyring-next.gpg > komira-archive-keyring.both.gpg`.
   Check that it lists both: `gpg --show-keys komira-archive-keyring.both.gpg`.
2. **Hosts installed by network.** Replace the keyring in the directory `--repositories` names, under
   the same file name, so the `.sources` files keep naming it. Restart `hostkeeper-pxe serve`: it
   checks the directory again at its start, and every seed it serves from then on carries both keys.
3. **Every host already set up**, by network or by hand: as root, write the new keyring beside the
   old one, then rename it over the old one, so apt never reads a half-written file:

   ```
   install -o root -g root -m 0644 komira-archive-keyring.both.gpg \
     /etc/hostkeeper/repositories/.komira-archive-keyring.gpg.new
   mv /etc/hostkeeper/repositories/.komira-archive-keyring.gpg.new \
     /etc/hostkeeper/repositories/komira-archive-keyring.gpg
   ```

   Rename the file right away. The daemon refuses to start on any file in the directory that is
   not an entry or a keyring an entry names, the temporary name included.
4. **Check** every host before the owner stops signing with the old key: on each host,
   `gpg --show-keys /etc/hostkeeper/repositories/komira-archive-keyring.gpg` lists both keys. A host
   still on the old keyring alone fails each `stage` from then on with `signature`, and the
   rollout gate stops on it.
5. **After the overlap**, once the suites are signed with the new key alone, repeat steps 2 to 4 with
   a keyring that holds the new key alone.

A changed or new `.sources` file is read at the daemon's next start
(`systemctl restart hostkeeper.service`). A keyring needs no restart.

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
