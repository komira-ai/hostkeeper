//! `hostkeeper-coordinator serve` with the MDM policy gate and the Macs' listener: the usage
//! lists the listener's flags, and the baseline flags go with the side and the allowlist and
//! the baseline file must be root's. The files and flags are those of `tests/mdm.rs`, shared
//! through `support::mdm`.

mod support;

use std::process::ExitCode;

use clap::Parser;
use hostkeeper_coordinator::config::Cli;
use support::mdm::{DAEMON_CONFIG, Files, IDENTIFIER, flags_of, good, put_build_group, refusal};

/// Catches: the new flags missing from the usage, or read from the environment.
#[test]
fn serve_help_lists_the_macs_listener_flags() {
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    for flag in [
        "--mdm-listen <MDM_LISTEN>",
        "--mdm-command-timeout-s <MDM_COMMAND_TIMEOUT_S>",
        "--mdm-queue-sweep-s <MDM_QUEUE_SWEEP_S>",
        "--mdm-pkg-dir <MDM_PKG_DIR>",
    ] {
        assert!(help.contains(flag), "{flag}: {help}");
    }
}

/// Catches: a baseline flag accepted without its partners (`--mdm-baseline-file` without
/// `--mdm-profile-identifier` or without `--mac-serial-allowlist`, the check interval without
/// the file, an interval of 0), a baseline file that is not root's let through the start (it
/// says what every Mac is configured with), and a default check interval other than six hours.
#[tokio::test]
async fn the_baseline_flags_go_with_the_side_and_the_allowlist_and_the_file_must_be_roots() {
    let files = Files::new("mdm-baseline-flags");
    put_build_group(&files).await;
    files.put("hostkeeper.toml", DAEMON_CONFIG, 0o644);
    files.put(
        "mac.toml",
        "[[mac]]\nserial = \"C02XK1ABJG5J\"\nhost_id_prefix = \"mac-\"\nrole = \"build\"\n",
        0o600,
    );
    files.put(
        "baseline.toml",
        "[[group]]\nhost_group = \"build\"\n[[group.entry]]\nremote_desktop = true\n",
        0o644,
    );
    let side = [
        "--mdm-profile-identifier",
        IDENTIFIER,
        "--mdm-daemon-config",
        "=hostkeeper.toml",
    ];
    let allowlist = ["--mac-serial-allowlist", "=mac.toml"];
    let baseline = ["--mdm-baseline-file", "=baseline.toml"];
    let refused: [Vec<&str>; 4] = [
        good()
            .into_iter()
            .chain(allowlist)
            .chain(baseline)
            .collect(),
        good().into_iter().chain(side).chain(baseline).collect(),
        good()
            .into_iter()
            .chain(side)
            .chain(allowlist)
            .chain(["--mdm-baseline-check-s", "60"])
            .collect(),
        good()
            .into_iter()
            .chain(side)
            .chain(allowlist)
            .chain(baseline)
            .chain(["--mdm-baseline-check-s", "0"])
            .collect(),
    ];
    for args in refused {
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&args)),
            ExitCode::from(2),
            "{args:?}"
        );
    }
    let all: Vec<&str> = good()
        .into_iter()
        .chain(side)
        .chain(allowlist)
        .chain(baseline)
        .collect();
    assert_eq!(flags_of(files.args(&all)).mdm.mdm_baseline_check_s, 21_600);
    assert_eq!(
        refusal(&files, &all),
        format!(
            "--mdm-baseline-file {}: it is not owned by root; the baseline and its profiles must be files of root's that no one else can write",
            files.path("baseline.toml").display()
        )
    );
}
