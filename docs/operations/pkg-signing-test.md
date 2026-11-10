# The pkg signing device test

This is task M11 of the MDM design ([hostkeeper-mdm.md](../design/hostkeeper-mdm.md),
section 7.2). It answers one question. Does a Mac install a pkg, sent by
`InstallEnterpriseApplication`, that is signed by a farm installer identity whose root a
configuration profile installed? The answer decides task M15:

- if the farm identity works, M15 builds `hostkeeper-mdm pkg add`;
- if it does not, the release workflow signs with Developer ID Installer, and question Q4 asks
  for that identity.

**The Developer ID Installer identity exists**, `Developer ID Installer: Komira AI, Inc.
(U7RTX4XD3X)`, and the release already signs its pkg with it, on a push to `main`
([macos.md](../macos.md), "The installer's signature"). Q4 is answered, and the release pkg the
MDM gate sends is a Developer ID pkg whatever this test finds. The test still decides whether the
farm identity works too, which a service's pkg signed by the operator would need (design section
7.3).

`productsign` on GitHub's hosted `macos-15` runner signs with the Developer ID identity from a
keychain of the job, as the release's job `sign-pkg` does on every push to `main`: the identity's
chain ends in Apple Root CA, which every Mac trusts. The question of the build step below (does
`productsign` sign with an identity whose root the Mac does not trust), and the refusal a macOS 15
runner showed for such an identity ("What a macOS 15 runner showed", below), concern the farm
identity of this kit only. A run of the test can add the
Developer ID case without a new kit: put the release's pkg on the gate's allowlist
([enrollment-coordinator.md](../enrollment-coordinator.md)) and read its install on the Mac as
for pkg 3 below. If a Mac refuses it and install.log names notarization, record that in the
design's section 7.2: notarization is not in the release, by design section 7.3.

The test needs one Mac that is enrolled in the farm's MDM. A Mac enrolled by Path A or Path B will
do. The run is held: the owner of the MDM host runs it. Nothing reaches a Mac until a `send` is
given `--apply`.

The kit is `cargo xtask mdm-signing-test` (`crates/xtask/src/mdm_signing_test.rs`). It has four
verbs:

- `build` runs on a Mac. It makes the test CAs and the four pkgs.
- `send` runs on the MDM host. It sends one step to the Mac through NanoHUB's API on loopback.
- `record` writes what the operator read for that step.
- `report` prints the run as a table, with its verdict.

## What the kit holds

`build` makes two test CAs with macOS's `/usr/bin/openssl`. Each CA has a self-signed RSA root and
an installer identity under that root. The identity has the `codeSigning` extended key usage.

- Root **A** is installed on the Mac, by the profile `signing-test-root.mobileconfig`. That
  profile holds one `com.apple.security.root` payload.
- Root **B** is deleted at the end of the build, so it is installed nowhere.

The build deletes every private key, the build's own keychain and root B before it ends, and it
deletes them if it fails too. The repository holds no secret, and no key outlives the build. A
second build makes new CAs.

The four pkgs follow section 7.2. Each installs one marker file in
`/Library/Application Support/hostkeeper-signing-test`, under its own identifier.

| Step | Pkg | Identifier |
|---|---|---|
| `pkg-1` | component pkg (`pkgbuild`), unsigned | `dev.hostkeeper.signing-test.pkg1` |
| `pkg-2` | distribution pkg (`productbuild --package`), unsigned | `dev.hostkeeper.signing-test.pkg2` |
| `pkg-3` | distribution pkg, signed by A's identity (`productsign`, no timestamp) | `dev.hostkeeper.signing-test.pkg3` |
| `pkg-4` | distribution pkg, signed by B's identity | `dev.hostkeeper.signing-test.pkg4` |

The pkgs carry a marker file, not the hostkeeper payload, for two reasons. First, a receipt then
names exactly one pkg. Second, the release pkg's daemon refuses each start when the Mac has no managed
config yet, which would leave a Mac that has the pkg's job loaded and failing, and that
would blur the signature's outcome.

The pkgs and the release pkg are signed the same way: `productsign` over a `productbuild`
distribution pkg. The kit signs with no timestamp; the release signs with a trusted timestamp.

The kit's directory holds three things:

- `kit.toml` describes the kit. It lists each pkg's identifier and digest, and what
  `pkgutil --check-signature` printed for it on the build Mac.
- `pkg/<sha256>.pkg` holds each pkg, under the name the proxy serves it by.
- `signing-test-root.mobileconfig` is the root's profile.

`send` writes `results.toml` beside them.

## Values used below

| Placeholder | What | On the live stack | After task M7 |
|---|---|---|---|
| `<kit>` | the kit's directory | | |
| `<prefix>` | the MDM stack's unit prefix | `kbf-mdm` | `hostkeeper-mdm` |
| `<state>` | its state directory | `/var/lib/js-mdm` | `/var/lib/hostkeeper-mdm` |
| `<etc>` | its configuration directory | `/etc/js-mdm` | `/etc/hostkeeper-mdm` |
| `<user>` | its user | `js-mdm` | `hostkeeper-mdm` |
| `<pkg-dir>` | the directory the proxy serves `/pkg/<sha256>.pkg` from | `<state>/www/pkg`, made by hand in step 2 | `/var/lib/hostkeeper-mdm-pkg` (`MDM_PKG_DIR`) |
| `<mdm-host>` | the MDM host's name, which its certificate carries | | |
| `<udid>` | the Mac's enrollment id in NanoHUB | | |
| `<scratch>` | a directory only root can read, on the MDM host | | |
| `<mac-scratch>` | the operator's scratch directory for the run, on the Mac under test | | |

The live stack's names come from kbf's kit as it was reinstalled under jobscheduler's paths. The
design's section 2 explains this mix. Confirm them on the host with
`systemctl list-units '<prefix>-*'` before you start.

## 1. Build the kit, on a Mac that is not the one under test

Use a checkout of this repository at the commit being tested:

```
cargo xtask mdm-signing-test build --out <kit> --profile-identifier com.komira.hostkeeper.mdm
```

Use a build Mac that no other person is using during the build. The password of the build's
keychain is random and new each build, but it is on the command lines of `openssl` and
`security` while they run.

The command prints the profile's identifier, root A's digest and each pkg's digest. The build's
keychain is first in the user's keychain search list only while the build signs. The build then
puts the list back and deletes the keychain.

If the build fails, it deletes its keys, but the kit's directory stays. Delete `<kit>` and build
again. If the build is interrupted while it signs, put the search list back by hand with
`security list-keychains -d user -s ~/Library/Keychains/login.keychain-db` and delete
`<kit>/work`.

**Stop if `productsign` fails** with `Could not find appropriate signing identity`. That means
this build Mac does not let `productsign` use an identity whose root it does not trust. Neither
the design nor Apple's documentation says whether it does, so this is to verify. Do not trust
root B anywhere to get past it, because pkg 4 needs root B trusted nowhere. Record the message in
the design's section 7.2; the build is then changed to sign in another way.

**What a macOS 15 runner showed.** The `macos-pkg` installer's device test once signed its
fixtures as this build does, on a GitHub-hosted runner (image `macos-15-arm64`, macOS 15.8.2):

- **Made the same way.** It made a root and an installer identity under it with `/usr/bin/openssl`.
  The identity had `digitalSignature` and the `codeSigning` extended key usage, as this build's has.
- **Imported for `productsign`.** It imported the identity into a keychain of its own with
  `-T /usr/bin/productsign` and `set-key-partition-list -S apple-tool:,apple:`. That keychain was
  first in the search list.
- **Signed with `productsign`.** It ran `productsign --sign <identity> --keychain <keychain>
  --timestamp=none` over a `productbuild --package` pkg.

For the identity whose root the Mac did not trust, `productsign` failed with:

```
productsign: error: Could not find appropriate signing identity for "<identity>" in keychain at "<keychain>". An installer signing identity (not an application signing identity) is required
```

What the run showed, and what it did not:

- **Observed: this identity was refused.** On macOS 15.8.2, `productsign` would not sign with an
  identity made the way this build makes its identities, whose root the Mac did not trust. The
  keychain was unlocked and its key partition list set. The run failed before the identity whose
  root it had trusted signed, so there is no case to compare with.
- **Not isolated: why.** Two causes fit the message, and the run cannot tell them apart. The first
  is the untrusted root. The second is the identity itself: the message asks for "an installer
  signing identity (not an application signing identity)", and the identity carried only the
  `codeSigning` extended key usage, as this build's identities do.
- **What it means for pkg 4.** In both cases this build cannot make pkg 4 on such a Mac. It stops
  at the paragraph above, by design, because root B must stay trusted nowhere.
- **What it means for pkg 3.** If the identity is the cause, pkg 3 fails the same way, whatever is
  trusted. If the root is the cause, pkg 3 fails too, because root A is not trusted on the build
  Mac (the profile installs it on the Mac under test). Either way this is inferred, not observed.
  Which change makes `productsign` sign (trusting the root on the build Mac, or an identity of
  another kind) is still to verify.
- **Inferred: trust changes need an operator.** On a runner with nobody at the screen, the test's
  undo of its trust setting (`security remove-trusted-cert -d`) then made no progress until the
  job's timeout. That call most likely waited for an authorization only an operator at the Mac can
  give. So a build that changes trust settings needs an operator at the Mac.

The installer's device test (`crates/hostkeeper-privileged/tests/install_macos_pkg.rs`) now signs
nothing and changes no trust setting or keychain. It builds pkg 1's and pkg 2's unsigned forms with
this build's commands. The form `pkgutil --check-signature` prints for a trusted signature is left
to this test's signed run.

Build the kit's binary for the MDM host as well. Build it on any Linux host of the same
architecture, from the same commit:

```
cargo build --release --locked -p xtask
```

Copy `target/release/xtask` and the whole `<kit>` directory to the MDM host. The commands below
call the binary `xtask`.

## 2. Serve the pkgs, on the MDM host (as root)

The proxy serves `https://<mdm-host>/pkg/<sha256>.pkg`, the URL design section 7.1 gives, from
`<pkg-dir>/<sha256>.pkg`, and the kit names each pkg `<sha256>.pkg`.

After task M7 the route is the kit's own: `hostkeeper-mdm install` renders it into the Caddyfile
and makes `<pkg-dir>` (`MDM_PKG_DIR`, root's, 0755). Put the pkgs in it:

```
install -m 0644 -o root -g root <kit>/pkg/*.pkg <pkg-dir>/
```

On the live stack, before task M7, the proxy has no `/pkg/` route. Add it by hand for this test,
and remove it at the end (step 6):

```
install -d -m 0755 -o <user> -g <user> <pkg-dir>
install -m 0644 -o <user> -g <user> <kit>/pkg/*.pkg <pkg-dir>/
```

Edit `<etc>/Caddyfile`. Inside the `https://<mdm-host>:443` block, put this route before the last
`handle { respond 404 }`:

```
	handle /pkg/* {
		root * <state>/www
		file_server
	}
```

Then restart the proxy:

```
systemctl restart <prefix>-proxy
```

From the Mac under test, check the route with
`curl -fsS -o /dev/null -w '%{http_code}\n' https://<mdm-host>/pkg/<sha256 of pkg 1>.pkg`. It
must print `200`.

## 3. The API key and the Mac's enrollment id, on the MDM host

`send` reads NanoHUB's API key from a file that only its owner can read. The key is the value of
`NANOHUB_API_KEY` in the unit's environment file:

```
umask 077
sed -n 's/^NANOHUB_API_KEY=//p' <etc>/nanohub.env > <scratch>/nanohub-api-key
```

The enrollment id is the Mac's UDID. NanoHUB logs it as the `id` of the Mac's check-ins. Find it
with `journalctl -u <prefix>-nanohub | grep -i tokenupdate`. On a Mac, the UDID is expected to be
the hardware UUID, but this is to verify in M16. You can read the hardware UUID with
`ioreg -rd1 -c IOPlatformExpertDevice | grep IOPlatformUUID`. Use the id from NanoHUB's log.

## 4. Run the steps, in this order

Every step follows the same pattern:

1. Run `send` without `--apply`. It prints the command and sends nothing.
2. Run `send` again with `--apply`. It queues the command, NanoHUB pushes the Mac, and the send
   is written to `results.toml` with its command UUID.
3. Find the Mac's answer in NanoHUB's log, on the line that has that command UUID:

   ```
   journalctl -u <prefix>-nanohub --since '-30min' | grep <command uuid>
   ```

   The line's `status=` is one of `Acknowledged`, `Error`, `CommandFormatError` or `NotNow`. If
   there is no line yet, the Mac has not answered.
4. Check the Mac.
5. Record what you read:

   ```
   xtask mdm-signing-test record --kit <kit> --step <step> --answer <status> \
     [--installed true|false] [--trusted true|false] [--note '<install log line>']
   ```

   `record` refuses `--installed` for a profile step, refuses a pkg step without it, and accepts
   `--trusted` for `pkg-3` only. Recording again replaces the earlier record of that send.

Use these flags for every `send`:

```
F="--kit <kit> --enrollment-id <udid> --api-url http://127.0.0.1:9004 \
  --api-key-file <scratch>/nanohub-api-key --mdm-host <mdm-host>"
xtask mdm-signing-test send $F --step pkg-1            # prints the command only
xtask mdm-signing-test send $F --step pkg-1 --apply
```

`--api-url` must be a loopback address. The port is NanoHUB's `-listen` port in
`<prefix>-nanohub.service`.

**Checking a pkg on the Mac.** `InstallEnterpriseApplication` is acknowledged before the Mac
downloads the pkg, and the Mac does not report install errors to the server (section 7.2). So the
outcome is read on the Mac, as an admin:

```
pkgutil --pkg-info dev.hostkeeper.signing-test.pkg<n>
grep -i -e signing-test -e 'signature' /var/log/install.log | tail -n 20
log show --last 30m --predicate 'process == "mdmclient" OR process == "installd"' | grep -i -e signing-test -e signature
```

The first command exits 0 and prints the version when the pkg installed. If install.log shows the
install ending for that identifier, or shows nothing 15 minutes after `Acknowledged`, record
`--installed false`. Copy the log line that says why into `--note`, and quote it whole when it
names the signature, the certificate, Gatekeeper or notarization.

The order of the steps:

1. **`pkg-1`.** The unsigned component pkg.
2. **`pkg-2`.** The unsigned distribution pkg.
3. **`root-profile`.** Record its answer. On the Mac, `profiles show -type configuration` lists
   `com.komira.hostkeeper.mdm.signing-test.root`, and
   `security find-certificate -c 'hostkeeper pkg signing test root A' /Library/Keychains/System.keychain`
   finds the root.

   `send` refuses `pkg-3` and `pkg-4` until this step is recorded `Acknowledged`, and refuses
   them again after `remove-root-profile`. Without that order, a pkg sent before its root was on
   the Mac would be read as a refusal of the identity.
4. **Pkg 3's chain on the Mac, before sending it.** Download pkg 3 and read its signature:

   ```
   mkdir -p <mac-scratch>
   curl -fsS -o <mac-scratch>/pkg3.pkg https://<mdm-host>/pkg/<sha256 of pkg 3>.pkg
   pkgutil --check-signature <mac-scratch>/pkg3.pkg
   ```

   With root A installed, `pkgutil` is expected to say the certificate is trusted. Record the
   result with pkg 3's outcome, as `--trusted true` or `--trusted false`. If it says the
   certificate is not trusted, the identity is the suspect, not the MDM, and the verdict says so.
5. **`pkg-3`.** Send it, check the Mac and record, including `--trusted`.
6. **`pkg-4`.** Send it, check the Mac and record. Root A stays installed. Pkg 4 shows that root
   A's payload, not any signature at all, is what lets pkg 3 install.
7. **`remove-root-profile`.** Send it and record its answer.

If a step must be sent again (for example, the Mac was asleep and answered `NotNow`), send it
again. Each send has its own command UUID, and the record and the report read the last send of
each step.

## 5. Report, and write the results into the design

Print the run:

```
xtask mdm-signing-test report --kit <kit>
```

It prints a table of every send (step, command, answer, installed, note) and a verdict:

| Verdict | Meaning | What follows |
|---|---|---|
| `farm identity` | pkg 3 installs and pkg 4 does not | M15 builds `hostkeeper-mdm pkg add` (design 7.3) |
| `Developer ID Installer` | the Mac trusts pkg 3's chain and still does not install it | M15 builds the Developer ID job; Q4 is asked |
| `inconclusive: pkg 4 installed …` | the Mac did not judge the chain | read install.log; nothing is decided |
| `inconclusive: pkg 3 did not install and the Mac was not shown to trust its chain …` | the identity, not the MDM, may be refused | fix the identity, build a new kit, run again |
| `inconclusive: the Mac answered … with Error` | the command, not the signature, was refused | read the answer and send again |
| `incomplete` | a pkg has no recorded outcome | finish the run |

An `Also:` line follows for each unsigned pkg that installed.

If pkg 3 fails and the install log names notarization, the verdict alone does not cover that
case. Write it into the design: section 7.3 then adds notarization to the plan.

Write into section 7.2 of the design:

- the table and the verdict;
- the Mac's macOS version (`sw_vers`) and its model;
- the path (A or B) by which the Mac enrolled;
- the `pkgutil --check-signature` lines.

Keep `<kit>/results.toml` and `<kit>/kit.toml` with the run's notes. They hold no key.

## 6. Clean up

On the Mac, as an admin, for each pkg that installed:

```
sudo rm -f '/Library/Application Support/hostkeeper-signing-test/pkg-<n>'
sudo pkgutil --forget dev.hostkeeper.signing-test.pkg<n>
sudo rmdir '/Library/Application Support/hostkeeper-signing-test'
```

Remove `<mac-scratch>/pkg3.pkg`. Confirm that `profiles show -type configuration` no longer
lists the root's profile. `report` prints a `Cleanup:` line while the run has not recorded
`remove-root-profile` as `Acknowledged`.

On the MDM host, as root:

1. On the live stack only (before task M7): take the `/pkg/*` route out of `<etc>/Caddyfile` and
   run `systemctl restart <prefix>-proxy`.
2. Remove the four pkgs from `<pkg-dir>`. On the live stack, remove the directory too.
3. Remove `<scratch>/nanohub-api-key`.
