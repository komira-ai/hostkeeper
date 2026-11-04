# Captured bundle

`gh-linux-amd64.sigstore.json` is a real attestation bundle that GitHub's own build produced and
published, kept byte for byte as served (one line of compact JSON, then a newline). It exists so
the offline parser (`src/bundle.rs`) is read against a certificate and provenance written by an
independent producer, not only against the synthetic bundles of `src/testing.rs`.

- Repository: `cli/cli` (https://github.com/cli/cli)
- Release tag: `v2.103.3`
- Asset: `gh_2.103.3_linux_amd64.tar.gz`
- Asset sha256 (from the release's `gh_2.103.3_checksums.txt`):
  `4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d`
- Source commit the attestation records: `63837fb778d6a39f02be0ce76ff89f2bc8d74bdc`
- Source ref of the signing certificate: `refs/heads/trunk`
- Captured on: 2026-11-04

## How it was captured

```
gh release download v2.103.3 --repo cli/cli --pattern '*checksums.txt'
gh api /repos/cli/cli/attestations/sha256:4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d
```

The answer lists two attestations; `bundle` is null in both and each carries a short-lived
`bundle_url` instead. The bundle is the file at the `bundle_url` of the attestation whose
predicate type is `https://slsa.dev/provenance/v1` (the other is the release attestation, which
names no commit). That file is a raw Snappy block (no framing); decompressed, it is the bundle
JSON, which was re-written compactly on one line.

## Use

The tests only parse it offline (`bundle::read`): no signature, certificate chain or transparency
log is checked, and nothing is fetched. Cryptographic verification of a real bundle belongs to
the designated release validation, not to the hermetic tests.
