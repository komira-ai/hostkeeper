//! The platform values: the documented paths of each host kind, and which one a host is.

use std::path::{Path, PathBuf};

use hostkeeper_launchd::{
    OPENRC_MARKER, Platform, PlatformKind, REVERSE_DNS_PREFIX, SYSTEMD_MARKER,
};

fn linux() -> Platform {
    Platform {
        kind: PlatformKind::Linux,
        unit_dir: PathBuf::from("/etc/systemd/system"),
        unit_suffix: ".service",
        admin_socket: PathBuf::from("/run/hostkeeper/admin.sock"),
        label_prefix: None,
    }
}

fn macos() -> Platform {
    Platform {
        kind: PlatformKind::Macos,
        unit_dir: PathBuf::from("/Library/LaunchDaemons"),
        unit_suffix: ".plist",
        admin_socket: PathBuf::from("/var/run/hostkeeper/admin.sock"),
        label_prefix: Some(REVERSE_DNS_PREFIX),
    }
}

fn openrc() -> Platform {
    Platform {
        kind: PlatformKind::Openrc,
        unit_dir: PathBuf::from("/etc/init.d"),
        unit_suffix: "",
        admin_socket: PathBuf::from("/run/hostkeeper/admin.sock"),
        label_prefix: None,
    }
}

/// A filesystem on which exactly `dirs` are directories.
fn only(dirs: &'static [&'static str]) -> impl Fn(&Path) -> bool {
    move |path| dirs.iter().any(|dir| Path::new(dir) == path)
}

/// Catches a constructor that names the wrong unit directory, suffix, socket or label prefix.
#[test]
fn the_platform_constructors_name_the_documented_paths() {
    assert_eq!(Platform::linux(), linux());
    assert_eq!(Platform::macos(), macos());
    assert_eq!(Platform::openrc(), openrc());
    assert_eq!(Platform::for_os(false), linux());
    assert_eq!(Platform::for_os(true), macos());
    assert_eq!(SYSTEMD_MARKER, "/run/systemd/system");
    assert_eq!(OPENRC_MARKER, "/run/openrc");
}

/// Catches a Linux host read by anything but its running service manager: systemd's marker makes
/// a systemd host even beside OpenRC's (a host that has both runs systemd), OpenRC's alone an
/// OpenRC host, and neither a systemd host, as before OpenRC was known; a Mac build is a Mac
/// whatever its filesystem has.
#[test]
fn a_linux_host_is_read_by_the_service_manager_that_runs_it() {
    let systemd = only(&[SYSTEMD_MARKER]);
    let rc = only(&[OPENRC_MARKER]);
    let both = only(&[SYSTEMD_MARKER, OPENRC_MARKER]);
    let neither = only(&[]);
    assert_eq!(Platform::detect(false, &systemd), linux());
    assert_eq!(Platform::detect(false, &rc), openrc());
    assert_eq!(Platform::detect(false, &both), linux());
    assert_eq!(Platform::detect(false, &neither), linux());
    for fs in [&systemd as &dyn Fn(&Path) -> bool, &rc, &both, &neither] {
        assert_eq!(Platform::detect(true, fs), macos());
    }
}
