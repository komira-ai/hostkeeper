//! The platform values: the documented paths of each host kind.

use std::path::PathBuf;

use hostkeeper_launchd::{Platform, PlatformKind, REVERSE_DNS_PREFIX};

/// Catches a constructor that names the wrong unit directory, suffix, run directory or label prefix, and
/// a `host` that picks the other platform than the one the binary was built for.
#[test]
fn the_platform_constructors_name_the_documented_paths() {
    let linux = Platform {
        kind: PlatformKind::Linux,
        unit_dir: PathBuf::from("/etc/systemd/system"),
        unit_suffix: ".service",
        run_dir: PathBuf::from("/run"),
        label_prefix: None,
    };
    let macos = Platform {
        kind: PlatformKind::Macos,
        unit_dir: PathBuf::from("/Library/LaunchDaemons"),
        unit_suffix: ".plist",
        run_dir: PathBuf::from("/var/run"),
        label_prefix: Some(REVERSE_DNS_PREFIX),
    };
    assert_eq!(Platform::linux(), linux);
    assert_eq!(Platform::macos(), macos);
    assert_eq!(Platform::for_os(false), linux);
    assert_eq!(Platform::for_os(true), macos);
    let expected = if cfg!(target_os = "macos") {
        macos
    } else {
        linux
    };
    assert_eq!(Platform::host(), expected);
}
