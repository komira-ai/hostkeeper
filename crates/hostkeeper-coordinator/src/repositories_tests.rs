//! The registry of package repositories ([`crate::repositories`]): a record read back as it was
//! stored, the rules a registry read back is judged by, what a `Put` and a `Remove` make of it,
//! the repositories file's text and refusals, and the audit reasons.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::release::v1::Repository;
use serde_json::{Value, json};

use crate::desired::DesiredError;
use crate::repositories::{
    Kept, PutError, Registry, encode_file, parse_file, put_reason, remove_reason, repositories_path,
};
use crate::repositories_support::{keyring, komira, repository};

fn kept(repository: Repository, generation: u64) -> Kept {
    Kept {
        repository,
        generation,
    }
}

fn refused(value: Value) -> String {
    Kept::from_json(&value).expect_err("refused")
}

/// Proves: a record is stored as its generation and its repository, the keyring as standard
/// base64, and read back equal; a record whose entry breaks a rule of the registry is refused.
///
/// Catches: a record stored in another shape, a keyring that does not survive the text, an entry
/// read back without the checks a `Put` makes.
#[test]
fn a_record_is_read_back_as_it_was_stored() {
    let stored = kept(komira(1), 3);
    let json = stored.to_json();
    assert_eq!(json["generation"], 3);
    assert_eq!(json["repository"]["name"], "komira");
    assert_eq!(json["repository"]["suites"], json!(["testing", "stable"]));
    assert_eq!(json["repository"]["keyring"], base64_of(&komira(1).keyring));
    assert_eq!(Kept::from_json(&json), Ok(stored));

    let mut bad = komira(1);
    bad.kind = "rpm".to_owned();
    assert_eq!(
        Kept::from_json(&kept(bad, 1).to_json()).expect_err("refused"),
        "the kind \"rpm\" is not \"apt\""
    );
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Proves: each way a stored record can be wrong is refused by its own words: no generation, no
/// repository, a field missing, a list holding a number, and a keyring that is not base64.
///
/// Catches: a record with a field missing read as a default, which a row could hold after a
/// manual edit.
#[test]
fn a_record_that_does_not_read_is_refused_by_reason() {
    let good = kept(komira(1), 1).to_json();
    let without = |path: &[&str]| {
        let mut value = good.clone();
        let mut at = &mut value;
        for key in &path[..path.len() - 1] {
            at = &mut at[*key];
        }
        at.as_object_mut()
            .expect("an object")
            .remove(path[path.len() - 1]);
        value
    };
    assert_eq!(refused(without(&["generation"])), "no generation");
    assert_eq!(refused(without(&["repository"])), "no repository");
    for field in [
        "name",
        "kind",
        "uri",
        "suites",
        "components",
        "fingerprints",
        "keyring",
    ] {
        assert_eq!(
            refused(without(&["repository", field])),
            format!("no {field}")
        );
    }
    let mut numbers = good.clone();
    numbers["repository"]["components"] = json!([1]);
    assert_eq!(
        refused(numbers),
        "components holds a value that is not text"
    );
    let mut garbled = good;
    garbled["repository"]["keyring"] = json!("not base64!");
    assert_eq!(refused(garbled), "a keyring that is not base64");
}

/// Proves: a registry is refused past 16 repositories, for an entry kept under another name, for
/// an entry at a generation past the registry's, and for two entries whose channels clash; one
/// that breaks none is read.
///
/// Catches: a registry read back that a `Put` could not have made.
#[test]
fn a_registry_read_back_is_judged_as_a_put_judges_it() {
    let sixteen: BTreeMap<String, Kept> = (0..16)
        .map(|n| {
            let name = format!("repo-{n}");
            let r = repository(&name, "https://packages.example.com/a", &["stable"], 1);
            (name, kept(r, 1))
        })
        .collect();
    assert!(Registry::new(1, sixteen.clone()).is_ok());
    let mut seventeen = sixteen;
    let r = repository("repo-x", "https://packages.example.com/a", &["stable"], 1);
    seventeen.insert("repo-x".to_owned(), kept(r, 1));
    assert_eq!(
        Registry::new(1, seventeen).expect_err("refused"),
        "more than 16 repositories"
    );

    let renamed = BTreeMap::from([("other".to_owned(), kept(komira(1), 1))]);
    assert_eq!(
        Registry::new(1, renamed).expect_err("refused"),
        "other holds the repository komira"
    );
    let ahead = BTreeMap::from([("komira".to_owned(), kept(komira(1), 5))]);
    assert_eq!(
        Registry::new(4, ahead).expect_err("refused"),
        "komira is at generation 5, past the registry's 4"
    );
    let clash = BTreeMap::from([
        (
            "a".to_owned(),
            kept(
                repository("a", "https://packages.example.com/a", &["b-c"], 1),
                1,
            ),
        ),
        (
            "a-b".to_owned(),
            kept(
                repository("a-b", "https://packages.example.com/b", &["c"], 2),
                1,
            ),
        ),
    ]);
    assert_eq!(
        Registry::new(1, clash).expect_err("refused"),
        "a: the channel a-b-c is another's too"
    );
}

/// Proves: a `Put` raises the generation by one and keeps the repository at it; a put of a name
/// the registry holds replaces it and leaves the others at their generations; a channel that is
/// another repository's is `Conflict` and names it; a new name at 16 is `Capacity` while a
/// replacement at 16 is taken; and the registry put over is unchanged.
///
/// Catches: a generation that does not move, a replacement that is counted against the cap, a
/// clash with another repository's channel let through, a put that edits the registry it reads.
#[test]
fn a_put_raises_the_generation_and_refuses_a_clash_and_a_seventeenth() {
    let empty = Registry::default();
    let first = empty.put(komira(1)).expect("put");
    assert_eq!(
        (first.generation, first.get("komira").map(|k| k.generation)),
        (1, Some(1))
    );
    assert_eq!(empty, Registry::default());

    let other = repository("other", "https://packages.example.com/o", &["stable"], 2);
    let two = first.put(other.clone()).expect("put");
    let replaced = two.put(komira(3)).expect("a replacement");
    assert_eq!(replaced.generation, 3);
    assert_eq!(replaced.get("komira"), Some(&kept(komira(3), 3)));
    assert_eq!(replaced.get("other"), Some(&kept(other, 2)));

    let a = repository("a", "https://packages.example.com/a", &["b-c"], 4);
    let a_b = repository("a-b", "https://packages.example.com/b", &["c"], 5);
    let with_a = replaced.put(a).expect("put");
    assert_eq!(with_a.put(a_b), Err(PutError::Conflict("a-b-c".to_owned())));

    let mut full = Registry::default();
    for n in 0..16 {
        let name = format!("repo-{n}");
        full = full
            .put(repository(
                &name,
                "https://packages.example.com/a",
                &[&format!("s{n}")],
                1,
            ))
            .expect("put");
    }
    let new = repository("repo-new", "https://packages.example.com/a", &["new"], 1);
    assert_eq!(full.put(new), Err(PutError::Capacity));
    let again = repository("repo-3", "https://packages.example.com/b", &["s3"], 9);
    assert!(full.put(again).is_ok());
}

/// Proves: a `Remove` raises the generation by one and drops the repository; a name the registry
/// does not hold is none, and the registry's generation survives the removal of its last
/// repository.
///
/// Catches: a removal that does not count as a change, one of an unknown name that does.
#[test]
fn a_remove_raises_the_generation_and_an_unknown_name_is_none() {
    let one = Registry::default().put(komira(1)).expect("put");
    assert_eq!(one.remove("other"), None);
    let none = one.remove("komira").expect("removed");
    assert_eq!((none.generation, none.entries.len()), (2, 0));
}

/// Proves: the entries a host needs are the registry's, by name and each once, whatever order the
/// names come in; one name the registry lacks makes it none.
///
/// Catches: a set built from a name that is not registered, a repository sent twice.
#[test]
fn the_entries_of_a_set_of_names_are_sorted_once_each_and_none_when_one_is_missing() {
    let two = Registry::default()
        .put(komira(1))
        .and_then(|r| {
            r.put(repository(
                "other",
                "https://packages.example.com/o",
                &["x"],
                2,
            ))
        })
        .expect("put");
    let got = two.entries_of(["other", "komira", "other"]).expect("both");
    assert_eq!(
        got.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["komira", "other"]
    );
    assert_eq!(two.entries_of(["komira", "absent"]), None);
}

/// Proves: the repositories file is `<state-file>.repositories`, holds the generation and each
/// repository's record by name as the text below, and reads back the same registry; the empty
/// registry is read back empty at generation 0.
///
/// Catches: a change of the file's shape, a generation that is lost with the last repository.
#[test]
fn the_repositories_file_is_written_as_this_text_and_read_back() {
    assert_eq!(
        repositories_path(std::path::Path::new("/state/desired.json")),
        std::path::PathBuf::from("/state/desired.json.repositories")
    );
    let held = Registry::default().put(komira(1)).expect("put");
    let text = String::from_utf8(encode_file(&held).expect("encoded")).expect("text");
    let expected = format!(
        "{{\n  \"generation\": 1,\n  \"repositories\": {{\n    \"komira\": {}\n  }}\n}}\n",
        serde_json::to_string_pretty(&kept(komira(1), 1).to_json())
            .expect("json")
            .replace('\n', "\n    ")
    );
    assert_eq!(text, expected);
    assert_eq!(parse_file(text.as_bytes()).expect("read"), held);

    let emptied = held.remove("komira").expect("removed");
    let text = encode_file(&emptied).expect("encoded");
    assert_eq!(parse_file(&text).expect("read"), emptied);
    assert_eq!(emptied.generation, 2);
}

/// Proves: a repositories file that is not the file's JSON, has no generation or no repositories,
/// holds a record that does not read (named by repository), or a registry that breaks a rule is
/// refused with the words that say which.
///
/// Catches: a bad file taken as an empty registry, which a restart would then overwrite.
#[test]
fn a_repositories_file_that_is_none_is_refused_by_reason() {
    let why = |bytes: &[u8]| match parse_file(bytes).expect_err("refused") {
        DesiredError::Malformed(why) => why,
        other => panic!("not malformed: {other}"),
    };
    assert!(why(b"not json").starts_with("the repositories: expected"));
    assert_eq!(
        why(br#"{"repositories":{}}"#),
        "the repositories: no generation"
    );
    assert_eq!(
        why(br#"{"generation":1}"#),
        "the repositories: no repositories"
    );
    let record = json!({"generation": 1, "repository": {"name": "komira"}});
    assert_eq!(
        why(json!({"generation": 1, "repositories": {"komira": record}})
            .to_string()
            .as_bytes()),
        "the repositories: komira: no kind"
    );
    let ahead = kept(komira(1), 9).to_json();
    assert_eq!(
        why(json!({"generation": 1, "repositories": {"komira": ahead}})
            .to_string()
            .as_bytes()),
        "the repositories: komira is at generation 9, past the registry's 1"
    );
}

/// Proves: the audit reasons are the operator's reason, then the repository's name, URI, suites
/// and fingerprints (a put) or its name (a remove), exactly.
///
/// Catches: an audit entry that does not say which repository changed or what it was changed to.
#[test]
fn the_audit_reasons_name_the_repository_and_what_it_was_put_as() {
    let repository = komira(1);
    let print = repository.fingerprints[0].clone();
    assert_eq!(
        put_reason("new key", &repository),
        format!(
            "new key: komira https://packages.example.com/apt suites testing,stable fingerprints {print}"
        )
    );
    assert_eq!(remove_reason("retired", "komira"), "retired: komira");
    assert_ne!(keyring(1), keyring(2));
}

/// A repository as `Add` makes it from the descriptor `text`: `komira(1)` with its descriptor and
/// base URL.
fn described(text: &Value) -> Repository {
    let mut entry = komira(1);
    entry.uri = "https://packages.example.com/apt".to_owned();
    entry.base_uri = "https://packages.example.com".to_owned();
    entry.descriptor = serde_json::to_vec(text).expect("json");
    entry
}

/// The descriptor of [`described`]: the keyring of `komira(1)` at `apt/key.gpg`.
fn descriptor_of_komira() -> Value {
    json!({
        "version": 1,
        "channels": ["testing", "stable"],
        "key": {
            "keyring": "apt/key.gpg",
            "fingerprints": [crate::repositories_support::fingerprint(&komira(1).keyring)]
        },
        "formats": { "apt": { "path": "apt", "components": ["main"] } }
    })
}

/// The release before's reading of a stored record, frozen: the named keys it knows (`name`,
/// `kind`, `uri`, `suites`, `components`, `fingerprints`, `keyring`, `access`, `credential_file`)
/// and no other, then the rules of the registry on the entry it made. It is what "an older
/// coordinator starts on the data" is judged by.
fn previous_release_reads(record: &Value) -> Result<Repository, String> {
    let stored = record.get("repository").ok_or("no repository")?;
    let text = |key: &str| {
        stored
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("no {key}"))
    };
    let optional = |key: &str| {
        stored
            .get(key)
            .map(|value| value.as_str().map(str::to_owned).ok_or("not text"))
            .transpose()
            .map(Option::unwrap_or_default)
    };
    let texts = |key: &str| -> Result<Vec<String>, String> {
        stored
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("no {key}"))?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "not text".to_owned())
            })
            .collect()
    };
    use base64::Engine as _;
    let repository = Repository {
        name: text("name")?,
        kind: text("kind")?,
        uri: text("uri")?,
        suites: texts("suites")?,
        components: texts("components")?,
        fingerprints: texts("fingerprints")?,
        keyring: base64::engine::general_purpose::STANDARD
            .decode(text("keyring")?)
            .map_err(|_| "not base64".to_owned())?,
        access: optional("access")?,
        credential_file: optional("credential_file")?,
        ..Repository::default()
    };
    hostkeeper_release::registry::check(&repository).map_err(|why| why.to_string())?;
    Ok(repository)
}

/// Proves: a record `to_json` wrote for an entry added from a descriptor (the descriptor as
/// standard base64 and the base URL) is read by the release before, which names the keys it knows
/// only: it starts on the data and gets the apt entry, as it was before the descriptor; this build
/// reads the record back equal.
///
/// Catches: a stored key that would stop the release before from starting (a format change that
/// is not additive), or a record this build cannot read back.
#[test]
fn previous_release_parses_a_record_with_descriptor_fields() {
    let entry = described(&descriptor_of_komira());
    let stored = kept(entry.clone(), 4);
    let record = stored.to_json();
    assert_eq!(
        record["repository"]["base_uri"],
        "https://packages.example.com"
    );
    assert_eq!(
        record["repository"]["descriptor"],
        base64_of(&entry.descriptor)
    );
    let older = previous_release_reads(&record).expect("the release before starts on it");
    assert_eq!(
        older,
        Repository {
            descriptor: Vec::new(),
            base_uri: String::new(),
            ..entry
        }
    );
    assert_eq!(Kept::from_json(&record), Ok(stored));
    let file = encode_file(
        &Registry::new(
            4,
            BTreeMap::from([("komira".to_owned(), kept(komira(1), 4))]),
        )
        .expect("registry"),
    )
    .expect("encoded");
    assert!(
        String::from_utf8(file)
            .expect("text")
            .contains("\"komira\"")
    );
}

/// Proves: the record of an entry put without a descriptor is the one the release before wrote,
/// byte for byte: the generation and the nine fields it knew and no `descriptor` or `base_uri`
/// key; a private one adds `access` and `credential_file` as before.
///
/// Catches: a new key written for every entry, which would make a rollback read a changed file.
#[test]
fn a_record_without_descriptor_fields_is_byte_identical_to_the_previous_release() {
    let record = kept(komira(1), 3).to_json();
    let expected = json!({
        "generation": 3,
        "repository": {
            "name": "komira",
            "kind": "apt",
            "uri": "https://packages.example.com/apt",
            "suites": ["testing", "stable"],
            "components": ["main"],
            "fingerprints": [crate::repositories_support::fingerprint(&komira(1).keyring)],
            "keyring": base64_of(&komira(1).keyring),
        },
    });
    assert_eq!(record, expected);
    assert_eq!(
        serde_json::to_string_pretty(&record).expect("text"),
        serde_json::to_string_pretty(&expected).expect("text")
    );
    let private = kept(
        crate::repositories_support::private(
            "priv",
            "https://p.example.com/",
            &["web"],
            2,
            "/etc/cred",
        ),
        1,
    )
    .to_json();
    let keys: Vec<&str> = private["repository"]
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert!(keys.contains(&"access") && keys.contains(&"credential_file"));
    assert!(!keys.contains(&"descriptor") && !keys.contains(&"base_uri"));
}

/// Proves: a record whose descriptor is not base64, is not text, or disagrees with its entry is
/// refused by its own words, and one holding a base URL and no descriptor too.
///
/// Catches: a stored descriptor that is read as empty, which would drop a repository's macOS
/// source silently.
#[test]
fn a_record_with_a_descriptor_that_does_not_read_is_refused() {
    let good = kept(described(&descriptor_of_komira()), 1).to_json();
    let mut garbled = good.clone();
    garbled["repository"]["descriptor"] = json!("not base64!");
    assert_eq!(refused(garbled), "a descriptor that is not base64");
    let mut numbers = good.clone();
    numbers["repository"]["descriptor"] = json!(7);
    assert_eq!(refused(numbers), "descriptor is not text");
    let mut other = good.clone();
    other["repository"]["uri"] = json!("https://packages.example.com/other");
    assert_eq!(
        refused(other),
        "the uri of the entry is not the descriptor's"
    );
    let mut orphan = good;
    orphan["repository"]
        .as_object_mut()
        .expect("o")
        .remove("descriptor");
    assert_eq!(
        refused(orphan),
        "the entry names a base_uri and no descriptor"
    );
}

/// Proves: the platforms of an entry are the Linux ones with no descriptor or a descriptor that
/// does not read, those its descriptor names (macOS included) for a public one, and the Linux ones
/// only for a private one.
///
/// Catches: a legacy entry shown as serving macOS, or a private repository's macOS format shown
/// as served.
#[test]
fn the_platforms_of_an_entry_are_its_descriptors() {
    use crate::repositories::platforms_of;
    let both = ["linux-x86_64", "linux-arm64"];
    assert_eq!(platforms_of(&komira(1)), both);
    let mut unreadable = komira(1);
    unreadable.descriptor = b"{".to_vec();
    assert_eq!(platforms_of(&unreadable), both);
    let mut mac = descriptor_of_komira();
    mac["formats"]["macos-pkg"] = json!({"path": "pkg", "platforms": ["macos-arm64"]});
    let public = described(&mac);
    assert_eq!(
        platforms_of(&public),
        ["linux-x86_64", "linux-arm64", "macos-arm64"]
    );
    let mut private = public;
    private.access = "hosts".to_owned();
    private.credential_file = "/etc/cred".to_owned();
    assert_eq!(platforms_of(&private), both);
}
