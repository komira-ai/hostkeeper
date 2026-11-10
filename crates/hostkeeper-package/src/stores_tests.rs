//! Tests of `stores.toml`: what a good file gives, the URL of a blob in each kind, no file, and
//! every refusal.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use super::*;
use crate::testing::scratch;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The user that owns the scratch directories the tests make.
fn me(dir: &Path) -> u32 {
    fs::metadata(dir).expect("the directory").uid()
}

/// Writes `bytes` to `dir/name` with `mode`.
fn put(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("a file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("its mode");
    path
}

/// Loads `text` as the stores file of a fresh directory, as its owner.
fn load_text(dir: &Path, text: &str) -> Result<Stores, StoresError> {
    let path = put(dir, "stores.toml", text.as_bytes(), 0o644);
    load(&path, me(dir))
}

fn refused(store: &str, rule: Rule) -> StoresError {
    StoresError::Store {
        store: store.to_owned(),
        rule,
    }
}

/// Catches a host without a file getting a store: it has none, and no store of any name.
#[test]
fn no_file_gives_no_store() {
    let scratch = scratch("stores-none");
    let stores = load(&scratch.path().join("stores.toml"), 0).expect("no stores");
    assert_eq!(stores, Stores::default());
    assert!(stores.all().is_empty());
    assert!(stores.get("channel").is_none());
    assert_eq!(stores.mirrors().count(), 0);
}

/// Catches a stores file read wrongly: two stores keep file order, the https one defaults to no
/// redirect and no credential, the oci one to 3 redirects, a mirror is found by `mirrors`, a
/// lookup by name finds one store, and each kind makes its URL.
#[test]
fn a_good_file_gives_its_stores_in_order_and_their_urls() {
    let scratch = scratch("stores-good");
    let dir = scratch.path();
    put(dir, "lan.basic", b"user:secret\n", 0o600);
    put(dir, "gh.token", b"tok-123", 0o600);
    let text = format!(
        "[[store]]\nname = \"github\"\nkind = \"https\"\nbase = \"https://GitHub.com/\"\nredirects = 5\n\
         bearer_file = \"{d}/gh.token\"\n\
         [[store]]\nname = \"lan\"\nkind = \"oci\"\nbase = \"https://mirror.example:443/pre/\"\nmirror = true\n\
         repository = \"releases\"\nbasic_auth_file = \"{d}/lan.basic\"\n\
         [[store]]\nname = \"plain\"\nkind = \"https\"\nbase = \"https://plain.example/\"\n\
         [[store]]\nname = \"reg\"\nkind = \"oci\"\nbase = \"https://reg.example/\"\n",
        d = dir.display()
    );
    let stores = load_text(dir, &text).expect("stores");
    let names: Vec<&str> = stores.all().iter().map(Store::name).collect();
    assert_eq!(names, ["github", "lan", "plain", "reg"]);
    let mirrors: Vec<&str> = stores.mirrors().map(Store::name).collect();
    assert_eq!(mirrors, ["lan"]);
    assert!(stores.get("nope").is_none());

    let github = stores.get("github").expect("github");
    assert_eq!(github.kind(), Kind::Https);
    assert_eq!(github.redirects(), 5);
    assert_eq!(github.origin(), "https://github.com:443");
    assert_eq!(github.authorization(), Some("Bearer tok-123"));
    assert_eq!(
        github
            .blob_url("o/r/releases/download/v1/a.tar.zst", DIGEST)
            .as_str(),
        "https://GitHub.com/o/r/releases/download/v1/a.tar.zst"
    );
    assert!(github.mirror_url(DIGEST).is_none());

    let lan = stores.get("lan").expect("lan");
    assert_eq!(lan.kind(), Kind::Oci);
    assert_eq!(lan.redirects(), OCI_REDIRECTS);
    assert!(lan.is_mirror());
    assert_eq!(lan.repository(), Some("releases"));
    assert_eq!(lan.authorization(), Some("Basic dXNlcjpzZWNyZXQ="));
    assert_eq!(
        lan.mirror_url(DIGEST).expect("a mirror url").as_str(),
        format!("https://mirror.example:443/pre/v2/releases/blobs/sha256:{DIGEST}")
    );
    assert_eq!(
        lan.blob_url("team/app", DIGEST).as_str(),
        format!("https://mirror.example:443/pre/v2/team/app/blobs/sha256:{DIGEST}")
    );

    let plain = stores.get("plain").expect("plain");
    assert_eq!((plain.redirects(), plain.authorization()), (0, None));
    let reg = stores.get("reg").expect("reg");
    assert!(reg.mirror_url(DIGEST).is_none());
    assert_eq!(reg.redirects(), OCI_REDIRECTS);
}

/// Catches a credential that shows in `Debug` output: neither the secret nor the header built
/// from it appears in the output of a store or of the stores.
#[test]
fn credentials_never_appear_in_debug_output() {
    let scratch = scratch("stores-debug");
    let dir = scratch.path();
    put(dir, "lan.basic", b"user:hunter2\n", 0o600);
    let text = format!(
        "[[store]]\nname = \"lan\"\nkind = \"https\"\nbase = \"https://lan.example/\"\n\
         basic_auth_file = \"{}/lan.basic\"\n",
        dir.display()
    );
    let stores = load_text(dir, &text).expect("stores");
    let shown = format!("{stores:?} {:?}", stores.all()[0]);
    assert!(shown.contains("Secret(<redacted>)"), "{shown}");
    for secret in ["hunter2", "dXNlcjpodW50ZXIy", "Basic"] {
        assert!(!shown.contains(secret), "{shown}");
    }
}

/// Catches an empty file being refused, or giving a store of its own: it defines no store.
#[test]
fn an_empty_file_defines_no_store() {
    let scratch = scratch("stores-empty");
    let stores = load_text(scratch.path(), "").expect("no stores");
    assert!(stores.all().is_empty());
}

/// The store text of a valid `https` store named `a`, with `extra` lines after it.
fn https(extra: &str) -> String {
    format!("[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"https://a.example/\"\n{extra}\n")
}

/// The store text of a valid `oci` store named `a`, with `extra` lines after it.
fn oci(extra: &str) -> String {
    format!("[[store]]\nname = \"a\"\nkind = \"oci\"\nbase = \"https://a.example/\"\n{extra}\n")
}

/// Catches a store that breaks one rule being accepted, or refused for another rule or without
/// its name: each case names the store and the rule it breaks.
#[test]
fn every_store_that_breaks_a_rule_is_refused_naming_the_store_and_the_rule() {
    let long = "x".repeat(33);
    let cases: Vec<(&str, String, StoresError)> = vec![
        (
            "a name in capitals",
            "[[store]]\nname = \"Up\"\nkind = \"https\"\nbase = \"https://a.example/\"\n".into(),
            refused("Up", Rule::Name),
        ),
        (
            "an empty name",
            "[[store]]\nname = \"\"\nkind = \"https\"\nbase = \"https://a.example/\"\n".into(),
            refused("", Rule::Name),
        ),
        (
            "a name of 33 bytes",
            format!(
                "[[store]]\nname = \"{long}\"\nkind = \"https\"\nbase = \"https://a.example/\"\n"
            ),
            refused(&long, Rule::Name),
        ),
        (
            "a name with an underscore",
            "[[store]]\nname = \"a_b\"\nkind = \"https\"\nbase = \"https://a.example/\"\n".into(),
            refused("a_b", Rule::Name),
        ),
        (
            "two stores of one name",
            format!("{}{}", https(""), https("")),
            refused("a", Rule::DuplicateName),
        ),
        (
            "the kind of the coordinator",
            "[[store]]\nname = \"a\"\nkind = \"coordinator\"\nbase = \"https://a.example/\"\n"
                .into(),
            refused("a", Rule::Kind("coordinator".to_owned())),
        ),
        (
            "an unknown kind",
            "[[store]]\nname = \"a\"\nkind = \"ftp\"\nbase = \"https://a.example/\"\n".into(),
            refused("a", Rule::Kind("ftp".to_owned())),
        ),
        (
            "a base with http",
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"http://a.example/\"\n".into(),
            refused("a", Rule::Base),
        ),
        (
            "a base with another scheme",
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"ftp://a.example/\"\n".into(),
            refused("a", Rule::Base),
        ),
        (
            "a base with a user name",
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"https://u:p@a.example/\"\n".into(),
            refused("a", Rule::Base),
        ),
        (
            "a base with a query",
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"https://a.example/?x=1\"\n".into(),
            refused("a", Rule::Base),
        ),
        (
            "a base with a fragment",
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"https://a.example/#x\"\n".into(),
            refused("a", Rule::Base),
        ),
        (
            "a base without a closing slash",
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"https://a.example\"\n".into(),
            refused("a", Rule::Base),
        ),
        (
            "a base that is not a URL",
            "[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"not a url/\"\n".into(),
            refused("a", Rule::Base),
        ),
        (
            "redirects over 5 on an https store",
            https("redirects = 6"),
            refused("a", Rule::RedirectsOver(6)),
        ),
        (
            "redirects on an oci store",
            oci("redirects = 1"),
            refused("a", Rule::RedirectsOnOci),
        ),
        (
            "a mirror that is an https store",
            https("mirror = true"),
            refused("a", Rule::MirrorOnHttps),
        ),
        (
            "a repository on an https store",
            https("repository = \"r\""),
            refused("a", Rule::RepositoryOnHttps),
        ),
        (
            "an oci mirror with no repository",
            oci("mirror = true"),
            refused("a", Rule::MirrorWithoutRepository),
        ),
        (
            "an oci repository that is not a locator",
            oci("mirror = true\nrepository = \"a//b\""),
            refused("a", Rule::Repository),
        ),
        (
            "both credential keys",
            https("basic_auth_file = \"/x\"\nbearer_file = \"/y\""),
            refused("a", Rule::TwoCredentials),
        ),
    ];
    for (case, text, expected) in cases {
        let scratch = scratch("stores-rule");
        let refusal = load_text(scratch.path(), &text).expect_err(case);
        assert_eq!(refusal, expected, "{case}");
    }
}

/// Catches a file that is not a stores file being read as one: text that is not TOML, a key no
/// store has, a key of the wrong type, a negative count and bytes that are not text are all
/// `Syntax`.
#[test]
fn text_that_is_not_a_stores_file_is_a_syntax_error() {
    let cases = [
        ("not TOML", "this = = that".to_owned()),
        ("an unknown key of a store", https("colour = \"red\"")),
        ("an unknown top-level key", "stores = 3\n".to_owned()),
        (
            "a name that is not text",
            "[[store]]\nname = 3\n".to_owned(),
        ),
        (
            "a missing kind",
            "[[store]]\nname = \"a\"\nbase = \"https://a.example/\"\n".to_owned(),
        ),
        ("negative redirects", https("redirects = -1")),
    ];
    for (case, text) in cases {
        let scratch = scratch("stores-syntax");
        let refusal = load_text(scratch.path(), &text).expect_err(case);
        assert!(
            matches!(refusal, StoresError::Syntax(_)),
            "{case}: {refusal:?}"
        );
    }
    let scratch = scratch("stores-binary");
    let path = put(scratch.path(), "stores.toml", &[0xff, 0xfe, b'\n'], 0o644);
    let refusal = load(&path, me(scratch.path())).expect_err("binary");
    assert_eq!(refusal, StoresError::Syntax("not UTF-8 text".to_owned()));
}

/// Catches a credential file that is not what the design requires being read: each defect is
/// refused with the store, the file's path and the fault, and nothing of the content.
#[test]
fn every_credential_file_that_breaks_a_rule_is_refused_naming_the_file_and_the_fault() {
    type Case<'a> = (&'a str, &'a str, Option<(&'a [u8], u32)>, FileFault);
    let cases: Vec<Case<'_>> = vec![
        ("missing", "absent", None, FileFault::Missing),
        (
            "mode 0640",
            "cred",
            Some((b"user:secret\n", 0o640)),
            FileFault::NotPrivate { found: 0o640 },
        ),
        (
            "mode 0400",
            "cred",
            Some((b"user:secret\n", 0o400)),
            FileFault::NotPrivate { found: 0o400 },
        ),
        ("empty", "cred", Some((b"", 0o600)), FileFault::Empty),
        (
            "a newline only",
            "cred",
            Some((b"\n", 0o600)),
            FileFault::Empty,
        ),
        (
            "two lines",
            "cred",
            Some((b"user:secret\nmore\n", 0o600)),
            FileFault::NotOneLine,
        ),
        (
            "a carriage return",
            "cred",
            Some((b"user:secret\r\n", 0o600)),
            FileFault::NotOneLine,
        ),
        (
            "bytes that are not text",
            "cred",
            Some((&[0xff, 0xfe], 0o600)),
            FileFault::NotOneLine,
        ),
    ];
    for (case, name, content, fault) in cases {
        let scratch = scratch("stores-cred");
        let dir = scratch.path();
        if let Some((bytes, mode)) = content {
            put(dir, name, bytes, mode);
        }
        let path = dir.join(name);
        let text = https(&format!("bearer_file = \"{}\"", path.display()));
        let refusal = load_text(dir, &text).expect_err(case);
        assert_eq!(
            refusal,
            refused("a", Rule::Credential { path, fault }),
            "{case}"
        );
    }
}

/// Catches a credential that is a link, a directory, a path relative to the working directory,
/// longer than the cap or owned by another user being read: a link is never followed.
#[test]
fn a_credential_that_is_a_link_a_directory_relative_large_or_foreign_is_refused() {
    let scratch = scratch("stores-cred-kinds");
    let dir = scratch.path();
    put(dir, "real", b"user:secret\n", 0o600);
    symlink(dir.join("real"), dir.join("link")).expect("a link");
    fs::create_dir(dir.join("folder")).expect("a directory");
    put(dir, "big", &vec![b'x'; 4097], 0o600);
    let faults = [
        ("link", FileFault::Link),
        ("folder", FileFault::NotRegular),
        ("big", FileFault::TooLarge(MAX_CREDENTIAL_BYTES)),
    ];
    for (name, fault) in faults {
        let path = dir.join(name);
        let text = https(&format!("basic_auth_file = \"{}\"", path.display()));
        let refusal = load_text(dir, &text).expect_err(name);
        assert_eq!(
            refusal,
            refused("a", Rule::Credential { path, fault }),
            "{name}"
        );
    }
    let text = https("bearer_file = \"relative/token\"");
    let refusal = load_text(dir, &text).expect_err("relative");
    assert_eq!(
        refusal,
        refused(
            "a",
            Rule::Credential {
                path: PathBuf::from("relative/token"),
                fault: FileFault::NotAbsolute
            }
        )
    );
    let me = me(dir);
    let fault = credential_line(&dir.join("real"), me + 1).expect_err("another owner");
    assert_eq!(
        fault,
        FileFault::WrongOwner {
            found: me,
            expected: me + 1
        }
    );
    assert_eq!(
        credential_line(&dir.join("real"), me).as_deref(),
        Ok("user:secret")
    );
}

/// Catches the stores file itself being trusted when it should not be: a link, a directory, a
/// file group- or other-writable, another owner's, over 64 KiB, or under a path that is not a
/// directory is refused with its path; a file that only others can read is fine.
#[test]
fn a_stores_file_that_is_unsafe_or_unreadable_is_refused() {
    let scratch = scratch("stores-file");
    let dir = scratch.path();
    let owner = me(dir);
    let body = https("");
    let real = put(dir, "real.toml", body.as_bytes(), 0o644);
    symlink(&real, dir.join("link.toml")).expect("a link");
    fs::create_dir(dir.join("dir.toml")).expect("a directory");
    put(dir, "group.toml", body.as_bytes(), 0o664);
    put(dir, "other.toml", body.as_bytes(), 0o646);
    put(dir, "tight.toml", body.as_bytes(), 0o600);
    put(dir, "big.toml", &vec![b'#'; 65537], 0o644);
    put(dir, "plain", b"x", 0o644);
    let at = |name: &str| dir.join(name);
    let refuse = |name: &str, fault: FileFault| StoresError::File {
        path: at(name),
        fault,
    };
    let check = |name: &str, uid: u32| load(&at(name), uid);
    assert_eq!(
        check("link.toml", owner),
        Err(refuse("link.toml", FileFault::Link))
    );
    assert_eq!(
        check("dir.toml", owner),
        Err(refuse("dir.toml", FileFault::NotRegular))
    );
    assert_eq!(
        check("group.toml", owner),
        Err(refuse(
            "group.toml",
            FileFault::WritableByOthers { found: 0o664 }
        ))
    );
    assert_eq!(
        check("other.toml", owner),
        Err(refuse(
            "other.toml",
            FileFault::WritableByOthers { found: 0o646 }
        ))
    );
    assert_eq!(
        check("real.toml", owner + 1),
        Err(refuse(
            "real.toml",
            FileFault::WrongOwner {
                found: owner,
                expected: owner + 1
            }
        ))
    );
    assert_eq!(
        check("big.toml", owner),
        Err(refuse("big.toml", FileFault::TooLarge(MAX_STORES_BYTES)))
    );
    assert!(matches!(
        check("plain/stores.toml", owner),
        Err(StoresError::File {
            fault: FileFault::Unreadable(_),
            ..
        })
    ));
    assert!(check("real.toml", owner).is_ok());
    assert!(check("tight.toml", owner).is_ok());
}

/// Catches an error message that hides which store broke which rule, or that carries a secret:
/// the full text of one error of each shape.
#[test]
fn error_messages_name_the_store_the_rule_and_the_file() {
    let store = refused("lan", Rule::Kind("ftp".to_owned()));
    assert_eq!(
        store.to_string(),
        "store \"lan\": its kind \"ftp\" is not https or oci"
    );
    let credential = refused(
        "lan",
        Rule::Credential {
            path: PathBuf::from("/etc/hostkeeper/stores/lan.basic"),
            fault: FileFault::NotPrivate { found: 0o644 },
        },
    );
    assert_eq!(
        credential.to_string(),
        "store \"lan\": its credential file /etc/hostkeeper/stores/lan.basic: has mode 0644, not 0600"
    );
    let file = StoresError::File {
        path: PathBuf::from("/etc/hostkeeper/stores.toml"),
        fault: FileFault::WritableByOthers { found: 0o666 },
    };
    assert_eq!(
        file.to_string(),
        "/etc/hostkeeper/stores.toml: the file has mode 0666, which group or others can write"
    );
}

/// Every host has the `coordinator` store, with or without a stores file, and a file does not list
/// it among the stores it defines: its blob URL is `/` and the locator, it has no credential, no
/// redirect and no mirror.
///
/// Catches a host without the store, a file that has to define it, and a URL with a base.
#[test]
fn the_coordinator_store_is_there_with_and_without_a_stores_file() {
    let scratch = scratch("stores-coordinator");
    let none = load(&scratch.path().join("stores.toml"), 0).expect("no file");
    let file = load_text(scratch.path(), &https("")).expect("a file");
    for stores in [&none, &file] {
        let store = stores
            .get(COORDINATOR_STORE)
            .expect("the coordinator store");
        assert_eq!(store.name(), "coordinator");
        assert_eq!(store.kind(), Kind::Coordinator);
        assert_eq!(store.redirects(), 0);
        assert!(!store.is_mirror());
        assert_eq!(store.authorization(), None);
        assert_eq!(store.repository(), None);
        assert_eq!(store.mirror_url(DIGEST), None);
        assert_eq!(
            store
                .blob_url("repositories/acme/pool/g/a.pkg", DIGEST)
                .as_str(),
            "/repositories/acme/pool/g/a.pkg"
        );
        assert!(stores.all().iter().all(|s| s.name() != COORDINATOR_STORE));
    }
    assert_eq!(file.all().len(), 1);
}

/// A stores file of the release before may name a store `coordinator`: it still loads, and that
/// store, not the implicit one, is the one its name finds, so the host fetches from it as before.
///
/// Catches a file the previous release read being refused, and a file's store shadowed.
#[test]
fn a_files_own_store_named_coordinator_is_kept_and_found_first() {
    let scratch = scratch("stores-coordinator-named");
    let text =
        "[[store]]\nname = \"coordinator\"\nkind = \"https\"\nbase = \"https://a.example/\"\n";
    let stores = load_text(scratch.path(), text).expect("the file loads as before");
    let store = stores.get(COORDINATOR_STORE).expect("the file's store");
    assert_eq!(store.kind(), Kind::Https);
    assert_eq!(
        store.blob_url("a/b.pkg", DIGEST).as_str(),
        "https://a.example/a/b.pkg"
    );
    assert_eq!(stores.all().len(), 1);
}
