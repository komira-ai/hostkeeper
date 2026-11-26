# The Mac proof: the owner's session

The Mac proof ([mac-proof.md](mac-proof.md)) had six steps only the owner could take. This page is what is left of them after the
lead and the farm took every step that does not need the owner: one short session, in order, each step one click or one command,
with what it unlocks, how long it takes and what the lead does right after it. Everything the session needs is done before it starts
("Before the owner starts"), and why each of the six steps went, stayed or changed is at the end ("What went off the owner's list").
The results go into the design's section 10 ([maintenance-updates.md](../design/maintenance-updates.md#10-proof-results)).

Two rules hold for every step. **No password goes into a command line, a chat or a message**: a password is typed only into the form
that asks for it, in the owner's own browser. **The lead changes a Mac only through a hostkeeper maintenance run that drains it**,
and never reads a Mac's root files with `sudo`: what the lead learns about the proof Mac comes from its daemon's report and from
probe runs ([maintenance.md](maintenance.md#probe-drain-read-put-back)).

In the commands, `<node-id>` is the proof Mac and `<version>` is the Xcode release the coordinator names as waiting.

## Before the owner starts

The lead does each of these, in this order, and checks it, before the session. None needs the owner.

| # | What | Check |
|---|---|---|
| B1 | The coordinator and the proof Mac run a hostkeeper release with the probe kind, the daemon's start that turns macOS's own install off, and the Xcode stage's signature check | the Mac's report lists `MAINTENANCE_KIND_PROBE` in `maintenance.onDemand`, and its daemon's version is that release's or newer |
| B2 | One probe run on the proof Mac ([mac-proof.md](mac-proof.md) MP-E13), and two reads in the administrator's `hk ssh` session, with no `sudo` (both files are readable without root): `defaults read /Library/Preferences/com.apple.SoftwareUpdate AutomaticallyInstallMacOSUpdates` and `stat -f "%Sp %Su %Sg" "/Library/Managed Preferences/dev.hostkeeper.config.plist"` | the probe says `id` uid 0, `managername` `System`, `env` empty; the `defaults` read says `0` (the daemon turned macOS's own install off); the `stat` says who can read the file that carries the Blueprint's hostkeeper settings, the answer step 4 turns on |
| B3 | The proof Mac out of builds for the session: the job scheduler's operator cordon ([mac-proof.md](mac-proof.md) MP-E6), placed for the session and lifted after step 3's check | the job scheduler shows the cordon and no job on the Mac |
| B4 | The proof Mac in a host group of its own, `mac-proof`, with the same packages and versions and `xcode_keep` 1 ([mac-proof.md](mac-proof.md) MP-E3) | `hostGroup` is `mac-proof`, its services unchanged; no other Mac's group keeps Xcode |
| B5 | The Xcode store is there and names the release to download | `xcode list` answers `configured` true and a `waiting` release (its `version` and `build`) |
| B6 | Nothing the Apple Business settings could install: no macOS update due on the proof Mac, no device-management conflict | the Mac's `maintenance.due` holds no `macos`, `maintenance.mac.mdmConflicts` is empty |
| B7 | The lead's box has room for the archive: at least 20 GB free in `~/Downloads` | `df -h ~/Downloads` |

The reads of B1, B2, B4, B5 and B6:

```
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> hosts get --node-id <node-id>
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> xcode list
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> maintenance schedule --kinds probe --host-ids <node-id> --take-last-host true --confirm true --reason "Mac proof: owner session, before"
hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> ssh <node-id> --reason "Mac proof: owner session, two reads without sudo"
```

## The owner's steps

About 25 minutes of the owner's hands, plus the archive's download. Steps 2 and 3 are one sitting in Apple Business; step 3's
removal follows the lead's read, about 15 minutes later. Step 1's download runs while the owner does steps 2 and 3.

### 1. Download the Xcode archive

- **Click.** On the lead's Linux box, in a browser: Apple's developer downloads page, signed in with the organisation's developer
  account (answer the two-factor prompt on the account's trusted device), then the `Xcode_<version>.xip` of the release the lead
  names, saved into `~/Downloads`. Not a beta and not a release candidate: the store refuses those.
- **Unlocks.** The Xcode store, and on the proof Mac, in one maintenance run: Apple's signature checked by `pkgutil` on a real
  archive (MP-X2's positive case), the expansion (MP-I1), the move, licence and first launch (MP-I3), and the install probed by
  js-agent before and after (MP-I5).
- **Time.** About 5 minutes of the owner's hands. The download, about 12 GB, runs by itself (about 16 minutes at 100 Mbit/s).
- **The lead, right after.** Puts the archive into the store with the store's upload route, from the lead's box
  ([xcode-releases.md](xcode-releases.md), "Pushing an archive"): the first four bytes `xar!`, the SHA-256 and size computed there,
  the version from the file's name and the build the coordinator lists for it, and the token in a header read from its file, never
  in an argument. Linux has no `pkgutil` or `xip`: Apple's signature, the version and the build are checked by the proof Mac's
  stage, before anything is installed ([maintenance.md](maintenance.md#the-private-store-xcode_keep-and-xcode_default)). Then one
  run of `xcode` on the proof Mac:

  ```
  hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> xcode list
  hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> maintenance schedule --kinds xcode --host-ids <node-id> --take-last-host true --confirm true --reason "Mac proof: Xcode from the store"
  ```

  A stage that refuses the archive defers the turn and installs nothing; its reason names the check
  (`xcode_archive_not_apple`, `xcode_expand_failed`, `xcode_build_mismatch`, `xcode_signature_invalid`). The lead records the run
  and the Mac's `maintenance.xcodes` after it in section 10.

### 2. Apple Business: the proof Mac on a Blueprint of its own

- **Click.** In Apple Business, duplicate the Blueprint the proof Mac is on (every Configuration and App comes with the copy), name
  the copy `mac-proof`, and assign the proof Mac, alone, to it.
- **Unlocks.** Step 3 on one Mac: a software-update setting made on the copy reaches no build Mac.
- **Time.** About 5 minutes, and the Mac's next check-in (minutes).
- **The lead, right after.** A probe run (the command of "Before the owner starts"; the daemon looks again as soon as the turn ends)
  and `hosts get`: the Mac still heartbeats, B2's `stat` still finds hostkeeper's settings file, `mdmConflicts` is empty,
  every service is as before. A difference means the copy is not the original: the owner assigns the Mac back.

### 3. Apple Business: the software-update settings, then their removal

- **Click.** On the `mac-proof` Blueprint, in one sitting, each of these. Every setting is one the design's section 5.8 counts as a
  conflict, so each one the Mac shows appears in its report by name:
  - the software-update settings: deferrals of minor, major and other updates (7 days); automatic download and install of macOS
    and of security updates; beta updates, if offered;
  - an enforcement of a specific macOS version: the version the Mac runs now (`sw_vers` in the probe), the deadline 14 days out, so
    there is nothing to install;
  - the restrictions that delay software updates, major upgrades and app updates (7 days), if the console still offers them
    (design 5.8: removed in macOS 27, which the farm Macs run; a Mac that drops them is a result too);
  - in the software-update payload: install macOS updates automatically, and require an administrator to install, if offered.

  Note the console's wording for each. Then wait for the lead's read, and remove every one of them.
- **Unlocks.** MP-M2 to MP-M4 and MP-M7, read from the daemon's own report (design 6.2): which console setting shows on the Mac,
  under which name, and whether the enforcement Configuration is the declaration 5.5.1 asks about.
- **Time.** About 10 minutes to make them, about 3 to remove them.
- **The lead, right after each half.** After the settings: a probe run, then `hosts get`, and records `maintenance.mac.mdmConflicts`
  against the list (a setting the Mac does not show is a result too). After the removal: a probe run, and `mdmConflicts` must be
  empty again; a name that stays is stale state, recorded and taken up with the owner. Then the lead lifts B3's cordon.

  ```
  hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> maintenance schedule --kinds probe --host-ids <node-id> --take-last-host true --confirm true --reason "Mac proof: owner session, settings read"
  hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> hosts get --node-id <node-id>
  ```

### 4. Decide how the update account reaches a Mac

No macOS update can run through hostkeeper on a Mac without the update account ([maintenance.md](maintenance.md#the-update-account)):
a run leaves such a Mac out with `no_update_account`. Every farm Mac lacks it today. On a Mac enrolled before its keys existed, the
only way built is a `sudo` line by hand, which this proof does not use. Its Secure Token is granted with the administrator's
password, which the built path reads from the Blueprint's hostkeeper settings, `dev.hostkeeper.config`, the file B2's
B2's `stat` describes.

- **Command.** One answer, to the lead:
  - **If B2's `stat` shows the file readable by root alone:** "Blueprint". The owner then adds the four keys
    (`update_account_name`, `update_account_password`, `admin_name`, `admin_password`) to `dev.hostkeeper.config` on the `mac-proof`
    Blueprint, typing each password into the form, and the lead builds the daemon writing the update-account file from those keys
    when it is missing.
  - **If the file is readable by other users:** "coordinator". A build job runs as a user of the Mac, so neither password goes
    into the Blueprint. The lead builds the delivery of the design's section 6.1 (`docs/design/xcode-one-action.md`), with the
    administrator's password delivered the same way for the one grant and kept nowhere on the Mac. The owner then hands the two
    passwords over once, each typed at a no-echo prompt.
- **Unlocks.** The update account on the proof Mac, then a minor macOS update in one maintenance run (MP-U1 to MP-U3), as soon as
  Apple offers one: none was offered on the day of design 10.1.2.
- **Time.** About 2 minutes, and about 5 more for "Blueprint".
- **The lead, right after.** Builds the answer's change. Once the Mac reports `updateAccount` present, an administrator and holding
  a Secure Token, and a `macos` item is due, the lead runs it:

  ```
  hk --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> maintenance schedule --kinds macos --host-ids <node-id> --take-last-host true --confirm true --reason "minor macOS update, one Mac, drained"
  ```

## What went off the owner's list

| Was | Now | Why |
|---|---|---|
| Download the Xcode `.xip` | step 1, on the lead's Linux box | Apple's developer download needs the Apple Account and its two-factor prompt. Everything after it is the lead's and the farm's: the upload, and the checks a Mac's tools make, in the proof Mac's stage |
| The administrator's password | step 4 decides where it goes | It is in a file on the lead's box that the lead does not read: the lead never puts it on a Mac by hand. It is used only for the update account's Secure Token, through the route step 4 picks |
| FileVault on, and its password at the pre-boot screen | off the list | The coordinator never orders `macos` on a Mac whose FileVault is not off (`filevault_not_ready`), and the ruling is FileVault off on build Macs (design, question 4). MP-A7, MP-A8, MP-U4, MP-U5 and MP-U8's FileVault-on case show a path no run takes. They come back only if FileVault is ever wanted on build Macs |
| Apple Business settings on the proof Mac's Blueprint | steps 2 and 3 | Apple Business's API cannot make these Configurations (design 5.5.1). One sitting instead of one setting at a time: the daemon names each setting it finds, and the lead reads them from its report, not from a root shell |
| Choose a Mac for a major upgrade | nothing to choose now | Every farm Mac runs macOS 27, the newest major release (design 10.1.1). A major upgrade cannot be undone, so it stays the owner's choice when macOS 28 ships: turn `macos_major` on for one Mac's group of its own, as [maintenance.md](maintenance.md#major-macos-upgrades-macos_major) says |
| Decide on the Mac's own `AutomaticallyInstallMacOSUpdates` 1 | decided: off | With it on, macOS installs a macOS update and restarts the Mac by itself, with no drain, in the middle of a build: what design 5.8 rules out. A Mac's daemon now turns it off at each start, as it keeps the Mac awake ([macos.md](../macos.md#macos-installs-no-update-by-itself)), and B2's `defaults` read shows it |
| Watch the screen during the first installs | off the list | A Mac that stops on a screen does not come back by itself: the run raises `maintenance_not_back` after the group's `return_secs`, and only then does a person look (maintenance.md, "A Mac that is not back") |
