//! The recorded sequences (`fixtures/recorded/sequences/*.json`) and the engine that replays
//! one against a coordinator built the way `config::build` builds it.
//!
//! A sequence is `{"from", "steps": [...]}` and, where kbf's test used a services file of its
//! own, `services_fixture` (a recorded services file to start from), `services_text` (text added
//! after it, or the whole file when there is no fixture) and `oidc_services` (the services the
//! allow-list entry of CI's token names). A step is one call at one time of the fake clock; its
//! expectations are the status, the body bytes (`expect_body`) or fragments of them
//! (`expect_contains`, `expect_not_contains`), as the kbf test that is its source asserted them.
//! An expectation a step does not have is one kbf did not make.
//!
//! Two things are not compared as kbf's build wrote them. `{COMMIT}` in an expected body stands
//! for the commit this build was made from (kbf's tests appended their own the same way). And
//! the status of this build says, of each host, which agent it heard from, in a key kbf's
//! status does not have: it is `"agent":{"name":"kbf-updater"},` for a host that sent no
//! agent, which is every host of a recorded sequence, and is taken out of a status before it
//! is compared.

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
    #[serde(default)]
    pub expect_body: Option<String>,
    #[serde(default)]
    pub expect_contains: Vec<String>,
    #[serde(default)]
    pub expect_not_contains: Vec<String>,
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
/// `name`: `Err` says the first step whose answer is not the recorded one.
pub async fn replay(sequence: &Sequence, name: &str) -> Result<(), String> {
    let mut rig = Rig::new(name, sequence.spec());
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
            other => return Err(format!("step {n}: no call named {other}")),
        };
        let at = format!(
            "{}: step {n} ({} at {})",
            sequence.from, step.call, step.at_secs
        );
        let want_status = step
            .expect_status
            .ok_or(format!("{at}: no expect_status"))?;
        if code != want_status {
            return Err(format!(
                "{at}: status {code}, recorded {want_status}: {got}"
            ));
        }
        if let Some(want) = &step.expect_body {
            let want = want.replace("{COMMIT}", hostkeeper_coordinator::BUILD_COMMIT);
            if got != want {
                return Err(format!("{at}: body\n  {got}\nrecorded\n  {want}"));
            }
        }
        for part in &step.expect_contains {
            if !got.contains(part.as_str()) {
                return Err(format!("{at}: the body lacks {part}: {got}"));
            }
        }
        for part in &step.expect_not_contains {
            if got.contains(part.as_str()) {
                return Err(format!("{at}: the body has {part}: {got}"));
            }
        }
    }
    Ok(())
}
