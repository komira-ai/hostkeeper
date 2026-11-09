//! The rules of the signing of the installer pkg ([`super::pkg_signing`]): the committed
//! artifacts.yml passes, and each defect put into a copy of it is refused with its own line.

use super::lint::lint;
use super::pkg_signing::{
    BUILD_PKG, CHECK_JOB, CHECK_SIGNATURE, CHECK_STEPS, DELETE_KEYCHAIN, DELETE_P12, IDENTITY,
    PUBLISHED_ARTIFACT, SAME_FILES, SIGN, SIGN_ENV, SIGN_JOB, SIGN_STEPS, SIGNED_ARTIFACT, SUMS_IF,
    Step, UNSIGNED_ARTIFACT,
};
use super::release::SIGN_IF;
use super::{with_in, workflows};

/// The repository's artifacts.yml.
fn artifacts_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "artifacts.yml")
        .map(|(_, text)| text)
        .expect("artifacts.yml")
}

/// The problems of `text` as artifacts.yml.
fn found(text: &str) -> Vec<String> {
    lint("artifacts.yml", text)
}

/// Fails unless the lint reports `want`, exactly, of `text`.
#[track_caller]
fn assert_refused(text: &str, want: &str) {
    let found = found(text);
    assert!(
        found.iter().any(|p| p == want),
        "{want:?} is not among {found:?}"
    );
}

/// The `run:` line of a step, as the file writes it.
fn run_line(run: &str) -> String {
    format!("        run: {run}\n")
}

/// The problem of the step `n` (from 1) of the signing job that is not the `n`th of
/// [`SIGN_STEPS`].
fn step_problem(n: usize) -> String {
    format!(
        "artifacts.yml: job {SIGN_JOB}: step {n} is not {:?}",
        SIGN_STEPS[n - 1]
    )
}

/// The two cleanup steps, as the file writes them.
const DELETE_KEYCHAIN_STEP: &str = "      - name: Delete the keychain, whether the signing worked or not\n        if: ${{ always() }}\n";
const DELETE_P12_STEP: &str = "      - name: Delete the identity's file, whether the signing worked or not\n        if: ${{ always() }}\n";

#[test]
fn the_committed_artifacts_yml_signs_checks_and_publishes_the_pkg() {
    // Catches: the rules refusing the committed file (a step or a value they spell differently
    // from it), or a step of SIGN_STEPS the file does not hold exactly once.
    let text = artifacts_yml();
    assert_eq!(found(&text), Vec::<String>::new());
    for step in SIGN_STEPS {
        if let Step::Run { run, .. } = step {
            assert!(text.contains(&format!("run: {run}\n")), "{run}");
        }
    }
    for run in [BUILD_PKG, CHECK_SIGNATURE, SAME_FILES] {
        assert_eq!(text.matches(&run_line(run)).count(), 1, "{run}");
    }
    assert_eq!(text.matches(IDENTITY).count(), 2);
}

#[test]
fn the_keychain_cleanup_missing_or_not_run_on_failure_is_refused() {
    // Catches: a keychain or a PKCS#12 file left on the runner when productsign fails (the
    // cleanup without `always()`), never deleted at all, or deleted only after the upload.
    let text = artifacts_yml();
    let not_always = with_in(
        &text,
        DELETE_KEYCHAIN_STEP,
        "      - name: Delete the keychain\n",
    );
    assert_refused(&not_always, &step_problem(11));
    let p12_not_always = with_in(
        &text,
        DELETE_P12_STEP,
        "      - name: Delete the identity's file\n        if: ${{ success() }}\n",
    );
    assert_refused(&p12_not_always, &step_problem(12));

    let without = with_in(
        &text,
        &format!("{DELETE_KEYCHAIN_STEP}{}", run_line(DELETE_KEYCHAIN)),
        "",
    );
    assert_refused(
        &without,
        &format!("artifacts.yml: job {SIGN_JOB} has 12 steps, not the 13 of SIGN_STEPS"),
    );
    assert_refused(&without, &step_problem(11));

    // The p12's deletion after the upload: the steps from 12 on are out of place.
    let p12 = format!("{DELETE_P12_STEP}{}", run_line(DELETE_P12));
    let upload = format!(
        "      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n        with:\n          name: {SIGNED_ARTIFACT}\n          path: dist/\n          retention-days: 90\n          if-no-files-found: error\n"
    );
    let late = with_in(&text, &format!("{p12}{upload}"), &format!("{upload}{p12}"));
    assert_refused(&late, &step_problem(12));
    assert_refused(&late, &step_problem(13));
}

#[test]
fn a_signing_into_the_login_keychain_or_without_a_timestamp_is_refused() {
    // Catches: the identity imported into the login keychain (where it outlives the job's
    // cleanup), the job's keychain not being the search list, or a signature that lapses with
    // the certificate.
    let text = artifacts_yml();
    let login = with_in(
        &text,
        "-k \"$RUNNER_TEMP/pkg-signing.keychain-db\" -f pkcs12",
        "-k \"$HOME/Library/Keychains/login.keychain-db\" -f pkcs12",
    );
    assert_refused(&login, &step_problem(6));
    let listed = with_in(
        &text,
        "security list-keychains -d user -s \"$RUNNER_TEMP/pkg-signing.keychain-db\"",
        "security list-keychains -d user -s \"$RUNNER_TEMP/pkg-signing.keychain-db\" \"$HOME/Library/Keychains/login.keychain-db\"",
    );
    assert_refused(&listed, &step_problem(8));
    let untimed = with_in(
        &text,
        SIGN,
        &SIGN.replace("--timestamp ", "--timestamp=none "),
    );
    assert_refused(&untimed, &step_problem(10));
}

#[test]
fn the_signing_job_running_repository_code_or_holding_more_is_refused() {
    // Catches: a checkout or a cargo step in the job that holds the key (a build script or a
    // dependency would run beside it), the job on another trigger, runner or environment, a
    // write scope, or other needs.
    let text = artifacts_yml();
    let header = format!("  {SIGN_JOB}:\n    needs: [version, build-macos]\n    if: {SIGN_IF}\n");
    let checkout = with_in(
        &text,
        "          name: pkg-unsigned-macos-arm64\n          path: unsigned\n      - name: The identity",
        "          name: pkg-unsigned-macos-arm64\n          path: unsigned\n      - run: cargo xtask --help\n      - name: The identity",
    );
    assert_refused(
        &checkout,
        &format!("artifacts.yml: job {SIGN_JOB} has 14 steps, not the 13 of SIGN_STEPS"),
    );
    assert_refused(&checkout, &step_problem(2));

    let dispatch = with_in(
        &text,
        &header,
        &format!(
            "  {SIGN_JOB}:\n    needs: [version, build-macos]\n    if: ${{{{ github.ref == 'refs/heads/main' }}}}\n"
        ),
    );
    assert_refused(
        &dispatch,
        &format!("artifacts.yml: job {SIGN_JOB} needs the exact `if: {SIGN_IF}`"),
    );
    let needs = with_in(
        &text,
        &header,
        &format!("  {SIGN_JOB}:\n    needs: [version, build-macos, build]\n    if: {SIGN_IF}\n"),
    );
    assert_refused(
        &needs,
        &format!("artifacts.yml: job {SIGN_JOB} must need exactly [\"build-macos\", \"version\"]"),
    );
    let env = with_in(
        &text,
        &format!("    environment: {SIGN_ENV}\n"),
        "    environment: release\n",
    );
    assert_refused(
        &env,
        &format!("artifacts.yml: job {SIGN_JOB} must be in the environment {SIGN_ENV}"),
    );
    let runner = with_in(
        &text,
        &format!("    runs-on: macos-15\n    timeout-minutes: 15\n    environment: {SIGN_ENV}\n"),
        &format!("    runs-on: macos-14\n    timeout-minutes: 15\n    environment: {SIGN_ENV}\n"),
    );
    assert_refused(
        &runner,
        &format!("artifacts.yml: job {SIGN_JOB} must run on macos-15"),
    );
    let scope = with_in(
        &text,
        &format!("    environment: {SIGN_ENV}\n    permissions:\n      contents: read\n"),
        &format!(
            "    environment: {SIGN_ENV}\n    permissions:\n      contents: read\n      id-token: write\n"
        ),
    );
    assert_refused(
        &scope,
        &format!("artifacts.yml: job {SIGN_JOB} must hold `contents: read` and nothing else"),
    );
}

#[test]
fn a_secret_named_outside_the_signing_steps_is_refused() {
    // Catches: the identity's secrets handed to a job that runs repository code, to the whole
    // workflow, to the signing job's own env (every step would see it), or another secret in the
    // signing job.
    let text = artifacts_yml();
    let in_build = with_in(
        &text,
        "      - run: gtar --version\n",
        "      - run: gtar --version\n        env:\n          K: ${{ secrets.MACOS_INSTALLER_P12 }}\n",
    );
    assert_refused(
        &in_build,
        &format!("artifacts.yml: job build-macos names a secret; only {SIGN_JOB} may"),
    );
    let beside = with_in(
        &text,
        "  CARGO_INCREMENTAL: 0\n",
        "  CARGO_INCREMENTAL: 0\n  K: ${{ secrets.MACOS_INSTALLER_P12 }}\n",
    );
    assert_refused(&beside, "artifacts.yml: names a secret beside the jobs");
    let job_env = with_in(
        &text,
        &format!(
            "      IDENTITY: \"{IDENTITY}\"\n    steps:\n      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1\n        with:\n          name: pkg-unsigned-macos-arm64\n          path: unsigned\n      - name: The identity"
        ),
        &format!(
            "      IDENTITY: \"{IDENTITY}\"\n      K: ${{{{ secrets.MACOS_INSTALLER_P12 }}}}\n    steps:\n      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1\n        with:\n          name: pkg-unsigned-macos-arm64\n          path: unsigned\n      - name: The identity"
        ),
    );
    assert_refused(
        &job_env,
        &format!(
            "artifacts.yml: job {SIGN_JOB} names a secret 6 times, not once in the env of each step of SIGN_STEPS that takes one (5)"
        ),
    );
    let other = with_in(
        &text,
        "          P12: ${{ secrets.MACOS_INSTALLER_P12 }}\n",
        "          P12: ${{ secrets.APT_GPG_PRIVATE_KEY }}\n",
    );
    assert_refused(
        &other,
        &format!(
            "artifacts.yml: job {SIGN_JOB} names a secret other than [\"MACOS_INSTALLER_P12\", \"MACOS_INSTALLER_P12_PASSWORD\"] (`secrets.APT_GPG_PRIVATE_KEY }}}}`)"
        ),
    );
    assert_refused(&other, &step_problem(2));
}

/// The problem of the step `n` (from 1) of the check job that is not the `n`th of
/// [`CHECK_STEPS`].
fn check_step_problem(n: usize) -> String {
    format!(
        "artifacts.yml: job {CHECK_JOB}: step {n} is not {:?}",
        CHECK_STEPS[n - 1]
    )
}

/// The check step, as the file writes it.
fn check_step() -> String {
    format!(
        "      - name: The signature is the identity's, under Apple's chain, with a trusted timestamp\n{}",
        run_line(CHECK_SIGNATURE)
    )
}

/// The upload of the check job, as the file writes it.
fn published_upload() -> String {
    format!(
        "      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n        with:\n          name: {PUBLISHED_ARTIFACT}\n          path: dist/\n          retention-days: 90\n          if-no-files-found: error\n"
    )
}

#[test]
fn the_signature_check_missing_skipped_or_after_the_upload_is_refused() {
    // Catches: a signed pkg published without its signature checked against the identity and
    // Apple's chain, a check whose failure does not fail the job (`continue-on-error`, on the
    // step or the job) or that never runs (an `if:`), a check only after the upload (a failed
    // check would leave the pkg in the release files), or no comparison with the unsigned pkg
    // that build-macos checked.
    let text = artifacts_yml();
    let check = check_step();
    let without = with_in(
        &text,
        &run_line(CHECK_SIGNATURE),
        "        run: echo nothing\n",
    );
    assert_refused(&without, &check_step_problem(5));
    let late = with_in(
        &with_in(&text, &check, ""),
        &published_upload(),
        &format!("{}{check}", published_upload()),
    );
    assert_refused(&late, &check_step_problem(5));
    assert_refused(&late, &check_step_problem(9));
    for key in [
        "continue-on-error: true",
        "if: ${{ false }}",
        "shell: sh",
        "working-directory: unsigned",
    ] {
        let skipped = with_in(
            &text,
            &check,
            &check.replace(
                "        run: cargo",
                &format!("        {key}\n        run: cargo"),
            ),
        );
        assert_refused(&skipped, &check_step_problem(5));
    }
    let whole_job = with_in(
        &text,
        &format!("  {CHECK_JOB}:\n    needs: [version, sign-pkg]\n"),
        &format!("  {CHECK_JOB}:\n    continue-on-error: true\n    needs: [version, sign-pkg]\n"),
    );
    assert_refused(
        &whole_job,
        &format!("artifacts.yml: job {CHECK_JOB} has the key continue-on-error"),
    );

    let no_diff = with_in(&text, &run_line(SAME_FILES), "        run: echo same\n");
    assert_refused(&no_diff, &check_step_problem(8));
    let unordered = with_in(
        &text,
        &format!("  {CHECK_JOB}:\n    needs: [version, sign-pkg]\n"),
        &format!("  {CHECK_JOB}:\n    needs: [version, build-macos]\n"),
    );
    assert_refused(
        &unordered,
        &format!("artifacts.yml: job {CHECK_JOB} must need exactly [\"sign-pkg\", \"version\"]"),
    );
    let env = format!(
        "    env:\n      RELEASE_VERSION: ${{{{ needs.version.outputs.version }}}}\n      IDENTITY: \"{IDENTITY}\"\n    steps:\n      - uses: actions/checkout@"
    );
    let want_env = format!(
        "artifacts.yml: job {CHECK_JOB} must have exactly the env [(\"RELEASE_VERSION\", \"${{{{ needs.version.outputs.version }}}}\"), (\"IDENTITY\", {IDENTITY:?})]"
    );
    let identity = with_in(
        &text,
        &env,
        &env.replace(
            IDENTITY,
            "Developer ID Installer: Someone Else (AAAAAAAAAA)",
        ),
    );
    assert_refused(&identity, &want_env);
    let extra = with_in(
        &text,
        &env,
        &env.replace("    steps:", "      RUSTFLAGS: -Cfoo\n    steps:"),
    );
    assert_refused(&extra, &want_env);
}

#[test]
fn the_recorded_sha256_of_the_pkg_is_of_the_signed_file() {
    // Catches: any way the bytes SHA256SUMS lists (and attest signs, and the release and the
    // MDM gate's allowlist take) for hostkeeper-<version>-macos-arm64.pkg are not the signed
    // pkg's: the unsigned pkg built into dist/ (so build-macos's assets-macos-arm64 carries
    // it), the check job publishing the unsigned download or another directory, another job
    // publishing a pkg, or the sums made before the check job published the signed one.
    let text = artifacts_yml();
    let into_dist = with_in(
        &text,
        &run_line(BUILD_PKG),
        &run_line(&BUILD_PKG.replace("--out pkg", "--out dist")),
    );
    assert_refused(
        &into_dist,
        &format!(
            "artifacts.yml: job build-macos does not build the unsigned pkg into pkg/ (`{BUILD_PKG}`)"
        ),
    );
    let unsigned_upload =
        "          name: pkg-unsigned-${{ env.PLATFORM }}\n          path: pkg/\n";
    let not_uploaded = with_in(
        &text,
        unsigned_upload,
        "          name: pkg-unsigned-${{ env.PLATFORM }}\n          path: dist/\n",
    );
    assert_refused(
        &not_uploaded,
        &format!("artifacts.yml: job build-macos does not upload pkg/ as {UNSIGNED_ARTIFACT}"),
    );

    let signed_download = format!("          name: {SIGNED_ARTIFACT}\n          path: dist\n");
    let unsigned_published = with_in(
        &text,
        &signed_download,
        "          name: pkg-signed-macos-arm64\n          path: signed\n",
    );
    assert_refused(&unsigned_published, &check_step_problem(4));
    let published = format!("          name: {PUBLISHED_ARTIFACT}\n          path: dist/\n");
    let elsewhere = with_in(
        &text,
        &published,
        &format!("          name: {PUBLISHED_ARTIFACT}\n          path: unsigned/\n"),
    );
    assert_refused(&elsewhere, &check_step_problem(9));
    let unnamed = with_in(
        &text,
        &published,
        "          name: assets-macos-arm64-pkg\n          path: dist/\n",
    );
    assert_refused(&unnamed, &check_step_problem(9));
    // The unsigned pkg fetched over the checked one, under the same name, before the upload.
    let swapped = with_in(
        &text,
        &published_upload(),
        &format!(
            "      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1\n        with:\n          name: {UNSIGNED_ARTIFACT}\n          path: dist\n{}",
            published_upload()
        ),
    );
    assert_refused(
        &swapped,
        &format!("artifacts.yml: job {CHECK_JOB} has 10 steps, not the 9 of CHECK_STEPS"),
    );
    assert_refused(&swapped, &check_step_problem(9));
    // The signing job fed a pkg of another run.
    let other_run = with_in(
        &text,
        &format!(
            "          name: {UNSIGNED_ARTIFACT}\n          path: unsigned\n      - name: The identity"
        ),
        &format!(
            "          name: {UNSIGNED_ARTIFACT}\n          path: unsigned\n          run-id: 1\n      - name: The identity"
        ),
    );
    assert_refused(&other_run, &step_problem(1));

    let from_build = with_in(
        &text,
        unsigned_upload,
        "          name: assets-pkg-${{ env.PLATFORM }}\n          path: pkg/\n",
    );
    assert_refused(
        &from_build,
        &format!(
            "artifacts.yml: job build-macos uploads an assets-pkg-* artifact; only {CHECK_JOB} publishes the pkg"
        ),
    );

    let sums_header =
        format!("    needs: [version, build, build-macos, image, check-pkg]\n    if: {SUMS_IF}\n");
    let want =
        format!("artifacts.yml: job sums must need {CHECK_JOB} under the exact `if: {SUMS_IF}`");
    let early = with_in(
        &text,
        &sums_header,
        &format!("    needs: [version, build, build-macos, image]\n    if: {SUMS_IF}\n"),
    );
    assert_refused(&early, &want);
    let skipped = with_in(
        &text,
        &sums_header,
        "    needs: [version, build, build-macos, image, check-pkg]\n",
    );
    assert_refused(&skipped, &want);
}

#[test]
fn an_artifacts_yml_without_the_signing_jobs_is_refused_for_each() {
    // Catches: the jobs removed or renamed, which would publish no pkg (or the unsigned one)
    // without the other rules noticing.
    let text = artifacts_yml();
    let renamed = with_in(
        &text,
        &format!("\n  {SIGN_JOB}:\n"),
        "\n  sign-installer:\n",
    );
    let renamed = renamed.replace("[version, sign-pkg]", "[version, sign-installer]");
    assert_refused(&renamed, &format!("artifacts.yml: has no job {SIGN_JOB}"));
    let renamed = with_in(
        &text,
        &format!("\n  {CHECK_JOB}:\n"),
        "\n  check-installer:\n",
    );
    let renamed = renamed.replace("image, check-pkg]", "image, check-installer]");
    assert_refused(&renamed, &format!("artifacts.yml: has no job {CHECK_JOB}"));
}

#[test]
fn a_signing_step_or_job_with_a_key_of_its_own_is_refused() {
    // Catches: a signing that may fail without failing the job (so check-pkg is fed whatever
    // dist/ holds), a cleanup that may be skipped by its own key, or the signing job taking a
    // container, a strategy or an env of its own.
    let text = artifacts_yml();
    let sign = run_line(SIGN);
    let lenient = with_in(
        &text,
        &sign,
        &format!("        continue-on-error: true\n{sign}"),
    );
    assert_refused(&lenient, &step_problem(10));
    let cleanup = run_line(DELETE_KEYCHAIN);
    let timed = with_in(
        &text,
        &cleanup,
        &format!("        timeout-minutes: 0\n{cleanup}"),
    );
    assert_refused(&timed, &step_problem(11));
    let header = format!("    environment: {SIGN_ENV}\n");
    let job_key = with_in(
        &text,
        &header,
        &format!("{header}    continue-on-error: true\n"),
    );
    assert_refused(
        &job_key,
        &format!("artifacts.yml: job {SIGN_JOB} has the key continue-on-error"),
    );
}
