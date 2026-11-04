//! The `platform` of a manifest: the operating system and CPU family the package was built for
//! (design sections 4.7 and 5.5).

mod support;

use hostkeeper_config::manifest::{Class, ManifestRefusal};
use hostkeeper_config::platform::Platform;
use support::package::{Package, with};

/// The manifest names the platform the package was built for: each of the three spellings loads
/// as its platform. A manifest without `platform`, or with any other spelling or a value that is
/// not text, is refused naming the key, class release.
#[test]
fn platform_is_required_and_one_of_the_three() {
    let package = Package::new("platform");
    let line = "platform       = \"linux-x86_64\"\n";
    for platform in Platform::ALL {
        let text = with(line, &format!("platform = \"{platform}\"\n"));
        let manifest = package.load_text(&text).expect("each platform loads");
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
