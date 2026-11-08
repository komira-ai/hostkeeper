//! The images the job `privileged` of `privileged.yml` pulls, held to
//! `crates/hostkeeper-privileged/linux-releases.toml`, the list of Linux releases the packages
//! test (`tests/linux_packages.rs`) runs a container of: the job has exactly one step
//! [`PULL`]` <image>` for each listed image, every image is pinned by digest, no other step pulls
//! an image, and every pull comes before the step that runs the tests as root (the test runs its
//! containers with `--pull=never`). The list itself must be a list of `[[release]]` tables with
//! the text keys `name`, `format` (`deb`, `rpm` or `arch`) and `image`. pr-device.yml's copy of the
//! job is held to this one by the mirror rule (`pr_device.rs`). `linux_releases_tests.rs` shows
//! each defect refused.

use std::collections::BTreeMap;

use super::lint::{entry, root_line};
use super::privileged_tests::runs;
use super::yaml;

/// The command of a step that pulls an image, before the image.
pub const PULL: &str = "docker pull";
/// The job the rule holds.
pub const JOB: &str = "privileged";

/// The image of each release the list holds, or what is wrong with the list.
fn listed_images(list: &str) -> Result<Vec<String>, Vec<String>> {
    let table: toml::Table = list
        .parse()
        .map_err(|e| vec![format!("linux-releases.toml does not parse: {e}")])?;
    let Some(releases) = table.get("release").and_then(toml::Value::as_array) else {
        return Err(vec![
            "linux-releases.toml has no [[release]] tables".to_owned(),
        ]);
    };
    let mut images = Vec::new();
    let mut problems = Vec::new();
    for (index, release) in releases.iter().enumerate() {
        let at = index + 1;
        let Some(release) = release.as_table() else {
            problems.push(format!("linux-releases.toml: release {at} is not a table"));
            continue;
        };
        for key in ["name", "format", "image"] {
            if release.get(key).and_then(toml::Value::as_str).is_none() {
                problems.push(format!(
                    "linux-releases.toml: release {at} has no text {key}"
                ));
            }
        }
        if let Some(format) = release.get("format").and_then(toml::Value::as_str)
            && !["deb", "rpm", "arch"].contains(&format)
        {
            problems.push(format!(
                "linux-releases.toml: release {at} has the format `{format}`, not deb, rpm or arch"
            ));
        }
        if let Some(image) = release.get("image").and_then(toml::Value::as_str) {
            if pinned(image) {
                images.push(image.to_owned());
            } else {
                problems.push(format!(
                    "linux-releases.toml: release {at} has the image `{image}`, which is not pinned by digest"
                ));
            }
        }
    }
    if releases.is_empty() {
        problems.push("linux-releases.toml lists no release".to_owned());
    }
    if problems.is_empty() {
        Ok(images)
    } else {
        Err(problems)
    }
}

/// Whether `image` is `<repository>@sha256:<64 lower-case hex digits>`.
fn pinned(image: &str) -> bool {
    image
        .split_once("@sha256:")
        .is_some_and(|(repository, digest)| {
            !repository.is_empty()
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// The problems of the workflow `text` against the list `list` (the module says what holds).
pub fn linux_releases_problems(text: &str, list: &str) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let images = match listed_images(list) {
        Ok(images) => images,
        Err(problems) => return problems,
    };
    let Some(job) = entry(&doc, "jobs").and_then(|jobs| entry(jobs, JOB)) else {
        return vec![format!("no job {JOB}")];
    };
    let runs = runs(job);
    let rooted = runs.iter().position(|run| root_line(&[run]).is_some());
    let mut problems = Vec::new();
    let mut pulls: BTreeMap<&str, usize> = BTreeMap::new();
    for (at, run) in runs.iter().enumerate() {
        if !run.contains(PULL) {
            continue;
        }
        let image = run
            .strip_prefix(PULL)
            .and_then(|rest| rest.strip_prefix(' '))
            .filter(|image| images.iter().any(|listed| listed == image));
        let Some(image) = image else {
            problems.push(format!(
                "job {JOB}: the step `{run}` pulls what linux-releases.toml does not list (a step is `{PULL} <image>`, the image listed)"
            ));
            continue;
        };
        *pulls.entry(image).or_default() += 1;
        if rooted.is_some_and(|rooted| rooted < at) {
            problems.push(format!(
                "job {JOB}: the pull of `{image}` comes after the run as root"
            ));
        }
    }
    for image in &images {
        match pulls.get(image.as_str()).copied().unwrap_or(0) {
            1 => {}
            0 => problems.push(format!("job {JOB}: no step runs `{PULL} {image}`")),
            n => problems.push(format!(
                "job {JOB}: {n} steps run `{PULL} {image}`, not one"
            )),
        }
    }
    problems
}
