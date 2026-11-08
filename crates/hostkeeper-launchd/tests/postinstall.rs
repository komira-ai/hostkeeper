//! The pkg's `postinstall` text: the boot-out and the load of the daemon's job, each path one
//! shell word.

use std::path::PathBuf;

use hostkeeper_launchd::postinstall::{DAEMON_CONFIG, MANAGED_CONFIG, Postinstall};

const COMMENT: &str = "#!/bin/sh\n\
# The hostkeeper pkg's postinstall: load the daemon's launchd job, once any copy\n\
# loaded before is booted out (which fails when none is loaded). The job's program,\n\
# the binary the pkg installed, puts itself in the binary directory of hostkeeper's\n\
# own update when current starts nothing and runs current, at each start.\n";

/// Catches a script for the pkg's paths that is not exactly the shebang, its comment, the
/// boot-out of the daemon's job whatever its status, and an `exec` of the bootstrap of the
/// installed plist (no seed and no `mac-config`, which the daemon's start does).
#[test]
fn the_pkgs_postinstall_boots_out_then_loads_the_daemons_job() {
    assert_eq!(
        Postinstall::pkg().script(),
        format!(
            "{COMMENT}'/bin/launchctl' bootout 'system/dev.hostkeeper.hostkeeper' 2>/dev/null\n\
             exec '/bin/launchctl' bootstrap system \
             '/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist'\n"
        )
    );
}

/// Catches a postinstall that runs the installed binary at install: the seed and the config are
/// the daemon's start, so the script names neither verb.
#[test]
fn the_pkgs_postinstall_runs_no_seed_and_no_mac_config() {
    let script = Postinstall::pkg().script();
    assert!(!script.contains("self-update-seed"));
    assert!(!script.contains("mac-config"));
    assert!(!script.contains("/usr/local/bin/hostkeeper"));
}

/// Catches path constants that drift from the ones the module names.
#[test]
fn the_daemon_config_and_managed_preferences_are_the_paths_the_start_uses() {
    assert_eq!(DAEMON_CONFIG, "/etc/hostkeeper/hostkeeper.toml");
    assert_eq!(
        PathBuf::from(MANAGED_CONFIG),
        PathBuf::from("/Library/Managed Preferences/dev.hostkeeper.config.plist")
    );
}

/// Catches a path the shell would split at a space or end at a quote: each is one word in
/// single quotes, a quote in it written `'\''`.
#[test]
fn a_path_with_a_space_or_a_quote_is_one_shell_word() {
    let script = Postinstall {
        launchctl: PathBuf::from("/l c"),
        plist: PathBuf::from("/p'l"),
    }
    .script();
    assert_eq!(
        script,
        format!(
            "{COMMENT}'/l c' bootout 'system/dev.hostkeeper.hostkeeper' 2>/dev/null\n\
             exec '/l c' bootstrap system '/p'\\''l'\n"
        )
    );
}
