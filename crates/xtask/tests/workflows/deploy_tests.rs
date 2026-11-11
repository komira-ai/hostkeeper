//! The deploy of the coordinator to its Docker Swarm service in `release.yml`
//! ([`super::deploy`]): the repository's file passes, each rule is shown to refuse its defect by
//! putting the defect into the repository's `release.yml` and reading the problems the lint
//! reports, the `if:` of the jobs that depend on the deploy is evaluated against each outcome of
//! it ([`super::expr`]), and the step of `deploy-digest` that reads the image's digest is run, with
//! the runner's own `jq`, on image indexes of each shape.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use super::deploy::{
    COMMIT, DEPLOY_IF, DIGEST_JOB, DIGEST_STEPS, GATE, JOB, RELEASE, WITH, WORKFLOW,
};
use super::desired::{SET_DESIRED_IF, SET_IF, job};
use super::expr::{Context, holds};
use super::lint::{ID_TOKEN_ALLOWED, WRITE_ALLOWED, entries, entry, lint};
use super::release::publish_problems;
use super::testdir::TestDir;
use super::{with_in, workflows, yaml};

/// The repository's release.yml.
fn release_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml exists")
}

/// The text of the job `name` of release.yml, from its line to the blank line that ends
/// it, its last line with its newline.
fn job_text(name: &str) -> String {
    let text = release_yml();
    let at = text
        .find(&format!("\n  {name}:\n"))
        .unwrap_or_else(|| panic!("release.yml has a job {name}"));
    let rest = &text[at + 1..];
    let end = at + 1 + rest.find("\n\n").expect("a blank line ends the job") + 1;
    text[at..end].to_owned()
}

/// Every problem the lint finds in `text` as release.yml.
fn all_problems(text: &str) -> Vec<String> {
    let mut found = lint("release.yml", text);
    found.extend(publish_problems("release.yml", text));
    found
}

/// Every problem the lint finds in release.yml with the one occurrence of `from` in the job
/// `name` replaced by `to`.
fn changed(name: &str, from: &str, to: &str) -> Vec<String> {
    let text = release_yml();
    let job = job_text(name);
    all_problems(&text.replacen(&job, &with_in(&job, from, to), 1))
}

/// Fails unless some problem of `found` holds `want`.
#[track_caller]
fn assert_has(found: &[String], want: &str) {
    assert!(
        found.iter().any(|p| p.contains(want)),
        "no problem holds {want:?}; found {found:?}"
    );
}

/// The `if:` line of both jobs.
fn if_line() -> String {
    format!("    if: {DEPLOY_IF}\n")
}

/// The `uses:` line of the job deploy.
fn uses_line() -> String {
    format!("    uses: {WORKFLOW}@{COMMIT} # v{RELEASE}\n")
}

#[test]
fn the_deploy_jobs_of_the_repository_pass() {
    // Catches: a rule that refuses release.yml as it is, so that every case below would fail for
    // that reason instead of the defect it adds; and a job that drifted from the constants the
    // rules and the docs name.
    assert_eq!(all_problems(&release_yml()), Vec::<String>::new());
    assert_eq!(job_text(DIGEST_JOB).matches(&if_line()).count(), 1);
    assert_eq!(job_text(JOB).matches(&if_line()).count(), 1);
    assert_eq!(job_text(JOB).matches(&uses_line()).count(), 1);
    assert_eq!(WITH.len(), 11);
    assert_eq!(DIGEST_STEPS.len(), 3);
}

#[test]
fn the_deploy_runs_only_for_the_newest_release_of_main_and_only_while_the_endpoint_is_set() {
    // Catches: a deploy that cannot be turned off (it runs with the variable empty), one that
    // runs for a release older than the newest published (a late run replacing a newer
    // release), or one that runs off main.
    assert!(DEPLOY_IF.contains(&format!("vars.{GATE} != ''")));
    let on = BTreeMap::from([
        ("needs.plan.outputs.release".to_owned(), "true".to_owned()),
        ("github.ref".to_owned(), "refs/heads/main".to_owned()),
        ("needs.targets.outputs.newest".to_owned(), "true".to_owned()),
        (format!("vars.{GATE}"), "https://front.example".to_owned()),
    ]);
    let runs = |change: &[(&str, &str)]| {
        let mut names = on.clone();
        for (k, v) in change {
            names.insert((*k).to_owned(), (*v).to_owned());
        }
        let context = Context {
            names,
            cancelled: false,
            failed: false,
        };
        holds(DEPLOY_IF, &context).expect("the test reads the condition")
    };
    assert!(runs(&[]));
    assert!(!runs(&[("vars.CP_DEPLOY_ENDPOINT", "")]));
    assert!(!runs(&[("needs.targets.outputs.newest", "false")]));
    assert!(!runs(&[("needs.targets.outputs.newest", "")]));
    assert!(!runs(&[("needs.plan.outputs.release", "false")]));
    assert!(!runs(&[("github.ref", "refs/heads/other")]));
}

#[test]
fn a_deploy_job_that_runs_with_the_endpoint_empty_or_off_main_is_refused() {
    // Catches: either job with its `if:` dropped or widened, so that it runs with the variable
    // empty, for an older release, or from another branch.
    let wider = "    if: ${{ needs.plan.outputs.release == 'true' }}\n";
    for name in [DIGEST_JOB, JOB] {
        for to in [wider, ""] {
            let found = changed(name, &if_line(), to);
            assert_has(
                &found,
                &format!("release.yml: job {name} needs the exact `if: {DEPLOY_IF}`"),
            );
        }
    }
    let found = changed(JOB, &if_line(), wider);
    assert_has(
        &found,
        &format!("job deploy: `id-token: write` needs the exact `if: {DEPLOY_IF}`"),
    );
}

#[test]
fn a_job_that_goes_on_after_a_failure_is_refused() {
    // Catches: a failed digest read or deploy that no longer stops the wait for the coordinator
    // and the set of the hosts' desired version.
    for (name, after) in [
        (DIGEST_JOB, "    timeout-minutes: 10\n"),
        (JOB, "    permissions:\n      id-token: write\n"),
    ] {
        let found = changed(
            name,
            after,
            &format!("{after}    continue-on-error: true\n"),
        );
        assert_has(
            &found,
            &format!(
                "release.yml: job {name} has `continue-on-error`: a failure must stop the deploy"
            ),
        );
    }
}

#[test]
fn the_coordinator_wait_runs_as_before_when_the_deploy_is_skipped_and_not_after_it_fails() {
    // Catches: the wait skipped because the deploy, which it needs, was skipped while the
    // endpoint variable is empty (the release would never be set); the wait run after a failed
    // deploy (the coordinator may still be the old release, or half replaced); a changed
    // condition where the deploy is off, which would change a release that does not use it.
    let before = "${{ needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && vars.RELEASE_SETS_DESIRED == 'true' && needs.targets.outputs.newest == 'true' }}";
    assert_eq!(
        SET_IF.replace("!cancelled() && !failure() && ", ""),
        before,
        "the condition is the one before the deploy, and the two status functions"
    );
    let on = BTreeMap::from([
        ("needs.plan.outputs.release".to_owned(), "true".to_owned()),
        ("github.ref".to_owned(), "refs/heads/main".to_owned()),
        ("vars.RELEASE_SETS_DESIRED".to_owned(), "true".to_owned()),
        ("needs.targets.outputs.newest".to_owned(), "true".to_owned()),
    ]);
    let runs = |names: &BTreeMap<String, String>, cancelled: bool, failed: bool| {
        let context = Context {
            names: names.clone(),
            cancelled,
            failed,
        };
        holds(SET_IF, &context).expect("the test reads the condition")
    };
    // The deploy skipped (no job it needs failed) and the deploy succeeded look the same to the
    // condition: the wait runs.
    assert!(runs(&on, false, false));
    // A job it needs failed, at any depth: the deploy, or the digest read before it.
    assert!(!runs(&on, false, true));
    assert!(!runs(&on, true, false));
    let mut older = on.clone();
    older.insert(
        "needs.targets.outputs.newest".to_owned(),
        "false".to_owned(),
    );
    assert!(!runs(&older, false, false));
    let mut off = on.clone();
    off.insert("vars.RELEASE_SETS_DESIRED".to_owned(), String::new());
    assert!(!runs(&off, false, false));
    // The set still needs the wait to have succeeded, so a failed deploy sets nothing either.
    assert!(SET_DESIRED_IF.contains("needs.coordinator.result == 'success'"));
}

#[test]
fn a_deploy_job_with_another_token_right_than_the_oidc_token_is_refused() {
    // Catches: the deploy given a repository right, a write scope or another read scope beside
    // the OIDC token, or none, so that it falls back to the repository's default.
    let perms = "    permissions:\n      id-token: write\n";
    let exactly = "release.yml: job deploy must hold exactly `id-token: write`";
    for to in [
        "    permissions:\n      id-token: write\n      contents: read\n",
        "    permissions:\n      id-token: write\n      actions: read\n",
        "    permissions:\n      contents: read\n",
        "    permissions:\n      id-token: read\n",
        "",
    ] {
        assert_has(&changed(JOB, perms, to), exactly);
    }
    let found = changed(
        JOB,
        perms,
        "    permissions:\n      id-token: write\n      contents: write\n",
    );
    assert_has(
        &found,
        "job deploy: `contents: write` is not allowed in release.yml",
    );
    assert_has(&found, exactly);
    let found = changed(
        JOB,
        perms,
        "    permissions:\n      id-token: write\n      packages: write\n",
    );
    assert_has(
        &found,
        "job deploy: `packages: write` is not allowed in release.yml",
    );
    assert_has(&found, exactly);
}

#[test]
fn id_token_write_is_refused_in_every_other_job_and_workflow() {
    // Catches: a second job that can ask for an OIDC token (so one more job the updater's
    // proxy would have to tell from the deploy), in release.yml or in another workflow, and the
    // deploy's name given to a job of another file.
    assert!(
        WRITE_ALLOWED
            .iter()
            .all(|a| a.job != JOB && a.job != DIGEST_JOB),
        "the deploy's scope is held by deploy.rs, not by the table"
    );
    assert!(ID_TOKEN_ALLOWED.iter().all(|(_, name)| *name != JOB));
    let read = "    permissions:\n      contents: read\n";
    let found = changed(
        DIGEST_JOB,
        read,
        "    permissions:\n      contents: read\n      id-token: write\n",
    );
    assert_has(
        &found,
        "job deploy-digest: `id-token: write` is not allowed in release.yml",
    );
    assert_has(
        &found,
        "release.yml: job deploy-digest must hold exactly `contents: read`",
    );
    let found = changed(
        "image",
        "      contents: read\n      packages: write\n",
        "      contents: read\n      packages: write\n      id-token: write\n",
    );
    assert_has(
        &found,
        "job image: `id-token: write` is not allowed in release.yml",
    );
    // The same call, named `deploy`, in another workflow file.
    let elsewhere = format!(
        "name: example\non:\n  push:\n    branches: [main]\npermissions: {{}}\njobs:\n  deploy:\n    permissions:\n      id-token: write\n    uses: {WORKFLOW}@{COMMIT} # v{RELEASE}\n"
    );
    let found = lint("ci.yml", &elsewhere);
    assert_has(
        &found,
        "job deploy: `id-token: write` is not allowed in ci.yml",
    );
    assert_has(&found, "job deploy: no `runs-on`");
}

#[test]
fn a_call_of_another_reusable_workflow_is_refused() {
    // Catches: the deploy job pointed at another repository's workflow, or another workflow of
    // the updater's, and a second job of release.yml that calls a reusable workflow and so runs
    // with no runner the lint can read.
    let uses = uses_line();
    for other in [
        "other-org/docker-swarm-updater/.github/workflows/deploy.yml",
        "komira-ai/docker-swarm-updater/.github/workflows/other.yml",
        "komira-ai/hostkeeper/.github/workflows/set-desired.yml",
    ] {
        let to = format!("    uses: {other}@{COMMIT} # v{RELEASE}\n");
        let found = changed(JOB, &uses, &to);
        let want = format!("release.yml: job deploy calls {other}, not {WORKFLOW}");
        assert_has(&found, &want);
    }
    let found = changed(JOB, &uses, "");
    let want = format!("release.yml: job deploy does not call {WORKFLOW}");
    assert_has(&found, &want);
    let text = format!(
        "{}  other:\n    needs: [plan]\n    uses: {WORKFLOW}@{COMMIT} # v{RELEASE}\n",
        release_yml()
    );
    let found = all_problems(&text);
    assert_has(&found, "job other: no `runs-on`");
    assert_has(&found, "release.yml: jobs are");
}

#[test]
fn a_call_that_is_not_pinned_to_the_commit_that_was_read_is_refused() {
    // Catches: a branch, a tag or a short or upper-case commit in place of the full commit (a
    // move of the tag would change what runs with the OIDC token), another commit, and a version
    // comment that names another release or none.
    let uses = uses_line();
    let at = |pin: &str, comment: &str| format!("    uses: {WORKFLOW}@{pin}{comment}\n");
    let must_pin = format!("release.yml: job deploy must pin {WORKFLOW} at the revision {COMMIT}");
    let comment = format!(" # v{RELEASE}");
    for pin in [
        "v0.1.3",
        "main",
        "c1fcc47610e2886d4d9e561be3c0c76f0452d1f",
        "C1FCC47610E2886D4D9E561BE3C0C76F0452D1F6",
    ] {
        let found = changed(JOB, &uses, &at(pin, &comment));
        assert_has(&found, "is not pinned to a 40-digit lowercase commit SHA");
        assert_has(&found, &must_pin);
    }
    let found = changed(JOB, &uses, &format!("    uses: {WORKFLOW}\n"));
    assert_has(&found, "is not pinned (owner/repo@<40 hex digits>)");
    let other = "0123456789abcdef0123456789abcdef01234567";
    let found = changed(JOB, &uses, &at(other, &comment));
    assert_has(&found, &format!("{must_pin}, not `{WORKFLOW}@{other}`"));
    let want = format!("the version comment on the `uses:` line must start with `v{RELEASE}`");
    for wrong in [" # v0.1.2", " # v0.1.30", ""] {
        let found = changed(JOB, &uses, &at(COMMIT, wrong));
        assert_has(&found, &want);
    }
}

#[test]
fn a_deploy_job_that_passes_other_inputs_or_other_keys_is_refused() {
    // Catches: an input changed to come from somewhere else (another service, a digest that is
    // not the read one, another environment, another tailnet tag), a signer input that would
    // name another repository, workflow or ref than this repository's signed release, and a
    // key that makes the caller run or carry something of its own.
    let text = job_text(JOB);
    let passes = "release.yml: job deploy passes ";
    let tags = "      tailscale-tags: tag:cp-deploy\n";
    let digest = "      image-digest: ${{ needs.deploy-digest.outputs.digest }}\n";
    let zeros = "0".repeat(64);
    for (from, to) in [
        (
            "      service: hostkeeper-coordinator\n",
            "      service: hostkeeper\n".to_owned(),
        ),
        (
            "      environment: controlplane\n",
            "      environment: production\n".to_owned(),
        ),
        (tags, "      tailscale-tags: tag:ci\n".to_owned()),
        (digest, format!("      image-digest: sha256:{zeros}\n")),
        (
            "      endpoint: ${{ vars.CP_DEPLOY_ENDPOINT }}\n",
            "      endpoint: https://front.example\n".to_owned(),
        ),
        (
            tags,
            format!("{tags}      signer-repository: other-org/hostkeeper\n"),
        ),
        (tags, format!("{tags}      signer-ref: refs/heads/other\n")),
        (tags, format!("{tags}      wait-minutes: 0\n")),
        (tags, String::new()),
    ] {
        assert!(text.contains(from), "{from:?}");
        let found = changed(JOB, from, &to);
        assert!(
            found.iter().any(|p| p.starts_with(passes)),
            "{from:?} as {to:?}: {found:?}"
        );
    }
    let pin = uses_line();
    for (key, line) in [
        ("secrets", "    secrets:\n      A: ${{ secrets.A }}\n"),
        ("runs-on", "    runs-on: ubuntu-24.04\n"),
        ("env", "    env:\n      A: b\n"),
        ("strategy", "    strategy:\n      matrix:\n        a: [b]\n"),
    ] {
        let found = changed(JOB, &pin, &format!("{pin}{line}"));
        let want = format!("release.yml: job deploy has `{key}`, which it may not");
        assert_has(&found, &want);
    }
    let found = changed(JOB, &pin, &format!("{pin}    secrets: inherit\n"));
    assert_has(
        &found,
        "`secrets: inherit` hands every secret of the caller on",
    );
}

#[test]
fn a_deploy_job_that_does_not_wait_for_the_digest_and_the_targets_is_refused() {
    // Catches: the call made before the digest is read or before `targets` found this release
    // the newest published, with an empty digest or the wrong version.
    let needs = "    needs: [plan, targets, deploy-digest]\n";
    let want = "[\"deploy-digest\", \"plan\", \"targets\"]";
    for (to, got) in [
        ("[plan, targets]", "[\"plan\", \"targets\"]"),
        ("[plan, deploy-digest]", "[\"deploy-digest\", \"plan\"]"),
        (
            "[plan, publish, deploy-digest]",
            "[\"deploy-digest\", \"plan\", \"publish\"]",
        ),
        ("targets", "[\"targets\"]"),
    ] {
        let found = changed(JOB, needs, &format!("    needs: {to}\n"));
        assert_has(
            &found,
            &format!("release.yml: job deploy needs {got}, not {want}"),
        );
    }
    let found = changed(
        DIGEST_JOB,
        "    needs: [plan, targets]\n",
        "    needs: [plan, publish]\n",
    );
    assert_has(
        &found,
        "release.yml: job deploy-digest needs [\"plan\", \"publish\"], not [\"plan\", \"targets\"]",
    );
}

#[test]
fn a_digest_job_with_other_steps_or_outputs_is_refused() {
    // Catches: the digest read from other bytes than the run's own checked artifact, another
    // platform's manifest taken for amd64's, a step that can be skipped or fail quietly, a value
    // written into a script, a step added after the checks, and another output.
    let who = "release.yml: job deploy-digest";
    let artifact = "          name: release-v${{ needs.targets.outputs.version }}\n";
    let found = changed(DIGEST_JOB, artifact, "          name: release-latest\n");
    assert_has(
        &found,
        &format!(
            "{who} step 1 is `uses actions/download-artifact with name=release-latest with path=dist`"
        ),
    );
    let found = changed(
        DIGEST_JOB,
        "          path: dist\n",
        "          path: dist\n          run-id: ${{ github.event.workflow_run.id }}\n",
    );
    assert_has(&found, &format!("{who} step 1 is "));
    let check =
        "        working-directory: dist\n        run: sha256sum --check --strict SHA256SUMS\n";
    let moved = "        run: sha256sum --check --strict dist/SHA256SUMS\n";
    let found = changed(DIGEST_JOB, check, moved);
    assert_has(&found, &format!("{who} step 2 is "));
    let found = changed(
        DIGEST_JOB,
        "architecture == \"amd64\"",
        "architecture == \"arm64\"",
    );
    assert_has(&found, &format!("{who} step 3 is "));
    let file = " end' \"hostkeeper-coordinator-$VERSION.image.json\"";
    let spliced =
        " end' \"hostkeeper-coordinator-${{ needs.targets.outputs.version }}.image.json\"";
    let found = changed(DIGEST_JOB, file, spliced);
    assert_has(
        &found,
        &format!("{who} step 3 writes an expression into its `run:`"),
    );
    let step = "      - name: The manifest digest of the linux/amd64 image\n";
    for (key, line) in [
        ("continue-on-error", "        continue-on-error: true\n"),
        ("shell", "        shell: sh\n"),
        ("if", "        if: ${{ false }}\n"),
    ] {
        let found = changed(DIGEST_JOB, step, &format!("{step}{line}"));
        let want = format!("{who} step 3 has `{key}`, which a step of it may not");
        assert_has(&found, &want);
    }
    let found = changed(DIGEST_JOB, "        id: digest\n", "        id: image\n");
    assert_has(&found, &format!("{who} step ids are"));
    let found = changed(
        DIGEST_JOB,
        "      digest: ${{ steps.digest.outputs.digest }}\n",
        "      digest: ${{ steps.digest.outputs.digest }}\n      extra: ${{ steps.digest.outputs.extra }}\n",
    );
    assert_has(&found, &format!("{who} outputs are"));
    let found = changed(
        DIGEST_JOB,
        "      contents: read\n",
        "      contents: read\n      packages: write\n",
    );
    assert_has(&found, &format!("{who} must hold exactly `contents: read`"));
}

/// How a run of the digest step ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// The step passed; what it wrote to `GITHUB_OUTPUT`.
    Passed(String),
    /// The step failed; what it wrote to `GITHUB_OUTPUT` before (nothing may be).
    Failed(String),
}

/// An image index as `release image-index` writes it, listing `manifests`, each as its
/// architecture, its operating system and its digest.
fn index_of(manifests: &[(&str, &str, &str)]) -> String {
    let list: Vec<serde_json::Value> = manifests
        .iter()
        .map(|(arch, os, digest)| {
            serde_json::json!({
                "digest": digest,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "platform": { "architecture": arch, "os": os },
                "size": 1234,
            })
        })
        .collect();
    serde_json::json!({
        "manifests": list,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "schemaVersion": 2,
    })
    .to_string()
}

/// Runs the committed step of the job deploy-digest that reads the digest, in a release
/// directory of the release `1.2.3` whose image index is `index`.
fn run_digest_step(index: &str) -> Ended {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("deploy-digest-run");
    let root = TestDir::under(&base, "run");
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist).expect("make the release directory");
    std::fs::write(dist.join("hostkeeper-coordinator-1.2.3.image.json"), index)
        .expect("write the image index");
    let doc = yaml::load(&release_yml()).expect("release.yml parses");
    let digest_job = job(&doc, DIGEST_JOB).expect("release.yml has the job deploy-digest");
    let steps = match entry(digest_job, "steps").map(|s| &s.value) {
        Some(yaml::Value::Seq(items)) => items,
        _ => panic!("the job has no steps"),
    };
    let step = steps
        .iter()
        .find(|s| entry(s, "id").and_then(yaml::Node::as_str) == Some("digest"))
        .expect("the job has the step digest");
    let run = entry(step, "run")
        .and_then(yaml::Node::as_str)
        .expect("the step runs a command");
    let env = entry(step, "env").map_or(&[][..], entries);
    assert_eq!(env.len(), 1, "the step's one input is the version");
    assert_eq!(env[0].0.as_str(), Some("VERSION"));
    let output = root.join("github-output");
    let out = Command::new("bash")
        .args(["--noprofile", "--norc", "-eo", "pipefail", "-c", run])
        .current_dir(&dist)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("VERSION", "1.2.3")
        .env("GITHUB_OUTPUT", &output)
        .output()
        .expect("run bash");
    let written = std::fs::read_to_string(&output).unwrap_or_default();
    if out.status.success() {
        Ended::Passed(written)
    } else {
        Ended::Failed(written)
    }
}

const AMD64: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const ARM64: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

#[test]
fn the_digest_is_the_linux_amd64_manifest_of_the_release_index() {
    // Catches: the digest of the arm64 manifest, or of the index itself, sent to the updater (a
    // Swarm node of another architecture, or an image the release's attestation does not name),
    // whichever order the index lists the platforms in.
    let want = Ended::Passed(format!("digest={AMD64}\n"));
    assert_eq!(
        run_digest_step(&index_of(&[
            ("amd64", "linux", AMD64),
            ("arm64", "linux", ARM64)
        ])),
        want
    );
    assert_eq!(
        run_digest_step(&index_of(&[
            ("arm64", "linux", ARM64),
            ("amd64", "linux", AMD64)
        ])),
        want
    );
}

#[test]
fn an_index_without_exactly_one_amd64_manifest_by_a_sha256_digest_fails_the_step() {
    // Catches: an empty digest passed to the updater (an index with no amd64 manifest, as of an
    // arm64-only release), a guess between two, a manifest of another operating system taken for
    // amd64's, and a value that is not a sha256 digest.
    let cases: [(&str, String); 5] = [
        ("no amd64", index_of(&[("arm64", "linux", ARM64)])),
        ("no manifests", index_of(&[])),
        (
            "two amd64",
            index_of(&[("amd64", "linux", AMD64), ("amd64", "linux", ARM64)]),
        ),
        ("not linux", index_of(&[("amd64", "windows", AMD64)])),
        (
            "not a sha256 digest",
            index_of(&[("amd64", "linux", "sha256:abc")]),
        ),
    ];
    for (name, index) in cases {
        assert_eq!(
            run_digest_step(&index),
            Ended::Failed(String::new()),
            "{name}"
        );
    }
    assert_eq!(
        run_digest_step("not json"),
        Ended::Failed(String::new()),
        "an index that is not JSON"
    );
}
