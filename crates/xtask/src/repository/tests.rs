use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::channels::aws::Answer;
use crate::testdir::TestDir;

fn scratch(name: &str) -> TestDir {
    let beside = std::env::current_exe().expect("test binary");
    TestDir::under(
        &beside.parent().expect("deps").join("xtask-repository-unit"),
        name,
    )
}

/// A keyring of one public key made from `seed`: a version 4 key packet in the new format.
fn keyring(seed: u8) -> Vec<u8> {
    let mut body = vec![4, 0, 0, 0, 1, 22];
    body.extend([seed; 8]);
    let mut packet = vec![0xc0 | 6, u8::try_from(body.len()).expect("short")];
    packet.extend(body);
    packet
}

fn print_of(seed: u8) -> String {
    fingerprints_of(&keyring(seed), "keyring").expect("a keyring")[0].clone()
}

/// The descriptor of a third-party repository that holds all three formats, signed by `seed`.
fn descriptor(seed: u8) -> Value {
    json!({
        "version": 1,
        "name": "acme",
        "description": "acme packages",
        "channels": ["testing", "stable"],
        "key": {"keyring": "apt/acme.gpg", "fingerprints": [print_of(seed)]},
        "formats": {
            "apt": {"path": "apt", "components": ["main"], "architectures": ["amd64"]},
            "rpm": {"path": "rpm", "architectures": ["x86_64"]},
            "macos-pkg": {"path": "pkg", "platforms": ["macos-arm64"]}
        }
    })
}

/// A complete tree under `dir` for the descriptor `descriptor`, whose keyring is `ring`.
fn tree(dir: &Path, descriptor: &Value, ring: &[u8]) {
    let put = |path: &str, bytes: &[u8]| {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("dirs");
        std::fs::write(file, bytes).expect("write");
    };
    put(
        "hostkeeper-repository.json",
        &serde_json::to_vec(descriptor).expect("json"),
    );
    put("apt/acme.gpg", ring);
    for channel in ["testing", "stable"] {
        put(&format!("apt/dists/{channel}/InRelease"), b"signed");
        std::fs::create_dir_all(dir.join(format!("rpm/{channel}"))).expect("rpm");
    }
    std::fs::create_dir_all(dir.join("pkg")).expect("pkg");
}

/// What a third-party publisher is told about `dir`.
fn told(dir: &Path) -> Result<Vec<String>, Error> {
    check_dir(dir)
}

fn refused(dir: &Path) -> Vec<String> {
    match told(dir) {
        Err(Error::Refused(problems)) => problems,
        other => panic!("not refused: {other:?}"),
    }
}

/// Proves: a tree that holds a valid descriptor, the keyring it names with the fingerprints it
/// claims, the `InRelease` of each channel, the rpm channels and the macOS directory passes, and
/// the one line says the channels, the platforms and the keys.
///
/// Catches: a conformance check that refuses a repository the coordinator would take.
#[test]
fn a_complete_tree_passes_and_says_what_it_serves() {
    let d = scratch("complete");
    tree(&d, &descriptor(1), &keyring(1));
    assert_eq!(
        told(&d).expect("valid"),
        [format!(
            "{}/hostkeeper-repository.json: valid, 2 channels, platforms linux-x86_64 macos-arm64, keys {}",
            d.display(),
            print_of(1)
        )]
    );
}

/// Proves: a tree with no rpm and no macOS format needs neither directory; the formats a
/// descriptor does not list are not asked for.
///
/// Catches: a check that demands a format the descriptor never promised.
#[test]
fn a_tree_needs_only_the_formats_its_descriptor_lists() {
    let d = scratch("apt-only");
    let mut only = descriptor(1);
    only["formats"] = json!({"apt": {"path": "apt", "components": ["main"]}});
    tree(&d, &only, &keyring(1));
    std::fs::remove_dir_all(d.join("rpm")).expect("rm");
    std::fs::remove_dir_all(d.join("pkg")).expect("rm");
    assert!(told(&d).expect("valid")[0].contains("platforms linux-x86_64 linux-arm64"));
}

/// Proves: every promise of the descriptor the tree does not keep is listed, one line each: the
/// `InRelease` of a channel, the rpm directory of a channel, and the macOS directory.
///
/// Catches: a repository that passes while a suite a host will ask for is not there.
#[test]
fn every_promise_the_tree_breaks_is_listed() {
    let d = scratch("incomplete");
    tree(&d, &descriptor(1), &keyring(1));
    std::fs::remove_file(d.join("apt/dists/stable/InRelease")).expect("rm");
    std::fs::remove_dir(d.join("rpm/testing")).expect("rm");
    std::fs::remove_dir(d.join("pkg")).expect("rm");
    assert_eq!(
        refused(&d),
        [
            "rpm/testing is not a directory: the rpm channel testing is not there",
            "apt/dists/stable/InRelease is not a file: the apt suite stable is not there",
            "pkg is not a directory: the macOS installers are not there",
        ]
    );
}

/// Proves: a descriptor that claims a fingerprint the keyring lacks is refused naming it, and a
/// keyring that is no keyring, one over the bound and one that is not in the tree fail with their
/// own words.
///
/// Catches: a descriptor whose key is not the keyring's, which the coordinator refuses
/// `descriptor_invalid`.
#[test]
fn the_keyring_must_exist_be_a_keyring_and_hold_the_claimed_fingerprints() {
    let d = scratch("keyring");
    tree(&d, &descriptor(1), &keyring(2));
    assert_eq!(
        refused(&d),
        [format!(
            "the descriptor claims the fingerprint {}, which no key of apt/acme.gpg has",
            print_of(1)
        )]
    );
    std::fs::write(d.join("apt/acme.gpg"), b"junk").expect("write");
    assert!(
        told(&d)
            .expect_err("not a keyring")
            .to_string()
            .starts_with("apt/acme.gpg is not a keyring of public keys")
    );
    std::fs::write(d.join("apt/acme.gpg"), vec![0; MAX_KEYRING + 1]).expect("write");
    assert_eq!(
        told(&d).expect_err("too large").to_string(),
        format!(
            "apt/acme.gpg is {} bytes, at most {MAX_KEYRING}",
            MAX_KEYRING + 1
        )
    );
    std::fs::remove_file(d.join("apt/acme.gpg")).expect("rm");
    assert!(
        told(&d)
            .expect_err("missing")
            .to_string()
            .contains("acme.gpg: ")
    );
}

/// Proves: a directory with no descriptor, a descriptor the coordinator would refuse, and a path
/// that is no directory each fail naming what is wrong.
///
/// Catches: a check that passes a tree no operator could add.
#[test]
fn a_tree_without_a_valid_descriptor_fails() {
    let d = scratch("descriptor");
    assert!(
        told(&d)
            .expect_err("no descriptor")
            .to_string()
            .contains("hostkeeper-repository.json: ")
    );
    std::fs::write(d.join("hostkeeper-repository.json"), r#"{"version": 2}"#).expect("write");
    let why = told(&d).expect_err("newer").to_string();
    assert!(why.starts_with(&d.display().to_string()), "{why}");
    assert!(why.contains("newer than this coordinator reads"), "{why}");
    let nowhere = d.join("absent");
    assert_eq!(
        told(&nowhere).expect_err("no directory"),
        Error::Usage(format!(
            "{} is neither an https URL nor a directory",
            nowhere.display()
        ))
    );
}

/// A host that serves `files` by URL.
#[derive(Default)]
struct Served {
    files: RefCell<BTreeMap<String, Vec<u8>>>,
}

impl Host for Served {
    fn run(&self, _program: &Path, args: &[String]) -> Result<Answer, Error> {
        let url = args.last().expect("a url");
        let at = args.iter().position(|a| a == "--output").expect("--output");
        let found = self.files.borrow().get(url).cloned();
        if let Some(bytes) = &found {
            std::fs::write(&args[at + 1], bytes).expect("write");
        }
        Ok(Answer {
            ok: found.is_some(),
            stdout: String::new(),
            stderr: "curl: (22) 404".to_owned(),
        })
    }
}

fn check_args(base: &str, work: Option<&Path>) -> Verb {
    Verb::Check {
        base: base.to_owned(),
        work: work.map(Path::to_path_buf),
        curl: PathBuf::from("curl"),
    }
}

/// Proves: a URL is checked as the host serves it, in the downloads directory the publisher
/// names, and is refused with a usage error when none is named; a directory ignores the downloads
/// directory.
///
/// Catches: a URL check that reads nothing from the host, or one that writes where it was not
/// told to.
#[test]
fn a_url_is_checked_on_the_host_and_needs_a_directory_for_the_downloads() {
    let d = scratch("url");
    let host = Served::default();
    host.files.borrow_mut().insert(
        "https://acme.example.com/hostkeeper-repository.json".to_owned(),
        serde_json::to_vec(&descriptor(1)).expect("json"),
    );
    host.files.borrow_mut().insert(
        "https://acme.example.com/apt/acme.gpg".to_owned(),
        keyring(1),
    );
    let work = d.join("downloads");
    let line = run(check_args("https://acme.example.com", Some(&work)), &host).expect("valid");
    assert!(
        line[0]
            .starts_with("https://acme.example.com/hostkeeper-repository.json: valid, 2 channels"),
        "{line:?}"
    );
    assert!(work.join("descriptor.json").is_file());
    assert!(matches!(
        run(check_args("https://acme.example.com", None), &host),
        Err(Error::Usage(_))
    ));
    let empty = Served::default();
    assert!(
        run(check_args("https://acme.example.com", Some(&work)), &empty)
            .expect_err("not served")
            .to_string()
            .contains("could not be downloaded")
    );
    tree(&d, &descriptor(1), &keyring(1));
    assert!(run(check_args(&d.display().to_string(), None), &empty).is_ok());
}
