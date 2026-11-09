//! The cross-release rig's target, which runs on both platforms, held to both jobs of
//! privileged.yml that run it (the shape check of `privileged_tests.rs`).

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::lint::{MACOS_DEVICE_JOB, PRIVILEGED_WORKFLOW};
use super::privileged_tests::{
    BOTH_FIRST_LINE, committed_targets, privileged_targets, shape_problems,
};

#[test]
fn a_target_of_both_platforms_left_out_of_either_job_is_refused() {
    // Catches: the cross-release rig, whose one target runs on Linux and on a Mac, dropped from
    // the macOS job's steps or from the Linux job's while the other still runs it, or a first
    // line of both platforms read as one job's only.
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/privileged.yml"),
    )
    .expect("read privileged.yml");
    let targets = committed_targets();
    assert!(targets["privileged"].contains("self_update_across_releases"));
    assert!(targets[MACOS_DEVICE_JOB].contains("self_update_across_releases"));
    assert!(
        privileged_targets(BOTH_FIRST_LINE).contains("self_update_across_releases"),
        "its file starts with the first line of both platforms"
    );
    let set = |names: &[&str]| -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    };
    let mac = set(&[
        "device_mac",
        "device_mac_e2e",
        "device_mac_selfupdate",
        "install_macos_pkg",
        "self_update_across_releases",
        "unpack_mac",
    ]);
    let mac_left = set(&[
        "device_mac",
        "device_mac_e2e",
        "device_mac_selfupdate",
        "install_macos_pkg",
        "unpack_mac",
    ]);
    let dropped_mac = text.replace(
        "--test device_mac_selfupdate --test install_macos_pkg --test self_update_across_releases --test unpack_mac",
        "--test device_mac_selfupdate --test install_macos_pkg --test unpack_mac",
    );
    assert_ne!(dropped_mac, text);
    assert_eq!(
        shape_problems(PRIVILEGED_WORKFLOW, &dropped_mac, &targets),
        [
            format!(
                "job macos-device: the build step names the test targets {mac_left:?}, not {mac:?}"
            ),
            format!(
                "job macos-device: the run as root step names the test targets {mac_left:?}, not {mac:?}"
            ),
        ]
    );
    let linux = &targets["privileged"];
    let mut linux_left = linux.clone();
    linux_left.remove("self_update_across_releases");
    let dropped_linux = text.replace(
        "--test tierstore_shaped --test self_update_across_releases",
        "--test tierstore_shaped",
    );
    assert_ne!(dropped_linux, text);
    assert_eq!(
        shape_problems(PRIVILEGED_WORKFLOW, &dropped_linux, &targets),
        [
            format!(
                "job privileged: the build step names the test targets {linux_left:?}, not {linux:?}"
            ),
            format!(
                "job privileged: the run as root step names the test targets {linux_left:?}, not {linux:?}"
            ),
        ]
    );
}
