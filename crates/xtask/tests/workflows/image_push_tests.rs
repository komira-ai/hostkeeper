//! The registry-push rule of `release.yml` and `artifacts.yml`: only the job `image` of
//! `release.yml` holds `packages: write`, and only under the release `if:`. Each defect is put into
//! an otherwise clean copy of the file and the problem the lint reports is read.

use super::lint::{WRITE_ALLOWED, lint};
use super::release::RELEASE_IF;
use super::{with_in, workflows};

/// The text of the repository's workflow `name`.
fn workflow(name: &str) -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == name)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{name} is missing"))
}

/// Fails unless the lint's problems for `text` as `file` include a line holding `want`.
#[track_caller]
fn assert_refused(file: &str, text: &str, want: &str) {
    let found = lint(file, text);
    assert!(
        found.iter().any(|p| p.contains(want)),
        "no problem holds {want:?}; found {found:?} in:\n{text}"
    );
}

#[test]
fn only_the_image_job_of_release_yml_holds_packages_write_and_only_under_its_if() {
    // Catches: a second job that can push to the registry, the image pushed by a run of another
    // branch or event than a release of main, or a workflow other than release.yml pushing.
    let pushers: Vec<(&str, &str, &str)> = WRITE_ALLOWED
        .iter()
        .filter(|a| a.scope == "packages")
        .map(|a| (a.file, a.job, a.condition))
        .collect();
    assert_eq!(pushers, [("release.yml", "image", RELEASE_IF)]);
    assert_eq!(
        lint("release.yml", &workflow("release.yml")),
        Vec::<String>::new()
    );

    let image_job = "  image:\n    needs: [plan, publish]\n";
    let verify_scope = "      actions: read\n      attestations: read\n";
    let in_verify = with_in(
        &workflow("release.yml"),
        verify_scope,
        &format!("{verify_scope}      packages: write\n"),
    );
    assert_refused(
        "release.yml",
        &in_verify,
        "job verify: `packages: write` is not allowed in release.yml",
    );
    let wider = with_in(
        &workflow("release.yml"),
        &format!("{image_job}    if: {RELEASE_IF}\n"),
        &format!("{image_job}    if: ${{{{ github.ref == 'refs/heads/main' }}}}\n"),
    );
    assert_refused(
        "release.yml",
        &wider,
        &format!("job image: `packages: write` needs the exact `if: {RELEASE_IF}`"),
    );
    let attest_scope = "      id-token: write\n      attestations: write\n";
    let in_artifacts = with_in(
        &workflow("artifacts.yml"),
        attest_scope,
        &format!("{attest_scope}      packages: write\n"),
    );
    assert_refused(
        "artifacts.yml",
        &in_artifacts,
        "job attest: `packages: write` is not allowed in artifacts.yml",
    );
}
