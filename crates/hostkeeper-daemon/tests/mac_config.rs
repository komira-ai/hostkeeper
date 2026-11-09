//! The daemon config of a Mac, written from the managed preferences the MDM installed by the
//! start of a Mac's daemon when its config is not there, before it reads it
//! ([`hostkeeper_daemon::mac_config::at_start`], docs/design/two-programs.md, section 2.1). Every
//! path is in a scratch directory, and the trusted owner is the uid the tests run as (the host's
//! start trusts root).

use std::cell::Cell;
use std::fs;
use std::num::NonZeroU64;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::{Bounds, DaemonConfig};
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_daemon::cli::{self, DEFAULT_CONFIG, EXIT_CONFIG, HostFiles};
use hostkeeper_daemon::mac_config::{
    Configured, HEADER, MacConfigError, mac_config, mac_config_with,
};
use hostkeeper_daemon::notify::NoNotifier;
use hostkeeper_daemon::start::{Settings, StartError};
use hostkeeper_launchd::postinstall::{DAEMON_CONFIG, MANAGED_CONFIG};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_testkit::Scratch;
use plist::{Dictionary, Value};

/// A scratch directory with a managed file's path and a config path (inside a directory that
/// does not exist yet), both holding a space.
struct Mac {
    scratch: Scratch,
    managed: PathBuf,
    config: PathBuf,
}

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

impl Mac {
    fn new(name: &str) -> Mac {
        let scratch =
            Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch dir");
        let managed = scratch.path().join("Managed Preferences.plist");
        let config = scratch
            .path()
            .join("etc hostkeeper")
            .join("hostkeeper.toml");
        Mac {
            scratch,
            managed,
            config,
        }
    }

    fn root(&self) -> &Path {
        self.scratch.path()
    }

    fn parent(&self) -> &Path {
        self.config.parent().expect("a parent")
    }

    fn run(&self) -> Result<Configured, MacConfigError> {
        mac_config(&self.managed, &self.config, me())
    }

    /// The daemon's start ([`cli::start_with`]) on this Mac's config and managed preferences,
    /// their trusted owner the test's uid, its packages directory a file: a start that gets past
    /// its config stops there, `Io` naming it, before it builds anything of the host. The
    /// platform's adapters are Linux's, which a start on any build makes.
    fn start(&self) -> StartError {
        self.start_reading(Some(self.managed.clone()))
    }

    /// [`Mac::start`] with `managed` as the host's managed preferences: none is a Linux host's.
    fn start_reading(&self, managed: Option<PathBuf>) -> StartError {
        let packages = self.root().join("packages");
        if !packages.exists() {
            fs::write(&packages, b"not a directory").expect("a file at the packages path");
        }
        let settings = Settings {
            config: self.config.clone(),
            state_dir: self.root().join("state"),
            packages_dir: packages,
            run_dir: self.root().join("run"),
            trusted_uid: me(),
            trusted_root_max_age_days: NonZeroU64::MIN,
        };
        let files = HostFiles {
            os_root: self.root().join("os"),
            ca_bundle: self.root().join("ca.pem"),
            gh: self.root().join("gh"),
            trusted_root: self.root().join("trusted_root.json"),
            stores: self.root().join("stores.toml"),
            macos_pkg_signers: None,
            pki: self.root().join("pki"),
            console: None,
            managed_config: managed,
        };
        let own = HostkeeperVersion::parse("1.0.0").expect("a version");
        cli::start_with(&settings, &files, own, &NoNotifier, &Platform::linux())
            .err()
            .expect("the start stops")
    }

    /// The start went past its config, written or kept, and loaded it: it stopped at the
    /// packages directory that is a file.
    fn past_config(&self, error: &StartError) -> bool {
        matches!(error, StartError::Io { path, .. } if *path == self.root().join("packages"))
    }

    /// The names in the config's directory.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.parent())
            .expect("the config's directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn write_xml(&self, top: Dictionary) {
        let mut bytes = Vec::new();
        Value::Dictionary(top)
            .to_writer_xml(&mut bytes)
            .expect("encode");
        self.write_managed(&bytes);
    }

    fn write_binary(&self, top: Dictionary) {
        let mut bytes = Vec::new();
        Value::Dictionary(top)
            .to_writer_binary(&mut bytes)
            .expect("encode");
        self.write_managed(&bytes);
    }

    fn write_managed(&self, bytes: &[u8]) {
        fs::write(&self.managed, bytes).expect("the managed file");
        fs::set_permissions(&self.managed, fs::Permissions::from_mode(0o644)).expect("its mode");
    }

    fn error(&self) -> String {
        self.run().expect_err("refused").to_string()
    }
}

fn dict(pairs: Vec<(&str, Value)>) -> Dictionary {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

const LOADER_REFUSAL: &str = "the managed config does not make a daemon config: ";

/// A managed dictionary the loader accepts, with every key and every bound.
fn full() -> Dictionary {
    dict(vec![
        ("coordinator", text("https://coordinator.example:8443")),
        (
            "ca_sha256",
            text("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
        ),
        ("release_channel", text("https://releases.example/path")),
        (
            "trusted_signers",
            Value::Dictionary(dict(vec![
                ("organisation", text("komira-ai")),
                ("workflow", text(".github/workflows/release.yml")),
                (
                    "refs",
                    Value::Array(vec![text("refs/tags/*"), text("refs/heads/main")]),
                ),
                ("hosted_runner", Value::Boolean(true)),
            ])),
        ),
        (
            "bounds",
            Value::Dictionary(dict(vec![
                ("max_package_bytes", Value::Integer(1000.into())),
                ("max_unpacked_bytes", Value::Integer(5000.into())),
                ("max_entries", Value::Integer(77.into())),
                ("fetch_timeout_secs", Value::Integer(600.into())),
                ("attestation_timeout_secs", Value::Integer(30.into())),
                ("force_start_secs", Value::Integer(15.into())),
            ])),
        ),
    ])
}

/// The table of `top` named `table`.
fn inner<'a>(top: &'a mut Dictionary, table: &str) -> &'a mut Dictionary {
    match top.get_mut(table) {
        Some(Value::Dictionary(inner)) => inner,
        other => panic!("no table {table}: {other:?}"),
    }
}

fn without(mut top: Dictionary, path: &[&str]) -> Dictionary {
    match path {
        [key] => top.remove(key),
        [table, key] => inner(&mut top, table).remove(key),
        _ => panic!("a path of one or two keys"),
    };
    top
}

fn with(mut top: Dictionary, path: &[&str], value: Value) -> Dictionary {
    match path {
        [key] => top.insert((*key).to_string(), value),
        [table, key] => inner(&mut top, table).insert((*key).to_string(), value),
        _ => panic!("a path of one or two keys"),
    };
    top
}

/// Catches a default config path that is not the one the pkg installs and the daemon reads, and a
/// managed default that is not the pkg's: a Mac's start reads the managed preferences at
/// `MANAGED_CONFIG` and a Linux host has none.
#[test]
fn the_defaults_are_the_paths_the_pkg_names() {
    assert_eq!(DEFAULT_CONFIG, DAEMON_CONFIG);
    let mac = HostFiles::for_platform(PlatformKind::Macos, |_| false);
    assert_eq!(
        mac.managed_config.as_deref(),
        Some(Path::new(MANAGED_CONFIG))
    );
    let linux = HostFiles::for_platform(PlatformKind::Linux, |_| false);
    assert_eq!(linux.managed_config, None);
}

/// Catches a rendering that loses a key, a bound or a signer, or that the daemon's own loader
/// reads differently from the managed preferences, in a binary plist and in an XML one.
#[test]
fn mac_config_writes_the_toml_the_loader_accepts() {
    for binary in [false, true] {
        let mac = Mac::new("mac-config-writes");
        match binary {
            true => mac.write_binary(full()),
            false => mac.write_xml(full()),
        }
        assert_eq!(mac.run().expect("written"), Configured::Written);
        let written = fs::read_to_string(&mac.config).expect("the config");
        assert!(written.starts_with(HEADER), "{written}");
        let config = DaemonConfig::parse(&written).expect("the loader accepts it");
        assert_eq!(
            config.coordinator.as_str(),
            "https://coordinator.example:8443"
        );
        assert_eq!(
            config.ca_sha256.as_str(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            config.release_channel.as_str(),
            "https://releases.example/path"
        );
        assert_eq!(config.trusted_signers.organisation, "komira-ai");
        assert_eq!(
            config.trusted_signers.workflow,
            ".github/workflows/release.yml"
        );
        assert_eq!(
            config.trusted_signers.refs,
            ["refs/tags/*", "refs/heads/main"]
        );
        assert!(config.trusted_signers.hosted_runner);
        assert_eq!(config.bounds.max_package_bytes, 1000);
        assert_eq!(config.bounds.max_unpacked_bytes, 5000);
        assert_eq!(config.bounds.max_entries, 77);
        assert_eq!(config.bounds.fetch_timeout_secs, 600);
        assert_eq!(config.bounds.attestation_timeout_secs, 30);
        assert_eq!(config.bounds.force_start_secs, 15);
    }
}

/// Catches a config without bounds that is refused: the table is optional, as in a hand-written
/// file, and the loader's defaults apply.
#[test]
fn the_bounds_table_is_optional() {
    let mac = Mac::new("mac-config-no-bounds");
    mac.write_xml(without(full(), &["bounds"]));
    assert_eq!(mac.run().expect("written"), Configured::Written);
    let config = DaemonConfig::parse(&fs::read_to_string(&mac.config).expect("config"))
        .expect("the loader accepts it");
    assert_eq!(config.bounds, Bounds::default());
}

/// Catches a managed config missing a required key that is written anyway, and a refusal that
/// leaves a directory or a file behind.
#[test]
fn each_missing_required_key_is_refused_with_the_loaders_reason() {
    let keys: [&[&str]; 7] = [
        &["coordinator"],
        &["ca_sha256"],
        &["release_channel"],
        &["trusted_signers"],
        &["trusted_signers", "organisation"],
        &["trusted_signers", "workflow"],
        &["trusted_signers", "refs"],
    ];
    for key in keys {
        let mac = Mac::new("mac-config-missing");
        mac.write_xml(without(full(), key));
        let message = mac.error();
        assert!(
            message.starts_with(&format!("{LOADER_REFUSAL}the daemon config")),
            "{key:?}: {message}"
        );
        assert!(!mac.parent().exists(), "{key:?}");
    }
}

/// Catches an unknown key, at the top and in a table, that is written instead of refused.
#[test]
fn an_unknown_key_is_refused_with_the_loaders_reason() {
    let mac = Mac::new("mac-config-unknown");
    mac.write_xml(with(full(), &["extra"], text("x")));
    assert_eq!(
        mac.error(),
        format!(
            "{LOADER_REFUSAL}the daemon config has the unknown key \"extra\" in the table \"\""
        )
    );
    mac.write_xml(with(full(), &["bounds", "speed"], Value::Integer(1.into())));
    assert_eq!(
        mac.error(),
        format!(
            "{LOADER_REFUSAL}the daemon config has the unknown key \"speed\" in the table \"bounds\""
        )
    );
    assert!(!mac.parent().exists());
}

/// Catches a bad value (a bound of 0, a boolean for a URL, a real for a count) written instead of
/// refused by the loader.
#[test]
fn a_bad_value_is_refused_with_the_loaders_reason() {
    let mac = Mac::new("mac-config-bad-value");
    mac.write_xml(with(
        full(),
        &["bounds", "max_entries"],
        Value::Integer(0.into()),
    ));
    assert_eq!(
        mac.error(),
        format!(
            "{LOADER_REFUSAL}the daemon config's bounds.max_entries is 0; every bound is from 1 up"
        )
    );
    mac.write_xml(with(full(), &["coordinator"], Value::Boolean(true)));
    assert!(mac.error().starts_with(LOADER_REFUSAL));
    mac.write_xml(with(full(), &["bounds", "max_entries"], Value::Real(1.5)));
    assert!(mac.error().starts_with(LOADER_REFUSAL));
}

/// Catches a start of a Mac's daemon with no config that does not make one from the managed
/// preferences, or makes one it then does not read: the config is written (the header, mode
/// 0600), the start loads it and goes on; a second start keeps it. A Linux host's start (no
/// managed preferences) never reads them, and stops at its missing config.
#[test]
fn the_start_writes_the_config_from_the_managed_preferences_and_reads_it() {
    let mac = Mac::new("mac-config-start-writes");
    mac.write_binary(full());
    let error = mac.start_reading(None);
    assert!(
        matches!(&error, StartError::Config { path, .. } if *path == mac.config),
        "{error}"
    );
    assert!(!mac.parent().exists());

    let error = mac.start();
    assert!(mac.past_config(&error), "{error}");
    let written = fs::read_to_string(&mac.config).expect("the config");
    assert!(written.starts_with(HEADER), "{written}");
    DaemonConfig::parse(&written).expect("the loader accepts it");
    assert_eq!(
        fs::metadata(&mac.config)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::remove_file(&mac.managed).expect("remove");
    let error = mac.start();
    assert!(mac.past_config(&error), "{error}");
    assert_eq!(fs::read_to_string(&mac.config).expect("kept"), written);
    assert_eq!(mac.names(), ["hostkeeper.toml"]);
}

/// Catches a start that reads the managed file, or changes the config, when a config is there:
/// the file's bytes stay, the managed file is not even required, and the start reads the config
/// that is there (here one its loader refuses, naming it).
#[test]
fn a_config_already_there_is_kept_by_the_start() {
    let mac = Mac::new("mac-config-keeps");
    fs::create_dir(mac.parent()).expect("dir");
    fs::write(&mac.config, b"the operator's config").expect("config");
    // A managed config is present and valid; it is not used.
    mac.write_xml(full());
    let error = mac.start();
    assert!(
        matches!(&error, StartError::Config { path, .. } if *path == mac.config),
        "{error}"
    );
    assert_eq!(error.exit_code(), EXIT_CONFIG);
    assert_eq!(mac.run().expect("kept"), Configured::Kept);
    assert_eq!(
        fs::read(&mac.config).expect("config"),
        b"the operator's config"
    );
    assert_eq!(mac.names(), ["hostkeeper.toml"]);
    // Nor is it read: with no managed file at all the config is kept.
    fs::remove_file(&mac.managed).expect("remove");
    let error = mac.start();
    assert!(
        matches!(&error, StartError::Config { path, .. } if *path == mac.config),
        "{error}"
    );
    assert_eq!(mac.run().expect("kept"), Configured::Kept);
}

/// Catches a link or a directory at the config path being kept as a config, which would load a
/// daemon whose loader refuses it (it opens the config without following a link): each is refused
/// by the start (exit 3) and by `mac_config`, named, left as it is, and the managed file is not used
/// to replace it.
#[test]
fn a_link_or_a_directory_at_the_config_path_is_refused_and_left() {
    let mac = Mac::new("mac-config-kept-kinds");
    mac.write_xml(full());
    fs::create_dir(mac.parent()).expect("dir");
    let target = mac.root().join("elsewhere.toml");
    fs::write(&target, b"elsewhere").expect("target");
    let refused = format!(
        "{} is there but is not a regular file; it is not replaced",
        mac.config.display()
    );
    symlink(&target, &mac.config).expect("link");
    let error = mac.start();
    assert!(
        matches!(
            &error,
            StartError::MacConfig(MacConfigError::NotAFile { .. })
        ),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!("the daemon config is not made from the managed preferences: {refused}")
    );
    assert_eq!(error.exit_code(), EXIT_CONFIG);
    assert_eq!(mac.error(), refused);
    assert!(
        fs::symlink_metadata(&mac.config)
            .expect("link")
            .is_symlink()
    );
    assert_eq!(fs::read(&target).expect("target"), b"elsewhere");
    fs::remove_file(&mac.config).expect("remove");
    fs::create_dir(&mac.config).expect("dir");
    let error = mac.start();
    assert!(
        matches!(
            &error,
            StartError::MacConfig(MacConfigError::NotAFile { .. })
        ),
        "{error}"
    );
    assert_eq!(mac.error(), refused);
    assert!(mac.config.is_dir());
    assert_eq!(mac.names(), ["hostkeeper.toml"]);
}

/// Catches a start that goes on with no config and no managed file, which would run a daemon that
/// cannot start: the start stops with the config's exit status (3) naming both files, before it
/// makes anything, and `mac_config` fails the same way.
#[test]
fn with_no_managed_file_and_no_config_the_start_fails() {
    let mac = Mac::new("mac-config-neither");
    let neither = format!(
        "neither a managed config ({}) nor a config ({}) is there",
        mac.managed.display(),
        mac.config.display()
    );
    let error = mac.start();
    assert!(
        matches!(
            &error,
            StartError::MacConfig(MacConfigError::Neither { .. })
        ),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!("the daemon config is not made from the managed preferences: {neither}")
    );
    assert_eq!(error.exit_code(), EXIT_CONFIG);
    assert!(!mac.parent().exists());
    assert!(!mac.root().join("state").exists());
    assert_eq!(mac.error(), neither);
    assert!(!mac.parent().exists());
}

/// Catches a managed file owned by someone other than the trusted uid being read.
#[test]
fn a_managed_file_of_another_owner_is_refused() {
    let mac = Mac::new("mac-config-owner");
    mac.write_xml(full());
    let other = me() + 1;
    let message = mac_config(&mac.managed, &mac.config, other)
        .expect_err("refused")
        .to_string();
    assert_eq!(
        message,
        format!(
            "the managed config {} is refused: owned by uid {}, not the trusted uid {other}",
            mac.managed.display(),
            me()
        )
    );
    assert!(!mac.parent().exists());
}

/// Catches a managed file that the group or others can write being read.
#[test]
fn a_group_or_other_writable_managed_file_is_refused() {
    for mode in [0o664, 0o646] {
        let mac = Mac::new("mac-config-writable");
        mac.write_xml(full());
        fs::set_permissions(&mac.managed, fs::Permissions::from_mode(mode)).expect("mode");
        assert_eq!(
            mac.error(),
            format!(
                "the managed config {} is refused: mode {mode:o} lets the group or others write it",
                mac.managed.display()
            )
        );
        assert!(!mac.parent().exists());
    }
}

/// Catches a managed file that is a link (followed to a file the trust check did not see) or a
/// directory being read.
#[test]
fn a_managed_link_or_directory_is_refused() {
    let mac = Mac::new("mac-config-kinds");
    let target = mac.root().join("target.plist");
    fs::write(&target, b"x").expect("target");
    symlink(&target, &mac.managed).expect("link");
    let message = mac.error();
    assert!(
        message.starts_with(&format!(
            "the managed config {} is refused: ",
            mac.managed.display()
        )),
        "{message}"
    );
    fs::remove_file(&mac.managed).expect("remove");
    fs::create_dir(&mac.managed).expect("dir");
    assert_eq!(
        mac.error(),
        format!(
            "the managed config {} is refused: not a regular file",
            mac.managed.display()
        )
    );
    assert!(!mac.parent().exists());
}

/// Catches a managed file that is not a property list being written through.
#[test]
fn a_file_that_is_not_a_plist_is_refused() {
    let mac = Mac::new("mac-config-not-plist");
    mac.write_managed(b"coordinator = \"https://x\"\n");
    let message = mac.error();
    assert!(
        message.starts_with(&format!(
            "the managed config {} is not a property list: ",
            mac.managed.display()
        )),
        "{message}"
    );
    assert!(!mac.parent().exists());
}

/// Catches a property list whose top level is not a dictionary being accepted.
#[test]
fn a_top_level_that_is_not_a_dictionary_is_refused() {
    let mac = Mac::new("mac-config-array");
    let mut bytes = Vec::new();
    Value::Array(vec![text("a")])
        .to_writer_xml(&mut bytes)
        .expect("encode");
    mac.write_managed(&bytes);
    assert_eq!(
        mac.error(),
        format!(
            "the managed config {} is not a dictionary at its top level",
            mac.managed.display()
        )
    );
}

/// Catches a date, data or uid value being dropped or coerced instead of refused with its dotted
/// key, and an array element's key missing its index.
#[test]
fn values_the_config_cannot_hold_are_refused_naming_their_key() {
    let date = Value::Date(plist::Date::from_xml_format("2026-10-02T03:04:05Z").expect("date"));
    let cases = [
        (
            vec!["coordinator"],
            date,
            "the managed config's coordinator is a date, which the daemon config cannot hold",
        ),
        (
            vec!["trusted_signers", "workflow"],
            Value::Data(vec![1, 2]),
            "the managed config's trusted_signers.workflow is data, which the daemon config cannot hold",
        ),
        (
            vec!["ca_sha256"],
            Value::Uid(plist::Uid::new(3)),
            "the managed config's ca_sha256 is a uid, which the daemon config cannot hold",
        ),
        (
            vec!["trusted_signers", "refs"],
            Value::Array(vec![text("a"), Value::Data(vec![1])]),
            "the managed config's trusted_signers.refs[1] is data, which the daemon config cannot hold",
        ),
    ];
    for (path, value, message) in cases {
        let mac = Mac::new("mac-config-unsupported");
        mac.write_binary(with(full(), &path, value));
        assert_eq!(mac.error(), message);
        assert!(!mac.parent().exists());
    }
}

/// Catches an integer above `i64::MAX` wrapped to a negative number or clamped instead of
/// refused.
#[test]
fn an_integer_above_i64_max_is_refused() {
    let mac = Mac::new("mac-config-big");
    mac.write_binary(with(
        full(),
        &["bounds", "max_package_bytes"],
        Value::Integer(u64::MAX.into()),
    ));
    assert_eq!(
        mac.error(),
        "the managed config's bounds.max_package_bytes does not fit a signed 64-bit integer"
    );
}

/// Catches a config file with open permissions, a config directory that is not 0755, a
/// temporary file left beside it, and a missing parent chain not made.
#[test]
fn the_config_is_mode_0600_in_a_directory_made_0755_with_nothing_else_in_it() {
    let mac = Mac::new("mac-config-modes");
    mac.write_xml(full());
    let deep = mac.root().join("a").join("b").join("hostkeeper.toml");
    assert_eq!(
        mac_config(&mac.managed, &deep, me()).expect("written"),
        Configured::Written
    );
    let parent = deep.parent().expect("parent");
    assert_eq!(
        fs::metadata(parent).expect("dir").permissions().mode() & 0o7777,
        0o755
    );
    assert_eq!(mac.run().expect("written"), Configured::Written);
    assert_eq!(
        fs::metadata(&mac.config)
            .expect("file")
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert_eq!(mac.names(), ["hostkeeper.toml"]);
    // A second run keeps it.
    assert_eq!(mac.run().expect("kept"), Configured::Kept);
}

/// Catches a refused managed config that leaves a temporary file in an existing config
/// directory.
#[test]
fn a_refusal_leaves_no_temporary_file() {
    let mac = Mac::new("mac-config-clean");
    fs::create_dir(mac.parent()).expect("dir");
    mac.write_xml(without(full(), &["coordinator"]));
    mac.run().expect_err("refused");
    assert_eq!(mac.names(), Vec::<String>::new());
}

/// Catches a config that appears between the check and the link being replaced, `mac_config` failing
/// for it, or its temporary file staying: the file that appeared is kept untouched.
#[test]
fn a_config_that_appears_before_the_link_is_kept() {
    let mac = Mac::new("mac-config-race");
    mac.write_xml(full());
    let calls = Cell::new(0);
    let result = mac_config_with(&mac.managed, &mac.config, me(), &|| {
        calls.set(calls.get() + 1);
        fs::write(&mac.config, b"appeared").expect("the other writer");
    });
    assert_eq!(result.expect("kept"), Configured::Kept);
    assert_eq!(calls.get(), 1);
    assert_eq!(fs::read(&mac.config).expect("config"), b"appeared");
    assert_eq!(mac.names(), ["hostkeeper.toml"]);
}

/// Catches a directory that appears at the config path between the check and the link being
/// taken for a kept config: it is refused, and no temporary file stays.
#[test]
fn a_directory_that_appears_before_the_link_is_refused() {
    let mac = Mac::new("mac-config-race-dir");
    mac.write_xml(full());
    let result = mac_config_with(&mac.managed, &mac.config, me(), &|| {
        fs::create_dir(&mac.config).expect("the other writer");
    });
    assert_eq!(
        result.expect_err("refused").to_string(),
        format!(
            "{} is there but is not a regular file; it is not replaced",
            mac.config.display()
        )
    );
    assert!(mac.config.is_dir());
    assert_eq!(mac.names(), ["hostkeeper.toml"]);
}

/// Catches a temporary file of an earlier run (the same name) being overwritten or removed by a
/// run that could not create its own, and the failure being swallowed.
#[test]
fn a_temporary_file_that_is_there_fails_the_write_and_stays() {
    let mac = Mac::new("mac-config-tmp-exists");
    mac.write_xml(full());
    fs::create_dir(mac.parent()).expect("dir");
    let tmp = mac
        .parent()
        .join(format!(".hostkeeper-mac-config.{}.tmp", std::process::id()));
    fs::write(&tmp, b"not ours").expect("tmp");
    let message = mac.error();
    assert!(
        message.starts_with(&format!("{}: ", mac.config.display())),
        "{message}"
    );
    assert_eq!(fs::read(&tmp).expect("tmp"), b"not ours");
    assert!(!mac.config.exists());
}

/// Catches a config directory that cannot be made (a file stands there) being reported as
/// success.
#[test]
fn a_config_directory_that_cannot_be_made_fails() {
    let mac = Mac::new("mac-config-parent-file");
    mac.write_xml(full());
    fs::write(mac.parent(), b"a file").expect("file");
    let message = mac.error();
    assert!(
        message.starts_with(&format!("{}: ", mac.config.display())),
        "{message}"
    );
}

/// Catches a link that fails for another reason than the config existing being taken as kept: a
/// config path ending in a slash cannot be linked, and no temporary file stays.
#[test]
fn a_link_that_fails_is_an_error_and_leaves_no_temporary_file() {
    let mac = Mac::new("mac-config-link-fails");
    mac.write_xml(full());
    let slash = PathBuf::from(format!("{}/", mac.config.display()));
    let result = mac_config(&mac.managed, &slash, me());
    assert!(
        matches!(result, Err(MacConfigError::Io { .. })),
        "{result:?}"
    );
    assert_eq!(mac.names(), Vec::<String>::new());
}
