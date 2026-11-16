# SSH through the coordinator: build item C0, measurements

Status: the commands and the results table are here; **no result is recorded yet**. Every cell of the results table says `not measured` until the owner (or an agent the owner lets onto the host) pastes the output. Section 3 of `docs/design/ssh-through-coordinator.md` (PR 394) stays **Unverified** on each point below until its row is filled. This file changes no code, no daemon behaviour and no wire format.

Scope against the design. Section 11 of the design defines C0 as read-only, owner-run commands with a table of results. In this file that is **Part A** and the rows that Part A settles. **Part B** (section 3) changes state on a live Mac and is **not C0**: it is called C0b here, it is owner-run, and the design's C0 row does not cover it. The design file lives on PR 394's branch and is not in the base of this PR, so this PR cannot edit it. Once PR 394 is merged, its section 11 C0 row and the **Unverified** tags of its sections 3 and 4 are updated in one commit to say that the results table is in `docs/design/ssh-c0-measurements.md`, Part A rows are C0, and rows that only Part B settles (R3 to R6, R15 to R17) are C0b.

Owner direction (2026-11-15): "we want hostkeeper to do this automatically. For all the hosts. That way we can remote ssh through the coordinator if necessary." The hosts are the Linux build nodes (all-in-one group), the NUC (control-plane), and the Mac Studios (mac-build group); `mac-studio-01` is the first, live.

## 1. Rules for running this

- Part A is read-only: it reads files, prints state, and runs `sshd -T` and `codesign -d`, which write nothing. Run it on `mac-studio-01` and on one Linux node (the design asks for one; run it on each distro in the fleet, the NUC included, if they differ).
- Part B (C0b) changes state on `mac-studio-01` (Remote Login, a launchd service, one scratch file). It is owner-run only, on that one Mac, with the undo next to each step. Nothing in this PR runs it.
- Baseline first, and undo means restore the baseline. Before any Part B step record `systemsetup -getremotelogin` and `launchctl print system/com.openssh.sshd` (loaded or not) in the results. Every undo below puts the host back to what was recorded, never to a fixed value: if Remote Login was on at entry, it is on at the end.
- No lockout. `mac-studio-01` is probably reached over ssh today. Turning Remote Login off, or `launchctl bootout` of `com.openssh.sshd`, stops sshd and can end the session the owner is working in, after which the undo may be unreachable. So B1 and B2 run only with physical or console access to the Mac, or with a second path that does not use sshd; if neither exists, B1 and B2 are not run and stay `not measured`.
- Paste output with host names, addresses, key material and user names of the lab removed. Never paste a private key; none of these commands prints one.
- Nothing here edits `sshd_config`, a group, or an `authorized_keys` file. B5 adds one scratch file to `sshd_config.d` and removes it in the same step.

## 2. Part A: read-only commands

### A1. Linux node (one per distro)

```
cat /etc/os-release
systemctl list-unit-files 'ssh*' 'sshd*'
systemctl is-enabled ssh.service sshd.service ssh.socket
systemctl is-active  ssh.service sshd.service ssh.socket
grep -n -i -E '^\s*(Include|PermitRootLogin|PasswordAuthentication|KbdInteractiveAuthentication|AuthenticationMethods|AllowUsers|DenyUsers|AllowGroups|DenyGroups|TrustedUserCAKeys|AuthorizedPrincipalsFile|RevokedKeys|UsePAM|Port|ListenAddress)\b' /etc/ssh/sshd_config
ls -l /etc/ssh/sshd_config.d/
sshd -V 2>&1 | head -1
sshd -T -C user=root,host=x,addr=127.0.0.1 | grep -E '^(permitrootlogin|passwordauthentication|kbdinteractiveauthentication|authenticationmethods|allowusers|denyusers|allowgroups|denygroups|trustedusercakeys|authorizedprincipalsfile|revokedkeys|usepam|port|listenaddress) '
ls -l ~root/.ssh/authorized_keys
```

What each line settles:

| Design claim (section 3 of the design) | Line that settles it |
|---|---|
| `ssh.service` or `sshd.service`, whichever the host has | `list-unit-files`, `is-enabled` |
| `ssh.socket` is the active unit on Debian and Ubuntu from 22.10 | `is-active ssh.socket`, and whether `Port` and `ListenAddress` lines in `sshd_config` are ignored |
| the `Include` is at the top on Debian and at the bottom on the RHEL family | the line number of `Include` from `grep -n` against the first line of any keyword the stock file sets |
| the first value of a keyword wins, so a stock line can shadow the drop-in | `sshd -T` printing the effective value |
| existing `authorized_keys` keeps working beside a trusted CA | the file exists and `authorizedkeysfile` is the default (add `authorizedkeysfile` to the `sshd -T` filter if a node differs) |
| the lockout rule: passwords are touched only where the host is already key-only | `passwordauthentication`, `kbdinteractiveauthentication`, `authenticationmethods` as printed |

### A2. mac-studio-01

```
sw_vers
uname -m
systemsetup -getremotelogin
launchctl print-disabled system | grep -i ssh
launchctl print system/com.openssh.sshd 2>&1 | head -20
ls -l /System/Library/LaunchDaemons/ssh.plist
ls -l /usr/local/bin/hostkeeper
ls -l /etc/ssh/ /etc/ssh/sshd_config.d/ 2>&1
grep -n -i -E '^\s*(Include|PermitRootLogin|PasswordAuthentication|KbdInteractiveAuthentication|AuthenticationMethods|UsePAM|AllowUsers|DenyUsers|AllowGroups|DenyGroups)\b' /etc/ssh/sshd_config
/usr/sbin/sshd -T -C user=ADMIN,host=x,addr=127.0.0.1 | grep -E '^(passwordauthentication|kbdinteractiveauthentication|authenticationmethods|usepam|allowusers|denyusers|allowgroups|denygroups|trustedusercakeys|authorizedprincipalsfile) '
dscl . -read /Groups/com.apple.access_ssh 2>&1
dscl . -list /Users UniqueID | awk '$2 >= 501'
dscl . -read /Groups/admin GroupMembership
cat /etc/pam.d/sshd
cat /etc/pam.d/sshd_actions 2>&1 | head -20
codesign -dr - /usr/local/bin/hostkeeper 2>&1
codesign -dvv /usr/local/bin/hostkeeper 2>&1 | grep -E '^(Identifier|TeamIdentifier|Authority|Signature|CodeDirectory)'
codesign -dr - /var/lib/updater/state/hostkeeper/bin/current 2>&1
codesign -dvv /var/lib/updater/state/hostkeeper/bin/current 2>&1 | grep -E '^(Identifier|TeamIdentifier|Authority|Signature|CodeDirectory)'
ls -l /var/lib/updater/state/hostkeeper/bin/
sudo sqlite3 "/Library/Application Support/com.apple.TCC/TCC.db" "select service,client,client_type,auth_value from access where service='kTCCServiceSystemPolicyAllFiles'" 2>&1
profiles list -type configuration 2>&1
profiles status -type enrollment 2>&1
```

`ADMIN` stands for the single local administrator account; replace it with that name when running and drop it from the pasted output. The `sqlite3` line is read-only but may itself be refused by TCC without FDA, which is a result: record the refusal.

What each line settles:

| Design claim | Line that settles it |
|---|---|
| macOS ships an `Include /etc/ssh/sshd_config.d/*` line | the `grep -n` of `sshd_config`, and `ls` of `sshd_config.d` |
| `/etc/ssh` survives minor updates | record `sw_vers` now and again after the next minor update |
| the baseline Remote Login state and which launchd label serves it | `systemsetup -getremotelogin`, `launchctl print-disabled`, `launchctl print system/com.openssh.sshd` |
| `com.apple.access_ssh` exists and gates login | the `dscl` read: absent group means no gate; present means only members log in |
| the single-administrator rule picks one account | the uid-501-or-more list against the `admin` membership, and hidden accounts noted |
| the PAM account stack allows the login user | `/etc/pam.d/sshd` as printed |
| the code-signing identifier and Team ID for the FDA profile | `codesign -dr -` and `-dvv` on `/usr/local/bin/hostkeeper` first: the design has the daemon run `systemsetup` as a child of that installed binary and the profile name that path, so that is the one TCC matches and the one whose output goes into the profile. The same two commands on `.../bin/current` are recorded beside it, to show whether the two binaries have the same identifier and requirement (R14); the design's `.mobileconfig` placeholder is replaced by this requirement string (PR 400 already records that the shipped daemon is ad-hoc signed, which this output confirms or refutes) |
| a grant is matched by identifier, not path | rows B3 and B4 of part B |

## 3. Part B: state-changing steps, owner-run, mac-studio-01 only

Run in order, as root, not from the daemon, so that the result does not depend on the daemon's rights. B1 and B2 only from the console or a second path that does not use sshd (section 1). Record the baseline first (section 1) and finish by confirming `systemsetup -getremotelogin` and the launchd state equal it. Each step names its undo, and every undo means restoring the recorded baseline.

| Step | Command | Undo | Answers |
|---|---|---|---|
| B1 | `systemsetup -setremotelogin on`, then `systemsetup -getremotelogin` | `systemsetup -setremotelogin <baseline>` (`off` only if it was off on entry) | does it refuse without Full Disk Access, with what message; run once from a terminal that has FDA and once from one that has not |
| B2 | `launchctl enable system/com.openssh.sshd`, then `launchctl bootstrap system /System/Library/LaunchDaemons/ssh.plist` (skip bootstrap if `launchctl print` shows it loaded), then `systemsetup -getremotelogin` and open System Settings, General, Sharing, Remote Login | only if the baseline was off and not loaded: `launchctl bootout system/com.openssh.sshd`, then `launchctl disable system/com.openssh.sshd`; if it was on or loaded, leave it as it was | does the `launchctl` way work without FDA; do System Settings and `systemsetup -getremotelogin` agree afterwards; does it survive a reboot |
| B3 | with FDA granted to the terminal by path, repeat B1; then replace the program by a copy at another path with the same signature and repeat | remove the grant in System Settings, Privacy and Security, Full Disk Access | whether the grant follows the path or the signature |
| B4 | push the PPPC profile of PR 400 with the placeholder replaced by the A2 `codesign -dr -` output (Apple Business, custom profile), restart the daemon, then let the next daemon update replace the binary, and read the `needs_full_disk_access` report before and after | remove the profile in Apple Business | Q3 (does Apple Business's built-in management take a custom profile), TCC identifier match, and whether a grant survives an update |
| B5 | only if `/etc/ssh/sshd_config.d/` exists (do not create it; if absent record that and stop). Write `/etc/ssh/sshd_config.d/99-c0-scratch.conf` containing exactly the one line `LogLevel VERBOSE`. Immediately run `sshd -t`; if it prints anything or exits non-zero, delete the file at once and record the output. Otherwise run `sshd -T | grep '^loglevel '`, then delete the file and run `sshd -t` again to confirm the tree is clean | delete `/etc/ssh/sshd_config.d/99-c0-scratch.conf` (the only file this step writes) | that the Include is read at all: `loglevel VERBOSE` appears only if the drop-in was read (the stock value is `INFO`). The file is on the disk for the seconds between write and delete only, and never while `sshd -t` is failing. The same step may run on one Linux node, where the directory exists on Debian and Ubuntu and may not on others |

B1 to B4 act on the one Mac named; B4 depends on a decision of the owner (the profile may be pushed to a live Mac); it is listed so the table has a row, and stays `not measured` until the owner decides.

## 4. Results table

Fill one cell per row; `not measured` is the only value allowed until output is pasted, and a cell that is a refusal records the exact message.

| Id | Question | Design expectation | mac-studio-01 | Linux node |
|---|---|---|---|---|
| R1 | OS and sshd version | none | not measured | not measured |
| R2 | Remote Login baseline | off | not measured | n/a |
| R3 | `systemsetup -setremotelogin on` as root without FDA | refused, needs FDA | not measured | n/a |
| R4 | `launchctl enable` and `bootstrap` as root without FDA | works | not measured | n/a |
| R5 | System Settings and `systemsetup -getremotelogin` agree after R4 | agree | not measured | n/a |
| R6 | Remote Login after R4 survives a reboot | survives | not measured | n/a |
| R7 | `Include /etc/ssh/sshd_config.d/*` present, and position | present | not measured | top (Debian family), bottom (RHEL family) |
| R8 | Drop-in file is read | read | not measured | not measured |
| R9 | `com.apple.access_ssh` group present, and the login user a member | may exist | not measured | n/a |
| R10 | Single local administrator account (uid 501 or more, `admin`, not hidden) | exactly one | not measured | n/a |
| R11 | PAM account stack allows the login user | allows | not measured | not measured |
| R12 | `ssh.socket` active | n/a | n/a | not measured |
| R13 | Effective `passwordauthentication`, `kbdinteractiveauthentication`, `authenticationmethods` | not applicable | not measured | not measured |
| R14 | Code-signing identifier, Team ID and requirement of `/usr/local/bin/hostkeeper`, and whether `./bin/current` is the same | ad-hoc (PR 400) | not measured | n/a |
| R15 | TCC grant matches by identifier, not path | identifier | not measured | n/a |
| R16 | A grant survives a daemon update | survives | not measured | n/a |
| R17 | Apple Business built-in management accepts the custom PPPC profile (Q3) | unknown | not measured | n/a |

## 5. What each outcome changes in the design

- R3 refused and R4 to R6 pass: Remote Login uses the `launchctl` way, the daemon needs no Full Disk Access, and section 4 of the design and the `.mobileconfig` are dropped from C3m.
- R3 refused and R4 to R6 fail: `systemsetup` is the only way; the profile stays, and R14 to R17 decide its identifier and whether a stable helper is needed.
- R3 works without FDA: `systemsetup` alone; no profile.
- R7 absent: the Mac reports `no_include` as the design says, and the owner is asked.
- R9 present and the user not a member: Q5 is asked as written.
- R10 not exactly one: `--ssh-login-user-macos` becomes required on those Macs.
- R12 active: the `ssh.socket` text of section 3 stands as written; if a node runs sshd as a service, the reload text stands.
