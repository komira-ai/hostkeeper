//! The release rules ([`super::release`]) on the pxe package's steps of `artifacts.yml`: the
//! package is attested for both Linux platforms, each bundle copied from its own step, and for
//! no macOS platform; each rule is shown to refuse its defect in an otherwise clean copy.

use super::release_tests::{all_problems, assert_refused, attest_steps, workflow};
use super::with_in;

/// The pxe package is attested for both Linux platforms, with its bundle copied from its own
/// step, and for no macOS platform.
///
/// Catches a release whose pxe package for a Linux platform is not attested, whose bundle is not
/// copied, or is copied from another package's step.
#[test]
fn attest_job_attests_the_pxe_package_for_each_linux_platform() {
    for platform in ["linux-x86_64", "linux-arm64"] {
        let pxe = attest_steps(
            &format!("pxe-{platform}"),
            &format!("Name the bundle of the pxe package for {platform}"),
            &format!("pxe-${{V}}-{platform}.tar.zst"),
            &format!("pxe-${{{{ needs.version.outputs.version }}}}-{platform}.tar.zst"),
        );
        let text = workflow("artifacts.yml");
        assert!(text.contains(&pxe), "{platform}");
        assert_refused(
            "artifacts.yml",
            &with_in(&text, &pxe, ""),
            &format!("artifacts.yml: job attest does not attest pxe-<version>-{platform}.tar.zst"),
        );
        let copy = format!(
            "        run: cp \"$BUNDLE\" \"dist/pxe-${{V}}-{platform}.tar.zst.sigstore.json\"\n"
        );
        let found = all_problems(
            "artifacts.yml",
            &with_in(&text, &copy, "        run: echo\n"),
        );
        assert!(
            found.iter().any(|p| p.contains(&format!(
                "job attest does not copy the bundle of pxe-<version>-{platform}.tar.zst"
            ))),
            "{found:?}"
        );
        assert_refused(
            "artifacts.yml",
            &with_in(
                &text,
                &format!("steps.pxe-{platform}.outputs.bundle-path"),
                &format!("steps.hello-{platform}.outputs.bundle-path"),
            ),
            &format!(
                "job attest does not copy the bundle of pxe-<version>-{platform}.tar.zst from the step pxe-{platform}"
            ),
        );
    }
    assert!(!workflow("artifacts.yml").contains("pxe-${V}-macos-arm64"));
}
