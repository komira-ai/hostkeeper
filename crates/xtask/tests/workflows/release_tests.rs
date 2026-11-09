//! The release rules ([`super::release`]) on the repository's own `release.yml` and
//! `artifacts.yml`: each file passes as it is, and each rule is shown to refuse its defect by
//! putting the defect into an otherwise clean copy of the file and finding the problem it
//! reports.

use super::lint::{WRITE_ALLOWED, lint};
use super::release::{
    PLAN_IF, PLAN_RUN, PUBLISH_GROUP, RELEASE_EXEMPT, RELEASE_GROUP, RELEASE_IF, SIGN_IF,
    publish_problems,
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
            // The top-level line: no job's `permissions: {}` starts a line.
            let text = with_in(&workflow(file), "\npermissions: {}\n", &format!("\n{top}"));
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
    // The publish job's download (the image job downloads the same artifact).
    let download = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1\n        with:\n          name: release-${{ matrix.tag }}\n          path: dist\n      # The same checks as verify";
    let text = with_in(
        &workflow("release.yml"),
        download,
        "actions/download-artifact@v8\n        with:\n          name: release-${{ matrix.tag }}\n          path: dist\n      # The same checks as verify",
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
        "\n  cancel-in-progress: false\n",
        "\n  cancel-in-progress: true\n",
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

/// The plan step of the repository's release.yml, whole.
const PLAN_STEP: &str = "      - name: Which commits since the last release are released, oldest first\n        id: walk\n        env:\n          GH_TOKEN: ${{ github.token }}\n          FREEZE: ${{ vars.RELEASE_FREEZE }}\n";

#[test]
fn release_workflow_plans_with_one_step_for_every_trigger() {
    // Catches: a plan that differs by trigger again (a `workflow_run` that plans only the commit
    // whose run completed, so that a busy main never releases): the step skipped for a trigger,
    // a second plan step beside it, other arguments, or a checkout of another commit than the
    // tip of main.
    let text = release_yml();
    assert!(
        text.contains(&format!("{PLAN_STEP}        run: {PLAN_RUN}\n")),
        "{text}"
    );
    let one_step = "release.yml: job plan needs exactly one plan step";
    let skipped = with_in(
        &text,
        "        id: walk\n",
        "        id: walk\n        if: ${{ github.event_name != 'workflow_run' }}\n",
    );
    assert_refused(
        "release.yml",
        &skipped,
        "release.yml: the plan step must run for every trigger, under no `if:`",
    );
    let second = with_in(
        &text,
        PLAN_STEP,
        &format!(
            "      - id: push\n        if: ${{{{ github.event_name == 'workflow_run' }}}}\n        env:\n          COMMIT: ${{{{ github.event.workflow_run.head_sha }}}}\n        run: cargo xtask release plan --repo \"$GITHUB_REPOSITORY\" --commit \"$COMMIT\" --output \"$GITHUB_OUTPUT\"\n{PLAN_STEP}"
        ),
    );
    assert_refused("release.yml", &second, one_step);
    for (from, to) in [
        (" --output", " --scheduled --output"),
        ("--commit \"$GITHUB_SHA\"", "--commit \"$COMMIT\""),
    ] {
        let changed = with_in(
            &text,
            &format!("{PLAN_RUN}\n"),
            &format!("{}\n", PLAN_RUN.replacen(from, to, 1)),
        );
        assert_refused("release.yml", &changed, one_step);
    }
    let none = with_in(
        &text,
        &format!("        run: {PLAN_RUN}\n"),
        "        run: echo none\n",
    );
    assert_refused("release.yml", &none, one_step);
    let elsewhere = with_in(
        &text,
        "        with:\n          fetch-depth: 0\n",
        "        with:\n          ref: ${{ github.event.workflow_run.head_sha }}\n          fetch-depth: 0\n",
    );
    assert_refused(
        "release.yml",
        &elsewhere,
        "release.yml: job plan must check out the tip of main (no `ref:`)",
    );
}

#[test]
fn release_workflow_publishes_one_tag_at_a_time_across_its_groups() {
    // Catches: a dispatch or schedule run that a completion of a push run replaces while it
    // waits (both in one group), or the two groups of main publishing one tag at once, which
    // creates two drafts of it: the job-level group of publish removed, cancelling, or not
    // keyed by the tag.
    let text = release_yml();
    let block = format!(
        "    concurrency:\n      group: {PUBLISH_GROUP}\n      cancel-in-progress: false\n"
    );
    let want = format!(
        "release.yml: job publish needs `concurrency:` group {PUBLISH_GROUP}, cancel-in-progress false"
    );
    for to in [
        String::new(),
        block.replace("cancel-in-progress: false", "cancel-in-progress: true"),
        block.replace(PUBLISH_GROUP, "release-publish"),
        block.replace("      cancel-in-progress: false\n", ""),
    ] {
        let changed = with_in(&text, &block, &to);
        assert_refused("release.yml", &changed, &want);
    }
    // The lint holds the whole expression: either group of main renamed is refused.
    for joined in ["&& 'main-dispatch' ||", "&& 'main' ||"] {
        assert_eq!(RELEASE_GROUP.matches(joined).count(), 1, "{joined}");
        let one = RELEASE_GROUP.replacen(joined, "&& 'main-shared' ||", 1);
        let changed = with_in(&text, RELEASE_GROUP, &one);
        assert_refused(
            "release.yml",
            &changed,
            &format!(
                "release.yml tags or publishes, so its concurrency group must be {RELEASE_GROUP}"
            ),
        );
    }
}

#[test]
fn release_workflow_keeps_its_five_jobs_and_checks_the_binary() {
    // Catches: a job added beside plan, verify, publish and the two deploy jobs (which could
    // write or build without a rule about it), publish no longer waiting for verify, and verify
    // no longer running the binary it is about to release.
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
        "release.yml: jobs are [\"channel\", \"image\", \"notify\", \"plan\", \"publish\", \"set-desired\", \"verify\"], not plan, verify, publish, image, channel and set-desired",
    );
    for program in ["hostkeeper", "hostkeeper-coordinator", "hostkeeper-mdm"] {
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
    // build that reuses the first one's cache, two builds never compared, or an image never
    // started through its entrypoint (or compared with another stamp than the coordinator's).
    let build_b = "      - name: The second build\n        run: docker buildx build --builder image-b --no-cache ";
    let compare = "        run: cargo xtask release compare-images ";
    let entrypoint = "test \"$(docker run --rm ";
    for (from, to, want) in [
        (
            entrypoint,
            "true \"$(docker run --rm ",
            "artifacts.yml: job image must run the image through its entrypoint",
        ),
        (
            "--version)\" = \"hostkeeper-coordinator $RELEASE_VERSION+${GITHUB_SHA:0:12}\"",
            "--version)\" = \"hostkeeper $RELEASE_VERSION+${GITHUB_SHA:0:12}\"",
            "artifacts.yml: job image must run the image through its entrypoint",
        ),
    ] {
        let text = with_in(&workflow("artifacts.yml"), from, to);
        assert_refused("artifacts.yml", &text, want);
    }
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
fn the_image_job_checks_that_the_image_holds_the_web_app() {
    // Catches: an image released without the operator web app, or checked against another
    // image or another directory than the commit's `ui/` (a deploy of it would answer 404 at
    // /ui/): the check removed, run on the second build's context, or on another directory.
    let check = "        run: cargo xtask release check-image-ui \"$RUNNER_TEMP/image-a.tar\" ui\n";
    let want = "artifacts.yml: job image must check that the image holds the web app";
    for to in [
        "        run: true\n",
        "        run: cargo xtask release check-image-ui \"$RUNNER_TEMP/context-a\" ui\n",
        "        run: cargo xtask release check-image-ui \"$RUNNER_TEMP/image-a.tar\" examples\n",
    ] {
        let text = with_in(&workflow("artifacts.yml"), check, to);
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

/// The two steps that attest the package `file` (a name with `${V}` for the version) and copy its
/// bundle: `id` is the id of the first, `name` the name of the second.
fn attest_steps(id: &str, name: &str, file: &str, subject: &str) -> String {
    format!(
        "      - id: {id}\n        uses: actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8 # v4.2.2\n        with:\n          subject-path: dist/{subject}\n      - name: {name}\n        env:\n          BUNDLE: ${{{{ steps.{id}.outputs.bundle-path }}}}\n        run: cp \"$BUNDLE\" \"dist/{file}.sigstore.json\"\n"
    )
}

#[test]
fn attest_job_attests_every_package_for_every_platform_and_copies_each_bundle() {
    // Catches: a release asset list that names the bundle of a hello package (or of its failing
    // build, or of netboot.json) while artifacts.yml has no step that signs it, one that signs it
    // without copying the bundle beside it, or one that copies the bundle of another step. A host trusts only
    // attested packages, so each of these would ship a package no host can install.
    let hello_x86 = attest_steps(
        "hello-linux-x86_64",
        "Name the bundle of the hello package for linux-x86_64",
        "hello-${V}-linux-x86_64.tar.zst",
        "hello-${{ needs.version.outputs.version }}-linux-x86_64.tar.zst",
    );
    let failing_arm = attest_steps(
        "hello-fail-ready-linux-arm64",
        "Name the bundle of the failing build of hello for linux-arm64",
        "hello-${V}-fail-ready-linux-arm64.tar.zst",
        "hello-${{ needs.version.outputs.version }}-fail-ready-linux-arm64.tar.zst",
    );
    let text = workflow("artifacts.yml");
    assert!(
        text.contains(&hello_x86),
        "the helper writes the steps as the file has them"
    );
    assert!(text.contains(&failing_arm));

    let text = with_in(&text, &hello_x86, "");
    assert_refused(
        "artifacts.yml",
        &text,
        "artifacts.yml: job attest does not attest hello-<version>-linux-x86_64.tar.zst",
    );
    let text = with_in(&workflow("artifacts.yml"), &failing_arm, "");
    assert_refused(
        "artifacts.yml",
        &text,
        "artifacts.yml: job attest does not attest hello-<version>-fail-ready-linux-arm64.tar.zst",
    );
    // Signed but never copied beside the package.
    let copy = "        run: cp \"$BUNDLE\" \"dist/hello-${V}-fail-ready-linux-arm64.tar.zst.sigstore.json\"\n";
    let text = with_in(&workflow("artifacts.yml"), copy, "        run: echo\n");
    let found = all_problems("artifacts.yml", &text);
    assert!(
        found.iter().any(|p| p.contains(
            "job attest does not copy the bundle of hello-<version>-fail-ready-linux-arm64.tar.zst"
        )),
        "{found:?}"
    );
    // A copy of the bundle of another step.
    let text = with_in(
        &workflow("artifacts.yml"),
        "steps.hello-linux-x86_64.outputs.bundle-path",
        "steps.package-linux-x86_64.outputs.bundle-path",
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "job attest does not copy the bundle of hello-<version>-linux-x86_64.tar.zst from the step hello-linux-x86_64",
    );
    // The macOS packages of hello are attested and their bundles copied like the others.
    let hello_mac = attest_steps(
        "hello-macos-arm64",
        "Name the bundle of the hello package for macos-arm64",
        "hello-${V}-macos-arm64.tar.zst",
        "hello-${{ needs.version.outputs.version }}-macos-arm64.tar.zst",
    );
    let text = workflow("artifacts.yml");
    assert!(text.contains(&hello_mac));
    assert_refused(
        "artifacts.yml",
        &with_in(&text, &hello_mac, ""),
        "artifacts.yml: job attest does not attest hello-<version>-macos-arm64.tar.zst",
    );
    let copy = "        run: cp \"$BUNDLE\" \"dist/hello-${V}-fail-ready-macos-arm64.tar.zst.sigstore.json\"\n";
    let text = with_in(&text, copy, "        run: echo\n");
    let found = all_problems("artifacts.yml", &text);
    assert!(
        found.iter().any(|p| p.contains(
            "job attest does not copy the bundle of hello-<version>-fail-ready-macos-arm64.tar.zst"
        )),
        "{found:?}"
    );
    // The boot sets of network install: attested, and their bundle copied from that step.
    let netboot = attest_steps(
        "netboot",
        "Name the bundle of the boot sets of network install",
        "netboot.json",
        "netboot.json",
    );
    let text = workflow("artifacts.yml");
    assert!(text.contains(&netboot));
    assert_refused(
        "artifacts.yml",
        &with_in(&text, &netboot, ""),
        "artifacts.yml: job attest does not attest netboot.json (a provenance step with an id and the subject-path dist/netboot.json)",
    );
    assert_refused(
        "artifacts.yml",
        &with_in(
            &text,
            "steps.netboot.outputs.bundle-path",
            "steps.pkg-macos-arm64.outputs.bundle-path",
        ),
        "artifacts.yml: job attest does not copy the bundle of netboot.json from the step netboot to dist/netboot.json.sigstore.json",
    );
    // A package attested under the name of another file.
    let text = with_in(
        &workflow("artifacts.yml"),
        "subject-path: dist/hello-${{ needs.version.outputs.version }}-linux-arm64.tar.zst",
        "subject-path: dist/hello-${{ needs.version.outputs.version }}-linux-amd64.tar.zst",
    );
    assert_refused(
        "artifacts.yml",
        &text,
        "job attest does not attest hello-<version>-linux-arm64.tar.zst",
    );
}

#[test]
fn attest_job_attests_the_command_line_and_copies_its_bundle() {
    // Catches: a release asset list that names the bundle of `hkctl-<version>` while
    // artifacts.yml has no step that signs it, one that signs another file under its name, or
    // one that copies the bundle of another step beside it.
    let hkctl = attest_steps(
        "hkctl",
        "Name the bundle of the command line",
        "hkctl-${V}",
        "hkctl-${{ needs.version.outputs.version }}",
    );
    let text = workflow("artifacts.yml");
    assert!(
        text.contains(&hkctl),
        "the helper writes the steps as the file has them"
    );
    assert_refused(
        "artifacts.yml",
        &with_in(&text, &hkctl, ""),
        "artifacts.yml: job attest does not attest hkctl-<version> (a provenance step with an id and the subject-path dist/hkctl-${{ needs.version.outputs.version }})",
    );
    assert_refused(
        "artifacts.yml",
        &with_in(
            &text,
            "subject-path: dist/hkctl-${{ needs.version.outputs.version }}",
            "subject-path: dist/hkctl",
        ),
        "artifacts.yml: job attest does not attest hkctl-<version>",
    );
    assert_refused(
        "artifacts.yml",
        &with_in(
            &text,
            "steps.hkctl.outputs.bundle-path",
            "steps.netboot.outputs.bundle-path",
        ),
        "artifacts.yml: job attest does not copy the bundle of hkctl-<version> from the step hkctl to dist/hkctl-${V}.sigstore.json",
    );
}

#[test]
fn attest_job_attests_every_binary_tarball_with_its_bundle_and_its_sbom() {
    // Catches: a binary tarball (hostkeeper-mdm's in particular, on either Linux platform) that
    // the release lists while the attest job does not sign it, signs it without copying its
    // bundle beside it, copies the bundle of another step, or attests no SBOM for it (or the SBOM
    // of another file). A consumer verifies the tarball by that bundle and that identity.
    for platform in ["linux-x86_64", "linux-arm64"] {
        let steps = attest_steps(
            &format!("mdm-{platform}"),
            &format!("Name the bundle of the tarball of hostkeeper-mdm for {platform}"),
            &format!("hostkeeper-mdm-${{V}}-{platform}.tar.gz"),
            &format!("hostkeeper-mdm-${{{{ needs.version.outputs.version }}}}-{platform}.tar.gz"),
        );
        let text = workflow("artifacts.yml");
        assert!(
            text.contains(&steps),
            "the helper writes the steps as the file has them"
        );
        let found = all_problems("artifacts.yml", &with_in(&text, &steps, ""));
        assert_eq!(
            found,
            vec![format!(
                "artifacts.yml: job attest does not attest hostkeeper-mdm-<version>-{platform}.tar.gz (a provenance step with an id and the subject-path dist/hostkeeper-mdm-${{{{ needs.version.outputs.version }}}}-{platform}.tar.gz)"
            )],
            "{platform}"
        );
        let text = with_in(
            &workflow("artifacts.yml"),
            &format!("steps.mdm-{platform}.outputs.bundle-path"),
            &format!("steps.coordinator-{platform}.outputs.bundle-path"),
        );
        assert_refused(
            "artifacts.yml",
            &text,
            &format!(
                "artifacts.yml: job attest does not copy the bundle of hostkeeper-mdm-<version>-{platform}.tar.gz from the step mdm-{platform} to dist/hostkeeper-mdm-${{V}}-{platform}.tar.gz.sigstore.json"
            ),
        );
        let sbom = format!(
            "          sbom-path: dist/hostkeeper-mdm-${{{{ needs.version.outputs.version }}}}-{platform}.cdx.json\n"
        );
        let other = format!(
            "          sbom-path: dist/hostkeeper-coordinator-${{{{ needs.version.outputs.version }}}}-{platform}.cdx.json\n"
        );
        let text = with_in(&workflow("artifacts.yml"), &sbom, &other);
        assert_refused(
            "artifacts.yml",
            &text,
            &format!(
                "artifacts.yml: job attest does not attest the SBOM of hostkeeper-mdm-<version>-{platform}.tar.gz"
            ),
        );
    }
    // The other binary tarballs are held to the same rule: hostkeeper's on macOS, the
    // coordinator's on linux-arm64.
    for (from, file) in [
        (
            "subject-path: dist/hostkeeper-${{ needs.version.outputs.version }}-macos-arm64.tar.gz\n          sbom-path",
            "hostkeeper-<version>-macos-arm64.tar.gz",
        ),
        (
            "subject-path: dist/hostkeeper-coordinator-${{ needs.version.outputs.version }}-linux-arm64.tar.gz\n          sbom-path",
            "hostkeeper-coordinator-<version>-linux-arm64.tar.gz",
        ),
    ] {
        let text = with_in(
            &workflow("artifacts.yml"),
            from,
            "subject-path: dist/other.tar.gz\n          sbom-path",
        );
        assert_refused(
            "artifacts.yml",
            &text,
            &format!("artifacts.yml: job attest does not attest the SBOM of {file}"),
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
