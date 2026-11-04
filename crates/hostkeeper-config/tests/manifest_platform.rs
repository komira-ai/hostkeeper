//! The `platform` of a manifest: the operating system and CPU family the package was built for
//! (design sections 4.7 and 5.5).

mod support;

use hostkeeper_config::manifest::{Class, Manifest, ManifestRefusal};
use hostkeeper_config::platform::Platform;
use support::package::{EXAMPLE, Package, RUNNING, expected, with};

/// The manifest names the platform the package was built for: each of the three spellings loads
/// for an order of that platform. A manifest without `platform`, or with any other spelling or a value that is
/// not text, is refused naming the key, class release.
#[test]
fn platform_is_required_and_one_of_the_three() {
    let package = Package::new("platform");
    let line = "platform       = \"linux-x86_64\"\n";
    for platform in Platform::ALL {
        package.manifest(&with(line, &format!("platform = \"{platform}\"\n")));
        let order = support::package::Expected {
            platform,
            ..expected("tierstore", "1.5.0")
        };
        let manifest = Manifest::load(&package.dir(), &order, &RUNNING).expect("loads");
        assert_eq!(manifest.platform, platform);
    }
    let missing = package.refusal(&with(line, ""));
    assert_eq!(
        missing,
        ManifestRefusal::Invalid {
            key: "platform".to_owned(),
            why: "is missing".to_owned(),
        }
    );
    assert_eq!(missing.class(), Class::Release);
    for (value, why) in [
        (
            "\"linux-amd64\"",
            "\"linux-amd64\" is not a platform: one of linux-x86_64, linux-arm64, macos-arm64",
        ),
        (
            "\"Linux-x86_64\"",
            "\"Linux-x86_64\" is not a platform: one of linux-x86_64, linux-arm64, macos-arm64",
        ),
        (
            "\"\"",
            "\"\" is not a platform: one of linux-x86_64, linux-arm64, macos-arm64",
        ),
        ("1", "is an integer, not a string"),
        ("[\"linux-x86_64\"]", "is an array, not a string"),
    ] {
        let refusal = package.refusal(&with(line, &format!("platform = {value}\n")));
        assert_eq!(
            refusal,
            ManifestRefusal::Invalid {
                key: "platform".to_owned(),
                why: why.to_owned(),
            },
            "{value}"
        );
        assert_eq!(refusal.reason(), "manifest_mismatch", "{value}");
    }
}

/// A package built for another platform than the order's is refused naming both, class release,
/// `manifest_mismatch`, as a package of another service or version is: for each pair of distinct
/// platforms, from the manifest's side and from the order's.
#[test]
fn a_manifest_for_another_platform_is_manifest_mismatch() {
    let package = Package::new("platform-mismatch");
    let line = "platform       = \"linux-x86_64\"\n";
    for built in Platform::ALL {
        package.manifest(&with(line, &format!("platform = \"{built}\"\n")));
        for ordered in Platform::ALL.into_iter().filter(|p| *p != built) {
            let order = support::package::Expected {
                platform: ordered,
                ..expected("tierstore", "1.5.0")
            };
            let refusal = Manifest::load(&package.dir(), &order, &RUNNING).unwrap_err();
            assert_eq!(
                refusal,
                ManifestRefusal::PlatformMismatch {
                    expected: ordered,
                    found: built,
                }
            );
            assert_eq!(refusal.class(), Class::Release);
            assert_eq!(refusal.reason(), "manifest_mismatch");
        }
    }
    package.manifest(EXAMPLE);
    let order = support::package::Expected {
        platform: Platform::MacosArm64,
        ..expected("tierstore", "1.5.0")
    };
    assert_eq!(
        Manifest::load(&package.dir(), &order, &RUNNING)
            .unwrap_err()
            .to_string(),
        "the manifest is for the platform linux-x86_64, not macos-arm64"
    );
}
