//! hello's macOS package (`examples/hello`), built at test time for a host of the test's own.
//!
//! The package is the one `cargo xtask release macos-pkg --service hello` builds
//! (`crates/xtask/src/release/native/hello.rs` says what it holds), with the host paths of the
//! test in place of the released ones and two additions the test needs and a release does not
//! carry:
//!
//! - **The payload** is the program `hello-e2e` (`tests/bin/hello_e2e.rs`) at
//!   `/Library/Application Support/hello/bin/hello`, the plists of the two slots' jobs
//!   (`hello.a`, `hello.b`) at `/Library/LaunchDaemons/`, rendered by
//!   `hostkeeper_launchd::service_plist` from the same fields xtask renders them from, with
//!   `Disabled` true added as xtask adds it (the program `<support>/bin/hello`, `--port <port>`
//!   and `--fail-ready` for the failing build, kept alive while `<record dir>/slots/<slot>`
//!   exists, output in `<log dir>/hello.<slot>.{out,err}.log`), and the file
//!   `/Library/Application Support/hello/version`, which says the version the program serves
//!   (hello's own program serves the version it was compiled as, which is one version for every
//!   package a test builds from one binary).
//! - **A `postinstall`** that loads the two jobs from the plists the package installed: it
//!   enables each label (the plists say `Disabled`), boots a loaded job out and bootstraps it
//!   from its plist again, so that a job runs the arguments of the plist it was installed with.
//!   This is what the design asks of a service whose hook starts its job with `launchctl
//!   kickstart`, which finds only a job launchd holds: "a service whose `.pkg` scripts bootstrap
//!   its job starts it before `cutover`, so such a service needs ... a hook that tolerates it"
//!   (docs/design/packages-native.md, section 10.2). The released hello `.pkg` has no script.
//!
//! The entry of an order is the one `release targets` writes for hello's `.pkg` on a Mac
//! (`crates/xtask/src/release/targets/manifest.rs`): the `macos-pkg` installer, the pin, the
//! package identifier, the two labels as its `run` units, the readiness URL, the params its
//! hook reads (`units` `launchd`) and the hook, the program the package installs with
//! `update-hook`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hostkeeper_config::platform::Platform;
use hostkeeper_launchd::{Label, ServiceJob, service_plist};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Location, Manifest, Param, StepBounds, Target, TargetList, param,
};

use super::pkg_build::{self, PkgFile, PkgSource};

/// The service.
pub const SERVICE: &str = "hello";
/// The two slots an update alternates between; the job of slot `s` is `hello.<s>`.
pub const SLOTS: [&str; 2] = ["a", "b"];
/// The package identifier `pkgutil` knows hello's receipt by.
pub const IDENTIFIER: &str = "dev.hostkeeper.hello";
/// The program the tests put at `bin/hello` of the package.
pub const HELLO_E2E: &str = env!("CARGO_BIN_EXE_hello-e2e");
/// The service's directory of `/Library/Application Support`, where the package installs.
pub const SUPPORT: &str = "/Library/Application Support/hello";
/// The program the package installs, and the hook command's program.
pub const PROGRAM: &str = "/Library/Application Support/hello/bin/hello";
/// The file of the package that says the version the program serves.
pub const VERSION_FILE: &str = "/Library/Application Support/hello/version";
/// Where the package installs its jobs' plists, and launchd reads them.
pub const LAUNCH_DAEMONS: &str = "/Library/LaunchDaemons";

/// The label of the job of `slot`.
pub fn label(slot: &str) -> Label {
    Label::parse(&format!("{SERVICE}.{slot}")).expect("hello's slot labels are labels")
}

/// Where the package installs the plist of `slot`'s job.
pub fn plist_path(slot: &str) -> PathBuf {
    Path::new(LAUNCH_DAEMONS).join(format!("{SERVICE}.{slot}.plist"))
}

/// The host paths and the port one set of hello packages is built for.
#[derive(Clone, Debug)]
pub struct HelloLayout {
    /// The hook's record directory (`[params] record_dir`), which holds the slots' markers.
    pub record_dir: PathBuf,
    /// Where the jobs' output goes.
    pub log_dir: PathBuf,
    /// The port the serving slot listens on.
    pub port: u16,
}

/// A built package and the entry that orders it.
#[derive(Clone, Debug)]
pub struct HelloPackage {
    pub version: String,
    /// The product package, unsigned.
    pub pkg: PathBuf,
    /// 64 lowercase hex digits: the pin.
    pub sha256: String,
    pub bytes: u64,
    /// The entry of an order for this package.
    pub entry: Target,
}

impl HelloPackage {
    /// The `targets` of a set of this version: the one `macos-pkg` entry, for this host's
    /// platform.
    pub fn targets(&self) -> BTreeMap<String, TargetList> {
        BTreeMap::from([(
            Platform::current().to_string(),
            TargetList {
                entries: vec![self.entry.clone()],
            },
        )])
    }
}

/// The key and value the `.pkg`'s plists carry beside what `service_plist` writes: launchd
/// leaves a job whose plist says `Disabled` unloaded at boot until an override enables it.
const DISABLED: &str = "\t<key>Disabled</key>\n\t<true/>\n";

/// The `postinstall` of hello's test package (the module says why it has one).
fn postinstall() -> String {
    let mut script = String::from("#!/bin/sh\n");
    for slot in SLOTS {
        let label = format!("{SERVICE}.{slot}");
        script.push_str(&format!(
            "/bin/launchctl enable system/{label} || exit 1\n\
             /bin/launchctl bootout system/{label} 2>/dev/null\n\
             /bin/launchctl bootstrap system '{LAUNCH_DAEMONS}/{label}.plist' || exit 1\n"
        ));
    }
    script.push_str("exit 0\n");
    script
}

impl HelloLayout {
    /// The plist of `slot`'s job; `fail_ready` starts the server with `--fail-ready`.
    pub fn plist(&self, slot: &str, fail_ready: bool) -> String {
        let mut args = vec![
            OsString::from("--port"),
            OsString::from(self.port.to_string()),
        ];
        if fail_ready {
            args.push(OsString::from("--fail-ready"));
        }
        let job = ServiceJob {
            label: label(slot),
            program: PathBuf::from(PROGRAM),
            args,
            run_while: self.record_dir.join("slots").join(slot),
            stdout: self.log_dir.join(format!("{SERVICE}.{slot}.out.log")),
            stderr: self.log_dir.join(format!("{SERVICE}.{slot}.err.log")),
        };
        let text = service_plist(&job).expect("hello's plist renders");
        let open = "<dict>\n";
        text.replacen(open, &format!("{open}{DISABLED}"), 1)
    }

    /// What the package of `version` holds, with `binary` (the bytes of `bin/hello`), its plists
    /// starting the server with `--fail-ready` when `fail_ready` is set.
    pub fn source(&self, version: &str, fail_ready: bool, binary: &[u8]) -> PkgSource {
        let from_root = |path: &str| path.trim_start_matches('/').to_owned();
        let mut files = vec![
            PkgFile::new(&from_root(PROGRAM), binary, 0o755),
            PkgFile::new(&from_root(VERSION_FILE), format!("{version}\n"), 0o644),
        ];
        for slot in SLOTS {
            files.push(PkgFile::new(
                &from_root(&plist_path(slot).to_string_lossy()),
                self.plist(slot, fail_ready),
                0o644,
            ));
        }
        PkgSource {
            identifier: IDENTIFIER.to_owned(),
            version: version.to_owned(),
            files,
            postinstall: Some(postinstall()),
        }
    }

    /// The entry of the package `sha256` of `bytes` bytes at `version`.
    pub fn entry(&self, version: &str, sha256: &str, bytes: u64) -> Target {
        let text = |value: &str| Param {
            value: Some(param::Value::Text(value.to_owned())),
        };
        Target {
            installer: "macos-pkg".to_owned(),
            locations: vec![Location {
                store: "channel".to_owned(),
                locator: format!("{SERVICE}/{version}/{SERVICE}-{version}-macos-arm64.pkg"),
            }],
            digest: sha256.to_owned(),
            bytes,
            manifest: Some(Manifest {
                min_hostkeeper: "0.1.0".to_owned(),
                package: Some(IDENTIFIER.to_owned()),
                run: SLOTS.map(|slot| format!("{SERVICE}.{slot}")).to_vec(),
                health_url: Some(format!("http://127.0.0.1:{}/readyz", self.port)),
                params: BTreeMap::from([
                    (
                        "port".to_owned(),
                        Param {
                            value: Some(param::Value::Integer(i64::from(self.port))),
                        },
                    ),
                    (
                        "record_dir".to_owned(),
                        text(self.record_dir.to_str().expect("a UTF-8 path")),
                    ),
                    ("units".to_owned(), text("launchd")),
                ])
                .into_iter()
                .collect(),
                hook: Some(Hook {
                    command: vec![PROGRAM.to_owned(), "update-hook".to_owned()],
                    timeout_secs: Some(StepBounds {
                        prepare: 30,
                        cutover: 60,
                        verify: 30,
                        rollback: 60,
                        cleanup: 30,
                        uninstall: Some(60),
                    }),
                    verify_deadline_secs: 120,
                    ..Hook::default()
                }),
                installer_bounds: Some(InstallerBounds {
                    stage_secs: 300,
                    activate_secs: 300,
                    remove_secs: 300,
                }),
                ..Manifest::default()
            }),
            ..Target::default()
        }
    }

    /// The package of `version` built in the new directory `dir`, `fail_ready` as for
    /// [`HelloLayout::source`], and its entry.
    pub fn build(
        &self,
        dir: &Path,
        version: &str,
        fail_ready: bool,
        binary: &[u8],
    ) -> HelloPackage {
        let pkg = pkg_build::build(dir, &self.source(version, fail_ready, binary));
        let sha256 = super::macos_pkg::sha256_of(&pkg);
        let bytes = std::fs::metadata(&pkg)
            .unwrap_or_else(|e| panic!("{}: {e}", pkg.display()))
            .len();
        HelloPackage {
            version: version.to_owned(),
            entry: self.entry(version, &sha256, bytes),
            pkg,
            sha256,
            bytes,
        }
    }
}
