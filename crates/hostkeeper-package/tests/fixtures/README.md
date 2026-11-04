# Attestation bundle fixtures

Synthetic Sigstore bundles for the tests of `hostkeeper-package`, and the tiny archive they attest.
They are **not signed**: the certificate holds only what this crate and the tests' fake `gh` read,
and the DSSE envelope has no signature. No real Sigstore verification runs in the hermetic tests,
so no Sigstore trusted root was used or captured. A run of the real `gh attestation verify` against
a real attestation belongs to the privileged workflow and the privileged proof of the design's build
order (section 12.2).

## How they were made

Each bundle is the output of `crate::testing::bundle_json` for the specification listed in
`src/fixtures_tests.rs` (`fixtures()`), and the test
`fixture_bundles_parse_and_the_bad_ones_are_rejected_for_the_reason_the_readme_names` fails when a
committed file differs from that output by one byte. To make a new one, add its row to `fixtures()`
and to the table below, and write the function's output to the file.

A bundle is one line of JSON: `mediaType` (bundle v0.3), `verificationMaterial.certificate.rawBytes`
(the certificate, base64) and `dsseEnvelope` (the in-toto statement, base64, with an empty
`signatures` list).

- The certificate is DER: a `tbsCertificate` with a version, the serial number 1 and five Sigstore
  (Fulcio) extensions, each a UTF8String: the build signer URI
  (`https://github.com/<organisation>/<repository>/<workflow>@<ref>`, `1.3.6.1.4.1.57264.1.9`),
  the runner environment (`.11`), the source repository digest, which is the commit (`.13`), the
  source repository ref (`.14`), and the source repository owner URI
  (`https://github.com/<organisation>`, `.16`). It has no key, no issuer and no signature.
- The statement has one subject (`good.tar.zst` and a SHA-256) and an SLSA provenance v1
  predicate whose `buildDefinition.resolvedDependencies` names the source repository at the ref
  and one `gitCommit`.

`good.tar.zst` is 39 bytes of text, not a zstd archive: this crate never opens an archive.

The good specification: organisation `example-org`, repository `tool-a`, workflow
`.github/workflows/artifacts.yml`, ref `refs/heads/main`, commit
`0123456789abcdef0123456789abcdef01234567` in both the certificate and the provenance, runner
`github-hosted`, subject `good.tar.zst` with the SHA-256 of `good.tar.zst`. Each bad one changes
one thing.

## The fixtures

The tests are configured with the trusted signer `example-org`, `.github/workflows/artifacts.yml`,
refs `refs/heads/main`, hosted runner required, for the service `service-b` (so the good bundle, from
the repository `tool-a`, also shows that no repository is compared with the service). The last
column is the reason the test expects, exactly.

| file | what it stands for | refused by |
|---|---|---|
| `good.sigstore.json` | a good attestation of `good.tar.zst` | accepted |
| `other-bytes.sigstore.json` | an attestation of other bytes (subject: the SHA-256 of `other bytes than the archive\n`): the channel serves a bundle that is not this archive's; refused before `gh` runs | the digest check before gh |
| `other-workflow.sigstore.json` | signed by `.github/workflows/release.yml`, a workflow other than the configured one | gh: --cert-identity-regex |
| `embedded-at-workflow.sigstore.json` | signed by `.github/workflows/artifacts.yml@other.yml`, a different workflow whose file name starts with the configured one and an `@`: the signer URI is then `.../artifacts.yml@other.yml@refs/heads/main`, which the complete-URI pattern refuses | gh: --cert-identity-regex |
| `other-organisation.sigstore.json` | from the repository `other-org/tool-a`, outside the configured organisation | gh: --owner |
| `self-hosted.sigstore.json` | built on a `self-hosted` runner | gh: --deny-self-hosted-runners |
| `disallowed-ref.sigstore.json` | made on the tag `refs/tags/v1.2.3`, which no configured ref matches; refused before `gh` runs | the ref refs/tags/v1.2.3 matches none of the trusted signer's refs |
| `commit-differs.sigstore.json` | the provenance names the commit `89abcdef0123456789abcdef0123456789abcdef`, the certificate another | gh: --source-digest |
