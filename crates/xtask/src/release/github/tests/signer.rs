//! `gh attestation verify` for the signer of a trust list entry: its own repository and
//! workflow, its ref only when it is one ref and no glob, and the hosted-runner flag only when the
//! entry asks for it.

use super::*;

/// The signer of the trust list entry `keys` (one `[[service]]` table's keys).
fn signer_of(keys: &str) -> Signer {
    let trust = hostkeeper_config::channel_trust::ChannelTrust::parse(&format!(
        "[[service]]\nname = \"js-agent\"\n{keys}\n"
    ))
    .expect("a trust list");
    Signer::of(&trust.services()[0])
}

#[test]
fn verify_for_another_repository_pins_its_own_signer() {
    // Catches: another repository's asset verified against hostkeeper's signer, so that no
    // release of it ever verifies (or one signed by hostkeeper's workflow does).
    let signer = signer_of(
        "repository = \"komira-ai/jobscheduler\"\nworkflow = \".github/workflows/release.yml\"\n\
         refs = [\"refs/heads/main\"]\nhosted_runner = true",
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
        "repository = \"komira-ai/jobscheduler\"\nworkflow = \".github/workflows/release.yml\"\n\
         refs = [\"refs/tags/v*\"]\nhosted_runner = false",
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
