# CM12a: what the farm Macs and the live stack show about `/enroll`, the Apple chain and the client-auth mode

Status: spike note for [coordinator-mdm.md](coordinator-mdm.md) section 4.2 and section 12 (items 4, 5, 10, 11, 12). Date of the reads: 2026-11-10.
No production code. The fixtures are in `docs/design/fixtures/cm12a/`.

## Summary

The task asked for a capture of the real ADE `/enroll` POST. **That capture could not be made**, and nothing in this note claims it. The reasons are
below. What was measured instead is listed with what each result changes in the design. Section 4.2's rule stays as written: no `/enroll` code
before a capture exists, and the fail-closed rule applies until the blob verifies against a real sample.

| Question | Result | Kind |
|---|---|---|
| Capture the ADE `/enroll` POST | Not possible on the farm Macs (see "Why no capture") | not done |
| Apple device-identity chain verifies against Apple roots | Not tested: it needs the blob. The tooling and roots were proved on a real Apple-issued chain (the push certificate) | partly measured |
| Must the `/enroll` response be CMS-signed (item 11) | Supported "no" by the live enrollment; the POST itself was not seen | inferred |
| SCEP exchange against a library fixture | Not repeated here; done in the `scep` spike (section "SCEP") | referenced |
| Client-auth mode of the live profile (item 5) | `SignMessage true`; the live proxy sends no TLS client-certificate request at all | measured |
| Client offers its identity given root hints (item 10) | Not testable on the live stack; avoidable by the live mode | not done |
| Client on 401, 403, 410 from `/mdm` (item 12) | Not tested (needs a wipe or a second enrolled Mac); nothing in the live journal shows it | not done |
| `CheckOutWhenRemoved` of the live profile (asked by the CM4 model check) | `true` | measured |

## Why no capture

- **m3ultra-2** (serial `MFDG20NQWJ`, `adityas-mac-studio`) is not in our ABM organization: the depserver returns an empty record and `profiles status`
  says "Enrolled via DEP: No". Setup Assistant on it never posts to our `/enroll`. Adding it to ABM is an Apple-account change and is not allowed in this run.
- **m3ultra-1** is enrolled, so it does not run Setup Assistant again without a wipe, which is not allowed.
- **The live proxy would not have kept the body anyway.** Its Caddyfile answers `/enroll` with `method GET` plus a static file, so an ADE POST body is
  never read or logged, and the proxy has no access log. The journal of the stack (since boot, 2026-11-10 05:18) has no `/enroll` line.
- macOS 27.0.1 `profiles install` refuses installs, so a hand-made enrollment cannot be driven without the GUI.

Ways to get the capture, for the owner to choose:

1. Add `MFDG20NQWJ` to ABM and assign it to `js-mdm`, then erase m3ultra-2 and run Setup Assistant against a listener that logs the body.
2. Allow one wipe of m3ultra-1 with a logging listener in front of it.
3. Ship the coordinator's `/enroll` fail-closed (an operator click), with a setup-gated raw-body log of the POST, and write the verification code after the first real
   enrollment produces a sample. This needs no Apple-account change. It is the choice this note recommends, since the fail-closed rule is already the design.

## Apple chain: what was proved without the blob

Apple's public roots are fetchable from `https://www.apple.com/certificateauthority/` and `https://www.apple.com/appleca/`. Fixtures (public certificates):

| File | Subject | Source |
|---|---|---|
| `apple-root-ca.pem` | Apple Root CA (RSA, to 2035-02-09), sha256 `B0:B1:73:0E:CB:C7:FF:45:05:14:2C:49:F1:29:5E:6E:DA:6B:CA:ED:7E:2C:68:C5:BE:91:B5:A1:10:01:F0:24` | `appleca/AppleIncRootCertificate.cer` |
| `apple-root-ca-g3.pem` | Apple Root CA - G3 (ECC, to 2039-04-30), sha256 `63:34:3A:BF:B8:9A:6A:03:EB:B5:7E:9B:3F:5F:A7:BE:7C:4F:5C:75:6F:30:17:B3:A8:C4:88:C3:65:3E:91:79` | `certificateauthority/AppleRootCA-G3.cer` |
| `apple-application-integration-2-ca.pem` | Apple Application Integration 2 CA, issued by Apple Root CA | `certificateauthority/AppleAAI2CA.cer` |

`openssl verify -CAfile apple-root-ca.pem -untrusted apple-application-integration-2-ca.pem <the live push certificate>` prints `OK`. The push certificate is
not stored here. That shows the root fixtures are the right trust anchors for a real Apple-issued chain, and that a chain needs the intermediate supplied.

**Not proved.** The device-identity chain of an ADE blob (the intermediates a Mac's "device certificate" carries) was not verified: no blob exists. The
intermediate CA for device certificates (the "Apple iPhone Device CA" family named in community sources) was not found among the certificates Apple lists on
the page above, and one guessed URL for it returned 404. Whether Apple publishes it elsewhere is unverified. The design anchors at the Apple root; the
first capture decides between taking the intermediates from the PKCS#7 (the blob carries its chain, per community sources, unverified) and shipping a
pinned intermediate bundle. `cms` in the workspace has no chain or signature-verify API (the `scep` spike, gaps 1 and 3), so CM12c writes the verification by hand.

## `/enroll` response signing (item 11)

m3ultra-1, supervised and `profile_status pushed` with profile url `/enroll`, enrolled (`Authenticate` at 2026-11-10 10:38 in the NanoHUB journal) through a
stack whose `/enroll` returns the **unsigned** plist (the static file, content type `application/x-apple-aspen-config`). So an unsigned response was
accepted by macOS 27.0.x. I did not see the POST, so this is an inference from the profile status, the file served and the timeline. 4.1's CMS-signing branch
and CM12c's signing step can be dropped unless the first capture contradicts it.

## SCEP

The `scep` spike built a server on the design's crates (`cms`, `rsa`, `x509-cert`, `der`, `aes`, `cbc`, `des`) and ran issuance, replay, key mismatch,
unknown challenge and a flipped byte against the OpenSSL CLI as the independent verifier. Its findings, including the three crate gaps, are in that spike's note
and are inputs to CM12b. This task did not repeat the exchange and adds no SCEP fixture: the spike's request files were built with a throwaway CA key, and a
fixture needs a CM12b-owned generator rather than a copy of key files.

## Client-auth mode of the live profile (items 5 and 10)

Measured from the unit and the proxy configuration, read-only:

- The enrollment profile has `SignMessage true`. NanoHUB is started with `-ca <scep depot ca.pem>` and its unit comment says a device is accepted when its
  identity certificate chains to that CA through `Mdm-Signature`.
- The Caddyfile has no client-authentication block, so the TLS handshake does not ask the Mac for a certificate. The NanoHUB journal has two
  "error verifying MDM certificate err=missing MDM certificate" lines (09:44:02 and 09:44:03), which are requests with neither a signature nor a certificate.
- `Authenticate` (10:38:04) and `TokenUpdate`, then the user channel `TokenUpdate` and `Idle` (11:11:33) and device `Idle` (11:13:41, 11:14:10, 11:14:48) all
  succeeded in this mode.

Consequence for item 10: **the live mode does not depend on whether a macOS client offers its TLS identity for root hints.** The question is only open for
the optional rustls client-certificate path. The new profile should keep `SignMessage true`, the mode that has run on a real Mac, and the listener should
treat the TLS client certificate as optional extra evidence, not as the way to identify the Mac. Verifying `Mdm-Signature` (a CMS detached signature by the
device identity) is then the check on `/checkin` and `/mdm`, which CM11a must implement and CM5 must carry fixtures for.

## `401`, `403`, `410` from `/mdm` (item 12)

Not tested. It needs a Mac to be answered with those statuses, which with a live profile risks removal of management on m3ultra-1. The journal of the live stack has no
such responses. Item 12 stays open and CM11a keeps its rule that the status for an unknown enrollment is chosen from Apple's protocol text first, with `410` unused.
A throwaway enrollment on a Mac that is not in production is the way to measure it (the same Mac needs to be enrolled to test anything about the client).

## `CheckOutWhenRemoved`

`true` in the live profile (read from `www/enroll.mobileconfig`, scep spike). Whether the Mac actually sends `CheckOut` when a profile is removed is not
observed. The CM4 model's decision on supersession (section 5.4 and 10) rests on this: with `CheckOutWhenRemoved true` a profile removal checks the old
enrollment out and re-enrollment of that Mac needs no operator click. A wipe that does not remove the profile first does not send it. Not measured.

## What this changes in the design

- Section 4.2: the capture is still the gate; record the three ways to get it, recommend the setup-gated raw-body log of the first deployed coordinator,
  and state that the device intermediates were not found on Apple's CA page (unverified whether published).
- Section 4.1 and item 11: CMS-signing of the `/enroll` response may not be needed (one Mac, macOS 27.0.x, inferred; item 11 stays open); CM12c's signing step is conditional on the first capture.
- Section 4.1 and items 5 and 10: the profile keeps `SignMessage true`; the TLS client certificate is optional evidence.
- Section 12: items 4, 10 and 12 stay open with the reasons above; items 5 and 11 move forward as stated.

## Not verified

- Anything about the real POST: its body encoding, the signature algorithm, the chain it carries, its freshness, or its headers.
- That the device chain verifies against the roots in the fixtures.
- What the MDM client does on 401, 403 and 410.
- Whether a Mac sends `CheckOut` on removal or wipe.
- That an ADE enrollment accepts an unsigned `/enroll` response (inferred from one enrolled Mac).

**Item 5 caveat.** The client-auth mode is measured from configuration and the journal. That `Mdm-Signature` verifies against the device identity is inferred from NanoHUB accepting the requests; one captured `/checkin` request, committed as a redacted fixture, is wanted before CM11a relies on it.
