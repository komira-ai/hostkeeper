//! The attempts of the promotion to `stable` ([`super::packages::last_attempt`]): five for the apt
//! index, which every product of the repository promotes into, three for the rpm tree. Each case
//! puts one defect into the repository's `release.yml` and reads the problem the lint reports.

use super::packages_tests::{assert_refused, in_job, job_range, problems, release_yml};

#[test]
fn the_apt_promotion_to_stable_has_five_attempts_and_the_rpm_one_three() {
    // Catches: the apt promotion cut back to three attempts (the shared index of stable is lost
    // to another product's promotion often enough that three in a row have all lost), a fifth
    // attempt that asks for a sixth, a third that fails where two are left, and an rpm write
    // whose third attempt asks for a fourth.
    let text = release_yml();
    assert_eq!(problems(&text), Vec::<String>::new());
    assert_refused(
        &in_job(&text, "stable-apt-write-5", " --last", ""),
        "release.yml: job stable-apt-write-5 passes --last exactly in the fifth attempt, which fails on a lost race instead of asking for another",
    );
    assert_refused(
        &in_job(
            &text,
            "stable-apt-write-3",
            " --no-pool",
            " --no-pool --last",
        ),
        "release.yml: job stable-apt-write-3 passes --last exactly in the fifth attempt, which fails on a lost race instead of asking for another",
    );
    assert_refused(
        &in_job(&text, "stable-rpm-write-3", " --last", ""),
        "release.yml: job stable-rpm-write-3 passes --last exactly in the third attempt, which fails on a lost race instead of asking for another",
    );
}

#[test]
fn the_later_apt_attempts_run_after_the_one_before_lost() {
    // Catches: a fourth attempt that runs after the wrong write, and a fifth write that runs
    // without its own signed index.
    let text = release_yml();
    let wrong = in_job(
        &text,
        "stable-apt-read-4",
        "needs.stable-apt-write-3.outputs.conflict == 'true'",
        "needs.stable-apt-write-2.outputs.conflict == 'true'",
    );
    let found = problems(&wrong);
    assert!(
        found.iter().any(|p| p.starts_with("release.yml: job stable-apt-read-4 needs the exact `if: ${{ !cancelled() && !failure() && needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && needs.stable-apt-write-3.outputs.conflict == 'true' }}`")),
        "{found:?}"
    );
    let unsigned = in_job(
        &text,
        "stable-apt-write-5",
        " && needs.stable-apt-index-5.result == 'success' }}",
        " }}",
    );
    let found = problems(&unsigned);
    assert!(
        found.iter().any(|p| p.starts_with("release.yml: job stable-apt-write-5 needs the exact `if: ${{ !cancelled() && !failure() && needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && needs.stable-apt-read-5.result == 'success' && needs.stable-apt-index-5.result == 'success' }}`")),
        "{found:?}"
    );
}

#[test]
fn a_workflow_with_three_apt_attempts_to_stable_is_refused() {
    // Catches: the promotion as it was before, attempts 4 and 5 removed and the third passing
    // --last.
    let text = release_yml();
    let (start, _) = job_range(&text, "stable-apt-read-4");
    let (end, _) = job_range(&text, "stable-rpm-read");
    let three = format!("{}{}", &text[..start], &text[end..]);
    let three = in_job(
        &three,
        "stable-apt-write-3",
        " --no-pool",
        " --no-pool --last",
    );
    let found = problems(&three);
    for job in [
        "stable-apt-read-4",
        "stable-apt-index-4",
        "stable-apt-write-4",
        "stable-apt-read-5",
        "stable-apt-index-5",
        "stable-apt-write-5",
    ] {
        let want = format!("release.yml: job {job} is missing");
        assert!(found.contains(&want), "no {want:?} in {found:?}");
    }
}
