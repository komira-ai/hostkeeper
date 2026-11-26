# Host identity: the host's own certificate, for the services hostkeeper installs

Built. This is the hostkeeper design's service identity, in place of a service's hand-placed
join token, made of the host's own enrollment certificate.

## 1. What it does

A host enrolled at the coordinator holds a certificate and key the coordinator's CA issued to it
(`/etc/updater/pki`, root's alone). The daemon keeps a copy of the current pair at one fixed path
that services on the host may read, and the certificate names the host in a way any TLS server can
check:

- **The certificate names the host by a DNS name.** Every host certificate the CA issues (a first
  issue and every renewal) carries, besides `CN=host:<id>` and the URI
  `urn:hostkeeper:host-role:host`, exactly one DNS name: the host id. Its extended key usage is
  `clientAuth` alone, as before.
- **The daemon keeps a copy for services.** `/var/lib/hostkeeper/identity/` holds `host.crt` (the
  certificate, PEM) and `host.key` (its key, PKCS#8 PEM). The directory is owned by root with the
  group `hostkeeper-identity`, mode `0750`; `host.key` is `root:hostkeeper-identity` `0640`, and
  `host.crt` is `root:hostkeeper-identity` `0644`. The daemon makes the group when the host lacks it.
- **A service uses it by its own configuration.** A service that should present the host's
  identity (js-agent) runs as root or as an account in `hostkeeper-identity`, which its own package
  arranges, and its configuration (the host group's config package) names the two files. Nothing
  in hostkeeper writes another service's configuration.
- **A relying server trusts hostkeeper's CA** by its own configuration and reads the host from the
  certificate's DNS name (js-lease-server: `--agent-client-ca`).

**The group is the trust boundary.** Any account in `hostkeeper-identity`, and anything that can
change `/etc/group` (root), reads the host's key: it can act as the host toward the coordinator and
toward every server that trusts hostkeeper's CA, until the certificate expires. Only a service that
is to speak for the host belongs in the group.

There is no certificate per service, no approval step beyond enrollment, and no revocation list.
An enrolled host is trusted; a host that must stop being trusted is retired (its certificate is
not renewed and expires within the host certificate's lifetime, 7 days by default) or taken off
the network.

## 2. Keeping the copy current

- The daemon's enrollment loop runs a step every 5 seconds. After each step the daemon reads the
  current generation of `/etc/updater/pki` (`current -> gen-<n>`: `cert.pem` and `key` from the
  same generation) and makes the two files say the same: a file whose bytes, owner, group or mode
  differ is replaced; one that already matches is left alone. A host with no current generation
  (not yet enrolled, or its pki directory moved aside) has both files removed, the key first, so
  moving the pki directory aside also takes the key away from the group.
- Each file is written to a temporary file of a fixed name in the directory (`.host.key.tmp`,
  `.host.crt.tmp`, removed at every step first, so none a crash left outlives a step), owned and
  moded before any byte is written, synced, and renamed over the old one; the directory is synced.
  Directories the daemon makes on the way to it are `0755` whatever the umask. A reader sees the old
  file or the new one, never half of one.
- `host.key` is written before `host.crt`. A renewal keeps the key (only the certificate changes),
  so a reader never pairs a new certificate with an old key; only a re-enrollment with a new key
  (an operator moving the pki directory aside) changes both, and a reader that fails to load the
  pair loads both again.
- The group is looked up by name (`/etc/group` on Linux at every step; on macOS the directory
  service, whose answer is kept and asked again every 12 steps). When the host has none, the daemon
  makes it (`groupadd --system` under systemd, `addgroup -S` on Alpine, `dseditgroup -o create` on
  macOS). When it cannot be made, the files are written root's alone (group id 0, the directory
  `0700`, the key `0600`) and the error is logged once, and again when it changes; making the group
  is tried again every 12 steps (a minute). A group known only to a network directory service, not
  to `/etc/group`, is not found on Linux.
- A failure never stops enrollment or renewal; it is logged and the next step tries again.

## 3. Compatibility

- A daemon before this change checks a certificate's subject, key, CA, key usage and extended key
  usage, and refuses only a critical extension it does not know; the DNS name is a non-critical
  subject alternative name, so it installs and uses the new certificate as before. Every reader in
  the coordinator identifies a host by its `CN` and key, not by the SANs.
- A certificate issued before this change has no DNS name until the host renews it. A daemon of
  this change or later renews such a certificate at its first step, not at half its life, so the
  update that brings it (which restarts the daemon) also brings the DNS name, once the
  coordinator issues it (a release updates the coordinator before it sets the daemons' version). A
  relying server that requires the DNS name refuses such a host until then. The same holds for an
  extended key usage other than `clientAuth` alone (`crates/hostkeeper-enroll/src/enroller.rs`).

## 4. What this replaces

The join token js-agent reads from `/etc/js-agent/token`, placed by hand on each host
(komira-ai/hostkeeper#288 asked hostkeeper to place it).

js-agent presents the host's certificate in place of the token on Linux and on a Mac (komira-ai/jobscheduler#922), so an
enrolled host needs neither `/etc/js-agent/token` nor `/etc/js-agent/instance`. What is left of #288, and the design of a
general path for a per-host file the certificate cannot replace, are in [host-held-files.md](host-held-files.md).

## 5. Decisions

- **One certificate per host, the enrollment certificate,** not one per service: the host is what
  a relying server needs to know, and the enrollment certificate already proves it.
- **A group-readable copy outside the pki directory,** not a mode change of the pki directory: the
  enroller refuses a key others can read, and the key is a hard link shared by every generation.
- **The daemon makes the group,** not the package scripts: a daemon replaced by self-update runs no
  package script, and the daemon is the one place every platform shares.
- **Fixed paths, no flag:** services' configurations name them, so they never move.
