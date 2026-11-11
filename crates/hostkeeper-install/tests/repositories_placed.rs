//! `load_as` (packages-repositories.md section 3.2): the rules of `load` over a directory the
//! host will see at another path, so `pxe serve` can check the operator's directory
//! before it writes the files to `/etc/hostkeeper/repositories/` on an installed host.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_install::repository::config::{FileFault, RepositoriesError, Rule, load, load_as};
use hostkeeper_testkit::Scratch;

const PLACED: &str = "/etc/hostkeeper/repositories";

/// A keyring's bytes as the loader takes them: binary, its first byte an OpenPGP packet header
/// (bit 7 set). The loader judges no more of it than that byte.
const KEYRING: [u8; 7] = [0x99, 0x01, 0x0d, 0x04, 0x00, 0xff, 0x0a];

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn put(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("the mode");
    path
}

/// A source directory (mode 0755) holding the keyring `k.gpg`.
fn source(name: &str) -> (Scratch, PathBuf) {
    let scratch =
        Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory");
    let dir = scratch.path().join("src");
    fs::create_dir(&dir).expect("the directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("the mode");
    put(&dir, "k.gpg", &KEYRING, 0o644);
    (scratch, dir)
}

fn stanza(signed_by: &str) -> String {
    format!(
        "Types: deb\nURIs: https://repo.example.com/debian\nSuites: stable\n\
         Components: main\nSigned-By: {signed_by}\n"
    )
}

/// Proves a directory whose stanzas name the placed path loads, with each entry's keyring under
/// the placed path and its text as written.
#[test]
fn a_directory_whose_stanzas_name_the_placed_path_loads() {
    let (_scratch, dir) = source("placed-ok");
    let text = stanza(&format!("{PLACED}/k.gpg"));
    put(&dir, "a.sources", text.as_bytes(), 0o644);
    let loaded = load_as(&dir, Path::new(PLACED), me()).expect("the directory loads");
    let entry = loaded.get("a").expect("the entry");
    assert_eq!(entry.keyring, Path::new(PLACED).join("k.gpg"));
    assert_eq!(entry.text, text);
}

/// Proves `load` is `load_as` of a directory placed where it is.
#[test]
fn load_is_load_as_at_the_same_path() {
    let (_scratch, dir) = source("placed-same");
    put(
        &dir,
        "a.sources",
        stanza(&format!("{}/k.gpg", dir.display())).as_bytes(),
        0o644,
    );
    assert_eq!(
        load(&dir, me()).expect("loads"),
        load_as(&dir, &dir, me()).expect("loads")
    );
}

/// Proves a Signed-By naming the source directory, or another directory, is refused naming the
/// file in the source directory.
#[test]
fn a_signed_by_outside_the_placed_directory_is_refused() {
    for signed_by in [
        |dir: &Path| format!("{}/k.gpg", dir.display()),
        |_: &Path| "/etc/other/k.gpg".to_owned(),
    ] {
        let (_scratch, dir) = source("placed-outside");
        let value = signed_by(&dir);
        put(&dir, "a.sources", stanza(&value).as_bytes(), 0o644);
        let error = load_as(&dir, Path::new(PLACED), me()).expect_err("refused");
        assert_eq!(
            error,
            RepositoriesError::Stanza {
                path: dir.join("a.sources"),
                rule: Rule::SignedBy(value),
            }
        );
    }
}

/// Proves a file no Signed-By names is refused as stray, naming the file in the source directory.
#[test]
fn a_stray_file_is_refused_naming_the_source_file() {
    let (_scratch, dir) = source("placed-stray");
    put(
        &dir,
        "a.sources",
        stanza(&format!("{PLACED}/k.gpg")).as_bytes(),
        0o644,
    );
    put(&dir, "extra.txt", b"x", 0o644);
    assert_eq!(
        load_as(&dir, Path::new(PLACED), me()).expect_err("refused"),
        RepositoriesError::Stray {
            path: dir.join("extra.txt")
        }
    );
}

/// Proves a keyring the stanza names but the source directory lacks is refused, naming the entry's
/// file in the source directory (the placed path does not exist here and is never read).
#[test]
fn a_keyring_missing_from_the_source_directory_is_refused() {
    let (_scratch, dir) = source("placed-missing");
    fs::remove_file(dir.join("k.gpg")).expect("the keyring goes");
    put(
        &dir,
        "a.sources",
        stanza(&format!("{PLACED}/gone.gpg")).as_bytes(),
        0o644,
    );
    let error = load_as(&dir, Path::new(PLACED), me()).expect_err("refused");
    match error {
        RepositoriesError::Keyring { path, fault } => {
            assert_eq!(path, dir.join("a.sources"));
            assert!(matches!(fault, FileFault::Unreadable(_)), "{fault:?}");
        }
        other => panic!("a keyring refusal was expected, not {other:?}"),
    }
}

/// Proves `read_file` returns a file's bytes, and refuses a link, a file over the bound, a file
/// another user owns, a group-writable file and a missing one, each with its fault.
#[test]
fn read_file_returns_the_bytes_or_the_fault() {
    use hostkeeper_install::repository::config::read_file;
    let (_scratch, dir) = source("read-file");
    let path = dir.join("k.gpg");
    assert_eq!(read_file(&path, me(), 7).expect("read"), KEYRING);
    assert_eq!(read_file(&path, me(), 6), Err(FileFault::Over(6)));
    assert_eq!(
        read_file(&path, me() + 1, 7),
        Err(FileFault::WrongOwner {
            found: me(),
            expected: me() + 1
        })
    );
    let link = dir.join("link.gpg");
    std::os::unix::fs::symlink(&path, &link).expect("link");
    assert_eq!(read_file(&link, me(), 7), Err(FileFault::Link));
    let loose = put(&dir, "loose.gpg", b"x", 0o664);
    assert_eq!(
        read_file(&loose, me(), 7),
        Err(FileFault::Writable { found: 0o664 })
    );
    assert!(matches!(
        read_file(&dir.join("none"), me(), 7),
        Err(FileFault::Unreadable(_))
    ));
    assert_eq!(FileFault::Over(6).to_string(), "is over 6 bytes");
}
