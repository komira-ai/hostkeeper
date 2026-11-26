//! `hk xcode add`: the checks before the upload, one by one (design section 2.3): the archive's
//! kind, the store asked first, the record that saves the expansion, the free space, `codesign`,
//! and the same release from other bytes.

use std::os::unix::fs::PermissionsExt as _;

use serde_json::{Value, json};

use crate::add_rig::{Add, Store, archive_bytes, lists, puts, sha256};
use crate::rig::STAMP;
use crate::verify_rig::df;

/// Nothing ran and nothing was uploaded: no program of the proof, no upload.
fn nothing_ran(add: &Add, server: &crate::rig::Server) {
    assert!(puts(server).is_empty());
    for tool in ["pkgutil", "xip", "plutil", "codesign", "df"] {
        assert_eq!(add.runs(tool), 0, "{tool} ran");
    }
}

/// Proves: an archive that starts `AA01` (Apple Archive) or `AEA1` (Apple Encrypted Archive) is
/// `archive_kind_unsupported: <kind>` (exit 1), and any other first bytes, a file of fewer than
/// four bytes and an empty file are `not_an_xip` (exit 2) naming the bytes; each before the
/// store is asked, with no program run and nothing uploaded.
#[test]
fn a_kind_that_is_no_xip_uploads_nothing() {
    let add = Add::new("kind");
    let server = Store::default().serve(&add.rig);
    let cases: [(&[u8], u8, &str); 5] = [
        (
            b"AA01, an apple archive",
            1,
            "archive_kind_unsupported: AA01 (",
        ),
        (
            b"AEA1, an encrypted archive",
            1,
            "archive_kind_unsupported: AEA1 (",
        ),
        (b"PK\x03\x04zip", 2, "not_an_xip: "),
        (b"xar", 2, "not_an_xip: "),
        (b"", 2, "not_an_xip: "),
    ];
    for (n, (bytes, code, says)) in cases.into_iter().enumerate() {
        let path = add.file("Xcode_27.2.xip", bytes, 100);
        let ran = add.add(&server, &[&path]);
        assert_eq!(ran.code, code, "{n}: {ran:?}");
        assert!(
            ran.stderr
                .lines()
                .last()
                .unwrap_or_default()
                .starts_with(&format!("hk: {says}")),
            "{n}: {ran:?}"
        );
        assert_eq!(ran.stdout, "", "{n}");
        assert!(server.seen().is_empty(), "{n}: the store was asked");
        nothing_ran(&add, &server);
    }
    let path = add.file("Xcode_27.2.xip", b"AA01...", 100);
    let ran = add.add(&server, &[&path]);
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: archive_kind_unsupported: AA01 ({path}) is an Apple Archive, which the farm Macs \
             would defer; an Xcode archive here is an xip; nothing was uploaded\n"
        )),
        "{ran:?}"
    );
    let path = add.file("Xcode_27.2.xip", b"PK\x03\x04", 100);
    let ran = add.add(&server, &[&path]);
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: not_an_xip: {path} starts with the bytes 504b0304, not xar!: it is no Xcode \
             archive; nothing was uploaded\n"
        )),
        "{ran:?}"
    );
}

/// Proves: an archive the store holds runs no `pkgutil`, no `xip`, no `codesign` and no `df`, is
/// asked of the store once, uploads nothing and exits 0 printing `already in the store: Xcode
/// 27.2 (27B52)` on stdout and nothing else; with a path given there is no line about a newer
/// release, and with nothing given and a release waiting the output ends with the line that adds it.
#[test]
fn an_archive_the_store_holds_ends_the_run_at_once() {
    let add = Add::new("held");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let store = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    let server = store.clone().serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, "already in the store: Xcode 27.2 (27B52)\n"),
        "{ran:?}"
    );
    nothing_ran(&add, &server);
    assert_eq!(lists(&server).len(), 1);
    assert!(
        !add.kept(&format!("{}.json", sha256(&archive_bytes())))
            .exists()
    );
    // A newer release waits.
    let server = store.clone().waiting("27.3", "27C21").serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(
        ran.stdout, "already in the store: Xcode 27.2 (27B52)\n",
        "{ran:?}"
    );
    let ran = add.add(&server, &[]);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (
            0,
            "already in the store: Xcode 27.2 (27B52)\nXcode 27.3 is waiting: hk xcode add 27.3\n"
        ),
        "{ran:?}"
    );
    // Nothing given, a file there, no release waiting: no line about one.
    let server = store.clone().serve(&add.rig);
    let ran = add.add(&server, &[]);
    assert_eq!(
        ran.stdout, "already in the store: Xcode 27.2 (27B52)\n",
        "{ran:?}"
    );
    // A version that is the held one, written as the store does not, is no mismatch.
    let ran = add.add(&server, &["27.2.0"]);
    assert_eq!(ran.code, 0, "{ran:?}");
}

/// Proves: when a version was given and the store holds these bytes as another version, the run
/// stops with `version_mismatch: asked 27.3, these bytes are Xcode 27.2 (27B52) in the store`
/// (exit 1), running nothing.
#[test]
fn held_bytes_of_another_version_are_a_mismatch() {
    let add = Add::new("held-mismatch");
    add.file("Xcode_27.3.xip", &archive_bytes(), 100);
    let server = Store::default()
        .holding(&archive_bytes(), "27.2", "27B52")
        .serve(&add.rig);
    let ran = add.add(&server, &["27.3"]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(
            "hk: version_mismatch: asked 27.3, these bytes are Xcode 27.2 (27B52) in the store; \
             nothing was uploaded\n"
        ),
        "{ran:?}"
    );
    nothing_ran(&add, &server);
}

/// The record `hk xcode add` must write for the archive of the tests.
fn expected_record(name: &str) -> Value {
    json!({
        "record": "hk-xcode-add-v1",
        "hk": STAMP,
        "archive": {
            "name": name,
            "size": archive_bytes().len(),
            "sha256": sha256(&archive_bytes()),
            "kind": "xar!",
        },
        "signature": {"status": "signed Apple Software"},
        "expansion": {
            "version": "27.2",
            "build": "27B52",
            "codesign": {"exit": 0, "stdout": "", "stderr": ""},
        },
        "verdict": "pass",
    })
}

/// The JSON of the record kept for the archive of the tests.
fn kept_record(add: &Add) -> Value {
    let text = std::fs::read_to_string(add.kept(&format!("{}.json", sha256(&archive_bytes()))))
        .expect("the record");
    serde_json::from_str(&text).expect("JSON")
}

/// Proves: a run that expands the archive keeps the record `hk-xcode-add-v1` by exact equality
/// (the archive's name, size, SHA-256 and kind, the signature's status, the version, the build and
/// what `codesign` printed, the verdict), with mode 0600 in `~/.hostkeeper/xcode/<sha256>.json`;
/// `codesign --verify --deep --strict` is run on the expanded app; the expansion beside the
/// archive is gone; and a second run on the same archive skips `xip` and `plutil` and `df`, still
/// checks the signature, and uploads the same release.
#[test]
fn a_passing_expansion_is_kept_and_saves_the_next_one() {
    let add = Add::new("record");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(kept_record(&add), expected_record("Xcode_27.2.xip"));
    let record = add.kept(&format!("{}.json", sha256(&archive_bytes())));
    assert_eq!(
        std::fs::metadata(&record)
            .expect("a file")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        (
            add.runs("pkgutil"),
            add.runs("xip"),
            add.runs("plutil"),
            add.runs("df")
        ),
        (1, 1, 2, 1)
    );
    let signed = add.record("codesign.args");
    assert!(
        signed.starts_with("--verify\t--deep\t--strict\t")
            && signed.trim_end().ends_with("/Xcode.app"),
        "{signed}"
    );
    assert_eq!(add.beside(), ["Xcode_27.2.xip"]);
    assert!(
        ran.stderr.contains(&format!(
            "hk: Xcode 27.2 (27B52), read from the expanded app; record kept in {}\n",
            record.display()
        )),
        "{ran:?}"
    );
    let again = add.add(&server, &[&path]);
    assert_eq!(again.code, 0, "{again:?}");
    assert_eq!(
        (
            add.runs("pkgutil"),
            add.runs("xip"),
            add.runs("plutil"),
            add.runs("df"),
            add.runs("codesign")
        ),
        (2, 1, 2, 1, 1)
    );
    assert!(
        again.stderr.contains(&format!(
            "hk: Xcode 27.2 (27B52), read from the record kept in {}; the expansion is skipped\n",
            record.display()
        )),
        "{again:?}"
    );
    let seen = puts(&server);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].target, seen[1].target);
}

/// Proves: a record is ignored, and replaced after a fresh expansion, when it names another
/// SHA-256, another size, has a verdict other than `pass` or none, holds no version or a build
/// that is not one, is not JSON, is not a regular file (a link), or has a mode other than 0600
/// (0640, 0644, 0400); each leaves `xip` run once and the record that of this run.
#[test]
fn a_record_that_cannot_be_trusted_is_ignored_and_replaced() {
    let size = archive_bytes().len();
    let sha = sha256(&archive_bytes());
    let good = expected_record("Xcode_27.2.xip");
    let with = |pointer: &str, value: Value| {
        let mut record = good.clone();
        *record.pointer_mut(pointer).expect("a key") = value;
        record.to_string()
    };
    let cases: Vec<(&str, String, u32)> = vec![
        ("sha", with("/archive/sha256", json!("0".repeat(64))), 0o600),
        ("size", with("/archive/size", json!(size + 1)), 0o600),
        ("fail", with("/verdict", json!("fail: signature")), 0o600),
        ("no-verdict", with("/verdict", json!(7)), 0o600),
        ("no-version", with("/expansion/version", json!(null)), 0o600),
        (
            "bad-version",
            with("/expansion/version", json!("27.x")),
            0o600,
        ),
        (
            "bad-build",
            with("/expansion/build", json!("27 B52")),
            0o600,
        ),
        ("not-json", "not json".to_owned(), 0o600),
        ("mode-640", good.to_string(), 0o640),
        ("mode-644", good.to_string(), 0o644),
        ("mode-400", good.to_string(), 0o400),
    ];
    for (case, text, mode) in cases {
        let add = Add::new(&format!("untrusted-{case}"));
        let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
        let dir = add.rig.path("home/.hostkeeper/xcode");
        std::fs::create_dir_all(&dir).expect("directories");
        for d in [dir.parent().expect("a parent"), dir.as_path()] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).expect("a mode");
        }
        let record = dir.join(format!("{sha}.json"));
        std::fs::write(&record, text).expect("a record");
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(mode)).expect("a mode");
        let server = Store::default().serve(&add.rig);
        let ran = add.add(&server, &[&path]);
        assert_eq!(ran.code, 0, "{case}: {ran:?}");
        assert_eq!(add.runs("xip"), 1, "{case}: the archive was not expanded");
        assert_eq!(
            kept_record(&add),
            expected_record("Xcode_27.2.xip"),
            "{case}"
        );
        assert_eq!(
            std::fs::metadata(&record)
                .expect("a file")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "{case}"
        );
    }
    // A link is not a record either, even to a record that would be used.
    let add = Add::new("untrusted-link");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let dir = add.rig.path("home/.hostkeeper/xcode");
    std::fs::create_dir_all(&dir).expect("directories");
    for d in [dir.parent().expect("a parent"), dir.as_path()] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).expect("a mode");
    }
    let target = add
        .rig
        .put("elsewhere.json", good.to_string().as_bytes(), 0o600);
    std::os::unix::fs::symlink(&target, dir.join(format!("{sha}.json"))).expect("a link");
    let server = Store::default().serve(&add.rig);
    assert_eq!(add.add(&server, &[&path]).code, 0);
    assert_eq!(add.runs("xip"), 1);
}

/// Proves: a record of `hk xcode verify` whose verdict is `pass` is never replaced by this run,
/// even when it cannot be trusted (here, its mode is 0644, so the archive is expanded again), and
/// a record of `hk xcode verify` that failed is replaced.
#[test]
fn a_passing_verify_record_is_never_replaced() {
    let sha = sha256(&archive_bytes());
    for (verdict, replaced) in [("pass", false), ("fail: signature", true)] {
        let add = Add::new(&format!("verify-record-{replaced}"));
        let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
        let dir = add.rig.path("home/.hostkeeper/xcode");
        std::fs::create_dir_all(&dir).expect("directories");
        for d in [dir.parent().expect("a parent"), dir.as_path()] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).expect("a mode");
        }
        let text = json!({"record": "hk-xcode-verify-v1", "verdict": verdict}).to_string();
        let record = dir.join(format!("{sha}.json"));
        std::fs::write(&record, &text).expect("a record");
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).expect("a mode");
        let server = Store::default().serve(&add.rig);
        let ran = add.add(&server, &[&path]);
        assert_eq!(ran.code, 0, "{ran:?}");
        assert_eq!(add.runs("xip"), 1);
        let now = std::fs::read_to_string(&record).expect("a record");
        assert_eq!(now == text, !replaced, "{verdict}: {now}");
        assert_eq!(
            ran.stderr
                .contains("holds a passing record of hk xcode verify, which is kept"),
            !replaced,
            "{ran:?}"
        );
    }
}

/// Proves: a volume with less than three times the archive's size free stops the run before
/// `xip` with `free_space_short`, naming the bytes free and the bytes needed (exit 1); exactly
/// three times is enough; a `df` that fails or prints no line is a failure naming it.
#[test]
fn too_little_free_space_stops_before_the_expansion() {
    let add = Add::new("free");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let size = archive_bytes().len() as u64;
    let server = Store::default().serve(&add.rig);
    // `df` counts KiB: none free is short, and one KiB is more than three times this archive.
    add.tool("df.out", df(0).as_bytes());
    let ran = add.add(&server, &[&path]);
    let dir = add.downloads().display().to_string();
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: free_space_short: the volume of {dir} has 0 bytes free and the expansion needs \
             {} (3 times the archive's {size}); nothing was uploaded\n",
            size * 3
        )),
        "{ran:?}"
    );
    assert_eq!(add.runs("xip"), 0);
    assert!(puts(&server).is_empty());
    add.tool("df.out", df(1).as_bytes());
    assert_eq!(add.add(&server, &[&path]).code, 0);
    // The record of that run would save the expansion, and the check with it.
    std::fs::remove_file(add.kept(&format!("{}.json", sha256(&archive_bytes()))))
        .expect("a record");
    add.tool("df.out", b"");
    let ran = add.add(&server, &[&path]);
    assert!(
        ran.stderr.contains("df -Pk ")
            && ran
                .stderr
                .contains("does not print a line with an Available column"),
        "{ran:?}"
    );
    add.tool("df.exit", b"1");
    let ran = add.add(&server, &[&path]);
    assert!(
        ran.stderr.contains("exited 1; nothing was uploaded\n"),
        "{ran:?}"
    );
}

/// Proves: `codesign` refusing the expanded app uploads nothing and writes no record (exit 1),
/// naming the program, its exit and its first line of complaint; a `codesign` that cannot be run
/// is said as such; the expansion is removed either way.
#[test]
fn codesign_refusing_uploads_nothing() {
    let mut add = Add::new("codesign");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    add.tool("codesign.exit", b"1");
    add.tool(
        "codesign.--verify.err",
        b"Xcode.app: a sealed resource is missing\nmore\n",
    );
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains("codesign --verify --deep --strict ")
            && ran.stderr.ends_with(
                "/Xcode.app failed (exit status: 1): Xcode.app: a sealed resource is missing; \
                 nothing was uploaded\n"
            ),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
    assert!(
        !add.kept(&format!("{}.json", sha256(&archive_bytes())))
            .exists()
    );
    assert_eq!(add.beside(), ["Xcode_27.2.xip"]);
    add.mac.codesign = add.rig.path("bin/no-such-codesign");
    let ran = add.add(&server, &[&path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains("hk: cannot run ")
            && ran
                .stderr
                .contains("no-such-codesign (No such file or directory"),
        "{ran:?}"
    );
}

/// Proves: when the store holds this release from other bytes the run stops before the upload
/// with the whole message naming the SHA-256 held and the `hk xcode remove` line; when it holds
/// another build of the version, with the build held and the same line; another version in the
/// store is no obstacle; `27.2` and `27.2.0` are one version.
#[test]
fn another_archive_of_the_release_stops_before_the_upload() {
    let add = Add::new("other-bytes");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let held = "c".repeat(64);
    let line = format!("hk xcode remove --sha256 {held} --reason <why> first");
    let server = Store::default()
        .holding_sha(&held, "27.2", "27B52")
        .serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: the store holds Xcode 27.2 (27B52) from other bytes (sha256 {held}); if these \
             are the ones to keep, {line}; nothing was uploaded\n"
        )),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
    let server = Store::default()
        .holding_sha(&held, "27.2.0", "27B50")
        .serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: the store holds another build of Xcode 27.2: 27B50 (sha256 {held}); if build \
             27B52 is the one to keep, {line}; nothing was uploaded\n"
        )),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
    let server = Store::default()
        .holding_sha(&held, "27.1", "27A10")
        .serve(&add.rig);
    assert_eq!(add.add(&server, &[&path]).code, 0);
}
