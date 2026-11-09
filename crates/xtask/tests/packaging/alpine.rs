//! The Alpine package: `packaging/alpine/nfpm.yaml` (`package`), the OpenRC init script
//! (`initd`) and the two apk scripts (`scripts`). The package carries the files of the Linux
//! packages that are not the systemd host's, the init script runs the flags of the systemd unit
//! and makes the directories of the tmpfiles rules, so each is held to the Linux file it follows.

mod initd;
mod package;
mod scripts;

use std::path::PathBuf;

const APK_NFPM: &str = "nfpm.yaml";
const INITD: &str = "hostkeeper.initd";
const APK_POSTINSTALL: &str = "scripts/postinstall.sh";
const APK_PREREMOVE: &str = "scripts/preremove.sh";

/// The dependency of the apk: OpenRC 0.62 (Alpine 3.22), the oldest the device tests run the
/// package on.
const DEPENDS: [&str; 1] = ["openrc>=0.62"];

fn alpine() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/alpine")
}

/// The text of `packaging/alpine/<relative>`.
fn read_alpine(relative: &str) -> String {
    std::fs::read_to_string(alpine().join(relative))
        .unwrap_or_else(|e| panic!("read packaging/alpine/{relative}: {e}"))
}
