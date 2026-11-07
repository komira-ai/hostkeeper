use std::path::{Path, PathBuf};

use super::*;
use crate::testdir::TestDir;

/// An empty directory of its own for one test, removed when it drops.
fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-pr-jobs-unit"),
        name,
    )
}

/// The paths komira-ai/hostkeeper#133 changed (the launchd job started the self-update binary
/// directory's `current`), one per line.
const PR_133: &str = "\
.github/workflows/artifacts.yml
Cargo.lock
crates/hostkeeper-daemon/src/cli.rs
crates/hostkeeper-daemon/src/lib.rs
crates/hostkeeper-daemon/src/seed.rs
crates/hostkeeper-launchd/src/lib.rs
crates/hostkeeper-launchd/src/plist.rs
crates/hostkeeper-launchd/src/postinstall.rs
crates/hostkeeper-launchd/tests/golden/daemon.plist
crates/hostkeeper-launchd/tests/plist.rs
crates/hostkeeper-launchd/tests/postinstall.rs
crates/hostkeeper-launchd/tests/sources.rs
crates/hostkeeper/Cargo.toml
crates/hostkeeper/tests/postinstall.rs
crates/xtask/src/release.rs
crates/xtask/src/release/fakes.rs
crates/xtask/src/release/macos_pkg.rs
crates/xtask/src/release/macos_pkg/check_tests.rs
crates/xtask/src/release/macos_pkg/scripts.rs
crates/xtask/src/release/macos_pkg/tests.rs
crates/xtask/src/release/macos_pkg/tests/self_update.rs
crates/xtask/src/release/macos_pkg_verb_tests.rs
crates/xtask/src/release/ports.rs
crates/xtask/tests/layering/check.rs
docs/deploy/verify-a-release.md
docs/design/hostkeeper-design.md
docs/macos.md
docs/operations/self-update-after-bootstrap.md
";

/// The paths komira-ai/hostkeeper#169 changed (the Mac self-update probe as a launchd job).
const PR_169: &str = "\
.github/workflows/privileged.yml
README.md
crates/hostkeeper-daemon/src/selfupdate.rs
crates/hostkeeper-daemon/tests/selfupdate_install.rs
crates/hostkeeper-daemon/tests/selfupdate_mac_probe.rs
crates/hostkeeper-launchd/src/label.rs
crates/hostkeeper-launchd/src/launcher.rs
crates/hostkeeper-launchd/src/lib.rs
crates/hostkeeper-launchd/tests/equivalence.rs
crates/hostkeeper-launchd/tests/label.rs
crates/hostkeeper-launchd/tests/launcher.rs
crates/hostkeeper-launchd/tests/launcher_probe.rs
crates/hostkeeper-privileged/src/lib.rs
crates/hostkeeper-privileged/tests/bin/selfupdate_daemon.rs
crates/hostkeeper-privileged/tests/device_mac.rs
crates/hostkeeper-privileged/tests/device_mac_selfupdate.rs
crates/hostkeeper-privileged/tests/mac_self_update/mod.rs
crates/hostkeeper-steps/src/systemd.rs
crates/xtask/tests/workflows/lint.rs
crates/xtask/tests/workflows/privileged_tests.rs
docs/macos.md
docs/operations/self-update-after-bootstrap.md
docs/report/hostkeeper.html
";

/// The paths komira-ai/hostkeeper#170 changed (orders past a try refusal, the step-record try
/// in every heartbeat): the coordinator, the link and the wire, and of the daemon only its link
/// adapters and tests.
const PR_170: &str = "\
crates/hostkeeper-coordinator/src/agent_service.rs
crates/hostkeeper-coordinator/src/fleet.rs
crates/hostkeeper-coordinator/src/grpc/beat_map.rs
crates/hostkeeper-coordinator/src/lib.rs
crates/hostkeeper-daemon/src/link_adapters.rs
crates/hostkeeper-daemon/tests/end_to_end.rs
crates/hostkeeper-link/src/beat.rs
crates/hostkeeper-link/src/heartbeater.rs
crates/hostkeeper-privileged/tests/install_uninstall.rs
crates/hostkeeper-privileged/tests/support/pair.rs
crates/hostkeeper-proto/proto/hostkeeper/agent/v1/agent.proto
crates/hostkeeper-wire/src/wire.rs
docs/design/hostkeeper-design.md
";

/// [`pr_jobs`] over `changed`, written to a file of `dir`, and what it appended to an output
/// file that held `before`.
fn run_in(dir: &Path, changed: &str, before: &str) -> (Vec<String>, String) {
    let changed_file = dir.join("changed");
    std::fs::write(&changed_file, changed).expect("write the changed paths");
    let output = dir.join("output");
    std::fs::write(&output, before).expect("write the output file");
    let lines = pr_jobs(&changed_file, &output).expect("pr-jobs");
    let text = std::fs::read_to_string(&output).expect("read the output file");
    (lines, text)
}

#[test]
fn the_changes_that_broke_the_mac_path_after_merge_select_the_macos_jobs() {
    // Catches: a Mac list that misses the launchd adapters, the Mac packaging, the self-update
    // probe or the daemon, each of which a change that reached main broken on the Mac touched.
    for (pr, changed) in [("133", PR_133), ("169", PR_169), ("170", PR_170)] {
        assert!(!on_list(MAC, changed).is_empty(), "#{pr}");
    }
    assert_eq!(
        on_list(MAC, PR_170),
        [
            "crates/hostkeeper-daemon/src/link_adapters.rs",
            "crates/hostkeeper-daemon/tests/end_to_end.rs",
            "crates/hostkeeper-privileged/tests/install_uninstall.rs",
            "crates/hostkeeper-privileged/tests/support/pair.rs",
        ]
    );
    // #169 changed the systemd adapter and self-update too: the privileged job on Linux runs.
    assert_eq!(
        on_list(LINUX_PRIVILEGED, PR_169),
        [
            ".github/workflows/privileged.yml",
            "crates/hostkeeper-daemon/src/selfupdate.rs",
            "crates/hostkeeper-daemon/tests/selfupdate_install.rs",
            "crates/hostkeeper-daemon/tests/selfupdate_mac_probe.rs",
            "crates/hostkeeper-privileged/src/lib.rs",
            "crates/hostkeeper-privileged/tests/bin/selfupdate_daemon.rs",
            "crates/hostkeeper-privileged/tests/device_mac.rs",
            "crates/hostkeeper-privileged/tests/device_mac_selfupdate.rs",
            "crates/hostkeeper-privileged/tests/mac_self_update/mod.rs",
            "crates/hostkeeper-steps/src/systemd.rs",
        ]
    );
}

#[test]
fn a_change_off_both_lists_selects_no_job() {
    // Catches: a list so wide that a pull request of docs, of the coordinator, of the MDM server,
    // of a dependency bump or of the Linux-only CI queues a macOS runner, and a crate whose name
    // starts with a listed crate's (`hostkeeper-mdm` against `hostkeeper`) matching it.
    let changed = "\
docs/macos.md
README.md
Cargo.lock
Cargo.toml
.github/workflows/ci.yml
crates/hostkeeper-coordinator/src/lib.rs
crates/hostkeeper-link/src/beat.rs
crates/hostkeeper-mdm/src/lib.rs
crates/hostkeeper-config/src/lib.rs
crates/hostkeeper-package/src/fetch.rs
crates/hostkeeper-daemonx/src/lib.rs
crates/xtask/src/release.rs
crates/xtask/tests/workflows/lint.rs
packaging/container/Dockerfile
";
    assert_eq!(on_list(MAC, changed), Vec::<&str>::new());
    assert_eq!(on_list(LINUX_PRIVILEGED, changed), Vec::<&str>::new());
}

#[test]
fn each_list_selects_its_own_surface() {
    // Catches: the Linux list running the privileged job for a launchd-only change, the Mac list
    // missing the step launcher, or either missing an entry that names a file by its stem.
    let launchd = "crates/hostkeeper-launchd/src/plist.rs\n";
    assert_eq!(on_list(MAC, launchd), [launchd.trim_end()]);
    assert_eq!(on_list(LINUX_PRIVILEGED, launchd), Vec::<&str>::new());
    let unit = "packaging/linux/hostkeeper.service\n";
    assert_eq!(on_list(MAC, unit), Vec::<&str>::new());
    assert_eq!(on_list(LINUX_PRIVILEGED, unit), [unit.trim_end()]);
    for both in [
        "crates/hostkeeper-steps/src/launcher.rs",
        "crates/hostkeeper-daemon/src/executor.rs",
        "crates/xtask/src/pr_jobs.rs",
        "crates/xtask/src/pr_jobs/tests.rs",
        ".github/workflows/pr-device.yml",
        "rust-toolchain.toml",
        ".cargo/config.toml",
    ] {
        assert_eq!(on_list(MAC, both), [both], "{both}");
        assert_eq!(on_list(LINUX_PRIVILEGED, both), [both], "{both}");
    }
    for stem in [
        "crates/xtask/src/release/macos_pkg.rs",
        "crates/xtask/src/release/macos_pkg/scripts.rs",
        "crates/xtask/src/release/macos_pkg_verb_tests.rs",
        "crates/hostkeeper-config/src/platform.rs",
        "crates/hostkeeper-package/src/index.rs",
        "crates/hostkeeper-package/src/index_tests.rs",
        "examples/hello/src/main.rs",
    ] {
        assert_eq!(on_list(MAC, stem), [stem], "{stem}");
    }
}

#[test]
fn paths_split_on_nul_and_newline_and_empty_entries_are_skipped() {
    // Catches: the NUL-separated list of `git diff -z` read as one path, so a Mac path after the
    // first entry selects nothing; and an empty list or empty entries matching.
    let nul = "docs/a.md\0crates/hostkeeper-launchd/src/lib.rs\0\0";
    assert_eq!(on_list(MAC, nul), ["crates/hostkeeper-launchd/src/lib.rs"]);
    let mixed = "\n\ndocs/a.md\ncrates/hostkeeper-steps/src/systemd.rs\0";
    assert_eq!(
        on_list(LINUX_PRIVILEGED, mixed),
        ["crates/hostkeeper-steps/src/systemd.rs"]
    );
    for empty in ["", "\0", "\n\n", "\0\n\0"] {
        assert_eq!(on_list(MAC, empty), Vec::<&str>::new(), "{empty:?}");
    }
}

/// Every file of the repository, by its path from the root with `/`, but those under the root's
/// `target` and `.git`.
fn repository_files() -> Vec<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found = Vec::new();
    let mut pending = vec![String::new()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&dir)).expect("list a directory") {
            let entry = entry.expect("an entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if dir.is_empty() && (name == "target" || name == ".git") {
                continue;
            }
            let path = format!("{dir}{name}");
            if entry.file_type().expect("a file type").is_dir() {
                pending.push(format!("{path}/"));
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn every_entry_of_both_lists_names_a_path_of_the_repository_and_the_lists_are_sorted() {
    // Catches: an entry left behind by a rename or a deletion, which would match nothing and
    // let changes to the moved code run without the jobs; a duplicate entry; and an unsorted list
    // that hides one in review.
    let files = repository_files();
    for (name, list) in [("mac", MAC), ("linux-privileged", LINUX_PRIVILEGED)] {
        for entry in list {
            assert!(
                files.iter().any(|f| f.starts_with(entry)),
                "{name}: {entry} names no file of the repository"
            );
        }
        let mut sorted = list.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted,
            list.to_vec(),
            "{name} is not sorted, or has a duplicate"
        );
    }
}

#[test]
fn pr_jobs_appends_both_outputs_and_says_why() {
    // Catches: an output written over what the file held (on Actions it holds the outputs of
    // earlier steps), an output name the workflow does not read, a missing newline that runs two
    // outputs into one, and a log line that does not name the paths that selected the jobs.
    let dir = scratch("both");
    let (lines, text) = run_in(
        &dir,
        "docs/macos.md\0crates/hostkeeper-launchd/src/plist.rs\0examples/hello/Cargo.toml\0",
        "earlier=1\n",
    );
    assert_eq!(text, "earlier=1\nmac=true\nlinux-privileged=false\n");
    assert_eq!(
        lines,
        [
            "mac: true, for crates/hostkeeper-launchd/src/plist.rs, examples/hello/Cargo.toml",
            "linux-privileged: false, no changed path is on its list",
        ]
    );
    let dir = scratch("none");
    let (lines, text) = run_in(&dir, "", "");
    assert_eq!(text, "mac=false\nlinux-privileged=false\n");
    assert_eq!(
        lines,
        [
            "mac: false, no changed path is on its list",
            "linux-privileged: false, no changed path is on its list",
        ]
    );
    let dir = scratch("linux");
    let (_, text) = run_in(&dir, "packaging/linux/hostkeeper.service\n", "");
    assert_eq!(text, "mac=false\nlinux-privileged=true\n");
}

#[test]
fn pr_jobs_creates_a_missing_output_file() {
    // Catches: a run outside Actions (no output file yet) failing instead of writing one.
    let dir = scratch("create");
    let changed = dir.join("changed");
    std::fs::write(&changed, "crates/hostkeeper-steps/src/systemd.rs").expect("write");
    let output = dir.join("new-output");
    pr_jobs(&changed, &output).expect("pr-jobs");
    assert_eq!(
        std::fs::read_to_string(&output).expect("read"),
        "mac=true\nlinux-privileged=true\n"
    );
}

#[test]
fn an_unreadable_changed_list_or_output_is_an_error_naming_the_file() {
    // Catches: a missing list read as "nothing changed", which would skip every job of a pull
    // request whose diff step failed; and an output that cannot be written passing silently.
    let dir = scratch("errors");
    let missing = dir.join("missing");
    let output = dir.join("output");
    let e = pr_jobs(&missing, &output).expect_err("a missing list");
    assert_eq!(
        e.to_string(),
        format!("read the changed paths {missing:?}: No such file or directory (os error 2)")
    );
    assert!(
        !output.exists(),
        "nothing is written when the list is missing"
    );
    let changed = dir.join("changed");
    std::fs::write(&changed, "README.md\n").expect("write");
    let e = pr_jobs(&changed, &dir).expect_err("a directory as the output");
    assert_eq!(
        e.to_string(),
        format!(
            "append the outputs to {:?}: Is a directory (os error 21)",
            &*dir
        )
    );
}
