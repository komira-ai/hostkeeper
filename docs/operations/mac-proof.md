# The Mac proof

This runbook is the Mac proof (MP) of `docs/design/maintenance-updates.md` section 8: on one Mac, drained and in a host group of its own,
it shows each claim marked **[V]** in sections 2.0, 2.4, 5.5 and 5.8 of that design. The results go into the design's section 10
(Proof results) and into the pull request that records them, each with the command, the macOS version (`sw_vers`), what it showed and
the decision it forces. The Mac tasks that build on these claims (the update account, the macOS apply step of the maintenance hook, the
Xcode install) cite those results, not this runbook.

Nothing here is a test of this repository. It is run by hand, once, by a person with a root shell on the proof Mac, with the owner for the
steps that need the owner's own hands (MP-E9). Reads are fine on any Mac; every change is made only on the proof Mac. Never run a step on a
Mac that takes builds.

## Who runs what, and in what order

| Order | Steps | Where | Who |
|---|---|---|---|
| 1 | MP-E1 to MP-E8, MP-E10; MP-E9 says what only the owner can do (there is no MP-E11: putting the Mac back is section 7) | the coordinator side, then the proof Mac | an operator with `hk`; then a root shell on the Mac |
| 2 | MP-X1 to MP-X6 (any time, independent) | the operator's own Mac, signed in to the developer account | the operator |
| 3 | MP-M1 (baseline), MP-A1, MP-A6 | the proof Mac | root shell |
| 4 | MP-I1 to MP-I5 | the proof Mac | root shell; needs the archive of step 2 |
| 5 | MP-A2 to MP-A5, with MP-E12's check right after MP-A2 | the proof Mac | root shell; the administrator's password from the owner |
| 6 | MP-U1 to MP-U3 (FileVault off) | the proof Mac | root shell; needs a pending minor update |
| 7 | MP-A7, MP-A4 and MP-A6 again, MP-A8 | the proof Mac | root shell and the owner (FileVault's password) |
| 8 | MP-U4, MP-U5 (FileVault on) | the proof Mac | root shell and the owner; each needs a fresh minor update |
| 9 | MP-M2 to MP-M7, one setting at a time | Apple Business, then the proof Mac | the owner sets; root shell reads |
| 10 | MP-U6, then MP-U7 or MP-U8 | the proof Mac | **Irreversible**: a major upgrade; the owner chooses the Mac |
| 11 | MP-U9 after each install; MP-A9; section 7 | the proof Mac, then the coordinator side | root shell, then the operator |

## Where the files go

Each machine has one scratch directory for the proof, named once and passed to every command as a variable; nothing is written
anywhere else, and nothing under `/tmp`:

- `$P` on the operator's side (the coordinator-side shell and the operator's own Mac): set in the preamble of MP-E1 (section 2
  sets it the same way), mode 0700, a path with no spaces. Operator credential files stay where they are and are named by path.
- `/var/root/hk-proof` on the proof Mac (`$D` in the steps): root's own, mode 0700, where the root-only password files (0600) and
  the harness's plists live. A launchd plist needs absolute paths, so the steps spell it out there.

No script file is written on either machine: the commands are pasted into the shell, the two helpers (`probe` in MP-I5, `snap`
in MP-M1) are functions of the root shell, and a launchd job's command is written into its plist.

## How a result is recorded

For each step, keep:

```
step:        MP-U3
command:     the exact command (as run, the password file named, never its content)
macOS:       sw_vers before (and after, for an update)
console:     stat -f %Su /dev/console inside the job
exit:        the exit code
output:      stdout and stderr, whole, or the files they went to
result:      what it showed, in one sentence
decides:     the "if" line of the step that matched, or what else happened
```

Copy every output off the Mac before a step that cannot be undone. A step that could not be run is recorded as
`cannot be shown, because <reason>`; that is a result too.

## 1. Before the proof: one Mac, drained, in a group of its own, with a root shell

These steps settle no [V] by themselves. They make the Mac the later sections need: moved to its own group (section 8, row MP), idle, with a root shell and a way to run a command from launchd with nobody logged in. In every step the Mac is `<node-id>`. "Expected" lines are not results.

### MP-E1 Read what the coordinator holds for the Macs
Settles: none; the baseline that MP-E5 and section 7 compare with (design 4.1). Runs: an operator on the coordinator side with hk. Needs: an operator token file (0600), the coordinator's private URL and CA file. Nothing on a Mac changes.
```sh
# the preamble of every operator-side shell (paste it; it writes no file):
P=<the proof's scratch directory>; umask 077; mkdir -p "$P"
# every hk call names the coordinator's private listener, its CA file and the operator token file (0600); <node-id> is the Mac
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> --response-file $P/get-before.json hosts get --node-id <node-id>
jq '(.host // .) | {hostGroup, live, agent: .heartbeat.agent, m: .heartbeat.maintenance, operatorKeys: .heartbeat.operatorKeys, ssh: .heartbeat.ssh}' $P/get-before.json
```
Record: node id, group, agent version, host.osVersion/osBuild/bootId, mac.filevault, authrestartSupported, updateAccount, commandLineTools, mdmConflicts, xcodes, due, lookFailed, operatorKeys, ssh.state. Keep `get-before.json`.
Decides:
- if `maintenance` is absent -> the coordinator or the daemon predates the report (design 4.1); the proof's reads are then made only on the Mac (MP-E5), and the comparison waits for a release with `maintenance-v1`.
- if `lookFailed` is set or `unparsedLines` > 0 -> fix the collector it names in `crates/hostkeeper-daemon/src/maintenance_*.rs` before any other step.

### MP-E2 Name the new group in the three group-keyed files
Settles: none; avoids a lockout. Runs: the coordinator's deploy owner, on the coordinator's configuration. Needs: MP-E1. Do this before MP-E3: a Mac in a group the operator-keys file does not list is sent the empty key set (`crates/hostkeeper-coordinator/src/operator_keys.rs`, doc lines 28-40).
```sh
# operator-keys.toml: [host-groups."mac-proof"] with the same keys as the Mac's current group
# ssh operators file: add "mac-proof" to the proof operator's classes
# host-prerequisites file: copy the Mac's current group table to [host-groups."mac-proof"]
# P set as in MP-E1, then:
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> --response-file $P/group-current.json host-groups get --host-group <current-group>
```
Record: the three diffs (names only, no keys), the time they were in force.
Decides:
- if the coordinator has no `--operator-keys-file` or `--ssh-ca-secret-file` -> `hk ssh` answers `not_configured`; the owner provides the root shell (MP-E9); finding against `docs/operations/operator-keys.md`.

### MP-E3 Move the Mac into a group of its own
Settles: none. Runs: an operator on the coordinator side with hk. Needs: MP-E2 in force (wait one `--tls-reload-secs`); the owner agrees the Mac leaves builds. The new group lists the same packages and the same desired versions, so the move is not also an update (`docs/operations/hostgroups.md`: a package the new group omits is uninstalled).
```sh
# P set as in MP-E1
# write $P/group.json from group-current.json: { "hostGroup": { "name": "mac-proof", "packages": [..same..], "policy": {..same..} } }
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> host-groups put --request $P/group.json --reason "Mac proof: one Mac in a group of its own"
# per package: $P/desired-<service>.json = { "desired": <its desired object> }
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> releases set-desired --service hostkeeper --host-group mac-proof --request $P/desired-hostkeeper.json
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> releases converge --service hostkeeper --host-group mac-proof
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> host-groups move --node-id <node-id> --host-group mac-proof --reason "Mac proof: taken out of its group"
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> --response-file $P/get-moved.json hosts get --node-id <node-id>
jq '(.host // .) | {hostGroup, operatorKeys: .heartbeat.operatorKeys, services}' $P/get-moved.json
```
Record: exit codes, the group's generation, operatorKeys digest (equal to MP-E1), services, the audit entry `host_move`.
Decides:
- if the digest is empty or refused -> MP-E2 was not in force; stop, fix, wait, check from the owner's console.
- if a service is leaving the Mac -> the package lists differ; move back (section 7) and fix `group.json`.

### MP-E4 Get a root shell
Settles: none. Runs: an operator on the coordinator side with hk, then in a root shell on the proof Mac. Needs: MP-E3; the operator listed for `mac-proof` (MP-E2). The administrator's password is typed at sudo's own prompt by the owner.
```sh
# P set as in MP-E1
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> --response-file $P/get-ssh.json hosts get --node-id <node-id>
jq '(.host // .) | {operatorKeys: .heartbeat.operatorKeys, ssh: .heartbeat.ssh}' $P/get-ssh.json
# hk ssh (docs/cli.md):
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> ssh <node-id> --reason "Mac proof shell" -- -t
# on the Mac, as the administrator:
id; sw_vers; sudo -n true; echo "sudo-n exit=$?"
sudo /bin/bash   # every block of this runbook assumes bash, not the login shell
D=/var/root/hk-proof; U=/Users   # U: where dscl keeps the local accounts' records; set it again in every new root shell
mkdir -p /var/root/hk-proof && chmod 700 /var/root/hk-proof
```
Record: hk ssh exit and refusal token, login name, ProductVersion/BuildVersion, whether `sudo -n` exits 0.
Decides:
- if `ssh.state` is `no_login_user` or the report says "n local administrator accounts" -> not exactly one visible admin; see MP-E12 (`crates/hostkeeper-daemon/src/login_account.rs`).
- if `ssh.state` is `blocked_by_access_group` -> finding against `docs/design/operator-keys.md` section 4.
- if `sudo -n` exits 0 -> report to the owner; the update path does not rely on it.

### MP-E5 Read the Mac as root and compare
Settles: none; checks that the collectors read the Mac as it is (design 5.8). Runs: a root shell on the proof Mac. Needs: MP-E4. Read-only. The listing argvs are the daemon's: `maintenance_macos.rs:31,34` (`softwareupdate --list`, `--list-full-installers`), `maintenance_mac.rs:41,44` (`fdesetup status`, `supportsauthrestart`), `maintenance_account.rs:128` (`sysadminctl -secureTokenStatus <name>`), all in `crates/hostkeeper-daemon/src/`.
```sh
cd /var/root/hk-proof
{ date -u; sw_vers; /usr/sbin/sysctl -n hw.model kern.bootsessionuuid; /usr/bin/xcode-select -p; } > base-1.txt 2>&1
{ /usr/bin/fdesetup status; /usr/bin/fdesetup supportsauthrestart; /usr/bin/fdesetup list -extended; /usr/sbin/diskutil apfs listUsers /; } > base-2.txt 2>&1
{ /usr/bin/dscl . -read /Groups/admin GroupMembership; for u in $(/usr/bin/dscl . -list /Users UniqueID | awk '$2>=501{print $1}'); do echo "== $u"; /usr/bin/dscl . -read "$U/$u" RecordName IsHidden; /usr/sbin/sysadminctl -secureTokenStatus $u; done; } > base-3.txt 2>&1
{ /usr/sbin/softwareupdate --list; /usr/sbin/softwareupdate --list-full-installers; } > base-4.txt 2>&1
{ ls -la "/Library/Managed Preferences" /var/db/softwareupdate; /usr/bin/profiles status -type enrollment; /usr/bin/plutil -p /var/db/softwareupdate/SoftwareUpdateDDMStatePersistence.plist; } > base-5.txt 2>&1
grep -il pass base-*.txt   # must print nothing
```
Record: the five files, and a table of each against the coordinator's reading (`hk hosts get`, as in MP-E1): sw_vers vs osVersion; fdesetup vs filevault; Secure Token vs updateAccount; softwareupdate vs due; Managed Preferences and the DDM file vs mdmConflicts. Expected (Apple's documentation, https://support.apple.com/guide/deployment/dep24dbdcf9e/web): `fdesetup list -extended` and `diskutil apfs listUsers /` show the volume owners.
Decides:
- if any pair differs -> fix the collector (`maintenance_mac.rs`, `maintenance_account.rs`, `maintenance_softwareupdate.rs`, `maintenance_mdm.rs`) before apply code uses that fact.
- if `sysadminctl -secureTokenStatus` prints wording other than the `TOKEN_ENABLED`/`TOKEN_DISABLED` the daemon matches -> change those constants in `maintenance_account.rs`.
- if FileVault is on -> the "FileVault on" runs need no change; if off, turning it on is the owner's (MP-E9); answers design question 4 with real state.

### MP-E6 Drain the Mac and check it is idle
Settles: none; prepares every proof (design 5.3). Runs: a root shell on the proof Mac. Needs: MP-E4; the farm owner agrees. Moving the group does not stop builds, because js-agent is not a package here. Only `js-agent cordon` does (design argv for the harness: not built in this repository; the verb is in the job scheduler's repository). Run it from launchd, never from the ssh shell: expected (recalled, not checked): the end of the session sends SIGHUP, which lifts the cordon. Its plist is also in `/var/root/hk-proof` and is loaded by hand; `KeepAlive` on a non-zero exit is deliberate, but a boot does not load it again, so after every reboot the cordon is placed again by hand and the gap is recorded.
```sh
cd /var/root/hk-proof
ls /Library/LaunchDaemons | grep -i -E 'js|kbf|jobsched'; ls -la /etc/js-agent
# write cordon.plist (Label hk-proof-cordon; ProgramArguments: /bin/sh -c '/usr/bin/stat -f %Su /dev/console; /usr/bin/sw_vers; exec <js-agent path> cordon --flags-file <F> --state-root <S> --holder mac-proof --for 12h --lease-secs 300', so every start records the console user; RunAtLoad true; KeepAlive {SuccessfulExit false}; the three Standard*Path files here)
launchctl bootstrap system /var/root/hk-proof/cordon.plist
sleep 30; tail -n 20 cordon.out; ps -axo pid,user,etime,command | grep -i -E 'js-agent|xcodebuild|xctest|simctl' | grep -v grep
```
Record: js-agent paths, the console owner and sw_vers at the top of each start in cordon.out, the cordon.out lines with times (`cordoned`, `idle`, any `kept`), the process list. After a reboot: the time from boot to `cordoned`, and whether a lease was taken in between (ask the scheduler's owner).
Decides:
- if the verb is refused (`invalid-cordon`, 401, 403) -> the lease server is too old; use the job owner's stop (disable, bootout js-agent; reversible) and record which.
- if a lease is taken after a reboot before the cordon returns -> reboot proofs run under the job owner's stop; the hold file of design 5.3/5.4 is needed.
- if `lapsed` appears during a proof -> restart the job; design 5.4's case for the hold file.

### MP-E7 Log the console user out
Settles: none; the "nobody logged in" state for design 5.5. Runs: a root shell on the proof Mac. Needs: MP-E6 idle; the owner agrees to end a standing session. Do not touch `/etc/kcpassword` or `autoLoginUser`.
```sh
cd /var/root/hk-proof
{ /usr/bin/stat -f %Su /dev/console; /usr/bin/who; echo "show State:$U/ConsoleUser" | /usr/sbin/scutil; /usr/bin/defaults read /Library/Preferences/com.apple.loginwindow autoLoginUser; ls -l /etc/kcpassword; } > session-1.txt 2>&1
CU=$(/usr/bin/stat -f %Su /dev/console); [ "$CU" != root ] && [ "$CU" != _mbsetupuser ] && launchctl bootout "gui/$(/usr/bin/id -u "$CU")"
sleep 5; { /usr/bin/stat -f %Su /dev/console; /usr/bin/who; } | tee session-2.txt
```
Record: console owner before and after; whether the session came back within 5 minutes (stat again); the same lines inside the harness job of MP-E8. Expected (recalled, not checked): `launchctl bootout gui/<uid>` ends that user's launchd domain.
Decides:
- if a session returns by itself -> a session helper restarts it; stop that job as in MP-E6 and add a "user logged in" run to MP; design 5.5 must not assume either state.
- if FileVault is on -> the login window is the resting state; a reboot needs the owner (MP-E9).
- if bootout is refused -> do not force; the owner logs out on the screen.

### MP-E8 The launchd harness every later step reuses
Settles: none; the context of every "as root from launchd, nobody logged in" proof (design 5.5). Runs: a root shell on the proof Mac. Needs: MP-E7. Shape follows the daemon's step job, `crates/hostkeeper-launchd/src/plist.rs:133-141` (`RunAtLoad` true, `AbandonProcessGroup` false, no `KeepAlive`, no `UserName`, no environment) and `docs/macos.md`; `StandardInPath` stands in for the pipe of the design's new runner (design argv, not built). One job per step, labelled `hk-proof-<name>`: a plist in `/var/root/hk-proof/<name>.plist`, `ProgramArguments` = `/bin/sh -c '<command>'`, the three Standard*Path files in the same directory, `<command>` starting with `/usr/bin/stat -f %Su /dev/console; /usr/bin/sw_vers;` so every run records both. A password goes only to `<name>.in` (0600, written from a root file or by `read -rs`), never an argument or environment; `rm -P` it when the step ends. A job that reboots the Mac is bootout'ed and its plist removed before the reboot, so a boot never runs it twice.
```sh
cd /var/root/hk-proof; N=probe; L=hk-proof-$N; : > $N.in; chmod 600 $N.in
# write $N.plist: Label $L; ProgramArguments /bin/sh -c '<command>'; RunAtLoad; AbandonProcessGroup false; StandardInPath /var/root/hk-proof/$N.in; StandardOutPath /var/root/hk-proof/$N.out; StandardErrorPath /var/root/hk-proof/$N.err (absolute paths)
chown root:wheel $N.plist; chmod 600 $N.plist
launchctl bootstrap system /var/root/hk-proof/$N.plist
until launchctl print system/$L | grep -q -E 'last exit code = [0-9]+'; do sleep 2; done   # a job not yet run prints `(never exited)`
launchctl print system/$L | grep -E 'state|last exit code'; cat $N.out $N.err
launchctl bootout system/$L; rm -P $N.plist $N.in
```
For the probe, `<command>` is `/usr/bin/stat -f %Su /dev/console; /usr/bin/sw_vers; id; /bin/launchctl managername; /usr/bin/who; env | sort` (no `env` in a job whose environment holds a password).
Record: id (uid 0), managername (expected System), console owner, the environment (note a missing PATH, HOME, TMPDIR), end state and exit code, sw_vers.
Decides:
- if a program needs an environment the job lacks -> the hook's new runner (beside `crates/hostkeeper-launchd`) passes it, with absolute paths (design 5.5).
- if bootstrap fails with I/O error 5 -> plist owner or mode wrong, or the label is loaded; bootout and retry.
- if managername is not System or id is not 0 -> stop; nothing can be concluded.

### MP-E9 Steps that need the owner's hands
Settles: none; what design 2.0, 5.5 and question 4 must not assume can be done remotely. Runs: the owner at the Mac. Needs: MP-E6.
```sh
# 1. FileVault on: after a restart, type the volume owner's password at the pre-boot screen; note the three times (restart, screen, first heartbeat)
# 2. FileVault on or off: System Settings, or in the owner's root shell: fdesetup enable | fdesetup disable (prompts; no password in an argument)
# 3. During each install, watch the screen; note any dialog, spinner or prompt, its exact text and time
# 4. Xcode_<version>.xip: on the operator's own Mac, signed in to the developer account; the farm Mac never signs in
# 5. Apple Business: read (do not change) the Blueprint and Configurations of this serial; list each key of design 5.8 that is set
```
Record: who did each step and when; dialog text exactly; the Apple Business view (no tokens or addresses); the `.xip` name, size and `shasum -a 256`.
Decides:
- if a FileVault-on restart stops at the pre-boot screen -> design 5.5 stands; a FileVault-on Mac is never ordered macos (question 4 stays "FileVault off").
- if an install needs a screen click on macOS 26 -> the MP [V] "stops on a step that needs the screen" is yes; section 5.5's hook must wait for device management or refuse.
- if Apple Business shows an update Configuration on the serial -> it counts as an mdm_conflict (design 5.8); compare with `base-5.txt`.
Irreversible only in effect: FileVault on or off is slow and changes the Mac's security; the owner decides.

### MP-E10 What the release workflow's macOS jobs already show
Settles: none; says which [V] has a recorded run. Runs: an operator on the coordinator side with hk (gh read access). Read-only. Expected (recalled, not checked): only the launchd root context on a hosted macOS 15 image (`macos-device`, `AGENTS.md` section 3); nothing on macOS 26, Secure Token, FileVault, softwareupdate, sysadminctl, xip or nobody logged in.
```sh
gh run list --repo <owner>/hostkeeper --workflow release.yml --limit 15
gh run view <run id> --repo <owner>/hostkeeper --json jobs --jq '.jobs[] | select(.name=="macos-test" or .name=="macos-device") | [.name,.conclusion,.url] | @tsv'
```
Record: the run and job URLs and the date; the image version of each job.
Decides:
- if a later run uses a macOS 26 image -> record its sw_vers; it still cannot show Secure Token, FileVault or softwareupdate, so MP stays required.

### MP-E12 Hide the proof account in the step that creates it
Settles: none; the order of the account steps in design 5.5. Runs: a root shell on the proof Mac. Needs: MP-E4 with the root shell kept open. The login rule is "the one local administrator: uid >= 501, a name not starting with `_`, in admin, not hidden" (`crates/hostkeeper-daemon/src/login_account.rs`); two visible administrators make `ssh.state` `no_login_user` and a daemon restart can cut the proof's access. The account is `hkproof`, never the production name; check `base-3.txt` first and stop if `hkproof` exists.
```sh
/usr/bin/dscl . -list /Users UniqueID | awk '$2>=501'
# MP-A2 creates hkproof and hides it in the same job; right after it, check:
/usr/bin/dscl . -read "$U/hkproof" RecordName IsHidden
# then from the operator side: the hosts get and jq lines of MP-E4, at each heartbeat across the window
```
Record: the time from creation to hiding; operatorKeys and ssh at each heartbeat across the window; whether `IsHidden 1` alone satisfies the daemon's next report.
Decides:
- if "n local administrator accounts" is reported in the window -> the account step of the hook writes `IsHidden 1` in the same step as `-addUser`, or creates without `-admin`, hides, then grants admin (design 5.5; not built).
- if `ssh.state` becomes `no_login_user` and the session drops -> real lockout risk; run the proof only with the owner at the Mac (MP-E9).


## 2. xcode-push on the operator's Mac (section 2.0)

Every step here runs as the operator's normal user in Terminal on the operator's own Mac, not on a proof Mac. Set
`P=<the proof's scratch directory>` first (see "Where the files go"). All steps
are read-only on the archive; they only make files under `$P`. Put the newest Xcode `.xip` (and one older release
if available) at `$P/Xcode_NEW.xip`, in an otherwise empty directory. Use `<node-id>` style placeholders for
anything host-specific; nothing here needs a farm host. No step runs as root, so no launchd harness is used here (the harness
of MP-E8 is for the farm-side lane; see the pointer at the end).

Built code is cited as file:line in `crates/hostkeeper-hk/src/xcode_push/`. Design text is `docs/design/maintenance-updates.md`.

### MP-X1. The tools and the macOS-only refusal

- Settles: [V] section 2.0 (`xcode-push` calls Apple tools on macOS only). Run by: the operator, at their Mac.
- Precondition: none beyond Terminal. `xcodes` is optional (only the version form needs it).
- Built: `mod.rs:66-69` (`xcodes` from PATH at `:66`; `/usr/sbin/pkgutil`, `/usr/bin/xip`, `/usr/bin/plutil` at `:67-69`); `mod.rs:199` refuses unless the OS is macOS.
  The built code runs no `shasum` or `curl` (it hashes in Rust and uploads with `ureq`), so the design's "shasum and curl" is stale (P10).

```sh
mkdir -p $P
sw_vers; uname -sm
ls -l /usr/sbin/pkgutil /usr/bin/xip /usr/bin/plutil /usr/bin/xar
command -v xcodes && xcodes version
/usr/bin/xip --help 2>&1 | head -20
man xip | col -b | sed -n 1,60p
```

- Record: full `sw_vers`; `uname -sm`; the `ls` lines; the `xcodes` version; the `xip --help` and `man xip` text verbatim.
- Decides:
  - if all three absolute paths exist -> correct the design text to name pkgutil, xip and plutil (not shasum or curl).
  - if `xip` or `pkgutil` is absent or elsewhere on this macOS -> change the paths in `Mac::system` (`mod.rs:67-69`), `tests/golden/xcode_push.txt` and `docs/operations/xcode-releases.md`.
  - if only `xcodes` is missing -> no change; it is an operator prerequisite (`docs/operations/xcode-releases.md`), and the file form needs no `xcodes`.
- Expected (a copy of `man xip` from an older macOS, read on the web, not checked on macOS 26): `xip --expand` is documented; no `--list` is documented there.

### MP-X2. Apple's signature, the positive case

- Settles: [V] section 2.0 step 2 (and 2.4 "check Apple's signature").
- Run by: the operator, at their Mac. Precondition: a genuine Apple `.xip` at `$P/Xcode_NEW.xip`.
- Built: argv `/usr/sbin/pkgutil --check-signature <path>`, stdin null (`signature.rs:116-127`). The verdict wants the line `Status: signed Apple Software`
  exactly (`signature.rs:27`), a last chain entry named `Apple Root CA` (`signature.rs:30`) and its SHA-256 fingerprint (`signature.rs:34`).

```sh
cd $P && sw_vers > x2.txt
/usr/sbin/pkgutil --check-signature Xcode_NEW.xip > x2-sig.txt 2> x2-sig.err; echo "exit=$?" >> x2.txt
cat x2.txt x2-sig.txt x2-sig.err
grep -n -E '^ *Status:' x2-sig.txt
grep -n -E '^ *[0-9]+\. ' x2-sig.txt
grep -n -A3 'SHA256 Fingerprint' x2-sig.txt | tail -6
curl -fsSL -o AppleIncRootCertificate.cer https://www.apple.com/appleca/AppleIncRootCertificate.cer
openssl x509 -inform der -in AppleIncRootCertificate.cer -noout -fingerprint -sha256 -subject
```

- Record: exit code; whole stdout and stderr; the Status line; every chain name in order; the last entry's fingerprint; the fingerprint of Apple's published root; archive name and macOS version for each archive tried.
- Decides:
  - if exit 0, the status is `signed Apple Software` and the last entry is `Apple Root CA` with the constant's fingerprint -> claim proven; `signature.rs` unchanged.
  - if exit 0 but the status wording differs -> change `SIGNED_BY_APPLE` (`signature.rs:27`) and the design wording; do not widen to developer-signed statuses; update `xcode_push/tests.rs` and the golden.
  - if the last entry's name or fingerprint differs -> change `APPLE_ROOT_CA` / `APPLE_ROOT_CA_SHA256` (`signature.rs:30,34`) to what the openssl cross-check shows; if releases end at several roots, accept an exact list of fingerprints, not a name prefix.
  - if the fingerprint rows are laid out differently -> fix `parse()` (`signature.rs:65-91`) and add the real output as a fixture (it fails closed meanwhile).
  - if exit non-zero or "no signature" for a genuine archive -> redesign section 2.0 step 2 and 2.4 for that archive kind; use MP-X5 and MP-X6 results.
- Expected (recalled, not checked): chain Software Signing -> Apple Code Signing Certification Authority -> Apple Root CA, and the status text above. The certificate list the code cites is https://www.apple.com/certificateauthority/.

### MP-X3. Negative cases for the signature check

- Settles: [V] section 2.0 step 2 ("anything else stops the push and uploads nothing"). Run by: the operator, at their Mac.
- Precondition: MP-X2 done; free space of at least the archive's size in `$P`.
- Built: `signature.rs:verdict`; a non-zero exit becomes a stop through `finish()` (`mod.rs:98`).

```sh
cd $P
printf 'not an archive\n' > fake.xip
/usr/sbin/pkgutil --check-signature fake.xip; echo "exit=$?"
cp Xcode_NEW.xip corrupt.xip
printf 'X' | dd of=corrupt.xip bs=1 seek=100000000 conv=notrunc 2>/dev/null
/usr/sbin/pkgutil --check-signature corrupt.xip; echo "exit=$?"
ls /Library/Developer/CommandLineTools/Packages/*.pkg 2>/dev/null && /usr/sbin/pkgutil --check-signature /Library/Developer/CommandLineTools/Packages/*.pkg 2>&1 | head -20   # only if present
```

- Record: exit code and full output of each case. If a developer-signed `.pkg` (any third-party installer) is at hand, add its output.
- Decides:
  - if `fake.xip` and `corrupt.xip` exit non-zero or show another status -> claim holds; keep as is.
  - if `corrupt.xip` exits 0 with `signed Apple Software` -> the signature does not cover the bytes; say so in section 2.0, and make the expansion (MP-X5) and the SHA-256 the coordinator recomputes the integrity check; the daemon's 2.4 check must rely on `xip --expand` exit 0, not on pkgutil alone.
  - if a developer-signed package ends at Apple Root CA but has another status -> confirms `verdict()` needs both status and chain; cite it in the design.
- Expected (recalled, not checked): a damaged archive reports a checksum that did not verify (forum thread https://developer.apple.com/forums/thread/667468).
- Cleanup by hand afterwards: `rm $P/fake.xip $P/corrupt.xip`.

### MP-X4. Can the version and build be read without expanding?

- Settles: [V] section 2.0 step 3 and 2.2 ("xip lists it before it expands"). Run by: the operator, at their Mac.
- Built: none. `archive.rs:155` (`release`) expands first, then reads the plists (MP-X5). The design's "xip lists it" has no built counterpart.

```sh
cd $P
/usr/bin/xip --list Xcode_NEW.xip > x4-list.txt 2>&1; echo "exit=$?"; head -5 x4-list.txt
/usr/bin/xar -tf Xcode_NEW.xip 2>&1 | head -20
/usr/bin/xar --dump-toc=toc.xml -f Xcode_NEW.xip; head -c 3000 toc.xml
mkdir -p meta && cd meta && /usr/bin/xar -xf ../Xcode_NEW.xip Metadata; file Metadata; (plutil -p Metadata || head -c 2000 Metadata)
/usr/bin/xar -tf ../Xcode_NEW.xip | grep -i -E 'Info.plist|version.plist'; echo "grep-exit=$?"
```

- Record: each command's exit and output; keep every `<name>`, `<size>`, `<checksum>` and signature element of `toc.xml`; the whole `Metadata` text; whether any member name holds a version or build.
- Decides:
  - if there is no `--list`, no `Info.plist` member and `Metadata` has no version -> rewrite 2.2's "xip lists it" to say the fetcher expands first; keep `archive.rs:release`; the daemon's stage free-space check must include the expansion size.
  - if `Metadata` or the TOC carries the version -> use it only as a cheap advisory unless MP-X3 showed it is inside the signed region; `ProductBuildVersion` after expansion stays the authority; keep the order (signature first).
  - if only the file name carries it -> never trust it; say so in section 2.0 step 3.
- Expected (a copy of `man xip` from an older macOS, not checked on macOS 26): only `--expand`. Expected (recalled, not checked): a `.xip` is a xar (https://developer.apple.com/forums/thread/133985) with `Metadata` and `Content` members.
- Cleanup by hand: `rm -r $P/meta`.

### MP-X5. The built expansion and version read, exactly

- Settles: [V] section 2.0 step 3 (and the same expansion in 2.4, daemon, as root). Run by: the operator, at their Mac.
- Precondition: free space of 12-20 GB beyond the archive (check with `df -h`); MP-X2 passed.
- Built: `archive.rs:173-181`: `/usr/bin/xip --expand <absolute archive>`, current directory `<archive dir>/.hk-xcode-push-expansion` (`EXPANSION`, `archive.rs:148`), stdin null; exactly one `*.app`
  (`archive.rs:183-195`); then `/usr/bin/plutil -extract <key> raw -o - <file>` (`archive.rs:134-140`) for `CFBundleShortVersionString` in `Contents/Info.plist` (digits and dots only) and `ProductBuildVersion` in `Contents/version.plist`.

```sh
cd $P && df -h . > x5.txt; ls -l Xcode_NEW.xip >> x5.txt; cat x5.txt
mkdir .hk-xcode-push-expansion && cd .hk-xcode-push-expansion
time /usr/bin/xip --expand "$HOME/hk-proof/Xcode_NEW.xip"; echo "exit=$?"
ls -d *.app | wc -l
/usr/bin/plutil -extract CFBundleShortVersionString raw -o - *.app/Contents/Info.plist; echo "exit=$?"
/usr/bin/plutil -extract ProductBuildVersion raw -o - *.app/Contents/version.plist; echo "exit=$?"
ls *.app/Contents/ | head -20
codesign --verify --deep --strict *.app; echo "codesign-exit=$?"
spctl --assess --type execute -vv *.app 2>&1 | head -5
cd .. && rm -r .hk-xcode-push-expansion
```

- Record: `sw_vers`; free space; archive size; the xip exit and any `signature` or `checksum` text it prints; time; the number of `.app` entries and the app name; both plutil outputs and exits; whether `version.plist` exists; codesign and spctl results (extra evidence, not built in hk).
- Decides:
  - if one app, `27.2`-style version and a build string -> claim proven; `archive.rs:release` stays.
  - if the version is not digits-and-dots, or the key lives in another file -> change `release` and `is_version` (`archive.rs`) and the design and `docs/operations/xcode-releases.md`.
  - if `ProductBuildVersion` is absent from `version.plist` -> change the build key or file in `release`; check `crates/hostkeeper-daemon/src/maintenance_xcode.rs` reads the same key.
  - if xip itself fails on trust or signature -> record the message; 2.4's expansion must run the way that works, and the design must say who runs it (see MP-X7).
  - if none or several apps -> hk stops with `holds N apps, not one Xcode`; change the design only if Apple's layout really has two.
  - if it runs out of disk or takes impractically long -> add a `df` check before the expansion in `archive.rs:release` and document the space in `docs/operations/xcode-releases.md`.
- Expected (a copy of `man xip` from an older macOS, not checked on macOS 26): `xip --expand` verifies the signature as it expands. Expected (recalled, not checked): `plutil -extract <keypath> raw -o - <file>` prints one raw value (`man plutil`).

### MP-X6. An encrypted or Apple Archive form of Xcode

- Settles: [V] section 2.0 / 2.3 ("newer releases, Apple's encrypted archive format"). Run by: the operator, at their Mac, signed in to the Apple developer account; a two-factor prompt is answered at the keyboard.
- Built: `archive.rs:56-60` (`xcodes download <version> --directory <dir>`); the filter at `archive.rs:71` keeps only `.xip`. Anything else stops with "left no .xip". The `aea`/`archiveutil` argv below are **design argv, not built**.

```sh
cd $P && mkdir -p dl
xcodes list 2>&1 | head -15
xcodes download NEW --directory $P/dl; echo "exit=$?"
ls -l dl
cd dl && for f in *; do echo "== $f"; file "$f"; head -c 16 "$f" | xxd; /usr/sbin/pkgutil --check-signature "$f" 2>&1 | head -8; done
ls /usr/bin/archiveutil /usr/bin/aa /usr/bin/aea 2>&1
for f in *.aea; do [ -e "$f" ] && /usr/bin/aea id -i "$f"; done
```

- Record: the exact file name and extension; magic bytes (`xar!` for xar, `AA01` for Apple Archive, `AEA1` for Apple Encrypted Archive); which of `archiveutil`, `aa`, `aea` exist (test `archiveutil` first, as the other design sections do); for any non-`.xip`, what the download page calls it and whether it needs a key fetched from a URL.
- Decides:
  - if Apple ships the release only as `.xip` (magic `xar!`) -> delete or soften "newer releases ... encrypted archive format" in the design (section 2.1, and 2.4 step 1); `archive.rs` unchanged.
  - if a `.aar` or `.aea` exists -> sections 2.0 steps 2-3 and 2.4 need that variant: new code in `archive.rs` (the download filter, the expansion command, a different signature check, the store's `<sha256>.xip` name in `docs/operations/xcode-releases.md`); copy the commands from this output, do not invent them.
  - if `xcodes` calls a helper tool for such archives -> name that dependency in `docs/operations/xcode-releases.md`.
- Expected (recalled, not checked): only `.xip` for Xcode; `man aa` and `man aea` exist on macOS 12 and later (https://developer.apple.com/documentation/applearchive).
- Cleanup by hand: `rm -r $P/dl`, then `rm` the remaining files of `$P`.

### MP-X7. Pointer: the same read as root with nobody logged in

Not a step here. Whether `pkgutil --check-signature` and `xip --expand` work as root under launchd with no login session is settled on a proof Mac in section 3, with the harness of MP-E8, using the outputs of MP-X2 and MP-X5 as the baseline. Nothing in the daemon runs `xip` yet.

## 3. The Xcode install, swap and removal on the proof Mac (section 2.4)

None of the install is built: `crates/hostkeeper-daemon/src/maintenance_xcode.rs` is only the collector. The argv below are
"design argv, not built" unless a step cites file:line. Every step runs on one drained proof Mac (Apple silicon, macOS 26.x),
in a group of its own, with nobody logged in, and has a root shell. Each launchd job uses the harness of MP-E8: the lines shown under `# job <name>` are its `<command>`, written into the
plist as they are (no script file is written), after `stat -f %Su /dev/console; sw_vers` so the result shows
nobody was logged in. Needed: 120 GB free on the volume of `/Applications`, and one real Apple Xcode `.xip` at
`/var/root/hk-proof/Xcode_VER.xip` (mode 0600, as Apple ships it, never repackaged). No Apple Account is used on the Mac.
Copy it there from the operator's Mac (section 2), for example `scp $P/Xcode_NEW.xip <admin>@<proof Mac>:` and then, as root, `mv` it into `/var/root/hk-proof/Xcode_VER.xip` and `chmod 600` it; check its `shasum -a 256` on both sides.

### MP-I1. Expand the archive from launchd; refuse a tampered one

Settles [V] of section 2.4 step 1 ("xip --expand, which checks Apple's signature"). Runs: a root shell, then launchd jobs.
Precondition: MP-E8 done; staging is on the volume of `/Applications`, so the later move is a rename.

```sh
cd /var/root/hk-proof
/usr/sbin/pkgutil --check-signature Xcode_VER.xip > i1-pkgutil.txt 2>&1; echo "exit=$?" >> i1-pkgutil.txt
mkdir -p /Applications/.hostkeeper-staging/good /Applications/.hostkeeper-staging/bad
cp Xcode_VER.xip Xcode_VER.tampered.xip
printf '\377' | dd of=Xcode_VER.tampered.xip bs=1 seek=$(( $(stat -f %z Xcode_VER.xip) / 2 )) conv=notrunc
shasum -a 256 Xcode_VER.xip Xcode_VER.tampered.xip > i1-sha.txt
# job i1-good, after the console line:
cd /Applications/.hostkeeper-staging/good && /usr/bin/time -l /usr/bin/xip --expand /var/root/hk-proof/Xcode_VER.xip
plutil -extract CFBundleShortVersionString raw -o - Xcode*.app/Contents/Info.plist
plutil -extract ProductBuildVersion raw -o - Xcode*.app/Contents/version.plist
/usr/bin/codesign --verify --deep --strict --verbose=2 Xcode*.app; echo "codesign exit=$?"
# job i1-bad: the same xip line with cwd .../bad and Xcode_VER.tampered.xip, then ls -laR .../bad
/usr/bin/xip --help 2>&1 | head -30     # root shell: is there a switch that skips the signature
```

The `pkgutil` and `xip` argv are exactly those of `hk xcode-push`: `crates/hostkeeper-hk/src/xcode_push/signature.rs:119-127`
and `archive.rs:173-181` (there the cwd is the expansion directory; here the staging directory). The expected statuses are at
`signature.rs:27-34`. Expected (recalled, not checked): `xip --expand` verifies the signature of a signed archive, and a
switch exists to skip it; the install never uses it.

Record: exit code, stdout and stderr of each command, `sw_vers`, console owner, version, build, codesign exit, archive size and
SHA-256, time and peak disk use, whether a partial `.app` is left in `bad`, the exact wording of any signature message.

Decides:
- if good exits 0 and bad exits non-zero with no `.app` -> keep "xip --expand checks Apple's signature" in section 2.4 step 1; the stage step treats non-zero as refusal and removes its staging directory.
- if bad exits 0 or leaves an `.app` -> section 2.4 step 1 and section 2.2 run `pkgutil --check-signature` in stage (judged as `signature.rs:27-34`) and the store's SHA-256 is the integrity check.
- if `pkgutil` fails on a genuine Apple archive -> fix the parser and constants in `signature.rs`; this blocks `hk xcode-push`.
- if `xip` works in a root shell but fails or hangs from launchd -> section 2.4 "as root with no sign-in" is wrong for expansion; decide the path before building.
- if codesign fails -> write the flags that pass into the stage step's check (section 2.4 step 1).
- if time or disk differ from the design -> set section 2.4's free-space figure and the stage timeout from them.

Reversible: delete the tampered copy and staging directories by hand. Nothing is installed.

### MP-I2. Expand an encrypted (.aea) archive

Settles [V] of section 2.4 step 1 ("for an encrypted archive, archiveutil as xcodes does"). Runs: a root shell, then launchd jobs.
Precondition: MP-I1 done; an Xcode in the encrypted form. If none exists, run only the first two commands and record "no .aea available".

```sh
ls -l /usr/bin/archiveutil; /usr/bin/archiveutil extractXcodeAEA 2>&1 | head -10    # usage text
head -c 4 /var/root/hk-proof/Xcode_AEA.aea | xxd     # expected (recalled, not checked): xip "xar!", aea "AA01"
mkdir -p /Applications/.hostkeeper-staging/aea
# job i2, after the console line (run it online, then again with the network off):
cd /Applications/.hostkeeper-staging/aea && /usr/bin/archiveutil extractXcodeAEA /var/root/hk-proof/Xcode_AEA.aea
/usr/bin/codesign --verify --deep --strict --verbose=2 Xcode*.app; echo "codesign exit=$?"
```

For the offline run, from the console (not ssh): `networksetup -setnetworkserviceenabled "<service>" off` for each service of
`networksetup -listallnetworkservices`, and `on` afterwards. Design argv, not built. Expected (recalled, not checked):
`archiveutil extractXcodeAEA <archive>` with the staging directory as cwd is what other Xcode installers run, and it may
need no network.

Record: presence and usage text of `archiveutil`, the first four bytes, exit code and output of both runs, version and build
of the expanded app, any message about keys, the network or Apple.

Decides:
- if it exits 0 offline from launchd and the app verifies -> the stage step picks the expander by the first four bytes (`AA01`: `archiveutil`, else `xip --expand`); the store names files `<sha256>.xip` (`xcode_store.rs:50`), so give it a type-neutral name.
- if it needs the network or an Apple session -> section 2.4 "no step needs an Apple Account" fails for `.aea` releases; the owner decides between the fetcher Mac of section 2.2 and expanding before upload (which breaks "unmodified archive").
- if `archiveutil` is absent or the subcommand is unknown -> the stage step refuses `.aea` with a named reason; section 2.4 says "not supported until macOS N" (record N).
- if no `.aea` could be had -> the claim stays [V], unproven; keep "reported to" and build no `archiveutil` branch.

Reversible: delete the aea staging directory by hand; turn every network service back on and record which.

### MP-I3. Move into place, accept the licence, run the first launch

Settles [V] of section 2.4 step 2 (`-license accept`, `-runFirstLaunch`, "installs packages every Xcode shares, which is why
the host is drained"). Runs: a root shell, then a launchd job. Precondition: MP-I1 done with a good expansion; `tmutil
localsnapshot` taken before the `mv` (required). **Irreversible**, expected (recalled, not checked): `-runFirstLaunch` installs system packages that deleting
the Xcode does not remove (`pkgutil --forget` drops only the receipt); a macOS reinstall undoes it, and the snapshot is for reading what changed, not a rollback.

```sh
cd /var/root/hk-proof && tmutil localsnapshot
pkgutil --pkgs | sort > i3-pkgs-before.txt
find /Library/Developer /Library/Apple /Library/Frameworks /Library/PrivilegedHelperTools /Library/LaunchDaemons -maxdepth 2 2>/dev/null | sort > i3-find-before.txt
mv /Applications/.hostkeeper-staging/good/Xcode*.app /Applications/Xcode-VER.app
DEV=/Applications/Xcode-VER.app/Contents/Developer
$DEV/usr/bin/xcodebuild -license accept; echo "license exit=$?"
"$DEV/usr/bin/xcodebuild" -checkFirstLaunchStatus; echo "status exit=$?"
# job i3, after the console line:
DEV=/Applications/Xcode-VER.app/Contents/Developer; date
$DEV/usr/bin/xcodebuild -runFirstLaunch; echo "runFirstLaunch exit=$?"; date
"$DEV/usr/bin/xcodebuild" -checkFirstLaunchStatus; echo "status exit=$?"
# after the job:
pkgutil --pkgs | sort > i3-pkgs-after.txt; diff i3-pkgs-before.txt i3-pkgs-after.txt | tee i3-pkgs.diff
find /Library/Developer /Library/Apple /Library/Frameworks /Library/PrivilegedHelperTools /Library/LaunchDaemons -maxdepth 2 2>/dev/null | sort | diff i3-find-before.txt -
for p in $(sed -n 's/^> //p' i3-pkgs.diff); do pkgutil --pkg-info "$p"; pkgutil --files "$p" | head -15; done
```

Design argv, not built (`xcodebuild` appears in the daemon only as a probe in `xcode_clt.rs`). Expected (Apple's
documentation: a copy of `man xcodebuild` read on the web, not checked on a Mac): `-runFirstLaunch` installs additional system
components and agrees to the licence; `-checkFirstLaunchStatus` exits non-zero while content needs installing; `-license
accept` works without launching Xcode and needs a privileged user.

Record: the snapshot name, all diffs, `pkg-info` of each added package, exit codes of licence accept and both status checks,
exit code and output of `runFirstLaunch` with its two timestamps, and any window, prompt, network fetch or "Install Started"
in the job's error file. If a second Xcode is first-launched, its status exit.

Decides:
- if `runFirstLaunch` exits 0 from launchd, adds packages, and the status then exits 0 -> keep the order in section 2.4 step 2 (`-license accept`, `-runFirstLaunch`, after the rename, before `xcode-select -s`) and name the packages in section 2.4 and `docs/macos.md`.
- if it adds nothing when another Xcode was already first-launched -> packages are shared once per macOS: section 2.4 runs it only when `-checkFirstLaunchStatus` exits non-zero, and a plain swap needs no drain.
- if it stops, asks for a login or screen, fetches from Apple, or exits non-zero from launchd -> section 2.4 "as root with no sign-in" fails for this step: the Mac needs a path to Apple, or the step moves to the operator's one-time setup and the hook reports "needs a person".
- if the status exits non-zero after a good `runFirstLaunch` -> use only the answer that is right as the step's check.
- if files change outside `/Library` -> list those places in the update notes of section 2.4.

### MP-I4. Simulator platforms: export and import

Settles [V] of section 2.4 step 2 (`-exportPlatform`, `-importPlatform`, "xcode-push then also uploads them"). Runs: a root
shell, then a launchd job. Precondition: MP-I3 done. The export needs Apple and about 10 GB: run it on a Mac with Apple
reachable, never in a drained turn. Not built: `hk xcode-push` uploads only the `.xip` (`crates/hostkeeper-hk/src/xcode_push/mod.rs`).

```sh
DEV=/Applications/Xcode-VER.app/Contents/Developer
$DEV/usr/bin/xcodebuild -help 2>&1 | grep -i -n 'platform\|export\|import'
$DEV/usr/bin/xcodebuild -exportPlatform 2>&1 | head -5; $DEV/usr/bin/xcodebuild -importPlatform 2>&1 | head -5
mkdir -p /var/root/hk-proof/platforms && cd /var/root/hk-proof/platforms
$DEV/usr/bin/xcodebuild -downloadPlatform iOS -exportPath /var/root/hk-proof/platforms > ../i4-export.txt 2>&1; echo "exit=$?" >> ../i4-export.txt
shasum -a 256 /var/root/hk-proof/platforms/* > ../i4-sha.txt; xcrun simctl runtime list > ../i4-runtimes-before.txt 2>&1
# job i4-import, after the console line (network off as in MP-I2; NAME is the file the export wrote):
$DEV/usr/bin/xcodebuild -importPlatform /var/root/hk-proof/platforms/NAME.dmg; echo "exit=$?"; xcrun simctl runtime list
```

Expected (recalled, not checked): `-downloadPlatform <name> -exportPath <dir>` and `-importPlatform <file>` exist. The published
man page (a copy of `man xcodebuild` read on the web) lists `-downloadAllPlatforms` but neither
`-exportPlatform` nor `-importPlatform`; the first two probes decide which flag names the design may use.

Record: the grep output and exit codes of the two bare flags, the flags that worked, file names, sizes and SHA-256, the import's
exit code and output online and offline, the runtime list before and after, where the runtime lands and whether it is per
Xcode, per user or machine-wide, and whether two exports of one platform have one SHA-256.

Decides:
- if `-exportPlatform` is not a flag -> section 2.4 names a flag that does not exist: write the recorded flags, and say the export contacts Apple, so it runs on the operator's Mac or the fetcher.
- if `-importPlatform <file>` works offline from launchd and a job can use the runtime -> keep the import as a hook step, the file stored beside the archive with its own SHA-256 (new store file type next to `<sha256>.xip`, an additive route per B8), and `hk xcode-push` gains an upload.
- if it needs the network or an Apple session, or fails from launchd -> drop "imported on each Mac" from section 2.4; platforms come another way (`xcrun simctl runtime add <dmg>`, record whether it works offline) or are left to a person.
- if the runtime is per Xcode or per user -> MP-I5's removal step also lists and removes the runtimes.

Reversible: delete the export directory by hand; remove the runtime with `xcrun simctl runtime delete <identifier>` (record the
identifier; this command is itself unproven).

### MP-I5. Install, swap and remove an Xcode, probed before and after each step

Settles [V] of section 2.4 steps 2 and 3 (rename to `Xcode-<version>.app`, `xcode-select -s`, remove older versions) and the
unverified claim at `crates/hostkeeper-daemon/src/maintenance_xcode.rs:9-11` (`xcode-select -p` exits 2 with no developer
directory). Runs: a root shell, and a launchd job for the swap. Precondition: MP-I3 done; a second, older Xcode expanded
(MP-I1) into `/Applications/.hostkeeper-staging/good2` and its licence accepted. The probe is mandatory at every step. It
reads what the built code reads: `xcode-select -p` (`maintenance_xcode.rs:94-103`, accepted exit codes `[0, 2]` at :99) and each
`version.plist` (:123-127), plus `xcodebuild -version` as js-mac-probe does. If the Mac runs js-agent, also record its log
lines and the builds in its next hello after each step.

```sh
cd /var/root/hk-proof
# the probe: a function of this root shell, written to no file
probe() {
  echo "### $(date)"; ls -d /Applications/Xcode*.app /Applications/.hostkeeper* 2>&1
  for A in /Applications/Xcode*.app; do echo "== $A"
    plutil -extract CFBundleShortVersionString raw -o - "$A/Contents/version.plist"; echo
    plutil -extract ProductBuildVersion raw -o - "$A/Contents/version.plist"; echo; done
  /usr/bin/xcode-select -p; echo "select -p exit=$?"; /usr/bin/xcodebuild -version; echo "version exit=$?"
}
probe > i5-0-start.txt 2>&1
cur=$(/usr/bin/xcode-select -p); /usr/bin/xcode-select --reset; /usr/bin/xcode-select -p; echo "none set: -p exit=$?"
/usr/bin/xcode-select -s "$cur"; /usr/bin/xcode-select -p
probe > i5-1-before-install.txt 2>&1
mv /Applications/.hostkeeper-staging/good2/Xcode*.app /Applications/Xcode-VER2.app
probe > i5-2-after-install.txt 2>&1; diff i5-1-before-install.txt i5-2-after-install.txt
probe > i5-3-before-swap.txt 2>&1
# job i5-swap, after the console line:
/usr/bin/xcode-select -s /Applications/Xcode-VER2.app; echo "app exit=$?"; /usr/bin/xcode-select -p
/usr/bin/xcode-select -s /Applications/Xcode-VER.app/Contents/Developer; echo "dev exit=$?"; /usr/bin/xcode-select -p
# after the job, in the root shell:
probe > i5-4-after-swap.txt 2>&1
# remove the one not selected (by hand; the daemon will use the scratch-removal tool)
probe > i5-5-before-remove.txt 2>&1; lsof +D /Applications/Xcode-VER2.app | head -5
time /bin/rm -R /Applications/Xcode-VER2.app; probe > i5-6-after-remove.txt 2>&1
# negative: the selected one goes away, then comes back
mv /Applications/Xcode-VER.app /Applications/Xcode-VER.app.moved; probe > i5-7-selected-gone.txt 2>&1
mv /Applications/Xcode-VER.app.moved /Applications/Xcode-VER.app
/usr/bin/xcode-select -s /Applications/Xcode-VER.app; probe > i5-8-restored.txt 2>&1
```

Design argv, not built. Expected (recalled, not checked): `xcode-select -s <path>` sets and `--reset` clears the developer
directory; the exit code when none is set is not documented, which is what the reset test measures.

Record: every `i5-*.txt` and the diffs; the exit code of `-p` with none set; what `-p` prints after `-s` with the app path and
with the `Contents/Developer` path; whether `-s` needs the path to exist; the build of each app; whether `xcodebuild -version`
fails while the selected Xcode is gone; the removal time; `stat -f %d` of the staging directory and of `/Applications`; the
js-agent log text and builds after each step.

Decides:
- if `-p` exits 2 with none set -> keep `[0, 2]` at `maintenance_xcode.rs:99`; replace the "unverified [V]" sentence at :9-11 with the fact and the macOS version.
- if `-p` exits another code, or 0 with the Command Line Tools path -> change the codes at :99 and the `code == 2` branch at :101-103, and update `maintenance_xcode_tests.rs` and the comment at :9-11.
- if `-p` prints `<app>/Contents/Developer` after `-s <app>` -> matches `maintenance_xcode.rs:137-139`; keep, and use the form that works from launchd.
- if `-s` from launchd fails, asks for a password, or needs licence or first launch first -> fix the step order in section 2.4 steps 2 and 3 and in the hook's step list.
- if removal leaves `-p` at exit 0 with a missing path, `xcodebuild -version` erroring, or the agent still advertising the build -> the hook never removes the selected Xcode and selects first (precondition in section 2.4 step 3); a stale agent build is for the job scheduler's owner (re-probe at job start), not a hostkeeper change.
- if a probe sees a partial `version.plist` during the rename -> staging stays under the dot name `.hostkeeper-staging` on the same volume, and no incomplete `Xcode*.app` name is ever left in `/Applications`.
- if removal takes longer than the hook's step deadline -> set the removal deadline from the recorded time (`maintenance_xcode.rs:38` is 60 s for `xcode-select` only).

**Irreversible** (removal): the app is gone; it can be re-expanded from the `.xip` kept in `/var/root/hk-proof` (MP-I1). The
negative test must be put back: before the Mac leaves its group, `xcode-select -p` prints a path that exists and
`i5-8-restored.txt` shows it. Install and swap are reversible with `mv` and `xcode-select -s`.

## 4. The update account and FileVault (section 5.5)

Settles the [V] marks of `docs/design/maintenance-updates.md` section 5.5. The proof account is `hkproof`, never the production
name. Every step runs as root on the one drained proof Mac with nobody logged in; run none of it on any other Mac. Each job goes
through the harness of MP-E8 (a command and a stdin file); each command starts with `stat -f %Su /dev/console; sw_vers;`, and the
console owner must be `root` in every result (expected with nobody logged in, recalled, not checked; anything else is recorded as not the case the design claims). Passwords go only on standard input from a 0600 file in
`/var/root/hk-proof`. No step captures a password into an output file. Order: A1, A2, A3, A4 (FileVault off), A5, A6, A7, A4 and A6
again (FileVault on), A8, A9. Copy `sw_vers`, `fdesetup status` and `fdesetup list` off the Mac before A8.

### MP-A1 The reads the daemon already makes, before the account exists

Settles section 5.5 (the built reads, [V] in `maintenance_account.rs` line 6 and the Secure Token stream). Run by a root shell on the proof Mac. Needs MP-E8. `hkproof` must not exist (a `dscl` exit other than 56 for it: stop).

```sh
umask 077; cd /var/root/hk-proof
printf 'admin name: '; IFS= read -r A; printf '%s\n' "$A" > admin-name; unset A
printf 'hkproof password (throwaway): '; IFS= read -rs PW; echo; printf '%s\n' "$PW" > pw-proof; unset PW
printf 'admin password: '; IFS= read -rs PW; echo; printf '%s\n' "$PW" > pw-admin; unset PW
cat pw-proof pw-admin > in-proof-admin; cat pw-admin pw-proof > in-admin-proof
A=$(cat admin-name)
for who in hkproof "$A"; do
  /usr/bin/dscl . -read "$U/$who" RecordName IsHidden >a1-dscl.out 2>a1-dscl.err; echo "dscl $who exit=$?"; cat a1-dscl.out a1-dscl.err
  /usr/sbin/sysadminctl -secureTokenStatus "$who" >a1-st.out 2>a1-st.err; echo "token $who exit=$?"; echo '-- stdout'; cat a1-st.out; echo '-- stderr'; cat a1-st.err
done
/usr/bin/dscl . -read /Groups/admin GroupMembership; /usr/bin/fdesetup status
```

Argv as merged: `maintenance_account.rs:127-128`, `:174-190`, `:41` (56), `:44`, `:47`; `maintenance_mac.rs:35,41`. Record: exit, stdout and stderr of each call apart; which stream carries the Secure Token line and its exact text; `fdesetup status`; the admin's name is not a secret.
Decides:
- dscl exits 56 for the missing user -> keep `RECORD_NOT_FOUND` (`maintenance_account.rs:40`), drop the [V] in its module doc; other code -> change the `Some(RECORD_NOT_FOUND)` arm in `collect()` and its fixtures.
- Token line text differs from `Secure token is ENABLED/DISABLED for user` -> change `TOKEN_ENABLED`/`TOKEN_DISABLED` (`:44`, `:47`) and `tests/fixtures/maintenance/account/token-*.txt`.
- Only one stream carries the line -> no code change (both are joined at `:131-135`); say which in the doc comment; add the real text as a fixture.
- The admin reads DISABLED -> stop; the grant in A3 cannot work; use another Mac.

### MP-A2 Create the account, hidden, in one job

Settles section 5.5 line 468-470 ([V]: `-password -` read from standard input without a terminal; else the daemon needs a pty it owns). Root shell on the proof Mac. Needs A1. The hide is in the creating job, so the Mac never holds two visible administrators (the daemon's one-administrator rule would fail). Design argv, not built (`SYSADMINCTL` path const, `maintenance_account.rs:35`).

```sh
# variant A, harness of MP-E8, stdin /var/root/hk-proof/pw-proof
CMD='U=/Users; stat -f %Su /dev/console; sw_vers; /usr/sbin/sysadminctl -addUser hkproof -password - -admin; echo rc=$?; tty; /usr/bin/dscl . -create "$U/hkproof" IsHidden 1; /usr/bin/dscl . -create "$U/hkproof" UserShell /usr/bin/false'
# variant B (only if A failed): stdin /dev/null, a pipe
CMD='stat -f %Su /dev/console; sw_vers; /bin/cat /var/root/hk-proof/pw-proof | /usr/sbin/sysadminctl -addUser hkproof -password - -admin; echo rc=$?'
# variant D (only if A and B failed): stdin pw-proof, a pty from /usr/bin/script; add the two dscl calls of A after it
CMD='stat -f %Su /dev/console; sw_vers; /usr/bin/script -q /dev/null /usr/sbin/sysadminctl -addUser hkproof -password - -admin; echo rc=$?'
# after the job, in the root shell
/usr/bin/dscl . -read "$U/hkproof" RecordName UniqueID UserShell NFSHomeDirectory IsHidden AuthenticationAuthority; echo "exit=$?"
/usr/bin/dscl . -read /Groups/admin GroupMembership
grep -l -F -f /var/root/hk-proof/pw-proof /var/root/hk-proof/*.out /var/root/hk-proof/*.err   # file names only; must print nothing, for every variant
```

Record: first variant that makes the account; exit, stdout, stderr (`Creating new user record`, or the error), a hang past the harness wait; `tty` text; the dscl lines; whether `;SecureToken;` is already in AuthenticationAuthority; whether the grep listed any file. If the account exists but a variant fails halfway, delete it with the first command of A9 b before the next variant.
Decides:
- A works (no tty, a file on stdin) -> remove the pseudo-terminal sentence of section 5.5 and the [V]; the runner is a plain stdin pipe (Launchctl port, design section 5.2).
- A fails, B works -> the runner must use a pipe, never a file; no pty.
- Only D works -> pty runner needed; R2 forbids `unsafe`, so run `/usr/bin/script -q /dev/null` as the program with the password on its stdin; script echoes the typed line: filter output from every log.
- No variant works -> design change in section 5.5 "Making the account" and mac-bootstrap.md section 4: use A3's `-adminUser` form, or make the account in the installer's package script or by MDM, not the daemon.
- The hide or the shell change drops admin -> hide after the token grant instead and record the order.

### MP-A3 Grant the Secure Token with two passwords on standard input

Settles section 5.5 line 469 ([V]: `-secureTokenOn ... -password - -adminUser <admin> -adminPassword -`, both passwords on stdin, which order). Root shell. Needs A2 (account has no token; `-secureTokenStatus hkproof` says DISABLED). Design argv, not built. Use the variant that worked in A2.

```sh
# order 1 (hkproof, then admin): harness of MP-E8, stdin /var/root/hk-proof/in-proof-admin
CMD='stat -f %Su /dev/console; sw_vers; A=$(cat /var/root/hk-proof/admin-name); /usr/sbin/sysadminctl -secureTokenOn hkproof -password - -adminUser "$A" -adminPassword -; echo rc=$?; /usr/sbin/sysadminctl -secureTokenStatus hkproof 2>&1'
# order 2, only if order 1 did not grant: same CMD, stdin in-admin-proof
# under a pty, only if both failed: same CMD with /usr/bin/script -q /dev/null before /usr/sbin/sysadminctl, stdin in-proof-admin
# one call, only if all failed (delete the account first, A9 b): the A2 command with -adminUser "$A" -adminPassword - added, stdin in-proof-admin
grep -l -F -f /var/root/hk-proof/pw-proof /var/root/hk-proof/*.out /var/root/hk-proof/*.err; grep -l -F -f /var/root/hk-proof/pw-admin /var/root/hk-proof/*.out /var/root/hk-proof/*.err
```

Record: exit, stdout, stderr of each try (`Done!`, `Authentication failed`, ...), the status line after, which order succeeded, whether either grep listed a file.
Decides:
- Order 1 works with no tty -> fix that order in the creating step and section 5.5 line 469; the runner writes `<proof pw>\n<admin pw>\n`.
- Only order 2 -> write that order in the design and in the runner's test; the fake `sysadminctl` asserts it.
- Only under script(1) -> pty runner as A2; strip both passwords' echo from every log (keep step and exit code only).
- Only the one-call form -> replace the two-step in section 5.5 and in the creating code.
- `Secure token operation failed` with a token-holding admin -> the admin must be a volume owner; stop and find out why it is not.
- A password text in an output file -> the daemon never logs the program's output on this step; add a no-leak test (T13).

### MP-A4 Three views of the token

Settles section 5.5 ([V]: the token also shows in `diskutil apfs listCryptoUsers /` and `fdesetup list`, only when FileVault is on). Root shell. Needs A3. Run once with FileVault off and again after A7. Only `sysadminctl -secureTokenStatus` is built (`maintenance_account.rs:127-146`); the other reads are design argv, not built.

```sh
A=$(cat /var/root/hk-proof/admin-name)
for who in hkproof "$A"; do /usr/sbin/sysadminctl -secureTokenStatus "$who" 2>&1; echo "exit=$?"; done
/usr/sbin/diskutil apfs listCryptoUsers /; echo "exit=$?"
/usr/bin/fdesetup list; echo "exit=$?"
/usr/bin/dscl . -read "$U/hkproof" AuthenticationAuthority GeneratedUID
/usr/bin/fdesetup status
```

Record: all five outputs; whether the GeneratedUID of `hkproof` is in `listCryptoUsers`; whether `fdesetup list` prints `hkproof,<UUID>`; whether AuthenticationAuthority has `;SecureToken;`, each with FileVault off and on.
Decides:
- ENABLED and listed in `listCryptoUsers` -> `maintenance_account.rs` stays.
- ENABLED but not listed (or the reverse) -> change `secure_token()` in `maintenance_account.rs` to the view that is right, with a fixture, or add a second check.
- `fdesetup list` shows `hkproof` with FileVault off -> the FileVault-user check need not look at the state first.

### MP-A5 Hidden, no shell, not in ssh

Settles section 5.5 line 470-471 (hidden, no login shell, not in `com.apple.access_ssh`). Root shell. Needs A2 (the hide was done there). Built reads: `login_account.rs:210-213` (`is_hidden`), `maintenance_account.rs:174-182`; nothing built writes `IsHidden`.

```sh
/usr/bin/dscl . -read "$U/hkproof" RecordName IsHidden; echo "exit=$?"
/usr/sbin/dseditgroup -o checkmember -m hkproof com.apple.access_ssh; echo "ssh-group exit=$?"
/usr/bin/defaults read /Library/Preferences/com.apple.loginwindow HiddenUsersList 2>&1
/usr/bin/dscl . -list /Users UniqueID | awk '$2>=500'; ls -ld "$U/hkproof"
/usr/sbin/sysadminctl -secureTokenStatus hkproof 2>&1; /usr/bin/dscl . -read /Groups/admin GroupMembership
```

Record: the `IsHidden` line and exit; the home path; the ssh-group text and exit; whether the account is still an administrator and still holds the token after the hide and the shell change; at the console (owner at the Mac), whether the login window or Users & Groups shows it.
Decides:
- `IsHidden: 1` -> `is_hidden()` stays; the creating step runs `dscl . -create "$U/hkproof" IsHidden 1` as in A2 (with the production name).
- The login window still lists it -> add `HiddenUsersList` (`defaults write`) to the creating step.
- In `com.apple.access_ssh` -> add `dseditgroup -o edit -d <name> -t user com.apple.access_ssh` to the creating code.

### MP-A6 FileVault state and `supportsauthrestart`

Settles section 5.5 line 481 ([V]: FileVault on needs a FileVault-user update account and `supportsauthrestart` true). Root shell. Needs MP-E8. Run with FileVault off, and again after A7. Merged: `maintenance_mac.rs:35` (`/usr/bin/fdesetup`), `:41` (`status`), `:44` (`supportsauthrestart`), `:171-179`, parsers `:199-231` (only `true`/`false`, exit 0).

```sh
# harness of MP-E8, stdin /dev/null
CMD='stat -f %Su /dev/console; sw_vers; /usr/bin/fdesetup status; echo "status rc=$?"; /usr/bin/fdesetup supportsauthrestart; echo "supportsauthrestart rc=$?"'
/usr/bin/fdesetup supportsauthrestart; echo "shell rc=$?"   # the same from the root shell
```

Record: exact stdout and exit of both, each state, launchd and shell. Expected (recalled, not checked): `true` or `false` on stdout, exit 0.
Decides:
- true/false and exit 0 in both states -> `maintenance_mac.rs` unchanged; drop the [V].
- A non-zero exit for false, or extra text -> add the code to the `&[0]` lists at `:171` and `:177` and fix `parse_supportsauthrestart`, or a FileVault-off Mac fails its whole mac section.
- false with FileVault on -> FileVault-on Macs cannot be served; the build Macs stay off (design question 4); `macos` stays refused.
- true with FileVault off -> the gate reads FileVault state first, not this alone.

### MP-A7 FileVault on, and the update account a FileVault user

Settles section 5.5 line 478 ([V]: the account must be a FileVault user; what `fdesetup add` needs). Root shell, owner at the Mac for a long conversion. Needs A3; FileVault off; admin is a volume owner. The enable prints a recovery key: it goes to a 0600 file, is deleted at the end of the step and is never recorded. Design argv, not built (`fdesetup enable -inputplist`, `fdesetup add -usertoadd <name> -inputplist`). The plists hold passwords and are written with the `printf` builtin, so no password is in any argv.

```sh
cd /var/root/hk-proof; umask 077; A=$(cat admin-name); PA=$(cat pw-admin); PU=$(cat pw-proof)
H='<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict>'
printf "$H<key>Username</key><string>%s</string><key>Password</key><string>%s</string></dict></plist>\n" "$A" "$PA" > fv-enable.plist
printf "$H<key>Username</key><string>%s</string><key>Password</key><string>%s</string><key>AdditionalUsers</key><array><dict><key>Username</key><string>hkproof</string><key>Password</key><string>%s</string></dict></array></dict></plist>\n" "$A" "$PA" "$PU" > fv-add2.plist; unset PA PU
# harness of MP-E8, stdin fv-enable.plist
CMD='stat -f %Su /dev/console; sw_vers; /usr/bin/fdesetup enable -inputplist > /var/root/hk-proof/recovery-key.txt; echo rc=$?; /usr/bin/fdesetup status'
/usr/bin/fdesetup status; /usr/sbin/diskutil apfs list | grep -i -E 'FileVault|Encrypted|Conversion'   # until it says On
# add, form 1 (plist = fv-enable.plist, the admin alone); stdin fv-enable.plist, then form 2 (fv-add2.plist) only if it failed
CMD='stat -f %Su /dev/console; sw_vers; /usr/bin/fdesetup add -usertoadd hkproof -inputplist; echo rc=$?; /usr/bin/fdesetup list'
/usr/bin/fdesetup list; /usr/sbin/diskutil apfs listCryptoUsers /; /usr/bin/fdesetup status
rm -P fv-enable.plist fv-add2.plist recovery-key.txt
```

Record: the enable exit and any error (not the key); the time to `FileVault is On.`; the add exit and stderr; which form worked; the `hkproof,<UUID>` line; whether the hidden account is in `listCryptoUsers`.
Decides:
- Form 1 works -> the update-account code (beside `maintenance_account.rs`) runs `fdesetup add -usertoadd <name> -inputplist` with the admin's credentials for that call only; add an optional `filevault_user` fact to `UpdateAccountState` in `maintenance.proto` (B8).
- Form 2 needed -> the same with `AdditionalUsers`.
- Add refuses a hidden or token-less account -> change the creating order (create, token, FileVault add, hide last).
- Volume owner error -> reset the Mac; if it repeats on a fresh one, FileVault-on Macs are out of scope (question 4) and the `macos` gate stays shut for them.
- Enable needs a person at the screen -> the farm never turns FileVault on itself; the design says it is an operator step.

### MP-A8 Authenticated restart

Settles section 5.5 line 477-478 ([V]: `fdesetup authrestart -inputplist` with the plist on stdin restarts with no pre-boot login). Root shell, the owner watching the console or KVM. Needs A7 (FileVault on, `hkproof` listed) and A6 true. The Mac must be reachable over the network after boot with nobody typing. **Irreversible**: the real run and the control run reboot the Mac. Design argv, not built (`fdesetup authrestart -inputplist`). Each launchd plist stays in `/var/root/hk-proof`, never `/Library/LaunchDaemons`, so the job does not run again after the reboot.

```sh
cd /var/root/hk-proof; umask 077; PU=$(cat pw-proof)
printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>Username</key><string>hkproof</string><key>Password</key><string>%s</string></dict></plist>\n' "$PU" > fv-auth.plist; unset PU
printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>Username</key><string>hkproof</string><key>Password</key><string>not-the-password</string></dict></plist>\n' > fv-auth-bad.plist
# dry run (-delayminutes -1 stores the key, no restart): harness of MP-E8, stdin fv-auth.plist
CMD='stat -f %Su /dev/console; sw_vers; /usr/bin/fdesetup authrestart -delayminutes -1 -inputplist; echo rc=$?'
# negative: the same CMD, stdin fv-auth-bad.plist
sysctl -n kern.boottime kern.bootsessionuuid | tee boot-before.txt
# real: stdin fv-auth.plist; bootout and rm -P the dry-run and negative jobs' plists first, and let this job remove its own plist before the restart
CMD='stat -f %Su /dev/console; sw_vers; /bin/rm -P /var/root/hk-proof/<name>.plist; /usr/bin/fdesetup authrestart -inputplist; echo rc=$?'
# after it is back
sysctl -n kern.boottime kern.bootsessionuuid | tee boot-after.txt; diff boot-before.txt boot-after.txt
log show --last 10m --predicate 'process == "fdesetup"' --style compact | tail -20; /usr/bin/fdesetup status
rm -P fv-auth.plist fv-auth-bad.plist
# control, only after the result is recorded; expected to wait at the pre-boot login
shutdown -r now
```

Record: dry-run exit, stdout, stderr; the wrong-password message and exit; for the real run the exit, delay, time until the network answers, whether a pre-boot prompt showed, the two boot ids, whether it was fully unattended; for the control whether it stopped at the pre-boot login.
Decides:
- Accepted, unattended, exit 0 -> section 5.5 stands; drop the [V]; the update-account code runs `fdesetup authrestart -inputplist` with the plist on a pipe; the reason is step and exit code only.
- Needs a terminal or a file path -> a 0600 file in the daemon's scratch, removed at once, or the A2 pty; edit "on standard input" in the design.
- Works only with the admin as `Username` -> the bootstrap would have to keep the admin password (against mac-bootstrap.md); FileVault-on Macs stay out (question 4).
- A pre-boot login still shows -> remove the FileVault-on branch from the design and the scheduler gate; such a Mac gets `no_authrestart` and is never ordered `macos`.
- The stored key serves one boot only -> say in the design that authrestart is the last action before the reboot, never before the install.

### MP-A9 Leave the Mac as found

Settles nothing; undoes A2 to A8. Root shell. Needs the results copied off the Mac. **Irreversible**: deleting the account and its home, and the secret files, cannot be undone; it is the cleanup of the proof Mac only. Turn FileVault off only if A1 found it off. Design argv, not built.

```sh
cd /var/root/hk-proof; umask 077; A=$(cat admin-name); PW=$(cat pw-admin)
/usr/bin/fdesetup remove -user hkproof; echo "remove rc=$?"
printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>Username</key><string>%s</string><key>Password</key><string>%s</string></dict></plist>\n' "$A" "$PW" > fv-off.plist; unset PW
# harness of MP-E8, stdin fv-off.plist
CMD='stat -f %Su /dev/console; sw_vers; /usr/bin/fdesetup disable -inputplist; echo rc=$?'
/usr/bin/fdesetup status
# b: the account; if it refuses for the token, same CMD shape with -adminUser "$A" -adminPassword - and stdin pw-admin
/usr/sbin/sysadminctl -deleteUser hkproof; echo "rc=$?"
/usr/bin/dscl . -read "$U/hkproof" 2>&1; echo "dscl rc=$? (56 expected)"; /usr/bin/fdesetup list
# c: secrets (the proof's directory stays for the other sections)
rm -P pw-proof pw-admin admin-name in-proof-admin in-admin-proof fv-*.plist recovery-key.txt 2>/dev/null; ls
```

Record: the exits of remove, disable, deleteUser and the final `dscl` (56); `fdesetup list` and `fdesetup status` equal to A1's.
Decides:
- deleteUser refuses or asks for the administrator -> no design change; record the message.
- deleteUser leaves the account in the FileVault list or token holders -> document the order: `fdesetup remove` first.

## 5. macOS updates as root from launchd (section 5.5)

These steps show whether the design's macOS path works: `softwareupdate` and `startosinstall`, run as root from launchd with nobody logged in, with the update account's password on standard input. Nothing here is built. The darwin apply of the maintenance hook has no `--install`, `--download`, `--fetch-full-installer` or `startosinstall` argv yet; the code only has `--list` (`crates/hostkeeper-daemon/src/maintenance_macos.rs:103-108`) and the Command Line Tools `--install` of `xcode_clt.rs`. So every update argv below is "design argv, not built" (`docs/design/maintenance-updates.md sections 5.2 and 5.5`). Documentation notes are "expected", never results.

Built pieces used: the account file (`crates/hostkeeper-bootstrap/src/update_account.rs:44`, text `name=<n>\npassword=<p>\n` at `:94-100`), the standard-input runner (`crates/hostkeeper-launchd/src/stdin_process.rs:1-9`: password on a pipe, empty environment), the binary path (`xcode_clt.rs:63`), the label read (`maintenance_softwareupdate.rs:9`) and the installer version read (`maintenance_softwareupdate.rs:114`, `maintenance_macos.rs:107-112`).

**Plan.** Run as root on one drained proof Mac (Apple silicon, macOS 26.x, a group of its own, a way to reach it out of band). A Mac consumes one minor update per run and a major upgrade cannot be repeated or rolled back, so the order is fixed:

1. MP-U1 baseline, MP-U2 download, MP-U3 minor update with FileVault off.
2. Turn FileVault on (MP-A7), with the update account a FileVault user.
3. MP-U4 and MP-U5 on later minor updates. Each needs a fresh minor update. If Apple has shipped only one, wait for the next, or use a second proof Mac held on an older minor.
4. Major last: MP-U6 fetch, then MP-U7 (FileVault off) or MP-U8 (FileVault on). They are alternatives on one Mac. Do both only with two Macs on an older major. Before any major, copy `baseline.txt`, `sw_vers` and `fdesetup list` off the Mac (`scp` to `$P` on the operator's Mac), as the old state is gone after.
5. MP-U9 after each install, with no one touching the Mac.

Every job below uses the harness of MP-E8 (one job per step, label `hk-proof-<step>` such as `hk-proof-u3`, `ProgramArguments` `/bin/sh -c '<command>'`, plist in `/var/root/hk-proof`, loaded with `launchctl bootstrap system <path>`, removed with `launchctl bootout system/<label>`). The harness records `stat -f %Su /dev/console` and `sw_vers` inside the job; a run with a console owner other than `root` is not the case the design claims, and the record says so. The `/bin/sh -c` wrapper is the harness only; the daemon would run the argv directly. Set the harness's `StandardInPath` as each step says. After each run copy its output files to `$D/<step>.out` and `$D/<step>.err`, then run `grep -l -F -f "$D/pw.stdin" "$D"/*.out "$D"/*.err` and record that no file is listed (a hit means the password was echoed: delete that file with `rm -P`). Never print a password, never `env`-dump in a step that holds one. Plists stay in `/var/root/hk-proof`, never `/Library/LaunchDaemons`, so a job with `RunAtLoad` cannot run again after the restart. A job whose command restarts the Mac (MP-U3, MP-U4 step 2, MP-U5, MP-U7, MP-U8) also starts, after the console line, with `/bin/rm -P <its own plist>`, so nothing is left for a boot to load.

### MP-U1. Baseline and password file

Settles: 5.5 (the update account, the Secure Token, "nobody logged in"). Who: a root shell on the proof Mac.
Preconditions: the hidden administrator `hkproof` holds a Secure Token (MP-A2, MP-A3); the account file exists, root 0600, naming it (the bootstrap writes it only when given the keys; if it is missing, write it as the first command below does); the harness of MP-E8 exists. Use one root shell; in every new one (after each restart too) paste the preamble first: `D=/var/root/hk-proof; U=/Users; F='/Library/Application Support/hostkeeper/update-account'; ACCT=$(sed -n 's/^name=//p' "$F")`.

```sh
D=/var/root/hk-proof; mkdir -p "$D"; chmod 700 "$D"
[ -e '/Library/Application Support/hostkeeper/update-account' ] || ( umask 077; mkdir -p '/Library/Application Support/hostkeeper'; { printf 'name=hkproof\npassword='; cat "$D/pw-proof"; } > '/Library/Application Support/hostkeeper/update-account' )   # pw-proof from MP-A1; the file is removed in section 7 if the proof wrote it
ACCT=$(sed -n 's/^name=//p' '/Library/Application Support/hostkeeper/update-account')
F='/Library/Application Support/hostkeeper/update-account'; ls -le "$F"; stat -f '%Su:%Sg %Lp' "$F"
{ date -u +%FT%TZ; sw_vers; uname -m; sysctl -n kern.boottime
  fdesetup status; fdesetup supportsauthrestart; fdesetup list
  diskutil apfs listUsers /; sysadminctl -secureTokenStatus "$ACCT"
  dscl . -read "$U/$ACCT" IsHidden; stat -f 'console: %Su' /dev/console
  defaults read /Library/Preferences/com.apple.loginwindow autoLoginUser
  profiles status -type enrollment; softwareupdate --list; softwareupdate --list-full-installers
} > "$D/baseline.txt" 2>&1; cat "$D/baseline.txt"
CU=$(stat -f %Su /dev/console)   # log a console user out; undone by logging in again
[ "$CU" != root ] && [ "$CU" != _mbsetupuser ] && launchctl bootout "gui/$(id -u "$CU")"
sleep 5; stat -f 'console now: %Su' /dev/console
```

Record: `baseline.txt` whole, the mode line of the account file, the console owner before and after, whether auto-login is set, whether the Mac is MDM-enrolled.
Decides:
- if `hkproof` is not an administrator with a Secure Token -> stop; fix the account in MP-A2 and MP-A3, nothing below means anything.
- if the Mac is MDM-enrolled -> design 5.5.1 and 5.5.2 cite it as a limit of every result below.
- if `supportsauthrestart` is false with FileVault on -> keep design section 5.5 (such a Mac is not ordered macos) and record why.

Each later step that needs the password builds `$D/pw.stdin` first and removes it last:

```sh
( umask 077; sed -n 's/^password=//p' "$F" > "$D/pw.stdin" )   # password plus one newline, as the daemon sends
rm -P "$D/pw.stdin"                                              # last line of the step
```

Reversible. Remove `baseline.txt` with `rm` when the proof ends (named files only).

### MP-U2. Download a minor update

Settles: 5.5, step 1 stage (design sections 5.2 and 5.5). Who: a root shell on the proof Mac.
Preconditions: MP-U1; `softwareupdate --list` names a minor update (same major, higher minor or patch); free space on the data volume at least twice the download.

```sh
LABEL=$(softwareupdate --list 2>&1 | sed -n 's/^\* Label: //p' | head -1); echo "[$LABEL]"   # the `* Label:` line maintenance_softwareupdate.rs reads (module doc, :9-13); set by hand if the first is not macOS
# harness command, StandardInPath /dev/null (design argv, not built):
/usr/sbin/softwareupdate --download "<LABEL>"
```

Poll for a number after `last exit code = ` in `launchctl print system/hk-proof-u2` (a job not yet run prints `(never exited)`), not for `not running`, which can match before the job starts. Then `softwareupdate --list; df -h /System/Volumes/Data`.
Record: exit code, stdout and stderr, label, seconds, free space before and after, how `--list` shows the update afterwards.
Decides:
- if exit 0 and MP-U3 has no download phase -> design section 5.2 stands; the prepare runs exactly this argv.
- if it needs a user or password -> step 1 passes `--user <account> --stdinpass` through the standard-input runner; change design section 5.2.
- if exit 0 but MP-U3 downloads again -> drop the download from step 1 for a minor update, keep the free-space check; change design sections 5.2 and 5.5.
- if it only runs with a user at the console -> the Mac group must auto-login the fetcher account, or step 1 is skipped for macos; record in design 5.5.

Reversible: a staged update can stay.

### MP-U3. Minor update, FileVault off

Settles: 5.5 macOS bullet. Who: a root shell on the proof Mac, then the coordinator side for the heartbeat time (`hk`, host page, <node-id>).
Preconditions: MP-U2; `fdesetup status` says `FileVault is Off.`; nobody logged in; someone can reach the Mac out of band.

```sh
( umask 077; sed -n 's/^password=//p' "$F" > "$D/pw.stdin" )
date -u +%FT%TZ | tee "$D/u3.start"; sysctl -n kern.boottime | tee "$D/boottime.before"
# harness command, StandardInPath "$D/pw.stdin" (design argv, not built):
/usr/sbin/softwareupdate --install "<LABEL>" --agree-to-license --restart --user <ACCT> --stdinpass
# the Mac restarts; after it is back, in a new root shell, the preamble of MP-U1, then:
sw_vers; sysctl -n kern.boottime; cat "$D/boottime.before"; launchctl list | grep -i hostkeeper
tail -n 80 /var/log/install.log; rm -P "$D/pw.stdin"
```

If the job ends without a reboot, read `launchctl print system/hk-proof-u3 | grep -E 'state|last exit code'` and the output files.
**Irreversible**: the update stays installed and the Mac restarts.
Record: `sw_vers` before and after, exit code if no reboot, output files, seconds from `u3.start` to the first heartbeat (from the coordinator), whether the restart needed any input.
Decides:
- if it installs, restarts and heartbeats with no screen -> design section 5.5 stands; set `return_secs` for a Mac from the measured time plus margin (design section 3 has 3600).
- if it fails on authentication, "not authorized" or the Secure Token -> the daemon path is not live; recheck MP-U1, then the daemon path of section 5.5 is not live: macOS updates stay by hand until device management can send them (5.5.1), and MU8 is not built as designed.
- if it prompts `Password:` on a terminal -> `--stdinpass` does not read the pipe; the daemon needs a pseudo-terminal variant of `stdin_process.rs` (design section 5.5).
- if it installs but does not restart -> `--restart` is not honoured from launchd; the daemon runs `shutdown -r now` after a clean exit (design section 5.2).
- if it restarts but stops on a screen -> see MP-U9.

### MP-U4. Minor update, FileVault on, authenticated restart

Settles: 5.5 FileVault bullet (design sections 5.5 and 5.2). Who: a root shell on the proof Mac.
Preconditions: FileVault on (MP-A7) and `fdesetup list` names `hkproof`; `supportsauthrestart` true; a fresh minor update downloaded (MP-U2); nobody logged in; someone can type a volume owner's password at the pre-boot screen if this fails.

```sh
fdesetup status; fdesetup supportsauthrestart; fdesetup list | grep -q "^$ACCT," && echo fv-user || echo NOT-fv-user
( umask 077; sed -n 's/^password=//p' "$F" > "$D/pw.stdin"
  PW=$(cat "$D/pw.stdin")   # shell variable, not exported; printf is a builtin, so no argv
  printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>' "<key>Username</key><string>$ACCT</string>" "<key>Password</key><string>$PW</string></dict></plist>" > "$D/fd-auth.plist"; unset PW )
sysctl -n kern.boottime | tee "$D/boottime.before"
# 1. harness command, StandardInPath "$D/pw.stdin" (design argv, not built); expected: no restart
/usr/sbin/softwareupdate --install "<LABEL>" --agree-to-license --user <ACCT> --stdinpass
# 2. harness command, StandardInPath "$D/fd-auth.plist" (design argv, not built); the Mac restarts
/usr/bin/fdesetup authrestart -inputplist
# after it is back, in a new root shell:
sw_vers; fdesetup status; launchctl list | grep -i hostkeeper
log show --last boot --info --predicate 'eventMessage CONTAINS[c] "authenticated restart" OR process == "fdesetup"' | tail -n 40
rm -P "$D/pw.stdin" "$D/fd-auth.plist"   # also run this if either step fails
```

The password must not hold `&` or `<` (XML), or escape them.
**Irreversible**: the update stays installed and the Mac restarts.
Record: the `fdesetup` lines before, the install's exit code and text, whether the install restarted by itself, the `authrestart` exit code and text, whether the Mac returned with no keystroke (the coordinator's heartbeat time, not a guess), `sw_vers` after, the log lines.
Decides:
- if the install exits 0 without restarting, `authrestart` exits 0 and the Mac heartbeats at the new version -> design section 5.5 stand; keep "not ordered until the account is a FileVault user and `supportsauthrestart` is true" (`maintenance_account.rs`, `maintenance_macos.rs`).
- if `authrestart` fails to authenticate -> add `fdesetup add` of the account to the account step (design section 5.5, mac-bootstrap.md section 4), or the plist keys differ on 26; record the keys that worked.
- if it stops at the pre-boot login -> a Mac with FileVault on is never ordered macos (design section 5.5); question 4 (FileVault off on build Macs) becomes a requirement.
- if the install restarts the Mac itself, or applies only at the next boot -> the two steps are not separable; rewrite design sections 5.5 and 5.2 around what happened.

### MP-U5. Is `--restart` alone enough with FileVault on

Settles: 5.5 FileVault bullet (design section 5.5 assume it stops at the pre-boot login). Who: a root shell on the proof Mac. Optional and risky: run last of the minors, and only with a person at the Mac, because a failing result strands it at the pre-boot screen.
Preconditions: as MP-U4, and a second fresh minor update staged.

```sh
( umask 077; sed -n 's/^password=//p' "$F" > "$D/pw.stdin" ); date -u +%FT%TZ | tee "$D/u5.start"
# harness command, StandardInPath "$D/pw.stdin" (the MP-U3 argv with FileVault on; design argv, not built):
/usr/sbin/softwareupdate --install "<LABEL>" --agree-to-license --restart --user <ACCT> --stdinpass
rm -P "$D/pw.stdin"   # after the Mac is back or the person has typed
```

**Irreversible**: reboot and install.
Record: whether the Mac reached the network without a keystroke; if not, how long it waited; output files; `sw_vers` after.
Decides:
- if it returns by itself -> drop the `fdesetup` step from design section 5.5; one call, the MP-U3 argv, for FileVault on and off.
- if it stops at the pre-boot login -> design section 5.5 stand; keep MP-U4's two calls.
Expected (Apple's documentation): `man softwareupdate` does not say whether `--restart` authenticates the restart (expected: not found in documentation; do not assume).

### MP-U6. Fetch the full installer for a major upgrade

Settles: 5.5, step 1 stage (design sections 5.5 and 5.2; a superseded version answers "Update not found", design section 1). Who: a root shell on the proof Mac.
Preconditions: MP-U1; the Mac is on an older major than the newest (`softwareupdate --list-full-installers` shows a higher major than `sw_vers`; if not, there is no major to prove); free space at least twice the installer (expected: about 15 to 20 GB, recalled, not checked).

```sh
softwareupdate --list-full-installers 2>&1 | tee "$D/full-list.txt"
VER=$(sed -n 's/.*Version: \([0-9][0-9.]*\).*/\1/p' "$D/full-list.txt" | head -1); echo "[$VER]"   # edit if not the next major
# harness command, StandardInPath /dev/null (design argv, not built):
/usr/sbin/softwareupdate --fetch-full-installer --full-installer-version "<VER>"
ls -ld /Applications/Install\ macOS*.app; du -sh /Applications/Install\ macOS*.app
codesign -dv --verbose=2 /Applications/Install\ macOS*.app 2>&1 | grep -E 'Authority|Identifier|TeamIdentifier'
/Applications/Install\ macOS*.app/Contents/Resources/startosinstall --usage 2>&1 | head -40
df -h /System/Volumes/Data
```

Reversible: delete that one app (`rm -r` of the single app) to undo.
Record: exit code, output files, exact app name and path, size, signing authority, seconds taken, free space before and after, the `--usage` text (the flags of MP-U7 must exist in it), whether it worked with nobody logged in.
Decides:
- if exactly one `/Applications/Install macOS <name>.app` appears, signed by Apple -> design section 5.5 stands; the prepare passes the path by glob `/Applications/Install macOS *.app` and refuses more than one; deadline is the measured seconds plus margin.
- if it fails as root from launchd -> a major cannot be staged by the daemon; `--macos-major` stays off and design section 5.5 changes.
- if "Update not found" for a listed version -> the collector's version string (`maintenance_softwareupdate.rs:114`) needs a build or another field; note the string that worked.
- if the app lands elsewhere -> change the path in design section 5.5, the apply and the verify's removal.

### MP-U7. Major upgrade, FileVault off

Settles: 5.5 macOS bullet (design section 5.5). Who: a root shell on the proof Mac, the coordinator side for the heartbeat. Alternative to MP-U8 on one Mac.
Preconditions: MP-U6; FileVault off; nobody logged in; `baseline.txt`, `sw_vers` and `fdesetup list` already copied off the Mac; the owner agreed this Mac may be reinstalled; out-of-band access ready. Expected: 30 to 90 minutes with several restarts, silent to the coordinator for that long (recalled, not checked).

```sh
INST=$(ls -d /Applications/Install\ macOS*.app | head -1); ls -d /Applications/Install\ macOS*.app | wc -l
( umask 077; sed -n 's/^password=//p' "$F" > "$D/pw.stdin" ); sysctl -n kern.boottime | tee "$D/boottime.before"
# harness command, StandardInPath "$D/pw.stdin" (design argv, not built):
"<INST>/Contents/Resources/startosinstall" --agreetolicense --nointeraction --user <ACCT> --stdinpass
# after 60 s: the output files and launchctl print; when the Mac is back, in a new root shell:
sw_vers; cat "$D/boottime.before"; ls -d /Applications/Install\ macOS*.app; launchctl list | grep -i hostkeeper; tail -n 120 /var/log/install.log
rm -P "$D/pw.stdin"
```

**Irreversible**: a major upgrade has no downgrade, only erase and reinstall.
Record: progress or error text, exit code if no reboot, number of restarts, minutes to the first heartbeat (coordinator), `sw_vers` before and after, whether a person touched the Mac, whether the installer app remains.
Decides:
- if it upgrades and heartbeats with nobody touching it -> design section 5.5 stand for FileVault off; keep installer removal in verify (design section 5.5); give a major its own `return_secs` if it exceeds 3600.
- if it fails on authentication or the Secure Token -> the daemon cannot upgrade a major; `--maintenance-macos-major` stays off, the major moves to the device-management path (5.5.1) or manual; rewrite design section 5.5.
- if it asks for `--forcequitapps` -> add it to the argv in the design and the apply; record why it appeared with nobody logged in.
- if it ends on a Setup Assistant pane -> see MP-U9.
- if it fails after starting (recovery, boot loop) -> the Mac needs a manual reinstall; no unattended major until a retry story exists.

### MP-U8. Major upgrade, FileVault on

Settles: 5.5 FileVault bullet (design section 5.5: "shown before a major upgrade runs on such a Mac"; until then a major on a FileVault-on Mac is not ordered). Who: as MP-U7. Alternative to MP-U7 on one Mac.
Preconditions: MP-U7's, but FileVault on (MP-A7), `hkproof` a FileVault user, `supportsauthrestart` true; a person can type a FileVault user's password at the pre-boot screen. From another machine, ping or ssh the Mac every minute and note the time of each state.

```sh
fdesetup status; fdesetup list; fdesetup supportsauthrestart
# same argv as MP-U7, no fdesetup call (design argv, not built); harness StandardInPath "$D/pw.stdin":
"<INST>/Contents/Resources/startosinstall" --agreetolicense --nointeraction --user <ACCT> --stdinpass
# when back: sw_vers; fdesetup status; the output files; then
log show --last boot --info --predicate 'eventMessage CONTAINS[c] "authenticated restart" OR eventMessage CONTAINS[c] "startosinstall"' | tail -n 60
rm -P "$D/pw.stdin"
```

**Irreversible**: the upgrade, and a possible stop at the pre-boot login. Expected (recalled, not checked): community reports say `startosinstall` with `--user`/`--stdinpass` restarts authenticated; Apple's startosinstall documentation does not say. Do not rely on it.
Record: whether anyone typed at the pre-boot screen, at which restart, minutes silent, whether the network came back by itself, output files, log lines, `sw_vers` after.
Decides:
- if it came back by itself at the new major -> design section 5.5 closes; remove "not ordered until shown" at design section 5.2; the coordinator's order decision allows a major with FileVault on when `supportsauthrestart` and the FileVault-user rule hold (design section 5.5).
- if a person had to type -> a major on a FileVault-on Mac stays not ordered (design sections 5.2 and 5.5); question 4 becomes a requirement; any other route is a design change.
- if only the first restart passed -> treat as not ordered; record which restart stopped.
- if it refuses because `--user` is not a FileVault user -> add `fdesetup add` to the account step (mac-bootstrap.md section 4) and rerun.

### MP-U9. Does an install stop on a screen

Settles: 5.5 macOS bullet (design sections 5.2 and 5.5: the Mac returns to a heartbeating state with nobody at the screen). Who: a root shell on the proof Mac, the coordinator side for times, and a person or the display for the final screen.
Preconditions: after each of MP-U3, MP-U4, MP-U7 or MP-U8, without touching the Mac. Note whether auto-login of the fetcher account is set (MP-U1): if so, post-update panes belong to its first login at boot.

```sh
D=/var/root/hk-proof; date -u +%FT%TZ; sw_vers; uptime; stat -f 'console: %Su' /dev/console
scutil <<< "show State:$U/ConsoleUser"; launchctl managername; launchctl list | grep -i hostkeeper
ps -axo user,pid,etime,command | grep -i -E 'Setup Assistant|SetupAssistant|loginwindow|softwareupdate' | grep -v grep
for u in $(dscl . -list /Users | grep -v '^_' | grep -v -E '^(daemon|nobody|root)$'); do echo "== $u"; defaults read "$U/$u/Library/Preferences/com.apple.SetupAssistant" 2>&1 | head -20; done
log show --last 2h --info --debug --predicate 'process == "Setup Assistant" OR process == "loginwindow" OR process == "softwareupdated" OR subsystem == "com.apple.SoftwareUpdate"' > "$D/post-update-log.txt" 2>&1
grep -i -E 'wait|blocked|user input|consent|terms|pane|FileVault|authenticat' "$D/post-update-log.txt" | tail -n 80
tail -n 200 /var/log/install.log | grep -i -E 'setup|finish|complete|restart|user'
```

On the coordinator side copy the time of the first heartbeat after each restart, the reported macOS version, and whether the host stays in `maintenance_not_back`. Compare `LastSeenBuddyBuildVersion` with the build in `sw_vers`. If an account is on the console, a screenshot through `launchctl asuser` and `screencapture -x` (a screen-recording permission may block it: record that); with nobody on the console ask someone at the Mac.
Reversible: read-only except the one screenshot file.
Record: per install run, the console owner, the SetupAssistant keys against the new build, the processes, the log lines, any screenshot, and one of (a) heartbeat and everything settled with nobody at the screen, (b) heartbeat but a pane waits at the next login, (c) the Mac stopped before the daemon ran; with times.
Decides:
- if (a) for minor and major -> design sections 5.2 and 5.5 stand; record the builds proved.
- if (b) -> the daemon works but the logged-in fetcher account (design section 2.2) is blocked; handle the pane once per major or set the Setup Assistant skip keys (name those that worked) in the apply step.
- if (c) -> unattended update is false for that case; macos is not ordered for it, design sections 5.5 and 5.2 state it, and the order decision gets a reason such as `macos_needs_screen`.
- if a consent pane for an Apple feature appears -> list the pane and its defaults key; add code only if it blocks a LaunchDaemon (expected: it does not).
Expected: Apple does not document headless installs (not found in documentation).

## 6. Device-management settings (sections 5.8 and 5.5.1)

These steps settle the [V] marks of `docs/design/maintenance-updates.md` sections 5.8 and 5.5.1: where the Mac shows each
device-management setting, and what the daemon's `maintenance_mdm.rs` reads. Nothing here installs an update.
The owner makes every setting in the Apple Business console on the one proof Mac alone in a Blueprint of its own; a root shell
on the proof Mac only reads. Output goes to `/var/root/hk-proof/mdm` (root, 0700). Every expectation below is stated as
"expected (recalled, not checked)" unless a source is named; no result is recorded in this document.

### MP-M1. Baseline and the shared collector (5.8)

- Settles: 5.8, that a Mac with none of the settings reports none, and that the proof's reads are the daemon's reads.
- Runs: a root shell on the proof Mac. Precondition: the Mac is drained, enrolled, and no other Mac or farm host is touched.
- Argv: `softwareupdate --list` is the daemon's own (`LIST_ARGS`, `crates/hostkeeper-daemon/src/maintenance_macos.rs:31`). The
  paths are `MANAGED_PREFERENCES` and `DDM_STATE` (`crates/hostkeeper-daemon/src/maintenance_mdm.rs:47,50`); the `profiles`,
  `plutil`, `defaults` and `log` calls are design argv, not built.

```sh
mkdir -p /var/root/hk-proof/mdm && chmod 700 /var/root/hk-proof/mdm && cd /var/root/hk-proof/mdm
{ sw_vers; softwareupdate --version 2>&1 | head -1; date -u; } > baseline-env.txt 2>&1
# the snapshot: a function of this root shell, written to no file; `snap LABEL` reads only and writes /var/root/hk-proof/mdm/LABEL/*
snap() {
  local D=/var/root/hk-proof/mdm/$1; mkdir -p "$D"
  { date -u; sw_vers; stat -f %Su /dev/console; } > "$D/when.txt" 2>&1
  ls -laeO@ "/Library/Managed Preferences" > "$D/mp-ls.txt" 2>&1
  for f in com.apple.applicationaccess com.apple.SoftwareUpdate; do
    p="/Library/Managed Preferences/$f.plist"
    if [ -e "$p" ]; then plutil -p "$p" > "$D/$f.txt" 2>&1; file "$p" > "$D/$f.type.txt"; else echo ABSENT > "$D/$f.txt"; fi
  done
  DDM=/var/db/softwareupdate/SoftwareUpdateDDMStatePersistence.plist
  ls -la /var/db/softwareupdate > "$D/swu-dir.txt" 2>&1
  if [ -e "$DDM" ]; then file "$DDM" > "$D/ddm.type.txt"; plutil -p "$DDM" > "$D/ddm.txt" 2>&1; else echo ABSENT > "$D/ddm.txt"; fi
  profiles status -type enrollment > "$D/profiles-status.txt" 2>&1
  profiles show -type configuration > "$D/profiles-show-config.txt" 2>&1
  profiles show -output stdout-xml > "$D/profiles-show-xml.txt" 2>&1
  profiles list -output stdout-xml > "$D/profiles-list-xml.txt" 2>&1
  ls -laR /var/db/ConfigurationProfiles > "$D/cfgprofiles-ls.txt" 2>&1
  defaults read /Library/Preferences/com.apple.SoftwareUpdate > "$D/defaults-swu.txt" 2>&1
  log show --last 15m --style compact --predicate 'process == "remotemanagementd" OR process == "softwareupdated" OR process == "mdmclient"' > "$D/log.txt" 2>&1
  softwareupdate --list > "$D/swu-list.txt" 2>&1; echo "exit=$?" >> "$D/swu-list.txt"
}
snap 00-baseline
```

- Record: all of `00-baseline` and `baseline-env.txt` (the `profiles` and `log` files can hold the management server's address and enrolment identifiers: keep every snapshot on the Mac, and copy one off only after the owner has removed those lines): whether the DDM file and either Managed Preferences file exist on a clean
  Mac, each file's keys, type (XML or binary), owner and mode, and whether `profiles show` needs root.
- Decides:
  - if the DDM file is absent -> no change (an absent file adds nothing, `maintenance_mdm.rs:127-130`).
  - if a file is unreadable by root or is not a dictionary at the top -> the whole look fails on every Mac (`maintenance_plist.rs`, look_failed); change `HostPlists` or the path.
  - if a clean Mac already reports keys `is_on` counts as on -> tighten `is_on` and `named_on` in `maintenance_mdm.rs` to the real value types.
- Expected (recalled, not checked): the managed files are written by the device-management client; the DDM state file is undocumented by Apple. Reversible: read-only; delete the directory when done.

### MP-M2. Restrictions payload keys (5.8)

- Settles: 5.8, that `forceDelayedSoftwareUpdates`, `forceDelayedMajorSoftwareUpdates`, `forceDelayedAppSoftwareUpdates` and the four `enforcedSoftwareUpdate*Delay` keys appear in `/Library/Managed Preferences/com.apple.applicationaccess.plist` (`RESTRICTION_KEYS`, `maintenance_mdm.rs:67-76`; read at `:131`).
- Runs: the owner in the console, then a root shell. Precondition: MP-M1. The owner adds one setting at a time and removes it, waiting for the Mac to drop it before the next: A "force delayed software updates" with 7 days, B major updates, C app updates. The owner notes which console wording gives which key. If the console has no setting for a key, record that and use no custom profile unless the owner decides.

```sh
cd /var/root/hk-proof/mdm
# wait for the Mac's own check-in (no `profiles renew`: the root shell only reads), then:
snap 10-restrictions-A-set
PL="/Library/Managed Preferences/com.apple.applicationaccess.plist"
for k in forceDelayedSoftwareUpdates forceDelayedMajorSoftwareUpdates forceDelayedAppSoftwareUpdates enforcedSoftwareUpdateDelay enforcedSoftwareUpdateMajorOSDeferredInstallDelay enforcedSoftwareUpdateMinorOSDeferredInstallDelay enforcedSoftwareUpdateNonOSDeferredInstallDelay; do
  printf '%s: ' "$k"; plutil -type "$k" "$PL" 2>&1; plutil -extract "$k" raw "$PL" 2>&1
done
softwareupdate --list 2>&1 | tee swu-list-under-deferral.txt   # argv as maintenance_macos.rs:31
```

- Record: per step the console wording, seconds from console write to the file appearing, the full `plutil -p`, each key's type and value, any other key, file type, owner and mode, the `softwareupdate --list` output against the baseline, and the log lines. Repeat with labels `11-B-set`, `12-C-set` and a `-removed` snapshot after each removal.
- Decides:
  - if a key appears with the exact name and a true boolean or positive integer -> keep `RESTRICTION_KEYS`; record in design 5.8.
  - if a key has another name, or an unset key reads 0 -> correct `RESTRICTION_KEYS` (`maintenance_mdm.rs:68-76`) and the list in design 5.8; `is_on` already reads 0 as off.
  - if the file appears only under a per-user subdirectory -> `managed()` reads the top level only; extend `maintenance_mdm.rs` or accept the gap in 5.8.
  - if the keys never arrive or the console cannot set them -> mark "not reproducible" in 5.8; keep the read.
  - if a deferral hides nothing from `softwareupdate --list` -> reword the 5.8 sentence that any period hides updates.
- Expected (recalled, not checked): the keys are the restrictions payload's, deprecated in macOS 26 in favour of the software-update settings declaration, with a 90-day limit. Reversible: the owner removes the setting; confirm with `snap` that the keys are gone before the next step.

### MP-M3. com.apple.SoftwareUpdate keys (5.8)

- Settles: 5.8, that `CatalogURL`, `AllowPreReleaseInstallation`, `AutomaticallyInstallMacOSUpdates` and `restrict-software-update-require-admin-to-install` appear in `/Library/Managed Preferences/com.apple.SoftwareUpdate.plist` (`SOFTWARE_UPDATE_KEYS`, `maintenance_mdm.rs:78-84`; read at `:132`). The code does not read `/Library/Preferences/com.apple.SoftwareUpdate.plist`.
- Runs: the owner, then a root shell. Precondition: MP-M1. The owner adds one at a time and removes it: A "automatically install macOS updates"; B "require administrator to install updates" if offered; C a custom profile with `CatalogURL` set to Apple's own catalog address, only if the owner agrees to upload one (no console setting is expected for `CatalogURL` or `AllowPreReleaseInstallation`). Step C changes what the Mac sees: remove it at once.

```sh
cd /var/root/hk-proof/mdm
sleep 60; snap 20-swu-A-set
PL="/Library/Managed Preferences/com.apple.SoftwareUpdate.plist"
for k in CatalogURL AllowPreReleaseInstallation AutomaticallyInstallMacOSUpdates restrict-software-update-require-admin-to-install; do
  printf '%s: ' "$k"; plutil -type "$k" "$PL" 2>&1; plutil -extract "$k" raw "$PL" 2>&1
done
defaults read /Library/Preferences/com.apple.SoftwareUpdate > swu-system-prefs-now.txt 2>&1
softwareupdate --list 2>&1 | tee swu-list-now.txt
```

- Record: console wording per key, key name, type and value, mode and owner, whether the system preference domain shows it too, and `softwareupdate --list` before and after for `CatalogURL`. Labels `21-B-set`, `22-C-set`, `-removed` after each.
- Decides:
  - if names and types match and are on -> keep `SOFTWARE_UPDATE_KEYS`.
  - if the console choice writes other names -> edit `SOFTWARE_UPDATE_KEYS` (`maintenance_mdm.rs:79`) to the names that mean "installs without us"; do not report benign ones such as `AutomaticCheckEnabled` true.
  - if `AutomaticallyInstallMacOSUpdates` false is delivered -> no change (`is_on` reads false as off).
  - if the value appears only in `/Library/Preferences/com.apple.SoftwareUpdate.plist` -> add that file as a read source in `maintenance_mdm.rs`.
- Expected (recalled, not checked): the four keys belong to the com.apple.SoftwareUpdate payload. Reversible: the owner removes the setting; if the listing is wrong after step C, `softwareupdate --clear-catalog` (root) restores the default catalog.

### MP-M4. Declarations in the DDM state file (5.8) [V]

- Settles: 5.8, the undocumented `SoftwareUpdateDDMStatePersistence.plist`: `TargetOSVersion` or `TargetBuildVersion` anywhere in it shows an enforced target; keys under `SUCorePersistedStatePolicyFields` -> `SUCoreDDMDeclarationGlobalSettings` show deferrals, automatic actions and a beta setting (`maintenance_mdm.rs:55-65,162-211`; every key is [V] in the file header, lines 7-9).
- Runs: the owner, then a root shell. Precondition: MP-M1. One at a time, removing between: E "enforce a specific version", a version newer than installed with a deadline 14 days out (never a deadline in the past or within hours; the owner removes it within the hour); D deferral of minor, major and non-OS by N days; AA automatic download and install of OS and security updates; B beta updates, if the console shows it. The owner records the exact console label and the time of each write.

```sh
cd /var/root/hk-proof/mdm
sleep 90; snap 30-ddm-E-set
diff 00-baseline/ddm.txt 30-ddm-E-set/ddm.txt
grep -n 'TargetOSVersion\|TargetBuildVersion\|TargetLocalDateTime\|DetailsURL\|SUCorePersistedStatePolicyFields\|SUCoreDDMDeclarationGlobalSettings' 30-ddm-E-set/ddm.txt | head -50
cat 30-ddm-E-set/ddm.type.txt
log show --last 30m --style compact --predicate 'process == "remotemanagementd" OR process == "softwareupdated" OR process == "mdmclient"' 2>&1 | grep -i 'declar\|softwareupdate\|enforce\|target' | head -80 > log-declarations.txt
ls -la /var/db/RemoteManagement* /var/db/softwareupdate 2>&1 | head -60
```

- Record: for E, D, AA and B the console label, the full `plutil -p` of the file and its diff against the baseline, each key's name, path and value type, the log lines, and whether each key disappears after removal (and how fast) or stays as stale state. Labels `31-D-set`, `32-AA-set`, `33-B-set` and `-removed` after each. Check the dump with the owner before it leaves the Mac; it is expected to hold target versions only.
- Decides:
  - if the target keys appear with E and vanish on removal -> keep `holds_target`; consider anchoring it on the real path.
  - if they stay after removal -> the daemon would report an enforced target forever; make `holds_target` check an active marker named from the file as recorded, or change the source.
  - if the names or file differ -> correct `TARGETS` (`:56`) and `DDM_STATE` (`:50`); if no file shows it, take `profiles show -output stdout-xml` or the log as the source, or declare the enforcement unseen on the Mac in 5.8.
  - if the settings keys have other names or nested shapes -> rewrite `declarations()` (`:162-188`), `POLICY_FIELDS`, `GLOBAL_SETTINGS` and `AUTOMATIC_KEYS` with value-aware checks, since `is_on` makes any present dictionary on.
  - if the file is root-only -> fine for a root daemon; confirm with `ps` on the real daemon.
  - if keys show only as pending, not applied -> record which; treat a pending declaration as a conflict in "no macOS order while one stands".
- Expected (recalled, not checked): the enforcement declaration takes a target version, a local date-time deadline and an optional details URL, on macOS 14 and later, supervised only; the settings declaration holds deferrals, automatic actions and beta. Apple does not say where macOS keeps them.
- Reversible, but a left-over target can force an install and restart: remove E before ending and confirm with `snap` that the listing and the file show it gone.

### MP-M5. Other read routes (5.8)

- Settles: 5.8, that `profiles show -type configuration`, `profiles status`, `profiles show -output stdout-xml`, `/var/db/ConfigurationProfiles` and `log show` can show the same settings if the DDM file proves wrong. Design argv, not built: the daemon runs no `profiles` command.
- Runs: a root shell. Precondition: MP-M2 to MP-M4 done; `snap` already ran these routes at every step. This step only reads.

```sh
cd /var/root/hk-proof/mdm
for d in 00-baseline 10-restrictions-A-set 20-swu-A-set 30-ddm-E-set; do
  echo "== $d"; grep -c . "$d/profiles-show-xml.txt"
  grep -n 'forceDelayed\|enforcedSoftwareUpdate\|CatalogURL\|AllowPreRelease\|TargetOSVersion\|Deferrals\|AutomaticActions\|Beta' "$d"/profiles-show-xml.txt "$d"/profiles-show-config.txt "$d"/profiles-list-xml.txt | head -20
done
grep -rIl 'TargetOSVersion\|forceDelayedSoftwareUpdates' /var/db/ConfigurationProfiles /var/db/softwareupdate /var/db/RemoteManagement 2>/dev/null | head
```

- Record: a table of setting by route (Managed Preferences file, DDM file, `profiles show`, its XML form, `log show`, `softwareupdate --list`) with yes, no, needs root, and seconds of delay.
- Decides:
  - if `profiles show -output stdout-xml` carries payloads the files do not -> add an optional fallback source to `maintenance_mdm.rs` (a runner parsed as a plist) and update 5.8.
  - if nothing beyond the files shows them -> no change; 5.8 says the files are the only routes.
  - if `profiles` needs a user session -> keep files only; the daemon does not use it.
- Expected (recalled, not checked): `man profiles` describes show, status and list, and `show` needs root. Reversible: read-only.

### MP-M6. The same reads from launchd with nobody logged in (5.8)

- Settles: 5.8, that the daemon (root under launchd, no login) sees the same files as a root shell (`collect()`, `maintenance_mdm.rs:126-148`; `HostPlists` opens with `std::fs`).
- Runs: a root shell, over ssh, with the Mac at the login window (note the FileVault state). Precondition: one setting from MP-M4 step E is set again with the owner. No password is used.
- Needs: the Mac and the coordinator on a release with `maintenance-v1` (design 10.1); until then this step cannot be shown, and the record says so.

The launchd view is the daemon's own: its Rust collector (`maintenance_mdm.rs`, root under launchd) reports what it read in the heartbeat
once the Mac runs a release with `maintenance-v1`, so no proof job repeats the reads.

```sh
# in the root shell, with the setting still set:
cd /var/root/hk-proof/mdm && snap 50-root-shell
# on the operator side (P set as in MP-E1), after the daemon's next hourly look or a restart of the daemon:
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> --response-file $P/get-m6.json hosts get --node-id <node-id>
jq '(.host // .) | .heartbeat.maintenance | {mdmConflicts: .mac.mdmConflicts, lookFailed: .lookFailed}' $P/get-m6.json
```

- Record: the console owner when the daemon looked (`root` shows nobody is logged in), the daemon's `mdmConflicts` and `lookFailed` against the root shell's `50-root-shell` files.
- Decides:
  - if they match -> no change.
  - if the daemon reports `look_failed` for a file the root shell reads (for example a privacy block on `/var/db/softwareupdate`) -> the daemon needs another route; add it to 5.8 and change `HostPlists` in `maintenance_plist.rs`.
- Expected (recalled, not checked): a system job runs as root with the same file view. Read-only.

### MP-M7. Is the software-update Configuration the enforcement declaration (5.5.1) [V]

- Settles: 5.5.1 (design section 5.5.1, "is not shown [V]"): whether Apple Business's SOFTWARE_UPDATE Configuration is `com.apple.configuration.softwareupdate.enforcement.specific`. Not built: the `apple-business` value of `--macos-update-via` is refused until the API call exists (5.5.2); the daemon reads only the trace (`maintenance_mdm.rs:55-56,127-130`).
- Runs: the owner and a root shell, reusing MP-M4 step E (far-off deadline), then D, AA and B. The owner also saves a screenshot of the console form and, on the operator's own Mac, reads the Configuration by the API with a GET only; the API key sits in a 0600 file in `$P`, passed with `curl -H @file`, never on a command line, and removed with `rm -P` afterwards.

```sh
cd /var/root/hk-proof/mdm
diff 00-baseline/ddm.txt 30-ddm-E-set/ddm.txt
grep -n 'enforcement\|softwareupdate' 30-ddm-E-set/profiles-show-xml.txt 30-ddm-E-set/log.txt | head -40
softwareupdate --list 2>&1 | tee swu-list-E.txt      # never run softwareupdate --install here
defaults read /Library/Preferences/com.apple.SoftwareUpdate 2>&1 | head -40
```

- Record: the console field names, the typed target and deadline, every place the target appears on the Mac (file, key, value), the saved API JSON, and the mapping of typed values to `TargetOSVersion`, `TargetBuildVersion` and `TargetLocalDateTime`.
- Decides:
  - if the Mac shows the typed target and deadline -> state 5.5.1 as recorded; the Blueprint route stays a known option, still not chosen.
  - if it shows nothing or another key set -> rewrite 5.5.1 (lines 506-509); if the declaration is not `enforcement.specific`, drop `ENFORCED` and `TARGETS` (`maintenance_mdm.rs:55-56,164`) as the detector, and the Blueprint list in Apple Business becomes the only check.
  - if the Configuration installs at its deadline regardless of drain -> confirms the reason for 5.8; no code change.
- Expected (recalled, not checked): the declaration is for macOS 14 and later, supervised; whether Apple Business's Configuration maps one to one onto it is not published, and the design's reading of the API reference says it cannot be created by the API.
- Reversible: the owner removes the Configuration and confirms with `snap` that the declaration is gone. A deadline in the past or near would force an install and restart, so keep it far out and record the time of removal.

## 7. Put the Mac back

Settles: none. Runs: a root shell on the proof Mac, then an operator on the coordinator side with hk. Needs: every proof step done or abandoned; if one left the Mac mid-update or halted, read its state and decide with the owner first, and do not tear down.
```sh
# on the Mac, as root: lift the cordon and remove every proof job (the counts must be 0)
for l in $(launchctl print system | grep -o 'hk-proof-[A-Za-z0-9-]*' | sort -u); do launchctl bootout "system/$l"; done
rm -P /var/root/hk-proof/*.plist /var/root/hk-proof/*.in
launchctl print system | grep -c 'hk-proof'   # must print 0
# remove proof accounts: sysadminctl -deleteUser hkproof (admin password at the prompt)
# if MP-U1 wrote the update-account file (it names hkproof), remove it: rm -P '/Library/Application Support/hostkeeper/update-account'
# copy the *.txt, *.out, *.err files off the Mac, then remove them one by one with rm -P
# P set as in MP-E1
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> host-groups move --node-id <node-id> --host-group <original-group> --reason "Mac proof finished: back in its group"
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> --response-file $P/get-after.json hosts get --node-id <node-id>
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> host-groups remove --host-group mac-proof --reason "Mac proof finished: group not needed"
```
Record: after against `get-before.json` (group, operatorKeys digest, services, macOS version, FileVault, Xcodes, due); no `hk-proof` job; the scheduler's owner confirms leases are taken again and the cordon list is empty; the three coordinator files no longer name `mac-proof` (deploy owner reverses MP-E2).
Decides:
- if the Mac differs from before (an Xcode, FileVault, an account remains) -> list each leftover as a result of the step that made it, and tell the owner before builds resume.
**Irreversible**: `host-groups remove` is final (a new Put re-creates the group); it is refused `not_empty` while a host is in or moving into the group.
