//! `gh attestation verify` for another repository's signer: its own repository and workflow, its
//! ref only when it is one ref and no glob, and the hosted-runner flag only when the signer asks
//! for it.

use hostkeeper_config::daemon::TrustedSigners;

use super::*;

/// The signer of `workflow` (a path in `repository`) on `refs`, on a hosted runner or not.
fn signer_of(repository: &str, workflow: &str, refs: &[&str], hosted_runner: bool) -> Signer {
    Signer {
        repo: repository.to_owned(),
        workflow: format!("{repository}/{workflow}"),
        signers: TrustedSigners {
            organisation: repository.split('/').next().unwrap_or_default().to_owned(),
            workflow: workflow.to_owned(),
            refs: refs.iter().map(|r| (*r).to_owned()).collect(),
            hosted_runner,
        },
    }
}

#[test]
fn verify_for_another_repository_pins_its_own_signer() {
    // Catches: another repository's asset verified against hostkeeper's signer, so that no
    // release of it ever verifies (or one signed by hostkeeper's workflow does).
    let signer = signer_of(
        "komira-ai/jobscheduler",
        ".github/workflows/release.yml",
        &["refs/heads/main"],
        true,
    );
    let line = words(&[
        "gh",
        "attestation",
        "verify",
        "/x/asset",
        "--repo",
        "komira-ai/jobscheduler",
        "--signer-workflow",
        "komira-ai/jobscheduler/.github/workflows/release.yml",
        "--source-ref",
        "refs/heads/main",
        "--source-digest",
        commit().as_str(),
        "--deny-self-hosted-runners",
        "--format",
        "json",
    ]);
    let uri =
        "https://github.com/komira-ai/jobscheduler/.github/workflows/release.yml@refs/heads/main";
    let api = scripted(&line, &json!([element(uri)]).to_string());
    let got = api.verify(&signer, Path::new("/x/asset"), None, &commit());
    assert_eq!(
        got.map(|list| list
            .into_iter()
            .map(|a| a.signer_workflow)
            .collect::<Vec<_>>()),
        Ok(vec![signer.workflow.clone()])
    );
}

#[test]
fn verify_for_a_signer_of_glob_refs_and_any_runner_passes_neither_flag() {
    // Catches: a glob passed as `--source-ref` (gh matches it literally, so nothing verifies), or
    // `--deny-self-hosted-runners` passed for a signer that allows a self-hosted runner.
    let signer = signer_of(
        "komira-ai/jobscheduler",
        ".github/workflows/release.yml",
        &["refs/tags/v*"],
        false,
    );
    let line = words(&[
        "gh",
        "attestation",
        "verify",
        "/x/asset",
        "--bundle",
        "/x/asset.sigstore.json",
        "--repo",
        "komira-ai/jobscheduler",
        "--signer-workflow",
        "komira-ai/jobscheduler/.github/workflows/release.yml",
        "--source-digest",
        commit().as_str(),
        "--format",
        "json",
    ]);
    let uri =
        "https://github.com/komira-ai/jobscheduler/.github/workflows/release.yml@refs/tags/v1.0.0";
    let api = scripted(&line, &json!([element(uri)]).to_string());
    let bundle = Path::new("/x/asset.sigstore.json");
    let got = api.verify(&signer, Path::new("/x/asset"), Some(bundle), &commit());
    assert_eq!(got.map(|list| list.len()), Ok(1));
}
