//! Private repositories in the registry: the credential file `Repositories.Put` checks, what the
//! audit entry and the answers say of it, and the record of a private entry in both stores and
//! the file ([`crate::repositories`], [`crate::operator_repositories`]).

use std::os::unix::fs::{PermissionsExt as _, symlink};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::RepositoriesRefusal as Refusal;
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use serde_json::{Value, json};

use crate::marks_support::{operator, rig, rig_on, start_on};
use crate::repositories::{Kept, Registry, encode_file, parse_file, put_reason};
use crate::repositories_support::{credential_in, fingerprint, komira, private};
use crate::store::Fake;
use crate::testutil::Rig;

const URI: &str = "https://priv.example.com/apt";
const TOKEN: &str = "Bearer s3cret-token-value";

fn put(rig: &Rig, repository: Repository) -> Result<proto::PutRepositoryResponse, Refusal> {
    rig.coordinator.repositories_put(
        &operator(),
        proto::PutRepositoryRequest {
            repository: Some(repository),
            reason: "a private one".to_owned(),
        },
    )
}

fn get(rig: &Rig, name: &str) -> proto::RegisteredRepository {
    rig.coordinator
        .repositories_get(
            &operator(),
            &proto::GetRepositoryRequest {
                name: name.to_owned(),
            },
        )
        .expect("got")
        .repository
        .expect("held")
}

fn generation(rig: &Rig) -> u64 {
    rig.coordinator
        .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
        .expect("listed")
        .generation
}

/// Proves: a `Put` of a private repository whose credential file is absent, a link, a directory,
/// a FIFO, readable by group or other, empty, only a newline, two lines, holding a control
/// character or a non-ASCII byte, or longer than 4096 bytes is refused `credential_unusable`, and
/// the registry, its generation and the audit are as they were; a file of 0600 or 0400, with or
/// without a final newline, up to 4096 bytes, is put.
///
/// Catches: a credential accepted that the origin read would then fail on, or a sloppy file
/// (readable by others, followed through a link) trusted.
#[test]
fn a_private_put_with_an_unusable_credential_file_is_refused() {
    let rig = rig("private-put-credential", "");
    let dir = rig.dir.path();
    let good = credential_in(dir, "good", &format!("{TOKEN}\n"));
    let broken = |name: &str, text: &[u8], mode: u32| {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("a file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        path.to_str().expect("a path").to_owned()
    };
    let link = dir.join("link");
    symlink(&good, &link).expect("a link");
    let fifo = dir.join("fifo");
    hostkeeper_testkit::fifo(&fifo, 0o600).expect("a fifo");
    let folder = dir.join("folder");
    std::fs::create_dir(&folder).expect("a directory");
    let paths = [
        dir.join("absent").to_str().expect("a path").to_owned(),
        link.to_str().expect("a path").to_owned(),
        folder.to_str().expect("a path").to_owned(),
        fifo.to_str().expect("a path").to_owned(),
        broken("group", TOKEN.as_bytes(), 0o640),
        broken("other", TOKEN.as_bytes(), 0o604),
        broken("empty", b"", 0o600),
        broken("newline", b"\n", 0o600),
        broken("two", b"Bearer a\nBearer b\n", 0o600),
        broken("control", b"Bearer a\tb", 0o600),
        broken("carriage", b"Bearer a\r\n", 0o600),
        broken("bytes", "Bearer \u{e9}".as_bytes(), 0o600),
        broken("long", &[b'a'; 4097], 0o600),
    ];
    for path in paths {
        assert_eq!(
            put(&rig, private("priv", URI, &["web"], 1, &path)).expect_err(&path),
            Refusal::CredentialUnusable,
            "{path}"
        );
    }
    assert_eq!(generation(&rig), 0);
    assert!(rig.coordinator.ops().state().audit.is_empty());

    for (name, text, mode) in [
        ("plain", TOKEN.as_bytes().to_vec(), 0o600),
        ("read-only", format!("{TOKEN}\n").into_bytes(), 0o400),
        ("longest", vec![b'a'; 4096], 0o600),
        (
            "longest-line",
            [vec![b'a'; 4095], b"\n".to_vec()].concat(),
            0o600,
        ),
    ] {
        let path = broken(name, &text, mode);
        put(&rig, private("priv", URI, &["web"], 1, &path)).expect(name);
    }
    assert_eq!(generation(&rig), 4);
}

/// Proves: a private entry is put as the other rules of the registry judge it: a credential file
/// on a public entry, none on a private one, and a path that is not absolute are `invalid_request`.
///
/// Catches: the rules of `registry::check` dropped from the operator's call.
#[test]
fn a_private_entry_breaks_the_registry_rules_as_invalid_request() {
    let rig = rig("private-put-rules", "");
    let good = credential_in(rig.dir.path(), "good", TOKEN);
    let mut public_with_file = komira(1);
    public_with_file.credential_file.clone_from(&good);
    let bare = private("priv", URI, &["web"], 1, "");
    let relative = private("priv", URI, &["web"], 1, "credential");
    for entry in [public_with_file, bare, relative] {
        assert_eq!(put(&rig, entry), Err(Refusal::InvalidRequest));
    }
    assert_eq!(generation(&rig), 0);
}

/// Proves: the audit entry of a private `Put` ends `access hosts credential <path>`, the file's
/// path and no byte of its content, by exact equality; a public `Put`'s reason is as it was.
///
/// Catches: a credential's bytes in the audit ring or an audit entry that does not say the
/// repository is private.
#[test]
fn a_private_put_is_audited_with_its_access_and_the_credential_path_only() {
    let rig = rig("private-put-audit", "");
    let path = credential_in(rig.dir.path(), "audit", TOKEN);
    let entry = private("priv", URI, &["web", "db"], 1, &path);
    put(&rig, entry.clone()).expect("put");
    let key = fingerprint(&entry.keyring);
    let expected = format!(
        "a private one: priv {URI} suites web,db fingerprints {key} access hosts credential {path}"
    );
    let state = rig.coordinator.ops().state();
    let reason = state.audit.last().and_then(|e| e.reason.clone());
    assert_eq!(reason.as_deref(), Some(expected.as_str()));
    assert_eq!(put_reason("a private one", &entry), expected);
    assert!(!expected.contains("s3cret"));
    assert_eq!(
        put_reason("why", &komira(1)),
        format!(
            "why: komira https://packages.example.com/apt suites testing,stable fingerprints {}",
            fingerprint(&komira(1).keyring)
        )
    );
}

/// Proves: `Get` and `List` answer a private entry as held, `access` and `credential_file`
/// included (the path is not secret), and `List` without the keyring only.
///
/// Catches: an answer that drops the fields of a private entry.
#[test]
fn list_and_get_answer_a_private_entry_as_held() {
    let rig = rig("private-put-answers", "");
    let path = credential_in(rig.dir.path(), "answers", TOKEN);
    let entry = private("priv", URI, &["web"], 1, &path);
    let done = put(&rig, entry.clone()).expect("put");
    assert_eq!(
        done.repository.expect("held").repository,
        Some(entry.clone())
    );
    assert_eq!(get(&rig, "priv").repository, Some(entry.clone()));
    let listed = rig
        .coordinator
        .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
        .expect("listed");
    let mut bare = entry;
    bare.keyring.clear();
    let found = listed
        .repositories
        .iter()
        .find(|held| held.repository.as_ref().is_some_and(|r| r.name == "priv"))
        .expect("listed");
    assert_eq!(found.repository, Some(bare));
}

/// Proves: a private entry survives a restart on the fake store and on the files, and the file of
/// the registry names its `access` and `credential_file`; and the registry's file text reads back
/// as the registry.
///
/// Catches: a private entry that is kept as a public one.
#[test]
fn a_private_entry_survives_a_restart_on_the_fake_and_on_the_files() {
    let fake = Fake::new();
    let on_fake = rig_on("private-restart-fake", &fake, "");
    let path = credential_in(on_fake.dir.path(), "restart", TOKEN);
    let entry = private("priv", URI, &["web"], 1, &path);
    put(&on_fake, entry.clone()).expect("put");
    let again = start_on(&fake, &on_fake.clock, "");
    let kept = again
        .repositories_get(
            &operator(),
            &proto::GetRepositoryRequest {
                name: "priv".to_owned(),
            },
        )
        .expect("got");
    assert_eq!(
        kept.repository.expect("held").repository,
        Some(entry.clone())
    );

    let mut files = rig("private-restart-files", "");
    let path = credential_in(files.dir.path(), "restart", TOKEN);
    let entry = private("priv", URI, &["web"], 1, &path);
    put(&files, entry.clone()).expect("put");
    files.coordinator = crate::marks_support::start(&files.dir, &files.clock, "");
    assert_eq!(get(&files, "priv").repository, Some(entry));
    let text = std::fs::read_to_string(files.dir.path().join("desired.json.repositories"))
        .expect("the file");
    assert!(text.contains("\"access\": \"hosts\""), "{text}");
    assert!(
        text.contains(&format!("\"credential_file\": {path:?}")),
        "{text}"
    );
}

/// Proves: the record of a private entry holds `access` and `credential_file`, that of a public
/// one holds neither (it is the record the release before wrote), a record without them reads as
/// public with both empty, and a record whose `access` or `credential_file` is not text is
/// refused by reason.
///
/// Catches: a record the release before cannot read, or a previous release's record that no longer
/// loads.
#[test]
fn the_record_of_a_private_entry_has_the_two_fields_and_a_record_without_them_is_public() {
    let entry = private("priv", URI, &["web"], 1, "/etc/hostkeeper/credentials/priv");
    let record = Kept {
        repository: entry.clone(),
        generation: 2,
    }
    .to_json();
    assert_eq!(record["repository"]["access"], "hosts");
    assert_eq!(
        record["repository"]["credential_file"],
        "/etc/hostkeeper/credentials/priv"
    );
    assert_eq!(Kept::from_json(&record).expect("read").repository, entry);

    let public = Kept {
        repository: komira(1),
        generation: 1,
    }
    .to_json();
    let keys: Vec<&String> = public["repository"]
        .as_object()
        .expect("a map")
        .keys()
        .collect();
    assert_eq!(
        keys,
        [
            "components",
            "fingerprints",
            "keyring",
            "kind",
            "name",
            "suites",
            "uri"
        ]
    );
    let read = Kept::from_json(&public).expect("read").repository;
    assert_eq!(
        (read.access.as_str(), read.credential_file.as_str()),
        ("", "")
    );
    assert!(!hostkeeper_release::registry::is_private(&read));

    for key in ["access", "credential_file"] {
        let mut broken: Value = record.clone();
        broken["repository"][key] = json!(7);
        assert_eq!(
            Kept::from_json(&broken).expect_err(key),
            format!("{key} is not text")
        );
    }
}

/// Proves: a registry file that holds a private entry and a public one reads back as the registry
/// it was written from, and a file of the release before (no `access`, no `credential_file`) reads
/// as a registry of public entries.
///
/// Catches: a registry file that the same release cannot read back.
#[test]
fn a_registry_file_with_a_private_entry_reads_back_and_the_previous_one_reads_public() {
    let path = "/etc/hostkeeper/credentials/priv";
    let held = Registry::default()
        .put(private("priv", URI, &["web"], 1, path))
        .expect("put")
        .put(komira(2))
        .expect("put");
    let bytes = encode_file(&held).expect("encoded");
    assert_eq!(parse_file(&bytes).expect("read"), held);

    let previous = Registry::default().put(komira(1)).expect("put");
    assert_eq!(
        parse_file(&encode_file(&previous).expect("encoded")).expect("read"),
        previous
    );
    assert!(
        !previous
            .get("komira")
            .map(|k| &k.repository)
            .is_some_and(hostkeeper_release::registry::is_private)
    );
}

/// Proves: the channels of the private entries are `<name>-<suite>` of those only.
///
/// Catches: a public repository's channel treated as private, or a private one missed.
#[test]
fn the_private_channels_are_those_of_the_private_entries() {
    let held = Registry::default()
        .put(private(
            "priv",
            URI,
            &["web", "db"],
            1,
            "/etc/hostkeeper/credentials/priv",
        ))
        .expect("put")
        .put(komira(2))
        .expect("put");
    assert_eq!(
        held.private_channels().into_iter().collect::<Vec<_>>(),
        ["priv-db", "priv-web"]
    );
    assert!(Registry::default().private_channels().is_empty());
}
