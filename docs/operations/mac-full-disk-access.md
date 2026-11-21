# Full Disk Access for hostkeeper on a Mac

hostkeeper runs as root from launchd, but on current macOS root alone cannot do what a host manager
must. Measured on a farm Mac running macOS 27 with a root daemon that has no privacy grant: deleting a
local account (`dscl . -delete`, `sysadminctl -deleteUser`) ends in `eDSPermissionError` (-14120);
removing an account's home, the per-user system folders or another account's `~/Library/Preferences`
is refused (the privacy daemon logs service `kTCCServiceSystemPolicyAllFiles` for the calling program,
result denied); and the system's privacy database cannot be opened. What lifts this is the privacy
grant **Full Disk Access** (`SystemPolicyAllFiles`), given by device management through a
configuration profile.

Three pieces make the grant durable:

1. **A stable signature.** The release signs every program it ships for macOS with the team's
   Developer ID Application identity, under a fixed identifier, with the hardened runtime and a
   trusted timestamp (`artifacts.yml`, job `sign-code`; [docs/macos.md](../macos.md)). The designated
   requirement of such a signature names the identifier and the team, never a hash of the build, so
   it is the same for every release:

   | Program | Identifier |
   |---|---|
   | `hostkeeper` | `dev.hostkeeper.hostkeeper` |
   | `hk` | `dev.hostkeeper.hk` |
   | `hello` | `dev.hostkeeper.hello` |
   | `hostkeeper-bootstrap` | `dev.hostkeeper.bootstrap` |

   `gh`, which the installer pkg carries, keeps GitHub's own signature.
2. **The profile.** `packaging/macos-mdm/hostkeeper-full-disk-access.mobileconfig` grants
   `SystemPolicyAllFiles` to `dev.hostkeeper.hostkeeper` by that requirement
   ([its README](../../packaging/macos-mdm/README.md)). Only hostkeeper is named: the other programs
   need no grant.
3. **Knowing.** hostkeeper checks at start, and again at the first heartbeat 60 seconds or more after its last check, whether the
   grant is in force, and reports it in every heartbeat; `hk hosts get --node-id <host>` shows it as
   `host.fullDiskAccess`, and the host page says when it is missing ([docs/macos.md](../macos.md),
   "Full Disk Access").

The owner's steps, in order, follow. Step 1 comes **before** the change that adds `sign-code` is
merged: from that merge on, a push to `main` signs the programs, and while the identity's secrets
are missing the job `sign-code` fails and names the missing secret, so no release is made.

## 1. The Developer ID Application certificate (owner, once)

Only the account holder of the Apple Developer account can create a Developer ID certificate. The
Developer ID **Installer** identity the release already uses stays as it is; this is a second
certificate of the same team.

1. On a Mac, open Keychain Access, choose Keychain Access > Certificate Assistant > Request a
   Certificate From a Certificate Authority, enter the account's email address and a common name
   (for example "hostkeeper Developer ID Application"), choose "Saved to disk", and save the
   certificate signing request.
2. In the Apple Developer account, open Certificates, Identifiers & Profiles > Certificates, choose
   the add button, choose **Developer ID Application**, choose the **G2 Sub-CA** profile type,
   upload the signing request, and download the certificate.
3. Open the downloaded certificate so it joins the login keychain beside the request's private
   key. In Keychain Access, under My Certificates, the identity reads
   `Developer ID Application: Komira AI, Inc. (U7RTX4XD3X)`. Any other name or team is refused by the
   release: the workflow signs with exactly that name, and its check compares the team.
4. Select that identity (the certificate with its key), choose File > Export Items, save it as a
   `.p12` file, and give it a new strong password.
5. Store it in the repository's environment `pkg-signing` as two secrets, from the Mac, without the
   file or the password ever being pasted into a message, an issue or a chat:

   ```sh
   base64 -i application.p12 | gh secret set MACOS_APPLICATION_P12 --env pkg-signing --repo komira-ai/hostkeeper
   gh secret set MACOS_APPLICATION_P12_PASSWORD --env pkg-signing --repo komira-ai/hostkeeper
   ```

   The second command reads the password from its prompt. Then delete the `.p12` file. The
   environment `pkg-signing` already holds the installer's two secrets and is limited to `main`.
6. After the next push to `main`, the run of `artifacts.yml` shows `sign-code` green, and
   `check-pkg` has checked the designated requirement of each program in the signed pkgs
   (`cargo xtask release check-code-signature`).

The certificate is valid for five years. When it is renewed, the identity's name and team do not
change, so the requirement and the profile stay the same; only the two secrets are replaced.

## 2. The profile in Apple Business (owner, once)

Apple Business's built-in device management takes a profile it has no template for as a **Custom**
configuration (Apple's guide "Create a Custom configuration in Apple Business"):

1. In Apple Business, choose Devices > Configurations > All Configurations, then Add next to
   **Custom**, and name it "hostkeeper Full Disk Access".
2. Upload `packaging/macos-mdm/hostkeeper-full-disk-access.mobileconfig` from this repository (the
   file of the release the Macs run, or newer: the profile changes only when the table of
   identifiers does). Apple Business lists any problem it finds in the file; there should be none.
3. Choose macOS as its operating system and save.
4. Open the Blueprint that installs the hostkeeper bootstrap and add the configuration to it, so
   every Mac that gets the bootstrap gets the grant too. A separate Blueprint assigned to the same
   Macs works as well.

A privacy payload is honoured only on a Mac enrolled in device management with user approval or by
Automated Device Enrollment (`profiles status -type enrollment` says "MDM enrollment: Yes (User
Approved)" and "Enrolled via DEP: Yes" on the farm's Macs). A profile installed by hand is refused.

## 3. Seeing that the grant is in force

On a Mac, as an administrator, with the release signed by step 1 installed:

1. The profile is installed: `sudo profiles show -type configuration` lists
   `dev.hostkeeper.full-disk-access`.
2. The running program carries the requirement the profile names. The daemon runs from its
   self-update copy, not from `/usr/local/bin/hostkeeper`:

   ```sh
   pid=$(pgrep -x hostkeeper | head -n 1)
   exe=$(ps -o comm= -p "$pid")
   codesign --display --verbose=2 "$exe"
   codesign --display --requirements - "$exe"
   ```

   The first shows `Identifier=dev.hostkeeper.hostkeeper`, `TeamIdentifier=U7RTX4XD3X`, the runtime
   flag and a timestamp; the second prints `designated =>` followed by exactly the profile's
   `CodeRequirement`.
3. hostkeeper's own check: the daemon's log `/var/log/hostkeeper/daemon.err.log` has a line
   ending `full disk access: granted` from the check at start (or a line
   `full disk access: not_granted -> granted` when the grant arrived while it ran);
   `hk hosts get --node-id <host>` answers `"fullDiskAccess": "FULL_DISK_ACCESS_GRANTED"`.
4. The grant does what it is for: `sudo log show --last 10m --predicate 'subsystem == "com.apple.TCC"'`
   shows the check of `dev.hostkeeper.hostkeeper` for `kTCCServiceSystemPolicyAllFiles` allowed.

**After a self-update.** Run 2 to 4 again once the host has taken a new release: the program then
runs from another path, and the grant must still hold. That is the point of granting by identifier
and requirement rather than by path.

## 4. Notarization

None of this asks for notarization, as far as the documentation goes: the profile's
`CodeRequirement` names the signing identity, not a notarization ticket, and Gatekeeper assesses a
program that carries the quarantine attribute, which a program `installer` lays down from a pkg
that device management delivered does not. This is an expectation, not a measurement: the release
does not notarize, and the first host that runs a signed release settles it. On that host:

```sh
xattr -l /usr/local/bin/hostkeeper
spctl --assess --verbose=4 --type execute /usr/local/bin/hostkeeper
```

The first prints no `com.apple.quarantine`. The second may answer "rejected (source=Unnotarized
Developer ID)"; that is Gatekeeper's verdict for a download a user opens, and the daemon runs anyway
when the first prints nothing. If a Mac ever refuses to start the signed daemon, notarizing the
programs is the next step (`docs/design/mac-bootstrap.md`, MB4).

## 5. Who else needs the grant

The job agent of the build farm cleans pool users' homes on the same Macs and meets the same wall.
It gets no grant of its own: only hostkeeper holds Full Disk Access, and the host operations others
need (remove an account, empty a guarded folder) become compiled-in operations of hostkeeper, done
by the daemon itself. This keeps the rule of one root process with compiled-in verbs, and the
profile and its requirement name one program. The operations must run inside the daemon: macOS
charges a process that another program starts to that program, so a `hostkeeper` command run by the
job agent would carry the agent's rights, not hostkeeper's.

## 6. A Mac already running hostkeeper

Nothing per Mac. Once step 2 is done, Apple Business installs the profile on every Mac the Blueprint
is assigned to, and the coordinator moves each Mac to a signed release as it does any update. Until
both have happened the host reports `FULL_DISK_ACCESS_NOT_GRANTED`, which is also what a release
signed ad hoc (before step 1) reports with the profile installed: its requirement does not match.
