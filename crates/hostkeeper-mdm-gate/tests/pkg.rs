//! The pkg allowlist (design 7.1): every rule `PkgAllowlist::parse` checks, the file rule of
//! `PkgAllowlist::load` (root's alone), the newest entry, and the MDM host's name.

mod support;

use std::path::Path;
use std::sync::Mutex;

use hostkeeper_enrollment::fs::{FileLock, FileStat, Fs, RealFs};
use hostkeeper_mdm_gate::{MdmHost, PkgAllowlist, PkgAllowlistError, PkgDigest};
use support::scratch;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn entry(sha256: &str, version: &str, path: &str) -> String {
    format!("[[pkg]]\nsha256 = \"{sha256}\"\nversion = \"{version}\"\npath = \"{path}\"\n")
}

fn at(sha256: &str) -> String {
    format!("/var/lib/hostkeeper-mdm-pkg/{sha256}.pkg")
}

fn refusal(text: &str) -> String {
    PkgAllowlist::parse(text).expect_err("refused").to_string()
}

/// Catches: an allowlist accepted with no pkg, a malformed digest (upper case, short, not
/// hex) or version, a path that is relative or not named for its digest (the URL would name a
/// digest the file is not), a digest listed twice, two versions of one release (no newest),
/// or text that is not TOML of entries; each refused with a text naming the flag and the rule.
#[test]
fn the_allowlist_refuses_each_broken_rule() {
    let cases = [
        (String::new(), "the allowlist lists no pkg ([[pkg]])"),
        (
            entry(&A.to_uppercase(), "0.1.66", &at(A)),
            &*format!(
                "sha256 \"{}\" is not 64 lower-case hex digits",
                A.to_uppercase()
            ),
        ),
        (
            entry(&A[1..], "0.1.66", &at(A)),
            &*format!("sha256 \"{}\" is not 64 lower-case hex digits", &A[1..]),
        ),
        (
            entry(&"g".repeat(64), "0.1.66", &at(A)),
            &*format!(
                "sha256 \"{}\" is not 64 lower-case hex digits",
                "g".repeat(64)
            ),
        ),
        (
            entry(A, "v0.1.66", &at(A)),
            "version \"v0.1.66\": the version must start with a digit",
        ),
        (
            entry(A, "0.1.66", &format!("{A}.pkg")),
            &*format!(
                "the path of {A} is not an absolute path to {A}.pkg, the name the kit's proxy serves"
            ),
        ),
        (
            entry(A, "0.1.66", &at(B)),
            &*format!(
                "the path of {A} is not an absolute path to {A}.pkg, the name the kit's proxy serves"
            ),
        ),
        (
            format!(
                "{}{}",
                entry(A, "0.1.66", &at(A)),
                entry(A, "0.1.67", &at(A))
            ),
            &*format!("sha256 {A} is listed twice"),
        ),
        (
            format!(
                "{}{}",
                entry(A, "0.1.66", &at(A)),
                entry(B, "0.1.66+1", &at(B))
            ),
            "versions 0.1.66 and 0.1.66+1 are the same release, so neither is the newest",
        ),
    ];
    for (text, why) in cases {
        assert_eq!(
            refusal(&text),
            format!("--mdm-pkg-allowlist: {why}"),
            "{text}"
        );
    }
    let not_entries = refusal("pkg = 3");
    assert!(
        not_entries.starts_with("--mdm-pkg-allowlist: invalid type: integer `3`"),
        "{not_entries}"
    );
    let missing_key = entry(A, "0.1.66", &at(A)).replace("version", "versio");
    assert!(
        refusal(&missing_key).starts_with("--mdm-pkg-allowlist: missing field `version`"),
        "{}",
        refusal(&missing_key)
    );
}

/// Catches: the gate sending another entry than the newest by release numbers (the file's
/// order, or the text's order of versions, `0.1.9` after `0.1.66`); a key a later release
/// adds refused by this one.
#[test]
fn the_newest_entry_is_sent_and_an_unknown_key_is_passed_over() {
    let text = format!(
        "{}{}{}",
        entry(A, "0.1.9", &at(A)),
        entry(B, "0.1.66", &at(B)),
        "release_sha256 = \"not read by this release\"\n"
    );
    let list = PkgAllowlist::parse(&text).expect("parses");
    assert_eq!(list.newest().sha256.as_str(), B);
    assert_eq!(list.newest().version.as_str(), "0.1.66");
    let order: Vec<&str> = list
        .entries()
        .iter()
        .map(|entry| entry.sha256.as_str())
        .collect();
    assert_eq!(order, [B, A]);

    // What the coordinator's start shows of the install: the list and the host, not the file
    // system.
    let install = hostkeeper_mdm_gate::Install::new(
        PkgAllowlist::parse(&entry(A, "0.1.66", &at(A)))
            .expect("parses")
            .installs(),
        MdmHost::parse("mdm.example.com").expect("a name"),
        std::sync::Arc::new(RealFs),
    );
    assert_eq!(
        format!("{install:?}"),
        format!(
            "Install {{ installs: Installs([InstallEntry {{ name: \"hostkeeper\", bundle_id: \"dev.hostkeeper.hostkeeper\", sha256: PkgDigest(\"{A}\"), version: Version(\"0.1.66\"), path: \"{}\", url: None }}]), host: MdmHost(\"mdm.example.com\"), .. }}",
            at(A)
        )
    );
}

/// A file system whose `read_private` gives the bytes of the real file with the stat a test
/// sets: a file of root's cannot be made without root.
struct StatFs(Mutex<FileStat>);

impl StatFs {
    fn new(uid: u32, mode: u32, is_file: bool) -> Self {
        Self(Mutex::new(FileStat { is_file, uid, mode }))
    }
}

impl Fs for StatFs {
    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        RealFs.read(path)
    }
    fn read_private(&self, path: &Path) -> std::io::Result<Option<(Vec<u8>, FileStat)>> {
        let stat = *self.0.lock().expect("stat");
        Ok(RealFs.read_private(path)?.map(|(bytes, _)| (bytes, stat)))
    }
    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
        RealFs.write_new(path, bytes, mode)
    }
    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
        RealFs.sync_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        RealFs.rename(from, to)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        RealFs.remove(path)
    }
    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        RealFs.sync_dir(dir)
    }
    fn create_dir(&self, dir: &Path) -> std::io::Result<()> {
        RealFs.create_dir(dir)
    }
    fn list(&self, dir: &Path) -> std::io::Result<Vec<String>> {
        RealFs.list(dir)
    }
    fn lock(&self, path: &Path) -> std::io::Result<FileLock> {
        RealFs.lock(path)
    }
}

/// Catches: an allowlist read that anyone but root could have written (another owner, a
/// group- or world-writable mode), that is no regular file, or that is missing, taken as the
/// list of what may be installed as root on every Mac; a file that is not UTF-8 read; and a
/// good file refused.
#[test]
fn the_allowlist_file_must_be_roots_alone() {
    let scratch = scratch("pkg-allowlist-file");
    let path = scratch.path().join("pkgs.toml");
    std::fs::write(&path, entry(A, "0.1.66", &at(A))).expect("written");
    let shown = path.display();
    let untrusted = |why: &str| {
        format!(
            "--mdm-pkg-allowlist {shown}: {why}; the pkg allowlist must be a file of root's that no one else can write"
        )
    };
    let load = |fs: &StatFs| PkgAllowlist::load(fs, &path);

    let good = load(&StatFs::new(0, 0o644, true)).expect("root's, 0644");
    assert_eq!(good.newest().sha256.as_str(), A);
    assert!(load(&StatFs::new(0, 0o600, true)).is_ok());
    let cases = [
        (StatFs::new(501, 0o644, true), "it is not owned by root"),
        (
            StatFs::new(0, 0o664, true),
            "its group or others can write it",
        ),
        (
            StatFs::new(0, 0o646, true),
            "its group or others can write it",
        ),
        (StatFs::new(0, 0o644, false), "it is not a regular file"),
    ];
    for (fs, why) in cases {
        let error = load(&fs).expect_err("refused");
        assert!(matches!(error, PkgAllowlistError::Untrusted { .. }));
        assert_eq!(error.to_string(), untrusted(why));
    }

    // The real owner is the test's user, not root.
    let error = PkgAllowlist::load(&RealFs, &path).expect_err("not root's");
    assert_eq!(error.to_string(), untrusted("it is not owned by root"));

    let missing = scratch.path().join("missing.toml");
    let error = PkgAllowlist::load(&StatFs::new(0, 0o644, true), &missing).expect_err("missing");
    assert!(matches!(error, PkgAllowlistError::Read { .. }));
    assert!(
        error
            .to_string()
            .starts_with(&format!("--mdm-pkg-allowlist {}: ", missing.display())),
        "{error}"
    );

    std::fs::write(&path, [0xff, 0xfe]).expect("written");
    let error = load(&StatFs::new(0, 0o644, true)).expect_err("not UTF-8");
    assert_eq!(
        error.to_string(),
        "--mdm-pkg-allowlist: the file is not UTF-8"
    );
}

/// Catches: an MDM host name with a scheme, a port, a path, an empty or over-long label, a
/// label starting or ending with `-`, a single label, or over 253 characters, accepted into the
/// URL the Mac fetches; a name not kept in lower case; a digest of upper case accepted.
#[test]
fn the_mdm_host_is_a_dns_name_alone() {
    let digest = PkgDigest::parse(A).expect("a digest");
    let host = MdmHost::parse("MDM.Example.com").expect("a name");
    assert_eq!(
        host.pkg_url(&digest),
        format!("https://mdm.example.com/pkg/{A}.pkg")
    );
    let long = format!("{}.com", "a".repeat(250));
    let refused = [
        "https://mdm.example.com",
        "mdm.example.com:443",
        "mdm.example.com/pkg",
        "localhost",
        "mdm..example.com",
        "-mdm.example.com",
        "mdm-.example.com",
        &format!("{}.com", "a".repeat(64)),
        &long,
    ];
    for text in refused {
        let error = MdmHost::parse(text).expect_err("refused");
        let shown: String = text.chars().take(253).collect();
        assert_eq!(
            error.to_string(),
            format!(
                "--mdm-host: {shown:?} is not a DNS name (the MDM host's name alone, with no scheme, port or path)"
            )
        );
    }
    assert!(PkgDigest::parse(&A.to_uppercase()).is_none());
    assert_eq!(digest.to_string(), A);
}
