//! The recorded sequences (`fixtures/recorded/sequences/*.json`) and the engine that replays
//! one against a coordinator built the way `config::build` builds it.
//!
//! A sequence is `{"from", "steps": [...]}` and, where kbf's test used a rollout file of its
//! own, `services_fixture` (a recorded rollout file to start from), `services_text` (text added
//! after it, or the whole file when there is no fixture) and `oidc_services` (the services the
//! allow-list entry of CI's token names). A step is one call at one time of the fake clock; its
//! expectations are the status and the body bytes (`expect_body`), which every step but a
//! `restart` and a `status` must have; a `status` has `expect_body` or at least one fragment of
//! the body (`expect_contains`), and any step may also have fragments that must (`expect_contains`)
//! or must not (`expect_not_contains`) be in the body. A step without its full answer is refused
//! by name before anything else of it is compared.
//!
//! Two things are not compared as kbf's build wrote them. `{COMMIT}` in an expected body stands
//! for the commit this build was made from (kbf's tests appended their own the same way). And
//! the status of this build says, of each host, which agent it heard from, in a key kbf's
//! status does not have: it is `"agent":{"name":"kbf-updater"},` for a host that sent no
//! agent, which is every host of a recorded sequence, and is taken out of a status before it
//! is compared.
//!
//! A step may keep what kbf's coordinator answered where a rule this coordinator cut decided the
//! answer: `kbf_status` and `kbf_body`. They are never compared with an answer; they document
//! the difference, and `check_changed_list` holds the sequences that keep them to the list in
//! `tests/replay.rs`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::rig::{Rig, Spec};
use super::{OPERATOR, ci_token, recorded};

/// The key every host of a status has that kbf's status does not (see the module).
const AGENT_KEY: &str = r#""agent":{"name":"kbf-updater"},"#;

/// The directory of the recorded sequences.
pub fn sequences_dir() -> PathBuf {
    Path::new(super::RECORDED).join("sequences")
}

/// The recorded sequence files, by name.
pub fn sequence_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(sequences_dir())
        .expect("the sequences directory")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

/// One recorded sequence.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    /// The kbf test it was transcribed from, and how.
    pub from: String,
    #[serde(default)]
    pub services_fixture: Option<String>,
    #[serde(default)]
    pub services_text: Option<String>,
    #[serde(default)]
    pub oidc_services: Option<Vec<String>>,
    pub steps: Vec<Step>,
}

/// One call.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The clock when the call is made.
    pub at_secs: u64,
    /// `heartbeat`, `desired`, `operator`, `status`, `convergence` or `restart`.
    pub call: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    /// The request body, as text.
    #[serde(default)]
    pub body: Option<String>,
    /// What follows the `?` of a convergence read.
    #[serde(default)]
    pub query: Option<String>,
    /// `ci`, `operator`, `none`, or any other text sent as the bearer token as it is.
    #[serde(default)]
    pub auth: Option<String>,
    #[serde(default)]
    pub expect_status: Option<u16>,
    /// The full body of the answer; required of every step but a `status` and a `restart`.
    #[serde(default)]
    pub expect_body: Option<String>,
    #[serde(default)]
    pub expect_contains: Vec<String>,
    #[serde(default)]
    pub expect_not_contains: Vec<String>,
    /// What kbf's coordinator answered (status), where a rule this coordinator cut decided and
    /// the answer differs. Documentation: never compared.
    #[serde(default)]
    pub kbf_status: Option<u16>,
    /// What kbf's coordinator answered (body), where it differs. Documentation: never compared.
    #[serde(default)]
    pub kbf_body: Option<String>,
}

impl Sequence {
    /// The sequence in the file `path`.
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).expect("a sequence file");
        Self::parse(&text).unwrap_or_else(|why| panic!("{}: {why}", path.display()))
    }

    /// The sequence in `text`.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// What a coordinator replaying this sequence is built from.
    pub fn spec(&self) -> Spec {
        let services = match (&self.services_fixture, &self.services_text) {
            (None, None) => recorded("services.toml"),
            (Some(fixture), None) => recorded(fixture),
            (None, Some(text)) => text.clone(),
            (Some(fixture), Some(text)) => format!("{}\n{text}", recorded(fixture)),
        };
        let mut spec = Spec::recorded().with_services(&services);
        spec.oidc_services.clone_from(&self.oidc_services);
        spec
    }
}

fn bearer(step: &Step, default: &str) -> Option<String> {
    match step.auth.as_deref().unwrap_or(default) {
        "ci" => Some(ci_token()),
        "operator" => Some(OPERATOR.to_owned()),
        "none" => None,
        other => Some(other.to_owned()),
    }
}

/// Replays `sequence` on a fresh coordinator whose files are in a scratch directory called
/// `name`: `Err` says every step whose answer is not the recorded one, with the answer in full.
pub async fn replay(sequence: &Sequence, name: &str) -> Result<(), String> {
    let mut rig = Rig::new(name, sequence.spec());
    let mut differences: Vec<String> = Vec::new();
    for (n, step) in sequence.steps.iter().enumerate() {
        rig.at(step.at_secs);
        let body = step.body.as_deref().unwrap_or("");
        let (code, got) = match step.call.as_str() {
            "restart" => {
                rig.restart();
                continue;
            }
            "heartbeat" => {
                let host = step.host.as_deref().ok_or("a heartbeat step has no host")?;
                let role = step.role.as_deref().ok_or("a heartbeat step has no role")?;
                rig.heartbeat(host, role, body).await
            }
            "desired" => {
                let token = bearer(step, "ci");
                rig.control("POST", "/v1/desired", token.as_deref(), body)
                    .await
            }
            "operator" => {
                let token = bearer(step, "operator");
                rig.control("POST", "/v1/operator", token.as_deref(), body)
                    .await
            }
            "status" => {
                let token = bearer(step, "operator");
                let (code, got) = rig.control("GET", "/v1/status", token.as_deref(), "").await;
                (code, got.replace(AGENT_KEY, ""))
            }
            "convergence" => {
                let token = bearer(step, "ci");
                let path = match &step.query {
                    Some(query) => format!("/v1/convergence?{query}"),
                    None => "/v1/convergence".to_owned(),
                };
                rig.control("GET", &path, token.as_deref(), "").await
            }
            other => return Err(format!("{name}: step {n}: no call named {other}")),
        };
        let at = format!("{name}: step {n} ({} at {})", step.call, step.at_secs);
        let Some(want_status) = step.expect_status else {
            return Err(format!("{at}: no expect_status"));
        };
        if step.call == "status" {
            if step.expect_body.is_none() && step.expect_contains.is_empty() {
                return Err(format!(
                    "{at}: a status step has neither expect_body nor expect_contains"
                ));
            }
        } else if step.expect_body.is_none() {
            return Err(format!("{at}: no expect_body"));
        }
        if code != want_status {
            differences.push(format!(
                "{at}: status {code}, recorded {want_status}: {got}"
            ));
        }
        if let Some(want) = &step.expect_body {
            let want = want.replace("{COMMIT}", hostkeeper_coordinator::BUILD_COMMIT);
            if got != want {
                differences.push(format!(
                    "{at}: body (status {code})\n  {got}\nrecorded\n  {want}"
                ));
            }
        }
        for part in &step.expect_contains {
            if !got.contains(part.as_str()) {
                differences.push(format!("{at}: the body lacks {part}: {got}"));
            }
        }
        for part in &step.expect_not_contains {
            if got.contains(part.as_str()) {
                differences.push(format!("{at}: the body has {part}: {got}"));
            }
        }
    }
    if differences.is_empty() {
        Ok(())
    } else {
        Err(differences.join("\n"))
    }
}

/// Whether the list `changed` agrees with what the sequences keep of kbf's answers: every name is
/// a recorded sequence; a listed sequence has a step that keeps kbf's answer (`kbf_status` or
/// `kbf_body`) and each answer kept differs from this coordinator's; an unlisted sequence keeps
/// none. `Err` says every disagreement, one per line.
pub fn check_changed_list(
    changed: &[&str],
    sequences: &[(String, Sequence)],
) -> Result<(), String> {
    let mut problems = Vec::new();
    for name in changed {
        if !sequences.iter().any(|(stem, _)| stem == name) {
            problems.push(format!(
                "CHANGED names {name}, which is no recorded sequence"
            ));
        }
    }
    for (stem, sequence) in sequences {
        let kept: Vec<(usize, &Step)> = sequence
            .steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.kbf_status.is_some() || step.kbf_body.is_some())
            .collect();
        let listed = changed.contains(&stem.as_str());
        if listed && kept.is_empty() {
            problems.push(format!(
                "{stem} is in CHANGED but no step keeps kbf's answer (kbf_status or kbf_body)"
            ));
        }
        if !listed && !kept.is_empty() {
            problems.push(format!(
                "{stem} keeps kbf's answer in a step but is not in CHANGED"
            ));
        }
        for (n, step) in kept {
            if step.kbf_status.is_some() && step.kbf_status == step.expect_status {
                problems.push(format!(
                    "{stem}: step {n}: kbf_status is the same as this coordinator's"
                ));
            }
            if step.kbf_body.is_some() && step.kbf_body == step.expect_body {
                problems.push(format!(
                    "{stem}: step {n}: kbf_body is the same as this coordinator's"
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}
