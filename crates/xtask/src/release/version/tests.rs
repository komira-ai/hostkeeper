use super::*;
use crate::release::contract::BadValue;
use crate::release::fakes::{FakeHistory, nth_commit};
use crate::release::ports::PortError;

fn v(text: &str) -> Version {
    text.parse().expect("a version")
}

/// The version of `commit` in `h`, from the version file `VERSION`.
fn at(h: &FakeHistory, commit: &Commit) -> Result<Version, Error> {
    version(h, commit, "VERSION")
}

/// A root commit holding `VERSION` as `text` and one other file.
fn root(h: &mut FakeHistory, text: &str) -> Commit {
    h.commit(None, &[("VERSION", Some(text)), ("a.txt", Some("0"))])
}

/// A commit on `parent` that touches only a code file.
fn code(h: &mut FakeHistory, parent: &Commit, text: &str) -> Commit {
    h.commit(Some(parent), &[("a.txt", Some(text))])
}

/// Catches: a patch that depends on the tip of the history or on the jobs' state, so two jobs
/// (or a re-run after more commits landed) give one commit two versions.
#[test]
fn the_same_commit_gives_the_same_version_in_every_job_and_rerun() {
    let mut h = FakeHistory::default();
    let c1 = root(&mut h, "0.1\n");
    let c2 = code(&mut h, &c1, "1");
    let c3 = code(&mut h, &c2, "2");
    assert_eq!(at(&h, &c2), Ok(v("0.1.2")));
    assert_eq!(at(&h, &c2), Ok(v("0.1.2")));
    let c4 = h.commit(Some(&c3), &[("VERSION", Some("0.3\n"))]);
    let side = code(&mut h, &c1, "side");
    assert_eq!(at(&h, &c2), Ok(v("0.1.2")));
    assert_eq!(at(&h, &c3), Ok(v("0.1.3")));
    assert_eq!(at(&h, &c4), Ok(v("0.3.1")));
    assert_eq!(at(&h, &side), Ok(v("0.1.2")));
}

/// Catches: a base that is the root commit (the patch never restarts), or a count that follows
/// the merged side branch (a non-first-parent count gives 6 at the merge, not 4).
#[test]
fn changing_version_file_restarts_the_patch_at_1() {
    let mut h = FakeHistory::default();
    let c1 = root(&mut h, "0.1\n");
    let c2 = code(&mut h, &c1, "1");
    let s1 = h.commit(Some(&c2), &[("side.txt", Some("1"))]);
    let s2 = h.commit(Some(&s1), &[("side.txt", Some("2"))]);
    let c3 = code(&mut h, &c2, "3");
    let m = h.merge(&c3, &s2, &[]);
    let edit = h.commit(Some(&m), &[("VERSION", Some("0.2\n"))]);
    let next = code(&mut h, &edit, "after");
    assert_eq!(at(&h, &c1), Ok(v("0.1.1")));
    assert_eq!(at(&h, &c2), Ok(v("0.1.2")));
    assert_eq!(at(&h, &c3), Ok(v("0.1.3")));
    assert_eq!(at(&h, &m), Ok(v("0.1.4")));
    assert_eq!(at(&h, &edit), Ok(v("0.2.1")));
    assert_eq!(at(&h, &next), Ok(v("0.2.2")));
}

/// Catches: counting in a shallow clone, where the history stops early and the patch would be
/// too small.
#[test]
fn a_shallow_history_is_an_error() {
    let mut h = FakeHistory::default();
    let c1 = root(&mut h, "0.1\n");
    h.shallow = true;
    assert_eq!(at(&h, &c1), Err(Error::Failed(SHALLOW.to_owned())));
}

/// Catches: a version taken from a commit that has no version file.
#[test]
fn a_commit_without_the_version_file_is_an_error() {
    let mut h = FakeHistory::default();
    let c1 = h.commit(None, &[("a.txt", Some("0"))]);
    assert_eq!(
        at(&h, &c1),
        Err(Error::Failed(format!("VERSION does not exist at {c1}")))
    );
    assert_eq!(
        base_of(&h, &[c1.clone()], "VERSION"),
        Err(Error::Failed(format!("VERSION does not exist at {c1}")))
    );
}

/// Catches: a version file that is not `X.Y` being accepted or reported without its commit.
#[test]
fn a_malformed_version_file_is_an_error() {
    let mut h = FakeHistory::default();
    let c1 = root(&mut h, "0.1.2\n");
    assert_eq!(
        at(&h, &c1),
        Err(Error::Failed(format!(
            "VERSION at {c1}: {}",
            BadValue::Base("0.1.2\n".to_owned())
        )))
    );
}

/// Catches: a merged side branch that edits the version file not restarting the count, or the
/// count restarting at the side branch's own commit instead of the merge.
#[test]
fn a_merged_side_branch_that_edits_the_version_file_makes_the_merge_the_base() {
    let mut h = FakeHistory::default();
    let c1 = root(&mut h, "0.1\n");
    let side = h.commit(Some(&c1), &[("VERSION", Some("0.2\n"))]);
    let c2 = code(&mut h, &c1, "1");
    let c3 = code(&mut h, &c2, "2");
    let m = h.merge(&c3, &side, &[]);
    let next = code(&mut h, &m, "after");
    assert_eq!(at(&h, &c3), Ok(v("0.1.3")));
    assert_eq!(at(&h, &m), Ok(v("0.2.1")));
    assert_eq!(at(&h, &next), Ok(v("0.2.2")));
}

/// Catches: a text that returns to an earlier value being treated as unchanged, so the patch
/// keeps counting across the edit.
#[test]
fn text_a_then_b_then_a_restarts_each_time() {
    let mut h = FakeHistory::default();
    let c1 = root(&mut h, "0.1\n");
    let c2 = h.commit(Some(&c1), &[("VERSION", Some("0.2\n"))]);
    let c3 = h.commit(Some(&c2), &[("VERSION", Some("0.1\n"))]);
    assert_eq!(at(&h, &c1), Ok(v("0.1.1")));
    assert_eq!(at(&h, &c2), Ok(v("0.2.1")));
    assert_eq!(at(&h, &c3), Ok(v("0.1.1")));
}

/// Catches: a file that appears, or is deleted and added back, in a later commit not counting
/// that commit as its base.
#[test]
fn a_version_file_that_appears_or_returns_is_changed_at_that_commit() {
    let mut h = FakeHistory::default();
    let c1 = h.commit(None, &[("a.txt", Some("0"))]);
    let c2 = h.commit(Some(&c1), &[("VERSION", Some("0.1\n"))]);
    let c3 = code(&mut h, &c2, "1");
    let c4 = h.commit(Some(&c3), &[("VERSION", None)]);
    let c5 = h.commit(Some(&c4), &[("VERSION", Some("0.1\n"))]);
    let c6 = code(&mut h, &c5, "2");
    assert_eq!(at(&h, &c2), Ok(v("0.1.1")));
    assert_eq!(at(&h, &c3), Ok(v("0.1.2")));
    assert_eq!(at(&h, &c5), Ok(v("0.1.1")));
    assert_eq!(at(&h, &c6), Ok(v("0.1.2")));
}

/// Catches: a text without a trailing line break, or a version file at another path, being
/// read wrongly.
#[test]
fn the_version_file_is_read_from_the_given_path_with_or_without_a_line_break() {
    let mut h = FakeHistory::default();
    let c1 = h.commit(None, &[("etc/BASE", Some("3.4"))]);
    assert_eq!(version(&h, &c1, "etc/BASE"), Ok(v("3.4.1")));
}

/// Catches: an empty chain panicking instead of failing, and the index of the base being wrong
/// for the commit itself (index 0) or for a root deep in the chain.
#[test]
fn base_of_gives_the_index_of_the_commit_that_last_changed_the_file() {
    let mut h = FakeHistory::default();
    let c1 = root(&mut h, "0.1\n");
    let c2 = code(&mut h, &c1, "1");
    let c3 = h.commit(Some(&c2), &[("VERSION", Some("0.2\n"))]);
    let c4 = code(&mut h, &c3, "2");
    let chain = h.first_parents(&c4).expect("the chain");
    assert_eq!(base_of(&h, &chain, "VERSION"), Ok(1));
    assert_eq!(base_of(&h, &chain[1..], "VERSION"), Ok(0));
    assert_eq!(base_of(&h, &chain[2..], "VERSION"), Ok(1));
    assert_eq!(
        base_of(&h, &[], "VERSION"),
        Err(Error::Failed("no commit to count".to_owned()))
    );
    assert_eq!(
        version_in(&h, &[], "VERSION"),
        Err(Error::Failed("no commit to count".to_owned()))
    );
}

/// Catches: a port failure in the walk being swallowed or turned into a version, whether the
/// unknown commit is the one asked for or a parent in the chain.
#[test]
fn a_port_error_is_returned() {
    let mut h = FakeHistory::default();
    let c1 = root(&mut h, "0.1\n");
    let unknown = nth_commit(99);
    let error = Error::Port(PortError::new(format!("git: {unknown}"), "unknown commit"));
    assert_eq!(at(&h, &unknown), Err(error.clone()));
    assert_eq!(version_in(&h, &[c1, unknown], "VERSION"), Err(error));
}
