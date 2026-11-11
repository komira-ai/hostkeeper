//! The rules of the pxe package in `artifacts.yml` ([`super::release::PXE_BUILD`] and
//! [`super::release::PXE_RUNS`], and the pxe `.deb`s in [`super::release::NATIVE_FILES`]): the
//! file passes as it is, and each rule refuses its defect put into an otherwise clean copy.

use super::lint::lint;
use super::release::{PXE_BUILD, PXE_RUNS};
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
fn artifacts_yml_builds_and_checks_the_pxe_deb_on_both_linux_platforms() {
    // Proves: the committed file passes the pxe rules, and each of the three runs they pin
    // appears once in it (the static build of `pxe`, the `.deb` built from it, the `.deb` read
    // back), so the rules and the file spell them alike.
    // Catches: a rule that spells a run differently from the file.
    let text = artifacts_yml();
    assert_eq!(lint("artifacts.yml", &text), Vec::<String>::new());
    for run in [PXE_BUILD, PXE_RUNS[0], PXE_RUNS[1]] {
        assert_eq!(text.matches(&run_line(run)).count(), 1, "{run}");
    }
}

#[test]
fn a_missing_pxe_run_is_refused() {
    // Proves: each of the build, the `.deb` step and its read-back is required of the build job.
    // Catches: a release whose pxe package is never built (so `release check` refuses the
    // release for lacking it), is built from the wrong binary, or is not read back.
    let text = artifacts_yml();
    for run in [PXE_BUILD, PXE_RUNS[0], PXE_RUNS[1]] {
        let without = with_in(&text, &run_line(run), "        run: echo nothing\n");
        assert_refused(
            &without,
            &format!("artifacts.yml: job build does not run `{run}`"),
        );
    }
}

#[test]
fn the_pxe_deb_built_from_the_gnu_binary_or_out_of_dist_is_refused() {
    // Proves: the `.deb` step must take the musl binary and write into dist/, where the attest
    // job and SHA256SUMS find it.
    // Catches: a `.deb` of a glibc build (it would not run on a distribution older than the
    // runner), or one written to a directory no later step reads.
    let text = artifacts_yml();
    for (old, new) in [
        (
            "--binary \"target/$MUSL_TARGET/release/pxe\"",
            "--binary target/release/pxe",
        ),
        ("pxe\" --out dist", "pxe\" --out native"),
    ] {
        assert_refused(
            &with_in(&text, old, new),
            &format!("artifacts.yml: job build does not run `{}`", PXE_RUNS[0]),
        );
    }
}

#[test]
fn a_pxe_run_under_an_if_is_refused() {
    // Proves: the pxe runs are for both Linux legs, so none may carry an `if:`.
    // Catches: pxe's `.deb` limited to the linux-x86_64 leg (as mdm's is), so the release lacks
    // the arm64 file and every host of that architecture is ordered nothing.
    let text = artifacts_yml();
    for run in [PXE_RUNS[0], PXE_BUILD] {
        let guarded = format!(
            "        if: matrix.platform == 'linux-x86_64'\n{}",
            run_line(run)
        );
        assert_refused(
            &with_in(&text, &run_line(run), &guarded),
            &format!(
                "artifacts.yml: job build runs `{run}` under an `if:`, so not on both Linux platforms"
            ),
        );
    }
}
