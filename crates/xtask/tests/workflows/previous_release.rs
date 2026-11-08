//! The download of the previous release in the two jobs of `privileged.yml` that run the
//! cross-release rig (`crates/hostkeeper-privileged/tests/self_update_across_releases.rs`), held to
//! its pin, `crates/hostkeeper-privileged/previous-release.toml`: each job's `PREVIOUS_RELEASE_URL`
//! and `PREVIOUS_RELEASE_SHA256` are the `url` and `sha256` of its platform's table there, and the
//! job runs [`DOWNLOAD`] and then [`CHECK`], both before its build step. pr-device.yml's copies of
//! the two jobs are held to these by the mirror rule (`pr_device.rs`). `previous_release_tests.rs`
//! shows each defect refused.

use super::lint::entry;
use super::privileged_tests::{BUILD, runs};
use super::yaml::{self, Node};

/// The step that downloads the pinned tarball: HTTPS only, the URL from the job's env.
pub const DOWNLOAD: &str = "curl --proto =https --proto-redir =https --tlsv1.2 --fail --silent --show-error --location --output \"$RUNNER_TEMP/previous-release.tar.gz\" \"$PREVIOUS_RELEASE_URL\"";
/// The step that checks it against the pinned SHA-256 and puts it where the test reads it.
pub const CHECK: &str = "cargo xtask install-tool --download \"$RUNNER_TEMP/previous-release.tar.gz\" --sha256 \"$PREVIOUS_RELEASE_SHA256\" --out target/previous-release/hostkeeper.tar.gz";

/// The jobs that run the cross-release rig, each with the platform table of the pin it
/// downloads.
pub const JOBS: [(&str, &str); 2] = [
    ("privileged", "linux-x86_64"),
    ("macos-device", "macos-arm64"),
];

/// The problems of the workflow `text` against the pin text `pin` (the module says what holds).
pub fn previous_release_problems(text: &str, pin: &str) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let pin: toml::Table = match pin.parse() {
        Ok(pin) => pin,
        Err(e) => return vec![format!("previous-release.toml does not parse: {e}")],
    };
    let mut problems = Vec::new();
    for (name, platform) in JOBS {
        let Some(job) = entry(&doc, "jobs").and_then(|jobs| entry(jobs, name)) else {
            problems.push(format!("no job {name}"));
            continue;
        };
        for (var, key) in [
            ("PREVIOUS_RELEASE_URL", "url"),
            ("PREVIOUS_RELEASE_SHA256", "sha256"),
        ] {
            let pinned = pin
                .get(platform)
                .and_then(|table| table.get(key))
                .and_then(toml::Value::as_str);
            let Some(pinned) = pinned else {
                problems.push(format!(
                    "job {name}: previous-release.toml has no {key} in [{platform}]"
                ));
                continue;
            };
            match entry(job, "env")
                .and_then(|env| entry(env, var))
                .and_then(Node::as_str)
            {
                None => problems.push(format!("job {name}: no env {var}")),
                Some(value) if value != pinned => problems.push(format!(
                    "job {name}: {var} is `{value}`, not the {key} `{pinned}` of [{platform}] in previous-release.toml"
                )),
                Some(_) => {}
            }
        }
        let runs = runs(job);
        let at = |command: &str| runs.iter().position(|run| *run == command);
        let build = runs.iter().position(|run| run.starts_with(BUILD));
        match (at(DOWNLOAD), at(CHECK)) {
            (None, _) => problems.push(format!("job {name}: no step runs `{DOWNLOAD}`")),
            (_, None) => problems.push(format!("job {name}: no step runs `{CHECK}`")),
            (Some(download), Some(check)) => {
                if check < download {
                    problems.push(format!(
                        "job {name}: the previous release is checked before it is downloaded"
                    ));
                }
                if build.is_none_or(|build| build < check) {
                    problems.push(format!(
                        "job {name}: the previous release is not checked before the build step"
                    ));
                }
            }
        }
    }
    problems
}
