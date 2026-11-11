# CM11b: `mdm verify`

Split from `coordinator-mdm.md` (section 12), which is over its line limit.

**What CM11b built.** (`mdm_verify.rs`, `Pusher::test_push`, the *Verify* section of `ui/pages/mdm.js`.) `Mdm.Verify` (session-only, audited as
`mdm_verify` with the enrollment id before anything is sent or asked) answers in one call: the status as `Status` computes it at that call; whether the Macs' listener runs, whether it has a TLS configuration, the
names of the certificate it serves and whether they cover the MDM name; the ABM check; and, when the caller names an enrolled Mac, a test push. The ABM check asks Apple through a port,
`AbmAccountCheck`, which the ABM client (CM13) implements and the MDM server holds; `GET /account` is the proof, since it answers only an accepted token (spike `abm.md`). A coordinator with
no such client answers `not_checked`, never `accepted`; with no token it answers `no_token`, with a token and no master key or one the key does not open or that does not read `unreadable` (the detail says which), without asking. The test push goes through the sender of the queue and
its pusher, by `Pusher::test_push`, which is outside the push policy: it waits for no floor, arms none and leaves no push outstanding, with `apns-expiration` 0, so that a test push
a Mac got later proves nothing; no command is queued and nothing is recorded against the Mac. Its result is one class: sent, no push identity, no listener, no such (live) enrollment, no push token,
and the answers of APNs apart: `BadDeviceToken` and `410` (this Mac's token), `TopicDisallowed` (the certificate and topic do not belong together, for every Mac),
`BadCertificateEnvironment` (a development certificate on the production service, which arrives as a connection alert, not a response), another certificate refusal, no client certificate, throttled, unavailable, any
other refusal and a failed connection. A check that fails is a field of the answer, not a refusal. The page loads the enrolled Macs by a button, so that drawing the page asks for nothing it does not show.
**Not verified:** the ABM check against Apple (only the port and a fake are built until CM13 gives the client), and a test push against the real APNs; the classes are those of the spikes and of `apns.rs`.
