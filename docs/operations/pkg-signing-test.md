# The pkg signing device test

This is task M11 of the MDM design ([hostkeeper-mdm.md](../design/hostkeeper-mdm.md),
section 7.2). It answers one question. Does a Mac install a pkg, sent by
`InstallEnterpriseApplication`, that is signed by a farm installer identity whose root a
configuration profile installed? The answer decides task M15:

- if the farm identity works, M15 builds `hostkeeper-mdm pkg add`;
- if it does not, the release workflow signs with Developer ID Installer, and question Q4 asks
  for that identity.

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
names exactly one pkg. Second, the release pkg's postinstall fails when the Mac has no managed
config yet (`mac-config`), and that failure would hide the signature's outcome.

The pkgs and the release pkg are signed the same way: `productsign` over a `productbuild`
distribution pkg.

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
| `<mdm-host>` | the MDM host's name, which its certificate carries | | |
| `<udid>` | the Mac's enrollment id in NanoHUB | | |
| `<scratch>` | a directory only root can read, on the MDM host | | |

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

Build the kit's binary for the MDM host as well. Build it on any Linux host of the same
architecture, from the same commit:

```
cargo build --release --locked -p xtask
```

Copy `target/release/xtask` and the whole `<kit>` directory to the MDM host. The commands below
call the binary `xtask`.

## 2. Serve the pkgs, on the MDM host (as root)

The proxy has no `/pkg/` route until task M5 adds it. For this test, add it by hand and remove
it at the end (step 6). The route serves `https://<mdm-host>/pkg/<sha256>.pkg` from
`<state>/www/pkg`, the URL design section 7.1 gives.

```
install -d -m 0755 -o <user> -g <user> <state>/www/pkg
install -m 0644 -o <user> -g <user> <kit>/pkg/*.pkg <state>/www/pkg/
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
   d="$HOME/pkg-signing-test"; mkdir -p "$d"
   curl -fsS -o "$d/pkg3.pkg" https://<mdm-host>/pkg/<sha256 of pkg 3>.pkg
   pkgutil --check-signature "$d/pkg3.pkg"
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

Remove `$HOME/pkg-signing-test`. Confirm that `profiles show -type configuration` no longer
lists the root's profile. `report` prints a `Cleanup:` line while the run has not recorded
`remove-root-profile` as `Acknowledged`.

On the MDM host, as root:

1. Take the `/pkg/*` route out of `<etc>/Caddyfile` and run `systemctl restart <prefix>-proxy`.
2. Remove the four pkgs from `<state>/www/pkg`, then remove the directory.
3. Remove `<scratch>/nanohub-api-key`.
