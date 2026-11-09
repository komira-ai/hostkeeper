//! The fakes the release tests run the verbs against: a git history held in memory, GitHub runs
//! and issues, a release store, a source of published releases and their asset bytes, a
//! verifier, a binary's `--version`, `pkgbuild`, the advisory check, downloads and a scripted
//! [`Exec`]. Each records the writes it was asked for, so a test can assert that nothing was
//! written.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::Path;

use super::contract::{
    Commit, DIGEST_SUFFIX, Package, Platform, Repo, SIGNER_REF, SIGNER_WORKFLOW, SUMS, Signer,
    Version, attested_names, bundle_of, hkctl_name, summed_names,
};
use super::file_sha256;
use super::image_index;
use super::index::ReleaseIndex;
use super::ports::{
    Adapters, AdvisoryCheck, Attestation, BuildPkg, Download, Exec, GitHistory, GitHubRuns, Issue,
    PortError, Ports, Ran, Release, ReleaseSource, ReleaseStore, Run, RunVersion, Said,
    StoredAsset, Verifier,
};
use crate::testdir::TestDir;
use std::path::PathBuf;

/// An empty directory of its own for one test, removed when it drops.
pub(crate) fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-release-unit"),
        name,
    )
}

/// Writes into `dir` every asset of the release of `version`, as a run of the artifacts workflow
/// leaves them: each file holds its own name and `marker`, the `.sha256` of each package holds
/// the package's digest, `SHA256SUMS` lists every summed asset and each bundle holds `{}`.
pub(crate) fn write_assets(dir: &Path, version: &Version, marker: &str) {
    let write = |name: &str, text: String| {
        std::fs::write(dir.join(name), text).expect("write an asset");
    };
    let summed = summed_names(version);
    for name in &summed {
        if name.ends_with(DIGEST_SUFFIX) | Package::ALL.iter().any(|p| name == p.index_name()) {
            continue;
        }
        if *name == image_index::index_name(version) {
            continue;
        }
        if let Some((_, arch)) = [
            (Platform::LinuxX86_64, "amd64"),
            (Platform::LinuxArm64, "arm64"),
        ]
        .into_iter()
        .find(|(p, _)| *name == image_index::image_name(version, *p))
        {
            std::fs::write(dir.join(name), image_index::fake_image(arch, marker))
                .expect("write an image");
            continue;
        }
        write(name, format!("{name} {marker}\n"));
    }
    let image = image_index::index_bytes(dir, version).expect("the image index");
    std::fs::write(dir.join(image_index::index_name(version)), image).expect("write the index");
    // The command line passes `release check` as the script `release hkctl` writes, stamped with
    // the 12 digits every commit of [`nth_commit`] below 2^32 starts with.
    write(
        &hkctl_name(version),
        format!("#!/usr/bin/env bash\n# {marker}\n_hk_version='hkctl {version}+000000000000'\n"),
    );
    for package in Package::ALL {
        let (service, text) = (package.service().name(), package.version_text(version));
        let index = ReleaseIndex::of_dir(dir, service, &text, package.platforms())
            .expect("index the packages");
        write(package.index_name(), index.to_json());
    }
    for name in &summed {
        if let Some(package) = name.strip_suffix(DIGEST_SUFFIX) {
            let digest = file_sha256(&dir.join(package)).expect("hash the package");
            write(name, format!("{digest}  {package}\n"));
        }
    }
    let mut sums = String::new();
    for name in &summed {
        let digest = file_sha256(&dir.join(name)).expect("hash an asset");
        sums.push_str(&format!("{digest}  {name}\n"));
    }
    write(SUMS, sums);
    for name in attested_names(version) {
        write(&bundle_of(&name), "{}\n".to_owned());
    }
}

/// The commit numbered `n` of a fake history: `n` in hex, 40 digits.
pub(crate) fn nth_commit(n: u64) -> Commit {
    format!("{n:040x}").parse().expect("a commit")
}

/// The committer time of the commit numbered `n`.
pub(crate) fn time_of(n: u64) -> u64 {
    1_800_000_000 + 60 * n
}

struct FakeCommit {
    parent: Option<Commit>,
    tree: BTreeMap<String, String>,
    time: u64,
}

/// A git history in memory. Commits are numbered from 1 in the order they are made
/// ([`nth_commit`]); each has a first parent (none for a root) and a tree of files.
#[derive(Default)]
pub(crate) struct FakeHistory {
    commits: BTreeMap<Commit, FakeCommit>,
    /// What `is_shallow` answers.
    pub(crate) shallow: bool,
}

/// One change to a tree: the path, and its new text or none to delete it.
pub(crate) type Change<'a> = (&'a str, Option<&'a str>);

impl FakeHistory {
    /// A commit on `parent` (none: a root commit) whose tree is the parent's with `changes`.
    pub(crate) fn commit(&mut self, parent: Option<&Commit>, changes: &[Change<'_>]) -> Commit {
        let tree = parent.map_or_else(BTreeMap::new, |p| self.commits[p].tree.clone());
        self.make(parent, tree, changes)
    }

    /// A merge commit: first parent `first`, its tree that of `first` with every file of `side`
    /// (the tip of the merged branch) and then `changes` applied.
    pub(crate) fn merge(
        &mut self,
        first: &Commit,
        side: &Commit,
        changes: &[Change<'_>],
    ) -> Commit {
        let mut tree = self.commits[first].tree.clone();
        tree.extend(self.commits[side].tree.clone());
        self.make(Some(first), tree, changes)
    }

    fn make(
        &mut self,
        parent: Option<&Commit>,
        mut tree: BTreeMap<String, String>,
        changes: &[Change<'_>],
    ) -> Commit {
        for (path, text) in changes {
            match text {
                Some(text) => tree.insert((*path).to_owned(), (*text).to_owned()),
                None => tree.remove(*path),
            };
        }
        let n = self.commits.len() as u64 + 1;
        let commit = nth_commit(n);
        self.commits.insert(
            commit.clone(),
            FakeCommit {
                parent: parent.cloned(),
                tree,
                time: time_of(n),
            },
        );
        commit
    }

    fn get(&self, commit: &Commit) -> Result<&FakeCommit, PortError> {
        self.commits
            .get(commit)
            .ok_or_else(|| PortError::new(format!("git: {commit}"), "unknown commit"))
    }
}

impl GitHistory for FakeHistory {
    fn is_shallow(&self) -> Result<bool, PortError> {
        Ok(self.shallow)
    }

    fn first_parents(&self, commit: &Commit) -> Result<Vec<Commit>, PortError> {
        let mut chain = vec![commit.clone()];
        let mut at = self.get(commit)?;
        while let Some(parent) = &at.parent {
            chain.push(parent.clone());
            at = self.get(parent)?;
        }
        Ok(chain)
    }

    fn file_at(&self, commit: &Commit, path: &str) -> Result<Option<String>, PortError> {
        Ok(self.get(commit)?.tree.get(path).cloned())
    }

    fn changed_paths(
        &self,
        commit: &Commit,
        parent: Option<&Commit>,
    ) -> Result<Vec<String>, PortError> {
        let tree = &self.get(commit)?.tree;
        let empty = BTreeMap::new();
        let parent = parent.map(|p| self.get(p)).transpose()?;
        let before = parent.map_or(&empty, |p| &p.tree);
        let mut paths: Vec<String> = tree
            .iter()
            .filter(|(path, text)| before.get(*path) != Some(*text))
            .map(|(path, _)| path.clone())
            .collect();
        paths.extend(before.keys().filter(|p| !tree.contains_key(*p)).cloned());
        paths.sort();
        Ok(paths)
    }

    fn commit_time(&self, commit: &Commit) -> Result<u64, PortError> {
        Ok(self.get(commit)?.time)
    }
}

/// GitHub's runs and issues, in memory.
#[derive(Default)]
pub(crate) struct FakeRuns {
    /// Each run with the workflow file it is a run of.
    runs: RefCell<Vec<(String, Run)>>,
    /// The commits `on_main` says are on `main`.
    pub(crate) on_main: RefCell<Vec<Commit>>,
    issues: RefCell<Vec<Issue>>,
    /// The writes asked for, in order: `open #<n>`, `edit #<n>` and `close #<n>: <comment>`.
    pub(crate) writes: RefCell<Vec<String>>,
}

impl FakeRuns {
    /// Adds a run of `workflow` for `commit` and returns its id; ids grow in the order runs
    /// are added.
    pub(crate) fn add(
        &self,
        workflow: &str,
        commit: &Commit,
        event: &str,
        branch: &str,
        conclusion: Option<&str>,
    ) -> u64 {
        let mut runs = self.runs.borrow_mut();
        let id = 1000 + runs.len() as u64;
        runs.push((
            workflow.to_owned(),
            Run {
                id,
                event: event.to_owned(),
                branch: branch.to_owned(),
                head_sha: commit.to_string(),
                status: if conclusion.is_some() {
                    "completed"
                } else {
                    "in_progress"
                }
                .to_owned(),
                conclusion: conclusion.map(str::to_owned),
            },
        ));
        id
    }

    /// Adds a completed push run of `workflow` on `main` for `commit` that ended `conclusion`.
    pub(crate) fn push(&self, workflow: &str, commit: &Commit, conclusion: &str) -> u64 {
        self.add(workflow, commit, "push", "main", Some(conclusion))
    }

    /// The open issues.
    pub(crate) fn issues(&self) -> Vec<Issue> {
        self.issues.borrow().clone()
    }
}

impl GitHubRuns for FakeRuns {
    fn runs(&self, _repo: &Repo, workflow: &str, _commit: &Commit) -> Result<Vec<Run>, PortError> {
        // Every run of the workflow, whatever its head: the caller picks the ones that count.
        Ok(self
            .runs
            .borrow()
            .iter()
            .filter(|(w, _)| w == workflow)
            .map(|(_, r)| r.clone())
            .collect())
    }

    fn on_main(&self, _repo: &Repo, commit: &Commit) -> Result<bool, PortError> {
        Ok(self.on_main.borrow().contains(commit))
    }

    fn open_issues(&self, _repo: &Repo) -> Result<Vec<Issue>, PortError> {
        Ok(self.issues())
    }

    fn open_issue(&self, _repo: &Repo, title: &str, body: &str) -> Result<u64, PortError> {
        let mut issues = self.issues.borrow_mut();
        let number = issues.len() as u64 + 1;
        issues.push(Issue {
            number,
            title: title.to_owned(),
            body: body.to_owned(),
        });
        self.writes.borrow_mut().push(format!("open #{number}"));
        Ok(number)
    }

    fn edit_issue(&self, _repo: &Repo, number: u64, body: &str) -> Result<(), PortError> {
        self.issues
            .borrow_mut()
            .iter_mut()
            .filter(|issue| issue.number == number)
            .for_each(|issue| issue.body = body.to_owned());
        self.writes.borrow_mut().push(format!("edit #{number}"));
        Ok(())
    }

    /// The closed issue is no longer listed as open.
    fn close_issue(&self, _repo: &Repo, number: u64, comment: &str) -> Result<(), PortError> {
        self.issues
            .borrow_mut()
            .retain(|issue| issue.number != number);
        self.writes
            .borrow_mut()
            .push(format!("close #{number}: {comment}"));
        Ok(())
    }
}

/// GitHub releases, in memory. An upload records the SHA-256 of the file's bytes, as GitHub
/// does.
#[derive(Default)]
pub(crate) struct FakeStore {
    releases: RefCell<Vec<Release>>,
    /// The writes asked for, in order: `create <tag>`, `upload <name>`, `publish <tag>`.
    pub(crate) writes: RefCell<Vec<String>>,
    /// When set, uploads fail once this many have succeeded (a run cut short).
    pub(crate) uploads_left: Cell<Option<usize>>,
    /// The notes of each draft created, in order.
    pub(crate) notes: RefCell<Vec<String>>,
}

impl FakeStore {
    /// Adds a release as it stands, without recording a write.
    pub(crate) fn insert(&self, release: Release) {
        self.releases.borrow_mut().push(release);
    }

    /// The releases.
    pub(crate) fn all(&self) -> Vec<Release> {
        self.releases.borrow().clone()
    }

    fn write(&self, what: String) {
        self.writes.borrow_mut().push(what);
    }

    /// Applies `edit` to the draft `tag`.
    fn on_draft(&self, tag: &str, edit: impl FnOnce(&mut Release)) -> Result<(), PortError> {
        let mut releases = self.releases.borrow_mut();
        let draft = releases
            .iter_mut()
            .find(|r| (r.tag == tag) & r.draft)
            .expect("the verb uploads to and publishes only a draft it saw");
        edit(draft);
        Ok(())
    }
}

impl ReleaseStore for FakeStore {
    fn releases(&self, _repo: &Repo) -> Result<Vec<Release>, PortError> {
        Ok(self.all())
    }

    fn create_draft(
        &self,
        _repo: &Repo,
        tag: &str,
        commit: &Commit,
        notes: &str,
    ) -> Result<(), PortError> {
        self.write(format!("create {tag}"));
        self.notes.borrow_mut().push(notes.to_owned());
        self.insert(Release {
            tag: tag.to_owned(),
            commit: commit.to_string(),
            draft: true,
            assets: Vec::new(),
        });
        Ok(())
    }

    fn upload(&self, _repo: &Repo, tag: &str, file: &Path) -> Result<(), PortError> {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(left) = self.uploads_left.get() {
            if left == 0 {
                return Err(PortError::new(
                    format!("upload {name}"),
                    "the run was cut short",
                ));
            }
            self.uploads_left.set(Some(left - 1));
        }
        let sha256 = file_sha256(file).expect("hash the uploaded file");
        self.write(format!("upload {name}"));
        self.on_draft(tag, |r| {
            r.assets.push(StoredAsset {
                name,
                sha256: Some(sha256),
            });
        })
    }

    fn publish(&self, _repo: &Repo, tag: &str) -> Result<(), PortError> {
        self.write(format!("publish {tag}"));
        self.on_draft(tag, |r| r.draft = false)
    }
}

/// The published releases of each repository and the bytes of their assets, in memory. A
/// download writes the bytes to the directory it is given; it fails, writing nothing, once
/// `downloads_left` downloads have succeeded (a transfer cut short), and for a release, asset or
/// repository the source does not hold. Each download is recorded as `<repo> <tag> <name>`.
#[derive(Default)]
pub(crate) struct FakeSource {
    releases: RefCell<BTreeMap<String, Vec<Release>>>,
    bytes: RefCell<BTreeMap<(String, String, String), Vec<u8>>>,
    /// When set, downloads fail once this many have succeeded.
    pub(crate) downloads_left: Cell<Option<usize>>,
    pub(crate) downloads: RefCell<Vec<String>>,
}

impl FakeSource {
    /// Publishes the release `tag` of `repo` at `commit` with `assets`, each a name and its
    /// bytes.
    pub(crate) fn publish(
        &self,
        repo: &str,
        tag: &str,
        commit: &str,
        assets: Vec<(String, Vec<u8>)>,
    ) {
        self.add(repo, tag, commit, false, assets);
    }

    /// Adds a release, a draft when `draft` is set.
    pub(crate) fn add(
        &self,
        repo: &str,
        tag: &str,
        commit: &str,
        draft: bool,
        assets: Vec<(String, Vec<u8>)>,
    ) {
        let mut stored = Vec::new();
        for (name, bytes) in assets {
            stored.push(StoredAsset {
                name: name.clone(),
                sha256: None,
            });
            self.bytes
                .borrow_mut()
                .insert((repo.to_owned(), tag.to_owned(), name), bytes);
        }
        self.releases
            .borrow_mut()
            .entry(repo.to_owned())
            .or_default()
            .push(Release {
                tag: tag.to_owned(),
                commit: commit.to_owned(),
                draft,
                assets: stored,
            });
    }
}

impl ReleaseSource for FakeSource {
    fn list(&self, repo: &Repo) -> Result<Vec<Release>, PortError> {
        self.releases
            .borrow()
            .get(repo.as_str())
            .cloned()
            .ok_or_else(|| PortError::new(format!("list the releases of {repo}"), "HTTP 404"))
    }

    fn download(&self, repo: &Repo, tag: &str, name: &str, dir: &Path) -> Result<(), PortError> {
        let what = format!("download {name} of {repo} {tag}");
        if let Some(left) = self.downloads_left.get() {
            if left == 0 {
                return Err(PortError::new(what, "the connection was reset"));
            }
            self.downloads_left.set(Some(left - 1));
        }
        let key = (repo.as_str().to_owned(), tag.to_owned(), name.to_owned());
        let bytes = self.bytes.borrow().get(&key).cloned();
        let bytes = bytes.ok_or_else(|| PortError::new(what, "no such asset"))?;
        let path = dir.join(name);
        assert!(
            !path.exists(),
            "a download never overwrites: {}",
            path.display()
        );
        std::fs::write(&path, bytes).expect("write a downloaded asset");
        self.downloads
            .borrow_mut()
            .push(format!("{repo} {tag} {name}"));
        Ok(())
    }
}

/// What a fake verifier answers for one file and bundle.
pub(crate) type Answer = Box<dyn Fn(&Path, Option<&Path>) -> Result<Vec<Attestation>, PortError>>;

/// A verifier that answers through a function of the file and the bundle, and records each call
/// as `online <name>` or `offline <name> <bundle name>`, and the signer's workflow it was asked
/// for in `signers`.
pub(crate) struct FakeVerifier {
    answer: Answer,
    /// Set by [`FakeVerifier::attests_as_asked`].
    as_asked: bool,
    pub(crate) calls: RefCell<Vec<String>>,
    pub(crate) signers: RefCell<Vec<String>>,
}

/// The attestation the signer of this repository makes of the file at `file` built from
/// `commit`.
pub(crate) fn good_attestation(file: &Path, commit: &str) -> Attestation {
    Attestation {
        signer_workflow: SIGNER_WORKFLOW.to_owned(),
        source_ref: SIGNER_REF.to_owned(),
        source_commit: commit.to_owned(),
        subjects: vec![file_sha256(file).expect("hash the attested file")],
    }
}

impl FakeVerifier {
    /// A verifier that answers `answer`.
    pub(crate) fn with(answer: Answer) -> FakeVerifier {
        FakeVerifier {
            answer,
            as_asked: false,
            calls: RefCell::new(Vec::new()),
            signers: RefCell::new(Vec::new()),
        }
    }

    /// A verifier for which every file is attested, online and in its bundle, by the signer the
    /// caller asks for and for the commit it asks about: each attestation names the file's
    /// SHA-256. `answer` is not called.
    pub(crate) fn attests_as_asked() -> FakeVerifier {
        FakeVerifier {
            as_asked: true,
            ..FakeVerifier::with(Box::new(|_, _| Ok(Vec::new())))
        }
    }

    /// A verifier for which every file is attested by this repository's signer for `commit`.
    pub(crate) fn good(commit: &Commit) -> FakeVerifier {
        let commit = commit.to_string();
        FakeVerifier::with(Box::new(move |file, _| {
            Ok(vec![good_attestation(file, &commit)])
        }))
    }
}

/// The file name of `path`, as text.
pub(crate) fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Verifier for FakeVerifier {
    fn verify(
        &self,
        signer: &Signer,
        file: &Path,
        bundle: Option<&Path>,
        commit: &Commit,
    ) -> Result<Vec<Attestation>, PortError> {
        let call = match bundle {
            Some(b) => format!("offline {} {}", name_of(file), name_of(b)),
            None => format!("online {}", name_of(file)),
        };
        self.calls.borrow_mut().push(call);
        self.signers.borrow_mut().push(signer.workflow.clone());
        if self.as_asked {
            return Ok(vec![Attestation {
                signer_workflow: signer.workflow.clone(),
                source_ref: signer.signers.refs[0].clone(),
                source_commit: commit.to_string(),
                subjects: vec![file_sha256(file).expect("hash the attested file")],
            }]);
        }
        (self.answer)(file, bundle)
    }
}

/// A binary whose `--version` says `said`.
pub(crate) struct FakeRunVersion {
    pub(crate) said: Said,
}

impl RunVersion for FakeRunVersion {
    fn version_of(&self, _binary: &Path) -> Result<Said, PortError> {
        Ok(self.said.clone())
    }
}

/// An advisory check that prints `said`.
pub(crate) struct FakeAdvisories {
    pub(crate) said: Said,
}

impl AdvisoryCheck for FakeAdvisories {
    fn check_advisories(&self) -> Result<Said, PortError> {
        Ok(self.said.clone())
    }
}

/// One call of [`FakePkgbuild`]: the tree, the identifier, the version and the pkg asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PkgCall {
    pub(crate) root: PathBuf,
    pub(crate) scripts: Option<PathBuf>,
    pub(crate) identifier: String,
    pub(crate) version: String,
    pub(crate) pkg: PathBuf,
}

/// One call of [`FakePkgbuild::build_product`]: the component, the identifier, the version and
/// the product asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProductCall {
    pub(crate) component: PathBuf,
    pub(crate) identifier: String,
    pub(crate) version: String,
    pub(crate) product: PathBuf,
}

/// A `pkgbuild` that writes [`FakePkgbuild::BYTES`] as the component pkg and a `productbuild`
/// that writes [`FakePkgbuild::PRODUCT_BYTES`] as the product, or fails with `fail` (the
/// component) or `fail_product` (the product) when set. Each call is recorded, the component's
/// with the tree as it was then (every path under the root, with `/` after a directory's,
/// sorted).
#[derive(Default)]
pub(crate) struct FakePkgbuild {
    pub(crate) fail: Option<PortError>,
    pub(crate) fail_product: Option<PortError>,
    pub(crate) calls: RefCell<Vec<PkgCall>>,
    pub(crate) product_calls: RefCell<Vec<ProductCall>>,
    pub(crate) trees: RefCell<Vec<Vec<String>>>,
    /// What `check_signature` answers: the report `pkgutil` prints, or its failure.
    pub(crate) signature: Option<Result<String, PortError>>,
    pub(crate) signature_calls: RefCell<Vec<PathBuf>>,
}

impl FakePkgbuild {
    /// What the fake writes as the component pkg.
    pub(crate) const BYTES: &'static [u8] = b"a component pkg";

    /// What the fake writes as the product pkg.
    pub(crate) const PRODUCT_BYTES: &'static [u8] = b"a product pkg";
}

/// Every path under `root`, relative to it, a directory's with a `/` after it, sorted.
pub(crate) fn tree_of(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read a directory") {
            let path = entry.expect("an entry").path();
            let name = path
                .strip_prefix(root)
                .expect("under the root")
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.push(format!("{name}/"));
                walk(root, &path, out);
            } else {
                out.push(name);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

impl BuildPkg for FakePkgbuild {
    fn build_pkg(
        &self,
        root: &Path,
        scripts: Option<&Path>,
        identifier: &str,
        version: &str,
        pkg: &Path,
    ) -> Result<(), PortError> {
        self.calls.borrow_mut().push(PkgCall {
            root: root.to_owned(),
            scripts: scripts.map(Path::to_owned),
            identifier: identifier.to_owned(),
            version: version.to_owned(),
            pkg: pkg.to_owned(),
        });
        self.trees.borrow_mut().push(tree_of(root));
        match &self.fail {
            Some(e) => Err(e.clone()),
            None => {
                std::fs::write(pkg, Self::BYTES).expect("write the pkg");
                Ok(())
            }
        }
    }

    fn build_product(
        &self,
        component: &Path,
        identifier: &str,
        version: &str,
        product: &Path,
    ) -> Result<(), PortError> {
        self.product_calls.borrow_mut().push(ProductCall {
            component: component.to_owned(),
            identifier: identifier.to_owned(),
            version: version.to_owned(),
            product: product.to_owned(),
        });
        match &self.fail_product {
            Some(e) => Err(e.clone()),
            None => {
                std::fs::write(product, Self::PRODUCT_BYTES).expect("write the product");
                Ok(())
            }
        }
    }

    fn check_signature(&self, pkg: &Path) -> Result<String, PortError> {
        self.signature_calls.borrow_mut().push(pkg.to_owned());
        self.signature
            .clone()
            .unwrap_or_else(|| Err(PortError::new("pkgutil", "not scripted")))
    }
}

/// An [`Exec`] that answers from a script: each entry is a command line (the program, then the
/// arguments) and what it prints. A command not in the script fails to start. Each call is
/// recorded.
#[derive(Default)]
pub(crate) struct FakeExec {
    script: Vec<(Vec<String>, Ran)>,
    pub(crate) calls: RefCell<Vec<Vec<String>>>,
}

impl FakeExec {
    /// Scripts `line` (program and arguments) to end `success` printing `stdout` and `stderr`.
    pub(crate) fn on(
        mut self,
        line: &[&str],
        success: bool,
        stdout: &str,
        stderr: &str,
    ) -> FakeExec {
        self.script.push((
            line.iter().map(|s| (*s).to_owned()).collect(),
            Ran {
                success,
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            },
        ));
        self
    }
}

impl Exec for FakeExec {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        let mut line = vec![program.to_owned()];
        line.extend(args.iter().cloned());
        self.calls.borrow_mut().push(line.clone());
        self.script
            .iter()
            .find(|(l, _)| *l == line)
            .map(|(_, ran)| ran.clone())
            .ok_or_else(|| std::io::Error::other(format!("not scripted: {}", line.join(" "))))
    }
}

/// A [`Download`] that answers from a map of URL to bytes: a URL it holds is written to the
/// file, any other fails. Each call is recorded as `(url, file)`.
#[derive(Default)]
pub(crate) struct FakeDownload {
    pub(crate) served: BTreeMap<String, Vec<u8>>,
    pub(crate) calls: RefCell<Vec<(String, PathBuf)>>,
}

impl Download for FakeDownload {
    fn download(&self, url: &str, file: &Path) -> Result<(), PortError> {
        self.calls
            .borrow_mut()
            .push((url.to_owned(), file.to_path_buf()));
        let bytes = self
            .served
            .get(url)
            .ok_or_else(|| PortError::new(format!("curl {url}"), "exit status 22"))?;
        std::fs::write(file, bytes).map_err(|e| PortError::new("write", e.to_string()))
    }
}

/// Every fake, as the adapters of a verb.
pub(crate) struct Fakes {
    pub(crate) git: FakeHistory,
    pub(crate) runs: FakeRuns,
    pub(crate) store: FakeStore,
    pub(crate) source: FakeSource,
    pub(crate) verifier: FakeVerifier,
    pub(crate) run_version: FakeRunVersion,
    pub(crate) pkgbuild: FakePkgbuild,
    pub(crate) advisories: FakeAdvisories,
    pub(crate) download: FakeDownload,
}

impl Fakes {
    /// Fakes over `git`, with no runs, no releases, a verifier that fails every file and a
    /// binary that prints nothing.
    pub(crate) fn over(git: FakeHistory) -> Fakes {
        Fakes {
            git,
            runs: FakeRuns::default(),
            store: FakeStore::default(),
            source: FakeSource::default(),
            verifier: FakeVerifier::with(Box::new(|_, _| {
                Err(PortError::new("verify", "no attestation"))
            })),
            run_version: FakeRunVersion {
                said: Said {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            },
            pkgbuild: FakePkgbuild::default(),
            advisories: FakeAdvisories {
                said: Said {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            },
            download: FakeDownload::default(),
        }
    }
}

impl Adapters for Fakes {
    fn ports(&self) -> Ports<'_> {
        Ports {
            git: &self.git,
            runs: &self.runs,
            store: &self.store,
            source: &self.source,
            verifier: &self.verifier,
            run_version: &self.run_version,
            pkgbuild: &self.pkgbuild,
            advisories: &self.advisories,
            download: &self.download,
        }
    }
}
