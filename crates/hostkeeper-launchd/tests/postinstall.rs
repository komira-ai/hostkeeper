//! The pkg's `postinstall` text: the seed, `mac-config` and the load of the daemon's job, each
//! path one shell word.

use std::path::PathBuf;

use hostkeeper_launchd::postinstall::{DAEMON_CONFIG, MANAGED_CONFIG, Postinstall};

const COMMENT: &str = "#!/bin/sh\n\
# The hostkeeper pkg's postinstall: three commands, in order. One that fails ends the\n\
# script with its status and its message, which fails the install.\n\
# 1. When current of the binary directory of hostkeeper's own update starts nothing,\n\
#    put the binary the pkg installed there and point current at it; when current\n\
#    starts a binary, as after a self-update, change nothing.\n\
# 2. Write the daemon config from the managed preferences the MDM installed. A config\n\
#    that is there is kept; with neither, fail.\n\
# 3. Load the daemon's launchd job, once any copy loaded before is booted out (which\n\
#    fails when none is loaded).\n";

/// Catches a script for the pkg's paths that is not exactly the shebang, its comment, the seed
/// and `mac-config` of the installed binary each ending the script when it fails, the boot-out of
/// the daemon's job whatever its status, and an `exec` of the bootstrap of the installed plist.
#[test]
fn the_pkgs_postinstall_seeds_writes_the_config_and_loads_the_daemons_job() {
    assert_eq!(
        Postinstall::pkg().script(),
        format!(
            "{COMMENT}'/usr/local/bin/hostkeeper' self-update-seed --state-dir \
             '/var/lib/updater/state' --binary '/usr/local/bin/hostkeeper' || exit\n\
             '/usr/local/bin/hostkeeper' mac-config --managed \
             '/Library/Managed Preferences/dev.hostkeeper.config.plist' --config \
             '/etc/hostkeeper/hostkeeper.toml' || exit\n\
             '/bin/launchctl' bootout 'system/dev.hostkeeper.hostkeeper' 2>/dev/null\n\
             exec '/bin/launchctl' bootstrap system \
             '/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist'\n"
        )
    );
}

/// Catches pkg paths that drift from the ones the module names.
#[test]
fn the_pkgs_paths_are_the_managed_preferences_and_the_daemon_config() {
    let pkg = Postinstall::pkg();
    assert_eq!(pkg.managed_config, PathBuf::from(MANAGED_CONFIG));
    assert_eq!(pkg.config, PathBuf::from(DAEMON_CONFIG));
}

/// Catches a path the shell would split at a space or end at a quote: each is one word in
/// single quotes, a quote in it written `'\''`.
#[test]
fn a_path_with_a_space_or_a_quote_is_one_shell_word() {
    let script = Postinstall {
        binary: PathBuf::from("/opt/it's here/hk"),
        state_dir: PathBuf::from("/s d"),
        managed_config: PathBuf::from("/m'p"),
        config: PathBuf::from("/c f"),
        launchctl: PathBuf::from("/l c"),
        plist: PathBuf::from("/p'l"),
    }
    .script();
    assert_eq!(
        script,
        format!(
            "{COMMENT}'/opt/it'\\''s here/hk' self-update-seed --state-dir '/s d' --binary \
             '/opt/it'\\''s here/hk' || exit\n\
             '/opt/it'\\''s here/hk' mac-config --managed '/m'\\''p' --config '/c f' || exit\n\
             '/l c' bootout 'system/dev.hostkeeper.hostkeeper' 2>/dev/null\n\
             exec '/l c' bootstrap system '/p'\\''l'\n"
        )
    );
}
