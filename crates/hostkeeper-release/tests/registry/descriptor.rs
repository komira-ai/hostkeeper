//! The descriptor and base URI an entry added from a descriptor holds (`Repository.descriptor`
//! and `base_uri`): set together, agreeing with the entry's other fields, never sent to a host.

use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::descriptor::DescriptorError;
use hostkeeper_release::registry::{self, EntryError, check, for_host};
use serde_json::json;

use super::real;
use super::support::*;

/// The descriptor text of the entry [`added`] makes.
fn text() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "version": 1,
        "channels": ["testing", "stable"],
        "key": { "keyring": "debian/key.gpg", "fingerprints": [real::PRIMARY] },
        "formats": { "apt": { "path": "debian", "components": ["main"] } }
    }))
    .expect("json")
}

/// An entry as `Add` makes it: the base `https://repo.example.com`, apt under `debian`.
fn added() -> Repository {
    Repository {
        uri: "https://repo.example.com/debian".into(),
        base_uri: "https://repo.example.com".into(),
        descriptor: text(),
        ..entry("komira")
    }
}

fn with(change: impl FnOnce(&mut Repository)) -> Repository {
    let mut e = added();
    change(&mut e);
    e
}

/// Proves an entry added from a descriptor passes, and a descriptor that names a fingerprint the
/// entry lists among others (the subkey too) passes.
#[test]
fn an_entry_that_agrees_with_its_descriptor_passes() {
    assert_eq!(check(&added()), Ok(()));
    let both = with(|e| e.fingerprints = vec![real::PRIMARY.into(), real::SUBKEY.into()]);
    assert_eq!(check(&both), Ok(()));
}

/// Proves the two fields are set together: one without the other is refused by its own message.
#[test]
fn the_descriptor_and_the_base_uri_are_set_together() {
    assert_eq!(
        check(&with(|e| e.descriptor.clear())),
        Err(EntryError::BaseWithoutDescriptor)
    );
    assert_eq!(
        check(&with(|e| e.base_uri.clear())),
        Err(EntryError::DescriptorWithoutBase)
    );
    assert_eq!(
        EntryError::BaseWithoutDescriptor.to_string(),
        "the entry names a base_uri and no descriptor"
    );
    assert_eq!(
        EntryError::DescriptorWithoutBase.to_string(),
        "the entry holds a descriptor and names no base_uri"
    );
}

/// Proves the base URI is judged: a trailing `/`, `http://`, and over 512 bytes are refused.
#[test]
fn the_base_uri_is_an_https_uri_with_no_trailing_slash() {
    for base in [
        "https://repo.example.com/".to_owned(),
        "http://repo.example.com".to_owned(),
        format!("https://repo.example.com/{}", "a".repeat(512)),
    ] {
        let refused = check(&with(|e| e.base_uri = base.clone()));
        assert_eq!(refused, Err(EntryError::BaseUri(base)));
    }
}

/// Proves a descriptor that is no descriptor is refused, naming the rule it breaks.
#[test]
fn a_broken_descriptor_is_refused_naming_its_rule() {
    let refused = check(&with(|e| e.descriptor = b"{}".to_vec())).expect_err("refused");
    assert_eq!(refused, EntryError::Descriptor(DescriptorError::Version));
    assert_eq!(
        refused.to_string(),
        "the descriptor is not valid: the descriptor's version is missing or not a whole number from 1"
    );
}

/// Proves each field the descriptor decides must be the descriptor's: the apt root, the suites
/// (a public repository's), the components, and the fingerprints it claims.
#[test]
fn the_entry_must_agree_with_its_descriptor() {
    let cases: [(&str, Repository); 4] = [
        (
            "uri",
            with(|e| e.uri = "https://repo.example.com/other".into()),
        ),
        ("suites", with(|e| e.suites = vec!["testing".into()])),
        (
            "components",
            with(|e| e.components = vec!["main".into(), "extra".into()]),
        ),
        (
            "fingerprints",
            with(|e| e.fingerprints = vec![real::SUBKEY.into()]),
        ),
    ];
    for (field, entry) in cases {
        assert_eq!(
            check(&entry),
            Err(EntryError::DescriptorDisagrees(field)),
            "{field}"
        );
    }
    assert_eq!(
        EntryError::DescriptorDisagrees("uri").to_string(),
        "the uri of the entry is not the descriptor's"
    );
}

/// Proves a private repository's suites are host groups', so they need not be the descriptor's
/// channels.
#[test]
fn a_private_repository_takes_host_groups_as_its_suites() {
    let private = with(|e| {
        e.access = registry::ACCESS_HOSTS.into();
        e.credential_file = "/etc/hostkeeper/cred".into();
        e.suites = vec!["web".into()];
    });
    assert_eq!(check(&private), Ok(()));
}

/// Proves a host is never sent the descriptor or the base URI: `for_host` clears both, and the
/// entry it makes is judged by the host's rules whatever the registry holds.
#[test]
fn a_host_is_sent_neither_the_descriptor_nor_the_base_uri() {
    let sent = for_host(&added(), "web").expect("public");
    assert!(sent.descriptor.is_empty() && sent.base_uri.is_empty());
    assert_eq!(registry::check_set(std::slice::from_ref(&sent)), Ok(()));
    let plain = Repository {
        descriptor: Vec::new(),
        base_uri: String::new(),
        ..added()
    };
    assert_eq!(sent, plain);
}

/// Proves the descriptor does not change what a host holds: the rendered files and the digest of
/// an entry holding a descriptor are those of the same entry put without one.
#[test]
fn the_descriptor_changes_neither_the_rendered_files_nor_the_digest() {
    let without = Repository {
        descriptor: Vec::new(),
        base_uri: String::new(),
        ..added()
    };
    assert_eq!(
        registry::digest(&[added()]),
        registry::digest(std::slice::from_ref(&without))
    );
    let dir = registry::REPOSITORIES_DIR;
    let mirror = registry::MIRROR_DIR;
    assert_eq!(
        registry::render(&[added()], dir, mirror),
        registry::render(&[without], dir, mirror)
    );
}
