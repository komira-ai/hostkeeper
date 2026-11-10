//! The rules of the mdm package in `artifacts.yml` ([`super::release::MDM_STEPS`], and the mdm
//! `.deb` in [`super::release::NATIVE_FILES`]): the file passes as it is, and each rule refuses its defect put into an otherwise clean copy.

use super::lint::lint;
use super::release::{MDM_IF, MDM_STEPS};
use super::{with_in, workflows};

/// The repository's artifacts.yml.
fn artifacts_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "artifacts.yml")
        .map(|(_, text)| text)
        .expect("artifacts.yml")
}

/// Fails unless the lint finds a problem holding `want` in `text` as artifacts.yml.
#[track_caller]
fn assert_refused(text: &str, want: &str) {
    let found = lint("artifacts.yml", text);
    assert!(
        found.iter().any(|p| p.contains(want)),
        "no problem holds {want:?}; found {found:?}"
    );
}

/// The `run:` line of `step`, as the file writes it.
fn run_line(step: &str) -> String {
    format!("        run: {step}\n")
}

#[test]
fn artifacts_yml_makes_the_mdm_deb_on_linux_x86_64() {
    // Catches: the rules themselves refusing the committed file (a step text or an `if:` they
    // spell differently from it).
    let text = artifacts_yml();
    assert_eq!(lint("artifacts.yml", &text), Vec::<String>::new());
    for step in MDM_STEPS {
        assert_eq!(text.matches(&run_line(step)).count(), 1, "{step}");
    }
}

#[test]
fn a_missing_mdm_step_is_refused_and_so_is_one_out_of_order() {
    // Catches: a release whose mdm package is built from archives no step fetched (or never
    // built at all, so `release check` refuses the release for lacking it), its program not
    // checked for its stamp, or the package built before its archives are fetched.
    let text = artifacts_yml();
    for step in MDM_STEPS {
        let without = with_in(&text, &run_line(step), "        run: echo nothing\n");
        assert_refused(
            &without,
            &format!("artifacts.yml: job build does not run `{step}`"),
        );
    }
    // The .deb built, then the archives fetched: the fetch is not found after the check.
    let [_, check, fetch, build] = MDM_STEPS;
    let swapped = with_in(&text, &run_line(fetch), "        run: FETCH\n");
    let swapped = with_in(&swapped, &run_line(build), &run_line(fetch));
    let swapped = with_in(&swapped, "        run: FETCH\n", &run_line(build));
    let found = lint("artifacts.yml", &swapped);
    assert!(
        found.contains(&format!(
            "artifacts.yml: job build does not run `{build}` (after the mdm steps before it)"
        )),
        "{found:?}"
    );
    assert!(!found.iter().any(|p| p.contains(check)), "{found:?}");
}

#[test]
fn an_mdm_step_under_another_if_or_a_build_without_the_commit_is_refused() {
    // Catches: an mdm step run on linux-arm64 too (a package refused for its platform, so the
    // arm64 leg fails), on no platform, or the program built without RELEASE_COMMIT (so it
    // prints a developer stamp and check-binary refuses it, or worse, a check that is skipped).
    let text = artifacts_yml();
    let fetch = MDM_STEPS[2];
    let guarded = format!("        if: {MDM_IF}\n{}", run_line(fetch));
    let other = format!(
        "        if: matrix.platform != 'macos-arm64'\n{}",
        run_line(fetch)
    );
    assert_refused(
        &with_in(&text, &guarded, &other),
        &format!("artifacts.yml: job build runs `{fetch}` without the exact `if: {MDM_IF}`"),
    );
    assert_refused(
        &with_in(&text, &guarded, &run_line(fetch)),
        &format!("artifacts.yml: job build runs `{fetch}` without the exact `if: {MDM_IF}`"),
    );
    let stamped = "        env:\n          RELEASE_COMMIT: ${{ github.sha }}\n        run: cargo build --locked --release -p hostkeeper-mdm --bin mdm\n";
    assert_refused(
        &with_in(&text, stamped, &run_line(MDM_STEPS[0])),
        &format!(
            "artifacts.yml: job build runs `{}` without RELEASE_COMMIT: ${{{{ github.sha }}}}",
            MDM_STEPS[0]
        ),
    );
}

#[test]
fn the_mdm_deb_is_attested_on_linux_x86_64_with_its_bundle() {
    // Catches: a release asset list that names the mdm .deb's bundle while no step signs it, or
    // one that signs it without copying the bundle beside it: a host trusts only attested
    // packages.
    let text = artifacts_yml();
    let subject =
        "          subject-path: dist/mdm_${{ needs.version.outputs.version }}_amd64.deb\n";
    assert_refused(
        &with_in(&text, subject, "          subject-path: dist/mdm.deb\n"),
        "artifacts.yml: job attest does not attest mdm_<version>_amd64.deb",
    );
    assert_refused(
        &with_in(
            &text,
            "steps.mdm-deb-linux-x86_64.outputs.bundle-path",
            "steps.hkctl.outputs.bundle-path",
        ),
        "artifacts.yml: job attest does not copy the bundle of mdm_<version>_amd64.deb from the step mdm-deb-linux-x86_64",
    );
}
