//! The `postinstall` script of the macOS installer pkg. The daemon's launchd job starts `current`
//! of the binary directory of hostkeeper's own update ([`crate::plist::DAEMON_PROGRAM`]), and
//! launchd has no step before a start (the Linux unit's `ExecStartPre=`), so the pkg gives that
//! directory its first binary at install: the script runs the installed binary's
//! `self-update-seed`, which points `current` at that binary when `current` starts nothing and
//! changes nothing otherwise.
//!
//! The installer runs the script as root once the payload is in place, with the path of the pkg,
//! the install location, the target volume and the root of the volume as its arguments, which
//! the script ignores. The script is one command after its `#!/bin/sh` line and comments, `exec`
//! of the seed, so its exit status is the seed's: 0 when `current` starts a binary, and the
//! seed's own status with its message on standard error when it could not make it so, which
//! fails the install.

use std::path::Path;

/// The text of the script that seeds the binary directory under `state_dir` from `binary`, run
/// by `binary` itself: the pkg's is [`postinstall`] of [`crate::plist::INSTALLED_BINARY`] and
/// [`crate::plist::DAEMON_STATE_DIR`]. Each path is quoted for the shell, so any UTF-8 path is
/// passed as one word, unchanged.
pub fn postinstall(binary: &Path, state_dir: &Path) -> String {
    let binary = quoted(binary);
    let state_dir = quoted(state_dir);
    format!(
        "#!/bin/sh\n\
         # The hostkeeper pkg's postinstall. The daemon's launchd job starts current of the binary\n\
         # directory of hostkeeper's own update; when current starts nothing, this puts the binary\n\
         # the pkg installed there and points current at it. When current starts a binary, as\n\
         # after a self-update, it changes nothing.\n\
         exec {binary} self-update-seed --state-dir {state_dir} --binary {binary}\n"
    )
}

/// `path` as one word of a POSIX shell: in single quotes, each `'` in it written `'\''`. Bytes
/// that are not UTF-8 are shown as U+FFFD.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}
