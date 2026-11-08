#![cfg(feature = "privileged")]
//! The same deb, rpm and Arch package installed and run on the oldest and newest release of each
//! family (docs/design/packages.md section 5.4, "One build, every release").
//!
//! The job builds the three packages into `target/linux-packages` from the programs staged in
//! `target/nfpm` (`hostkeeper`, `hostkeeper-pxe`, `gh`), and pulls each image of
//! `linux-releases.toml` in a step of its own. For each release the test runs a container of its
//! image, with no pull and no network and the packages directory mounted read only, which
//! installs the release's package with the format's own tool (where the image has no `systemctl`
//! or `systemd-tmpfiles`, after putting a stand-in for it: no container runs a service manager)
//! and runs `/usr/bin/hostkeeper --version`, `/usr/bin/hostkeeper-pxe --version` and
//! `/usr/lib/hostkeeper/gh --version`. The run must exit 0 and print what the same staged
//! programs print on the runner, so no version is written here. The containers are removed when
//! they end (`--rm`) and named for this run. The test writes nothing on the host.

use std::path::{Path, PathBuf};
use std::process::Command;

use hostkeeper_privileged::linux_packages::{
    PACKAGES_DIR, PROGRAMS, RELEASES_TEXT, STAGED_DIR, container_name, docker_args, find_packages,
    parse_releases,
};

/// The target directory the job built into: the parent of cargo's scratch directory for tests.
fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("the target directory")
        .to_path_buf()
}

/// What `--version` of the staged program `file` prints on the runner.
fn staged_version(staged: &Path, file: &str) -> Vec<u8> {
    let program = staged.join(file);
    let out = Command::new(&program)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("run {} --version: {e}", program.display()));
    assert!(
        out.status.success(),
        "{} --version failed: {:?}",
        program.display(),
        out.status
    );
    out.stdout
}

#[test]
fn every_release_installs_the_packages_and_runs_their_programs() {
    let docker = Command::new("docker").arg("--version").output();
    assert!(
        docker.is_ok_and(|out| out.status.success()),
        "docker is missing or does not run: the job's runner has it, and this test does not skip"
    );
    let releases = parse_releases(RELEASES_TEXT).unwrap_or_else(|e| panic!("{e}"));
    let target = target_dir();
    let packages = find_packages(&target.join(PACKAGES_DIR)).unwrap_or_else(|e| panic!("{e}"));
    let staged = target.join(STAGED_DIR);
    let expected: Vec<u8> = PROGRAMS
        .iter()
        .flat_map(|(_, file)| staged_version(&staged, file))
        .collect();
    assert!(!expected.is_empty(), "the staged programs print no version");

    let mut failures = Vec::new();
    for (index, release) in releases.iter().enumerate() {
        let name = container_name(std::process::id(), index);
        let out = Command::new("docker")
            .args(docker_args(&name, release, &packages))
            .output()
            .unwrap_or_else(|e| panic!("run docker for {}: {e}", release.name));
        println!(
            "{} ({}): {:?}\n{}{}",
            release.name,
            release.image,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            failures.push(format!(
                "{}: the container exited {:?}",
                release.name, out.status
            ));
        } else if out.stdout != expected {
            failures.push(format!(
                "{}: the programs printed {:?}, not what the staged programs print, {:?}",
                release.name,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&expected)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "releases that did not install and run the packages:\n{}",
        failures.join("\n")
    );
}
