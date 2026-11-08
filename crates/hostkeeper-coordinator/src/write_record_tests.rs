use std::sync::Arc;

use hostkeeper_wire::version::Version;

use super::*;
use crate::desired::{DesiredStore, Recorded};
use crate::service::SetError;
use crate::store::{Fake, Shared, store_failure};
use crate::testutil::scratch;

fn version(text: &str) -> Version {
    Version::parse(text).expect("a version")
}

/// A desired state of `at` whose storage minimum is `storage`.
fn state(at: &str, storage: u32) -> Desired {
    let text = format!(r#"{{"version":"{at}","min_up":{{"storage":{storage},"frontend":0}}}}"#);
    Desired::parse(text.as_bytes()).expect("a desired state")
}

fn id(text: &str) -> WriteId {
    WriteId::parse(text).expect("a write id")
}

/// A desired-state store over `fake`, as it holds the state.
fn store_on(fake: &Fake) -> DesiredStore {
    let backend: Shared = Arc::new(fake.clone());
    DesiredStore::over(backend, fake.desired())
}

fn keep_all(_: &Version) -> bool {
    true
}

/// Proves: a write id is 1 to 128 visible ASCII characters, `!` to `~`: one character and 128
/// are taken, and the empty id, 129 characters, a space, a tab, a DEL and a character beyond
/// ASCII are each refused with the text that says the rule.
///
/// Catches: a write id the record could not give back as sent (a space or a control character
/// in a log line, a multi-byte character), or one of any length.
#[test]
fn a_write_id_is_one_to_128_visible_ascii_characters() {
    for text in ["a", "!~", "build-42/attempt.1", &"x".repeat(MAX_WRITE_ID)] {
        assert_eq!(
            WriteId::parse(text).map(|id| id.as_str().to_owned()),
            Ok(text.to_owned())
        );
    }
    for text in [
        "",
        &"x".repeat(MAX_WRITE_ID + 1),
        "a b",
        "a\tb",
        "a\u{7f}",
        "é",
    ] {
        assert_eq!(WriteId::parse(text), Err(BadWriteId), "{text:?}");
    }
    assert_eq!(
        BadWriteId.to_string(),
        "a write id is 1 to 128 visible ASCII characters"
    );
}

/// Proves: each write raises the generation by one and keeps its own id, none clearing the one
/// before; the first write replaces nothing; a change of version replaces the version and the
/// minimums before it; a write of the same version keeps what the last change of version
/// replaced; the generation stops at the largest count rather than wrapping.
///
/// Catches: a record that keeps a stale write id, that names a replaced state for a write of the
/// same version, or that forgets it.
#[test]
fn a_write_raises_the_generation_and_keeps_its_own_id() {
    let first = WriteRecord::default().after(None, &state("1.0", 2), Some(id("w1")));
    assert_eq!(
        first,
        WriteRecord {
            generation: 1,
            replaced: None,
            write_id: Some(id("w1")),
        }
    );
    let same = first.after(Some(&state("1.0", 2)), &state("1.0", 3), None);
    assert_eq!(
        (same.generation, same.replaced, same.write_id),
        (2, None, None)
    );

    let changed = WriteRecord {
        generation: 2,
        ..WriteRecord::default()
    }
    .after(Some(&state("1.0", 3)), &state("2.0", 1), Some(id("w3")));
    let replaced = Replaced {
        version: version("1.0"),
        min_up: state("1.0", 3).min_up,
    };
    assert_eq!(
        changed,
        WriteRecord {
            generation: 3,
            replaced: Some(replaced.clone()),
            write_id: Some(id("w3")),
        }
    );
    let again = changed.after(Some(&state("2.0", 1)), &state("2.0", 1), None);
    assert_eq!((again.generation, again.replaced), (4, Some(replaced)));

    let top = WriteRecord {
        generation: u64::MAX,
        ..WriteRecord::default()
    };
    assert_eq!(top.after(None, &state("1.0", 0), None).generation, u64::MAX);
}

/// Proves: a write that names no generation is taken at any generation, one that names the
/// generation now is taken, and one that names another is refused `Conflict` with both numbers;
/// only a precondition or a write id needs a store that keeps records; and each refusal reaches
/// the set as the error `Releases` maps (`generation_conflict`, `invalid_request`,
/// `store_failed`), with its text.
///
/// Catches: a precondition read as "at least", or a refusal that reaches the caller as another
/// refusal.
#[test]
fn a_precondition_admits_only_the_generation_now() {
    let none = WriteAsk::default();
    assert!(!none.needs_records());
    assert!(none.admits(7).is_ok());
    let named = WriteAsk {
        if_generation: Some(7),
        write_id: None,
    };
    assert!(named.needs_records());
    assert!(named.admits(7).is_ok());
    let error = named.admits(8).expect_err("another generation");
    assert!(matches!(error, WriteError::Conflict { asked: 7, now: 8 }));
    assert_eq!(
        error.to_string(),
        "generation_conflict: the service's generation is 8, not 7"
    );
    let with_id = WriteAsk {
        if_generation: None,
        write_id: Some(id("w")),
    };
    assert!(with_id.needs_records());
    assert!(with_id.admits(3).is_ok());

    assert!(matches!(
        SetError::from(error),
        SetError::Invalid(DesiredError::GenerationConflict { asked: 7, now: 8 })
    ));
    assert!(matches!(
        SetError::from(WriteError::Unrecorded),
        SetError::Invalid(DesiredError::Unrecorded)
    ));
    assert_eq!(
        WriteError::Unrecorded.to_string(),
        "the desired-state store keeps no generation or write id"
    );
    let failed = WriteError::Store(store_failure().into_store());
    assert_eq!(failed.to_string(), "fake: the fake refuses this commit");
    assert!(matches!(SetError::from(failed), SetError::Store(_)));
}

/// Proves: over a backend that keeps write records every accepted write raises the generation,
/// a set identical to the state held included, and each is committed with its record; the store
/// answers the record after each write and shows it as `write_record`, generation 0 before the
/// first; a force's staging raises it too, with no write id.
///
/// Catches: an identical set skipped as `Unchanged` (so a writer's `if_generation` could not
/// tell its set landed), or a record kept in memory and not committed.
#[test]
fn an_accepted_write_raises_the_generation_even_for_an_identical_set() {
    let fake = Fake::keeping_records();
    let store = store_on(&fake);
    assert_eq!(store.write_record("web"), Some(WriteRecord::default()));
    for generation in 1..=2 {
        let recorded = store.record("web", state("1.0", 1), keep_all);
        assert_eq!(recorded.expect("written"), Recorded::Written);
        assert_eq!(
            store.write_record("web").map(|r| r.generation),
            Some(generation)
        );
    }
    assert_eq!(fake.commits(), 2);
    assert_eq!(fake.desired().records["web"].generation, 2);

    let ask = WriteAsk {
        if_generation: Some(2),
        write_id: Some(id("ci-7")),
    };
    let record = store
        .record_as("web", state("1.0", 1), keep_all, &ask, None)
        .expect("taken");
    let expected = WriteRecord {
        generation: 3,
        replaced: None,
        write_id: Some(id("ci-7")),
    };
    assert_eq!(record, Some(expected.clone()));
    assert_eq!(fake.desired().records["web"], expected);

    let staged = store.stage("web", state("2.0", 1), keep_all);
    let after = staged.after.records["web"].clone();
    assert_eq!((after.generation, after.write_id), (4, None));
    assert_eq!(after.replaced.map(|r| r.version), Some(version("1.0")));
}

/// Proves: a write whose `if_generation` is not the generation now is refused `Conflict` with
/// nothing committed and nothing changed in memory; a write the backend refuses is refused with
/// the store's error, and nothing changes either.
///
/// Catches: a refused precondition that still commits, or that leaves memory ahead of what is
/// durable.
#[test]
fn a_stale_if_generation_is_refused_generation_conflict_and_changes_nothing() {
    let fake = Fake::keeping_records();
    let store = store_on(&fake);
    store
        .record("web", state("1.0", 1), keep_all)
        .expect("written");
    let before = (store.all(), store.write_record("web"), fake.commits());
    let stale = WriteAsk {
        if_generation: Some(0),
        write_id: Some(id("late")),
    };
    let refused = store.record_as("web", state("2.0", 1), keep_all, &stale, None);
    assert!(matches!(
        refused,
        Err(WriteError::Conflict { asked: 0, now: 1 })
    ));
    assert!(matches!(
        store.admits("web", &stale),
        Err(WriteError::Conflict { asked: 0, now: 1 })
    ));
    assert_eq!(
        (store.all(), store.write_record("web"), fake.commits()),
        before
    );

    fake.fail();
    let fresh = WriteAsk {
        if_generation: Some(1),
        write_id: None,
    };
    let failed = store.record_as("web", state("2.0", 1), keep_all, &fresh, None);
    assert!(matches!(failed, Err(WriteError::Store(_))));
    assert_eq!(
        (store.all(), store.write_record("web"), fake.commits()),
        before
    );
}

/// Proves: over the desired-state file, which keeps no write records, the store shows no record,
/// an identical set still writes nothing (`Unchanged`), and a write that names a generation or a
/// write id is refused `Unrecorded` with the file as it was.
///
/// Catches: a file store that claims a generation it would lose at the next start, or that takes
/// a precondition it cannot check.
#[test]
fn the_file_store_keeps_no_record_and_refuses_what_needs_one() {
    let dir = scratch("write-record-files");
    let path = dir.path().join("desired.json");
    let store = DesiredStore::open(&path, None).expect("a store");
    assert!(!store.keeps_write_records());
    assert_eq!(store.write_record("web"), None);
    let written = store.record("web", state("1.0", 1), keep_all);
    assert_eq!(written.expect("written"), Recorded::Written);
    let again = store.record("web", state("1.0", 1), keep_all);
    assert_eq!(again.expect("nothing to write"), Recorded::Unchanged);
    let text = std::fs::read(&path).expect("the file");
    for ask in [
        WriteAsk {
            if_generation: Some(0),
            write_id: None,
        },
        WriteAsk {
            if_generation: None,
            write_id: Some(id("w")),
        },
    ] {
        let refused = store.record_as("web", state("2.0", 1), keep_all, &ask, None);
        assert!(matches!(refused, Err(WriteError::Unrecorded)));
    }
    let plain = store.record_as("web", state("1.0", 1), keep_all, &WriteAsk::default(), None);
    assert_eq!(plain.expect("taken"), None);
    assert_eq!(std::fs::read(&path).expect("the file"), text);
    assert_eq!(store.all()["web"].version, version("1.0"));
}
