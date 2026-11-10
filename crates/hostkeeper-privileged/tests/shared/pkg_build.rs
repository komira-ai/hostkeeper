//! A `.pkg` built on the runner for a Mac device test, with the Mac's own `pkgbuild` and
//! `productbuild`, in the form a release has: a component package of the payload (install
//! location `/`, every entry root's, `--ownership recommended`) wrapped by `productbuild` as the
//! one package of a distribution package, unsigned (nothing is signed on a runner:
//! `shared/macos_pkg.rs` says how the daemon's signature check is answered). A `postinstall` the
//! package carries is the script `installer` runs after it laid the payload down.
//!
//! Every program is run through the bounded runner of the launchd crate (an empty environment, no
//! standard input, at most [`TOOL_BOUND`]), so one that hangs fails the test naming it.

#![allow(dead_code, reason = "each test file uses part of this module")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

/// The most any program the test builds with is waited for before it is killed and the test fails
/// naming it: a bound on a hang, not a measure of time.
pub const TOOL_BOUND: Duration = Duration::from_secs(300);

/// One file of a package: its path from `/` without the leading slash, its bytes and its mode.
#[derive(Clone, Debug)]
pub struct PkgFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub mode: u32,
}

impl PkgFile {
    /// The file at `path` (from `/`, no leading slash) with `bytes` and mode `mode`.
    pub fn new(path: &str, bytes: impl Into<Vec<u8>>, mode: u32) -> PkgFile {
        PkgFile {
            path: path.to_owned(),
            bytes: bytes.into(),
            mode,
        }
    }
}

/// What a package holds.
#[derive(Clone, Debug)]
pub struct PkgSource {
    /// The package identifier, the name `pkgutil` knows the receipt by.
    pub identifier: String,
    /// The version text of the component.
    pub version: String,
    /// The payload.
    pub files: Vec<PkgFile>,
    /// The text of the package's `postinstall`, when it has one.
    pub postinstall: Option<String>,
}

/// Runs `program` with `args`, which must exit 0; its standard output.
pub fn run(program: &str, args: &[&str]) -> String {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let out = LaunchctlProcess::new(Path::new(program), TOOL_BOUND)
        .run(&args)
        .unwrap_or_else(|e| panic!("{program} did not run to its end: {e}"));
    assert_eq!(
        out.code,
        Some(0),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn text(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

/// Builds the package `source` in the new directory `dir`: the payload under `dir/root`, the
/// component `dir/component.pkg` and the product `dir/product.pkg`, which is given.
pub fn build(dir: &Path, source: &PkgSource) -> PathBuf {
    fs::create_dir(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let root = dir.join("root");
    fs::create_dir(&root).expect("create the payload root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("its mode");
    for file in &source.files {
        let at = root.join(&file.path);
        let mut dirs: Vec<&Path> = at
            .ancestors()
            .skip(1)
            .take_while(|dir| *dir != root)
            .collect();
        dirs.reverse();
        for made in dirs {
            if !made.is_dir() {
                fs::create_dir(made).expect("create a payload directory");
                fs::set_permissions(made, fs::Permissions::from_mode(0o755)).expect("chmod");
            }
        }
        fs::write(&at, &file.bytes).expect("write a payload file");
        fs::set_permissions(&at, fs::Permissions::from_mode(file.mode)).expect("chmod");
    }
    run("/usr/sbin/chown", &["-R", "0:0", text(&root)]);
    let component = dir.join("component.pkg");
    let mut args = vec![
        "--root".to_owned(),
        text(&root).to_owned(),
        "--identifier".to_owned(),
        source.identifier.clone(),
        "--version".to_owned(),
        source.version.clone(),
        "--install-location".to_owned(),
        "/".to_owned(),
        "--ownership".to_owned(),
        "recommended".to_owned(),
    ];
    if let Some(script) = &source.postinstall {
        let scripts = dir.join("scripts");
        fs::create_dir(&scripts).expect("create the scripts directory");
        let at = scripts.join("postinstall");
        fs::write(&at, script).expect("write the postinstall");
        fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).expect("its mode");
        args.extend(["--scripts".to_owned(), text(&scripts).to_owned()]);
    }
    args.push(text(&component).to_owned());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run("/usr/bin/pkgbuild", &args);
    let product = dir.join("product.pkg");
    run(
        "/usr/bin/productbuild",
        &[
            "--package",
            text(&component),
            "--identifier",
            &source.identifier,
            "--version",
            &source.version,
            text(&product),
        ],
    );
    product
}
