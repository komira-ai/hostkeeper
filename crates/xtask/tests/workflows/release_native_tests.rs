//! The attest job of the repository's own `artifacts.yml` on the services' native packages:
//! hello's `.deb`s and `.pkg` and mdm's `.deb` are each attested, with their bundle copied beside
//! them, and the release rules ([`super::release`]) refuse a copy of the file that drops either.
//! Beside `release_tests.rs`, whose helpers it uses.

use super::release_tests::{assert_refused, attest_steps, workflow};
use super::with_in;

#[test]
fn attest_job_attests_the_services_native_packages_and_copies_each_bundle() {
    // Catches: hello's or mdm's native package listed as an asset (and published to the
    // repository's testing channel, whose installer directory holds each bundle) while the
    // attest job does not sign it, signs it under another name, or copies another step's bundle
    // beside it.
    let text = workflow("artifacts.yml");
    for (id, name, file, missing) in [
        (
            "hello-deb-linux-x86_64",
            "Name the bundle of hello's .deb for linux-x86_64",
            "hello_${V}_amd64.deb",
            "hello_<version>_amd64.deb",
        ),
        (
            "hello-deb-linux-arm64",
            "Name the bundle of hello's .deb for linux-arm64",
            "hello_${V}_arm64.deb",
            "hello_<version>_arm64.deb",
        ),
        (
            "mdm-deb-linux-x86_64",
            "Name the bundle of mdm's .deb for linux-x86_64",
            "mdm_${V}_amd64.deb",
            "mdm_<version>_amd64.deb",
        ),
        (
            "hello-pkg-macos-arm64",
            "Name the bundle of hello's .pkg for macos-arm64",
            "hello-${V}-macos-arm64.pkg",
            "hello-<version>-macos-arm64.pkg",
        ),
    ] {
        let subject = file.replace("${V}", "${{ needs.version.outputs.version }}");
        let steps = attest_steps(id, name, file, &subject);
        assert!(
            text.contains(&steps),
            "the helper writes the steps as the file has them: {id}"
        );
        assert_refused(
            "artifacts.yml",
            &with_in(&text, &steps, ""),
            &format!(
                "artifacts.yml: job attest does not attest {missing} (a provenance step with an id and the subject-path dist/{subject})"
            ),
        );
        assert_refused(
            "artifacts.yml",
            &with_in(
                &text,
                &format!("steps.{id}.outputs.bundle-path"),
                "steps.netboot.outputs.bundle-path",
            ),
            &format!(
                "artifacts.yml: job attest does not copy the bundle of {missing} from the step {id} to dist/{file}.sigstore.json"
            ),
        );
    }
}
