//! The farm values and the tunables: where they are read from and what refuses them.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use hostkeeper_bootstrap::settings::{
    Bounds, DEFAULT_RELEASE_CHANNEL, DEFAULT_TEAM_ID, Settings, SettingsError, Sources,
};

use crate::support::{CA, paths_in, scratch, settings_text, write_mode};

fn plist_file(path: &Path, dict: plist::Dictionary, mode: u32) {
    let mut bytes = Vec::new();
    plist::Value::Dictionary(dict)
        .to_writer_xml(&mut bytes)
        .expect("a plist");
    write_mode(path, std::str::from_utf8(&bytes).expect("text"), mode);
}

fn plist_binary(path: &Path, dict: plist::Dictionary, mode: u32) {
    let mut bytes = Vec::new();
    plist::Value::Dictionary(dict)
        .to_writer_binary(&mut bytes)
        .expect("a plist");
    fs::write(path, bytes).expect("write");
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode)).expect("chmod");
}

fn farm_dict(coordinator: &str) -> plist::Dictionary {
    let mut dict = plist::Dictionary::new();
    dict.insert("coordinator".into(), coordinator.into());
    dict.insert("ca_sha256".into(), CA.into());
    dict
}

fn sources(dir: &Path) -> Sources {
    Sources {
        managed: dir.join("managed.plist"),
        fallback: dir.join("bootstrap.toml"),
        trusted_uid: fs::metadata(dir).expect("dir").uid(),
    }
}

/// The farm's two values come from the managed preferences of dev.hostkeeper.config; every other
/// key has its default, and the default release channel and Team ID are the public ones.
#[test]
fn farm_values_are_read_from_the_managed_preference_domain() {
    let dir = scratch("settings-managed");
    plist_file(
        &dir.path().join("managed.plist"),
        farm_dict("https://managed.example:8443"),
        0o600,
    );
    let settings = sources(dir.path()).load().expect("settings");
    assert_eq!(settings.coordinator, "https://managed.example:8443");
    assert_eq!(settings.ca_sha256, CA);
    assert_eq!(settings.release_channel, DEFAULT_RELEASE_CHANNEL);
    assert_eq!(settings.team_id, DEFAULT_TEAM_ID);
    assert_eq!(settings.bounds, Bounds::default());
    assert!(settings.macos_pkg_signers.is_empty());
}

/// Tunables arrive in the managed domain as nested dictionaries, arrays and integers, and
/// override the defaults.
#[test]
fn the_managed_domain_carries_tunables_as_dictionaries_and_arrays() {
    let dir = scratch("settings-tunables");
    let mut dict = farm_dict("https://managed.example:8443");
    dict.insert("team_id".into(), "ABCDE12345".into());
    dict.insert("release_channel".into(), "https://mirror.example/".into());
    dict.insert(
        "macos_pkg_signers".into(),
        plist::Value::Array(vec![crate::support::leaf_hex().into()]),
    );
    let mut bounds = plist::Dictionary::new();
    bounds.insert("wait_secs".into(), plist::Value::Integer(120.into()));
    bounds.insert("retry_secs".into(), plist::Value::Integer(7.into()));
    dict.insert("bounds".into(), plist::Value::Dictionary(bounds));
    let mut signers = plist::Dictionary::new();
    signers.insert("hosted_runner".into(), false.into());
    dict.insert("trusted_signers".into(), plist::Value::Dictionary(signers));
    plist_file(&dir.path().join("managed.plist"), dict, 0o644);
    let settings = sources(dir.path()).load().expect("settings");
    assert_eq!(settings.team_id, "ABCDE12345");
    assert_eq!(settings.release_channel, "https://mirror.example");
    assert_eq!(settings.macos_pkg_signers, [crate::support::leaf_hex()]);
    assert_eq!(
        (settings.bounds.wait_secs, settings.bounds.retry_secs),
        (120, 7)
    );
    assert!(!settings.trusted_signers.hosted_runner);
    assert_eq!(settings.trusted_signers.organisation, "komira-ai");
}

/// With no managed preferences the fallback file of a manual install is the source.
#[test]
fn the_fallback_file_is_read_when_there_are_no_managed_preferences() {
    let dir = scratch("settings-fallback");
    let paths = paths_in(
        dir.path(),
        "[bounds]\nwait_secs = 5\nretry_secs = 5\nfetch_timeout_secs = 9\nconnect_timeout_secs = 3\nmax_package_bytes = 99\ntool_timeout_secs = 4\ninstaller_timeout_secs = 8\n\n[trusted_signers]\norganisation = \"acme\"\nworkflow = \"w.yml\"\nrefs = [\"refs/heads/main\"]\nhosted_runner = false\n",
    );
    let settings = paths.sources.load().expect("settings");
    assert_eq!(settings.coordinator, "https://coordinator.example:8443");
    assert_eq!(settings.bounds.max_package_bytes, 99);
    assert_eq!(settings.bounds.fetch_timeout_secs, 9);
    assert_eq!(settings.trusted_signers.organisation, "acme");
    assert_eq!(settings.trusted_signers.refs, ["refs/heads/main"]);
}

/// The managed preferences win over the fallback file when both are there.
#[test]
fn the_managed_preferences_win_over_the_fallback_file() {
    let dir = scratch("settings-both");
    let paths = paths_in(dir.path(), "");
    plist_file(
        &paths.sources.managed,
        farm_dict("https://managed.example"),
        0o600,
    );
    assert_eq!(
        paths.sources.load().expect("settings").coordinator,
        "https://managed.example"
    );
}

/// A managed file that is refused is an error: the fallback does not hide it.
#[test]
fn a_refused_managed_file_is_not_replaced_by_the_fallback() {
    let dir = scratch("settings-refused");
    let paths = paths_in(dir.path(), "");
    plist_file(
        &paths.sources.managed,
        farm_dict("https://managed.example"),
        0o666,
    );
    let error = paths.sources.load().expect_err("refused");
    assert!(matches!(error, SettingsError::Refused { .. }), "{error}");
    assert!(error.to_string().contains("managed.plist"), "{error}");
}

/// A file another account owns is refused as the daemon refuses its own config.
#[test]
fn a_source_owned_by_another_account_is_refused() {
    let dir = scratch("settings-owner");
    let paths = paths_in(dir.path(), "");
    let mut other = paths.sources.clone();
    other.trusted_uid += 1;
    assert!(matches!(other.load(), Err(SettingsError::Refused { .. })));
}

/// With neither source, the error names both paths.
#[test]
fn no_source_names_both_paths() {
    let dir = scratch("settings-none");
    let error = sources(dir.path()).load().expect_err("no source");
    assert!(matches!(error, SettingsError::NoSource { .. }));
    let text = error.to_string();
    assert!(
        text.contains("managed.plist") && text.contains("bootstrap.toml"),
        "{text}"
    );
}

/// A source larger than the bound is refused before it is parsed.
#[test]
fn an_oversized_source_is_refused() {
    let dir = scratch("settings-big");
    let paths = paths_in(dir.path(), "");
    write_mode(&paths.sources.fallback, &"# padding\n".repeat(8000), 0o600);
    assert!(matches!(
        paths.sources.load(),
        Err(SettingsError::TooLarge { .. })
    ));
}

/// Each way a source can be wrong is refused with its own reason.
#[test]
fn bad_sources_are_refused_with_the_reason() {
    let dir = scratch("settings-bad");
    let path = dir.path().join("bootstrap.toml");
    let fallback = sources(dir.path());
    let load = |text: &str| {
        write_mode(&path, text, 0o600);
        fallback.load().expect_err("refused").to_string()
    };
    assert!(load("coordinator = \"https://c.example\"\n").contains("no ca_sha256"));
    assert!(load(&format!("ca_sha256 = \"{CA}\"\n")).contains("no coordinator"));
    assert!(load(&settings_text("surprise = 1\n")).contains("not settings"));
    assert!(load(&settings_text("team_id = \"u7rtx4xd3x\"\n")).contains("team_id"));
    assert!(load(&settings_text("team_id = \"SHORT\"\n")).contains("team_id"));
    assert!(
        load(&settings_text(
            "macos_pkg_signers = [\"1\",\"2\",\"3\",\"4\",\"5\"]\n"
        ))
        .contains("more than 4")
    );
    assert!(load(&settings_text("macos_pkg_signers = [\"12\"]\n")).contains("64 hex digits"));
    assert!(load(&settings_text("[bounds]\nretry_secs = 0\n")).contains("bounds.retry_secs"));
    let daemon_refuses = format!("coordinator = \"http://plain.example\"\nca_sha256 = \"{CA}\"\n");
    assert!(load(&daemon_refuses).contains("daemon would refuse"));
    write_mode(&path, "", 0o600);
    fs::write(&path, [0xff, 0xfe]).expect("bytes");
    assert!(
        fallback
            .load()
            .expect_err("refused")
            .to_string()
            .contains("not UTF-8")
    );
}

/// Property lists that cannot be settings are refused: not a plist, not a dictionary, and values
/// with no settings form, each naming its key.
#[test]
fn bad_managed_preferences_are_refused_with_the_reason() {
    let dir = scratch("settings-bad-plist");
    let managed = dir.path().join("managed.plist");
    let source = sources(dir.path());
    write_mode(&managed, "not a plist", 0o600);
    assert!(matches!(source.load(), Err(SettingsError::NotPlist { .. })));
    let mut bytes = Vec::new();
    plist::Value::Array(Vec::new())
        .to_writer_xml(&mut bytes)
        .expect("plist");
    write_mode(&managed, std::str::from_utf8(&bytes).expect("text"), 0o600);
    assert!(
        source
            .load()
            .expect_err("refused")
            .to_string()
            .contains("not a dictionary")
    );
    let cases: [(&str, plist::Value, &str); 5] = [
        ("a", plist::Value::Real(1.5), "a real"),
        (
            "b",
            plist::Value::Date(plist::Date::from_xml_format("2026-11-14T10:00:00Z").expect("date")),
            "a date",
        ),
        ("c", plist::Value::Data(vec![1]), "data"),
        (
            "d",
            plist::Value::Integer(plist::Integer::from(u64::MAX)),
            "does not fit",
        ),
        ("e", plist::Value::Uid(plist::Uid::new(1)), "a uid"),
    ];
    for (key, value, kind) in cases {
        let mut dict = farm_dict("https://c.example");
        dict.insert(key.into(), plist::Value::Array(vec![value]));
        plist_binary(&managed, dict, 0o600);
        let text = source.load().expect_err("refused").to_string();
        assert!(
            text.contains(&format!("holds {key}")) && text.contains(kind),
            "{text}"
        );
    }
}

/// The daemon config the settings make is exactly what the daemon's loader reads, with the
/// farm values, the channel and the signers.
#[test]
fn the_daemon_config_is_the_text_the_daemons_loader_accepts() {
    let dir = scratch("settings-daemon");
    let paths = paths_in(dir.path(), "");
    let settings: Settings = paths.sources.load().expect("settings");
    let text = settings.daemon_config();
    let config = hostkeeper_config::daemon::DaemonConfig::parse(&text).expect("a daemon config");
    assert_eq!(
        config.coordinator.as_str(),
        "https://coordinator.example:8443"
    );
    assert_eq!(config.ca_sha256.as_str(), CA);
    assert_eq!(config.trusted_signers.organisation, "komira-ai");
    assert!(text.starts_with("# Written by hostkeeper-bootstrap"));
}

/// A managed file that is a link is refused (it is opened without following it), and is not
/// replaced by the fallback.
#[test]
fn a_managed_link_is_refused() {
    let dir = scratch("settings-link");
    let paths = paths_in(dir.path(), "");
    std::os::unix::fs::symlink(&paths.sources.fallback, &paths.sources.managed).expect("link");
    assert!(matches!(
        paths.sources.load(),
        Err(SettingsError::Refused { .. })
    ));
}
