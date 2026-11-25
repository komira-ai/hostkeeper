# Backing up the coordinator's CAs

The fleet trusts three certificate authorities. Each is made once, and nothing else holds a copy
of its key: `hostkeeper-coordinator backup` never copies one, and the database's dumps never hold
one. The copy this page makes, sealed and offline, is the only one. The rest of the coordinator's
state (the state file, the ops file and the enrollment store, with the files beside them) is backed
up and restored as [coordinator-state.md](coordinator-state.md) says.

| CA | Its files | What the fleet pins | What its loss costs |
| --- | --- | --- | --- |
| The enrollment CA | `ca.key`, PKCS#8 DER of an ECDSA P-256 key, mode 0600; `ca.key.cert`, its self-signed certificate, DER, mode 0644. The paths are `--ca-key-file` and that path with `.cert` added | every daemon config's `ca_sha256`: the SHA-256 of `ca.key.cert`'s bytes | every daemon enrolled again, with a new `ca_sha256` in its config |
| The fleet SSH user CA | `ssh_user_ca`, an unencrypted OpenSSH Ed25519 private key, mode 0600 (the file `ssh-ca init` wrote, and the file of `--ssh-ca-secret-file`; during a rotation that secret holds two keys) | every fleet host's `/etc/ssh/hostkeeper/user_ca.pub`, which the coordinator sends it | a new CA, trusted by every host, before anyone can sign in by a certificate |
| The control-plane SSH user CA | `ssh_user_ca` as above, one key, in a directory of its own (the file of `--ssh-cp-ca-secret-file`, when the coordinator holds it) | each control-plane host's `/etc/ssh/hostkeeper/user_ca.pub`, and the file of `--ssh-cp-ca-public-file` | the same, on the control-plane hosts |

What each file is and how it is made: [enrollment-coordinator.md](../enrollment-coordinator.md) and
the deploy layout in [docs/deploy/coordinator](../deploy/coordinator/README.md) for the enrollment
CA; [ssh-ca.md](ssh-ca.md) for the two SSH CAs.

## When

Once for each CA, right after it is made: the first start with `--ca-init`, and each
`ssh-ca init`. Again only when a CA is made again (an SSH CA's `next` key during a rotation is a new
key: copy it as soon as `ssh-ca init` writes it), or when the offline key the copies are encrypted
to changes. The files of a CA never change once made, so a copy does not go stale.

## What you need

- Root on the machine that holds the CA's files.
- `hostkeeper-coordinator` with the verb `check-ca`, the same binary as the coordinator's release
  (the package's `/usr/bin/hostkeeper-coordinator`, or the release's static binary). It reads and
  never writes; it needs no running coordinator, no network and no database.
- `age`, and a recipients file: the public half of the operator's
  offline key, one `age1...` line made by `age-keygen` or one `ssh-ed25519` line. The private half
  stays offline; it is needed only to decrypt (the drill below, or a real restore).
- One removable medium, and a tamper-evident bag.
- The fleet's pins, read from the fleet and not from the copy: the `ca_sha256` line of any enrolled
  host's `/etc/hostkeeper/hostkeeper.toml`, and the `/etc/ssh/hostkeeper/user_ca.pub` of one fleet
  host and of one control-plane host.

## Make the copy

Every command runs as root. The key is copied only into memory (`/dev/shm`), checked there, and
leaves the machine only encrypted: no clear byte of a key is written to the medium.

1. Name the places, and make an empty directory in memory that only root can open:

   ```sh
   umask 077
   ca_dir=<the directory of --ca-key-file>
   fleet_dir=<the directory of the fleet CA's ssh_user_ca>
   cp_dir=<the directory of the control-plane CA's ssh_user_ca>
   media=<where the removable medium is mounted>
   recipients=<the recipients file of the offline key>
   stage=$(mktemp -d /dev/shm/ca-copy.XXXXXX)
   mkdir "$stage/enrollment" "$stage/fleet" "$stage/control-plane"
   ```

   In the compose deployment, `ca_dir` is the coordinator's volume:
   `docker volume inspect --format '{{.Mountpoint}}' hostkeeper-coordinator_hostkeeper-coordinator`.

2. Copy each CA's files into the stage, owned by root, the key 0600, and beside each SSH key the
   `user_ca.pub` of a host of its class (`<a fleet host>`, `<a control-plane host>`; any way of
   reading that file will do, `ssh` is one):

   ```sh
   install -m 0600 "$ca_dir/ca.key" "$stage/enrollment/ca.key"
   install -m 0644 "$ca_dir/ca.key.cert" "$stage/enrollment/ca.key.cert"
   install -m 0600 "$fleet_dir/ssh_user_ca" "$stage/fleet/ssh_user_ca"
   install -m 0600 "$cp_dir/ssh_user_ca" "$stage/control-plane/ssh_user_ca"
   ssh <a fleet host> cat /etc/ssh/hostkeeper/user_ca.pub > "$stage/fleet/user_ca.pub"
   ssh <a control-plane host> cat /etc/ssh/hostkeeper/user_ca.pub \
       > "$stage/control-plane/user_ca.pub"
   ```

3. Check that each copy is the CA the fleet pins. `<ca_sha256>` is the 64 hex digits of a daemon
   config's `ca_sha256`:

   ```sh
   hostkeeper-coordinator check-ca enrollment --key-file "$stage/enrollment/ca.key" \
       --certificate-file "$stage/enrollment/ca.key.cert" --ca-sha256 <ca_sha256>
   hostkeeper-coordinator check-ca ssh --key-file "$stage/fleet/ssh_user_ca" \
       --public-file "$stage/fleet/user_ca.pub"
   hostkeeper-coordinator check-ca ssh --key-file "$stage/control-plane/ssh_user_ca" \
       --public-file "$stage/control-plane/user_ca.pub"
   ```

   Each prints the CA's fingerprint (`ca_sha256: <hex>`, or `fingerprint: SHA256:<base64>` for each
   key of an SSH key file) and exits 0. Anything else stops here: a non-zero exit names what is
   wrong (a key that is not its certificate's, a certificate the daemons do not pin, a key no host
   trusts, a file cut short or open to others) and never prints a byte of a key.

4. Encrypt each CA's directory to the offline key, straight onto the medium, and take the SHA-256
   of each encrypted file for the label:

   ```sh
   for ca in enrollment fleet control-plane; do
       tar -C "$stage/$ca" -cf - . | age -R "$recipients" -o "$media/$ca-ca.tar.age"
   done
   sha256sum "$media"/*-ca.tar.age
   ```

5. Remove the stage, and unmount the medium:

   ```sh
   rm "$stage"/enrollment/ca.key "$stage"/enrollment/ca.key.cert \
       "$stage"/fleet/ssh_user_ca "$stage"/fleet/user_ca.pub \
       "$stage"/control-plane/ssh_user_ca "$stage"/control-plane/user_ca.pub
   rmdir "$stage"/enrollment "$stage"/fleet "$stage"/control-plane "$stage"
   umount "$media"
   ```

6. Run the drill below once, on the machine that holds the offline key, before the medium is
   sealed.

7. Label the medium and the bag, then seal the bag and keep it offline. The label holds, for each
   file: the file's name, its `sha256sum` line from step 4, the CA's fingerprint from step 3
   (`ca_sha256: <hex>` or each `fingerprint: SHA256:<base64>`), which offline key it is encrypted
   to (that key's name and its public half's fingerprint), and the day it was made. Never a byte of
   a key: the label is read by anyone who handles the bag.

One CA made on its own (an SSH CA's `next` key, say) is the same steps for that CA alone, onto its
own medium, labelled the same way.

## The drill: restore into an empty directory

Run it on the machine that holds the offline key, after step 4 above, and whenever the sealed copy
is to be trusted again (before a move, or yearly). It reads the copy, checks it against what the
fleet pins today, and removes what it decrypted; it changes nothing of the fleet.

1. Mount the medium (`<medium>`), check the encrypted file is the one on the label, then decrypt
   it into an empty directory in memory (`<ca>` is `enrollment`, `fleet` or `control-plane`):

   ```sh
   umask 077
   sha256sum <medium>/<ca>-ca.tar.age
   drill=$(mktemp -d /dev/shm/ca-drill.XXXXXX)
   age -d -i <the offline key> <medium>/<ca>-ca.tar.age | tar -C "$drill" -xf -
   ```

2. Check the copy against the fleet's pins as they are today, not against the copy's own
   `user_ca.pub`: take `ca_sha256` from a daemon config and `user_ca.pub` from a host again,
   as in step 2 above, into `<pin file>`:

   ```sh
   hostkeeper-coordinator check-ca enrollment --key-file "$drill/ca.key" \
       --certificate-file "$drill/ca.key.cert" --ca-sha256 <ca_sha256>
   hostkeeper-coordinator check-ca ssh --key-file "$drill/ssh_user_ca" --public-file <pin file>
   ```

   The fingerprint it prints is the one on the label, and the exit is 0.

3. Remove what was decrypted:

   ```sh
   rm "$drill"/ca.key "$drill"/ca.key.cert    # the enrollment CA
   rm "$drill"/ssh_user_ca "$drill"/user_ca.pub    # an SSH CA
   rmdir "$drill"
   ```

A drill that fails (the SHA-256 is not the label's, `age` cannot decrypt, or `check-ca` exits
non-zero) means the copy is not trusted: make a new one from the live files while they are there.

## Restoring a CA

Decrypt and check the copy as the drill does, then put the files where the coordinator reads them,
instead of step 3 of the drill:

- The enrollment CA: install `ca.key` (0600) and `ca.key.cert` (0644), owned by the user the
  coordinator runs as, at the path `--ca-key-file` names and that path with `.cert`; in the compose
  deployment, as [the deploy README](../deploy/coordinator/README.md) places them in a new volume.
  Start without `--ca-init`. Then restore the rest of the state as
  [coordinator-state.md](coordinator-state.md) says.
- An SSH CA: load `ssh_user_ca` as the secret the coordinator reads (`--ssh-ca-secret-file` or
  `--ssh-cp-ca-secret-file`), as [ssh-ca.md](ssh-ca.md) says, or put it back where the owner signs
  from (`ssh-ca sign --ca-key-file`).

Then remove the decrypted files as in step 3 of the drill.
