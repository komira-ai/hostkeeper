//! The rule of `previous_release.rs` on the committed workflows and pin, and each defect it
//! refuses, put into the committed `privileged.yml` and `release.yml` (or the pin) one at a time.

use std::path::PathBuf;

use super::lint::PRIVILEGED_WORKFLOW;
use super::pr_device;
use super::previous_release::{
    CHECK, DOWNLOAD, LINUX_JOBS, MACOS_JOBS, previous_release_problems as problems_of,
};
use super::release_macos;
use super::workflows;

/// The committed workflow `file`.
fn committed(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .unwrap_or_else(|| panic!("{file} is in the repository"))
        .1
}

/// The committed pin.
fn pin() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hostkeeper-privileged/previous-release.toml");
    std::fs::read_to_string(path).expect("read previous-release.toml")
}

/// The value of `key` in the `[platform]` table of the pin `pin`.
fn pinned(pin: &str, platform: &str, key: &str) -> String {
    let table: toml::Table = pin.parse().expect("the pin parses");
    table[platform][key]
        .as_str()
        .expect("a text value")
        .to_owned()
}

/// `text` with `from` replaced by `to` in the lines of the job `job` alone, once.
fn in_job(text: &str, job: &str, from: &str, to: &str) -> String {
    let head = format!("\n  {job}:\n");
    let start = text.find(&head).expect("the job is in the text") + 1;
    let end = text[start..]
        .match_indices("\n  ")
        .find(|(at, _)| {
            let rest = &text[start + at + 3..];
            !rest.starts_with(' ') && !rest.starts_with('#') && !rest.starts_with('\n')
        })
        .map_or(text.len(), |(at, _)| start + at + 1);
    let section = &text[start..end];
    assert_eq!(
        section.matches(from).count(),
        1,
        "{from:?} once in job {job}"
    );
    format!(
        "{}{}{}",
        &text[..start],
        section.replacen(from, to, 1),
        &text[end..]
    )
}

const RELEASE: &str = release_macos::FILE;

#[test]
fn the_committed_jobs_download_and_check_the_pinned_release() {
    // Catches: a job of any workflow that runs the cross-release rig without the pinned
    // release, with another release's URL or digest, or checking it after the tests are built.
    let pin = pin();
    for file in [PRIVILEGED_WORKFLOW, pr_device::FILE] {
        assert_eq!(
            problems_of(&committed(file), &pin, &LINUX_JOBS),
            Vec::<String>::new(),
            "{file}"
        );
    }
    assert_eq!(
        problems_of(&committed(RELEASE), &pin, &MACOS_JOBS),
        Vec::<String>::new(),
        "{RELEASE}"
    );
}

#[test]
fn an_env_that_differs_from_the_pin_is_refused() {
    // Catches: the other platform's URL, a digest one character off, a pin moved without the
    // workflow, and an env left out.
    let text = committed(PRIVILEGED_WORKFLOW);
    let release = committed(RELEASE);
    let pin = pin();
    let linux_url = pinned(&pin, "linux-x86_64", "url");
    let mac_url = pinned(&pin, "macos-arm64", "url");
    let mac_sha = pinned(&pin, "macos-arm64", "sha256");

    let swapped = in_job(&text, "privileged", &linux_url, &mac_url);
    assert_eq!(
        problems_of(&swapped, &pin, &LINUX_JOBS),
        [format!(
            "job privileged: PREVIOUS_RELEASE_URL is `{mac_url}`, not the url `{linux_url}` of [linux-x86_64] in previous-release.toml"
        )]
    );
    let typo = format!("{}0", &mac_sha[..63]);
    assert_ne!(typo, mac_sha);
    let off = in_job(&release, "macos-device", &mac_sha, &typo);
    assert_eq!(
        problems_of(&off, &pin, &MACOS_JOBS),
        [format!(
            "job macos-device: PREVIOUS_RELEASE_SHA256 is `{typo}`, not the sha256 `{mac_sha}` of [macos-arm64] in previous-release.toml"
        )]
    );
    let moved = pin.replace(&mac_sha, &typo);
    assert_eq!(
        problems_of(&release, &moved, &MACOS_JOBS),
        [format!(
            "job macos-device: PREVIOUS_RELEASE_SHA256 is `{mac_sha}`, not the sha256 `{typo}` of [macos-arm64] in previous-release.toml"
        )]
    );
    let no_env = in_job(
        &text,
        "privileged",
        &format!("      PREVIOUS_RELEASE_URL: {linux_url}\n"),
        "",
    );
    assert_eq!(
        problems_of(&no_env, &pin, &LINUX_JOBS),
        ["job privileged: no env PREVIOUS_RELEASE_URL"]
    );
}

#[test]
fn a_download_or_check_step_missing_or_out_of_order_is_refused() {
    // Catches: the rig run with no download (the test would fail by name, after the build), with
    // the download unchecked, the check before the download, or the check after the build.
    let text = committed(PRIVILEGED_WORKFLOW);
    let release = committed(RELEASE);
    let pin = pin();
    let no_download = in_job(&text, "privileged", DOWNLOAD, "true");
    assert_eq!(
        problems_of(&no_download, &pin, &LINUX_JOBS),
        [format!("job privileged: no step runs `{DOWNLOAD}`")]
    );
    let no_check = in_job(&release, "macos-device", CHECK, "true");
    assert_eq!(
        problems_of(&no_check, &pin, &MACOS_JOBS),
        [format!("job macos-device: no step runs `{CHECK}`")]
    );
    let swapped = in_job(&text, "privileged", DOWNLOAD, "PLACEHOLDER");
    let swapped = in_job(&swapped, "privileged", CHECK, DOWNLOAD);
    let swapped = in_job(&swapped, "privileged", "PLACEHOLDER", CHECK);
    assert_eq!(
        problems_of(&swapped, &pin, &LINUX_JOBS),
        ["job privileged: the previous release is checked before it is downloaded"]
    );
    let build = release
        .lines()
        .find(|line| line.contains("--no-run --test device_mac "))
        .and_then(|line| line.trim_start().strip_prefix("run: "))
        .expect("the macOS job's build step")
        .to_owned();
    let late = in_job(&release, "macos-device", CHECK, "PLACEHOLDER");
    let late = in_job(&late, "macos-device", &build, CHECK);
    let late = in_job(&late, "macos-device", "PLACEHOLDER", &build);
    assert_eq!(
        problems_of(&late, &pin, &MACOS_JOBS),
        ["job macos-device: the previous release is not checked before the build step"]
    );
}

#[test]
fn a_missing_job_or_an_unreadable_pin_is_refused() {
    // Catches: the rule passing when a job it holds is renamed away, when the pin lacks the
    // job's platform, or when the pin or the workflow does not parse.
    let text = committed(PRIVILEGED_WORKFLOW);
    let release = committed(RELEASE);
    let pin = pin();
    let gone = release.replace("\n  macos-device:\n", "\n  macos-gone:\n");
    assert_eq!(
        problems_of(&gone, &pin, &MACOS_JOBS),
        ["no job macos-device"]
    );
    let no_table = pin.replace("[linux-x86_64]", "[linux-x86_64-old]");
    assert_eq!(
        problems_of(&text, &no_table, &LINUX_JOBS),
        [
            "job privileged: previous-release.toml has no url in [linux-x86_64]",
            "job privileged: previous-release.toml has no sha256 in [linux-x86_64]",
        ]
    );
    let broken = problems_of(&text, "[linux-x86_64", &LINUX_JOBS);
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(
        broken[0].starts_with("previous-release.toml does not parse: "),
        "{broken:?}"
    );
    assert_eq!(problems_of("on: [unclosed\n", &pin, &LINUX_JOBS).len(), 1);
}
