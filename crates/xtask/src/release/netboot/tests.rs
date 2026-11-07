use super::*;
use crate::release::fakes::scratch;

/// The committed pins.
const PINS: &str = include_str!("../../../../../packaging/netboot/pins.toml");

/// The name the problems of the pins in these tests start with.
const WHAT: &str = "pins.toml";

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn version() -> Version {
    "1.2.3".parse().expect("a version")
}

/// The boot set of a platform in the pins of these tests: `arch` its deb architecture, every
/// size its `n` and every digest `sha`.
fn set(platform: &str, arch: &str, n: u64, sha: &str) -> String {
    format!(
        "[{platform}.iso]\n\
         url = \"https://cd.example/r/u-{arch}.iso\"\n\
         sums = \"https://cd.example/r/SHA256SUMS\"\n\
         bytes = {n}\n\
         sha256 = \"{sha}\"\n\
         [{platform}.shim]\n\
         iso_path = \"pool/s/shim_1_{arch}.deb\"\n\
         deb_path = \"usr/lib/shim/shim.signed\"\n\
         bytes = {n}\n\
         sha256 = \"{sha}\"\n\
         [{platform}.grub]\n\
         iso_path = \"pool/g/grub_1_{arch}.deb\"\n\
         deb_path = \"usr/lib/grub/grubnet.signed\"\n\
         bytes = {n}\n\
         sha256 = \"{sha}\"\n\
         [{platform}.kernel]\n\
         iso_path = \"casper/vmlinuz\"\n\
         bytes = {n}\n\
         sha256 = \"{sha}\"\n\
         [{platform}.initrd]\n\
         iso_path = \"casper/initrd\"\n\
         bytes = {n}\n\
         sha256 = \"{sha}\"\n"
    )
}

/// Pins of both platforms: arm64's sizes 1 and digests `A`, x86-64's sizes 2 and digests `B`.
fn pins() -> String {
    format!(
        "ubuntu = \"24.04.5\"\n{}{}",
        set("linux-arm64", "arm64", 1, A),
        set("linux-x86_64", "amd64", 2, B)
    )
}

/// The pins of these tests with `from` replaced by `to` (once).
fn edited(from: &str, to: &str) -> String {
    let text = pins();
    assert!(text.contains(from), "{from}");
    text.replacen(from, to, 1)
}

/// `text` without the table that starts at the line `header`, up to the next table.
fn without_table(text: &str, header: &str) -> String {
    let (before, rest) = text.split_once(&format!("{header}\n")).expect("the table");
    let after = rest.find("\n[").map_or("", |at| &rest[at + 1..]);
    format!("{before}{after}")
}

/// The problems of `text` as pins, or the asset of version 1.2.3 they render.
fn render(text: &str) -> Result<String, Error> {
    Netboot::parse(text, WHAT).map(|n| n.to_json(&version()))
}

/// The refusal naming exactly `problems`, each after the pins' name.
fn refused(problems: &[&str]) -> Result<String, Error> {
    Err(Error::Refused(
        problems.iter().map(|p| format!("{WHAT}: {p}")).collect(),
    ))
}

/// The asset of the pins of these tests, a file `{...}` of one platform: `d` its deb
/// architecture, `e` its UEFI one, `n` its sizes and `sha` its digests.
fn set_json(platform: &str, d: &str, e: &str, n: u64, sha: &str) -> String {
    format!(
        "\"{platform}\":{{\
         \"grub\":{{\"bytes\":{n},\"deb_path\":\"usr/lib/grub/grubnet.signed\",\"iso_path\":\"pool/g/grub_1_{d}.deb\",\"name\":\"grub{e}.efi\",\"sha256\":\"{sha}\"}},\
         \"initrd\":{{\"bytes\":{n},\"iso_path\":\"casper/initrd\",\"name\":\"initrd-{platform}\",\"sha256\":\"{sha}\"}},\
         \"iso\":{{\"bytes\":{n},\"name\":\"u-{d}.iso\",\"sha256\":\"{sha}\",\"sums\":\"https://cd.example/r/SHA256SUMS\",\"url\":\"https://cd.example/r/u-{d}.iso\"}},\
         \"kernel\":{{\"bytes\":{n},\"iso_path\":\"casper/vmlinuz\",\"name\":\"vmlinuz-{platform}\",\"sha256\":\"{sha}\"}},\
         \"shim\":{{\"bytes\":{n},\"deb_path\":\"usr/lib/shim/shim.signed\",\"iso_path\":\"pool/s/shim_1_{d}.deb\",\"name\":\"shim{e}.efi\",\"sha256\":\"{sha}\"}}\
         }}"
    )
}

/// The canonical asset of `pins()` for version 1.2.3.
fn asset() -> String {
    format!(
        "{{\"platforms\":{{{},{}}},\"ubuntu\":\"24.04.5\",\"version\":\"1.2.3\"}}",
        set_json("linux-arm64", "arm64", "aa64", 1, A),
        set_json("linux-x86_64", "amd64", "x64", 2, B)
    )
}

#[test]
fn the_asset_is_written_canonically_with_each_file_named_as_it_is_served() {
    // Catches: a field left out of the asset or renamed, keys out of order or white space (the
    // asset is attested and fetched by its digest, so its bytes are the contract), and a served
    // name other than the design's: the arm64 shim or GRUB named for x86-64, or the kernel and
    // initrd of the two platforms under one name.
    assert_eq!(render(&pins()), Ok(asset()));
}

#[test]
fn the_committed_pins_render_the_boot_sets_of_both_linux_platforms() {
    // Catches: committed pins the verb refuses (the artifacts workflow would fail on main), a
    // platform without its boot set, and pins whose ISO, shim or GRUB is of the other
    // architecture.
    let text = render(PINS).expect("the committed pins render");
    let asset: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(asset["ubuntu"], "24.04.5");
    assert_eq!(asset["version"], "1.2.3");
    let platforms = asset["platforms"].as_object().expect("an object");
    assert_eq!(
        platforms.keys().collect::<Vec<_>>(),
        ["linux-arm64", "linux-x86_64"]
    );
    for (platform, iso, efi) in [
        (
            "linux-arm64",
            "ubuntu-24.04.5-live-server-arm64.iso",
            "aa64",
        ),
        (
            "linux-x86_64",
            "ubuntu-24.04.5-live-server-amd64.iso",
            "x64",
        ),
    ] {
        let set = &platforms[platform];
        let names: Vec<&str> = ["iso", "shim", "grub", "kernel", "initrd"]
            .iter()
            .map(|role| set[role]["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(
            names,
            [
                iso.to_owned(),
                format!("shim{efi}.efi"),
                format!("grub{efi}.efi"),
                format!("vmlinuz-{platform}"),
                format!("initrd-{platform}"),
            ],
            "{platform}"
        );
    }
}

#[test]
fn each_platform_is_named_for_the_architecture_its_debs_carry() {
    // Catches: the table of boot platforms naming a deb architecture other than the one the
    // release's own debs are built for (`hostkeeper_<v>_<deb arch>.deb`).
    for arch in BOOT_PLATFORMS {
        assert_eq!(
            arch.platform.linux_arches().map(|(deb, _)| deb),
            Some(arch.deb),
            "{}",
            arch.platform.name()
        );
    }
}

#[test]
fn pins_missing_a_platform_a_file_a_key_or_the_release_are_refused_naming_each() {
    // Catches: a release without the boot set of one platform, or a boot set without one of its
    // five files, written all the same; and a refusal that names only the first problem.
    let no_arm = format!(
        "ubuntu = \"24.04.5\"\n{}",
        set("linux-x86_64", "amd64", 2, B)
    );
    assert_eq!(
        render(&no_arm),
        refused(&["missing the boot set of linux-arm64"])
    );
    let no_x86 = format!(
        "ubuntu = \"24.04.5\"\n{}",
        set("linux-arm64", "arm64", 1, A)
    );
    assert_eq!(
        render(&no_x86),
        refused(&["missing the boot set of linux-x86_64"])
    );
    assert_eq!(
        render(&pins().replacen("ubuntu = \"24.04.5\"\n", "", 1)),
        refused(&["missing ubuntu, the Ubuntu release of the ISOs"])
    );
    for role in ["iso", "shim", "grub", "kernel", "initrd"] {
        let text = pins().replacen(
            &format!("[linux-arm64.{role}]"),
            &format!("[linux-arm64.moved-{role}]"),
            1,
        );
        assert_eq!(
            render(&text),
            refused(&[
                &format!(
                    "linux-arm64.moved-{role} is not a file of a boot set (expected iso, shim, \
                     grub, kernel, initrd)"
                ),
                &format!("linux-arm64: missing {role}"),
            ]),
            "{role}"
        );
    }
    // The first line of each key in `pins()` is the arm64 ISO's.
    for (key, line) in [
        ("url", "url = \"https://cd.example/r/u-arm64.iso\"\n"),
        ("sums", "sums = \"https://cd.example/r/SHA256SUMS\"\n"),
        ("bytes", "bytes = 1\n"),
        ("sha256", &format!("sha256 = \"{A}\"\n")),
    ] {
        assert_eq!(
            render(&edited(line, "")),
            refused(&[&format!("linux-arm64.iso: missing {key}")]),
            "{key}"
        );
    }
    let text = edited("deb_path = \"usr/lib/shim/shim.signed\"\n", "");
    assert_eq!(
        render(&text),
        refused(&["linux-arm64.shim: missing deb_path"])
    );
    let text = edited("iso_path = \"casper/initrd\"\n", "");
    assert_eq!(
        render(&text),
        refused(&["linux-arm64.initrd: missing iso_path"])
    );
}

#[test]
fn pins_with_an_unknown_key_or_a_value_of_another_type_are_refused() {
    // Catches: a key the reader does not know silently dropped (a typo of a pin, or a macOS boot
    // set nothing serves), and a table or value of the wrong type read as something else.
    assert_eq!(
        render(&format!("{}[macos-arm64.iso]\nbytes = 1\n", pins())),
        refused(&[
            "macos-arm64 is not a key of the pins (expected ubuntu, linux-arm64, linux-x86_64)"
        ])
    );
    assert_eq!(
        render(&edited(
            "[linux-arm64.kernel]\n",
            "[linux-arm64.kernel]\ndeb_path = \"boot/vmlinuz\"\n"
        )),
        refused(&[
            "linux-arm64.kernel.deb_path is not a key of a kernel (expected iso_path, bytes, sha256)"
        ])
    );
    assert_eq!(
        render(&format!(
            "ubuntu = \"24.04.5\"\nlinux-arm64 = 1\n{}",
            set("linux-x86_64", "amd64", 2, B)
        )),
        refused(&["linux-arm64 is not a table"])
    );
    let iso_a_string = format!(
        "{}[linux-arm64]\niso = \"u-arm64.iso\"\n",
        without_table(&pins(), "[linux-arm64.iso]")
    );
    assert_eq!(
        render(&iso_a_string),
        refused(&["linux-arm64.iso is not a table"])
    );
    assert_eq!(
        render(&edited("sha256 = \"aaaa", "sha256 = 1 #\"aaaa")),
        refused(&["linux-arm64.iso.sha256: 1 is not a string"])
    );
    assert_eq!(
        render(&edited("ubuntu = \"24.04.5\"", "ubuntu = 24")),
        refused(&["ubuntu: 24 is not a string"])
    );
}

#[test]
fn pins_whose_values_are_not_of_their_form_are_refused_naming_each() {
    // Catches: a size, digest, URL, path or release of the wrong form pinned and served; an ISO
    // or a deb of one architecture pinned for the other platform; and a path that climbs out of
    // the ISO or the deb, or starts at its root.
    let cases: [(&str, &str, &str); 25] = [
        (
            "bytes = 1\n",
            "bytes = 0\n",
            "linux-arm64.iso.bytes: 0 is not a size (a whole number of bytes above 0)",
        ),
        (
            "bytes = 1\n",
            "bytes = -1\n",
            "linux-arm64.iso.bytes: -1 is not a size (a whole number of bytes above 0)",
        ),
        (
            "bytes = 1\n",
            "bytes = 1.5\n",
            "linux-arm64.iso.bytes: 1.5 is not a size (a whole number of bytes above 0)",
        ),
        (
            "bytes = 1\n",
            "bytes = \"1\"\n",
            "linux-arm64.iso.bytes: \"1\" is not a size (a whole number of bytes above 0)",
        ),
        (
            A,
            &A.to_uppercase(),
            "linux-arm64.iso.sha256: \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\" is not a SHA-256 (64 lowercase hex digits)",
        ),
        (
            A,
            &A[1..],
            "linux-arm64.iso.sha256: \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" is not a SHA-256 (64 lowercase hex digits)",
        ),
        (
            "https://cd.example/r/u-arm64.iso",
            "http://cd.example/r/u-arm64.iso",
            "linux-arm64.iso.url: \"http://cd.example/r/u-arm64.iso\" is not an https:// URL of a live-server ISO of arm64 (a file named ...-arm64.iso)",
        ),
        (
            "https://cd.example/r/u-arm64.iso",
            "https://cd.example/r/u-amd64.iso",
            "linux-arm64.iso.url: \"https://cd.example/r/u-amd64.iso\" is not an https:// URL of a live-server ISO of arm64 (a file named ...-arm64.iso)",
        ),
        (
            "https://cd.example/r/u-arm64.iso",
            "https://cd.example/r/u arm64.iso",
            "linux-arm64.iso.url: \"https://cd.example/r/u arm64.iso\" is not an https:// URL of a live-server ISO of arm64 (a file named ...-arm64.iso)",
        ),
        (
            "https://cd.example/r/u-arm64.iso",
            "https://u-arm64.iso",
            "linux-arm64.iso.url: \"https://u-arm64.iso\" is not an https:// URL of a live-server ISO of arm64 (a file named ...-arm64.iso)",
        ),
        (
            "https://cd.example/r/u-arm64.iso",
            "https://cd.example/r/u-arm64.img",
            "linux-arm64.iso.url: \"https://cd.example/r/u-arm64.img\" is not an https:// URL of a live-server ISO of arm64 (a file named ...-arm64.iso)",
        ),
        (
            "https://cd.example/r/SHA256SUMS",
            "http://cd.example/r/SHA256SUMS",
            "linux-arm64.iso.sums: \"http://cd.example/r/SHA256SUMS\" is not an https:// URL of a file named SHA256SUMS",
        ),
        (
            "https://cd.example/r/SHA256SUMS",
            "https://cd.example/r/SHA256SUMS.gpg",
            "linux-arm64.iso.sums: \"https://cd.example/r/SHA256SUMS.gpg\" is not an https:// URL of a file named SHA256SUMS",
        ),
        (
            "https://cd.example/r/SHA256SUMS",
            "https://cd.example/../SHA256SUMS",
            "linux-arm64.iso.sums: \"https://cd.example/../SHA256SUMS\" is not an https:// URL of a file named SHA256SUMS",
        ),
        (
            "\"casper/vmlinuz\"",
            "\"/casper/vmlinuz\"",
            "linux-arm64.kernel.iso_path: \"/casper/vmlinuz\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "\"casper/vmlinuz\"",
            "\"casper/../vmlinuz\"",
            "linux-arm64.kernel.iso_path: \"casper/../vmlinuz\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "\"casper/vmlinuz\"",
            "\"./casper/vmlinuz\"",
            "linux-arm64.kernel.iso_path: \"./casper/vmlinuz\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "\"casper/vmlinuz\"",
            "\"casper//vmlinuz\"",
            "linux-arm64.kernel.iso_path: \"casper//vmlinuz\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "\"casper/vmlinuz\"",
            "\"casper/\"",
            "linux-arm64.kernel.iso_path: \"casper/\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "\"casper/vmlinuz\"",
            "\"casper\\\\vmlinuz\"",
            "linux-arm64.kernel.iso_path: \"casper\\\\vmlinuz\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "\"casper/vmlinuz\"",
            "\"\"",
            "linux-arm64.kernel.iso_path: \"\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "pool/s/shim_1_arm64.deb",
            "pool/s/shim_1_amd64.deb",
            "linux-arm64.shim.iso_path: \"pool/s/shim_1_amd64.deb\" is not a deb of arm64 (a name ending _arm64.deb)",
        ),
        (
            "pool/g/grub_1_arm64.deb",
            "/pool/g/grub_1_arm64.deb",
            "linux-arm64.grub.iso_path: \"/pool/g/grub_1_arm64.deb\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "\"usr/lib/shim/shim.signed\"",
            "\"/usr/lib/shim/shim.signed\"",
            "linux-arm64.shim.deb_path: \"/usr/lib/shim/shim.signed\" is not a relative path (parts of ASCII letters, digits and . _ + ~ -, none empty, . or ..)",
        ),
        (
            "ubuntu = \"24.04.5\"",
            "ubuntu = \"noble\"",
            "ubuntu: \"noble\" is not an Ubuntu release (X.Y or X.Y.Z)",
        ),
    ];
    for (from, to, problem) in cases {
        assert_eq!(render(&edited(from, to)), refused(&[problem]), "{to}");
    }
    for release in ["24", "24.04.5.1", "24..5", "24.04a", ".04"] {
        assert_eq!(
            render(&edited("\"24.04.5\"", &format!("{release:?}"))),
            refused(&[&format!(
                "ubuntu: {release:?} is not an Ubuntu release (X.Y or X.Y.Z)"
            )]),
            "{release}"
        );
    }
    // A release of two numbers is one, and so is a part with + or ~ in a path.
    assert!(render(&edited("\"24.04.5\"", "\"24.04\"")).is_ok());
    assert!(
        render(&edited(
            "pool/s/shim_1_arm64.deb",
            "pool/s/shim_1+2~3_arm64.deb"
        ))
        .is_ok()
    );
}

#[test]
fn every_problem_of_the_pins_is_named_at_once() {
    // Catches: a refusal that stops at the first problem, so that fixing pins takes a run per
    // mistake, and a problem of one platform hiding one of the other.
    let text = edited("bytes = 1\n", "bytes = 0\n").replacen(
        "[linux-x86_64.grub]\niso_path = \"pool/g/grub_1_amd64.deb\"",
        "[linux-x86_64.grub]\niso_path = \"pool/g/grub_1_arm64.deb\"",
        1,
    );
    assert_eq!(
        render(&format!("extra = 1\n{text}")),
        refused(&[
            "extra is not a key of the pins (expected ubuntu, linux-arm64, linux-x86_64)",
            "linux-arm64.iso.bytes: 0 is not a size (a whole number of bytes above 0)",
            "linux-x86_64.grub.iso_path: \"pool/g/grub_1_arm64.deb\" is not a deb of amd64 (a name ending _amd64.deb)",
        ])
    );
}

#[test]
fn a_text_that_is_not_toml_fails_naming_the_pins() {
    // Catches: a syntax error in the pins reported without the file it is in.
    let out = render("ubuntu = ");
    assert!(
        matches!(&out, Err(Error::Failed(why)) if why.starts_with("pins.toml: ")),
        "{out:?}"
    );
}

#[test]
fn the_verb_writes_netboot_json_into_the_directory_of_the_assets() {
    // Catches: the asset written under another name or elsewhere (the sums job would not list
    // it, and attest would find no file to sign), or not exactly the canonical text.
    let dir = scratch("netboot-verb");
    let pins_path = dir.join("pins.toml");
    std::fs::write(&pins_path, pins()).expect("write the pins");
    let path = dir.join("netboot.json");
    assert_eq!(
        write_netboot(&pins_path, &version(), &dir),
        Ok(vec![format!(
            "{}: the boot sets of Ubuntu 24.04.5 for linux-arm64, linux-x86_64",
            path.display()
        )])
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the asset"),
        asset()
    );
}

#[test]
fn the_verb_writes_nothing_for_refused_pins() {
    // Catches: a partial or stale `netboot.json` left for the sums job after a refusal.
    let dir = scratch("netboot-refused");
    let pins_path = dir.join("pins.toml");
    std::fs::write(&pins_path, edited("bytes = 1\n", "bytes = 0\n")).expect("write the pins");
    let out = write_netboot(&pins_path, &version(), &dir);
    assert_eq!(
        out,
        Err(Error::Refused(vec![format!(
            "{}: linux-arm64.iso.bytes: 0 is not a size (a whole number of bytes above 0)",
            pins_path.display()
        )]))
    );
    assert!(!dir.join("netboot.json").exists());
}

#[test]
fn pins_that_cannot_be_read_and_an_asset_that_cannot_be_written_are_io_errors() {
    // Catches: a missing pins file or a failed write reported as success.
    let dir = scratch("netboot-io");
    let absent = dir.join("absent.toml");
    assert!(matches!(
        write_netboot(&absent, &version(), &dir),
        Err(Error::Io { what, .. }) if what == format!("read {}", absent.display())
    ));
    let pins_path = dir.join("pins.toml");
    std::fs::write(&pins_path, pins()).expect("write the pins");
    std::fs::create_dir(dir.join("netboot.json")).expect("a directory in the way");
    assert!(matches!(
        write_netboot(&pins_path, &version(), &dir),
        Err(Error::Io { what, .. }) if what == format!("write {}", dir.join("netboot.json").display())
    ));
}
