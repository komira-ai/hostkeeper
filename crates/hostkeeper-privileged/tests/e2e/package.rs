//! The tierstore-shaped package, built at test time as a `.deb` and ordered through its `apt`
//! entry.
//!
//! A package is the `.deb` (`dpkg-deb`, no maintainer script) of the service
//! [`SERVICE`]: the `tierstore-like` program of `tests/bin/tierstore_like.rs` at `usr/bin/`
//! (the run units' program) and again at the hook's fixed path (`usr/lib/<package>/hostkeeper-hook`,
//! which the daemon runs from the sealed tree), the release file of the version (its version and
//! its two run units), the unit template, and one plain unit file for each of the version's two
//! ports (a `run` unit of an entry is a file of the package), the unit text from
//! `fixtures/tierstore-shaped/` and a `{{placeholder}}` for the release file. This is the layout
//! of a production service's `.deb`; the test builds it itself so that each scenario can have
//! several versions without anything binary in the repository.
//!
//! The entry that orders a version ([`BuiltPackage::entry`]) is the `apt` entry of the package in
//! the scenario's repository: its pin, the two run units, the health URL with `{port}`, the
//! hook's bounds and the params the hook reads (`tierstore_like_record_dir`,
//! `tierstore_like_fail_verify`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_proto::hostkeeper::release::v1::{StepBounds, Target, TargetList};

use super::aptrepo::{self, AptRepo, Deb, File, Published, Service};
use super::{BINARY_NAME, REPOSITORY_NAME, SERVICE, UNIT_TEMPLATE};

/// The release file template; the placeholders are listed in `fixtures/tierstore-shaped/README.md`.
const RELEASE_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/tierstore-shaped/release.toml.tmpl"
));
/// The unit template of the run units, and the text of each instance's unit file.
const UNIT_TEMPLATE_TEXT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/tierstore-shaped/units/hostkeeper-ci-tierstore@.service.tmpl"
));

/// Where the units of a package are.
const UNIT_DIR: &str = "usr/lib/systemd/system";
/// Where the package keeps its own files.
const SHARE_DIR: &str = "usr/share/hostkeeper-ci-tierstore";
/// Mode of the programs.
const MODE_EXEC: u32 = 0o755;
/// Mode of the other files.
const MODE_FILE: u32 = 0o644;

/// One version of the fixture service.
#[derive(Clone, Copy, Debug)]
pub struct FixtureVersion {
    /// The version text, `[A-Za-z0-9._+-]`: the release's and the package's.
    pub version: &'static str,
    /// The ports of the two run instances (each version of a scenario has its own pair, so the
    /// old and the new release can run side by side during the cutover).
    pub ports: [u16; 2],
    /// Whether this version's `verify` step fails (`tierstore_like_fail_verify`).
    pub fail_verify: bool,
}

/// A built package and what an order for it needs.
#[derive(Clone, Debug)]
pub struct BuiltPackage {
    pub version: String,
    /// The package as published in the repository: its pin and its `.deb`.
    pub published: Published,
    /// 64 lowercase hex digits: the pin of the package's entry.
    pub sha256: String,
    pub ports: [u16; 2],
    /// `hostkeeper-ci-tierstore@<port>.service` for each port.
    pub run_units: Vec<String>,
    /// The entry that orders this version.
    pub entry: Target,
}

impl BuiltPackage {
    /// The `targets` of a set of this version: the one `apt` entry, for this host's platform.
    pub fn targets(&self) -> BTreeMap<String, TargetList> {
        aptrepo::targets(self.entry.clone())
    }
}

/// Builds the packages of one host's scenario into a repository.
#[derive(Clone, Debug)]
pub struct PackageBuilder {
    /// The directory the hook records into.
    record_dir: PathBuf,
    /// The built `tierstore-like` program.
    binary: PathBuf,
}

impl PackageBuilder {
    /// A builder for a host whose hook records into `record_dir`, and whose binary is the file
    /// `binary`.
    pub fn new(record_dir: &Path, binary: &Path) -> PackageBuilder {
        PackageBuilder {
            record_dir: record_dir.to_path_buf(),
            binary: binary.to_path_buf(),
        }
    }

    /// The package of `version`: its files, not yet in a repository.
    pub fn deb(&self, version: &FixtureVersion) -> Deb {
        let binary = fs::read(&self.binary).unwrap_or_else(|e| {
            panic!(
                "cannot read the fixture binary {}: {e}",
                self.binary.display()
            )
        });
        let ports = version.ports.map(|port| port.to_string());
        let release = render(
            RELEASE_TEMPLATE,
            &[
                ("service", SERVICE.to_owned()),
                ("version", version.version.to_owned()),
                ("port_a", ports[0].clone()),
                ("port_b", ports[1].clone()),
            ],
        );
        let mut files = vec![
            File::new(&format!("usr/bin/{BINARY_NAME}"), binary.clone(), MODE_EXEC),
            File::new(
                &format!("usr/lib/{SERVICE}/hostkeeper-hook"),
                binary,
                MODE_EXEC,
            ),
            File::new(
                &format!("{SHARE_DIR}/release.toml"),
                release.into_bytes(),
                MODE_FILE,
            ),
            File::new(
                &format!("{UNIT_DIR}/{UNIT_TEMPLATE}"),
                UNIT_TEMPLATE_TEXT,
                MODE_FILE,
            ),
        ];
        for port in &ports {
            files.push(File::new(
                &format!("{UNIT_DIR}/{SERVICE}@{port}.service"),
                UNIT_TEMPLATE_TEXT,
                MODE_FILE,
            ));
        }
        Deb {
            package: SERVICE.to_owned(),
            version: version.version.to_owned(),
            files,
        }
    }

    /// The entry that orders `published`, the package of `version`.
    pub fn entry(&self, version: &FixtureVersion, published: &Published) -> Target {
        let run_units: Vec<String> = version
            .ports
            .iter()
            .map(|port| format!("{SERVICE}@{port}.service"))
            .collect();
        let record_dir = self.record_dir.to_string_lossy().into_owned();
        let service = Service {
            repository: REPOSITORY_NAME.to_owned(),
            run: run_units,
            health_url: Some("http://127.0.0.1:{port}/healthz".to_owned()),
            params: [
                (
                    "tierstore_like_record_dir".to_owned(),
                    aptrepo::text(&record_dir),
                ),
                (
                    "tierstore_like_fail_verify".to_owned(),
                    aptrepo::flag(version.fail_verify),
                ),
            ]
            .into_iter()
            .collect(),
            hook_command: vec![format!("/usr/lib/{SERVICE}/hostkeeper-hook")],
            // prepare is unbounded: it waits on the drain gate for as long as the host is busy.
            steps: StepBounds {
                prepare: 0,
                cutover: 60,
                verify: 30,
                rollback: 60,
                cleanup: 30,
                ..StepBounds::default()
            },
            // At least the verify and the rollback bounds together (30 + 60 is less than this).
            verify_deadline_secs: 120,
        };
        aptrepo::entry(&service, published)
    }

    /// Builds the package of `version`, publishes it in `repo`, and gives it with its entry.
    pub fn build(&self, version: &FixtureVersion, repo: &AptRepo) -> BuiltPackage {
        let published = repo.publish(&self.deb(version));
        let entry = self.entry(version, &published);
        BuiltPackage {
            version: version.version.to_owned(),
            sha256: published.sha256.clone(),
            published,
            ports: version.ports,
            run_units: version
                .ports
                .iter()
                .map(|port| format!("{SERVICE}@{port}.service"))
                .collect(),
            entry,
        }
    }
}

/// Fills `{{name}}` for each `(name, value)`, and panics when a placeholder is left (a template
/// and its caller out of step).
fn render(template: &str, values: &[(&str, String)]) -> String {
    let mut text = template.to_owned();
    for (name, value) in values {
        text = text.replace(&format!("{{{{{name}}}}}"), value);
    }
    assert!(
        !text.contains("{{"),
        "a placeholder of the template was not filled:\n{text}"
    );
    text
}
