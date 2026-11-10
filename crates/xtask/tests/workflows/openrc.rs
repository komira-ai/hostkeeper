//! The job `openrc-device` of `privileged.yml` ([`super::lint::OPENRC_DEVICE_JOB`]): the first
//! line of its test file, and its nfpm held to the one `artifacts.yml` builds the packages with.
//!
//! The job builds its test on the glibc runner and the programs its container runs for musl, so
//! its test file (`crates/hostkeeper-privileged/tests/openrc_device.rs`) starts with
//! [`OPENRC_FIRST_LINE`], which no other job's files start with: the lint ties a test file to
//! its job by that line. [`nfpm_problems`] holds the job's `NFPM_VERSION` and `NFPM_SHA256` to
//! the version and the Linux x86_64 digest of `artifacts.yml`, and its download to that
//! platform's archive name. `openrc_tests.rs` shows each defect refused.

use super::lint::entry;
use super::yaml::{self, Node, Value};

/// The first line of a privileged test file of the job `openrc-device`: built on the glibc
/// runner, driving a musl container.
pub const OPENRC_FIRST_LINE: &str =
    "#![cfg(all(target_os = \"linux\", target_env = \"gnu\", feature = \"privileged\"))]";

/// The workflow that pins nfpm for the packages.
pub const ARTIFACTS: &str = "artifacts.yml";

/// The `platform` of the matrix entry of `artifacts.yml`'s job `build` whose nfpm the job uses.
pub const PLATFORM: &str = "linux-x86_64";

/// The text value of `key` in the mapping `node`.
fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    entry(node, key).and_then(Node::as_str)
}

/// The problems of the job `openrc-device` of the privileged workflow `privileged` against the
/// artifacts workflow `artifacts`: its `NFPM_VERSION` is not `artifacts.yml`'s, its
/// `NFPM_SHA256` is not the `nfpm-sha256` of the matrix entry of [`PLATFORM`], or no step
/// downloads that entry's `nfpm-archive`.
pub fn nfpm_problems(privileged: &str, artifacts: &str) -> Vec<String> {
    let (privileged, artifacts) = match (yaml::load(privileged), yaml::load(artifacts)) {
        (Ok(privileged), Ok(artifacts)) => (privileged, artifacts),
        (Err(e), _) | (_, Err(e)) => return vec![e],
    };
    let job = entry(&privileged, "jobs").and_then(|jobs| entry(jobs, "openrc-device"));
    let Some(job) = job else {
        return vec!["no job openrc-device".to_owned()];
    };
    let include = entry(&artifacts, "jobs")
        .and_then(|jobs| entry(jobs, "build"))
        .and_then(|build| entry(build, "strategy"))
        .and_then(|strategy| entry(strategy, "matrix"))
        .and_then(|matrix| entry(matrix, "include"));
    let platform = match include.map(|include| &include.value) {
        Some(Value::Seq(items)) => items
            .iter()
            .find(|item| text(item, "platform") == Some(PLATFORM)),
        _ => None,
    };
    let (Some(platform), Some(version)) = (
        platform,
        entry(&artifacts, "env").and_then(|env| text(env, "NFPM_VERSION")),
    ) else {
        return vec![format!(
            "{ARTIFACTS} has no NFPM_VERSION or no matrix entry for {PLATFORM}"
        )];
    };
    let mut problems = Vec::new();
    let env = entry(job, "env");
    for (var, want) in [
        ("NFPM_VERSION", Some(version)),
        ("NFPM_SHA256", text(platform, "nfpm-sha256")),
    ] {
        let found = env.and_then(|env| text(env, var));
        if found.is_none() || found != want {
            problems.push(format!(
                "job openrc-device: {var} is {found:?}, not {want:?} of {ARTIFACTS}"
            ));
        }
    }
    let download = text(platform, "nfpm-archive")
        .map(|archive| format!("nfpm_${{NFPM_VERSION}}_{archive}.tar.gz"));
    let downloads = super::privileged_tests::runs(job)
        .iter()
        .any(|run| download.as_deref().is_some_and(|name| run.contains(name)));
    if !downloads {
        problems.push(format!(
            "job openrc-device: no step downloads {download:?}, the nfpm-archive of {PLATFORM} in {ARTIFACTS}"
        ));
    }
    problems
}
