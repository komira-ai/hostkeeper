use std::path::Path;

use super::*;
use crate::mirrored::tests::{Answer, FakeWeb, Pauses, names, scratch};

const ISO: &[u8] = b"an ISO's bytes";
const OTHER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The boot set of `platform` (`arch` its deb architecture): its ISO at `iso_url`, of `bytes`
/// bytes and digest `sha`, every other file a placeholder.
fn set(platform: &str, arch: &str, iso_url: &str, bytes: u64, sha: &str) -> String {
    let file = |key: &str, extra: &str| {
        format!("[{platform}.{key}]\n{extra}bytes = 1\nsha256 = \"{OTHER}\"\n")
    };
    format!(
        "[{platform}.iso]\nurl = \"{iso_url}\"\nsums = \"https://cd.example/r/SHA256SUMS\"\n\
         bytes = {bytes}\nsha256 = \"{sha}\"\n{}{}{}{}",
        file(
            "shim",
            &format!("iso_path = \"pool/s/shim_1_{arch}.deb\"\ndeb_path = \"usr/s\"\n")
        ),
        file(
            "grub",
            &format!("iso_path = \"pool/g/grub_1_{arch}.deb\"\ndeb_path = \"usr/g\"\n")
        ),
        file("kernel", "iso_path = \"casper/vmlinuz\"\n"),
        file("initrd", "iso_path = \"casper/initrd\"\n"),
    )
}

const ARM_URL: &str = "https://cd.example/ubuntu/releases/24.04/release/u-arm64.iso";

/// Pins whose arm64 ISO is `ISO` at [`ARM_URL`].
fn pins() -> String {
    format!(
        "ubuntu = \"24.04.5\"\n{}{}",
        set(
            "linux-arm64",
            "arm64",
            ARM_URL,
            ISO.len() as u64,
            &Sha256::of(ISO).to_string()
        ),
        set(
            "linux-x86_64",
            "amd64",
            "https://rel.example/24.04/u-amd64.iso",
            9,
            OTHER
        ),
    )
}

/// Runs the verb for linux-arm64 with `pins` (written to `dir/pins.toml`) and `mirrors`, into
/// `dir/iso`.
fn run_in(
    dir: &Path,
    pins: &str,
    mirrors: &[&str],
    web: &FakeWeb,
    pauses: &Pauses,
) -> Result<String, Error> {
    let file = dir.join("pins.toml");
    std::fs::write(&file, pins).expect("pins");
    let mirrors: Vec<String> = mirrors.iter().map(|m| (*m).to_owned()).collect();
    run(
        &file,
        "linux-arm64",
        &mirrors,
        &dir.join("iso"),
        web,
        &pauses.pause(),
    )
}

const MIRROR: &str = "https://mirror.example/ubuntu-cdimage";
const MIRROR_URL: &str =
    "https://mirror.example/ubuntu-cdimage/ubuntu/releases/24.04/release/u-arm64.iso";

#[test]
fn the_pinned_url_is_tried_first_then_each_mirror_with_the_pinned_size_as_limit() {
    // Catches: a mirror tried before the pin's own URL, a mirror URL that does not keep the
    // pin's path below the mirror's base, no size limit, and an ISO kept under another name.
    let dir = scratch("iso-mirror");
    let web = FakeWeb::with(&[
        (ARM_URL, Answer::Fail("dns error".to_owned())),
        (MIRROR_URL, Answer::Body(ISO.to_vec())),
    ]);
    let pauses = Pauses::default();
    let out = run_in(&dir, &pins(), &[MIRROR], &web, &pauses).expect("fetched");
    assert_eq!(
        out,
        format!(
            "ok: {MIRROR_URL} has SHA-256 {}, after 1 failed: {ARM_URL}: dns error",
            Sha256::of(ISO)
        )
    );
    assert_eq!(
        *web.gets.borrow(),
        [
            (ARM_URL.to_owned(), Some(ISO.len() as u64)),
            (MIRROR_URL.to_owned(), Some(ISO.len() as u64))
        ]
    );
    assert_eq!(names(&dir.join("iso")), ["u-arm64.iso"]);
    assert_eq!(
        std::fs::read(dir.join("iso/u-arm64.iso")).expect("the ISO"),
        ISO
    );
}

#[test]
fn a_file_with_the_pinned_size_and_digest_is_kept_and_another_is_fetched_again() {
    // Catches: an ISO a build cache restored downloaded again, and a partial or stale file of
    // the ISO's name kept or refused instead of replaced.
    let dir = scratch("iso-kept");
    std::fs::create_dir(dir.join("iso")).expect("iso");
    std::fs::write(dir.join("iso/u-arm64.iso"), ISO).expect("kept");
    let web = FakeWeb::default();
    let out = run_in(&dir, &pins(), &[], &web, &Pauses::default()).expect("kept");
    assert_eq!(
        out,
        format!(
            "ok: the kept u-arm64.iso has {} bytes and SHA-256 {}",
            ISO.len(),
            Sha256::of(ISO)
        )
    );
    assert_eq!(web.urls(), Vec::<String>::new());

    for stale in [&ISO[..3], b"another ISO's b".as_slice()] {
        std::fs::write(dir.join("iso/u-arm64.iso"), stale).expect("stale");
        let web = FakeWeb::with(&[(ARM_URL, Answer::Body(ISO.to_vec()))]);
        run_in(&dir, &pins(), &[], &web, &Pauses::default()).expect("fetched");
        assert_eq!(web.urls(), [ARM_URL]);
        assert_eq!(
            std::fs::read(dir.join("iso/u-arm64.iso")).expect("the ISO"),
            ISO
        );
    }
}

#[test]
fn no_url_giving_the_iso_fails_naming_each_attempt() {
    // Catches: an ISO of another digest accepted, or a failure that does not name what was
    // tried.
    let dir = scratch("iso-none");
    // Of the pinned size, so that it is its digest that is refused.
    let web = FakeWeb::with(&[(ARM_URL, Answer::Body(b"an ISO's bytez".to_vec()))]);
    let err = run_in(&dir, &pins(), &[MIRROR], &web, &Pauses::default()).unwrap_err();
    let wrong = format!(
        "{ARM_URL}: SHA-256 {}, not the pinned one",
        Sha256::of(b"an ISO's bytez")
    );
    let dns = format!("{MIRROR_URL}: dns error: failed to lookup address information");
    let attempts: Vec<String> = (0..3).flat_map(|_| [wrong.clone(), dns.clone()]).collect();
    assert_eq!(
        err.to_string(),
        format!(
            "u-arm64.iso: no download was the pinned file: {}",
            attempts.join("; ")
        )
    );
    assert_eq!(names(&dir.join("iso")), Vec::<String>::new());
}

#[test]
fn bad_pins_an_unknown_platform_or_a_bad_mirror_are_refused_before_any_download() {
    // Catches: pins that cannot be read or are not boot-set pins, a platform with no ISO, and a
    // mirror that is not a plain HTTPS base (http, a trailing '/', a query, a user) each
    // reaching the network.
    let dir = scratch("iso-refused");
    let web = FakeWeb::default();
    let missing = dir.join("missing.toml");
    let err = run(
        &missing,
        "linux-arm64",
        &[],
        &dir,
        &web,
        &Pauses::default().pause(),
    );
    let err = err.unwrap_err();
    assert!(
        matches!(&err, Error::Pins { path, .. } if *path == missing),
        "{err}"
    );
    assert!(
        err.to_string()
            .starts_with(&format!("the pins {missing:?}: "))
    );

    let err = run_in(&dir, "ubuntu = \"24.04\"\n", &[], &web, &Pauses::default()).unwrap_err();
    assert!(
        err.to_string()
            .contains("missing the boot set of linux-arm64"),
        "{err}"
    );

    let file = dir.join("pins.toml");
    std::fs::write(&file, pins()).expect("pins");
    let err = run(
        &file,
        "linux-riscv",
        &[],
        &dir,
        &web,
        &Pauses::default().pause(),
    );
    assert_eq!(err.unwrap_err(), Error::Platform("linux-riscv".to_owned()));
    assert_eq!(
        Error::Platform("linux-riscv".to_owned()).to_string(),
        "the pins have no ISO of the platform \"linux-riscv\""
    );

    for bad in [
        "http://mirror.example/u",
        "https://mirror.example/u/",
        "https://mirror.example/u?x",
        "https://me@mirror.example/u",
        "https://",
    ] {
        let err = run_in(&dir, &pins(), &[MIRROR, bad], &web, &Pauses::default()).unwrap_err();
        assert_eq!(err, Error::Mirror(bad.to_owned()));
        assert_eq!(
            err.to_string(),
            format!(
                "the mirror {bad:?} is not an https:// URL with no trailing '/', query, \
                 fragment or user"
            )
        );
    }
    assert_eq!(web.urls(), Vec::<String>::new());
}

#[test]
fn a_directory_that_cannot_be_made_or_read_names_its_path() {
    // Catches: a directory in the way of the ISO, or a file in the way of the directory, not
    // named.
    let dir = scratch("iso-fs");
    let web = FakeWeb::default();
    std::fs::write(dir.join("iso"), b"a file").expect("a file in the way");
    let err = run_in(&dir, &pins(), &[], &web, &Pauses::default()).unwrap_err();
    assert!(
        matches!(&err, Error::Dir { path, .. } if *path == dir.join("iso")),
        "{err}"
    );

    let dir = scratch("iso-fs-dir");
    std::fs::create_dir_all(dir.join("iso/u-arm64.iso")).expect("a directory in the way");
    let err = run_in(&dir, &pins(), &[], &web, &Pauses::default()).unwrap_err();
    let path = dir.join("iso/u-arm64.iso");
    assert!(
        matches!(&err, Error::Dir { path: p, .. } if *p == path),
        "{err}"
    );
    assert!(err.to_string().starts_with(&format!("{path:?}: ")));
    assert_eq!(web.urls(), Vec::<String>::new());
}

#[test]
fn the_committed_pins_give_each_platform_its_iso_and_a_url_with_a_path() {
    // Catches: an accessor that reads another file of the boot set than its ISO, or an ISO
    // whose URL has no path a mirror could stand under.
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/netboot/pins.toml"),
    )
    .expect("pins.toml");
    let netboot = Netboot::parse(&text, "pins.toml").expect("pins");
    let (url, pin) = netboot.iso("linux-x86_64").expect("x86-64 ISO");
    assert_eq!(
        url,
        "https://releases.ubuntu.com/24.04/ubuntu-24.04.5-live-server-amd64.iso"
    );
    assert_eq!(pin.name, "ubuntu-24.04.5-live-server-amd64.iso");
    assert_eq!(pin.bytes, Some(4_080_486_400));
    assert_eq!(
        pin.sha256.to_string(),
        "97f3d7ffb032c3eb3b23d2c8be9cc76e60c2c1f2c0146ba5ba9fe01cafae0fd8"
    );
    let (url, _) = netboot.iso("linux-arm64").expect("arm64 ISO");
    assert!(
        url.starts_with("https://cdimage.ubuntu.com/ubuntu/releases/"),
        "{url}"
    );
    assert_eq!(netboot.iso("macos-arm64"), None);
}
