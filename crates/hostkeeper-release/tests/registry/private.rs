//! The rules of a private repository (`access` "hosts"): `check`, `check_set`, `for_host`,
//! `is_private`, `mirror_uri`, and the files and digest of a private entry.

use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::registry::{
    ACCESS_HOSTS, ACCESS_PUBLIC, EntryError, MAX_CREDENTIAL_PATH, MIRROR_DIR, REPOSITORIES_DIR,
    SetError, check, check_set, digest, for_host, is_private, mirror_uri, render,
};
use sha2::{Digest, Sha256};

use super::support::*;

const CREDENTIAL: &str = "/etc/hostkeeper-coordinator/lab.credential";

/// A valid private entry of the registry: suites `testing` and `stable`, naming its credential.
fn private(name: &str) -> Repository {
    Repository {
        access: ACCESS_HOSTS.into(),
        credential_file: CREDENTIAL.into(),
        ..entry(name)
    }
}

/// A valid private entry as a host is sent it: one suite, no credential file.
fn sent(name: &str) -> Repository {
    Repository {
        credential_file: String::new(),
        suites: vec!["stable".into()],
        ..private(name)
    }
}

fn with_credential(path: &str) -> Repository {
    Repository {
        credential_file: path.into(),
        ..private("lab")
    }
}

/// Proves the constants and the two small helpers: the names of the two accesses, the longest
/// credential path, which entry is private, and the mirror URI of a name.
#[test]
fn the_accesses_and_the_mirror_uri() {
    assert_eq!(ACCESS_PUBLIC, "public");
    assert_eq!(ACCESS_HOSTS, "hosts");
    assert_eq!(MAX_CREDENTIAL_PATH, 4096);
    assert_eq!(MIRROR_DIR, "/var/lib/hostkeeper/mirror");
    assert!(is_private(&private("lab")));
    assert!(!is_private(&entry("lab")));
    let public = Repository {
        access: "public".into(),
        ..entry("lab")
    };
    assert!(!is_private(&public));
    assert_eq!(mirror_uri("/m", "lab"), "file:/m/lab");
    assert_eq!(
        mirror_uri(MIRROR_DIR, "lab"),
        "file:/var/lib/hostkeeper/mirror/lab"
    );
}

/// Proves the registry accepts an empty and a "public" access without a credential, and a
/// private entry naming a good credential path.
#[test]
fn check_accepts_public_and_private_entries() {
    assert_eq!(check(&entry("a")), Ok(()));
    let public = Repository {
        access: "public".into(),
        ..entry("a")
    };
    assert_eq!(check(&public), Ok(()));
    assert_eq!(check(&private("a")), Ok(()));
}

/// Proves an access that is not empty, "public" or "hosts" is refused, whatever else the entry
/// holds, and the text names the value.
#[test]
fn check_refuses_an_unknown_access() {
    for access in ["Hosts", "private", "hosts ", "PUBLIC"] {
        let bad = Repository {
            access: access.into(),
            ..private("a")
        };
        assert_eq!(check(&bad), Err(EntryError::Access(access.into())));
    }
    assert_eq!(
        EntryError::Access("private".into()).to_string(),
        "the access \"private\" is not \"public\" or \"hosts\""
    );
}

/// Proves a private entry without a credential file is refused, and the text says so.
#[test]
fn check_refuses_a_private_entry_without_a_credential() {
    let bad = Repository {
        credential_file: String::new(),
        ..private("a")
    };
    assert_eq!(check(&bad), Err(EntryError::CredentialAbsent));
    assert_eq!(
        EntryError::CredentialAbsent.to_string(),
        "a private repository (access \"hosts\") names no credential_file"
    );
}

/// Proves a public entry (access empty or "public") that names a credential file is refused.
#[test]
fn check_refuses_a_public_entry_with_a_credential() {
    for access in ["", "public"] {
        let bad = Repository {
            access: access.into(),
            credential_file: CREDENTIAL.into(),
            ..entry("a")
        };
        assert_eq!(check(&bad), Err(EntryError::CredentialPublic));
    }
    assert_eq!(
        EntryError::CredentialPublic.to_string(),
        "a public repository names a credential_file: only access \"hosts\" takes one"
    );
}

/// Proves each way a credential path can be malformed is refused: relative, `/` alone, an empty
/// component, `.`, `..`, a control character, and 4097 bytes.
#[test]
fn check_refuses_a_bad_credential_path() {
    let long = format!("/{}", "a".repeat(MAX_CREDENTIAL_PATH));
    assert_eq!(long.len(), MAX_CREDENTIAL_PATH + 1);
    for path in [
        "etc/cred",
        "cred",
        "/",
        "/etc//cred",
        "/etc/cred/",
        "/etc/./cred",
        "/etc/../cred",
        "/etc/cred/..",
        "/etc/.",
        "/etc/cr\ned",
        "/etc/cr\u{7f}ed",
        "/etc/cr\0ed",
        long.as_str(),
    ] {
        assert_eq!(
            check(&with_credential(path)),
            Err(EntryError::CredentialPath(path.into())),
            "{path:?}"
        );
    }
}

/// Proves the longest path is accepted (exactly 4096 bytes), as are dotted names that are not
/// the `.` or `..` component.
#[test]
fn check_accepts_a_credential_path_of_exactly_the_limit() {
    let longest = format!("/{}", "a".repeat(MAX_CREDENTIAL_PATH - 1));
    assert_eq!(longest.len(), MAX_CREDENTIAL_PATH);
    assert_eq!(check(&with_credential(&longest)), Ok(()));
    assert_eq!(check(&with_credential("/etc/.cred/..cred/a.b")), Ok(()));
}

/// Proves the text of a bad path names it and the rule, with a control character escaped.
#[test]
fn the_text_of_a_bad_credential_path() {
    assert_eq!(
        EntryError::CredentialPath("a\nb".into()).to_string(),
        "the credential_file \"a\\nb\" is not an absolute path of at most 4096 bytes with no \
         control character and no empty, . or .. component"
    );
}

/// Proves the access rules come after the earlier fields: an entry with a bad name and a bad
/// access is refused for its name.
#[test]
fn the_access_is_judged_after_the_earlier_fields() {
    let bad = Repository {
        access: "x".into(),
        ..entry("Bad Name")
    };
    assert_eq!(check(&bad), Err(EntryError::Name("Bad Name".into())));
}

/// Proves a host's set refuses an entry naming a credential file, public or private.
#[test]
fn check_set_refuses_a_credential_file() {
    for bad in [
        private("a"),
        Repository {
            credential_file: CREDENTIAL.into(),
            ..entry("a")
        },
    ] {
        assert_eq!(
            check_set(std::slice::from_ref(&bad)),
            Err(SetError::Entry {
                name: "a".into(),
                source: EntryError::CredentialSent
            })
        );
    }
    assert_eq!(
        EntryError::CredentialSent.to_string(),
        "an entry a host is sent names a credential_file"
    );
}

/// Proves a private entry of a host's set has exactly one suite: two are refused, none is
/// refused by the suite count rule that comes first, and one is accepted without a credential.
#[test]
fn check_set_wants_one_suite_of_a_private_entry() {
    let two = Repository {
        suites: vec!["testing".into(), "stable".into()],
        ..sent("a")
    };
    assert_eq!(
        check_set(&[two]),
        Err(SetError::Entry {
            name: "a".into(),
            source: EntryError::PrivateSuites(2)
        })
    );
    let mut none = sent("a");
    none.suites.clear();
    assert_eq!(
        check_set(&[none]),
        Err(SetError::Entry {
            name: "a".into(),
            source: EntryError::SuiteCount(0)
        })
    );
    assert_eq!(check_set(&[sent("a")]), Ok(()));
    assert_eq!(
        EntryError::PrivateSuites(2).to_string(),
        "a private entry a host is sent has 2 suites, not 1"
    );
    assert_eq!(
        EntryError::PrivateSuites(0).to_string(),
        "a private entry a host is sent has 0 suites, not 1"
    );
}

/// Proves a public entry with several suites and no credential is still valid in a host's set,
/// and an unknown access is refused there too.
#[test]
fn check_set_keeps_the_public_and_access_rules() {
    assert_eq!(check_set(&[entry("a")]), Ok(()));
    let bad = Repository {
        access: "x".into(),
        ..sent("a")
    };
    assert_eq!(
        check_set(&[bad]),
        Err(SetError::Entry {
            name: "a".into(),
            source: EntryError::Access("x".into())
        })
    );
}

/// Proves a public entry is sent as it is with its credential cleared and every suite kept,
/// whatever group the host is in.
#[test]
fn for_host_sends_a_public_entry_whole() {
    let plain = entry("a");
    assert_eq!(for_host(&plain, "web"), Some(plain.clone()));
    assert_eq!(for_host(&plain, "testing"), Some(plain));
    let explicit = Repository {
        access: "public".into(),
        ..entry("a")
    };
    assert_eq!(for_host(&explicit, "web"), Some(explicit.clone()));
    let stray = Repository {
        credential_file: CREDENTIAL.into(),
        ..entry("a")
    };
    let got = for_host(&stray, "web").expect("sent");
    assert_eq!(got, entry("a"));
    assert_eq!(got.suites, vec!["testing", "stable"]);
}

/// Proves a private entry is sent to a host whose group is one of its suites, with that group
/// as the one suite and no credential file, and to a host of another group not at all.
#[test]
fn for_host_sends_a_private_entry_to_the_listed_group_only() {
    let registry = private("lab");
    let got = for_host(&registry, "stable").expect("listed");
    assert_eq!(got, sent("lab"));
    assert_eq!(got.suites, vec!["stable"]);
    assert_eq!(got.credential_file, "");
    assert_eq!(got.access, "hosts");
    assert_eq!(check_set(&[got]), Ok(()));
    let first = for_host(&registry, "testing").expect("listed");
    assert_eq!(first.suites, vec!["testing"]);
    assert_eq!(for_host(&registry, "web"), None);
    assert_eq!(for_host(&registry, ""), None);
}

fn mirrored(uri: &str, suite: &str) -> Vec<u8> {
    format!(
        "Types: deb\nURIs: {uri}\nSuites: {suite}\nComponents: main\n\
         Signed-By: /etc/hostkeeper/repositories/lab.gpg\n"
    )
    .into_bytes()
}

/// Proves a private entry renders its URIs as `file:<mirror>/<name>`, byte for byte, with the
/// rest of the file as for a public entry, and a public entry keeps its own URI.
#[test]
fn render_writes_a_private_entry_at_its_mirror() {
    let files = render(&[sent("lab")], REPOSITORIES_DIR, "/scratch/mirror");
    let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["lab-stable.sources", "lab.gpg"]);
    assert_eq!(
        files[0].bytes,
        mirrored("file:/scratch/mirror/lab", "stable")
    );
    assert_eq!(
        files[0].bytes,
        b"Types: deb\nURIs: file:/scratch/mirror/lab\nSuites: stable\nComponents: main\n\
          Signed-By: /etc/hostkeeper/repositories/lab.gpg\n"
    );
    let public = Repository {
        suites: vec!["stable".into()],
        ..entry("lab")
    };
    let files = render(&[public], REPOSITORIES_DIR, "/scratch/mirror");
    assert_eq!(
        files[0].bytes,
        mirrored("https://repo.example.com/debian", "stable")
    );
}

/// Proves the digest of a private entry is the SHA-256 over its files rendered at the
/// production directories (names, zero byte, eight-byte length, bytes), and differs from the
/// digest of the same entry public.
#[test]
fn the_digest_of_a_private_entry_is_over_its_mirror_files() {
    let entry = sent("lab");
    let uri = format!("file:{MIRROR_DIR}/lab");
    let mut hash = Sha256::new();
    for (name, bytes) in [
        ("lab-stable.sources", mirrored(&uri, "stable")),
        ("lab.gpg", entry.keyring.to_vec()),
    ] {
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(&bytes);
    }
    let want: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest(std::slice::from_ref(&entry)), want);
    let public = Repository {
        access: String::new(),
        ..entry
    };
    assert_ne!(digest(&[public]), want);
}
