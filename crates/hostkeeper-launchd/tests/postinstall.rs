//! The pkg's `postinstall` text: one `exec` of the seed, run by the binary it seeds from, each path
//! one shell word.

use std::path::Path;

use hostkeeper_launchd::plist::{DAEMON_STATE_DIR, INSTALLED_BINARY};
use hostkeeper_launchd::postinstall::postinstall;

const COMMENT: &str = "#!/bin/sh\n\
# The hostkeeper pkg's postinstall. The daemon's launchd job starts current of the binary\n\
# directory of hostkeeper's own update; when current starts nothing, this puts the binary\n\
# the pkg installed there and points current at it. When current starts a binary, as\n\
# after a self-update, it changes nothing.\n";

/// Catches a script for the pkg's paths that is not exactly the shebang, its comment and one
/// `exec` of the installed binary's seed of the daemon's state directory.
#[test]
fn the_pkgs_postinstall_execs_the_installed_binarys_seed_of_the_daemons_state_directory() {
    assert_eq!(
        postinstall(Path::new(INSTALLED_BINARY), Path::new(DAEMON_STATE_DIR)),
        format!(
            "{COMMENT}exec '/usr/local/bin/hostkeeper' self-update-seed --state-dir \
             '/var/lib/updater/state' --binary '/usr/local/bin/hostkeeper'\n"
        )
    );
}

/// Catches a path the shell would split at a space or end at a quote: each is one word in
/// single quotes, a quote in it written `'\''`.
#[test]
fn a_path_with_a_space_or_a_quote_is_one_shell_word() {
    assert_eq!(
        postinstall(Path::new("/opt/it's here/hk"), Path::new("/s d")),
        format!(
            "{COMMENT}exec '/opt/it'\\''s here/hk' self-update-seed --state-dir '/s d' --binary \
             '/opt/it'\\''s here/hk'\n"
        )
    );
}
