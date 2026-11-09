//! The stores file and the macos-pkg signers file of a Mac, written from the managed preferences of
//! `dev.hostkeeper.stores` and `dev.hostkeeper.macos-pkg` by the start of a Mac's daemon
//! ([`hostkeeper_daemon::mac_files`], docs/design/packages-repositories.md, section 4.7). Every
//! path is in a scratch directory, and the trusted owner is the uid the tests run as.

use std::fs;
use std::num::NonZeroU64;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_daemon::cli::{self, EXIT_CONFIG, HostFiles};
use hostkeeper_daemon::mac_files::{self, MacFilesError, Rendered};
use hostkeeper_daemon::notify::NoNotifier;
use hostkeeper_daemon::start::{Settings, StartError};
use hostkeeper_install::macos_pkg::signers;
use hostkeeper_launchd::postinstall::{MANAGED_MACOS_PKG, MANAGED_STORES};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_package::stores;
use hostkeeper_testkit::Scratch;
use plist::{Dictionary, Value};

const LEAF_A: &str = "00000000000000000000000000000000000000000000000000000000000000a1";
const LEAF_B: &str = "00000000000000000000000000000000000000000000000000000000000000b2";

/// A daemon config the loader accepts, written by the test so that a start gets past its config.
const CONFIG: &str = "coordinator = \"https://coordinator.example:8443\"\n\
ca_sha256 = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n\
release_channel = \"https://releases.example/path\"\n\
[trusted_signers]\n\
organisation = \"komira-ai\"\n\
workflow = \".github/workflows/release.yml\"\n\
refs = [\"refs/tags/*\"]\n\
hosted_runner = true\n";

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

fn dict(pairs: Vec<(&str, Value)>) -> Dictionary {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

/// The managed dictionary of the signers `leaves`.
fn signers_of(leaves: &[&str]) -> Dictionary {
    dict(vec![(
        "signers",
        Value::Array(leaves.iter().map(|leaf| text(leaf)).collect()),
    )])
}

/// The managed dictionary of one `https` store `name`, with `extra` keys.
fn store(name: &str, extra: Vec<(&str, Value)>) -> Dictionary {
    let mut pairs = vec![
        ("name", text(name)),
        ("kind", text("https")),
        ("base", text("https://store.example/")),
    ];
    pairs.extend(extra);
    dict(vec![(
        "store",
        Value::Array(vec![Value::Dictionary(dict(pairs))]),
    )])
}

/// A Mac under a scratch directory: the two managed files and the two targets, the targets in
/// directories that do not exist yet.
struct Mac {
    scratch: Scratch,
    managed_stores: PathBuf,
    managed_pkg: PathBuf,
    stores: PathBuf,
    signers: PathBuf,
}

impl Mac {
    fn new(name: &str) -> Mac {
        let scratch =
            Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch dir");
        let root = scratch.path().to_path_buf();
        let etc = root.join("etc hostkeeper");
        Mac {
            managed_stores: root.join("stores managed.plist"),
            managed_pkg: root.join("macos-pkg managed.plist"),
            stores: etc.join("stores.toml"),
            signers: etc.join("macos-pkg.toml"),
            scratch,
        }
    }

    fn root(&self) -> &Path {
        self.scratch.path()
    }

    fn etc(&self) -> &Path {
        self.stores.parent().expect("a parent")
    }

    /// Writes `top` as the managed file at `path`, XML, mode 0644.
    fn write(&self, path: &Path, top: Dictionary) {
        let mut bytes = Vec::new();
        Value::Dictionary(top)
            .to_writer_xml(&mut bytes)
            .expect("encode");
        self.write_bytes(path, &bytes);
    }

    fn write_bytes(&self, path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("a managed file");
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("its mode");
    }

    /// A credential file of mode 0600 with one line.
    fn credential(&self, name: &str, mode: u32) -> PathBuf {
        let path = self.root().join(name);
        fs::write(&path, "token\n").expect("a credential");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("its mode");
        path
    }

    fn stores(&self) -> Result<Rendered, MacFilesError> {
        mac_files::stores(&self.managed_stores, &self.stores, me())
    }

    fn pkg(&self) -> Result<Rendered, MacFilesError> {
        mac_files::macos_pkg(&self.managed_pkg, &self.signers, me())
    }

    /// The daemon's start on a host with `files`' managed preferences: its config is written by
    /// the test and its packages directory is a file, so a start that gets past the Mac's files
    /// and its config stops there, `Io` naming it.
    fn start(&self, mac: bool) -> StartError {
        let packages = self.root().join("packages");
        let config = self.root().join("hostkeeper.toml");
        fs::write(&packages, b"not a directory").expect("a file at the packages path");
        fs::write(&config, CONFIG).expect("the config");
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("its mode");
        let settings = Settings {
            config,
            state_dir: self.root().join("state"),
            packages_dir: packages,
            run_dir: self.root().join("run"),
            trusted_uid: me(),
            trusted_root_max_age_days: NonZeroU64::MIN,
        };
        let files = HostFiles {
            repositories: None,
            os_root: self.root().join("os"),
            ca_bundle: self.root().join("ca.pem"),
            gh: self.root().join("gh"),
            trusted_root: self.root().join("trusted_root.json"),
            stores: self.stores.clone(),
            macos_pkg_signers: Some(self.signers.clone()),
            pki: self.root().join("pki"),
            console: None,
            managed_config: None,
            managed_stores: mac.then(|| self.managed_stores.clone()),
            managed_macos_pkg: mac.then(|| self.managed_pkg.clone()),
        };
        let own = HostkeeperVersion::parse("1.0.0").expect("a version");
        cli::start_with(&settings, &files, own, &NoNotifier, &Platform::linux())
            .err()
            .expect("the start stops")
    }

    fn past_files(&self, error: &StartError) -> bool {
        matches!(error, StartError::Io { path, .. } if *path == self.root().join("packages"))
    }

    fn etc_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.etc())
            .expect("the directory")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

fn inode(path: &Path) -> u64 {
    fs::metadata(path).expect("a file").ino()
}

/// Catches a stores file that is not written from `dev.hostkeeper.stores` when it is absent, or
/// is written as bytes the loader reads differently from the managed store: a start with only the
/// stores domain writes `stores.toml` under a header naming the domain, `stores::load` reads the
/// written file as it reads the operator's text of the same store (credential file included), its
/// mode is 0644 and its directory 0755 with nothing else in it, and the signers file, whose domain
/// is absent, is not made.
#[test]
fn a_start_writes_the_stores_file_from_its_domain_when_there_is_none() {
    let mac = Mac::new("mac-files-stores-written");
    let bearer = mac.credential("gh.token", 0o600);
    mac.write(
        &mac.managed_stores,
        store(
            "lan",
            vec![("bearer_file", text(bearer.to_str().expect("utf8")))],
        ),
    );
    let error = mac.start(true);
    assert!(mac.past_files(&error), "{error}");
    let written = fs::read_to_string(&mac.stores).expect("written");
    assert!(
        written.starts_with(
            "# Written by hostkeeper from the managed preferences of dev.hostkeeper.stores.\n"
        ),
        "{written}"
    );
    let operator = format!(
        "[[store]]\nname = \"lan\"\nkind = \"https\"\nbase = \"https://store.example/\"\nbearer_file = \"{}\"\n",
        bearer.display()
    );
    let channel = DaemonConfig::parse(CONFIG).expect("config").release_channel;
    let loaded = stores::load(&mac.stores, &channel, me()).expect("the loader accepts it");
    let expected = stores::parse(operator.as_bytes(), me()).expect("the operator's text");
    assert_eq!(format!("{loaded:?}"), format!("{expected:?}"));
    let mode = |path: &Path| fs::metadata(path).expect("meta").permissions().mode() & 0o7777;
    assert_eq!(mode(&mac.stores), 0o644);
    assert_eq!(mode(mac.etc()), 0o755);
    assert_eq!(
        mac.etc_names(),
        ["stores.toml"],
        "no temporary, no signers file"
    );
}

/// Catches a signers file that is not written from `dev.hostkeeper.macos-pkg` when absent, or that
/// the loader reads differently from the managed signers: the start writes `macos-pkg.toml` under
/// a header naming the domain, `signers::load` reads the leaf, and the stores file is not made
/// when its domain is absent.
#[test]
fn a_start_writes_the_signers_file_from_its_domain_when_there_is_none() {
    let mac = Mac::new("mac-files-signers-written");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    let error = mac.start(true);
    assert!(mac.past_files(&error), "{error}");
    let written = fs::read_to_string(&mac.signers).expect("written");
    assert!(
        written.starts_with(
            "# Written by hostkeeper from the managed preferences of dev.hostkeeper.macos-pkg.\n"
        ),
        "{written}"
    );
    let loaded = signers::load(&mac.signers, me())
        .expect("loads")
        .expect("a file");
    let expected = signers::parse(format!("signers = [\"{LEAF_A}\"]\n").as_bytes()).expect("parse");
    assert_eq!(loaded, expected);
    assert_eq!(mac.etc_names(), ["macos-pkg.toml"]);
    assert_eq!(
        fs::metadata(&mac.signers)
            .expect("meta")
            .permissions()
            .mode()
            & 0o7777,
        0o644
    );
}

/// Catches a file that is rewritten when the managed preferences have not changed: the second run
/// says `Unchanged` and the file is the same inode, for both domains.
#[test]
fn a_file_that_holds_the_rendered_bytes_is_not_written_again() {
    let mac = Mac::new("mac-files-unchanged");
    mac.write(&mac.managed_stores, store("lan", vec![]));
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    assert_eq!(mac.stores().expect("first"), Rendered::Written);
    assert_eq!(mac.pkg().expect("first"), Rendered::Written);
    let (stores_inode, signers_inode) = (inode(&mac.stores), inode(&mac.signers));
    assert_eq!(mac.stores().expect("second"), Rendered::Unchanged);
    assert_eq!(mac.pkg().expect("second"), Rendered::Unchanged);
    assert_eq!(inode(&mac.stores), stores_inode);
    assert_eq!(inode(&mac.signers), signers_inode);
}

/// Catches a rotation that does not reach the Mac: the managed signers change from one leaf to two
/// and the file follows (`Written`), and the stores change from one store to another likewise,
/// with no temporary file left beside them.
#[test]
fn a_file_that_differs_from_the_managed_preferences_is_replaced() {
    let mac = Mac::new("mac-files-rotation");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    mac.write(&mac.managed_stores, store("one", vec![]));
    mac.pkg().expect("first");
    mac.stores().expect("first");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A, LEAF_B]));
    mac.write(&mac.managed_stores, store("two", vec![]));
    assert_eq!(mac.pkg().expect("rotated"), Rendered::Written);
    assert_eq!(mac.stores().expect("rotated"), Rendered::Written);
    let now = signers::load(&mac.signers, me())
        .expect("loads")
        .expect("a file");
    let expected = signers::parse(format!("signers = [\"{LEAF_A}\", \"{LEAF_B}\"]\n").as_bytes())
        .expect("parse");
    assert_eq!(now, expected);
    assert!(
        fs::read_to_string(&mac.stores)
            .expect("stores")
            .contains("name = \"two\"")
    );
    assert_eq!(mac.etc_names(), ["macos-pkg.toml", "stores.toml"]);
}

/// Catches a hand edit surviving a domain that is present: the operator's differing file is
/// replaced, where the daemon config would have been kept.
#[test]
fn an_operators_file_that_differs_from_a_present_domain_is_replaced() {
    let mac = Mac::new("mac-files-operator-replaced");
    fs::create_dir_all(mac.etc()).expect("etc");
    fs::write(&mac.signers, format!("signers = [\"{LEAF_B}\"]\n")).expect("operator's file");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    assert_eq!(mac.pkg().expect("replaced"), Rendered::Written);
    let now = fs::read_to_string(&mac.signers).expect("file");
    assert!(now.contains(LEAF_A) && !now.contains(LEAF_B), "{now}");
}

/// Catches a Mac with neither domain changing its files: the operator's files are kept byte for
/// byte and none is made where there was none, by the functions and by a start.
#[test]
fn without_a_domain_the_operators_files_are_kept_and_none_is_made() {
    let mac = Mac::new("mac-files-absent");
    assert_eq!(mac.stores().expect("absent"), Rendered::Absent);
    assert_eq!(mac.pkg().expect("absent"), Rendered::Absent);
    let error = mac.start(true);
    assert!(mac.past_files(&error), "{error}");
    assert!(!mac.etc().exists(), "no directory, no file is made");
    fs::create_dir_all(mac.etc()).expect("etc");
    fs::write(&mac.stores, "# mine\n").expect("stores");
    fs::write(&mac.signers, format!("signers = [\"{LEAF_B}\"]\n")).expect("signers");
    let error = mac.start(true);
    assert!(mac.past_files(&error), "{error}");
    assert_eq!(fs::read_to_string(&mac.stores).expect("stores"), "# mine\n");
    assert_eq!(
        fs::read_to_string(&mac.signers).expect("signers"),
        format!("signers = [\"{LEAF_B}\"]\n")
    );
    assert_eq!(mac.etc_names(), ["macos-pkg.toml", "stores.toml"]);
}

/// Catches a Linux host (no managed paths) reading the domains: with plists present at the Mac's
/// paths but none in `HostFiles`, the start renders nothing.
#[test]
fn a_host_without_managed_paths_renders_nothing() {
    let mac = Mac::new("mac-files-linux");
    mac.write(&mac.managed_stores, store("lan", vec![]));
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    let error = mac.start(false);
    assert!(mac.past_files(&error), "{error}");
    assert!(!mac.etc().exists());
}

/// Catches a managed Mac whose host files do not name the domains' paths: `for_platform` gives a
/// Mac the two managed paths and Linux and OpenRC none.
#[test]
fn a_mac_names_the_two_domains_and_linux_none() {
    let mac = HostFiles::for_platform(PlatformKind::Macos, |_| false);
    assert_eq!(
        mac.managed_stores.as_deref(),
        Some(Path::new(MANAGED_STORES))
    );
    assert_eq!(
        mac.managed_macos_pkg.as_deref(),
        Some(Path::new(MANAGED_MACOS_PKG))
    );
    assert_eq!(
        MANAGED_STORES,
        "/Library/Managed Preferences/dev.hostkeeper.stores.plist"
    );
    assert_eq!(
        MANAGED_MACOS_PKG,
        "/Library/Managed Preferences/dev.hostkeeper.macos-pkg.plist"
    );
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        let host = HostFiles::for_platform(kind, |_| false);
        assert_eq!((host.managed_stores, host.managed_macos_pkg), (None, None));
    }
}

/// The error text of a refusal by the signers loader of `toml`.
fn signers_refusal(managed: &Path, toml: &str) -> String {
    let reason = signers::parse(toml.as_bytes()).expect_err("the loader refuses it");
    format!(
        "the managed preferences {} of dev.hostkeeper.macos-pkg do not make a signers file: {reason}",
        managed.display()
    )
}

/// The error text of a refusal by the stores loader of `toml`.
fn stores_refusal(managed: &Path, toml: &str) -> String {
    let reason = stores::parse(toml.as_bytes(), me()).expect_err("the loader refuses it");
    format!(
        "the managed preferences {} of dev.hostkeeper.stores do not make a stores file: {reason}",
        managed.display()
    )
}

/// Catches a managed file the loader refuses being written, or an existing file lost or a
/// temporary left: five signers, a signer that is not 64 hex digits, an unknown key and a bad URL
/// in the stores, and a credential file the loader refuses each stop the start with
/// `EXIT_CONFIG` and the loader's reason in full, the existing file is unchanged and the
/// directory holds nothing else.
#[test]
fn a_managed_file_the_loader_refuses_stops_the_start_and_changes_nothing() {
    let leaves: Vec<&str> = vec![LEAF_A, LEAF_B, "c3", "d4", "e5"];
    let bad_credential = Mac::new("mac-files-cred").credential("loose.token", 0o644);
    let signer_cases: Vec<(Dictionary, String)> = vec![
        (
            signers_of(&leaves),
            format!(
                "signers = [{}]\n",
                leaves
                    .iter()
                    .map(|l| format!("\"{l}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        (
            signers_of(&["not-hex"]),
            "signers = [\"not-hex\"]\n".to_string(),
        ),
    ];
    for (n, (top, toml)) in signer_cases.into_iter().enumerate() {
        let mac = Mac::new(&format!("mac-files-refused-signers-{n}"));
        fs::create_dir_all(mac.etc()).expect("etc");
        fs::write(&mac.signers, "signers = [\"keep\"]\n").expect("existing");
        mac.write(&mac.managed_pkg, top);
        let error = mac.start(true);
        assert!(matches!(error, StartError::MacFiles(_)), "{error}");
        assert_eq!(error.exit_code(), EXIT_CONFIG);
        let said = error.to_string();
        let refusal = signers_refusal(&mac.managed_pkg, &toml);
        assert_eq!(
            said,
            format!("a file is not made from the managed preferences: {refusal}")
        );
        assert_eq!(
            fs::read_to_string(&mac.signers).expect("kept"),
            "signers = [\"keep\"]\n"
        );
        assert_eq!(mac.etc_names(), ["macos-pkg.toml"]);
    }
    let store_cases: Vec<(Dictionary, String)> = vec![
        (
            dict(vec![
                ("store", Value::Array(vec![Value::Dictionary(dict(vec![
                    ("name", text("a")), ("kind", text("https")),
                    ("base", text("https://a.example/")), ("flavour", text("x")),
                ]))])),
            ]),
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"https://a.example/\"\nflavour = \"x\"\n".to_string(),
        ),
        (
            dict(vec![
                ("store", Value::Array(vec![Value::Dictionary(dict(vec![
                    ("name", text("a")), ("kind", text("https")), ("base", text("http://a.example/")),
                ]))])),
            ]),
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"http://a.example/\"\n".to_string(),
        ),
        (
            store("a", vec![("bearer_file", text(bad_credential.to_str().expect("utf8")))]),
            format!(
                "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"https://store.example/\"\nbearer_file = \"{}\"\n",
                bad_credential.display()
            ),
        ),
    ];
    for (n, (top, toml)) in store_cases.into_iter().enumerate() {
        let mac = Mac::new(&format!("mac-files-refused-stores-{n}"));
        fs::create_dir_all(mac.etc()).expect("etc");
        fs::write(&mac.stores, "# keep\n").expect("existing");
        mac.write(&mac.managed_stores, top);
        let error = mac.start(true);
        assert!(matches!(error, StartError::MacFiles(_)), "{error}");
        assert_eq!(error.exit_code(), EXIT_CONFIG);
        let refusal = stores_refusal(&mac.managed_stores, &toml);
        assert_eq!(
            error.to_string(),
            format!("a file is not made from the managed preferences: {refusal}")
        );
        assert_eq!(fs::read_to_string(&mac.stores).expect("kept"), "# keep\n");
        assert_eq!(mac.etc_names(), ["stores.toml"]);
    }
}

/// Catches a managed file with no TOML form being accepted or described wrongly: bytes that are
/// not a property list, a list at the top, a date, data, and an integer over 64 bits are each
/// refused with their reason and name the domain's file, and nothing is written.
#[test]
fn a_managed_file_with_no_toml_form_is_refused_with_its_reason() {
    let mac = Mac::new("mac-files-render");
    let shown = mac.managed_pkg.display().to_string();
    let prefix =
        format!("the managed preferences {shown} of dev.hostkeeper.macos-pkg are refused: ");
    let one = |value: Value| -> Vec<u8> {
        let mut bytes = Vec::new();
        Value::Dictionary(dict(vec![("signers", value)]))
            .to_writer_xml(&mut bytes)
            .expect("encode");
        bytes
    };
    let mut array = Vec::new();
    Value::Array(vec![])
        .to_writer_xml(&mut array)
        .expect("encode");
    let big = one(Value::Integer(u64::MAX.into()));
    let cases: Vec<(Vec<u8>, String)> = vec![
        (array, "not a dictionary at its top level".to_string()),
        (
            one(Value::Date(
                plist::Date::from_xml_format("2027-01-02T03:04:05Z").expect("date"),
            )),
            "signers is a date, which a TOML file cannot hold".to_string(),
        ),
        (
            one(Value::Data(vec![1])),
            "signers is data, which a TOML file cannot hold".to_string(),
        ),
        (
            big,
            "signers does not fit a signed 64-bit integer".to_string(),
        ),
    ];
    for (bytes, reason) in cases {
        mac.write_bytes(&mac.managed_pkg, &bytes);
        let error = mac.pkg().expect_err("refused");
        assert_eq!(error.to_string(), format!("{prefix}{reason}"));
    }
    mac.write_bytes(&mac.managed_pkg, b"signers = []\n");
    let error = mac.pkg().expect_err("refused").to_string();
    assert!(
        error.starts_with(&format!("{prefix}not a property list: ")),
        "{error}"
    );
    assert!(!mac.etc().exists());
}

/// Catches a managed file that is not trusted being read: another owner, a mode a group or others
/// can write, a link and a directory are each refused ("are refused"), and nothing is written.
#[test]
fn an_untrusted_managed_file_is_refused() {
    let mac = Mac::new("mac-files-trust");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    let prefix = format!(
        "the managed preferences {} of dev.hostkeeper.macos-pkg are refused: ",
        mac.managed_pkg.display()
    );
    let other = me() + 1;
    let error = mac_files::macos_pkg(&mac.managed_pkg, &mac.signers, other).expect_err("owner");
    assert_eq!(
        error.to_string(),
        format!("{prefix}owned by uid {}, not the trusted uid {other}", me())
    );
    fs::set_permissions(&mac.managed_pkg, fs::Permissions::from_mode(0o666)).expect("mode");
    let error = mac.pkg().expect_err("mode").to_string();
    assert_eq!(
        error,
        format!("{prefix}mode 666 lets the group or others write it")
    );
    fs::remove_file(&mac.managed_pkg).expect("remove");
    let real = mac.root().join("real.plist");
    mac.write(&real, signers_of(&[LEAF_A]));
    symlink(&real, &mac.managed_pkg).expect("link");
    assert!(
        mac.pkg()
            .expect_err("link")
            .to_string()
            .starts_with(&prefix)
    );
    fs::remove_file(&mac.managed_pkg).expect("remove");
    fs::create_dir(&mac.managed_pkg).expect("dir");
    assert!(mac.pkg().expect_err("dir").to_string().starts_with(&prefix));
    assert!(!mac.etc().exists());
}

/// Catches a link or a directory at the target path being replaced: both are refused naming the
/// path, the link still points at its file (which is not written through) and the directory is
/// still a directory.
#[test]
fn a_link_or_a_directory_at_the_target_is_refused_and_left() {
    let mac = Mac::new("mac-files-notfile");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    mac.write(&mac.managed_stores, store("lan", vec![]));
    fs::create_dir_all(mac.etc()).expect("etc");
    let real = mac.root().join("real.toml");
    fs::write(&real, "# real\n").expect("real");
    symlink(&real, &mac.signers).expect("link");
    fs::create_dir(&mac.stores).expect("dir");
    for (error, path) in [
        (mac.pkg().expect_err("link"), &mac.signers),
        (mac.stores().expect_err("dir"), &mac.stores),
    ] {
        assert!(matches!(error, MacFilesError::NotAFile { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "{} is there but is not a regular file; it is not replaced",
                path.display()
            )
        );
    }
    let error = mac.start(true);
    assert!(matches!(error, StartError::MacFiles(_)), "{error}");
    assert_eq!(error.exit_code(), EXIT_CONFIG);
    assert_eq!(fs::read_link(&mac.signers).expect("still a link"), real);
    assert_eq!(fs::read_to_string(&real).expect("real"), "# real\n");
    assert!(mac.stores.is_dir());
    assert_eq!(mac.etc_names(), ["macos-pkg.toml", "stores.toml"]);
}

/// Catches a filesystem failure being swallowed or a temporary being left behind: a directory
/// that cannot be made (a dangling link in its place) and a temporary name already taken are each
/// an `Io` error naming the target, and a target path under a file is one too.
#[test]
fn a_failure_to_write_is_an_error_naming_the_target() {
    let mac = Mac::new("mac-files-io");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    symlink(mac.root().join("nowhere"), mac.etc()).expect("a dangling link");
    let error = mac.pkg().expect_err("no directory");
    assert!(
        matches!(&error, MacFilesError::Io { path, .. } if *path == mac.signers),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .starts_with(&format!("{}: ", mac.signers.display())),
        "{error}"
    );
    fs::remove_file(mac.etc()).expect("remove the link");
    fs::create_dir(mac.etc()).expect("etc");
    let taken = mac
        .etc()
        .join(format!(".hostkeeper-mac-files.{}.tmp", std::process::id()));
    fs::create_dir(&taken).expect("a directory of the temporary's name");
    let error = mac.pkg().expect_err("name taken");
    assert!(matches!(error, MacFilesError::Io { .. }), "{error}");
    assert!(taken.is_dir());
    assert!(!mac.signers.exists());
    fs::remove_dir(&taken).expect("remove");
    let under_file = mac.root().join("a file");
    fs::write(&under_file, "x").expect("file");
    let error = mac_files::macos_pkg(&mac.managed_pkg, &under_file.join("macos-pkg.toml"), me())
        .expect_err("under a file");
    assert!(matches!(error, MacFilesError::Io { .. }), "{error}");
}

/// Catches a start killed between making its temporary file and renaming it that stops every
/// later start with the same process id: a regular file left at the temporary's name is removed,
/// and the target is written.
#[test]
fn a_temporary_left_by_a_killed_start_is_removed() {
    let mac = Mac::new("mac-files-stale");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    fs::create_dir(mac.etc()).expect("etc");
    let stale = mac
        .etc()
        .join(format!(".hostkeeper-mac-files.{}.tmp", std::process::id()));
    fs::write(&stale, "left behind").expect("a stale temporary");
    assert_eq!(mac.pkg().expect("written"), Rendered::Written);
    assert_eq!(mac.etc_names(), vec!["macos-pkg.toml".to_owned()]);
    signers::load(&mac.signers, me())
        .expect("the loader reads it")
        .expect("it is there");
}

/// Catches a file with the rendered bytes that the loader would still refuse being kept: a target
/// with the right bytes but mode 0666 is written again, mode 0644.
#[test]
fn a_file_with_the_rendered_bytes_and_another_mode_is_written_again() {
    let mac = Mac::new("mac-files-mode");
    mac.write(&mac.managed_pkg, signers_of(&[LEAF_A]));
    assert_eq!(mac.pkg().expect("first"), Rendered::Written);
    fs::set_permissions(&mac.signers, fs::Permissions::from_mode(0o666)).expect("opened up");
    assert_eq!(mac.pkg().expect("second"), Rendered::Written);
    let mode = fs::metadata(&mac.signers)
        .expect("the file")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o644);
    assert_eq!(mac.pkg().expect("third"), Rendered::Unchanged);
}

/// Catches a managed stores domain the loader accepts as text that would make a file the start's
/// load refuses as too large, after the working file was replaced: it is refused before any
/// write, naming its size and the cap, and the file there stays as it was.
#[test]
fn a_domain_that_renders_over_the_loaders_cap_is_refused_and_changes_nothing() {
    let mac = Mac::new("mac-files-large");
    mac.write(&mac.managed_stores, store("lan", vec![]));
    assert_eq!(mac.stores().expect("first"), Rendered::Written);
    let before = fs::read(&mac.stores).expect("the file");
    let many: Vec<Value> = (0..800)
        .map(|n| {
            Value::Dictionary(dict(vec![
                ("name", text(&format!("store-{n}"))),
                ("kind", text("https")),
                ("base", text("https://store.example/a/path/long/enough/")),
            ]))
        })
        .collect();
    mac.write(
        &mac.managed_stores,
        dict(vec![("store", Value::Array(many))]),
    );
    let error = mac.stores().expect_err("too large");
    let MacFilesError::TooLarge { bytes, cap, .. } = error else {
        panic!("not TooLarge: {error}");
    };
    assert_eq!(cap, stores::MAX_STORES_BYTES);
    assert!(bytes as u64 > cap);
    assert_eq!(
        error_text(&mac),
        format!(
            "the managed preferences {} of dev.hostkeeper.stores render to {bytes} bytes, over the {cap} its file may hold",
            mac.managed_stores.display()
        )
    );
    assert_eq!(fs::read(&mac.stores).expect("the file"), before);
}

fn error_text(mac: &Mac) -> String {
    mac.stores().expect_err("refused").to_string()
}
