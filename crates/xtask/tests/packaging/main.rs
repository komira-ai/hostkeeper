//! The packaging of hostkeeper: in `packaging/linux/`, the unit, the tmpfiles rules, the config
//! template, `nfpm.yaml`, the package scripts and the boot-image overlay, with the two units of
//! the daemon (the bootstrap unit the deb and the rpm install, and the package unit hostkeeper's
//! own package carries); in `packaging/alpine/`, the apk's `nfpm.yaml`, the OpenRC init script
//! and the apk's scripts, each held to the Linux file it follows; and the coordinator's container
//! build file, `packaging/container/Dockerfile`. Each is read from the committed file and held to
//! docs/design/hostkeeper-design.md (sections 4.1, 4.6, 6.5, 6.6, 10 and 12.3). Each check is a
//! function of the texts, and each is also shown to refuse its defect on an altered text.
//! Nothing here runs systemd, OpenRC, nfpm, a package manager or a container build.

mod alpine;
mod attestation_files;
mod config_template;
mod container;
mod daemon_flags;
mod directories;
mod nfpm;
mod overlay;
mod package;
mod package_names;
mod script;
mod self_update;
mod template;
#[path = "../../src/testdir.rs"]
mod testdir;
mod tmpfiles;
mod two_units;
mod unit;
mod unit_file;
#[path = "../workflows/yaml.rs"]
#[allow(
    dead_code,
    reason = "the workflow lint's YAML tree; this test reads only part of it"
)]
mod yaml;

use std::fs;
use std::path::PathBuf;

use nfpm::Config;

/// The bootstrap unit, from `packaging/linux`: the deb and the rpm install it, and the overlay
/// holds a copy.
const UNIT: &str = "hostkeeper.service";
/// The package unit, from `packaging/linux`: hostkeeper's own package carries it.
const PACKAGE_UNIT: &str = "units/hostkeeper.service";
const TMPFILES: &str = "hostkeeper.tmpfiles.conf";
const TEMPLATE: &str = "hostkeeper.toml.template";
const NFPM: &str = "nfpm.yaml";
const POSTINSTALL: &str = "scripts/postinstall.sh";
const PREREMOVE: &str = "scripts/preremove.sh";
const INSTALLED_UNIT: &str = "/usr/lib/systemd/system/hostkeeper.service";

fn linux() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux")
}

/// The text of `packaging/linux/<relative>`.
fn read(relative: &str) -> String {
    fs::read_to_string(linux().join(relative))
        .unwrap_or_else(|e| panic!("read packaging/linux/{relative}: {e}"))
}

/// `text` with its one occurrence of `from` replaced by `to`. Panics when `from` is not in it
/// exactly once, so a case cannot pass by changing nothing.
fn altered(text: &str, from: &str, to: &str) -> String {
    assert_eq!(
        text.matches(from).count(),
        1,
        "{from:?} is not in the text once"
    );
    text.replacen(from, to, 1)
}

/// Asserts that `check` refuses each altered text with a message holding its words.
fn refuses(check: impl Fn(&str) -> Result<(), String>, cases: &[(String, &str)]) {
    for (text, words) in cases {
        match check(text.as_str()) {
            Ok(()) => panic!("accepted a text that must be refused for {words:?}"),
            Err(message) => assert!(message.contains(words), "{message:?} lacks {words:?}"),
        }
    }
}

/// The package unit's seed line, with its newline.
const SEED_LINE: &str = "ExecStartPre=-/usr/bin/hostkeeper self-update-seed --state-dir /var/lib/updater/state --binary /usr/bin/hostkeeper\n";

/// One of the daemon's two units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The unit the deb, the rpm and a boot image install: it starts the installed binary.
    Bootstrap,
    /// The unit hostkeeper's own package carries: it seeds, then starts `current`.
    Package,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Bootstrap, Kind::Package];

    /// The unit's path from `packaging/linux`.
    fn path(self) -> &'static str {
        match self {
            Kind::Bootstrap => UNIT,
            Kind::Package => PACKAGE_UNIT,
        }
    }

    /// The unit's text.
    fn read(self) -> String {
        read(self.path())
    }

    /// The program the unit's `ExecStart=` runs.
    fn program(self) -> &'static str {
        match self {
            Kind::Bootstrap => "/usr/bin/hostkeeper",
            Kind::Package => "/var/lib/updater/state/hostkeeper/bin/current",
        }
    }
}

fn config() -> Config {
    Config::parse(&read(NFPM)).expect("nfpm.yaml parses")
}
