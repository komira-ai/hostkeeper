//! hello's macOS package (`examples/hello`), built at test time for a host of the test's own.
//!
//! The package is the one `cargo xtask release package --service hello --platform macos-arm64`
//! builds, with the host's paths of the test in place of the released ones: the manifest is the
//! committed golden `examples/hello/package/macos/manifest.toml` with its version, port and hook
//! record directory replaced; the plists of the two slots are rendered by
//! `hostkeeper_launchd::service_plist` from the same fields xtask renders them from (label
//! `hello.<slot>`, the program `<packages>/hello/current/bin/hello`, `--port <port>` and
//! `--fail-ready` for the failing build, kept alive while `<record dir>/slots/<slot>` exists,
//! output in `<log dir>/hello.<slot>.{out,err}.log`); and the entries are those of the golden
//! `entries.txt`, in its order and modes. [`HelloLayout::released`] is the released layout, with
//! which the manifest and the plists are the goldens byte for byte (`tests/tierstore_rig.rs`
//! checks it in the pull-request set).
//!
//! `bin/hello` is the program `hello-e2e` (`tests/bin/hello_e2e.rs`): hello's own hook and
//! server, answering `/version` with the version of the manifest it is unpacked with.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hostkeeper_launchd::{Label, ServiceJob, service_plist};
use sha2::{Digest, Sha256};

use super::package::{ArchiveEntry, compressed_archive};

/// The service.
pub const SERVICE: &str = "hello";
/// The two slots an update alternates between; the job of slot `s` is `hello.<s>`.
pub const SLOTS: [&str; 2] = ["a", "b"];
/// The platform of the macOS package.
pub const PLATFORM: &str = "macos-arm64";
/// The program the tests put at `bin/hello`.
pub const HELLO_E2E: &str = env!("CARGO_BIN_EXE_hello-e2e");

/// The version the golden manifest names.
pub const GOLDEN_VERSION: &str = "1.2.3";
/// The committed goldens of the macOS package.
pub const GOLDEN_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/hello/package/macos/manifest.toml"
));
pub const GOLDEN_PLIST_A: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/hello/package/macos/units/hello.a.plist"
));
pub const GOLDEN_PLIST_B: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/hello/package/macos/units/hello.b.plist"
));
pub const GOLDEN_ENTRIES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/hello/package/macos/entries.txt"
));

/// What the released package says of the host, which the golden manifest carries.
const RELEASED_PORT: u16 = 9310;
const RELEASED_PACKAGES: &str = "/var/lib/hostkeeper/packages";
const RELEASED_RECORD_DIR: &str = "/var/lib/updater/hooks/hello";
const RELEASED_LOG_DIR: &str = "/var/log";

/// The label of the job of `slot`.
pub fn label(slot: &str) -> Label {
    Label::parse(&format!("{SERVICE}.{slot}")).expect("hello's slot labels are labels")
}

/// The host paths and the port one set of hello packages is built for.
#[derive(Clone, Debug)]
pub struct HelloLayout {
    /// The daemon's packages directory, which the plists' program goes through.
    pub packages: PathBuf,
    /// The hook's record directory (`[params] record_dir`), which holds the slots' markers.
    pub record_dir: PathBuf,
    /// Where the jobs' output goes.
    pub log_dir: PathBuf,
    /// The port the serving slot listens on.
    pub port: u16,
}

/// A built package.
#[derive(Clone, Debug)]
pub struct HelloPackage {
    pub version: String,
    /// `hello-<version>-macos-arm64.tar.zst`.
    pub file_name: String,
    pub bytes: Vec<u8>,
    /// 64 lowercase hex digits.
    pub sha256: String,
}

/// `path` as text that goes into a manifest or a plist unchanged.
fn text(path: &Path) -> &str {
    let text = path
        .to_str()
        .unwrap_or_else(|| panic!("{} is not UTF-8", path.display()));
    assert!(
        !text.contains(['"', '\\', '\n', '<', '>', '&']),
        "{text:?} cannot be written into a manifest or a plist unchanged"
    );
    text
}

/// `text` with the one `from` in it replaced by `to`; panics when `from` is not there exactly
/// once (the golden and this module out of step).
fn replace_once(text: &str, from: &str, to: &str) -> String {
    assert_eq!(
        text.matches(from).count(),
        1,
        "the golden manifest holds {from:?} exactly once"
    );
    text.replacen(from, to, 1)
}

impl HelloLayout {
    /// The layout the released package is built for (`HELLO` of `cargo xtask release package`).
    pub fn released() -> HelloLayout {
        HelloLayout {
            packages: PathBuf::from(RELEASED_PACKAGES),
            record_dir: PathBuf::from(RELEASED_RECORD_DIR),
            log_dir: PathBuf::from(RELEASED_LOG_DIR),
            port: RELEASED_PORT,
        }
    }

    /// The manifest of `version`: the golden with its version, port and record directory
    /// replaced.
    pub fn manifest(&self, version: &str) -> String {
        let text = replace_once(
            GOLDEN_MANIFEST,
            &format!("version = \"{GOLDEN_VERSION}\""),
            &format!("version = \"{version}\""),
        );
        let text = replace_once(
            &text,
            &format!("url = \"http://127.0.0.1:{RELEASED_PORT}/readyz\""),
            &format!("url = \"http://127.0.0.1:{}/readyz\"", self.port),
        );
        let text = replace_once(
            &text,
            &format!("port = {RELEASED_PORT}\n"),
            &format!("port = {}\n", self.port),
        );
        replace_once(
            &text,
            &format!("record_dir = \"{RELEASED_RECORD_DIR}\""),
            &format!("record_dir = \"{}\"", text_of(&self.record_dir)),
        )
    }

    /// The plist of `slot`'s job; `fail_ready` starts the server with `--fail-ready`.
    pub fn plist(&self, slot: &str, fail_ready: bool) -> String {
        let mut args = vec![
            OsString::from("--port"),
            OsString::from(self.port.to_string()),
        ];
        if fail_ready {
            args.push(OsString::from("--fail-ready"));
        }
        let packages = text(&self.packages);
        let job = ServiceJob {
            label: label(slot),
            program: PathBuf::from(format!("{packages}/{SERVICE}/current/bin/{SERVICE}")),
            args,
            run_while: self.record_dir.join("slots").join(slot),
            stdout: self.log_dir.join(format!("{SERVICE}.{slot}.out.log")),
            stderr: self.log_dir.join(format!("{SERVICE}.{slot}.err.log")),
        };
        service_plist(&job).expect("hello's plist renders")
    }

    /// The package of `version` with `binary` (the bytes of `bin/hello`), its plists starting the
    /// server with `--fail-ready` when `fail_ready` is set.
    pub fn build(&self, version: &str, fail_ready: bool, binary: &[u8]) -> HelloPackage {
        let manifest = self.manifest(version);
        let plists = SLOTS.map(|slot| self.plist(slot, fail_ready));
        let data = |name: &str| -> &[u8] {
            match name {
                "bin/hello" => binary,
                "manifest.toml" => manifest.as_bytes(),
                "units/hello.a.plist" => plists[0].as_bytes(),
                "units/hello.b.plist" => plists[1].as_bytes(),
                _ => &[],
            }
        };
        let listed = golden_entries();
        let entries: Vec<ArchiveEntry<'_>> = listed
            .iter()
            .map(|(mode, name)| ArchiveEntry {
                name,
                mode: *mode,
                data: data(name),
            })
            .collect();
        let bytes = compressed_archive(&entries);
        HelloPackage {
            version: version.to_owned(),
            file_name: format!("{SERVICE}-{version}-{PLATFORM}.tar.zst"),
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
        }
    }
}

/// [`text`] of a path, owned.
fn text_of(path: &Path) -> String {
    text(path).to_owned()
}

/// The entries of the golden `entries.txt`, in its order: each line `<octal mode> <name>`.
pub fn golden_entries() -> Vec<(u32, String)> {
    GOLDEN_ENTRIES
        .lines()
        .map(|line| {
            let (mode, name) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("{line:?} is not `<mode> <name>`"));
            let mode = u32::from_str_radix(mode, 8)
                .unwrap_or_else(|e| panic!("{mode:?} is not an octal mode: {e}"));
            (mode, name.to_owned())
        })
        .collect()
}
