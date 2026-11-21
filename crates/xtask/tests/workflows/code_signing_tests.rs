//! The rules of the signing of the release's Mach-O programs ([`super::code_signing`]): the
//! committed artifacts.yml passes, and each defect put into a copy of it is refused with its own
//! line.

use super::code_signing::{
    BUILD_JOB, CHECK_BOOTSTRAP_CODE, CHECK_HELLO_CODE, CHECK_HOSTKEEPER_CODE, CHECK_PACKAGED_HK,
    CHECK_PACKAGED_HOSTKEEPER, CHMOD, CODE_IDENTITY, COMPILE_JOB, DELETE_CODE_KEYCHAIN,
    DELETE_CODE_P12, EXPAND_BOOTSTRAP_FULL, EXPAND_FULL, EXPAND_HELLO_FULL, NOT_SET_P12,
    NOT_SET_PASSWORD, PROGRAMS, SIGN_BOOTSTRAP_CODE, SIGN_CODE_JOB, SIGN_CODE_STEPS,
    SIGN_HELLO_CODE, SIGN_HK, SIGN_HOSTKEEPER, SIGNED_MACHO, UNSIGNED_MACHO,
};
use super::lint::lint;
use super::pkg_signing::{CHECK_JOB, CHECK_STEPS, SIGN_ENV, SIGN_JOB, SUMS_IF, Step};
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

fn found(text: &str) -> Vec<String> {
    lint("artifacts.yml", text)
}

/// Fails unless the lint reports `want`, exactly, among the problems of `text`.
#[track_caller]
fn assert_refused(text: &str, want: &str) {
    let found = found(text);
    assert!(
        found.iter().any(|p| p == want),
        "{want:?} is not among {found:?}"
    );
}

/// The `run:` line of a plain step, as the file writes it.
fn run_line(run: &str) -> String {
    format!("        run: {run}\n")
}

/// The `run:` line of a step the file writes in single quotes.
fn quoted_run_line(run: &str) -> String {
    format!("        run: '{run}'\n")
}

/// The problem of the step `n` (from 1) of the signing job that is not the `n`th of
/// [`SIGN_CODE_STEPS`].
fn code_step(n: usize) -> String {
    format!(
        "artifacts.yml: job {SIGN_CODE_JOB}: step {n} is not {:?}",
        SIGN_CODE_STEPS[n - 1]
    )
}

/// The problem of the step `n` (from 1) of the check job that is not the `n`th of
/// [`CHECK_STEPS`].
fn check_step(n: usize) -> String {
    format!(
        "artifacts.yml: job {CHECK_JOB}: step {n} is not {:?}",
        CHECK_STEPS[n - 1]
    )
}

/// The problem of a program not signed as the release signs it.
fn unsigned_program(name: &str, identifier: &str) -> String {
    format!(
        "artifacts.yml: job {SIGN_CODE_JOB} does not sign {name} with `--identifier {identifier} --options runtime --timestamp signed/{name}`"
    )
}

/// The head of the signing job up to its environment, as the file writes it.
fn sign_head() -> String {
    format!(
        "  {SIGN_CODE_JOB}:\n    needs: [{COMPILE_JOB}]\n    if: {SIGN_IF}\n    runs-on: macos-15\n    timeout-minutes: 15\n    environment: {SIGN_ENV}\n"
    )
}

/// The head of the compile job, as the file writes it.
fn compile_head() -> String {
    format!(
        "  {COMPILE_JOB}:\n    name: compile macos-arm64\n    needs: [version]\n    runs-on: macos-15\n    timeout-minutes: 45\n    permissions:\n      contents: read\n"
    )
}

/// The head of the packaging job, as the file writes it.
fn build_head() -> String {
    format!(
        "  {BUILD_JOB}:\n    name: macos-arm64\n    needs: [version, {COMPILE_JOB}, {SIGN_CODE_JOB}]\n    if: {SUMS_IF}\n"
    )
}

/// The downloads of `build-macos`, as the file writes them.
fn build_downloads() -> String {
    format!(
        "      - name: The compiled programs\n        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1\n        with:\n          name: {UNSIGNED_MACHO}\n          path: target/release\n"
    )
}

fn signed_download() -> String {
    format!(
        "      - name: The signed programs, over the compiled ones (push to main)\n        if: {SIGN_IF}\n        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1\n        with:\n          name: {SIGNED_MACHO}\n          path: target/release\n"
    )
}

fn chmod_step() -> String {
    format!(
        "      - name: The programs are executable\n{}",
        run_line(CHMOD)
    )
}

#[test]
fn the_committed_artifacts_yml_compiles_signs_and_packages_the_programs() {
    // Catches: the rules refusing the committed file (a step or a value they spell differently
    // from it), or a command of the lists the file does not hold exactly once.
    let text = artifacts_yml();
    assert_eq!(found(&text), Vec::<String>::new());
    for step in SIGN_CODE_STEPS {
        if let Step::Run { run, .. } = step {
            let plain = text.matches(&run_line(run)).count();
            let quoted = text.matches(&quoted_run_line(run)).count();
            let bare = text.matches(&format!("      - run: {run}\n")).count();
            assert!(plain + quoted + bare == 1, "{run}");
        }
    }
    for run in [
        SIGN_HOSTKEEPER,
        SIGN_HK,
        SIGN_HELLO_CODE,
        SIGN_BOOTSTRAP_CODE,
        CHMOD,
        CHECK_PACKAGED_HOSTKEEPER,
        CHECK_PACKAGED_HK,
        EXPAND_FULL,
        CHECK_HOSTKEEPER_CODE,
        EXPAND_HELLO_FULL,
        CHECK_HELLO_CODE,
        EXPAND_BOOTSTRAP_FULL,
        CHECK_BOOTSTRAP_CODE,
    ] {
        assert_eq!(text.matches(&run_line(run)).count(), 1, "{run}");
    }
    assert_eq!(PROGRAMS.len(), 4);
}

#[test]
fn the_compile_job_with_other_needs_runner_scope_environment_or_upload_is_refused() {
    // Catches: the job that runs repository code holding the key's environment or a write
    // scope, running elsewhere, or not handing the four programs on under the artifact the
    // signing job downloads.
    let text = artifacts_yml();
    let head = compile_head();
    let needs = with_in(
        &text,
        &head,
        &head.replace("needs: [version]", "needs: [version, build]"),
    );
    assert_refused(
        &needs,
        &format!("artifacts.yml: job {COMPILE_JOB} must need exactly [\"version\"]"),
    );
    let runner = with_in(&text, &head, &head.replace("macos-15", "macos-14"));
    assert_refused(
        &runner,
        &format!("artifacts.yml: job {COMPILE_JOB} must run on macos-15"),
    );
    let scope = with_in(&text, &head, &format!("{head}      id-token: write\n"));
    assert_refused(
        &scope,
        &format!("artifacts.yml: job {COMPILE_JOB} must hold `contents: read` and nothing else"),
    );
    let environment = with_in(
        &text,
        &head,
        &head.replace(
            "    runs-on:",
            &format!("    environment: {SIGN_ENV}\n    runs-on:"),
        ),
    );
    assert_refused(
        &environment,
        &format!("artifacts.yml: job {COMPILE_JOB} must be in no environment"),
    );
    let upload = format!("          name: {UNSIGNED_MACHO}\n          path: |\n");
    let renamed = with_in(
        &text,
        &upload,
        "          name: macho-compiled\n          path: |\n",
    );
    let no_upload = format!(
        "artifacts.yml: job {COMPILE_JOB} does not upload the four programs as {UNSIGNED_MACHO}"
    );
    assert_refused(&renamed, &no_upload);
    let fewer = with_in(
        &text,
        "            target/release/hello\n            target/release/hostkeeper-bootstrap\n",
        "            target/release/hello\n",
    );
    assert_refused(&fewer, &no_upload);
}

#[test]
fn the_signing_job_with_other_needs_condition_runner_scope_or_environment_is_refused() {
    // Catches: the signing job on another trigger, runner, environment or needs, with a write
    // scope or an env of its own, or with a key (`continue-on-error`) its steps would not hold.
    let text = artifacts_yml();
    let head = sign_head();
    let dispatch = with_in(
        &text,
        &head,
        &head.replace(SIGN_IF, "${{ github.ref == 'refs/heads/main' }}"),
    );
    assert_refused(
        &dispatch,
        &format!("artifacts.yml: job {SIGN_CODE_JOB} needs the exact `if: {SIGN_IF}`"),
    );
    let needs = with_in(
        &text,
        &head,
        &head.replace("[compile-macos]", "[compile-macos, version]"),
    );
    assert_refused(
        &needs,
        &format!("artifacts.yml: job {SIGN_CODE_JOB} must need exactly [\"{COMPILE_JOB}\"]"),
    );
    let environment = with_in(
        &text,
        &head,
        &head.replace(&format!("environment: {SIGN_ENV}"), "environment: release"),
    );
    assert_refused(
        &environment,
        &format!("artifacts.yml: job {SIGN_CODE_JOB} must be in the environment {SIGN_ENV}"),
    );
    let runner = with_in(&text, &head, &head.replace("macos-15", "macos-14"));
    assert_refused(
        &runner,
        &format!("artifacts.yml: job {SIGN_CODE_JOB} must run on macos-15"),
    );
    let scope = with_in(
        &text,
        &format!("{head}    permissions:\n      contents: read\n"),
        &format!("{head}    permissions:\n      contents: read\n      id-token: write\n"),
    );
    assert_refused(
        &scope,
        &format!("artifacts.yml: job {SIGN_CODE_JOB} must hold `contents: read` and nothing else"),
    );
    let key = with_in(
        &text,
        &head,
        &format!("{head}    continue-on-error: true\n"),
    );
    assert_refused(
        &key,
        &format!("artifacts.yml: job {SIGN_CODE_JOB} has the key continue-on-error"),
    );
    let env = format!("      IDENTITY: \"{CODE_IDENTITY}\"\n");
    let want_env = format!(
        "artifacts.yml: job {SIGN_CODE_JOB} must have exactly the env {:?}",
        [("IDENTITY", CODE_IDENTITY)]
    );
    let other_identity = with_in(
        &text,
        &env,
        "      IDENTITY: \"Developer ID Application: Someone Else (AAAAAAAAAA)\"\n",
    );
    assert_refused(&other_identity, &want_env);
    let extra = with_in(&text, &env, &format!("{env}      RUSTFLAGS: -Cfoo\n"));
    assert_refused(&extra, &want_env);
}

#[test]
fn a_checkout_or_cargo_step_in_the_signing_job_or_a_step_moved_or_changed_is_refused() {
    // Catches: repository code running beside the key (a checkout, a cargo step), a step added,
    // removed or changed, or one with a key of its own.
    let text = artifacts_yml();
    let after_download = format!("          name: {UNSIGNED_MACHO}\n          path: unsigned\n");
    let cargo = with_in(
        &text,
        &after_download,
        &format!("{after_download}      - run: cargo xtask --help\n"),
    );
    assert_refused(
        &cargo,
        &format!(
            "artifacts.yml: job {SIGN_CODE_JOB} has {} steps, not the {} of SIGN_CODE_STEPS",
            SIGN_CODE_STEPS.len() + 1,
            SIGN_CODE_STEPS.len()
        ),
    );
    assert_refused(&cargo, &code_step(2));
    let other_run = with_in(
        &text,
        &after_download,
        &format!("{after_download}          run-id: 1\n"),
    );
    assert_refused(&other_run, &code_step(1));
    let list = with_in(
        &text,
        "        run: security list-keychains -d user -s \"$RUNNER_TEMP/code-signing.keychain-db\"\n",
        "        run: security list-keychains -d user -s \"$RUNNER_TEMP/code-signing.keychain-db\" \"$HOME/Library/Keychains/login.keychain-db\"\n",
    );
    assert_refused(&list, &code_step(10));
    let login = with_in(
        &text,
        "-k \"$RUNNER_TEMP/code-signing.keychain-db\" -f pkcs12",
        "-k \"$HOME/Library/Keychains/login.keychain-db\" -f pkcs12",
    );
    assert_refused(&login, &code_step(8));
    let lenient = with_in(
        &text,
        &run_line(SIGN_HK),
        &format!("        continue-on-error: true\n{}", run_line(SIGN_HK)),
    );
    assert_refused(&lenient, &code_step(13));
}

#[test]
fn a_secret_that_is_not_set_must_stop_the_job_by_its_name() {
    // Catches: a missing secret reaching `base64 --decode` or `security` and failing with a
    // message that does not name the secret, a check that does not stop the job, or one that
    // names the other secret.
    let text = artifacts_yml();
    let p12 = quoted_run_line(NOT_SET_P12);
    let absent = with_in(&text, &p12, "        run: echo fine\n");
    assert_refused(&absent, &code_step(2));
    let go_on = with_in(&text, &p12, &p12.replace("exit 1", "exit 0"));
    assert_refused(&go_on, &code_step(2));
    let password = quoted_run_line(NOT_SET_PASSWORD);
    let other = with_in(
        &text,
        &password,
        &password.replace(
            "the secret MACOS_APPLICATION_P12_PASSWORD",
            "the secret MACOS_APPLICATION_P12",
        ),
    );
    assert_refused(&other, &code_step(3));
}

#[test]
fn a_program_not_signed_under_its_identifier_with_the_runtime_and_a_timestamp_is_refused() {
    // Catches: a program signed under another identifier (the privacy profile would not match
    // it), without the hardened runtime, without a secure timestamp, or not signed at all.
    let text = artifacts_yml();
    let cases: [(&str, &str, usize, &str, &str); 4] = [
        (
            SIGN_HOSTKEEPER,
            "--identifier dev.hostkeeper.hostkeeper",
            12,
            "hostkeeper",
            "dev.hostkeeper.hostkeeper",
        ),
        (SIGN_HK, "--options runtime ", 13, "hk", "dev.hostkeeper.hk"),
        (
            SIGN_HELLO_CODE,
            "--identifier dev.hostkeeper.hello",
            14,
            "hello",
            "dev.hostkeeper.hello",
        ),
        (
            SIGN_BOOTSTRAP_CODE,
            "--timestamp signed",
            15,
            "hostkeeper-bootstrap",
            "dev.hostkeeper.bootstrap",
        ),
    ];
    for (run, part, step, name, identifier) in cases {
        let changed = match part {
            "--options runtime " => run.replace(part, ""),
            "--timestamp signed" => run.replace(part, "--timestamp=none signed"),
            _ => run.replace(part, "--identifier hostkeeper"),
        };
        let mutated = with_in(&text, &run_line(run), &run_line(&changed));
        assert_refused(&mutated, &code_step(step));
        assert_refused(&mutated, &unsigned_program(name, identifier));
    }
    let never = with_in(
        &text,
        &run_line(SIGN_HELLO_CODE),
        "        run: cp unsigned/hello signed/hello\n",
    );
    assert_refused(&never, &unsigned_program("hello", "dev.hostkeeper.hello"));
}

#[test]
fn the_keychain_and_the_p12_left_after_a_failed_signing_or_the_upload_before_them_is_refused() {
    // Catches: a key left on the runner when codesign fails (a cleanup without `always()`), or
    // the signed programs uploaded before the cleanup.
    let text = artifacts_yml();
    let keychain = format!(
        "      - name: Delete the keychain, whether the signing worked or not\n        if: ${{{{ always() }}}}\n{}",
        run_line(DELETE_CODE_KEYCHAIN)
    );
    let not_always = with_in(
        &text,
        &keychain,
        &keychain.replace("${{ always() }}", "${{ success() }}"),
    );
    assert_refused(&not_always, &code_step(16));
    let p12 = format!(
        "      - name: Delete the identity's file, whether the signing worked or not\n        if: ${{{{ always() }}}}\n{}",
        run_line(DELETE_CODE_P12)
    );
    let upload = format!(
        "      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n        with:\n          name: {SIGNED_MACHO}\n          path: signed/\n          retention-days: 90\n          if-no-files-found: error\n"
    );
    let late = with_in(&text, &format!("{p12}{upload}"), &format!("{upload}{p12}"));
    assert_refused(&late, &code_step(17));
    assert_refused(&late, &code_step(18));
}

#[test]
fn a_secret_named_outside_the_signing_job_or_the_wrong_one_in_it_is_refused() {
    // Catches: the application identity's secrets handed to the job that compiles, to the
    // installer's job, or the installer's secret to the programs' job, or one named twice.
    let text = artifacts_yml();
    let in_compile = with_in(
        &text,
        "      - name: Build hostkeeper\n        env:\n          RELEASE_COMMIT: ${{ github.sha }}\n        run: cargo build --locked --release -p hostkeeper --bin hostkeeper\n      - name: Build hk\n",
        "      - name: Build hostkeeper\n        env:\n          RELEASE_COMMIT: ${{ github.sha }}\n          K: ${{ secrets.MACOS_APPLICATION_P12 }}\n        run: cargo build --locked --release -p hostkeeper --bin hostkeeper\n      - name: Build hk\n",
    );
    assert_refused(
        &in_compile,
        &format!(
            "artifacts.yml: job {COMPILE_JOB} names a secret; only {SIGN_JOB} and {SIGN_CODE_JOB} may"
        ),
    );
    let in_pkg = with_in(
        &text,
        "          P12: ${{ secrets.MACOS_INSTALLER_P12 }}\n",
        "          P12: ${{ secrets.MACOS_APPLICATION_P12 }}\n",
    );
    assert_refused(
        &in_pkg,
        &format!(
            "artifacts.yml: job {SIGN_JOB} names a secret other than [\"MACOS_INSTALLER_P12\", \"MACOS_INSTALLER_P12_PASSWORD\"] (`secrets.MACOS_APPLICATION_P12 }}}}`)"
        ),
    );
    let in_code = with_in(
        &text,
        "          P12: ${{ secrets.MACOS_APPLICATION_P12 }}\n        run: base64 --decode",
        "          P12: ${{ secrets.MACOS_INSTALLER_P12 }}\n        run: base64 --decode",
    );
    assert_refused(
        &in_code,
        &format!(
            "artifacts.yml: job {SIGN_CODE_JOB} names a secret other than [\"MACOS_APPLICATION_P12\", \"MACOS_APPLICATION_P12_PASSWORD\"] (`secrets.MACOS_INSTALLER_P12 }}}}`)"
        ),
    );
    let takes = SIGN_CODE_STEPS
        .iter()
        .filter(|s| {
            matches!(
                s,
                Step::Run {
                    secret: Some(_),
                    ..
                }
            )
        })
        .count();
    let in_env = with_in(
        &text,
        &format!("      IDENTITY: \"{CODE_IDENTITY}\"\n"),
        &format!(
            "      IDENTITY: \"{CODE_IDENTITY}\"\n      K: ${{{{ secrets.MACOS_APPLICATION_P12 }}}}\n"
        ),
    );
    assert_refused(
        &in_env,
        &format!(
            "artifacts.yml: job {SIGN_CODE_JOB} names a secret {} times, not once in the env of each step of SIGN_CODE_STEPS that takes one ({takes})",
            takes + 1
        ),
    );
}

#[test]
fn the_packaging_job_with_other_needs_or_condition_or_that_builds_is_refused() {
    // Catches: a job that packages programs it does not wait for the signing of, one that a
    // skipped signing (a run by hand) or a failed one would not stop, or one that compiles a
    // program of its own (unsigned, beside the signed ones).
    let text = artifacts_yml();
    let head = build_head();
    let needs = with_in(
        &text,
        &head,
        &head.replace(
            "[version, compile-macos, sign-code]",
            "[version, compile-macos]",
        ),
    );
    assert_refused(
        &needs,
        &format!(
            "artifacts.yml: job {BUILD_JOB} must need exactly [\"{COMPILE_JOB}\", \"{SIGN_CODE_JOB}\", \"version\"]"
        ),
    );
    let condition = with_in(&text, &head, &head.replace(SUMS_IF, "${{ always() }}"));
    assert_refused(
        &condition,
        &format!("artifacts.yml: job {BUILD_JOB} needs the exact `if: {SUMS_IF}`"),
    );
    let builds = with_in(
        &text,
        &chmod_step(),
        &format!(
            "{}      - run: cargo build --release -p hello\n",
            chmod_step()
        ),
    );
    assert_refused(
        &builds,
        &format!(
            "artifacts.yml: job {BUILD_JOB} builds a program; the programs it packages are compiled by {COMPILE_JOB} and signed by {SIGN_CODE_JOB}"
        ),
    );
}

#[test]
fn the_packaged_programs_not_downloaded_in_order_or_not_made_executable_are_refused() {
    // Catches: unsigned programs packaged on a push to main (the signed download missing or
    // skipped), the signed ones overwritten by the unsigned ones, or programs without their
    // mode (an artifact drops it).
    let text = artifacts_yml();
    let no_unsigned = with_in(
        &text,
        &build_downloads(),
        &build_downloads().replace("path: target/release", "path: target/debug"),
    );
    assert_refused(
        &no_unsigned,
        &format!(
            "artifacts.yml: job {BUILD_JOB} does not download {UNSIGNED_MACHO} into target/release"
        ),
    );
    let signed = signed_download();
    let always = with_in(
        &text,
        &signed,
        &signed.replace(&format!("        if: {SIGN_IF}\n"), ""),
    );
    let want_signed = format!(
        "artifacts.yml: job {BUILD_JOB} does not download {SIGNED_MACHO} into target/release under the exact `if: {SIGN_IF}`"
    );
    assert_refused(&always, &want_signed);
    let elsewhere = with_in(
        &text,
        &signed,
        &signed.replace("path: target/release", "path: signed"),
    );
    assert_refused(&elsewhere, &want_signed);
    let no_chmod = with_in(
        &text,
        &run_line(CHMOD),
        &run_line(&CHMOD.replace("0755", "0777")),
    );
    assert_refused(
        &no_chmod,
        &format!("artifacts.yml: job {BUILD_JOB} does not run `{CHMOD}`"),
    );
    let reordered = with_in(
        &with_in(&text, &signed, ""),
        &build_downloads(),
        &format!("{signed}{}", build_downloads()),
    );
    assert_refused(
        &reordered,
        &format!(
            "artifacts.yml: job {BUILD_JOB} must download {UNSIGNED_MACHO}, then {SIGNED_MACHO}, then set the modes"
        ),
    );
}

#[test]
fn the_packaged_hostkeeper_and_hk_not_checked_on_a_push_to_main_are_refused() {
    // Catches: tarballs whose programs are not checked against the release's signature, or
    // checked only under another condition.
    let text = artifacts_yml();
    for command in [CHECK_PACKAGED_HOSTKEEPER, CHECK_PACKAGED_HK] {
        let want = format!(
            "artifacts.yml: job {BUILD_JOB} does not run `{command}` under the exact `if: {SIGN_IF}`"
        );
        let echo = with_in(&text, &run_line(command), "        run: echo fine\n");
        assert_refused(&echo, &want);
        let step = format!("        if: {SIGN_IF}\n{}", run_line(command));
        let unconditional = with_in(&text, &step, &run_line(command));
        assert_refused(&unconditional, &want);
    }
}

#[test]
fn a_signed_pkg_whose_program_is_not_read_back_is_refused() {
    // Catches: a pkg published without the signature of the program inside it checked, at its
    // payload path, against the program's own name, for each of the three pkgs.
    let text = artifacts_yml();
    let cases = [
        (EXPAND_FULL, 9),
        (CHECK_HOSTKEEPER_CODE, 10),
        (EXPAND_HELLO_FULL, 15),
        (CHECK_HELLO_CODE, 16),
        (EXPAND_BOOTSTRAP_FULL, 21),
        (CHECK_BOOTSTRAP_CODE, 22),
    ];
    for (run, n) in cases {
        let skipped = with_in(&text, &run_line(run), "        run: echo fine\n");
        assert_refused(&skipped, &check_step(n));
    }
    let other_program = with_in(
        &text,
        &run_line(CHECK_BOOTSTRAP_CODE),
        &run_line(&CHECK_BOOTSTRAP_CODE.replace("--program hostkeeper-bootstrap", "--program hk")),
    );
    assert_eq!(found(&other_program), [check_step(22)]);
    let other_path = with_in(
        &text,
        &run_line(CHECK_HOSTKEEPER_CODE),
        &run_line(
            &CHECK_HOSTKEEPER_CODE
                .replace("/Payload/usr/local/bin/hostkeeper", "/Payload/hostkeeper"),
        ),
    );
    assert_eq!(found(&other_path), [check_step(10)]);
}
