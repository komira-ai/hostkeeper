//! The pkg's `postinstall` text: the check that the daemon's job is loaded and the load of it
//! when it is not (never a boot-out), each path one shell word.

use std::path::PathBuf;

use hostkeeper_launchd::postinstall::{DAEMON_CONFIG, MANAGED_CONFIG, Postinstall};

const COMMENT: &str = "#!/bin/sh
\
# The hostkeeper pkg's postinstall: load the daemon's launchd job when launchd does
\
# not have it loaded, and leave it alone when it does. An installer the daemon
\
# itself runs (its self-update) must not boot the running daemon out, so the job is
\
# never booted out here; its new plist takes effect at the next boot. A first
\
# install, or a host where the job is not loaded, gets it loaded.
";

/// Catches a script for the pkg's paths that is not exactly the shebang, its comment, a quiet
/// `print` of the daemon's job that ends the script with 0 when it is loaded, and an `exec` of
/// the bootstrap of the installed plist (no boot-out, no seed and no `mac-config`, which the
/// daemon's start does).
#[test]
fn the_pkgs_postinstall_loads_the_daemons_job_only_when_it_is_not_loaded() {
    assert_eq!(
        Postinstall::pkg().script(),
        format!(
            "{COMMENT}'/bin/launchctl' print 'system/dev.hostkeeper.hostkeeper' >/dev/null 2>&1 \
             && exit 0\n\
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
    assert!(!script.contains("bootout"));
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
            "{COMMENT}'/l c' print 'system/dev.hostkeeper.hostkeeper' >/dev/null 2>&1 \
             && exit 0\n\
             exec '/l c' bootstrap system '/p'\\''l'\n"
        )
    );
}
