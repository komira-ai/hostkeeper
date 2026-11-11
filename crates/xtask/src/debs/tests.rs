use std::path::Path;

use super::*;
use crate::mirrored::WAITS;
use crate::mirrored::tests::{Answer, FakeWeb, Pauses, names, scratch};

const A: &[u8] = b"package a";
const B: &[u8] = b"package b";

/// A pins file with two packages for each architecture, served by two mirrors.
fn pins_text() -> String {
    let table = |arch: &str| {
        format!(
            "[{arch}]\nmirrors = [\"https://one.example/ubuntu\", \"https://two.example/ubuntu\"]\n\
             debs = [\n\
             {{ package = \"a\", version = \"1.0-1\", path = \"pool/main/a/a/a_1.0-1_{arch}.deb\", sha256 = \"{}\" }},\n\
             {{ package = \"b\", version = \"2:3.1\", path = \"pool/main/b/b_3.1_all.deb\", sha256 = \"{}\" }},\n\
             ]\n",
            Sha256::of(A),
            Sha256::of(B)
        )
    };
    format!("# pins\n{}\n{}", table("amd64"), table("arm64"))
}

/// `pins_text()` with `from` replaced by `to` (which must occur in it).
fn pins_with(from: &str, to: &str) -> String {
    let text = pins_text();
    assert!(text.contains(from), "{from}");
    text.replacen(from, to, 1)
}

const A_NAME: &str = "a_1.0-1_amd64.deb";
const B_NAME: &str = "b_3.1_all.deb";
const ONE_A: &str = "https://one.example/ubuntu/pool/main/a/a/a_1.0-1_amd64.deb";
const TWO_A: &str = "https://two.example/ubuntu/pool/main/a/a/a_1.0-1_amd64.deb";
const ONE_B: &str = "https://one.example/ubuntu/pool/main/b/b_3.1_all.deb";

/// Runs `debs` for amd64 with the pins `pins` (written to `dir/pins.toml`) into `dir/debs`.
fn run_in(dir: &Path, pins: &str, web: &FakeWeb, pauses: &Pauses) -> Result<String, Error> {
    let file = dir.join("pins.toml");
    std::fs::write(&file, pins).expect("pins");
    run(&file, Arch::Amd64, &dir.join("debs"), web, &pauses.pause())
}

#[test]
fn an_architecture_is_amd64_or_arm64() {
    // Catches: Rust's architecture names mapped to the wrong Debian table, or another one taken
    // for one of them.
    assert_eq!(Arch::of("x86_64"), Ok(Arch::Amd64));
    assert_eq!(Arch::of("aarch64"), Ok(Arch::Arm64));
    assert_eq!(Arch::of("riscv64"), Err(Error::Arch("riscv64".to_owned())));
    assert_eq!(
        Error::Arch("riscv64".to_owned()).to_string(),
        "this host's architecture \"riscv64\" is neither x86_64 nor aarch64"
    );
    assert_eq!(Arch::Amd64.name(), "amd64");
    assert_eq!(Arch::Arm64.name(), "arm64");
}

#[test]
fn a_pins_file_gives_each_architecture_its_table() {
    // Catches: the wrong table read, an epoch kept in the file name, or an `all` package
    // refused.
    let text = pins_text();
    for (arch, a) in [(Arch::Amd64, A_NAME), (Arch::Arm64, "a_1.0-1_arm64.deb")] {
        let pins = parse(&text, arch).expect("pins");
        assert_eq!(
            pins.mirrors,
            ["https://one.example/ubuntu", "https://two.example/ubuntu"]
        );
        let names: Vec<&str> = pins.debs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, [a, B_NAME]);
        assert_eq!(pins.debs[0].sha256, Sha256::of(A));
        assert_eq!(pins.debs[1].path, "pool/main/b/b_3.1_all.deb");
    }
}

#[test]
fn a_pins_file_that_breaks_a_rule_is_refused() {
    // Catches: each rule of the pins file not kept: TOML, known keys, a table for the host,
    // plain HTTPS mirrors, at least one package, a digest, a path inside `pool/` that ends in
    // the name the package, version and architecture make, and no file pinned twice.
    let cases: Vec<(String, &str)> = vec![
        ("[amd64\n".to_owned(), "unclosed table"),
        (pins_with("[arm64]", "[i386]"), "unknown field `i386`"),
        (
            pins_with("mirrors = [", "colour = 1\nmirrors = ["),
            "unknown field `colour`",
        ),
        (
            pins_with(", sha256 = ", ", size = 1, sha256 = "),
            "unknown field `size`",
        ),
        (
            pins_text()
                .split_once("[arm64]")
                .map(|(_, arm64)| format!("[arm64]{arm64}"))
                .expect("an arm64 table"),
            "no [amd64] table",
        ),
        (
            pins_with(
                "[\"https://one.example/ubuntu\", \"https://two.example/ubuntu\"]",
                "[]",
            ),
            "[amd64] names no mirror",
        ),
        (
            pins_with(
                "https://one.example/ubuntu\"",
                "http://one.example/ubuntu\"",
            ),
            "the mirror \"http://one.example/ubuntu\" is not an https:// URL with no trailing '/', query, fragment or user",
        ),
        (
            pins_with("https://one.example/ubuntu\"", "https://\""),
            "the mirror \"https://\" is not",
        ),
        (
            pins_with(
                "https://one.example/ubuntu\"",
                "https://one.example/ubuntu/\"",
            ),
            "the mirror \"https://one.example/ubuntu/\" is not",
        ),
        (
            pins_with(
                "https://one.example/ubuntu\"",
                "https://one.example/u?x=1\"",
            ),
            "the mirror \"https://one.example/u?x=1\" is not",
        ),
        (
            pins_with("https://one.example/ubuntu\"", "https://me@one.example/u\""),
            "the mirror \"https://me@one.example/u\" is not",
        ),
        (
            pins_text()
                .split_once("debs = [")
                .map(|(head, tail)| {
                    let rest = tail.split_once("]\n").expect("the end").1;
                    format!("{head}debs = []\n{rest}")
                })
                .expect("a debs list"),
            "[amd64] pins no package",
        ),
        (
            pins_with(&format!("\"{}\"", Sha256::of(A)), "\"abc\""),
            "not a SHA-256: \"abc\"",
        ),
        (
            pins_with("path = \"pool/main/a/a/", "path = \"dists/main/a/a/"),
            "the path \"dists/main/a/a/a_1.0-1_amd64.deb\" of a 1.0-1 is not pool/.../a_1.0-1_amd64.deb (or the `all` package)",
        ),
        (
            pins_with("path = \"pool/main/a/a/", "path = \"pool/../a/a/"),
            "the path \"pool/../a/a/a_1.0-1_amd64.deb\" of a 1.0-1 is not",
        ),
        (
            pins_with("path = \"pool/main/a/a/", "path = \"pool/main//a/"),
            "the path \"pool/main//a/a_1.0-1_amd64.deb\" of a 1.0-1 is not",
        ),
        (
            pins_with("a_1.0-1_amd64.deb", "a_1.0-2_amd64.deb"),
            "the path \"pool/main/a/a/a_1.0-2_amd64.deb\" of a 1.0-1 is not",
        ),
        (
            pins_with("a_1.0-1_amd64.deb", "a_1.0-1_arm64.deb"),
            "the path \"pool/main/a/a/a_1.0-1_arm64.deb\" of a 1.0-1 is not",
        ),
        (
            pins_with(
                &format!("sha256 = \"{}\" }},\n]", Sha256::of(B)),
                &format!(
                    "sha256 = \"{b}\" }},\n{{ package = \"b\", version = \"3.1\", \
                     path = \"pool/x/b_3.1_all.deb\", sha256 = \"{b}\" }},\n]",
                    b = Sha256::of(B)
                ),
            ),
            "b_3.1_all.deb is pinned twice",
        ),
    ];
    for (text, why) in cases {
        let err = parse(&text, Arch::Amd64).expect_err(why);
        assert!(err.starts_with(why), "{err:?} does not start with {why:?}");
    }
}

#[test]
fn cached_files_that_match_are_kept_without_a_download() {
    // Catches: a cache hit that still reaches the network, or a pause on a hit.
    let dir = scratch("debs-hit");
    std::fs::create_dir(dir.join("debs")).expect("debs");
    std::fs::write(dir.join("debs").join(A_NAME), A).expect("a");
    std::fs::write(dir.join("debs").join(B_NAME), B).expect("b");
    let web = FakeWeb::default();
    let pauses = Pauses::default();
    let out = run_in(&dir, &pins_text(), &web, &pauses).expect("kept");
    assert_eq!(
        out,
        format!(
            "ok: the cached {A_NAME} has SHA-256 {}\nok: the cached {B_NAME} has SHA-256 {}",
            Sha256::of(A),
            Sha256::of(B)
        )
    );
    assert_eq!(web.urls(), Vec::<String>::new());
    assert_eq!(*pauses.0.borrow(), Vec::<Duration>::new());
}

#[test]
fn a_cache_miss_fetches_each_file_from_the_first_mirror_that_has_it() {
    // Catches: a miss not fetched, fetched from the wrong URL (mirror and pool path), with no
    // fall-through to the next mirror, or a second run that downloads again.
    let dir = scratch("debs-miss");
    let web = FakeWeb::with(&[
        (ONE_A, Answer::Fail("dns error".to_owned())),
        (TWO_A, Answer::Body(A.to_vec())),
        (ONE_B, Answer::Body(B.to_vec())),
    ]);
    let pauses = Pauses::default();
    let out = run_in(&dir, &pins_text(), &web, &pauses).expect("fetched");
    assert_eq!(
        out,
        format!(
            "ok: {TWO_A} has SHA-256 {}, after 1 failed: {ONE_A}: dns error\n\
             ok: {ONE_B} has SHA-256 {}",
            Sha256::of(A),
            Sha256::of(B)
        )
    );
    assert_eq!(web.urls(), [ONE_A, TWO_A, ONE_B]);
    assert!(web.gets.borrow().iter().all(|(_, limit)| limit.is_none()));
    assert_eq!(names(&dir.join("debs")), [A_NAME, B_NAME]);
    assert_eq!(std::fs::read(dir.join("debs").join(B_NAME)).expect("b"), B);

    let again = FakeWeb::default();
    run_in(&dir, &pins_text(), &again, &Pauses::default()).expect("kept");
    assert_eq!(again.urls(), Vec::<String>::new());
}

#[test]
fn a_package_no_mirror_gives_fails_the_run_naming_each_attempt() {
    // Catches: a run that goes on after a package could not be fetched, or whose error does not
    // name the attempts and that nothing was installed.
    let dir = scratch("debs-none");
    let web = FakeWeb::with(&[(ONE_A, Answer::Body(A.to_vec()))]);
    let pauses = Pauses::default();
    let err = run_in(&dir, &pins_text(), &web, &pauses).unwrap_err();
    let two_b = "https://two.example/ubuntu/pool/main/b/b_3.1_all.deb";
    let why = "dns error: failed to lookup address information";
    let attempts: Vec<String> = (0..3)
        .flat_map(|_| [ONE_B, two_b])
        .map(|url| format!("{url}: {why}"))
        .collect();
    assert_eq!(
        err.to_string(),
        format!(
            "{B_NAME}: no download was the pinned file: {}; nothing was installed",
            attempts.join("; ")
        )
    );
    assert_eq!(
        *pauses.0.borrow(),
        [[Duration::ZERO].as_slice(), &WAITS[..]].concat()
    );
    assert_eq!(names(&dir.join("debs")), [A_NAME]);
}

#[test]
fn a_wrong_cached_file_and_a_stray_file_are_refused() {
    // Catches: a cached file of other bytes replaced or installed, and a file no pin names (a
    // stale `.part` included) left for `dpkg --install <dir>/*.deb` to install.
    let dir = scratch("debs-wrong");
    std::fs::create_dir(dir.join("debs")).expect("debs");
    std::fs::write(dir.join("debs").join(A_NAME), b"other").expect("a");
    let web = FakeWeb::default();
    let err = run_in(&dir, &pins_text(), &web, &Pauses::default()).unwrap_err();
    let path = dir.join("debs").join(A_NAME);
    assert_eq!(
        err.to_string(),
        format!(
            "the cached {path:?} has SHA-256 {}, not the pinned {}; nothing was installed",
            Sha256::of(b"other"),
            Sha256::of(A)
        )
    );
    assert_eq!(std::fs::read(&path).expect("left as it was"), b"other");
    assert_eq!(web.urls(), Vec::<String>::new());

    for stray in ["evil_1_amd64.deb", "b_3.1_all.deb.part"] {
        let dir = scratch("debs-stray");
        std::fs::create_dir(dir.join("debs")).expect("debs");
        std::fs::write(dir.join("debs").join(stray), b"x").expect("stray");
        let err = run_in(&dir, &pins_text(), &web, &Pauses::default()).unwrap_err();
        assert_eq!(
            err,
            Error::Stray {
                dir: dir.join("debs"),
                name: stray.to_owned()
            }
        );
        assert_eq!(
            err.to_string(),
            format!(
                "{:?} holds {stray:?}, which no pin names; nothing was installed",
                dir.join("debs")
            )
        );
    }
    assert_eq!(web.urls(), Vec::<String>::new());
}

#[test]
fn file_system_failures_name_their_path() {
    // Catches: a missing pins file, a directory path that is a file, and a pinned name that is
    // a directory, each failing without the path that failed.
    let dir = scratch("debs-fs");
    let web = FakeWeb::default();
    let missing = dir.join("missing.toml");
    let err = run(
        &missing,
        Arch::Amd64,
        &dir,
        &web,
        &Pauses::default().pause(),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Pins { path, .. } if *path == missing),
        "{err}"
    );
    assert!(
        err.to_string()
            .starts_with(&format!("the pins file {missing:?}: "))
    );

    std::fs::write(dir.join("debs"), b"a file").expect("a file in the way");
    let err = run_in(&dir, &pins_text(), &web, &Pauses::default()).unwrap_err();
    assert!(
        matches!(&err, Error::Dir { path, .. } if *path == dir.join("debs")),
        "{err}"
    );
    assert!(err.to_string().ends_with("; nothing was installed"));

    let dir = scratch("debs-fs-dir");
    std::fs::create_dir_all(dir.join("debs").join(A_NAME)).expect("a directory in the way");
    let err = run_in(&dir, &pins_text(), &web, &Pauses::default()).unwrap_err();
    assert!(
        matches!(&err, Error::Dir { path, .. } if *path == dir.join("debs").join(A_NAME)),
        "{err}"
    );
}

#[test]
fn the_repository_pins_files_parse_and_name_a_mirror_outside_canonical() {
    // Catches: a committed pins file that does not parse, pins no package of its set, or whose
    // mirrors are all Canonical's, so that an outage of Canonical's names fails a cache miss.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/debs");
    let mut sets = names(&root);
    sets.sort();
    assert_eq!(sets, ["qemu-arm.toml", "qemu-x86.toml"]);
    for (set, wanted) in [
        ("qemu-x86.toml", ["qemu-system-x86_", "ovmf_"]),
        ("qemu-arm.toml", ["qemu-system-arm_", "qemu-efi-aarch64_"]),
    ] {
        let text = std::fs::read_to_string(root.join(set)).expect("the pins file");
        let pins = parse(&text, Arch::Amd64).expect(set);
        assert_eq!(
            parse(&text, Arch::Arm64),
            Err("no [arm64] table".to_owned())
        );
        for name in wanted {
            assert!(
                pins.debs.iter().any(|d| d.name.starts_with(name)),
                "{set}: {name}"
            );
        }
        assert_eq!(
            pins.mirrors[0], "https://archive.ubuntu.com/ubuntu",
            "{set}"
        );
        let outside = pins
            .mirrors
            .iter()
            .filter(|m| !m.contains("ubuntu.com/") && !m.contains("canonical.com/"))
            .count();
        assert!(outside >= 1, "{set}: {:?}", pins.mirrors);
    }
}
