//! The release rules ([`super::release`]) on the repository's own `release.yml` and
//! `artifacts.yml`: each file passes as it is, and each rule is shown to refuse its defect by
//! putting the defect into an otherwise clean copy of the file and finding the problem it
//! reports.

use super::lint::{WRITE_ALLOWED, lint};
use super::release::{
    PLAN_IF, RELEASE_EXEMPT, RELEASE_GROUP, RELEASE_IF, SIGN_IF, publish_problems,
};
use super::{CLEAN, push_only, with, with_in, workflows};

/// The text of the repository's workflow `name`.
fn workflow(name: &str) -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == name)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{name} is missing"))
}

/// Every problem the lint and the publish rule find in `text`, read as the file `file`.
fn all_problems(file: &str, text: &str) -> Vec<String> {
    let mut found = lint(file, text);
    found.extend(publish_problems(file, text));
    found
}

/// Fails unless the problems of `text` as `file` include a line holding `want`.
#[track_caller]
fn assert_refused(file: &str, text: &str, want: &str) {
    let found = all_problems(file, text);
    assert!(
        found.iter().any(|p| p.contains(want)),
        "no problem holds {want:?}; found {found:?} in:\n{text}"
    );
}

/// The repository's release.yml.
fn release_yml() -> String {
    workflow("release.yml")
}

/// The repository's artifacts.yml.
fn artifacts_yml() -> String {
    workflow("artifacts.yml")
}

#[test]
fn the_release_workflows_pass_the_release_rules() {
    // Catches: a rule that refuses the release workflows as they are (with their push,
    // completion and schedule triggers), so that every case below would fail for that reason
    // instead of the defect it adds.
    for file in ["release.yml", "artifacts.yml"] {
        let text = workflow(file);
        assert_eq!(
            all_problems(file, &text),
            Vec::<String>::new(),
            "{file}:\n{text}"
        );
    }
}

#[test]
fn release_workflow_is_refused_on_pull_request_trigger() {
    // Catches: the workflow that publishes running for a pull request, by `pull_request` or its
    // review events, and any other workflow that tags or publishes doing the same.
    for trigger in ["pull_request", "pull_request_review", "merge_group"] {
        let text = with_in(
            &release_yml(),
            "\non:\n  workflow_run:\n",
            &format!("\non:\n  {trigger}:\n  workflow_run:\n"),
        );
        assert_refused(
            "release.yml",
            &text,
            &format!("release.yml tags or publishes, so it may not run on {trigger}"),
        );
        assert_refused(
            "release.yml",
            &text,
            &format!("release.yml: trigger {trigger} is not one of"),
        );
    }
    let tags = with(
        "      - run: cargo test --workspace --locked\n",
        "      - run: git tag v1.2.3 && git push origin v1.2.3\n",
    );
    assert_refused(
        "tag.yml",
        &tags,
        "tag.yml tags or publishes, so it may not run on pull_request",
    );
    // The same workflow on pushes only is held to the release group, not refused for its trigger.
    let pushed = with_in(
        &push_only(),
        "      - run: cargo test --workspace --locked\n",
        "      - run: gh release create v1.2.3\n",
    );
    let found = all_problems("tag.yml", &pushed);
    assert!(
        found.iter().all(|p| !p.contains("may not run on")),
        "{found:?}"
    );
}

#[test]
fn artifacts_workflow_is_refused_on_pull_request_trigger() {
    // Catches: release files built (and a signing job planned) for every pull request, which
    // AGENTS.md B9 refuses, and a push trigger wider than main.
    let text = with_in(&artifacts_yml(), "  push:\n", "  pull_request:\n  push:\n");
    assert_refused(
        "artifacts.yml",
        &text,
        "artifacts.yml: trigger pull_request is not one of",
    );
    let tags = with_in(
        &artifacts_yml(),
        "    branches: [main]\n  workflow_dispatch:\n",
        "    branches: [main]\n    tags: ['v*']\n  workflow_dispatch:\n",
    );
    assert_refused(
        "artifacts.yml",
        &tags,
        "artifacts.yml: push must be `branches: [main]` and nothing else",
    );
}

#[test]
fn only_the_attest_job_holds_id_token_write() {
    // Catches: a second job (or a second workflow) that can sign, so that the signer identity a
    // consumer checks is no longer one job of artifacts.yml; and the attest job signing under a
    // wider condition than a push to main.
    let signers: Vec<(&str, &str)> = WRITE_ALLOWED
        .iter()
        .filter(|a| a.scope == "id-token" || a.scope == "attestations")
        .map(|a| (a.file, a.job))
        .collect();
    assert_eq!(
        signers,
        [("artifacts.yml", "attest"), ("artifacts.yml", "attest")]
    );
    let version_job = "  version:\n    runs-on: ubuntu-24.04\n    timeout-minutes: 15\n    permissions:\n      contents: read\n";
    let text = with_in(
        &workflow("artifacts.yml"),
        version_job,
        &format!("{version_job}      id-token: write\n"),
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "job version: `id-token: write` is not allowed in artifacts.yml",
    );
    let wider = with_in(
        &workflow("artifacts.yml"),
        &format!("    needs: [version, sums]\n    if: {SIGN_IF}\n"),
        "    needs: [version, sums]\n    if: ${{ github.ref == 'refs/heads/main' }}\n",
    );
    assert_refused(
        "artifacts.yml",
        &wider,
        &format!("job attest: `id-token: write` needs the exact `if: {SIGN_IF}`"),
    );
    assert_refused(
        "artifacts.yml",
        &wider,
        "artifacts.yml: job attest needs the exact `if:",
    );
    let in_release = with_in(
        &workflow("release.yml"),
        "      actions: read\n      attestations: read\n",
        "      actions: read\n      attestations: read\n      id-token: write\n",
    );
    assert_refused(
        "release.yml",
        &in_release,
        "job verify: `id-token: write` is not allowed in release.yml",
    );
}

#[test]
fn release_workflows_have_empty_top_level_permissions() {
    // Catches: a release workflow whose jobs start with the repository's default token rights,
    // which would let a job write without an entry of the table.
    for file in ["release.yml", "artifacts.yml"] {
        for top in [
            "permissions: write-all\n",
            "permissions:\n  contents: write\n",
            "",
        ] {
            let text = with_in(&workflow(file), "permissions: {}\n", top);
            assert_refused(file, &text, "no top-level `permissions: {}`");
        }
    }
}

#[test]
fn publish_job_alone_holds_contents_write() {
    // Catches: contents: write (the right to tag and publish) on the plan or verify job, or in
    // artifacts.yml; and publish holding it under a wider condition.
    let verify = "      actions: read\n      attestations: read\n";
    let text = with_in(
        &workflow("release.yml"),
        verify,
        &format!("{verify}      contents: write\n"),
    );
    assert_refused(
        "release.yml",
        &text,
        "job verify: `contents: write` is not allowed in release.yml",
    );
    let plan = "      actions: read\n      issues: write\n";
    let text = with_in(
        &workflow("release.yml"),
        plan,
        &format!("{plan}      contents: write\n"),
    );
    assert_refused(
        "release.yml",
        &text,
        "job plan: `contents: write` is not allowed in release.yml",
    );
    let attest = "      id-token: write\n";
    let text = with_in(
        &workflow("artifacts.yml"),
        attest,
        &format!("{attest}      contents: write\n"),
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "job attest: `contents: write` is not allowed in artifacts.yml",
    );
    let publish = format!("    needs: [plan, verify]\n    if: {RELEASE_IF}\n");
    let text = with_in(
        &workflow("release.yml"),
        &publish,
        "    needs: [plan, verify]\n    if: ${{ always() }}\n",
    );
    assert_refused(
        "release.yml",
        &text,
        &format!("job publish: `contents: write` needs the exact `if: {RELEASE_IF}`"),
    );
    assert_refused(
        "release.yml",
        &text,
        &format!("release.yml: job publish needs the exact `if: {RELEASE_IF}`"),
    );
    // The plan opens the issue of a held release under its own exact condition.
    let text = with_in(
        &workflow("release.yml"),
        &format!("    if: {PLAN_IF}\n"),
        &format!("    if: {}\n", PLAN_IF.replacen("&&", "||", 1)),
    );
    assert_refused(
        "release.yml",
        &text,
        &format!("job plan: `issues: write` needs the exact `if: {PLAN_IF}`"),
    );
}

#[test]
fn unpinned_action_is_refused_in_the_new_workflows() {
    // Catches: an action of a release workflow that a tag or branch move would change under us,
    // in the job that publishes and in the job that signs.
    let download = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1\n        with:\n          name: release-";
    let text = with_in(
        &workflow("release.yml"),
        download,
        "actions/download-artifact@v8\n        with:\n          name: release-",
    );
    assert_refused(
        "release.yml",
        &text,
        "`uses: actions/download-artifact@v8` is not pinned to a 40-digit lowercase commit SHA",
    );
    let provenance = "actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8 # v4.2.2\n        with:\n          subject-checksums";
    let text = with_in(
        &workflow("artifacts.yml"),
        provenance,
        "actions/attest-build-provenance@main\n        with:\n          subject-checksums",
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "`uses: actions/attest-build-provenance@main` is not pinned to a 40-digit lowercase commit SHA",
    );
}

#[test]
fn release_workflow_with_a_build_step_is_refused() {
    // Catches: release.yml building, packaging or signing what it publishes instead of
    // publishing the files artifacts.yml built and signed: a cargo build, a release verb that
    // builds, a packager or an archiver, and an attest action.
    let step = "      - name: Create the release and publish it\n";
    for (run, want) in [
        (
            "cargo build --locked --release",
            "job publish builds (`cargo build --locked --release`)",
        ),
        (
            "cargo xtask release package --platform linux-x86_64",
            "job publish builds (`cargo xtask release package --platform linux-x86_64`)",
        ),
        (
            "cargo xtask release set-version --version 0.1.1",
            "job publish builds (`cargo xtask release set-version --version 0.1.1`)",
        ),
        (
            "nfpm package --packager deb",
            "job publish builds (`nfpm package --packager deb`)",
        ),
        (
            "tar -czf dist/x.tar.gz hostkeeper",
            "job publish builds (`tar -czf dist/x.tar.gz hostkeeper`)",
        ),
        ("docker build .", "job publish builds (`docker build .`)"),
    ] {
        let text = with_in(
            &workflow("release.yml"),
            step,
            &format!("      - run: {run}\n{step}"),
        );
        assert_refused("release.yml", &text, &format!("release.yml: {want}"));
    }
    let attest = "      - uses: actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8 # v4.2.2\n        with:\n          subject-path: dist/SHA256SUMS\n";
    let text = with_in(&workflow("release.yml"), step, &format!("{attest}{step}"));
    assert_refused(
        "release.yml",
        &text,
        "release.yml: job publish signs (`actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8`)",
    );
}

#[test]
fn a_workflow_that_publishes_outside_the_release_group_is_refused() {
    // Catches: two runs that tag or publish at once, or a release cancelled half done: every
    // such workflow is in the one release group and never cancels; artifacts.yml is exempt only
    // with its own per-commit group.
    let step = "      - run: cargo test --workspace --locked\n";
    let publishes = with_in(
        &push_only(),
        step,
        "      - run: gh release create v1.2.3\n",
    );
    assert_refused(
        "tag.yml",
        &publishes,
        &format!("tag.yml tags or publishes, so its concurrency group must be {RELEASE_GROUP}"),
    );
    assert_refused(
        "tag.yml",
        &publishes,
        "tag.yml tags or publishes, so its concurrency must not cancel (cancel-in-progress: false)",
    );
    let grouped = with_in(
        &publishes,
        "permissions: {}\n",
        &format!(
            "permissions: {{}}\nconcurrency:\n  group: {RELEASE_GROUP}\n  cancel-in-progress: false\n"
        ),
    );
    assert_eq!(publish_problems("tag.yml", &grouped), Vec::<String>::new());
    let cancels = with_in(
        &workflow("release.yml"),
        "  cancel-in-progress: false\n",
        "  cancel-in-progress: true\n",
    );
    assert_refused(
        "release.yml",
        &cancels,
        "release.yml tags or publishes, so its concurrency must not cancel",
    );
    // A workflow that neither tags nor publishes is not held to the group, and a comment that
    // names `gh release` does not make it one that does.
    let commented = with(
        step,
        &format!("      # gh release and contents: write are named here only\n{step}"),
    );
    assert_eq!(publish_problems("ci.yml", &commented), Vec::<String>::new());
    assert_eq!(publish_problems("ci.yml", CLEAN), Vec::<String>::new());
    let shared = with_in(
        &workflow("artifacts.yml"),
        "  group: artifacts-${{ github.sha }}\n",
        "  group: artifacts\n",
    );
    assert_refused(
        "artifacts.yml",
        &shared,
        "artifacts.yml: concurrency must be group artifacts-${{ github.sha }}, cancel-in-progress false",
    );
    for (file, reason) in RELEASE_EXEMPT {
        assert!(
            !reason.trim().is_empty(),
            "{file} is exempt without a reason"
        );
        assert!(
            workflows().iter().any(|(name, _)| name == file),
            "{file} is exempt but missing"
        );
    }
}

#[test]
fn release_workflow_run_names_exactly_the_three_required_workflows() {
    // Catches: a release that no longer waits for one of ci, privileged and artifacts (a red
    // privileged run would no longer hold it), or that starts for a run on another branch.
    let text = with_in(
        &release_yml(),
        "workflows: [ci, privileged, artifacts]",
        "workflows: [ci, artifacts]",
    );
    assert_refused(
        "release.yml",
        &text,
        "release.yml: workflow_run.workflows is [\"artifacts\", \"ci\"], not [\"artifacts\", \"ci\", \"privileged\"]",
    );
    let text = with_in(
        &release_yml(),
        "    branches: [main]\n",
        "    branches: [main, 'release/*']\n",
    );
    assert_refused(
        "release.yml",
        &text,
        "release.yml: workflow_run.branches is",
    );
    let text = with_in(
        &release_yml(),
        "  schedule:\n",
        "  push:\n    branches: [main]\n  schedule:\n",
    );
    assert_refused(
        "release.yml",
        &text,
        "release.yml: trigger push is not one of",
    );
}

#[test]
fn release_workflow_keeps_its_three_jobs_and_checks_the_binary() {
    // Catches: a job added beside plan, verify and publish (which could write or build without a
    // rule about it), publish no longer waiting for verify, and verify no longer running the
    // binary it is about to release.
    let text = with_in(
        &workflow("release.yml"),
        "    needs: [plan, verify]\n",
        "    needs: [plan]\n",
    );
    assert_refused(
        "release.yml",
        &text,
        "release.yml: job publish needs [\"plan\"], not [\"plan\", \"verify\"]",
    );
    let text = format!(
        "{}  notify:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo done\n",
        workflow("release.yml")
    );
    assert_refused(
        "release.yml",
        &text,
        "release.yml: jobs are [\"notify\", \"plan\", \"publish\", \"verify\"], not plan, verify and publish",
    );
    for program in ["hostkeeper", "hostkeeper-coordinator"] {
        let check = format!(
            "      - name: {program} says the version and the commit it is released under\n        run: cargo xtask release check-binary \"$RUNNER_TEMP/binary/{program}\" \"$RELEASE_VERSION\" \"$COMMIT\"\n"
        );
        let text = with_in(&workflow("release.yml"), &check, "");
        let found = all_problems("release.yml", &text);
        assert_eq!(
            found,
            vec![format!(
                "release.yml: job verify runs no `cargo xtask release check-binary` of {program}"
            )],
            "without the check of {program}"
        );
    }
}

#[test]
fn the_image_job_builds_twice_from_scratch_and_compares() {
    // Catches: an image released from one build (nothing shows it can be rebuilt), a second
    // build that reuses the first one's cache, or two builds never compared.
    let build_b = "      - name: The second build\n        run: docker buildx build --builder image-b --no-cache ";
    let compare = "        run: cargo xtask release compare-images ";
    let want = "artifacts.yml: job image must build the image twice";
    for (from, to) in [
        (
            build_b,
            "      - name: The second build\n        run: docker build --no-cache ",
        ),
        (
            build_b,
            "      - name: The second build\n        run: docker buildx build --builder image-b ",
        ),
        (compare, "        run: cmp "),
    ] {
        let text = with_in(&workflow("artifacts.yml"), from, to);
        assert_refused("artifacts.yml", &text, want);
    }
}

#[test]
fn attest_job_runs_no_repository_code() {
    // Catches: code of the repository (which a pull request can change) running while a token
    // can sign: a checkout, a cargo command, or a bundle copy that runs a second command.
    let step = "      - name: Refuse sums other than the build's\n";
    let checkout = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          persist-credentials: false\n";
    let text = with_in(
        &workflow("artifacts.yml"),
        step,
        &format!("{checkout}{step}"),
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "artifacts.yml: job attest checks out the repository",
    );
    let text = with_in(
        &workflow("artifacts.yml"),
        step,
        &format!("      - run: cargo xtask --help\n{step}"),
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "artifacts.yml: job attest runs `cargo xtask --help`",
    );
    let copy = "cp \"$BUNDLE\" \"dist/hostkeeper-${V}-linux-x86_64.tar.gz.sigstore.json\"";
    for bad in [
        "cp \"$BUNDLE\" \"dist/x.sigstore.json\"; curl -d @dist .sigstore.json\"",
        "cp \"$BUNDLE\" \"dist/$(id).sigstore.json\"",
        "cp \"$BUNDLE\" \"dist/.sigstore.json\"",
        "cp \"$BUNDLE\" \"dist/x.json\"",
    ] {
        let text = with_in(&workflow("artifacts.yml"), copy, bad);
        assert_refused(
            "artifacts.yml",
            &text,
            &format!("artifacts.yml: job attest runs `{bad}`"),
        );
    }
}

#[test]
fn artifacts_version_job_counts_on_a_whole_history() {
    // Catches: the version computed on a shallow clone, or from another commit or version file
    // than the one built.
    let whole = "          fetch-depth: 0\n          persist-credentials: false\n      - run: rustup toolchain install\n      - name: The version of this commit\n";
    let text = with_in(
        &workflow("artifacts.yml"),
        whole,
        &whole.replacen("          fetch-depth: 0\n", "", 1),
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "artifacts.yml: job version must check out the whole history",
    );
    let text = with_in(
        &workflow("artifacts.yml"),
        "--commit \"$GITHUB_SHA\" --version-file VERSION --output",
        "--commit \"$COMMIT\" --version-file VERSION --output",
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "artifacts.yml: job version must check out the whole history",
    );
}

#[test]
fn the_verify_doc_names_the_identity_the_attest_job_signs_with() {
    // Catches: the consumer document and the signing workflow drifting apart on the repository,
    // the signer workflow, the ref or the commit check.
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/deploy/verify-a-release.md"
    ))
    .expect("docs/deploy/verify-a-release.md");
    for flag in [
        "--repo komira-ai/hostkeeper",
        "--signer-workflow komira-ai/hostkeeper/.github/workflows/artifacts.yml",
        "--source-ref refs/heads/main",
        "--source-digest \"$PIN\"",
        "--deny-self-hosted-runners",
        "sha256sum --check --strict",
    ] {
        assert!(doc.contains(flag), "the document lacks {flag}");
    }
    assert!(workflow("artifacts.yml").contains(&format!("    if: {SIGN_IF}\n")));
    assert!(SIGN_IF.contains("refs/heads/main"));
}
