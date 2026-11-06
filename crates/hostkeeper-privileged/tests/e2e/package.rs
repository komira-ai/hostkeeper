//! The tierstore-shaped package, built in memory at test time.
//!
//! A package is the archive the daemon downloads and unpacks (`hostkeeper-unpack`): one zstd frame
//! holding a ustar tar of `bin/`, `bin/tierstore-like`, `manifest.toml`, `units/` and one unit
//! template, in that order, modes 0555 for directories and the binary and 0444 for the other
//! files, owned by root, dated 0, so the bytes depend on the inputs alone. This is the shape
//! `cargo xtask release package` builds for the production services; the test builds it itself
//! so that each scenario can have several versions without anything binary in the repository.
//!
//! The manifest and the unit come from the templates under `fixtures/tierstore-shaped/`, which
//! `include_str!` embeds and `{{placeholder}}` fill. The binary is the `tierstore-like` program of
//! `tests/bin/tierstore_like.rs`, whose path the caller gives (`CARGO_BIN_EXE_tierstore-like` in
//! the test file).
//!
//! [`PackageBuilder::with_link`] adds a symlink entry, which the unpacker refuses: it is for the
//! test of that refusal and no other.

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::platform::Platform;
use sha2::{Digest, Sha256};

use super::{BINARY_NAME, SERVICE, UNIT_TEMPLATE};

/// The manifest template; the placeholders are listed in `fixtures/tierstore-shaped/README.md`.
const MANIFEST_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/tierstore-shaped/manifest.toml.in"
));
/// The unit template of the run units.
const UNIT_TEMPLATE_TEXT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/tierstore-shaped/units/hostkeeper-ci-tierstore@.service.in"
));

/// Mode of a directory and of the executable.
const MODE_EXEC: u32 = 0o555;
/// Mode of the other files.
const MODE_FILE: u32 = 0o444;

/// One version of the fixture service.
#[derive(Clone, Copy, Debug)]
pub struct FixtureVersion {
    /// The version text, `[A-Za-z0-9._+-]`.
    pub version: &'static str,
    /// The ports of the two run instances (each version of a scenario has its own pair, so the
    /// old and the new release can run side by side during the cutover).
    pub ports: [u16; 2],
    /// Whether this version's `verify` step fails (`tierstore_like_fail_verify`).
    pub fail_verify: bool,
}

/// A built package and what the release channel needs to publish it.
#[derive(Clone, Debug)]
pub struct BuiltPackage {
    pub version: String,
    /// `<service>-<version>-<platform>.tar.zst`.
    pub file_name: String,
    /// Where [`PackageBuilder::build`] wrote it.
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// 64 lowercase hex digits.
    pub sha256: String,
    pub ports: [u16; 2],
    /// `hostkeeper-ci-tierstore@<port>.service` for each port.
    pub run_units: Vec<String>,
}

/// Builds the packages of one host's scenario.
#[derive(Clone, Debug)]
pub struct PackageBuilder {
    /// The daemon's `--packages-dir`, which the unit's `ExecStart` names.
    packages_dir: PathBuf,
    /// The directory the hook records into.
    record_dir: PathBuf,
    /// The built `tierstore-like` program.
    binary: PathBuf,
    /// Symlink entries `(name, target)` appended to the archive.
    links: Vec<(String, String)>,
}

impl PackageBuilder {
    /// A builder for a host whose packages directory is `packages_dir`, whose hook records into
    /// `record_dir`, and whose binary is the file `binary`.
    pub fn new(packages_dir: &Path, record_dir: &Path, binary: &Path) -> PackageBuilder {
        PackageBuilder {
            packages_dir: packages_dir.to_path_buf(),
            record_dir: record_dir.to_path_buf(),
            binary: binary.to_path_buf(),
            links: Vec::new(),
        }
    }

    /// Adds a symlink entry `name` -> `target` after the regular entries. For the test that the
    /// unpacker refuses a link and for nothing else.
    pub fn with_link(mut self, name: &str, target: &str) -> PackageBuilder {
        self.links.push((name.to_owned(), target.to_owned()));
        self
    }

    /// Builds the package of `version` and writes it to `out_dir/<file name>`.
    pub fn build(&self, version: &FixtureVersion, out_dir: &Path) -> BuiltPackage {
        let binary = fs::read(&self.binary).unwrap_or_else(|e| {
            panic!(
                "cannot read the fixture binary {}: {e}",
                self.binary.display()
            )
        });
        let manifest = manifest_text(version, &self.record_dir);
        let unit = unit_text(&self.packages_dir);
        let tar = self.tar(&binary, manifest.as_bytes(), unit.as_bytes());
        let bytes = ruzstd::encoding::compress_to_vec(
            &tar[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let file_name = format!("{SERVICE}-{}-{}.tar.zst", version.version, platform());
        fs::create_dir_all(out_dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", out_dir.display()));
        let path = out_dir.join(&file_name);
        fs::write(&path, &bytes).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
        BuiltPackage {
            version: version.version.to_owned(),
            file_name,
            path,
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
            ports: version.ports,
            run_units: version
                .ports
                .iter()
                .map(|port| format!("{SERVICE}@{port}.service"))
                .collect(),
        }
    }

    /// The ustar archive: the entries in the order the unpacker needs (a directory before what is
    /// in it), then any links, then the end marker `tar` writes.
    fn tar(&self, binary: &[u8], manifest: &[u8], unit: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let unit_path = format!("units/{UNIT_TEMPLATE}");
        let bin_path = format!("bin/{BINARY_NAME}");
        append(&mut builder, "bin/", MODE_EXEC, true, &[]);
        append(&mut builder, &bin_path, MODE_EXEC, false, binary);
        append(&mut builder, "manifest.toml", MODE_FILE, false, manifest);
        append(&mut builder, "units/", MODE_EXEC, true, &[]);
        append(&mut builder, &unit_path, MODE_FILE, false, unit);
        for (name, target) in &self.links {
            let mut header = header(tar::EntryType::Symlink, 0o777);
            builder
                .append_link(&mut header, name, target)
                .unwrap_or_else(|e| panic!("cannot add the link {name}: {e}"));
        }
        builder
            .into_inner()
            .expect("the tar archive is written to memory")
    }
}

/// The manifest of `version`, with every placeholder of the template filled.
pub fn manifest_text(version: &FixtureVersion, record_dir: &Path) -> String {
    let [port_a, port_b] = version.ports;
    render(
        MANIFEST_TEMPLATE,
        &[
            ("service", SERVICE.to_owned()),
            ("version", version.version.to_owned()),
            ("platform", platform().to_owned()),
            ("port_a", port_a.to_string()),
            ("port_b", port_b.to_string()),
            ("record_dir", path_text(record_dir)),
            ("fail_verify", version.fail_verify.to_string()),
        ],
    )
}

/// The run units' template file, with the packages directory its `ExecStart` goes through.
pub fn unit_text(packages_dir: &Path) -> String {
    render(
        UNIT_TEMPLATE_TEXT,
        &[
            ("service", SERVICE.to_owned()),
            ("packages", path_text(packages_dir)),
        ],
    )
}

/// The platform string of this machine, as the daemon names it in the manifest and the file name.
fn platform() -> &'static str {
    Platform::current().as_str()
}

/// `path` as the text of a TOML or unit file, which must be UTF-8 without quotes or backslashes.
fn path_text(path: &Path) -> String {
    let text = path
        .to_str()
        .unwrap_or_else(|| panic!("{} is not UTF-8", path.display()));
    assert!(
        !text.contains(['"', '\\', '\n']),
        "{text:?} cannot be written into a manifest or a unit"
    );
    text.to_owned()
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

/// A header owned by root, dated 0.
fn header(kind: tar::EntryType, mode: u32) -> tar::Header {
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(kind);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(0);
    header
}

/// Appends a directory (`name` ends with `/`) or a regular file.
fn append(
    builder: &mut tar::Builder<Vec<u8>>,
    name: &str,
    mode: u32,
    directory: bool,
    data: &[u8],
) {
    let kind = if directory {
        tar::EntryType::Directory
    } else {
        tar::EntryType::Regular
    };
    let mut header = header(kind, mode);
    header.set_size(data.len() as u64);
    builder
        .append_data(&mut header, name, data)
        .unwrap_or_else(|e| panic!("cannot add {name} to the archive: {e}"));
}
