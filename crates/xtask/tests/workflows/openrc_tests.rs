//! The job `openrc-device` of `privileged.yml` and its copy in `pr-device.yml`: the committed
//! job passes every rule, and each rule is shown to refuse its defect by putting it into the
//! committed job, one at a time, and reading the exact problems it reports.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::lint::{OPENRC_DEVICE_JOB, PRIVILEGED_WORKFLOW, lint};
use super::openrc::{ARTIFACTS, OPENRC_FIRST_LINE, nfpm_problems};
use super::pr_device::{self, mirror_problems};
use super::privileged_tests::{
    committed_targets, privileged_targets, shape_problems, starts_with_attribute,
};
use super::workflows;

/// The start of the job in a workflow, once in the file.
const HEAD: &str = "\n  openrc-device:\n";

/// The committed workflow `file`.
fn committed(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .unwrap_or_else(|| panic!("{file} is in the repository"))
        .1
}

/// `text` with `from` replaced by `to`, once, in the lines of the job `openrc-device`, the last
/// job of both workflows.
fn in_job(text: &str, from: &str, to: &str) -> String {
    let start = text.find(HEAD).expect("the job is in the text");
    let (before, job) = text.split_at(start);
    assert_eq!(job.matches(from).count(), 1, "{from:?} once in the job");
    format!("{before}{}", job.replacen(from, to, 1))
}

fn privileged() -> String {
    committed(PRIVILEGED_WORKFLOW)
}

fn device() -> String {
    committed(pr_device::FILE)
}

/// The test targets of the job, as the shape check reads them.
fn targets() -> BTreeSet<String> {
    BTreeSet::from(["openrc_device".to_owned()])
}

fn shape(text: &str) -> Vec<String> {
    shape_problems(PRIVILEGED_WORKFLOW, text, &committed_targets())
}

#[test]
fn the_committed_openrc_device_job_passes_every_rule_and_its_pins_are_artifacts_ymls() {
    // Catches: the job failing the lint or the shape check in either workflow, its copy drifting
    // from it, its test file not tied to it by the first line (so that its target is named in no
    // step), its nfpm not the one artifacts.yml builds the packages with, and its target counted
    // in another job.
    assert_eq!(
        lint(PRIVILEGED_WORKFLOW, &privileged()),
        Vec::<String>::new()
    );
    assert_eq!(lint(pr_device::FILE, &device()), Vec::<String>::new());
    assert_eq!(shape(&privileged()), Vec::<String>::new());
    assert_eq!(
        mirror_problems(&device(), &privileged()),
        Vec::<String>::new()
    );
    assert_eq!(
        nfpm_problems(&privileged(), &committed(ARTIFACTS)),
        Vec::<String>::new()
    );
    let all = committed_targets();
    assert_eq!(all[OPENRC_DEVICE_JOB], targets());
    for (job, others) in &all {
        assert!(
            *job == OPENRC_DEVICE_JOB || !others.contains("openrc_device"),
            "{job} runs the OpenRC device test"
        );
    }
}

#[test]
fn the_first_line_of_the_openrc_test_file_belongs_to_its_job_alone() {
    // Catches: a file read as the job's with another first line (the plain Linux line, which
    // would put it in the job `privileged` on the systemd runner), a first line of another job
    // read as this job's, a file the lint does not discover, and a first line broken over lines
    // by rustfmt not read as the same.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hostkeeper-privileged/tests");
    let text = std::fs::read_to_string(dir.join("openrc_device.rs")).expect("the test file");
    assert!(starts_with_attribute(&text, OPENRC_FIRST_LINE));
    assert_eq!(privileged_targets(OPENRC_FIRST_LINE), targets());
    for other in [
        "#![cfg(feature = \"privileged\")]",
        "#![cfg(all(target_os = \"linux\", feature = \"privileged\"))]",
        "#![cfg(all(target_os = \"macos\", feature = \"privileged\"))]",
        "#![cfg(all(any(target_os = \"linux\", target_os = \"macos\"), feature = \"privileged\"))]",
        "#![cfg(all(target_os = \"linux\", target_arch = \"x86_64\", feature = \"privileged\"))]",
    ] {
        assert!(!starts_with_attribute(&text, other), "{other}");
        assert!(!starts_with_attribute(other, OPENRC_FIRST_LINE), "{other}");
    }
    let broken = "#![cfg(all(\n    target_os = \"linux\",\n    target_env = \"gnu\",\n    feature = \"privileged\"\n))]\n";
    assert!(starts_with_attribute(broken, OPENRC_FIRST_LINE));
}

#[test]
fn a_step_of_the_openrc_job_naming_other_targets_or_running_as_root_twice_is_refused() {
    // Catches: the build or the run as root leaving out the job's test target or adding another
    // job's, a second command as root (here the image build), a build moved under root, and a
    // step that chains two commands.
    let build = "--no-run --test openrc_device";
    let missing = shape(&in_job(&privileged(), build, "--no-run"));
    assert_eq!(
        missing,
        [format!(
            "job openrc-device: the build step names the test targets {:?}, not {:?}",
            BTreeSet::<String>::new(),
            targets()
        )]
    );
    let run = "--test openrc_device -- --test-threads=1";
    let extra = shape(&in_job(
        &privileged(),
        run,
        "--test openrc_device --test observer -- --test-threads=1",
    ));
    assert_eq!(
        extra,
        [format!(
            "job openrc-device: the run as root step names the test targets {:?}, not {:?}",
            BTreeSet::from(["observer".to_owned(), "openrc_device".to_owned()]),
            targets()
        )]
    );
    let image = "run: docker build --tag";
    assert_eq!(
        shape(&in_job(
            &privileged(),
            image,
            "run: sudo docker build --tag"
        )),
        [
            "job openrc-device: sudo is named 2 times, not once",
            "job openrc-device: 2 run as root steps, not one",
        ]
    );
    let version = "run: docker --version\n";
    let chained = shape(&in_job(
        &privileged(),
        version,
        "run: docker --version && docker info\n",
    ));
    assert_eq!(chained.len(), 1, "{chained:?}");
    assert!(
        chained[0].starts_with("job openrc-device: the step `docker --version && docker info`")
            && chained[0].ends_with("runs more than one command"),
        "{chained:?}"
    );
}

#[test]
fn the_openrc_job_is_refused_on_any_runner_but_the_listed_one() {
    // Catches: the job on a runner whose image moves, on another Ubuntu, on macOS (in either
    // workflow), or with a condition or a write scope in pr-device.yml, where it is a required
    // check that must run on every pull request and hold no write scope.
    let runner = "    runs-on: ubuntu-24.04\n";
    for label in ["ubuntu-latest", "ubuntu-22.04", "macos-15"] {
        let text = in_job(&privileged(), runner, &format!("    runs-on: {label}\n"));
        assert_eq!(
            lint(PRIVILEGED_WORKFLOW, &text),
            ["job openrc-device: privileged job on a runner outside the privileged list"],
            "{label}"
        );
    }
    let on_mac = in_job(&device(), runner, "    runs-on: macos-15\n");
    assert_eq!(
        lint(pr_device::FILE, &on_mac),
        [
            "job openrc-device: macOS on pull_request (`macos-15`)",
            "job openrc-device: runs on a runner outside [\"ubuntu-24.04\"]",
        ]
    );
    let conditional = in_job(
        &device(),
        runner,
        "    if: ${{ github.event.pull_request.draft == false }}\n    runs-on: ubuntu-24.04\n",
    );
    assert_eq!(
        lint(pr_device::FILE, &conditional),
        ["job openrc-device: has `if`, and must run and report on every pull request"]
    );
    let write = in_job(&device(), "contents: read", "contents: write");
    assert_eq!(
        lint(pr_device::FILE, &write),
        [
            "job openrc-device: `contents: write` is not allowed in pr-device.yml",
            "job openrc-device: privileged on pull_request (`contents: write`)",
        ]
    );
    let secret = in_job(
        &device(),
        "persist-credentials: false",
        "persist-credentials: false\n          token: ${{ secrets.GITHUB_TOKEN }}",
    );
    assert_eq!(
        lint(pr_device::FILE, &secret),
        ["job openrc-device: a secret on pull_request (`${{ secrets.GITHUB_TOKEN }}`)"]
    );
}

#[test]
fn a_copy_of_the_openrc_job_that_drifted_from_privileged_yml_is_refused() {
    // Catches: a pull request running other OpenRC device steps than run after the merge: a step,
    // a pin or the timeout changed in the copy or in privileged.yml alone, and the copy missing.
    let drift = "job openrc-device: not privileged.yml's job openrc-device, key for key";
    let timeout = "timeout-minutes: 45";
    let changed = in_job(&device(), timeout, "timeout-minutes: 30");
    assert_eq!(mirror_problems(&changed, &privileged()), [drift]);
    let source = in_job(&privileged(), timeout, "timeout-minutes: 30");
    assert_eq!(mirror_problems(&device(), &source), [drift]);
    assert_eq!(mirror_problems(&changed, &source), Vec::<String>::new());
    let run = "--test openrc_device -- --test-threads=1 --nocapture";
    let narrowed = in_job(&device(), run, "--test openrc_device -- --test-threads=1");
    assert_eq!(mirror_problems(&narrowed, &privileged()), [drift]);
    let text = device();
    let at = text
        .find("  # privileged.yml's job openrc-device;")
        .expect("the copy");
    assert_eq!(mirror_problems(&text[..at], &privileged()), [drift]);
}

#[test]
fn an_nfpm_that_is_not_artifacts_ymls_is_refused() {
    // Catches: the job's nfpm version or digest drifting from the one the packages are built
    // with (in the job or in artifacts.yml alone), the arm64 digest or archive taken for the
    // x86_64 one, a download of another archive, and a missing job, entry or file that parses.
    let artifacts = committed(ARTIFACTS);
    let text = privileged();
    let sha = "6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750";
    let arm = "a9a75213763783b7669719e505474c748c5ad7dc66591a195619deb50f85c2f4";
    let wrong_sha = |found: &str| {
        format!(
            "job openrc-device: NFPM_SHA256 is Some({found:?}), not Some({sha:?}) of artifacts.yml"
        )
    };
    let wrong_version = |found: &str, want: &str| {
        format!(
            "job openrc-device: NFPM_VERSION is Some({found:?}), not Some({want:?}) of artifacts.yml"
        )
    };
    assert_eq!(
        nfpm_problems(&in_job(&text, sha, arm), &artifacts),
        [wrong_sha(arm)]
    );
    assert_eq!(
        nfpm_problems(
            &in_job(&text, "NFPM_VERSION: 2.48.0", "NFPM_VERSION: 2.49.0"),
            &artifacts
        ),
        [wrong_version("2.49.0", "2.48.0")]
    );
    let bumped = artifacts.replace("NFPM_VERSION: 2.48.0", "NFPM_VERSION: 2.49.0");
    assert_ne!(bumped, artifacts);
    assert_eq!(
        nfpm_problems(&text, &bumped),
        [wrong_version("2.48.0", "2.49.0")]
    );
    let archive = "nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz";
    let download = nfpm_problems(
        &in_job(&text, archive, "nfpm_${NFPM_VERSION}_Linux_arm64.tar.gz"),
        &artifacts,
    );
    assert_eq!(
        download,
        [
            "job openrc-device: no step downloads Some(\"nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz\"), the nfpm-archive of linux-x86_64 in artifacts.yml"
        ]
    );
    let unset = in_job(&text, "      NFPM_SHA256: ", "      OTHER_SHA256: ");
    assert_eq!(
        nfpm_problems(&unset, &artifacts),
        [
            "job openrc-device: NFPM_SHA256 is None, not Some(\"6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750\") of artifacts.yml"
        ]
    );
    let no_job = text.replace(HEAD, "\n  openrc-gone:\n");
    assert_eq!(nfpm_problems(&no_job, &artifacts), ["no job openrc-device"]);
    let no_entry = artifacts.replace("platform: linux-x86_64", "platform: linux-amd64");
    let want = "artifacts.yml has no NFPM_VERSION or no matrix entry for linux-x86_64";
    assert_eq!(nfpm_problems(&text, &no_entry), [want]);
    assert_eq!(nfpm_problems(&text, "name: x\n"), [want]);
    assert_eq!(nfpm_problems("on: [unclosed\n", &artifacts).len(), 1);
    assert_eq!(nfpm_problems(&text, "on: [unclosed\n").len(), 1);
}
