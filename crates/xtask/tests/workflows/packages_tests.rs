//! The rules of the package-channel jobs of `release.yml` ([`super::packages`]): the repository's
//! file passes, and each rule is shown to refuse its defect by putting the defect into one job of
//! the repository's `release.yml` and reading the problem the lint reports.

use super::lint::lint;
use super::packages::{
    NEWEST_RUN, PUBLISH_ENV, SIGNING_ENV, STAGES, UBUNTU_IMAGE, docker_line_ok, id_token_condition,
    job_names, rows,
};
use super::release::publish_problems;
use super::workflows;
use super::xtask_calls::{call_problem, commands, xtask_args};

/// The repository's release.yml.
fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// Every problem the lint and the publish rule find in `text` as release.yml.
fn problems(text: &str) -> Vec<String> {
    let mut found = lint("release.yml", text);
    found.extend(publish_problems("release.yml", text));
    found
}

/// The byte range of the job `name` in `text`: from its header to the next job's header.
fn job_range(text: &str, name: &str) -> (usize, usize) {
    let start = text
        .find(&format!("\n  {name}:\n"))
        .unwrap_or_else(|| panic!("release.yml has no job {name}"))
        + 1;
    let end = text[start..]
        .match_indices("\n  ")
        .map(|(at, _)| start + at + 1)
        .find(|&at| {
            let rest = &text[at + 2..];
            !rest.starts_with(' ')
                && !rest.starts_with('#')
                && rest.split_once(":\n").is_some_and(|(n, _)| {
                    n.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                })
        })
        .unwrap_or(text.len());
    (start, end)
}

/// `text` with the first `from` inside the job `name` replaced by `to`.
fn in_job(text: &str, name: &str, from: &str, to: &str) -> String {
    let (start, end) = job_range(text, name);
    let job = &text[start..end];
    assert!(
        job.contains(from),
        "{from:?} is not in the job {name}:\n{job}"
    );
    format!(
        "{}{}{}",
        &text[..start],
        job.replacen(from, to, 1),
        &text[end..]
    )
}

#[track_caller]
fn assert_refused(text: &str, want: &str) {
    let found = problems(text);
    assert!(
        found.iter().any(|p| p.contains(want)),
        "no problem holds {want:?}; found {found:?}"
    );
}

#[test]
fn the_package_channel_jobs_of_the_repository_pass() {
    // Catches: a rule that refuses the jobs as they are, so that every case below would fail for
    // that reason instead of the defect it adds.
    assert_eq!(problems(&release_yml()), Vec::<String>::new());
}

#[test]
fn the_rows_hold_the_key_and_the_role_in_no_job_and_the_role_in_exactly_the_writers_and_readers() {
    // Catches: a table that lets one job hold the signing key and assume the cloud role, or a
    // job that assumes the role without an exact `if:` for its `id-token: write`.
    let all = rows();
    assert_eq!(all.len(), 43);
    let mut names = job_names();
    names.dedup();
    assert_eq!(names.len(), all.len(), "a job has two rows");
    for row in &all {
        let role = row.permissions.contains(&("id-token", "write"));
        let assumes = row.verbs.iter().any(|v| v == "assume");
        assert_eq!(
            role, assumes,
            "{}: id-token and channels assume go together",
            row.name
        );
        assert!(
            !(role && row.environment == Some(SIGNING_ENV)),
            "{} holds the key and the role",
            row.name
        );
        assert_eq!(role, row.environment == Some(PUBLISH_ENV), "{}", row.name);
        assert_eq!(
            id_token_condition("release.yml", &row.name).is_some(),
            role,
            "{}",
            row.name
        );
        let signs = row
            .verbs
            .iter()
            .any(|v| v.ends_with("sign") && v != "rpm check-signed");
        assert_eq!(
            signs,
            row.environment == Some(SIGNING_ENV),
            "{} signs",
            row.name
        );
    }
    assert_eq!(id_token_condition("ci.yml", "testing-apt-read"), None);
    assert_eq!(id_token_condition("release.yml", "testing-apt-index"), None);
}

#[test]
fn a_missing_or_extra_job_is_refused() {
    // Catches: a package job deleted (its row unchecked), and a job added beside them that could
    // write or sign without a row.
    let text = release_yml();
    let (start, end) = job_range(&text, "check-pkg");
    let without = format!("{}{}", &text[..start], &text[end..]);
    assert_refused(&without, "release.yml: job check-pkg is missing");
    assert_refused(&without, "release.yml: jobs are [");
    let extra =
        format!("{text}  notify:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo done\n");
    assert_refused(&extra, "release.yml: jobs are [");
}

#[test]
fn a_job_with_other_needs_or_another_if_is_refused() {
    // Catches: a job that runs without the job it reads from, a retry that runs when it should
    // not, and a stable job that no longer waits for the decision.
    let text = release_yml();
    assert_refused(
        &in_job(
            &text,
            "check-pkg",
            "needs: [plan, newest, testing-pkg-upload]",
            "needs: [plan, testing-pkg-upload]",
        ),
        "release.yml: job check-pkg needs [\"plan\", \"testing-pkg-upload\"], not [\"newest\", \"plan\", \"testing-pkg-upload\"]",
    );
    assert_refused(
        &in_job(
            &text,
            "stable-apt-write-2",
            "!cancelled() && !failure() && ",
            "",
        ),
        "release.yml: job stable-apt-write-2 needs the exact `if:",
    );
    assert_refused(
        &in_job(
            &text,
            "promote-pkg",
            " && needs.promote-check.outputs.promote == 'true'",
            "",
        ),
        "release.yml: job promote-pkg needs the exact `if:",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-rpm-read-2",
            " && needs.testing-rpm-write.outputs.conflict == 'true'",
            "",
        ),
        "release.yml: job testing-rpm-read-2 needs the exact `if:",
    );
}

#[test]
fn a_job_in_the_wrong_environment_or_with_other_permissions_is_refused() {
    // Catches: the index job (the key) in the environment that holds the role, the read job in
    // none, the key's job given the token that mints the role's credentials, and a write scope.
    let text = release_yml();
    assert_refused(
        &in_job(
            &text,
            "testing-apt-index",
            "environment: apt-signing",
            "environment: apt-publish",
        ),
        "release.yml: job testing-apt-index is in the environment Some(\"apt-publish\"), not Some(\"apt-signing\")",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-apt-read",
            "    environment: apt-publish\n",
            "",
        ),
        "release.yml: job testing-apt-read is in the environment None",
    );
    let both = in_job(
        &text,
        "testing-apt-index",
        "      attestations: read\n",
        "      attestations: read\n      id-token: write\n",
    );
    assert_refused(
        &both,
        "release.yml: job testing-apt-index needs exactly the permissions",
    );
    assert_refused(
        &both,
        "job testing-apt-index: `id-token: write` is not allowed in release.yml",
    );
    assert_refused(
        &in_job(
            &text,
            "check-apt",
            "      contents: read\n",
            "      contents: write\n",
        ),
        "job check-apt: `contents: write` is not allowed in release.yml",
    );
}

#[test]
fn id_token_write_needs_the_jobs_exact_if() {
    // Catches: a job that assumes the role under a wider condition than its row's, such as one
    // that runs on a failed or cancelled run.
    let text = release_yml();
    let (start, end) = job_range(&text, "testing-apt-read-2");
    let job = &text[start..end];
    let wide = job.replacen(
        " && needs.testing-apt-write.outputs.conflict == 'true' }}",
        " }}",
        1,
    );
    let text = format!("{}{wide}{}", &text[..start], &text[end..]);
    assert_refused(
        &text,
        "job testing-apt-read-2: `id-token: write` needs the exact `if: ${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && needs.testing-apt-write.outputs.conflict == 'true' }}`",
    );
}

#[test]
fn a_job_that_runs_a_verb_of_another_row_is_refused() {
    // Catches: the job with the key assuming the role (so that one compromised step holds both),
    // a read job that also writes, and an index job that signs what the other index job signs.
    let text = release_yml();
    let assume = "      - run: cargo xtask channels assume --role-arn x --session-name hostkeeper-x --region us-east-1 --env-file \"$GITHUB_ENV\"\n";
    let step = |job: &str, run: &str| {
        let (start, end) = job_range(&text, job);
        let job_text = text[start..end].trim_end_matches('\n');
        format!("{}{job_text}\n{run}\n{}", &text[..start], &text[end..])
    };
    assert_refused(
        &step("testing-apt-index", assume.trim_end()),
        "release.yml: job testing-apt-index runs the `channels` verbs",
    );
    assert_refused(
        &step(
            "testing-apt-read",
            "      - run: cargo xtask channels apt write --suite testing --bucket b --distribution-id d --hostname h --next n --no-pool",
        ),
        "release.yml: job testing-apt-read runs the `channels` verbs",
    );
    assert_refused(
        &step(
            "testing-apt-index",
            "      - run: cargo xtask channels rpm sign --rpms r --out o --version 1.2.3 --sums s --work w --fingerprint f --key-file k --passphrase-file p --signed-at 1",
        ),
        "release.yml: job testing-apt-index runs the `channels` verbs",
    );
}

#[test]
fn channels_runs_in_no_job_outside_the_package_channels() {
    // Catches: the package tool run by the jobs that publish or verify the release, which hold
    // other rights than the package jobs' rows.
    let text = release_yml();
    let (start, end) = job_range(&text, "verify");
    let job = text[start..end].trim_end_matches('\n');
    let changed = format!(
        "{}{job}\n      - run: cargo xtask channels gate --label verify --setting A=b\n\n{}",
        &text[..start],
        &text[end..]
    );
    assert_refused(
        &changed,
        "release.yml: job verify builds (`cargo xtask channels gate",
    );
}

#[test]
fn only_the_signing_jobs_name_the_key_and_only_the_job_that_assumes_it_names_the_role() {
    // Catches: the key's secrets in a job that holds the role, a secret of another name in the
    // key's job, and the role's variable in the key's job.
    let text = release_yml();
    assert_refused(
        &in_job(
            &text,
            "testing-apt-read",
            "        with:\n          name: apt-current\n",
            "        env:\n          K: ${{ secrets.APT_GPG_PRIVATE_KEY }}\n        with:\n          name: apt-current\n",
        ),
        "release.yml: job testing-apt-read names the secret Some(\"APT_GPG_PRIVATE_KEY\")",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-apt-index",
            "          APT_GPG_PASSPHRASE: ${{ secrets.APT_GPG_PASSPHRASE }}\n        run: cargo xtask channels gate",
            "          APT_GPG_PASSPHRASE: ${{ secrets.OTHER }}\n        run: cargo xtask channels gate",
        ),
        "release.yml: job testing-apt-index names the secret Some(\"OTHER\")",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-apt-index",
            "          READ_ENABLED: ${{ needs.testing-apt-read.outputs.enabled }}\n",
            "          READ_ENABLED: ${{ needs.testing-apt-read.outputs.enabled }}\n          R: ${{ vars.APT_PUBLISH_ROLE_ARN }}\n",
        ),
        "release.yml: job testing-apt-index names APT_PUBLISH_ROLE_ARN",
    );
}

#[test]
fn every_step_after_the_gate_runs_under_the_gate() {
    // Catches: a step that writes whether or not the settings exist, a gate named for another
    // job, a gate that is not right after the build of xtask, and the rpm tree read before the
    // repository variable says it is deployed.
    let text = release_yml();
    let gated = "        if: ${{ steps.gate.outputs.enabled == 'true' }}\n        env:\n          APT_BUCKET: ${{ vars.APT_BUCKET }}\n        run: cargo xtask channels apt read --suite testing";
    assert_refused(
        &in_job(
            &text,
            "testing-apt-read",
            gated,
            &gated.replacen(
                "        if: ${{ steps.gate.outputs.enabled == 'true' }}\n",
                "",
                1,
            ),
        ),
        "release.yml: job testing-apt-read has step 6 that does not run under `if: ${{ steps.gate.outputs.enabled == 'true' }}`",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-apt-read",
            "--label testing-apt-read ",
            "--label testing-apt-write ",
        ),
        "release.yml: job testing-apt-read needs a gate that starts `cargo xtask channels gate --label testing-apt-read --output \"$GITHUB_OUTPUT\"`",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-apt-read",
            "      - name: Compile xtask\n        run: cargo xtask --help\n",
            "",
        ),
        "release.yml: job testing-apt-read must run its gate right after the checkout, the toolchain and the compile of xtask",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-rpm-read",
            " --require \"RPM_REPO_READY=$RPM_REPO_READY\"",
            "",
        ),
        "release.yml: job testing-rpm-read must be off while the repository variable RPM_REPO_READY is not `true`",
    );
}

#[test]
fn the_last_attempt_fails_on_a_lost_race_and_stable_writes_no_pool() {
    // Catches: a third attempt that asks for a fourth, a first attempt that fails on the race it
    // is built to retry, a stable write that uploads packages again, and a testing write that
    // does not upload them.
    let text = release_yml();
    assert_refused(
        &in_job(&text, "testing-apt-write-3", " --last", ""),
        "release.yml: job testing-apt-write-3 passes --last exactly in the third attempt",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-rpm-write",
            "--version \"${TAG#v}\" --output",
            "--version \"${TAG#v}\" --last --output",
        ),
        "release.yml: job testing-rpm-write passes --last exactly in the third attempt",
    );
    assert_refused(
        &in_job(&text, "stable-apt-write", " --no-pool", ""),
        "release.yml: job stable-apt-write passes --no-pool exactly when it writes the index of stable",
    );
    assert_refused(
        &in_job(&text, "stable-rpm-write-2", " --no-pool", ""),
        "release.yml: job stable-rpm-write-2 passes --no-pool exactly when it writes the index of stable",
    );
}

#[test]
fn a_job_names_only_the_suite_its_row_says() {
    // Catches: the stable write that writes testing, the testing index merged into stable, and a
    // read of stable in a testing job.
    let text = release_yml();
    assert_refused(
        &in_job(
            &text,
            "stable-apt-write",
            "--suite stable",
            "--suite testing",
        ),
        "release.yml: job stable-apt-write passes --suite [\"testing\"], not [\"stable\"]",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-apt-read",
            "--suite testing",
            "--suite stable",
        ),
        "release.yml: job testing-apt-read passes --suite [\"stable\"], not [\"testing\"]",
    );
}

#[test]
fn a_session_of_the_role_starts_with_the_products_prefix() {
    // Catches: the role assumed under a name that does not say whose release it is.
    let text = release_yml();
    assert_refused(
        &in_job(
            &text,
            "testing-apt-read",
            "--session-name \"hostkeeper-",
            "--session-name \"other-",
        ),
        "release.yml: job testing-apt-read assumes the role under the session \"other-testing-apt-read-$GITHUB_RUN_ID\"",
    );
}

#[test]
fn a_package_job_works_on_the_newest_release_of_the_run() {
    // Catches: a job that checks out another commit than the release it publishes, takes its
    // commit or tag from the plan (the oldest release's), or downloads another release's files.
    let text = release_yml();
    assert_refused(
        &in_job(
            &text,
            "testing-apt-index",
            "          ref: ${{ needs.newest.outputs.commit }}\n",
            "          ref: ${{ github.sha }}\n",
        ),
        "release.yml: job testing-apt-index needs a first step that checks out `ref: ${{ needs.newest.outputs.commit }}`",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-apt-index",
            "      COMMIT: ${{ needs.newest.outputs.commit }}",
            "      COMMIT: ${{ github.sha }}",
        ),
        "release.yml: job testing-apt-index needs `env: COMMIT: ${{ needs.newest.outputs.commit }}`",
    );
    assert_refused(
        &in_job(
            &text,
            "testing-apt-index",
            "          name: release-${{ needs.newest.outputs.tag }}\n",
            "          name: release-assets\n",
        ),
        "release.yml: job testing-apt-index downloads the artifact Some(\"release-assets\")",
    );
}

#[test]
fn the_newest_job_reads_the_last_entry_of_the_matrix_and_nothing_else() {
    // Catches: the first entry of the matrix (the oldest release), and a second step.
    let text = release_yml();
    assert!(text.contains(NEWEST_RUN));
    assert_refused(
        &in_job(&text, "newest", "'.include[-1] |", "'.include[0] |"),
        "release.yml: job newest needs exactly the one step",
    );
}

#[test]
fn docker_runs_only_in_the_install_checks_with_an_image_pinned_by_digest() {
    // Catches: docker in a job that publishes, an install check whose image moves, a container
    // with privilege or a mount, and a matrix image that is a tag.
    let text = release_yml();
    let mut found = problems(&text);
    found.retain(|p| p.contains("docker"));
    assert_eq!(found, Vec::<String>::new());
    assert_refused(
        &in_job(
            &text,
            "verify",
            "      - run: mkdir \"$RUNNER_TEMP/binary\"\n",
            "      - run: docker run --rm alpine true\n",
        ),
        "release.yml: job verify builds (`docker run --rm alpine true`)",
    );
    assert_refused(
        &in_job(&text, "check-apt", UBUNTU_IMAGE, "ubuntu:24.04"),
        "release.yml: job check-apt builds (`docker run --rm --volume",
    );
    assert_refused(
        &in_job(
            &text,
            "check-apt",
            "docker run --rm ",
            "docker run --rm --privileged ",
        ),
        "release.yml: job check-apt builds (`docker run --rm --privileged",
    );
    assert_refused(
        &in_job(&text, "check-apt", ":ro\"", "\" --volume /:/host"),
        "release.yml: job check-apt builds (`docker run",
    );
    assert_refused(
        &in_job(
            &text,
            "check-rpm",
            "          - fedora@sha256:",
            "          - fedora:41@@sha256:",
        ),
        "release.yml: job check-rpm: the image Some(\"fedora:41@@sha256:",
    );
    assert_refused(
        &in_job(
            &text,
            "check-rpm",
            "        image:\n          - fedora@sha256:a7d72ba2745b100a3351087472e5ee7b2450af7cf331e7673007e3817612839c\n",
            "        image:\n          - fedora\n",
        ),
        "is not pinned by digest",
    );
    assert_refused(
        &in_job(
            &text,
            "check-rpm",
            "IMAGE: ${{ matrix.image }}",
            "IMAGE: fedora:latest",
        ),
        "release.yml: job check-rpm must start its container from `IMAGE: ${{ matrix.image }}`",
    );
    assert!(docker_line_ok(
        "check-rpm",
        "docker run --detach --name check-rpm \"$IMAGE\" sleep infinity"
    ));
    assert!(!docker_line_ok(
        "check-rpm",
        "docker run --detach --name check-rpm fedora sleep infinity"
    ));
    assert!(!docker_line_ok(
        "verify",
        "docker run --detach --name check-rpm \"$IMAGE\" sleep infinity"
    ));
}

#[test]
fn the_install_check_command_inside_the_container_is_read_like_any_xtask_call() {
    // Catches: a check whose flags the binary would refuse, which a `docker run` line would hide
    // from the rule that reads every `cargo xtask` call.
    let words = |line: &str| commands(line).into_iter().next().expect("one command");
    let ok = format!(
        "docker run --rm --volume \"$PWD/target/debug/xtask:/usr/local/bin/xtask:ro\" {UBUNTU_IMAGE} xtask channels check-apt --package p --arch amd64 --version 1.2.3 --commit {}",
        "0".repeat(40)
    );
    let command = words(&ok);
    let args = xtask_args(&command).expect("the call inside the container");
    assert_eq!(args[..2], ["channels".to_owned(), "check-apt".to_owned()]);
    assert_eq!(call_problem(args), None);
    let bad = words(&ok.replace("--arch", "--archx"));
    let args = xtask_args(&bad).expect("the call inside the container");
    assert!(call_problem(args).is_some_and(|p| p.contains("--archx")));
    // Not a call: a docker command that names no image, or runs another program.
    assert_eq!(
        xtask_args(&words("docker run --rm alpine xtask channels gate")),
        None
    );
    assert_eq!(
        xtask_args(&words(&format!("docker run {UBUNTU_IMAGE} ls xtask"))),
        None
    );
    let text = release_yml();
    assert_refused(
        &in_job(&text, "check-apt", "--arch \"$ARCH\"", "--archx \"$ARCH\""),
        "`cargo xtask channels check-apt`",
    );
}

#[test]
fn promote_check_gives_decide_every_stage_from_its_own_job() {
    // Catches: a stage left out of the decision (a failed check then promotes), a stage given the
    // result of another job, and a stage the verb does not know.
    let text = release_yml();
    assert_eq!(STAGES.len(), 10);
    let result = " --result \"check-pkg=$CHECK_PKG\"";
    assert_refused(
        &in_job(&text, "promote-check", result, ""),
        "release.yml: job promote-check gives `channels decide` no `--result check-pkg=$CHECK_PKG`",
    );
    assert_refused(
        &in_job(
            &text,
            "promote-check",
            "CHECK_RPM: ${{ needs.check-rpm.result }}",
            "CHECK_RPM: ${{ needs.check-apt.result }}",
        ),
        "release.yml: job promote-check needs `CHECK_RPM: ${{ needs.check-rpm.result }}`",
    );
    assert_refused(
        &in_job(
            &text,
            "promote-check",
            result,
            &format!("{result} --result \"extra=$X\""),
        ),
        "release.yml: job promote-check gives `channels decide` 11 results, not the 10 stages",
    );
}
