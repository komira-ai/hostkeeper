//! The package proxy cache's start: the limits of the settings, what a start deletes and keeps of
//! a directory a crash left, and the start `--proxy-cache-dir` makes.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::tests::{clock, digest, key, limits, names, open, scratch};
use super::{Limits, ProxyCache};

/// Proves: the limits are the settings that were set, and the defaults of `docs/design/package-
/// proxy.md` for those that were not or are not numbers.
///
/// Catches: a tunable that is read from nowhere, or a hand-edited value that stops the proxy.
#[test]
fn the_limits_are_the_proxy_settings_with_their_defaults() {
    assert_eq!(
        limits(),
        Limits {
            wait: Duration::from_secs(10),
            redirects: 3,
            index_ttl: Duration::from_secs(300),
            max_bytes: 20 << 30,
            fetches: 8,
        }
    );
    let set: BTreeMap<String, String> = [
        ("proxy_wait_s", "2"),
        ("proxy_redirects", "0"),
        ("proxy_index_ttl_s", "60"),
        ("proxy_cache_max_bytes", "1000"),
        ("proxy_fetches", "many"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    assert_eq!(
        Limits::from_settings(&set),
        Limits {
            wait: Duration::from_secs(2),
            redirects: 0,
            index_ttl: Duration::from_secs(60),
            max_bytes: 1000,
            fetches: 8,
        }
    );
}

/// Proves: a cache opened over what a crash left deletes the temp files and the partial objects,
/// keeps and counts the whole ones, and evicts the least recently fetched when the limit has
/// been lowered below what they hold; a file it does not know is left alone.
///
/// Catches: a partial file served after a crash (no file is synced, so one can be there).
#[test]
fn a_start_deletes_what_a_crash_leaves_and_keeps_the_rest() {
    let dir = scratch("pp1-start");
    let cache = open(dir.path());
    drop(cache);
    let objects = dir.path().join("objects");
    // A fill that was never committed.
    fs::write(dir.path().join("tmp").join("abc.0"), b"half").expect("a file");
    // Something in `tmp/` that cannot be deleted as a file is left, and does not stop the start.
    fs::create_dir(dir.path().join("tmp").join("stuck")).expect("a directory");
    // Whole objects of 4 bytes fetched at 100, 200 and 300; the sidecars are those of a fill.
    let commit = |path: &str, fetched_at: u64, body: &[u8]| {
        let layout = super::Layout::create(dir.path()).expect("a layout");
        let meta = super::Meta {
            validator: None,
            fetched_at,
            length: body.len() as u64,
        };
        let (tmp_body, tmp_meta) = (
            dir.path().join("tmp").join("b"),
            dir.path().join("tmp").join("m"),
        );
        fs::write(&tmp_body, body).expect("a body");
        layout
            .commit(&key(path), &meta, &tmp_body, &tmp_meta)
            .expect("committed");
    };
    commit("old", 100, b"1234");
    commit("mid", 200, b"1234");
    commit("new", 300, b"1234");
    // A body of the wrong length (a crash before the write ended).
    commit("cut", 400, b"1234");
    fs::write(objects.join(digest("cut")), b"12").expect("shortened");
    // A sidecar with no body, a body with no sidecar.
    commit("orphan-meta", 500, b"1234");
    fs::remove_file(objects.join(digest("orphan-meta"))).expect("removed");
    commit("orphan-body", 600, b"1234");
    fs::remove_file(objects.join(format!("{}.meta", digest("orphan-body")))).expect("removed");
    // A sidecar that is not JSON, one that is too large, and one that names another key.
    commit("junk", 700, b"1234");
    fs::write(
        objects.join(format!("{}.meta", digest("junk"))),
        b"not json",
    )
    .expect("junk");
    commit("huge", 800, b"1234");
    fs::write(
        objects.join(format!("{}.meta", digest("huge"))),
        vec![b' '; 70_000],
    )
    .expect("huge");
    commit("liar", 900, b"1234");
    fs::copy(
        objects.join(format!("{}.meta", digest("new"))),
        objects.join(format!("{}.meta", digest("liar"))),
    )
    .expect("a copy");
    // A body that is a directory, and a file of no name this cache gives.
    commit("dir", 950, b"1234");
    fs::remove_file(objects.join(digest("dir"))).expect("removed");
    fs::create_dir(objects.join(digest("dir"))).expect("a directory");
    fs::write(objects.join("README"), b"mine").expect("a file");

    let cache = open(dir.path());
    assert_eq!(names(dir.path(), "tmp"), ["stuck"]);
    assert_eq!(cache.bytes(), 12);
    let mut kept = names(dir.path(), "objects");
    kept.retain(|n| n != "README" && *n != digest("dir"));
    let mut want: Vec<String> = ["old", "mid", "new"]
        .iter()
        .flat_map(|p| [digest(p), format!("{}.meta", digest(p))])
        .collect();
    want.sort();
    assert_eq!(kept, want);
    assert!(objects.join("README").exists());
    drop(cache);

    // The limit is lowered to 9 bytes: the oldest fetch goes first, and nothing else.
    let tight = Limits {
        max_bytes: 9,
        ..limits()
    };
    let cache = ProxyCache::open(dir.path(), clock(), &tight).expect("a cache");
    assert_eq!(cache.bytes(), 8);
    assert_eq!(cache.stats().evictions, 1);
    assert!(!objects.join(digest("old")).exists());
    assert!(objects.join(digest("mid")).exists());
    assert!(objects.join(digest("new")).exists());
    drop(cache);

    // A limit of nothing evicts every object.
    let none = Limits {
        max_bytes: 0,
        ..limits()
    };
    let cache = ProxyCache::open(dir.path(), clock(), &none).expect("a cache");
    assert_eq!((cache.bytes(), cache.stats().evictions), (0, 2));
}

/// Proves: a directory that cannot be made is an error naming the cause and not a cache with no
/// directory.
///
/// Catches: a start that goes on with a cache that will fail every commit.
#[test]
fn a_cache_directory_that_cannot_be_made_is_an_error() {
    let dir = scratch("pp1-bad-dir");
    let file = dir.path().join("a-file");
    fs::write(&file, b"x").expect("a file");
    assert!(ProxyCache::open(&file.join("cache"), clock(), &limits()).is_err());
    // A directory whose `tmp` cannot be read.
    let base: PathBuf = dir.path().join("cache");
    fs::create_dir_all(&base).expect("a directory");
    fs::write(base.join("tmp"), b"x").expect("a file where tmp should be");
    assert!(ProxyCache::open(&base, clock(), &limits()).is_err());
}

/// A setup store whose settings cannot be read.
struct Unreadable;

impl crate::store::mdm_setup::SetupStore for Unreadable {
    fn put_secret(
        &self,
        _: &crate::store::mdm_setup::SecretRow,
    ) -> Result<(), crate::store::mdm_setup::SetupError> {
        Ok(())
    }

    fn secrets(
        &self,
    ) -> Result<Vec<crate::store::mdm_setup::SecretRow>, crate::store::mdm_setup::SetupError> {
        Ok(Vec::new())
    }

    fn prune_superseded(&self, _: i64) -> Result<u64, crate::store::mdm_setup::SetupError> {
        Ok(0)
    }

    fn settings(&self) -> Result<BTreeMap<String, String>, crate::store::mdm_setup::SetupError> {
        Err(crate::store::mdm_setup::SetupError::Store(
            "down".to_owned(),
        ))
    }

    fn set_setting(&self, _: &str, _: &str) -> Result<(), crate::store::mdm_setup::SetupError> {
        Ok(())
    }
}

/// Proves: at start there is no cache without the flag; with it the directory is made; a
/// directory that cannot be made refuses the start naming the flag, and so do settings that
/// cannot be read.
///
/// Catches: a flag that is accepted and ignored, or a start that goes on with no usable cache.
#[test]
fn the_start_opens_the_cache_the_flag_names_under_the_settings() {
    use crate::mdm_setup::MdmSetup;

    let setup = MdmSetup::volatile();
    assert!(
        super::open_at_start(None, &setup)
            .expect("no cache")
            .is_none()
    );

    let dir = scratch("pp1-start-flag");
    let at = dir.path().join("cache");
    let cache = super::open_at_start(Some(&at), &setup)
        .expect("a start")
        .expect("a cache");
    assert!(at.join("objects").is_dir() && at.join("tmp").is_dir());
    assert_eq!(cache.bytes(), 0);
    drop(cache);

    let blocked = dir.path().join("a-file");
    fs::write(&blocked, b"x").expect("a file");
    let refused = super::open_at_start(Some(&blocked.join("c")), &setup)
        .err()
        .expect("refused");
    assert!(
        refused
            .to_string()
            .starts_with(&format!("--proxy-cache-dir {}/c: ", blocked.display())),
        "{refused}"
    );

    let down = MdmSetup::new(Arc::new(Unreadable), None, crate::enrollment::os_random());
    let refused = super::open_at_start(Some(&at), &down)
        .err()
        .expect("refused");
    assert!(refused.to_string().contains("down"), "{refused}");
}
