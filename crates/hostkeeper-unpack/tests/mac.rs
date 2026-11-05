//! The macOS checks of the staged tree: no quarantine attribute on any entry, a valid code
//! signature on every Mach-O file, and nothing of either on Linux. The verifier is a fake that
//! records what it is asked.

mod support;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io;
use std::path::Path;

use hostkeeper_hook::{Class, Outcome};
use hostkeeper_unpack::{
    MacVerify, Platform, Rule, SignatureVerdict, UnpackError, Unpacked, VerifyIoError, unpack,
};
use support::{MANIFEST, Rig, Tar, limits, zstd};

/// A Mach-O header (64-bit, little-endian magic) padded to 32 bytes.
fn mach_o() -> Vec<u8> {
    let mut bytes = vec![0xCF, 0xFA, 0xED, 0xFE];
    bytes.resize(32, 0);
    bytes
}

/// A named file of a package, for [`package`].
fn named<'a>(name: &'a str, bytes: &'a [u8]) -> (&'a str, &'a [u8]) {
    (name, bytes)
}

/// Makes the error of a tool that fails.
type Failure = fn() -> VerifyIoError;

/// A verifier that records every call by the path relative to the staging directory.
#[derive(Default)]
struct Fake {
    quarantined: BTreeSet<String>,
    verdicts: BTreeMap<String, SignatureVerdict>,
    quarantine_error: Option<Failure>,
    signature_error: Option<Failure>,
    quarantine_calls: RefCell<Vec<String>>,
    signature_calls: RefCell<Vec<String>>,
}

impl Fake {
    fn quarantined(names: &[&str]) -> Fake {
        Fake {
            quarantined: names.iter().map(|n| (*n).to_owned()).collect(),
            ..Fake::default()
        }
    }

    fn verdict(name: &str, verdict: SignatureVerdict) -> Fake {
        Fake {
            verdicts: BTreeMap::from([(name.to_owned(), verdict)]),
            ..Fake::default()
        }
    }

    fn quarantine_calls(&self) -> Vec<String> {
        self.quarantine_calls.borrow().clone()
    }

    fn signature_calls(&self) -> Vec<String> {
        self.signature_calls.borrow().clone()
    }
}

/// The part of `path` below the staging directory, `/`-separated.
fn relative(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let stage = parts
        .iter()
        .position(|p| p.starts_with(".stage-"))
        .expect("a path inside the staging directory");
    parts[stage + 1..].join("/")
}

impl MacVerify for Fake {
    fn quarantine(&self, path: &Path) -> Result<bool, VerifyIoError> {
        let name = relative(path);
        self.quarantine_calls.borrow_mut().push(name.clone());
        match self.quarantine_error {
            Some(fail) => Err(fail()),
            None => Ok(self.quarantined.contains(&name)),
        }
    }

    fn signature(&self, binary: &Path) -> Result<SignatureVerdict, VerifyIoError> {
        let name = relative(binary);
        self.signature_calls.borrow_mut().push(name.clone());
        match self.signature_error {
            Some(fail) => Err(fail()),
            None => Ok(self
                .verdicts
                .get(&name)
                .cloned()
                .unwrap_or(SignatureVerdict::Valid)),
        }
    }
}

/// A package archive: the manifest of the tests, `bin/x` with the bytes `x`, `files/c.conf`, and
/// after them `bin` and `files` entries (names below those directories, the bin ones
/// executable). Returns the compressed archive and the decompressed bytes before the end
/// marker, which is what an error counts.
fn package(x: &[u8], bin: &[(&str, &[u8])], files: &[(&str, &[u8])]) -> (Vec<u8>, u64) {
    let mut tar = Tar::new();
    tar.file("manifest.toml", 0o644, MANIFEST.as_bytes())
        .dir("bin")
        .file("bin/x", 0o755, x);
    for (name, bytes) in bin {
        tar.file(&format!("bin/{name}"), 0o755, bytes);
    }
    tar.dir("units")
        .file("units/tierstore.service", 0o644, b"[Service]\n")
        .dir("files")
        .file("files/c.conf", 0o640, b"c\n");
    for (name, bytes) in files {
        tar.file(&format!("files/{name}"), 0o644, bytes);
    }
    let at = tar.bytes.len() as u64;
    (zstd(&tar.end().bytes()), at)
}

/// Unpacks `archive` in a new rig as a macOS host with `mac`.
fn on_mac(name: &str, archive: &[u8], mac: &Fake) -> (Rig, Result<Unpacked, UnpackError>) {
    let rig = Rig::new(name);
    let (path, order) = rig.archive("a.tar.zst", archive);
    let result = unpack(&path, &order, &limits(), &rig.host(), Platform::MacOs(mac));
    (rig, result)
}

/// The package directory holds nothing: no staging directory, no version, no seal.
fn assert_nothing_left(rig: &Rig) {
    assert_eq!(rig.listing(), Vec::<String>::new());
}

/// The error of a failed unpack.
fn refusal(result: Result<Unpacked, UnpackError>) -> UnpackError {
    result.expect_err("the unpack is refused")
}

const FAILED: (Outcome, Class, &str) = (Outcome::Failure, Class::Release, "bad_archive");

/// Catches a quarantine attribute that goes unseen on a file below `files/`, on a file below
/// `bin/` or on a directory: each makes the package a bad archive naming the entry and the
/// rule, and leaves no staging directory, no version directory and no seal.
#[test]
fn a_quarantined_file_in_the_staged_tree_is_bad_archive() {
    let (archive, at) = package(b"#!x\n", &[], &[]);
    for (index, name) in ["files/c.conf", "bin/x", "units"].into_iter().enumerate() {
        let fake = Fake::quarantined(&[name]);
        let (rig, result) = on_mac(&format!("mac-quarantine-{index}"), &archive, &fake);
        let err = refusal(result);
        assert_eq!(err.outcome(), FAILED, "{name}");
        assert_eq!(
            err.to_string(),
            format!(
                "bad archive: {name}: a com.apple.quarantine attribute \
                 (after {at} decompressed bytes)"
            )
        );
        assert!(
            matches!(
                &err,
                UnpackError::BadArchive {
                    rule: Rule::Quarantined,
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(fake.signature_calls().is_empty(), "{name}");
        assert_nothing_left(&rig);
    }
}

/// Catches a Mach-O binary with no signature, or with one that does not verify, passing: each
/// is a bad archive naming the binary, with a message of its own, and nothing is left.
#[test]
fn an_unsigned_or_invalidly_signed_binary_is_bad_archive() {
    let (archive, at) = package(&mach_o(), &[], &[]);
    let note = "code or signature have been modified";
    let cases = [
        (
            SignatureVerdict::Unsigned,
            format!(
                "bad archive: bin/x: a Mach-O binary with no code signature (unsigned) \
                 (after {at} decompressed bytes)"
            ),
        ),
        (
            SignatureVerdict::Invalid {
                note: note.to_owned(),
            },
            format!(
                "bad archive: bin/x: a Mach-O binary whose code signature is invalid \
                 (invalid_signature): {note} (after {at} decompressed bytes)"
            ),
        ),
    ];
    for (index, (verdict, message)) in cases.into_iter().enumerate() {
        let fake = Fake::verdict("bin/x", verdict);
        let (rig, result) = on_mac(&format!("mac-signature-{index}"), &archive, &fake);
        let err = refusal(result);
        assert_eq!(err.outcome(), FAILED);
        assert_eq!(err.to_string(), message);
        assert_eq!(fake.signature_calls(), ["bin/x"]);
        assert_nothing_left(&rig);
    }
}

/// Catches a script or a data file sent to the signature check: with a shebang script in
/// `bin/`, a text file in `files/` and one Mach-O in `bin/`, the signature is asked for the
/// Mach-O alone, and the quarantine for every entry, directories included, in archive order.
#[test]
fn only_mach_o_files_are_checked_for_a_signature() {
    let (archive, _) = package(
        b"#!/bin/sh\nexit 0\n",
        &[("tool", &mach_o())],
        &[("note.txt", b"text\n")],
    );
    let fake = Fake::default();
    let (_rig, result) = on_mac("mac-only-mach-o", &archive, &fake);
    assert!(result.expect("the package is accepted").fresh);
    assert_eq!(fake.signature_calls(), ["bin/tool"]);
    assert_eq!(
        fake.quarantine_calls(),
        [
            "manifest.toml",
            "bin",
            "bin/x",
            "bin/tool",
            "units",
            "units/tierstore.service",
            "files",
            "files/c.conf",
            "files/note.txt",
        ]
    );
}

/// Catches a Mach-O file missed, or a file taken for one wrongly: files with each thin and fat
/// magic number are checked; a Java class file (the fat magic with a version of 45 or more), a
/// file cut short of its magic, an empty file and an ELF file are not.
#[test]
fn files_are_taken_for_mach_o_by_their_magic_number() {
    let java: &[u8] = &[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52];
    let fat: &[u8] = &[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 2];
    let files = vec![
        named("thin64", &[0xFE, 0xED, 0xFA, 0xCF]),
        named("thin64_swapped", &[0xCF, 0xFA, 0xED, 0xFE]),
        named("thin32", &[0xFE, 0xED, 0xFA, 0xCE]),
        named("thin32_swapped", &[0xCE, 0xFA, 0xED, 0xFE]),
        named("fat", fat),
        named("fat64", &[0xCA, 0xFE, 0xBA, 0xBF]),
        named("fat_swapped", &[0xBE, 0xBA, 0xFE, 0xCA]),
        named("fat64_swapped", &[0xBF, 0xBA, 0xFE, 0xCA]),
        named("java.class", java),
        named("cut_short", &[0xCF, 0xFA, 0xED]),
        named("empty", &[]),
        named("elf", &[0x7F, b'E', b'L', b'F', 2, 1, 1, 0]),
    ];
    let (archive, _) = package(b"#!x\n", &[], &files);
    let fake = Fake::default();
    let (_rig, result) = on_mac("mac-magic", &archive, &fake);
    result.expect("the package is accepted");
    assert_eq!(
        fake.signature_calls(),
        [
            "files/thin64",
            "files/thin64_swapped",
            "files/thin32",
            "files/thin32_swapped",
            "files/fat",
            "files/fat64",
            "files/fat_swapped",
            "files/fat64_swapped",
        ]
    );
}

/// Catches a tool that cannot run or does not finish being taken for a verdict on the package:
/// for the quarantine check and for the signature check, with each of the two causes, the
/// outcome is `Deferred`, `environment`, `verify_unavailable`, the message names the entry and
/// the cause, and the staging directory is removed.
#[test]
fn a_verification_tool_failure_defers_and_never_judges_the_package() {
    let (archive, _) = package(&mach_o(), &[], &[]);
    let spawn: Failure = || VerifyIoError::Spawn {
        tool: "xattr",
        source: io::Error::other("no such tool"),
    };
    let unfinished: Failure = || VerifyIoError::Unfinished {
        tool: "xattr",
        why: "timed out after 30 s".to_owned(),
    };
    let signature_spawn: Failure = || VerifyIoError::Spawn {
        tool: "codesign",
        source: io::Error::other("no such tool"),
    };
    let signature_unfinished: Failure = || VerifyIoError::Unfinished {
        tool: "codesign",
        why: "killed".to_owned(),
    };
    let cases = [
        (
            Fake {
                quarantine_error: Some(spawn),
                ..Fake::default()
            },
            "cannot verify manifest.toml on macOS: xattr could not be run: no such tool",
            "xattr could not be run: no such tool",
        ),
        (
            Fake {
                quarantine_error: Some(unfinished),
                ..Fake::default()
            },
            "cannot verify manifest.toml on macOS: xattr did not finish: timed out after 30 s",
            "xattr did not finish: timed out after 30 s",
        ),
        (
            Fake {
                signature_error: Some(signature_spawn),
                ..Fake::default()
            },
            "cannot verify bin/x on macOS: codesign could not be run: no such tool",
            "codesign could not be run: no such tool",
        ),
        (
            Fake {
                signature_error: Some(signature_unfinished),
                ..Fake::default()
            },
            "cannot verify bin/x on macOS: codesign did not finish: killed",
            "codesign did not finish: killed",
        ),
    ];
    for (index, (fake, message, cause)) in cases.into_iter().enumerate() {
        let (rig, result) = on_mac(&format!("mac-tool-{index}"), &archive, &fake);
        let err = refusal(result);
        assert_eq!(
            err.outcome(),
            (Outcome::Deferred, Class::Environment, "verify_unavailable")
        );
        assert_eq!(err.to_string(), message);
        assert_eq!(err.source().map(|e| e.to_string()).as_deref(), Some(cause));
        assert_nothing_left(&rig);
    }
}

/// Catches the macOS checks changing what is unpacked: the same archive, with a signed Mach-O
/// and no quarantine, unpacks on Linux and on macOS to a fresh version with the same tree
/// digest and the same archive digest in its seal.
#[test]
fn a_valid_signed_unquarantined_package_still_seals_and_publishes() {
    let (archive, _) = package(&mach_o(), &[named("script", b"#!/bin/sh\n")], &[]);
    let linux = Rig::new("mac-same-linux");
    let (path, order) = linux.archive("a.tar.zst", &archive);
    let on_linux = unpack(&path, &order, &limits(), &linux.host(), Platform::Linux)
        .expect("the package unpacks on Linux");
    let fake = Fake::default();
    let (_rig, result) = on_mac("mac-same-macos", &archive, &fake);
    let on_macos = result.expect("the package unpacks on macOS");
    assert!(on_linux.fresh);
    assert!(on_macos.fresh);
    assert_eq!(on_linux.seal.tree_digest(), on_macos.seal.tree_digest());
    assert_eq!(on_linux.seal, on_macos.seal);
    assert_eq!(fake.signature_calls(), ["bin/x"]);
}

/// Catches the macOS checks running on Linux: an archive with an unsigned Mach-O and a
/// quarantined file unpacks on Linux, and the same archive is refused as a macOS host, so the
/// platform is what guards the checks.
#[test]
fn linux_does_not_call_the_mac_checks() {
    let (archive, _) = package(&mach_o(), &[], &[]);
    let linux = Rig::new("mac-guard-linux");
    let (path, order) = linux.archive("a.tar.zst", &archive);
    let unpacked =
        unpack(&path, &order, &limits(), &linux.host(), Platform::Linux).expect("Linux unpacks it");
    assert!(unpacked.fresh);
    let fake = Fake {
        quarantined: BTreeSet::from(["files/c.conf".to_owned()]),
        verdicts: BTreeMap::from([("bin/x".to_owned(), SignatureVerdict::Unsigned)]),
        ..Fake::default()
    };
    let (_rig, result) = on_mac("mac-guard-macos", &archive, &fake);
    let err = refusal(result);
    assert_eq!(err.outcome(), FAILED);
    assert!(
        matches!(
            &err,
            UnpackError::BadArchive {
                rule: Rule::Quarantined,
                ..
            }
        ),
        "{err:?}"
    );
}

/// Catches a Mach-O file with no signature passing when it is the only fault: quarantine is
/// clean, so the signature check runs and refuses it.
#[test]
fn an_unsigned_binary_is_refused_when_nothing_is_quarantined() {
    let (archive, _) = package(&mach_o(), &[], &[]);
    let fake = Fake::verdict("bin/x", SignatureVerdict::Unsigned);
    let (_rig, result) = on_mac("mac-unsigned-only", &archive, &fake);
    let err = refusal(result);
    assert!(
        matches!(
            &err,
            UnpackError::BadArchive {
                rule: Rule::Unsigned,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(fake.quarantine_calls().len(), 7);
}

/// Catches a platform that prints as something else, and a verdict that does not compare: the
/// debug text of each platform and of a tool failure is what it names.
#[test]
fn a_platform_and_a_tool_failure_print_what_they_name() {
    let fake = Fake::default();
    assert_eq!(format!("{:?}", Platform::MacOs(&fake)), "MacOs");
    let unfinished = VerifyIoError::Unfinished {
        tool: "codesign",
        why: "killed".to_owned(),
    };
    assert_eq!(
        format!("{unfinished:?}"),
        r#"Unfinished { tool: "codesign", why: "killed" }"#
    );
    assert_eq!(format!("{:?}", Platform::Linux), "Linux");
    let invalid = SignatureVerdict::Invalid {
        note: "n".to_owned(),
    };
    assert_eq!(invalid.clone(), invalid);
    assert_ne!(invalid, SignatureVerdict::Valid);
    assert_eq!(format!("{:?}", SignatureVerdict::Unsigned), "Unsigned");
}

/// Catches a rule whose text drifts from the design: the three rules of the macOS checks print
/// exactly their words.
#[test]
fn the_macos_rules_say_what_they_refuse() {
    assert_eq!(
        Rule::Quarantined.to_string(),
        "a com.apple.quarantine attribute"
    );
    assert_eq!(
        Rule::Unsigned.to_string(),
        "a Mach-O binary with no code signature (unsigned)"
    );
    assert_eq!(
        Rule::InvalidSignature("why".to_owned()).to_string(),
        "a Mach-O binary whose code signature is invalid (invalid_signature): why"
    );
}
