//! `release channel-sync`: copies the new published releases of one or more GitHub repositories
//! into a release channel's tree, so that a release reaches the channel the daemons fetch from
//! without a push from CI and with no credential beyond reading public release assets.
//!
//! For each repository (`--repo`, in the order given) the published releases are read through
//! the [`ReleaseSource`] port, drafts left out, oldest version first; a release whose tag is not
//! `v<X.Y.Z>` is skipped, and with `--since v<X.Y.Z>` so is every release older than that tag.
//! A release carries one release index per service version it ships (`index.json`, and any
//! `<name>-index.json`: a hostkeeper release carries hostkeeper's, hello's and hello's failing
//! build's). For each release:
//!
//! 1. its asset names must all be plain file names, and its commit a full commit id; a release
//!    with no index is skipped;
//! 2. its indexes are downloaded, and each is read for the service `S` and version `V` it is the
//!    index of. `S` must be a service the repository ships ([`ships`]: the one it is named for,
//!    and hello in hostkeeper's) and no other index of the release may name the same `S/V`, or
//!    the version is refused. Each `S/V` is then compared with the channel:
//!    - `S/V/index.json` holds the same bytes and every package it lists, with its bundle, is
//!      there at its size: the version is already in the channel and nothing is done;
//!    - `S/V/index.json` holds other bytes: the version is refused, since a release in the
//!      channel never changes, and the others go on;
//!    - `S/V` is absent: the version is new;
//!    - `S/V` is there without its index, or without a package or bundle at its size: a stage
//!      that was cut short, which is finished in place;
//! 3. when any `S/V` is new or unfinished, every other asset is downloaded and the release is
//!    checked: a hostkeeper release by `release check` itself (every asset, the sums, the
//!    indexes and every attestation, [`check::check`]); the release of any other repository by
//!    its sums (`SHA256SUMS` lists every asset but itself and the bundles, at its SHA-256) and
//!    the attestation of every asset that has a bundle, online and from the bundle, by that
//!    repository's own `artifacts.yml` on `refs/heads/main` ([`Signer::of`]) at the release's
//!    commit. A download that fails or a check that refuses stages nothing of the release;
//! 4. each `S/V` is staged by `release channel-stage` ([`channel_stage::stage`]), requiring a
//!    package for every platform. A new version is staged into a work directory inside the
//!    channel directory and then renamed to `S/V` as a whole, so a daemon never sees a version
//!    that is half there; an unfinished one is completed in place, which never changes a file
//!    already there.
//!
//! The downloads and the staged tree live in `<channel>/.channel-sync-<pid>-<n>/` while a release
//! is synced and are removed after it; each run first removes any such directory a run that was
//! killed left behind (runs never overlap). A service name never starts with `.`, so that
//! directory is never a service's. Nothing else in the channel is overwritten or removed.
//!
//! Every release is reported, one line each: `staged`, `unchanged`, `skipped` or `refused`. A run
//! with nothing new reports no `staged` line and changes nothing. A run in which anything was
//! refused (a version with other bytes in the channel, a release that fails its check or its
//! download, a repository whose releases cannot be read) still syncs everything else, and then
//! fails with every line. With `--every-secs` the run is repeated forever, a pause of that many
//! seconds after each ([`every`]).

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read as _};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hostkeeper_config::index::{ReleaseIndex, attestation_path, index_path, package_path};
use hostkeeper_config::names::{MAX_NAME, ServiceName, Version as ChannelVersion};
use hostkeeper_config::platform::Platform;
use serde_json::Value;

use super::channel_stage::{self, MAX_INDEX_BYTES};
use super::check::{self, judge, sums_line};
use super::contract::{Commit, HELLO, REPO, Repo, SUMS, Signer, Version, bundle_of, is_bundle};
use super::ports::{Release, ReleaseSource, Verifier};
use super::{Error, file_sha256};

/// What `release channel-sync` syncs, and where.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    /// The repositories whose releases are synced, in this order.
    pub(crate) repos: Vec<Repo>,
    /// The root of the channel's tree.
    pub(crate) channel: PathBuf,
    /// When set, releases older than this version are skipped.
    pub(crate) since: Option<Version>,
}

/// What a release index's file name looks like: `index.json`, or `<name>-index.json`.
const INDEX: &str = "index.json";
const INDEX_SUFFIX: &str = "-index.json";

/// The start of the name of a work directory in the channel.
pub(crate) const WORK_PREFIX: &str = ".channel-sync-";

/// The version of the tag `v<X.Y.Z>`, for `--since`.
pub(crate) fn parse_tag(text: &str) -> Result<Version, String> {
    Version::of_tag(text).ok_or_else(|| format!("not a release tag: {text:?} (expected v<X.Y.Z>)"))
}

/// Whether `name` is a plain file name: 1 to [`MAX_NAME`] bytes of A-Z, a-z, 0-9, `.`, `_`, `+`
/// and `-`, not starting with `.`. As a `gh release download` pattern it matches only itself.
fn plain_name(name: &str) -> bool {
    let chars = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'+' | b'-'));
    chars & !name.is_empty() & (name.len() <= MAX_NAME) & !name.starts_with('.')
}

/// Whether a release of `repo` may stage `service`: the service its repository is named for
/// (`komira-ai/tierstore` ships `tierstore`), and for hostkeeper's own releases also the example
/// service hello they carry. A release that names another service would otherwise claim that
/// service's version in the channel first, and a version in the channel never changes.
fn ships(repo: &Repo, service: &ServiceName) -> bool {
    let own = repo.as_str().rsplit('/').next() == Some(service.as_str());
    own | ((repo.as_str() == REPO) & (service.as_str() == HELLO))
}

/// Whether the asset `name` is a release index.
fn is_index(name: &str) -> bool {
    (name == INDEX) | name.ends_with(INDEX_SUFFIX)
}

/// What one run found, in the order it found it.
#[derive(Default)]
struct Report {
    lines: Vec<String>,
    staged: usize,
    unchanged: usize,
    refused: usize,
}

impl Report {
    fn staged(&mut self, line: String) {
        self.staged += 1;
        self.lines.push(format!("staged {line}"));
    }

    fn unchanged(&mut self, line: String) {
        self.unchanged += 1;
        self.lines.push(format!("unchanged {line}"));
    }

    fn skipped(&mut self, line: String) {
        self.lines.push(format!("skipped {line}"));
    }

    fn refused(&mut self, line: String) {
        self.refused += 1;
        self.lines.push(format!("refused {line}"));
    }

    /// The lines and a summary: the lines when nothing was refused, else the refusal of all.
    fn finish(mut self) -> Result<Vec<String>, Error> {
        self.lines.push(format!(
            "channel-sync: {} versions staged, {} already in the channel, {} refused",
            self.staged, self.unchanged, self.refused
        ));
        if self.refused == 0 {
            Ok(self.lines)
        } else {
            Err(Error::Refused(self.lines))
        }
    }
}

/// Refuses a request that cannot be run: a repository named twice, or a channel directory that
/// is not a directory (a mistyped path is not made).
fn check_request(request: &Request) -> Result<(), Error> {
    for (i, repo) in request.repos.iter().enumerate() {
        if request.repos[..i].contains(repo) {
            return Err(Error::Failed(format!("--repo {repo} is named twice")));
        }
    }
    let channel = &request.channel;
    match std::fs::metadata(channel) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(Error::Failed(format!(
            "--channel-dir {}: not a directory",
            channel.display()
        ))),
        Err(e) => Err(Error::io(format!("--channel-dir {}", channel.display()))(e)),
    }
}

/// The `release channel-sync` verb, once (see the module).
pub(crate) fn sync(
    source: &dyn ReleaseSource,
    verifier: &dyn Verifier,
    request: &Request,
) -> Result<Vec<String>, Error> {
    check_request(request)?;
    round(source, verifier, request)
}

/// One run over a checked request.
fn round(
    source: &dyn ReleaseSource,
    verifier: &dyn Verifier,
    request: &Request,
) -> Result<Vec<String>, Error> {
    let mut report = Report::default();
    clear_work(&request.channel, &mut report);
    for repo in &request.repos {
        sync_repo(source, verifier, request, repo, &mut report);
    }
    report.finish()
}

/// Removes every work directory an earlier run left in `channel` (a run that was killed): runs
/// never overlap, so none is in use. A process often gets the same id after a restart (it is 1 in
/// a container), and its directory names would otherwise collide with those left behind.
fn clear_work(channel: &Path, report: &mut Report) {
    let entries = match std::fs::read_dir(channel).and_then(|r| r.collect::<Result<Vec<_>, _>>()) {
        Ok(entries) => entries,
        Err(e) => return report.refused(format!("read {}: {e}", channel.display())),
    };
    let left = entries
        .iter()
        .filter(|e| e.file_name().to_string_lossy().starts_with(WORK_PREFIX));
    for entry in left {
        let path = entry.path();
        match std::fs::remove_dir_all(&path) {
            Ok(()) => report.lines.push(format!(
                "removed {}, left by an earlier run",
                path.display()
            )),
            Err(e) => report.refused(format!("remove {}: {e}", path.display())),
        }
    }
}

/// What runs between two rounds of [`every`].
pub(crate) trait Pause {
    /// Called after each round with what it gave; [`ControlFlow::Break`] ends the loop with it.
    fn after(&self, round: &Result<Vec<String>, Error>) -> ControlFlow<()>;
}

/// The pause of `--every-secs`: prints what the round gave (its lines on standard output, a
/// failure on standard error, as `xtask` prints a verb's) and sleeps; it never ends the loop.
pub(crate) struct Sleep(pub(crate) Duration);

impl Pause for Sleep {
    fn after(&self, round: &Result<Vec<String>, Error>) -> ControlFlow<()> {
        match round {
            Ok(lines) => lines.iter().for_each(|line| println!("{line}")),
            Err(e) => eprintln!("xtask: {e}"),
        }
        std::thread::sleep(self.0);
        ControlFlow::Continue(())
    }
}

/// The verb with `--every-secs`: the request is checked once, then run in rounds with `pause`
/// after each until it says to stop. A round that refuses something does not stop the loop.
pub(crate) fn every(
    source: &dyn ReleaseSource,
    verifier: &dyn Verifier,
    request: &Request,
    pause: &dyn Pause,
) -> Result<Vec<String>, Error> {
    check_request(request)?;
    loop {
        let result = round(source, verifier, request);
        if pause.after(&result).is_break() {
            return result;
        }
    }
}

/// Syncs the releases of `repo`, oldest version first.
fn sync_repo(
    source: &dyn ReleaseSource,
    verifier: &dyn Verifier,
    request: &Request,
    repo: &Repo,
    report: &mut Report,
) {
    let releases = match source.list(repo) {
        Ok(releases) => releases,
        Err(e) => return report.refused(format!("{repo}: its releases cannot be read: {e}")),
    };
    let mut wanted: BTreeMap<Version, &Release> = BTreeMap::new();
    for release in releases.iter().filter(|r| !r.draft) {
        match Version::of_tag(&release.tag) {
            None => report.skipped(format!("{repo} {}: not a tag v<X.Y.Z>", release.tag)),
            Some(v) if request.since.is_some_and(|since| v < since) => {}
            Some(v) => {
                wanted.insert(v, release);
            }
        }
    }
    for (version, release) in wanted {
        let context = Context {
            source,
            verifier,
            channel: &request.channel,
            repo,
            version,
            release,
        };
        context.sync(report);
    }
}

/// One release being synced.
struct Context<'a> {
    source: &'a dyn ReleaseSource,
    verifier: &'a dyn Verifier,
    channel: &'a Path,
    repo: &'a Repo,
    version: Version,
    release: &'a Release,
}

/// One service version a release carries: the asset name of its index, what the index says it
/// is the index of, and its text.
struct Shipped {
    index_name: String,
    service: ServiceName,
    version: ChannelVersion,
    text: String,
}

/// How a version stands in the channel.
#[derive(Debug, PartialEq, Eq)]
enum InChannel {
    /// `S/V` is not there.
    Absent,
    /// `S/V` is there but not whole: finished in place.
    Unfinished,
    /// `S/V` holds this index and every file it lists.
    Same,
    /// `S/V` holds something else, which never changes: why.
    Other(String),
}

impl Context<'_> {
    /// `<repo> <tag>`, the start of each line about the release.
    fn label(&self) -> String {
        format!("{} {}", self.repo, self.release.tag)
    }

    fn sync(&self, report: &mut Report) {
        let label = self.label();
        let names: Vec<&str> = self
            .release
            .assets
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        if let Some(bad) = names.iter().find(|name| !plain_name(name)) {
            return report.refused(format!(
                "{label}: the asset name {bad:?} is not a plain file name"
            ));
        }
        let indexes: Vec<&str> = names.iter().copied().filter(|n| is_index(n)).collect();
        if indexes.is_empty() {
            return report.skipped(format!("{label}: no release index among its assets"));
        }
        let Ok(commit) = self.release.commit.parse::<Commit>() else {
            return report.refused(format!(
                "{label}: its commit {:?} is not a full commit id",
                self.release.commit
            ));
        };
        let work = match Work::new(self.channel) {
            Ok(work) => work,
            Err(e) => return report.refused(format!("{label}: {e}")),
        };
        let synced = self.sync_in(&work, &names, &indexes, &commit, report);
        let removed = work.remove();
        for e in [synced.err(), removed.err()].into_iter().flatten() {
            report.refused(format!("{label}: {e}"));
        }
    }

    /// Steps 2 to 4 of the module, with `work` for the downloads and the staged tree. An error is
    /// a refusal of the whole release.
    fn sync_in(
        &self,
        work: &Work,
        names: &[&str],
        indexes: &[&str],
        commit: &Commit,
        report: &mut Report,
    ) -> Result<(), Error> {
        let label = self.label();
        for name in indexes {
            self.download(name, &work.assets)?;
        }
        let mut todo = Vec::new();
        let mut seen = BTreeMap::new();
        for name in indexes {
            let shipped = read_index(&work.assets, name)?;
            let what = format!("{label}: {} {}", shipped.service, shipped.version);
            let key = index_path(&shipped.service, &shipped.version);
            if let Some(first) = seen.insert(key, *name) {
                report.refused(format!(
                    "{what}: {name} is a second index of it in the release, after {first}"
                ));
                continue;
            }
            if !ships(self.repo, &shipped.service) {
                report.refused(format!(
                    "{what}: {name} is the index of a service a release of {} does not ship",
                    self.repo
                ));
                continue;
            }
            match in_channel(self.channel, &shipped)? {
                InChannel::Same => report.unchanged(what),
                InChannel::Other(why) => report.refused(format!(
                    "{what}: {why}; a release in the channel never changes, so it is left as it is"
                )),
                state => todo.push((shipped, state)),
            }
        }
        if todo.is_empty() {
            return Ok(());
        }
        for name in names.iter().filter(|n| !indexes.contains(n)) {
            self.download(name, &work.assets)?;
        }
        self.check(&work.assets, names, commit)?;
        for (shipped, state) in todo {
            let what = format!("{label}: {} {}", shipped.service, shipped.version);
            match self.stage(work, &shipped, &state) {
                Ok(()) => report.staged(what),
                Err(e) => report.refused(format!("{what}: {e}")),
            }
        }
        Ok(())
    }

    /// Downloads the asset `name` into `dir`.
    fn download(&self, name: &str, dir: &Path) -> Result<(), Error> {
        Ok(self
            .source
            .download(self.repo, &self.release.tag, name, dir)?)
    }

    /// Step 3: the check of the release in `dir`, whose assets are `names`.
    fn check(&self, dir: &Path, names: &[&str], commit: &Commit) -> Result<(), Error> {
        if self.repo.as_str() == REPO {
            return check::check(self.verifier, dir, &self.version, commit).map(drop);
        }
        check_signed(self.verifier, &Signer::of(self.repo), dir, names, commit)
    }

    /// Step 4 for one version.
    fn stage(&self, work: &Work, shipped: &Shipped, state: &InChannel) -> Result<(), Error> {
        let out = match state {
            InChannel::Absent => work.tree.clone(),
            _ => self.channel.to_owned(),
        };
        channel_stage::stage(&channel_stage::Request {
            service: shipped.service.clone(),
            version: shipped.version.clone(),
            from: work.assets.clone(),
            index: Some(work.assets.join(&shipped.index_name)),
            require: Platform::ALL.to_vec(),
            out,
        })?;
        if *state != InChannel::Absent {
            return Ok(());
        }
        let parent = self.channel.join(shipped.service.as_str());
        std::fs::create_dir_all(&parent)
            .map_err(Error::io(format!("create {}", parent.display())))?;
        let staged = work
            .tree
            .join(shipped.service.as_str())
            .join(shipped.version.as_str());
        let target = parent.join(shipped.version.as_str());
        // A rename replaces an empty directory and fails on any other: look first, so that a
        // directory made since step 2 is left as it is.
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(Error::Failed(format!(
                "{} appeared while the release was staged; it is left as it is",
                target.display()
            )));
        }
        std::fs::rename(&staged, &target).map_err(Error::io(format!(
            "rename {} to {}",
            staged.display(),
            target.display()
        )))
    }
}

/// The service and version the index `name` in `dir` says it is the index of, and its text.
/// Whether it is a valid index is `channel-stage`'s to check.
fn read_index(dir: &Path, name: &str) -> Result<Shipped, Error> {
    let path = dir.join(name);
    let what = format!("read {}", path.display());
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(Error::io(&what))?;
    let refused = |why: &str| Error::Failed(format!("{name}: index_invalid: {why}"));
    let text = String::from_utf8(bytes).map_err(|_| refused("not UTF-8"))?;
    let field = |value: &Value, key: &str| -> Option<String> {
        value.get(key).and_then(Value::as_str).map(str::to_owned)
    };
    let fields = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| Some((field(&value, "service")?, field(&value, "version")?)));
    let (service, version) =
        fields.ok_or_else(|| refused("not JSON with a service and a version"))?;
    let service = ServiceName::new(&service).map_err(|e| refused(&e.to_string()))?;
    let version = ChannelVersion::new(&version).map_err(|e| refused(&e.to_string()))?;
    Ok(Shipped {
        index_name: name.to_owned(),
        service,
        version,
        text,
    })
}

/// What is at `path`, not following a link; none when nothing is.
fn kind_of(path: &Path) -> Result<Option<std::fs::Metadata>, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(format!("read {}", path.display()))(e)),
    }
}

/// How the version `shipped` stands in the channel (step 2 of the module). A package is compared
/// by its size only: hashing every staged package on every run would read the whole channel, and
/// the daemon checks each package's SHA-256 against the index as it fetches it.
fn in_channel(channel: &Path, shipped: &Shipped) -> Result<InChannel, Error> {
    let (service, version) = (&shipped.service, &shipped.version);
    let dir = channel.join(service.as_str()).join(version.as_str());
    match kind_of(&dir)? {
        None => return Ok(InChannel::Absent),
        Some(meta) if !meta.is_dir() => {
            return Ok(InChannel::Other(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        Some(_) => {}
    }
    let index_at = channel.join(index_path(service, version));
    match kind_of(&index_at)? {
        None => return Ok(InChannel::Unfinished),
        Some(meta) if !meta.is_file() => {
            return Ok(InChannel::Other(format!(
                "{} is not a file",
                index_at.display()
            )));
        }
        Some(_) => {}
    }
    let staged =
        std::fs::read(&index_at).map_err(Error::io(format!("read {}", index_at.display())))?;
    if staged != shipped.text.as_bytes() {
        return Ok(InChannel::Other(format!(
            "{} holds another index than the release's",
            index_at.display()
        )));
    }
    // The staged index is the release's, which channel-stage checked when it staged it.
    let Ok(index) = ReleaseIndex::parse(&shipped.text, service, version) else {
        return Ok(InChannel::Unfinished);
    };
    for entry in index.packages() {
        let package = kind_of(&channel.join(package_path(service, version, entry.platform)))?;
        let bundle = kind_of(&channel.join(attestation_path(service, version, entry.platform)))?;
        let whole = package.is_some_and(|m| m.is_file() & (m.len() == entry.bytes))
            & bundle.is_some_and(|m| m.is_file());
        if !whole {
            return Ok(InChannel::Unfinished);
        }
    }
    Ok(InChannel::Same)
}

/// Step 3 for a repository other than hostkeeper: the sums of every asset in `dir` (`names`), and
/// every attested asset (one with a bundle) verified online and from its bundle as signed by
/// `signer` at `commit`. Every problem found is in the refusal.
fn check_signed(
    verifier: &dyn Verifier,
    signer: &Signer,
    dir: &Path,
    names: &[&str],
    commit: &Commit,
) -> Result<(), Error> {
    let mut problems = Vec::new();
    if !names.contains(&SUMS) {
        return Err(Error::Refused(vec![format!("missing {SUMS}")]));
    }
    let path = dir.join(SUMS);
    let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut listed = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        match sums_line(line) {
            Some((digest, name)) => {
                listed.insert(name, digest);
            }
            None => problems.push(format!(
                "{SUMS} line {} is not '<sha256>  <name>'",
                index + 1
            )),
        }
    }
    for name in listed.keys().filter(|n| !names.contains(n)) {
        problems.push(format!(
            "{SUMS} lists {name}, which is not an asset of the release"
        ));
    }
    for name in names.iter().filter(|n| (**n != SUMS) & !is_bundle(n)) {
        match listed.get(name) {
            None => problems.push(format!("{SUMS} does not list {name}")),
            Some(digest) => {
                let actual = file_sha256(&dir.join(name))?;
                if actual != *digest {
                    problems.push(format!(
                        "{name}: its SHA-256 is {actual}, {SUMS} says {digest}"
                    ));
                }
            }
        }
    }
    let attested = names
        .iter()
        .filter(|n| names.contains(&bundle_of(n).as_str()));
    for name in attested {
        let file = dir.join(name);
        let bundle = dir.join(bundle_of(name));
        let digest = file_sha256(&file)?;
        for (form, bundle) in [("online", None), ("offline", Some(bundle.as_path()))] {
            judge(
                &format!("{name} ({form})"),
                verifier.verify(signer, &file, bundle, commit),
                &digest,
                (signer, commit),
                &mut problems,
            );
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Refused(problems))
    }
}

/// The number the next work directory of this process takes.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// The work directory of one release in the channel directory: `assets/` for its downloads,
/// `tree/` for the staged tree of its new versions.
struct Work {
    root: PathBuf,
    assets: PathBuf,
    tree: PathBuf,
}

impl Work {
    fn new(channel: &Path) -> Result<Work, Error> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = channel.join(format!("{WORK_PREFIX}{}-{n}", std::process::id()));
        let assets = root.join("assets");
        let tree = root.join("tree");
        for dir in [&root, &assets, &tree] {
            std::fs::create_dir(dir).map_err(Error::io(format!("create {}", dir.display())))?;
        }
        Ok(Work { root, assets, tree })
    }

    /// Removes the work directory and everything left in it.
    fn remove(&self) -> Result<(), Error> {
        std::fs::remove_dir_all(&self.root)
            .map_err(Error::io(format!("remove {}", self.root.display())))
    }
}

#[cfg(test)]
mod tests;
