# Onboarding a Mac with the bootstrap pkg

How a new Mac gets hostkeeper with no hand work on it
([design/mac-bootstrap.md](../design/mac-bootstrap.md)). Apple Business installs one small pkg, the
bootstrap, which installs hostkeeper's own signed pkg and exits; hostkeeper's daemon then enrolls
through the usual approval ([enrollment.md](enrollment.md), "The Mac path"). The Apple Business steps
are the owner's; this page says what to put in them and what to watch.

## What the Blueprint holds

1. **The bootstrap pkg**: its versioned URL and its SHA-256, from the release's `SHA256SUMS` (the
   line of `hostkeeper-bootstrap-<version>-macos-arm64.pkg`) or from the `SHA256SUMS` of the version's
   directory, `https://packages.komira.ai/pkg/hostkeeper/<version>/`. Never `stable.json`: a pointer
   moves, and a Blueprint's pin must not. The file is signed by the Developer ID Installer identity
   and is not notarized.
2. **The farm's two values**, as the managed preference domain `dev.hostkeeper.config`: the keys
   `coordinator` (the LAN listener, an `https://` URL) and `ca_sha256` (the SHA-256 of the farm
   CA, 64 hex digits), and nothing else is needed. Optional keys are in the design's section 4. For
   maintenance updates of macOS, add the update account's four keys, all four or none:
   `update_account_name` (`hkupdate`), `update_account_password`, `admin_name` and `admin_password`
   (the administrator made at Setup Assistant); the bootstrap writes the update account's name and
   password to `/Library/Application Support/hostkeeper/update-account` (root, 0600) and writes the
   administrator's password nowhere. Never commit these settings to a repository. It is not
   yet verified that Apple Business's built-in management can carry a custom preference payload; if it
   cannot, put the same keys, as TOML, in `/Library/Application Support/hostkeeper/bootstrap.toml` on
   the Mac (owned by root, mode 0600 or 0644, not writable by others; 0600 when it holds the update
   account's keys), which the bootstrap reads when there are no managed preferences.

## Before the Mac is powered on

Add the record that approves it, in the host group it belongs to:

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment add-record --host-id mac-03 --host-group build --serial <serial>
```

or leave it to an operator's click on the Enrollment page when the request arrives: the coordinator
then writes the Mac's record from its serial, added by `coordinator`, so the Mac is listed in the
inventory ([enrollment.md](enrollment.md#a-host-approved-without-a-record)). A re-imaged Mac needs
the click with `--replace true` (the old key holds the host id).

## What happens, and where to look

On the Mac, `/var/log/hostkeeper-bootstrap.log` holds the bootstrap's lines: the pkg it fetched, each
failed attempt with its reason, and `installed hostkeeper <version>`, `hostkeeper is installed;
nothing to do`, or `hostkeeper is installed and its job was not loaded: enabled and loaded it`.

| You see | It means |
|---|---|
| `no farm values: neither ... is there` | the Blueprint's preferences have not arrived, and there is no fallback file; the bootstrap retries every 30 seconds for 15 minutes, then exits and launchd starts it again |
| `... is refused: ...` | a settings file is not root's, is writable by others, is a link, or holds an unknown key or a value the daemon would refuse; the message names the file and the key |
| `GET <url> ...` | the repository could not be reached or answered another status |
| `the pkg is refused: the pkg is signed by Team ID X, not Y` | the pkg is not signed by the pinned Team ID; nothing was written or installed |
| `the pkg's SHA-256 is ..., the pointer says ...` | a corrupted or cut download; it is retried |
| `installer exited ...` | the install failed; the next attempt installs the same bytes again |
| `installed hostkeeper <version>` | done: the daemon is loaded, finds `/etc/hostkeeper/hostkeeper.toml`, and enrolls |
| `hostkeeper is installed; nothing to do` | hostkeeper was on the Mac and launchd has the daemon's job loaded; nothing was changed |
| `hostkeeper is installed and its job was not loaded: enabled and loaded it` | hostkeeper was on the Mac but launchd did not have the daemon's job loaded (a job marked disabled is not loaded at boot); the bootstrap enabled it and loaded it, and the daemon starts |
| `launchctl <verb> exited ...` or `launchctl could not be run: ...` | on a Mac that has hostkeeper, `launchctl` failed to report, enable or load the daemon's job; the bootstrap retries, then exits and launchd starts it again |

After that the request is in the coordinator's queue (or approved by the record) and the Mac is an
ordinary host. At its first heartbeat its daemon writes its group's operator SSH keys into the
administrator account's `authorized_keys` and turns Remote Login on, so the Mac can be reached over
SSH with no password typed ([operator-keys.md](operator-keys.md)). A Mac that already has a host certificate, or `/usr/local/bin/hostkeeper` with the bootstrap's finished-install marker, is not installed
again by the bootstrap, however many times it runs; it only checks that the daemon's job is loaded, and loads it when it is not. The bootstrap does not install Apple's Command Line
Tools and nobody types `sudo softwareupdate` on the Mac: when the Mac's host group lists
`xcode-command-line-tools` in the coordinator's host prerequisites file, its daemon installs them as
root ([host-prerequisites.md](host-prerequisites.md)).

## What the bootstrap does not do

It does not enroll, hold a secret or talk to the coordinator. It writes
`/etc/hostkeeper/hostkeeper.toml` and `/etc/hostkeeper/macos-pkg.toml` only when they are absent, so a
reinstall never changes a Mac's trust root. How those files change on an enrolled Mac is later work.
