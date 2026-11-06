use std::cell::Cell;

use super::*;
use crate::release::fakes::{
    FakeHistory, FakeStore, FakeVerifier, Fakes, nth_commit, scratch, write_assets,
};
use crate::release::ports::{Adapters, PortError, ReleaseStore};
use crate::testdir::TestDir;

const TAG: &str = "v0.1.5";

fn version() -> Version {
    "0.1.5".parse().expect("a version")
}

fn repo() -> Repo {
    REPO.parse().expect("a repository")
}

fn names() -> Vec<String> {
    asset_names(&version(), &Platform::RELEASED)
}

/// Fakes whose verifier attests every file for the commit, and a directory of assets built
/// with `marker`.
fn setup(name: &str, marker: &str) -> (Fakes, TestDir, Commit) {
    let commit = nth_commit(7);
    let mut fakes = Fakes::over(FakeHistory::default());
    fakes.verifier = FakeVerifier::good(&commit);
    let dir = scratch(name);
    write_assets(&dir, &version(), marker);
    (fakes, dir, commit)
}

fn publish_to(fakes: &Fakes, dir: &Path, commit: &Commit) -> Result<Vec<String>, Error> {
    publish(&fakes.ports(), dir, &repo(), &version(), commit)
}

fn check_line(commit: &Commit) -> String {
    format!(
        "release check: the 60 assets of v0.1.5 match SHA256SUMS, and 42 attestations of commit \
         {commit} verified online and from their bundles"
    )
}

/// The writes of a whole publication: the draft, every upload in name order, the publication.
fn all_writes() -> Vec<String> {
    let mut writes = vec![format!("create {TAG}")];
    writes.extend(names().iter().map(|n| format!("upload {n}")));
    writes.push(format!("publish {TAG}"));
    writes
}

fn digest(dir: &Path, name: &str) -> String {
    file_sha256(&dir.join(name)).expect("hash")
}

/// What the store holds for every asset in `dir`.
fn assets_in(dir: &Path) -> Vec<StoredAsset> {
    names()
        .iter()
        .map(|n| StoredAsset {
            name: n.clone(),
            sha256: Some(digest(dir, n)),
        })
        .collect()
}

fn release(commit: &Commit, draft: bool, assets: Vec<StoredAsset>) -> Release {
    Release {
        tag: TAG.to_owned(),
        commit: commit.to_string(),
        draft,
        assets,
    }
}

fn failed(text: String) -> Result<Vec<String>, Error> {
    Err(Error::Failed(text))
}

#[test]
fn publish_is_idempotent_for_identical_assets() {
    // Catches: a second run that writes again, or fails, for a release already published with
    // the same assets.
    let (fakes, dir, commit) = setup("publish-idempotent", "x");
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        Ok(vec![
            check_line(&commit),
            format!("published {TAG} at {commit} with 60 assets"),
        ])
    );
    assert_eq!(*fakes.store.writes.borrow(), all_writes());
    let stored = fakes.store.all();
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].draft);
    assert_eq!(stored[0].assets, assets_in(&dir));

    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        Ok(vec![
            check_line(&commit),
            format!("release {TAG} is published with identical assets: nothing was changed"),
        ])
    );
    assert_eq!(*fakes.store.writes.borrow(), all_writes());
}

#[test]
fn publish_fails_for_a_different_asset_and_names_it() {
    // Catches: a rerun from other bytes that overwrites, or silently accepts, a published release.
    let (fakes, dir, commit) = setup("publish-different-a", "x");
    publish_to(&fakes, &dir, &commit).expect("first publication");
    let (_, other, _) = setup("publish-different-b", "y");
    let writes = fakes.store.writes.borrow().clone();
    assert_eq!(
        publish_to(&fakes, &other, &commit),
        failed(format!(
            "release {TAG} exists with a different SHA256SUMS: sha256 {}, not {}; nothing was \
             changed",
            digest(&dir, "SHA256SUMS"),
            digest(&other, "SHA256SUMS")
        ))
    );
    assert_eq!(*fakes.store.writes.borrow(), writes);
}

#[test]
fn a_partial_draft_is_completed_from_the_same_runs_assets() {
    // Catches: a rerun after a cut-short upload that creates a second draft, re-uploads assets
    // the draft already holds, or publishes an incomplete draft.
    let (fakes, dir, commit) = setup("publish-partial", "x");
    let all = names();
    fakes.store.uploads_left.set(Some(5));
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        Err(Error::Port(PortError::new(
            format!("upload {}", all[5]),
            "the run was cut short"
        )))
    );
    assert_eq!(fakes.store.all()[0].assets.len(), 5);
    assert!(fakes.store.all()[0].draft);

    fakes.store.uploads_left.set(None);
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        Ok(vec![
            check_line(&commit),
            format!("published {TAG} at {commit} with 60 assets"),
        ])
    );
    assert_eq!(*fakes.store.writes.borrow(), all_writes());
    assert_eq!(fakes.store.all().len(), 1);
    assert!(!fakes.store.all()[0].draft);
}

#[test]
fn a_draft_holding_an_asset_of_other_bytes_is_refused() {
    // Catches: a draft completed over an asset that is not this run's.
    let (fakes, dir, commit) = setup("publish-draft-bytes", "x");
    let mut assets = assets_in(&dir);
    assets[0].sha256 = Some("0".repeat(64));
    fakes.store.insert(release(&commit, true, assets));
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        failed(format!(
            "the draft {TAG} exists with a different SHA256SUMS: sha256 {}, not {}; nothing was \
             changed",
            "0".repeat(64),
            digest(&dir, "SHA256SUMS")
        ))
    );
    assert!(fakes.store.writes.borrow().is_empty());
}

#[test]
fn a_digest_the_store_does_not_give_reads_none() {
    // Catches: an asset without a store digest taken for a match.
    let (fakes, dir, commit) = setup("publish-draft-none", "x");
    let mut assets = assets_in(&dir);
    assets[0].sha256 = None;
    fakes.store.insert(release(&commit, true, assets));
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        failed(format!(
            "the draft {TAG} exists with a different SHA256SUMS: sha256 none, not {}; nothing was \
             changed",
            digest(&dir, "SHA256SUMS")
        ))
    );
}

#[test]
fn a_draft_holding_a_foreign_asset_is_refused() {
    // Catches: a draft with an asset outside the version's list published.
    let (fakes, dir, commit) = setup("publish-draft-foreign", "x");
    let stray = StoredAsset {
        name: "stray.txt".to_owned(),
        sha256: None,
    };
    fakes.store.insert(release(&commit, true, vec![stray]));
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        failed(format!(
            "the draft {TAG} holds stray.txt, which is not an asset of {TAG}; nothing was changed"
        ))
    );
    assert!(fakes.store.writes.borrow().is_empty());
}

#[test]
fn a_release_or_draft_at_another_commit_is_refused() {
    // Catches: a version already cut from another commit being published again.
    for (draft, what) in [(false, "release"), (true, "the draft")] {
        let (fakes, dir, commit) = setup("publish-other-commit", "x");
        let other = nth_commit(9);
        fakes.store.insert(release(&other, draft, assets_in(&dir)));
        assert_eq!(
            publish_to(&fakes, &dir, &commit),
            failed(format!(
                "{what} {TAG} exists at {other}, not {commit}: nothing was changed"
            ))
        );
        assert!(fakes.store.writes.borrow().is_empty());
    }
}

#[test]
fn a_published_release_with_a_missing_or_foreign_asset_is_refused() {
    // Catches: a published release lacking an asset, or holding a stray one, taken for equal.
    let (fakes, dir, commit) = setup("publish-published-shape", "x");
    let mut lacking = assets_in(&dir);
    lacking.remove(0);
    fakes.store.insert(release(&commit, false, lacking));
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        failed(format!(
            "release {TAG} exists without SHA256SUMS; nothing was changed"
        ))
    );

    let (fakes, dir, commit) = setup("publish-published-foreign", "x");
    let mut extra = assets_in(&dir);
    extra.push(StoredAsset {
        name: "stray.txt".to_owned(),
        sha256: None,
    });
    fakes.store.insert(release(&commit, false, extra));
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        failed(format!(
            "release {TAG} holds stray.txt, which is not an asset of {TAG}; nothing was changed"
        ))
    );
    assert!(fakes.store.writes.borrow().is_empty());
}

#[test]
fn two_releases_with_one_tag_are_refused() {
    // Catches: a publication that picks one of two releases of the same tag.
    let (fakes, dir, commit) = setup("publish-two", "x");
    fakes.store.insert(release(&commit, true, Vec::new()));
    fakes.store.insert(release(&commit, false, assets_in(&dir)));
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        failed("2 releases have the tag v0.1.5: nothing was changed".to_owned())
    );
    assert!(fakes.store.writes.borrow().is_empty());
}

#[test]
fn a_release_of_another_tag_does_not_matter() {
    // Catches: a lookup that takes any release for the one of this version.
    let (fakes, dir, commit) = setup("publish-other-tag", "x");
    let mut older = release(&nth_commit(1), false, Vec::new());
    older.tag = "v0.1.4".to_owned();
    fakes.store.insert(older);
    assert_eq!(
        publish_to(&fakes, &dir, &commit).map(|lines| lines.len()),
        Ok(2)
    );
    assert_eq!(*fakes.store.writes.borrow(), all_writes());
}

/// A store that reads like `inner`, except that after its first read the draft it reads is
/// altered by `tamper`: what a read back that differs from what was uploaded looks like.
struct Misreading {
    inner: FakeStore,
    reads: Cell<u32>,
    tamper: fn(&mut Release),
}

impl ReleaseStore for Misreading {
    fn releases(&self, repo: &Repo) -> Result<Vec<Release>, PortError> {
        let mut all = self.inner.releases(repo)?;
        if self.reads.replace(self.reads.get() + 1) > 0 {
            all.iter_mut().for_each(self.tamper);
        }
        Ok(all)
    }

    fn create_draft(
        &self,
        repo: &Repo,
        tag: &str,
        commit: &Commit,
        notes: &str,
    ) -> Result<(), PortError> {
        self.inner.create_draft(repo, tag, commit, notes)
    }

    fn upload(&self, repo: &Repo, tag: &str, file: &Path) -> Result<(), PortError> {
        self.inner.upload(repo, tag, file)
    }

    fn publish(&self, repo: &Repo, tag: &str) -> Result<(), PortError> {
        self.inner.publish(repo, tag)
    }
}

/// Publishes into a store whose read back is altered by `tamper`, and asserts the refusal that
/// names `detail` (`SUMS_DIGEST` stands for the digest of SHA256SUMS) and that nothing is
/// published.
fn misread(tamper: fn(&mut Release), detail: &str) {
    let (fakes, dir, commit) = setup("publish-misread", "x");
    let store = Misreading {
        inner: FakeStore::default(),
        reads: Cell::new(0),
        tamper,
    };
    let ports = Ports {
        git: &fakes.git,
        runs: &fakes.runs,
        store: &store,
        source: &fakes.source,
        verifier: &fakes.verifier,
        run_version: &fakes.run_version,
        pkgbuild: &fakes.pkgbuild,
    };
    let detail = detail.replace("SUMS_DIGEST", &digest(&dir, "SHA256SUMS"));
    assert_eq!(
        publish(&ports, &dir, &repo(), &version(), &commit),
        failed(format!(
            "the draft {TAG} holds other assets than the checked ones ({detail}); it was not \
             published"
        ))
    );
    let mut writes = all_writes();
    writes.pop();
    assert_eq!(*store.inner.writes.borrow(), writes);
}

#[test]
fn a_read_back_that_differs_from_the_checked_assets_is_not_published() {
    // Catches: a draft published without reading back that it holds exactly the checked assets.
    misread(
        |r| {
            r.assets.remove(0);
        },
        "SHA256SUMS is missing",
    );
    misread(
        |r| r.assets[0].sha256 = None,
        "SHA256SUMS has sha256 none, not SUMS_DIGEST",
    );
    misread(
        |r| {
            r.assets.push(StoredAsset {
                name: "stray.txt".to_owned(),
                sha256: None,
            });
        },
        "stray.txt is not a checked asset",
    );
}

#[test]
fn a_failing_directory_publishes_nothing() {
    // Catches: a publication that goes ahead when the directory does not pass the check.
    let (fakes, dir, commit) = setup("publish-failing", "x");
    std::fs::remove_file(dir.join("SHA256SUMS")).expect("remove");
    assert_eq!(
        publish_to(&fakes, &dir, &commit),
        Err(Error::Refused(vec!["missing SHA256SUMS".to_owned()]))
    );
    assert!(fakes.store.writes.borrow().is_empty());
    assert!(fakes.store.all().is_empty());

    let empty = scratch("publish-empty");
    assert!(matches!(
        publish_to(&fakes, &empty, &commit),
        Err(Error::Refused(_))
    ));
    assert!(fakes.store.writes.borrow().is_empty());
}

#[test]
fn a_directory_that_fails_the_attestation_check_publishes_nothing() {
    // Catches: a publication of assets whose attestations do not verify.
    let (mut fakes, dir, commit) = setup("publish-unattested", "x");
    fakes.verifier = Fakes::over(FakeHistory::default()).verifier;
    let result = publish_to(&fakes, &dir, &commit);
    assert_eq!(
        result.map_err(|e| matches!(e, Error::Refused(lines) if lines.len() == 42)),
        Err(true)
    );
    assert!(fakes.store.writes.borrow().is_empty());
}

#[test]
fn the_notes_name_the_release_the_commit_the_signer_and_the_verify_command() {
    // Catches: release notes that omit the pin a reader needs to verify a file.
    let commit = nth_commit(7);
    assert_eq!(
        notes(&version(), &commit),
        format!(
            "hostkeeper v0.1.5, built from commit {commit}. Each binary was built with \
             RELEASE_COMMIT set to that commit and prints `hostkeeper 0.1.5+{}` for --version.\n\
             \n\
             The assets were built and signed by .github/workflows/artifacts.yml on main. To \
             verify a file:\n\
             \n\
             gh attestation verify <file> --repo komira-ai/hostkeeper --signer-workflow \
             komira-ai/hostkeeper/.github/workflows/artifacts.yml --source-ref refs/heads/main \
             --source-digest {commit}\n",
            commit.short()
        )
    );
}
