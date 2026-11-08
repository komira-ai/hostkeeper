//! The pin, the check of the downloaded tarball against it, and the version a program runs as,
//! without root. Files go to a scratch directory beside the test binary, inside the target
//! directory.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use hostkeeper_config::platform::Platform;
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

use super::{
    Asset, DOWNLOAD_FILE, MEMBER, PIN_TEXT, Pin, PinError, asset_url, checked_binary,
    download_path, program_version,
};

/// A scratch directory beside the test binary.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let base = exe
        .parent()
        .expect("its directory")
        .join("privileged-across");
    Scratch::under(&base, name).expect("create the scratch directory")
}

const SHA_A: &str = "aaf45bb52940d5d032ee469dd3ce9e6c1c728853dff09343154973d20713aed1";
const SHA_B: &str = "423ec6f09f04030219014289b864403513d37298b13ec0548d39a600caad1948";
const SHA_C: &str = "128ca92f16e157ba4eb7e3776f05ea54a94e7a79d1dbe81e647150e12ba93457";

/// A pin of release `0.2.3` that [`Pin::parse`] takes.
fn good_pin() -> String {
    let url = |platform: &str| asset_url("v0.2.3", "0.2.3", platform);
    format!(
        "tag = \"v0.2.3\"\nversion = \"0.2.3\"\n\n\
         [linux-arm64]\nurl = \"{}\"\nsha256 = \"{SHA_A}\"\n\n\
         [linux-x86_64]\nurl = \"{}\"\nsha256 = \"{SHA_B}\"\n\n\
         [macos-arm64]\nurl = \"{}\"\nsha256 = \"{SHA_C}\"\n",
        url("linux-arm64"),
        url("linux-x86_64"),
        url("macos-arm64"),
    )
}

/// [`good_pin`] with `from` replaced by `to`, once.
fn pin_with(from: &str, to: &str) -> String {
    let text = good_pin();
    assert_eq!(text.matches(from).count(), 1, "{from:?} once in the pin");
    text.replacen(from, to, 1)
}

/// What [`Pin::parse`] says of `text`, as its message.
fn refusal(text: &str) -> String {
    match Pin::parse(text) {
        Ok(pin) => panic!("taken: {pin:?}"),
        Err(e) => e.to_string(),
    }
}

/// Catches a checked-in pin the rig cannot use: one that does not parse, lacks a release
/// platform, or names a URL other than the release's own binary tarball (every check of
/// [`Pin::parse`] passes on the committed file, and each platform's URL is the tag's asset).
#[test]
fn the_committed_pin_names_the_binary_tarball_of_one_release_for_every_platform() {
    let pin = Pin::parse(PIN_TEXT).expect("the committed pin parses");
    assert_eq!(pin.tag, format!("v{}", pin.version));
    let platforms: Vec<&str> = pin.assets.keys().copied().collect();
    assert_eq!(platforms, ["linux-arm64", "linux-x86_64", "macos-arm64"]);
    for platform in Platform::ALL {
        let asset = pin.asset(platform);
        assert_eq!(
            asset.url,
            format!(
                "https://github.com/komira-ai/hostkeeper/releases/download/{}/hostkeeper-{}-{}.tar.gz",
                pin.tag,
                pin.version,
                platform.as_str()
            )
        );
        assert_eq!(asset.sha256.len(), 64, "{asset:?}");
    }
}

/// Catches a pin read field by field into the wrong place: the tag, version and each platform's
/// URL and digest of a good pin come back as written.
#[test]
fn a_good_pin_is_read_as_written() {
    let pin = Pin::parse(&good_pin()).expect("a good pin");
    assert_eq!(pin.tag, "v0.2.3");
    assert_eq!(pin.version.to_string(), "0.2.3");
    assert_eq!(
        pin.asset(Platform::LinuxArm64),
        &Asset {
            url: "https://github.com/komira-ai/hostkeeper/releases/download/v0.2.3/hostkeeper-0.2.3-linux-arm64.tar.gz".to_owned(),
            sha256: SHA_A.to_owned(),
        }
    );
    assert_eq!(pin.asset(Platform::LinuxX86_64).sha256, SHA_B);
    assert_eq!(pin.asset(Platform::MacosArm64).sha256, SHA_C);
}

/// Catches a pin that names something the rig would not check: each defect, one at a time, is
/// refused with the message naming it, and nothing is read from it.
#[test]
fn every_defect_of_a_pin_is_refused_by_name() {
    let cases: Vec<(String, String)> = vec![
        (
            "tag = ".to_owned(),
            "the pin is not TOML: string values must be quoted, expected literal string".to_owned(),
        ),
        (
            pin_with(
                "tag = \"v0.2.3\"\n",
                "tag = \"v0.2.3\"\nchannel = \"main\"\n",
            ),
            "the pin has the key \"channel\", which it does not define".to_owned(),
        ),
        (
            pin_with("tag = \"v0.2.3\"\n", ""),
            "the pin has no tag".to_owned(),
        ),
        (
            pin_with("version = \"0.2.3\"\n", ""),
            "the pin has no version".to_owned(),
        ),
        (
            pin_with("version = \"0.2.3\"", "version = 3"),
            "the pin's version is not text".to_owned(),
        ),
        (
            pin_with("tag = \"v0.2.3\"", "tag = [\"v0.2.3\"]"),
            "the pin's tag is not text".to_owned(),
        ),
        (
            pin_with("version = \"0.2.3\"", "version = \"0.2.3-rc1\""),
            "the pin's version \"0.2.3-rc1\" is not major.minor.patch".to_owned(),
        ),
        (
            pin_with("tag = \"v0.2.3\"", "tag = \"0.2.3\""),
            "the pin's tag \"0.2.3\" is not v0.2.3".to_owned(),
        ),
        (
            pin_with(
                &format!(
                    "[macos-arm64]\nurl = \"{}\"\nsha256 = \"{SHA_C}\"\n",
                    asset_url("v0.2.3", "0.2.3", "macos-arm64")
                ),
                "",
            ),
            "the pin has no [macos-arm64]".to_owned(),
        ),
        (
            pin_with(
                &format!("sha256 = \"{SHA_B}\"\n"),
                &format!("sha256 = \"{SHA_B}\"\nsize = 1\n"),
            ),
            "the pin has the key \"linux-x86_64.size\", which it does not define".to_owned(),
        ),
        (
            pin_with(
                &format!(
                    "url = \"{}\"\n",
                    asset_url("v0.2.3", "0.2.3", "linux-x86_64")
                ),
                "",
            ),
            "the pin has no linux-x86_64.url".to_owned(),
        ),
        (
            pin_with(
                &asset_url("v0.2.3", "0.2.3", "linux-x86_64"),
                &asset_url("v0.2.3", "0.2.3", "linux-arm64"),
            ),
            format!(
                "the pin's url of linux-x86_64 is {:?}, not {:?}",
                asset_url("v0.2.3", "0.2.3", "linux-arm64"),
                asset_url("v0.2.3", "0.2.3", "linux-x86_64")
            ),
        ),
        (
            pin_with(&format!("sha256 = \"{SHA_B}\"\n"), ""),
            "the pin has no linux-x86_64.sha256".to_owned(),
        ),
        (
            pin_with(SHA_B, &SHA_B.to_uppercase()),
            format!(
                "the pin's sha256 of linux-x86_64 is {:?}, not 64 lower-case hex digits",
                SHA_B.to_uppercase()
            ),
        ),
        (
            pin_with(SHA_B, &SHA_B[1..]),
            format!(
                "the pin's sha256 of linux-x86_64 is {:?}, not 64 lower-case hex digits",
                &SHA_B[1..]
            ),
        ),
        (
            pin_with(&format!("sha256 = \"{SHA_B}\""), "sha256 = 5"),
            "the pin's linux-x86_64.sha256 is not text".to_owned(),
        ),
    ];
    for (text, expected) in cases {
        let said = refusal(&text);
        assert!(
            said.starts_with(&expected),
            "{text}\nsaid: {said}\nexpected: {expected}"
        );
    }
}

/// Catches a key outside the release platforms taken as one, and a platform that is not a
/// table: each is refused by name.
#[test]
fn a_pin_with_another_platform_or_a_platform_that_is_not_a_table_is_refused() {
    let other = format!("{}\n[windows-x86_64]\nurl = \"x\"\n", good_pin());
    assert_eq!(
        refusal(&other),
        "the pin has the key \"windows-x86_64\", which it does not define"
    );
    let flat = good_pin().replacen(
        &format!(
            "[macos-arm64]\nurl = \"{}\"\nsha256 = \"{SHA_C}\"\n",
            asset_url("v0.2.3", "0.2.3", "macos-arm64")
        ),
        "",
        1,
    );
    let flat = flat.replacen(
        "version = \"0.2.3\"\n",
        "version = \"0.2.3\"\nmacos-arm64 = \"x\"\n",
        1,
    );
    assert_eq!(refusal(&flat), "the pin's [macos-arm64] is not a table");
}

/// A gzip'd tar of `entries`, each `(name, bytes)` a file of mode 0555.
fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for (name, data) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(0o555);
        header.set_cksum();
        builder
            .append_data(&mut header, name, *data)
            .expect("append an entry");
    }
    builder
        .into_inner()
        .and_then(GzEncoder::finish)
        .expect("finish the tarball")
}

/// The first half of a tarball whose one member is larger than the half: the member's bytes end
/// before its header says.
fn truncated() -> Vec<u8> {
    let data: Vec<u8> = (0..64 * 1024u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let whole = tarball(&[(MEMBER, &data)]);
    whole[..whole.len() / 2].to_vec()
}

/// `bytes` written at `<dir>/hostkeeper.tar.gz`, and the asset that pins them.
fn downloaded(dir: &Path, bytes: &[u8]) -> (PathBuf, Asset) {
    let path = dir.join(DOWNLOAD_FILE);
    fs::File::create(&path)
        .and_then(|mut file| file.write_all(bytes))
        .expect("write the tarball");
    let asset = Asset {
        url: "https://example.invalid/x.tar.gz".to_owned(),
        sha256: hex::encode(Sha256::digest(bytes)),
    };
    (path, asset)
}

/// Catches the test running a binary other than the pinned release's: a tarball whose digest is
/// the pin's gives exactly its one member's bytes.
#[test]
fn the_pinned_tarball_gives_its_one_member() {
    let dir = scratch("good");
    let (path, asset) = downloaded(dir.path(), &tarball(&[(MEMBER, b"the released daemon")]));
    let binary = checked_binary(&path, &asset).expect("the pinned tarball");
    assert_eq!(binary, b"the released daemon");
}

/// Catches a test that runs whatever binary a stale cache or a changed download left: a tarball
/// one byte away from the pinned one fails, naming the path and both digests.
#[test]
fn a_tarball_that_differs_from_the_pin_fails_by_name() {
    let dir = scratch("differs");
    let good = tarball(&[(MEMBER, b"the released daemon")]);
    let (path, asset) = downloaded(dir.path(), &good);
    let mut other = good.clone();
    let last = other.len() - 1;
    other[last] ^= 1;
    fs::write(&path, &other).expect("write the other tarball");
    let actual = hex::encode(Sha256::digest(&other));
    let error = checked_binary(&path, &asset).expect_err("a tarball that differs");
    assert_eq!(
        error.to_string(),
        format!(
            "{} differs from the pin: its sha256 is {actual}, the pin's {}",
            path.display(),
            asset.sha256
        )
    );
    assert!(matches!(error, PinError::Differs { .. }), "{error:?}");
}

/// Catches a test that skips, or fetches for itself, when the workflow did not download the
/// release: a missing tarball fails naming the path and the workflow step that puts it there.
#[test]
fn a_tarball_that_was_not_downloaded_fails_by_name() {
    let dir = scratch("missing");
    let path = dir.path().join(DOWNLOAD_FILE);
    let asset = Asset {
        url: "https://example.invalid/x.tar.gz".to_owned(),
        sha256: SHA_A.to_owned(),
    };
    let error = checked_binary(&path, &asset).expect_err("no tarball");
    assert_eq!(
        error.to_string(),
        format!(
            "the pinned release is not downloaded at {}: privileged.yml's step `Check the previous release (pinned checksum)` puts it there",
            path.display()
        )
    );
}

/// Catches a read error taken for a missing download or a mismatch: a directory where the
/// tarball should be is an error of its own, naming the path.
#[test]
fn a_tarball_that_cannot_be_read_fails_naming_its_path() {
    let dir = scratch("unreadable");
    let path = dir.path().join(DOWNLOAD_FILE);
    fs::create_dir(&path).expect("a directory in its place");
    let asset = Asset {
        url: "https://example.invalid/x.tar.gz".to_owned(),
        sha256: SHA_A.to_owned(),
    };
    let error = checked_binary(&path, &asset).expect_err("a directory");
    assert!(matches!(error, PinError::Unreadable { .. }), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with(&format!("{}: ", path.display())),
        "{error}"
    );
}

/// Catches a pinned file that is not a release's binary tarball taken as one: a file that is not
/// gzip, a tar with another member, with a second member, with none, and one cut off inside its
/// member are each refused, even with the right digest.
#[test]
fn a_pinned_file_that_is_not_a_binary_tarball_is_refused() {
    let cases: [(&str, Vec<u8>, &str); 5] = [
        ("plain", b"not gzip".to_vec(), "it holds"),
        (
            "other",
            tarball(&[("bin/hostkeeper", b"x")]),
            "it holds \"bin/hostkeeper\", not only one \"hostkeeper\"",
        ),
        (
            "two",
            tarball(&[(MEMBER, b"x"), (MEMBER, b"y")]),
            "it holds \"hostkeeper\", not only one \"hostkeeper\"",
        ),
        ("empty", tarball(&[]), "it holds no \"hostkeeper\""),
        ("truncated", truncated(), "it holds"),
    ];
    for (name, bytes, why) in cases {
        let dir = scratch(name);
        let (path, asset) = downloaded(dir.path(), &bytes);
        let error = checked_binary(&path, &asset).expect_err(name);
        let prefix = format!("{} is not a release's binary tarball: ", path.display());
        let said = error.to_string();
        assert!(said.starts_with(&prefix), "{name}: {said}");
        if !["plain", "truncated"].contains(&name) {
            assert_eq!(said, format!("{prefix}{why}"), "{name}");
        }
    }
}

/// Catches the download looked for anywhere but `<target>/previous-release/hostkeeper.tar.gz`,
/// where the workflow puts it.
#[test]
fn the_download_is_looked_for_in_the_target_directory() {
    assert_eq!(
        download_path(Path::new("/w/target")),
        Path::new("/w/target/previous-release/hostkeeper.tar.gz")
    );
}

/// Catches a program of this build that runs as the version it was built with, or as the name
/// of a link: a file named `0.1.139`, and `current` linking to it, run as `0.1.139`.
#[test]
fn a_program_runs_as_the_version_its_file_is_named() {
    let dir = scratch("named");
    let versions = dir.path().join("versions");
    fs::create_dir(&versions).expect("versions/");
    fs::write(versions.join("0.1.139"), b"program").expect("a staged program");
    std::os::unix::fs::symlink("versions/0.1.139", dir.path().join("current")).expect("current");
    assert_eq!(
        program_version(&versions.join("0.1.139")).map(|v| v.to_string()),
        Ok("0.1.139".to_owned())
    );
    assert_eq!(
        program_version(&dir.path().join("current")).map(|v| v.to_string()),
        Ok("0.1.139".to_owned())
    );
}

/// Catches a program that starts under a name that is no version, or from a path that is not
/// there: each is refused with a message naming the path.
#[test]
fn a_program_whose_file_is_not_a_version_is_refused() {
    let dir = scratch("unnamed");
    let program = dir.path().join("hostkeeper");
    fs::write(&program, b"program").expect("a program");
    let real = fs::canonicalize(&program).expect("its real path");
    assert_eq!(
        program_version(&program).map(|v| v.to_string()),
        Err(format!(
            "the daemon runs from versions/<major.minor.patch> of a binary directory, not from {}",
            real.display()
        ))
    );
    let gone = dir.path().join("gone");
    let said = program_version(&gone).expect_err("no such file");
    assert!(said.starts_with(&format!("{}: ", gone.display())), "{said}");
}
