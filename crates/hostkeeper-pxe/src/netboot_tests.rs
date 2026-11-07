use serde_json::{Value, json};

use super::*;
use crate::testing::release::Release;

const X86: BootPlatform = BootPlatform::LinuxX86_64;

fn both() -> BTreeSet<BootPlatform> {
    BootPlatform::ALL.into_iter().collect()
}

fn parse(value: &Value, platforms: &BTreeSet<BootPlatform>) -> Result<Netboot, NetbootInvalid> {
    Netboot::parse(value.to_string().as_bytes(), platforms)
}

/// The refusal of `netboot.json` with `key` of the x86-64 set's `part` set to `value`.
fn refusal(part: &str, key: &str, value: Value) -> String {
    let mut netboot = Release::new().netboot;
    netboot["platforms"]["linux-x86_64"][part][key] = value;
    parse(&netboot, &[X86].into_iter().collect())
        .expect_err("refused")
        .to_string()
}

/// The asset `cargo xtask release netboot` writes reads back for both platforms: the version,
/// the Ubuntu release, and each file's source, served name, size and SHA-256 as written; a field
/// the reader does not know (`sums`, and one added later) is ignored.
///
/// Catches a reader that drops a pin or reads one from the wrong key, and one that refuses an
/// asset grown additively.
#[test]
fn the_released_form_reads_back_with_every_pin() {
    let release = Release::new();
    let mut netboot = release.netboot.clone();
    netboot["added_later"] = json!({"any": "thing"});
    netboot["platforms"]["linux-arm64"]["kernel"]["added_later"] = json!(1);
    let read = parse(&netboot, &both()).expect("read");
    assert_eq!(
        (read.version.as_str(), read.ubuntu.as_str()),
        ("1.2.3", "24.04.5")
    );
    assert_eq!(
        read.sets.keys().copied().collect::<Vec<_>>(),
        BootPlatform::ALL
    );
    let files = &release.files[&X86];
    let set = &read.sets[&X86];
    let pin = |name: &str, bytes: &[u8]| Pin {
        name: name.to_owned(),
        bytes: bytes.len() as u64,
        sha256: crate::testing::sha256_hex(bytes),
    };
    assert_eq!(
        set.iso,
        Iso {
            url: "https://isos.example/24.04/ubuntu-24.04.5-live-server-amd64.iso".to_owned(),
            pin: pin("ubuntu-24.04.5-live-server-amd64.iso", &files.iso),
        }
    );
    assert_eq!(
        set.kernel,
        InIso {
            iso_path: "casper/vmlinuz".to_owned(),
            pin: pin("vmlinuz-linux-x86_64", &files.kernel),
        }
    );
    assert_eq!(
        set.initrd,
        InIso {
            iso_path: "casper/initrd".to_owned(),
            pin: pin("initrd-linux-x86_64", &files.initrd),
        }
    );
    assert_eq!(
        set.shim,
        InDeb {
            iso_path: crate::testing::release::shim_deb_path(X86),
            deb_path: "usr/lib/shim/shimx64.efi.signed.latest".to_owned(),
            pin: pin("hostkeeper/shimx64.efi", &files.shim),
        }
    );
    assert_eq!(
        set.grub,
        InDeb {
            iso_path: crate::testing::release::grub_deb_path(X86),
            deb_path: "usr/lib/grub/x86_64-efi-signed/grubnetx64.efi.signed".to_owned(),
            pin: pin("hostkeeper/grubx64.efi", &files.grub),
        }
    );
}

/// The committed pins (`packaging/netboot/pins.toml`), in the form `cargo xtask release netboot`
/// renders them (each table as it is, and the served name the design gives each file), read for
/// both platforms, every pin as committed.
///
/// Catches a reader that refuses the asset the release workflow attests for the pins on `main`.
#[test]
fn the_committed_pins_in_the_released_form_read() {
    let pins: toml::Table = include_str!("../../../packaging/netboot/pins.toml")
        .parse()
        .expect("the committed pins");
    let mut platforms = serde_json::Map::new();
    for platform in BootPlatform::ALL {
        let (p, efi) = (platform.as_str(), platform.efi_arch());
        let mut set: Value = serde_json::to_value(&pins[p]).expect("a boot set");
        let url = set["iso"]["url"]
            .as_str()
            .expect("the ISO's URL")
            .to_owned();
        let iso = url.rsplit('/').next().expect("a file name").to_owned();
        for (part, name) in [
            ("iso", iso),
            ("shim", format!("hostkeeper/shim{efi}.efi")),
            ("grub", format!("hostkeeper/grub{efi}.efi")),
            ("kernel", format!("vmlinuz-{p}")),
            ("initrd", format!("initrd-{p}")),
        ] {
            set[part]["name"] = json!(name);
        }
        platforms.insert(p.to_owned(), set);
    }
    let asset =
        json!({"platforms": platforms, "ubuntu": pins["ubuntu"].as_str(), "version": "0.1.80"});
    let read = parse(&asset, &both()).expect("read");
    for platform in BootPlatform::ALL {
        let set = &read.sets[&platform];
        let pinned = &pins[platform.as_str()];
        assert_eq!(
            Some(set.iso.pin.bytes),
            pinned["iso"]["bytes"]
                .as_integer()
                .and_then(|n| u64::try_from(n).ok())
        );
        assert_eq!(
            Some(set.shim.deb_path.as_str()),
            pinned["shim"]["deb_path"].as_str()
        );
        assert_eq!(
            Some(set.kernel.pin.sha256.as_str()),
            pinned["kernel"]["sha256"].as_str()
        );
    }
}

/// Each value of the wrong form is refused naming the platform, the file and the key: a size of
/// 0, a digest that is not 64 lowercase hex digits, an ISO URL that is not `https://`, an ISO
/// name that is a path or not `.iso`, a served name other than the design's, and a source path
/// that is absolute, climbs out, or has an empty part.
///
/// Catches a pin that would make the fetch write outside its directory, or seal a boot file
/// under a name `serve` does not serve.
#[test]
fn each_value_of_the_wrong_form_is_refused_by_key() {
    let cases = [
        (
            refusal("kernel", "bytes", json!(0)),
            "linux-x86_64 kernel: bytes is 0",
        ),
        (
            refusal("shim", "sha256", json!("A".repeat(64))),
            "linux-x86_64 shim: sha256 \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\" \
             is not 64 lowercase hex digits",
        ),
        (
            refusal("iso", "url", json!("http://isos.example/a.iso")),
            "linux-x86_64 iso: url \"http://isos.example/a.iso\" is not an https:// URL",
        ),
        (
            refusal("iso", "url", json!("https://isos.example/a b.iso")),
            "linux-x86_64 iso: url \"https://isos.example/a b.iso\" is not an https:// URL",
        ),
        (
            refusal("iso", "name", json!("sub/a.iso")),
            "linux-x86_64 iso: name \"sub/a.iso\" is not one file name ending in .iso",
        ),
        (
            refusal("iso", "name", json!("a.img")),
            "linux-x86_64 iso: name \"a.img\" is not one file name ending in .iso",
        ),
        (
            refusal("iso", "name", json!("..")),
            "linux-x86_64 iso: name \"..\" is not one file name ending in .iso",
        ),
        (
            refusal("grub", "name", json!("hostkeeper/grubaa64.efi")),
            "linux-x86_64 grub: name \"hostkeeper/grubaa64.efi\" is not \"hostkeeper/grubx64.efi\"",
        ),
        (
            refusal("kernel", "name", json!("../vmlinuz")),
            "linux-x86_64 kernel: name \"../vmlinuz\" is not \"vmlinuz-linux-x86_64\"",
        ),
        (
            refusal("kernel", "iso_path", json!("/casper/vmlinuz")),
            "linux-x86_64 kernel iso_path \"/casper/vmlinuz\" is not a relative path without \
             empty, . or .. parts",
        ),
        (
            refusal("shim", "deb_path", json!("usr/../../etc/shadow")),
            "linux-x86_64 shim deb_path \"usr/../../etc/shadow\" is not a relative path without \
             empty, . or .. parts",
        ),
        (
            refusal("grub", "iso_path", json!("pool//g.deb")),
            "linux-x86_64 grub iso_path \"pool//g.deb\" is not a relative path without empty, . \
             or .. parts",
        ),
        (
            refusal("initrd", "iso_path", json!("")),
            "linux-x86_64 initrd iso_path \"\" is not a relative path without empty, . or .. parts",
        ),
    ];
    for (got, expected) in cases {
        assert_eq!(got, expected);
    }
}

/// A text that is not the asset (not JSON, a missing key, a value of another type), a version
/// that is not `X.Y.Z`, and a platform asked for with no boot set are refused, each naming what
/// is wrong.
///
/// Catches a fetch that takes a release's deb name from a version it never checked.
#[test]
fn an_asset_of_another_form_is_refused() {
    let x86: BTreeSet<BootPlatform> = [X86].into_iter().collect();
    let not_json = Netboot::parse(b"not json", &x86).expect_err("refused");
    assert!(
        not_json.0.starts_with("is not the boot-set asset: "),
        "{not_json}"
    );
    let mut netboot = Release::new().netboot;
    netboot["platforms"]["linux-x86_64"]
        .as_object_mut()
        .expect("the set")
        .remove("initrd");
    let missing = parse(&netboot, &x86).expect_err("refused");
    assert_eq!(missing.to_string(), "linux-x86_64: missing field `initrd`");
    let mut netboot = Release::new().netboot;
    netboot["platforms"]["linux-x86_64"]["kernel"]["bytes"] = json!("15");
    let typed = parse(&netboot, &x86).expect_err("refused");
    assert!(
        typed
            .0
            .starts_with("linux-x86_64: invalid type: string \"15\""),
        "{typed}"
    );
    for version in ["1.2", "1.2.x", "1..2", "1.2.3.", "v1.2.3"] {
        let mut netboot = Release::new().netboot;
        netboot["version"] = json!(version);
        assert_eq!(
            parse(&netboot, &x86).expect_err(version).to_string(),
            format!("version {version:?} is not X.Y.Z")
        );
    }
    let mut netboot = Release::new().netboot;
    netboot["platforms"]
        .as_object_mut()
        .expect("the sets")
        .remove("linux-x86_64");
    assert_eq!(
        parse(&netboot, &x86).expect_err("refused").to_string(),
        "has no boot set for linux-x86_64"
    );
}
