//! The closed set of platforms: the three spellings, nothing else, and the platform of the
//! target the tests were compiled for.

use hostkeeper_config::platform::{Platform, UnknownPlatform};

/// Each platform reads from and writes as exactly its spelling.
#[test]
fn each_platform_reads_and_writes_its_spelling() {
    for (platform, spelling) in [
        (Platform::LinuxX86_64, "linux-x86_64"),
        (Platform::LinuxArm64, "linux-arm64"),
        (Platform::MacosArm64, "macos-arm64"),
    ] {
        assert_eq!(platform.as_str(), spelling);
        assert_eq!(platform.to_string(), spelling);
        assert_eq!(spelling.parse::<Platform>(), Ok(platform));
    }
}

/// `ALL` holds the three platforms once each, and the order of platforms is the order of their
/// spellings, so a list sorted by platform is sorted by spelling.
#[test]
fn the_order_of_platforms_is_the_order_of_their_spellings() {
    let spellings: Vec<&str> = Platform::ALL.iter().map(|p| p.as_str()).collect();
    assert_eq!(spellings, ["linux-arm64", "linux-x86_64", "macos-arm64"]);
    let mut sorted = Platform::ALL;
    sorted.sort();
    assert_eq!(sorted, Platform::ALL);
    assert!(Platform::LinuxArm64 < Platform::LinuxX86_64);
    assert!(Platform::LinuxX86_64 < Platform::MacosArm64);
}

/// Any other spelling is refused, naming it: other case, other names for the same CPU, other
/// systems, surrounding white space, the empty text.
#[test]
fn any_other_spelling_is_refused() {
    for bad in [
        "",
        "Linux-x86_64",
        "LINUX-ARM64",
        "linux-amd64",
        "linux-aarch64",
        "linux_x86_64",
        "darwin-arm64",
        "macos-x86_64",
        "windows-x86_64",
        " linux-arm64",
        "linux-arm64\n",
        "linux",
    ] {
        assert_eq!(
            bad.parse::<Platform>(),
            Err(UnknownPlatform(bad.to_owned())),
            "{bad:?}"
        );
    }
    assert_eq!(
        UnknownPlatform("linux-amd64".to_owned()).to_string(),
        r#""linux-amd64" is not a platform: one of linux-x86_64, linux-arm64, macos-arm64"#
    );
}

/// `current` is the platform of the target the code was compiled for, read here from the
/// standard library's names of the target's system and CPU.
#[test]
fn current_is_the_compile_target() {
    let cpu = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    let expected = format!("{}-{cpu}", std::env::consts::OS);
    assert_eq!(Platform::current().as_str(), expected);
}
