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
   ([its README](../../packaging/macos-mdm/README.md)). Of this repository's programs only hostkeeper is
   named: the others need no grant. The job agent's grant joins the same profile (section 5).
3. **Knowing.** hostkeeper checks at start, and again at the first heartbeat 60 seconds or more after its last check, whether the
   grant is in force, and reports it in every heartbeat; `hk hosts get --node-id <host>` shows it as
   `host.fullDiskAccess`, and the host page says when it is missing ([docs/macos.md](../macos.md),
   "Full Disk Access").

What remains for the owner is the profile (step 2). The signing identity is in place (step 1).

## 1. The Developer ID Application identity

The identity is `Developer ID Application: Komira AI, Inc. (U7RTX4XD3X)`, issued by Apple's
`Developer ID Certification Authority` (G2) for code signing, a second certificate of the team beside
the Developer ID **Installer** identity the pkgs are signed with. It is valid until **2027-12-21**.

The release reads it from two organisation secrets given to this repository, named beside the
installer's:

| Secret | Holds |
|---|---|
| `MACOS_APPLICATION_P12` | the PKCS#12 file of the identity's certificate and key, base64, one line |
| `MACOS_APPLICATION_P12_PASSWORD` | its password |

The file holds the leaf and its key only. Apple's intermediate, which `codesign` needs to build the
chain, is fetched by the job `sign-code` from Apple's certificate authority page
(`DeveloperIDG2CA.cer`) and imported only if its SHA-256 is the one the workflow pins; a runner
that holds the intermediate already answers that it exists, which passes.

`sign-code` stops, naming what is wrong:
- when a secret is absent or not given to this repository;
- when the intermediate is not the pinned one, or its import fails otherwise than as a duplicate;
- when the identity is not a valid code-signing identity of the job's keychain
  (`security find-identity -v -p codesigning`: its chain does not build to Apple's root);
- **30 days before the identity expires** (`openssl x509 -checkend 2592000` of the certificate in
  the job's keychain), so it is renewed while releases still go out.

**Renewal.** Before 2027-11-21, the account holder creates a new Developer ID Application
certificate (G2 Sub-CA) of the same team in the Apple Developer account, exports it with its key as
a PKCS#12 file, and replaces the two secrets. The identity's name and team do not change, so the
designated requirement, the profile and the Macs' grant stay as they are. The file and its password
never go into a message, an issue or a chat.

After a push to `main`, the run of `artifacts.yml` shows `sign-code` green, and `check-pkg` has
checked the designated requirement of each program in the signed pkgs
(`cargo xtask release check-code-signature`). After replacing the secrets, a signing run on a
branch (`gh workflow run artifacts.yml --ref <branch> -f sign=true`,
[docs/macos.md](../macos.md#the-signing-run)) shows the same before the next push to `main`
depends on them.

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

The job agent of the build farm (komira-ai/jobscheduler) makes an account for each job and deletes it
afterwards, on the same Macs, and meets the same wall. It holds a Full Disk Access grant of its own:
deleting an account is on the path of every job and must not wait for a round trip to the
coordinator. It cannot borrow hostkeeper's grant either way: macOS charges a process that another
program starts to that program, so a `hostkeeper` command run by the agent would carry the agent's
rights. hostkeeper keeps its grant for its own host operations.

**One profile carries both grants.** `SystemPolicyAllFiles` is a list: the agent's program is a second
entry beside hostkeeper's, by its own identifier and designated requirement (a Developer ID
Application signature of the same team, never a hash). The entry is added to this profile once the
agent's identifier is fixed, and the profile's test then pins both entries. The owner uploads that
one file to Apple Business. A profile that keeps its `PayloadIdentifier`
(`dev.hostkeeper.full-disk-access`) replaces the one installed before, so a Mac never holds two.
Until the agent's entry is in, uploading hostkeeper's profile alone works, and a later upload with
both entries replaces it.

## 6. A Mac already running hostkeeper

Nothing per Mac. Once step 2 is done, Apple Business installs the profile on every Mac the Blueprint
is assigned to, and the coordinator moves each Mac to a signed release as it does any update. Until
both have happened the host reports `FULL_DISK_ACCESS_NOT_GRANTED`, which is also what a release
signed ad hoc (before step 1) reports with the profile installed: its requirement does not match.
