//! The coordinator updater's release files in `artifacts.yml` and `release.yml`
//! ([`super::release`]): its tarball for each Linux platform is attested with its bundle and its
//! SBOM (the verify job of `release.yml` runs it: `release_tests.rs` holds every shipped binary to
//! that). Each rule is shown to refuse its defect by taking the step out of the repository's file
//! and reading the problem it reports.

use super::release_tests::{assert_refused, attest_steps, workflow};
use super::with_in;

/// The program, the stem of its tarball's name.
const PROGRAM: &str = "hostkeeper-coordinator-update";

#[test]
fn attest_job_attests_the_coordinator_updaters_tarball_with_its_bundle_and_its_sbom() {
    // Catches: the coordinator updater's tarball, on either Linux platform, listed by the release
    // while the attest job does not sign it (so the machine of the coordinator's container could
    // not check what it installs), signs it without copying its bundle beside it, copies the
    // bundle of another step, or attests no SBOM for it (or the SBOM of another file).
    for platform in ["linux-x86_64", "linux-arm64"] {
        let id = format!("coordinator-update-{platform}");
        let steps = attest_steps(
            &id,
            &format!("Name the bundle of the tarball of {PROGRAM} for {platform}"),
            &format!("{PROGRAM}-${{V}}-{platform}.tar.gz"),
            &format!("{PROGRAM}-${{{{ needs.version.outputs.version }}}}-{platform}.tar.gz"),
        );
        let text = workflow("artifacts.yml");
        assert!(
            text.contains(&steps),
            "the helper writes the steps as the file has them"
        );
        assert_refused(
            "artifacts.yml",
            &with_in(&text, &steps, ""),
            &format!(
                "artifacts.yml: job attest does not attest {PROGRAM}-<version>-{platform}.tar.gz (a provenance step with an id and the subject-path dist/{PROGRAM}-${{{{ needs.version.outputs.version }}}}-{platform}.tar.gz)"
            ),
        );
        let text = with_in(
            &workflow("artifacts.yml"),
            &format!("steps.{id}.outputs.bundle-path"),
            &format!("steps.mdm-{platform}.outputs.bundle-path"),
        );
        assert_refused(
            "artifacts.yml",
            &text,
            &format!(
                "artifacts.yml: job attest does not copy the bundle of {PROGRAM}-<version>-{platform}.tar.gz from the step {id} to dist/{PROGRAM}-${{V}}-{platform}.tar.gz.sigstore.json"
            ),
        );
        let sbom = format!(
            "          sbom-path: dist/{PROGRAM}-${{{{ needs.version.outputs.version }}}}-{platform}.cdx.json\n"
        );
        let other = format!(
            "          sbom-path: dist/hostkeeper-mdm-${{{{ needs.version.outputs.version }}}}-{platform}.cdx.json\n"
        );
        assert_refused(
            "artifacts.yml",
            &with_in(&workflow("artifacts.yml"), &sbom, &other),
            &format!(
                "artifacts.yml: job attest does not attest the SBOM of {PROGRAM}-<version>-{platform}.tar.gz"
            ),
        );
    }
}
