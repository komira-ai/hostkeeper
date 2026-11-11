//! The keyring an entry names (packages-repositories.md section 3.1): the loader reads it once,
//! up to `MAX_KEYRING_BYTES`, refuses one that is not a binary OpenPGP keyring (empty, or armored
//! text), and `load_with_keyrings` gives the bytes it judged.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_install::repository::config::{
    FileFault, MAX_KEYRING_BYTES, RepositoriesError, load, load_as, load_with_keyrings,
};
use hostkeeper_testkit::Scratch;

const PLACED: &str = "/etc/hostkeeper/repositories";

/// A binary keyring's first bytes: an OpenPGP public-key packet header, then a zero and a high
/// byte.
const KEYRING: [u8; 6] = [0x99, 0x01, 0x0d, 0x04, 0x00, 0xff];

/// The first line of an armored keyring, which apt does not take from a `.gpg` path.
const ARMORED: &[u8] = b"-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQENBF\n";

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("the mode");
    path
}

/// A directory (mode 0755) holding `a.sources`, whose `Signed-By` is `<signed_dir>/k.gpg`, and
/// the keyring `k.gpg` holding `keyring`.
fn dir_with(name: &str, signed_dir: &str, keyring: &[u8]) -> (Scratch, PathBuf) {
    let scratch =
        Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory");
    let dir = scratch.path().join("repositories");
    fs::create_dir(&dir).expect("the directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("the mode");
    let signed_dir = signed_dir.replace("@DIR@", &dir.display().to_string());
    let text = format!(
        "Types: deb\nURIs: https://repo.example.com/debian\nSuites: stable\n\
         Components: main\nSigned-By: {signed_dir}/k.gpg\n"
    );
    put(&dir, "a.sources", text.as_bytes());
    put(&dir, "k.gpg", keyring);
    (scratch, dir)
}

/// Proves an armored keyring, an empty one and a text file are refused, naming the entry, with
/// the text that says apt takes a binary keyring, by `load` and by `load_as` alike.
///
/// Catches: a start that accepts a keyring apt cannot read, so every `apt-get update` of the
/// entry fails with no public key.
#[test]
fn a_keyring_that_is_armored_empty_or_text_is_refused_naming_the_entry() {
    for (index, bytes) in [ARMORED, b"", b"keyring"].into_iter().enumerate() {
        let (_scratch, dir) = dir_with(&format!("keyring-text-{index}"), "@DIR@", bytes);
        let error = load(&dir, me()).expect_err("the keyring is refused");
        let sources = dir.join("a.sources");
        assert_eq!(
            error,
            RepositoriesError::Keyring {
                path: sources.clone(),
                fault: FileFault::NotBinaryKeyring,
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "{}: the keyring it names is not a binary OpenPGP keyring (it is empty or armored \
                 text); apt reads armor only from a keyring named .asc",
                sources.display()
            )
        );
        let (_placed_scratch, placed) = dir_with(&format!("keyring-text-p{index}"), PLACED, bytes);
        assert_eq!(
            load_as(&placed, Path::new(PLACED), me()).expect_err("refused"),
            RepositoriesError::Keyring {
                path: placed.join("a.sources"),
                fault: FileFault::NotBinaryKeyring,
            }
        );
    }
}

/// Proves the keyring's bound: one of exactly `MAX_KEYRING_BYTES` loads, one byte more is
/// refused naming the entry and the bound.
///
/// Catches: an unbounded read of a file the operator mistook for a keyring, or a bound that
/// refuses a keyring of its full size.
#[test]
fn a_keyring_loads_up_to_the_bound_and_is_refused_over_it() {
    let full = usize::try_from(MAX_KEYRING_BYTES).expect("the bound fits");
    let mut bytes = KEYRING.to_vec();
    bytes.resize(full, 0x2a);
    let (_scratch, dir) = dir_with("keyring-full", "@DIR@", &bytes);
    load(&dir, me()).expect("a keyring of the bound loads");
    bytes.push(0x2a);
    put(&dir, "k.gpg", &bytes);
    let error = load(&dir, me()).expect_err("one byte over is refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{}: the keyring it names is over 1048576 bytes",
            dir.join("a.sources").display()
        )
    );
}

/// Proves `load_with_keyrings` gives the entries `load_as` gives and the bytes of each keyring,
/// by the path the entries name it by (under the placed path); two
/// entries sharing one keyring give it once. A missing directory gives no entry and no keyring.
///
/// Catches: keyring bytes other than those judged, or keyed by the staging path.
#[test]
fn load_with_keyrings_gives_each_keyrings_bytes_by_its_placed_path() {
    let (scratch, dir) = dir_with("keyring-digest", PLACED, &KEYRING);
    let text = fs::read(dir.join("a.sources")).expect("the entry");
    put(&dir, "b.sources", &text);
    let loaded = load_with_keyrings(&dir, Path::new(PLACED), me()).expect("loads");
    assert_eq!(
        loaded.repositories,
        load_as(&dir, Path::new(PLACED), me()).expect("loads")
    );
    assert_eq!(loaded.repositories.names(), ["a", "b"]);
    assert_eq!(
        loaded.keyrings.into_iter().collect::<Vec<_>>(),
        [(Path::new(PLACED).join("k.gpg"), KEYRING.to_vec())]
    );
    let none = load_with_keyrings(&scratch.path().join("none"), Path::new(PLACED), me())
        .expect("a missing directory is no entry");
    assert!(none.repositories.is_empty());
    assert!(none.keyrings.is_empty());
}

/// Proves a keyring named `.asc` is judged as apt reads it, as armor: an armored one loads, and
/// a binary one or an empty one is refused naming the entry; the armored text that loads at
/// `.asc` is refused at `.gpg` (the first test).
///
/// Catches: a host whose working `.asc` keyring stops its daemon, or a `.asc` file apt cannot
/// read accepted.
#[test]
fn a_keyring_named_asc_must_be_armored() {
    let scratch =
        Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "keyring-asc").expect("scratch");
    let dir = scratch.path().join("repositories");
    fs::create_dir(&dir).expect("the directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("the mode");
    let text = format!(
        "Types: deb\nURIs: https://repo.example.com/debian\nSuites: stable\n\
         Components: main\nSigned-By: {}/k.asc\n",
        dir.display()
    );
    let sources = put(&dir, "a.sources", text.as_bytes());
    put(&dir, "k.asc", ARMORED);
    let loaded = load_with_keyrings(&dir, &dir, me()).expect("an armored .asc loads");
    assert_eq!(
        loaded.keyrings.into_iter().collect::<Vec<_>>(),
        [(dir.join("k.asc"), ARMORED.to_vec())]
    );
    for bytes in [&KEYRING[..], b""] {
        put(&dir, "k.asc", bytes);
        let error = load(&dir, me()).expect_err("not armor at .asc");
        assert_eq!(
            error,
            RepositoriesError::Keyring {
                path: sources.clone(),
                fault: FileFault::NotArmoredKeyring,
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "{}: the keyring it names is named .asc but does not begin \
                 \"-----BEGIN PGP PUBLIC KEY BLOCK-----\"; apt reads a .asc keyring as armor",
                sources.display()
            )
        );
    }
}
