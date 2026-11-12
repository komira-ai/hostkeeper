//! A fake of the repository installer's tools port that simulates apt, dpkg and dpkg-deb on a
//! model: a repository holding package versions, the host's installed packages, and the faults
//! a test injects. It writes the files apt would write (the lists on an update, the package file
//! on a download, the tree and the control members on an extraction) and reads the work
//! directory's own files to know which repository the run sees and which package files it finds,
//! so what the installer does with files is the real thing.

#![allow(
    dead_code,
    reason = "each part of the fake is used by some of the test modules"
)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[path = "apt_mirror.rs"]
pub mod apt_mirror;

use hostkeeper_install::repository::tools::ToolError;
use hostkeeper_install::repository::{Run, Tool, ToolOutput, Tools};
use sha2::{Digest, Sha256};

#[path = "apt_fake_units.rs"]
mod units;

/// The events the fake and the records port both log, in order.
pub type Events = Arc<Mutex<Vec<String>>>;

static SERIAL: AtomicU64 = AtomicU64::new(0);

/// One entry of a package's tree.
#[derive(Clone, Debug)]
pub enum Node {
    File { mode: u32, text: String },
    Dir(u32),
    Link(String),
    Fifo,
}

/// A package file's model: its control fields, its control members and its tree. Its bytes are
/// unique to the build, so the fake finds the model of a file by its content.
#[derive(Clone, Debug)]
pub struct Deb {
    pub package: String,
    pub version: String,
    pub arch: String,
    /// The control members beside `control`.
    pub members: Vec<String>,
    pub tree: BTreeMap<String, Node>,
    bytes: Vec<u8>,
}

impl Deb {
    pub fn new(package: &str, version: &str, arch: &str) -> Deb {
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        Deb {
            package: package.to_owned(),
            version: version.to_owned(),
            arch: arch.to_owned(),
            members: Vec::new(),
            tree: BTreeMap::new(),
            bytes: format!("!<arch>\ndeb {package} {version} {arch} #{serial}\n").into_bytes(),
        }
    }

    pub fn file(mut self, path: &str, mode: u32, text: &str) -> Deb {
        let text = text.to_owned();
        self.tree.insert(path.to_owned(), Node::File { mode, text });
        self
    }

    pub fn dir(mut self, path: &str, mode: u32) -> Deb {
        self.tree.insert(path.to_owned(), Node::Dir(mode));
        self
    }

    pub fn link(mut self, path: &str, target: &str) -> Deb {
        self.tree
            .insert(path.to_owned(), Node::Link(target.to_owned()));
        self
    }

    pub fn fifo(mut self, path: &str) -> Deb {
        self.tree.insert(path.to_owned(), Node::Fifo);
        self
    }

    pub fn member(mut self, name: &str) -> Deb {
        self.members.push(name.to_owned());
        self
    }

    pub fn without(mut self, path: &str) -> Deb {
        self.tree.remove(path);
        self
    }

    /// The model after `f` changed its fields.
    pub fn mutate(mut self, f: impl FnOnce(&mut Deb)) -> Deb {
        f(&mut self);
        self
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The SHA-256 of the file, lower-case hex.
    pub fn pin(&self) -> String {
        hex::encode(Sha256::digest(&self.bytes))
    }

    /// The file's name in apt's archives directory.
    fn file_name(&self) -> String {
        format!(
            "{}_{}_{}.deb",
            self.package,
            self.version.replace(':', "%3a"),
            self.arch
        )
    }
}

/// One package version of a repository's index.
#[derive(Clone, Debug)]
pub struct Entry {
    pub package: String,
    pub version: String,
    pub arch: String,
    pub sha256: String,
    pub size: u64,
    /// The stanza's `Filename`; none leaves the field out.
    pub filename: Option<String>,
    pub deb: Deb,
}

impl Entry {
    pub fn of(deb: Deb) -> Entry {
        Entry {
            package: deb.package.clone(),
            version: deb.version.clone(),
            arch: deb.arch.clone(),
            sha256: deb.pin(),
            size: deb.bytes.len() as u64,
            filename: Some(format!("pool/main/{}", deb.file_name())),
            deb,
        }
    }

    fn stanza(&self) -> String {
        let filename = self
            .filename
            .as_ref()
            .map_or_else(String::new, |name| format!("Filename: {name}\n"));
        format!(
            "Package: {}\nVersion: {}\nArchitecture: {}\nSize: {}\n{filename}SHA256: {}\n\n",
            self.package, self.version, self.arch, self.size, self.sha256
        )
    }
}

/// How a repository's signature fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sig {
    /// apt before 3.0, gpgv: no key.
    NoPubkey,
    BadSignature,
    ExpiredKey,
    RevokedKey,
    /// apt 3.0, sqv: no key.
    SqvMissingKey,
    /// apt 3.0, sqv: a manipulated message.
    SqvManipulated,
    Expired,
    NoLongerSigned,
}

impl Sig {
    pub const ALL: [Sig; 8] = [
        Sig::NoPubkey,
        Sig::BadSignature,
        Sig::ExpiredKey,
        Sig::RevokedKey,
        Sig::SqvMissingKey,
        Sig::SqvManipulated,
        Sig::Expired,
        Sig::NoLongerSigned,
    ];

    fn stderr(self, uri: &str) -> String {
        let gpg = |token: &str| {
            format!(
                "Err:1 {uri} stable InRelease\n  The following signatures couldn't be verified: \
                 {token} 0123456789ABCDEF\nW: GPG error: {uri} stable InRelease: {token} \
                 0123456789ABCDEF\nE: The repository '{uri} stable InRelease' is not signed.\n"
            )
        };
        match self {
            Sig::NoPubkey => gpg("NO_PUBKEY"),
            Sig::BadSignature => gpg("BADSIG"),
            Sig::ExpiredKey => gpg("EXPKEYSIG"),
            Sig::RevokedKey => gpg("REVKEYSIG"),
            Sig::SqvMissingKey => format!(
                "Err:1 {uri} stable InRelease\n  Sub-process /usr/bin/sqv returned an error code \
                 (1), error message is:\n   Missing key 0123456789ABCDEF, which is needed to \
                 verify signature.\nE: The repository '{uri} stable InRelease' is not signed.\n"
            ),
            Sig::SqvManipulated => format!(
                "Err:1 {uri} stable InRelease\n  Sub-process /usr/bin/sqv returned an error code \
                 (1), error message is:\n   Message has been manipulated\nE: The repository \
                 '{uri} stable InRelease' is not signed.\n"
            ),
            Sig::Expired => format!(
                "E: Release file for {uri}/dists/stable/InRelease is expired (invalid since 3d \
                 2h). Updates for this repository will not be applied.\n"
            ),
            Sig::NoLongerSigned => {
                format!("E: The repository '{uri} stable InRelease' is no longer signed.\n")
            }
        }
    }
}

/// What a repository does when it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Up,
    Unreachable,
    Signature(Sig),
}

#[derive(Clone, Debug)]
pub struct Repo {
    pub health: Health,
    pub entries: Vec<Entry>,
}

/// What a download gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Download {
    /// The index's bytes.
    #[default]
    Right,
    /// Other bytes than the index's.
    WrongBytes,
    /// apt's own `Hash Sum mismatch` failure.
    HashFailure,
    /// The repository could not be reached.
    Unreachable,
    /// A success that leaves no package file.
    NoFile,
    /// A success that leaves two package files.
    TwoFiles,
    /// A package file the daemon cannot read.
    Unreadable,
    /// A success after which the archives directory is gone.
    NoArchivesDir,
}

/// The faults a test injects.
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// Per verb (`simulate`, `install`, `purge`, `hold` for both of apt-mark's): the tries the
    /// package manager's lock is held.
    pub busy: BTreeMap<&'static str, u32>,
    /// dpkg was interrupted before the run.
    pub interrupted: bool,
    /// The simulation and the install find an unmet dependency.
    pub unmet: bool,
    /// Lines the simulation prints beside its own.
    pub sim_extra: Vec<String>,
    /// An install of this version fails in a maintainer script (status `iF`).
    pub script_fails_for: Option<String>,
    /// An install that exits 0 and leaves this version.
    pub leaves: Option<String>,
    /// An install that exits 0 and leaves the package half-configured.
    pub incomplete: bool,
    /// An install that exits 0 and leaves no package.
    pub noop: bool,
    /// A purge another package's dependency refuses.
    pub purge_refused: bool,
    pub download: Download,
    /// Directories an update makes in the work directory, beside the lists.
    pub litter: Vec<String>,
    /// An update leaves the index as a link to the repository's own file, outside the work
    /// directory, as apt does for an uncompressed index of a `file:` repository.
    pub index_as_link: bool,
}

/// A package as the host's database holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inst {
    pub version: String,
    /// dpkg's state letter: `i` installed, `F` half-configured.
    pub state: char,
    pub held: bool,
}

/// One run the fake saw.
#[derive(Clone, Debug)]
pub struct Call {
    pub run: Run,
    pub label: String,
    /// Whether the installer lock was held while it ran.
    pub locked: bool,
    /// The text of the host root's `usr/sbin/policy-rc.d` and its mode while it ran; `None`:
    /// no file, or no host root watched.
    pub policy: Option<(String, u32)>,
}

struct Rule {
    label: String,
    skip: u32,
    answer: Result<ToolOutput, ToolError>,
}

#[derive(Default)]
pub struct State {
    pub repos: BTreeMap<String, Repo>,
    pub debs: Vec<Deb>,
    pub installed: BTreeMap<String, Inst>,
    pub calls: Vec<Call>,
    pub faults: Faults,
    rules: Vec<Rule>,
}

pub struct FakeHost {
    state: Mutex<State>,
    lock_path: PathBuf,
    events: Events,
    /// The host's root, whose policy file each call records.
    root: Mutex<Option<PathBuf>>,
    /// The service manager's unit directory: where it is on disk and how the host names it,
    /// read by `systemctl show` ([`FakeHost::watch_units`]).
    units: Mutex<Option<(PathBuf, String)>>,
}

/// The name a run goes by in the fake's record.
pub fn label(run: &Run) -> String {
    let has = |word: &str| run.args.iter().any(|a| a == word);
    let first = run.args.first().cloned().unwrap_or_default();
    match run.tool {
        Tool::AptGet if has("update") => "apt-get update".to_owned(),
        Tool::AptGet if has("download") => "apt-get download".to_owned(),
        Tool::AptGet if has("install") && has("--simulate") => "apt-get simulate".to_owned(),
        Tool::AptGet if has("install") => "apt-get install".to_owned(),
        Tool::AptGet => "apt-get ?".to_owned(),
        Tool::AptCache => "apt-cache show".to_owned(),
        Tool::DpkgQuery => "dpkg-query".to_owned(),
        tool => format!("{} {first}", tool.name()),
    }
}

fn out(code: i32, stdout: &str, stderr: &str) -> ToolOutput {
    ToolOutput {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
        cut: false,
    }
}

/// The work directory an apt run names in its options.
fn work_of(run: &Run) -> PathBuf {
    let sources = run
        .args
        .iter()
        .find_map(|a| a.strip_prefix("Dir::Etc::SourceParts="))
        .unwrap_or_default();
    PathBuf::from(sources.strip_suffix("/sources").unwrap_or(sources))
}

/// The URI of the one entry the work directory's sources file holds.
fn uri_of(work: &Path) -> Option<String> {
    let mut files: Vec<PathBuf> = fs::read_dir(work.join("sources"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    let text = fs::read_to_string(files.first()?).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("URIs:"))
        .map(|u| u.trim().to_owned())
}

/// The package and the version of an apt run's last argument, `package=version`.
fn target(run: &Run) -> (String, String) {
    let last = run.args.last().cloned().unwrap_or_default();
    let (p, v) = last.split_once('=').unwrap_or((&last, ""));
    (p.to_owned(), v.to_owned())
}

/// Where a repository serves the index `index` of `stem` from, beside the work directories:
/// written there, and answered.
fn served_index(work: &Path, stem: &str, index: &str) -> std::io::Result<PathBuf> {
    let served = work
        .parent()
        .unwrap_or(work)
        .with_file_name("served")
        .join(format!("{stem}_Packages"));
    fs::create_dir_all(served.parent().unwrap_or(work))?;
    fs::write(&served, index)?;
    Ok(served)
}

/// The served indexes of the repositories under `root`, `served/` beside the work directories.
pub fn served_dir(work_root: &Path) -> PathBuf {
    work_root.with_file_name("served")
}

/// The stanzas of every index in the work directory's lists.
fn list_stanzas(work: &Path) -> Vec<String> {
    let mut files: Vec<PathBuf> = fs::read_dir(work.join("lists"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with("_Packages"))
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|f| fs::read_to_string(f).ok())
        .flat_map(|t| {
            t.split("\n\n")
                .filter(|s| !s.trim().is_empty())
                .map(|s| format!("{s}\n"))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn field<'a>(stanza: &'a str, key: &str) -> Option<&'a str> {
    stanza.lines().find_map(|l| {
        l.split_once(": ")
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
    })
}

impl FakeHost {
    pub fn new(lock_path: &Path, events: Events) -> FakeHost {
        FakeHost {
            state: Mutex::new(State::default()),
            lock_path: lock_path.to_path_buf(),
            events,
            root: Mutex::new(None),
            units: Mutex::new(None),
        }
    }

    /// Makes `systemctl show` read the unit directory the host names `spelled`, at `disk`.
    pub fn watch_units(&self, disk: &Path, spelled: &str) {
        *self.units.lock().expect("the units") = Some((disk.to_path_buf(), spelled.to_owned()));
    }

    /// Makes each call record the policy file under `root`.
    pub fn watch_root(&self, root: &Path) {
        *self.root.lock().expect("the root") = Some(root.to_path_buf());
    }

    fn policy(&self) -> Option<(String, u32)> {
        let root = self.root.lock().expect("the root").clone()?;
        let path = root.join("usr/sbin/policy-rc.d");
        let text = fs::read_to_string(&path).ok()?;
        let mode = fs::metadata(&path).ok()?.permissions().mode() & 0o7777;
        Some((text, mode))
    }

    /// Changes the model or the faults.
    pub fn edit<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().expect("the fake's state"))
    }

    /// Publishes `deb` in the repository at `uri` (made up when it is the first), answers its pin.
    pub fn publish(&self, uri: &str, deb: Deb) -> String {
        let pin = deb.pin();
        self.edit(|s| {
            s.debs.push(deb.clone());
            s.repos
                .entry(uri.to_owned())
                .or_insert(Repo {
                    health: Health::Up,
                    entries: Vec::new(),
                })
                .entries
                .push(Entry::of(deb));
        });
        pin
    }

    /// Changes every entry of every repository.
    pub fn each_entry(&self, f: impl Fn(&mut Entry)) {
        self.edit(|s| {
            s.repos
                .values_mut()
                .flat_map(|r| r.entries.iter_mut())
                .for_each(&f);
        });
    }

    pub fn set_health(&self, uri: &str, health: Health) {
        self.edit(|s| {
            if let Some(repo) = s.repos.get_mut(uri) {
                repo.health = health;
            }
        });
    }

    /// Empties every repository: what a rollback must not need.
    pub fn empty_repositories(&self) {
        self.edit(|s| s.repos.values_mut().for_each(|r| r.entries.clear()));
    }

    pub fn set_installed(&self, package: &str, version: &str, state: char, held: bool) {
        self.edit(|s| {
            s.installed.insert(
                package.to_owned(),
                Inst {
                    version: version.to_owned(),
                    state,
                    held,
                },
            );
        });
    }

    pub fn installed(&self, package: &str) -> Option<Inst> {
        self.edit(|s| s.installed.get(package).cloned())
    }

    pub fn all_installed(&self) -> BTreeMap<String, Inst> {
        self.edit(|s| s.installed.clone())
    }

    pub fn calls(&self) -> Vec<Call> {
        self.edit(|s| s.calls.clone())
    }

    pub fn labels(&self) -> Vec<String> {
        self.calls().into_iter().map(|c| c.label).collect()
    }

    /// The calls with `label`.
    pub fn calls_of(&self, label: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.label == label)
            .collect()
    }

    /// Every run with `label` answers `answer` (after `skip` runs that are simulated).
    pub fn answer(&self, label: &str, skip: u32, answer: Result<ToolOutput, ToolError>) {
        self.edit(|s| {
            s.rules.push(Rule {
                label: label.to_owned(),
                skip,
                answer,
            });
        });
    }

    /// Every run with `label` cannot be run at all.
    pub fn cannot_run(&self, label: &str) {
        self.answer(label, 0, Err(ToolError(format!("{label} was not found"))));
    }

    fn lock_is_held(&self) -> bool {
        let opened = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.lock_path);
        opened.is_ok_and(|file| file.try_lock().is_err())
    }

    fn event(&self, text: String) {
        self.events.lock().expect("the events").push(text);
    }
}

impl Tools for FakeHost {
    fn run(&self, run: &Run) -> Result<ToolOutput, ToolError> {
        let label = label(run);
        let locked = self.lock_is_held();
        let policy = self.policy();
        self.event(format!("run {label}"));
        let mut s = self.state.lock().expect("the fake's state");
        s.calls.push(Call {
            run: run.clone(),
            label: label.clone(),
            locked,
            policy,
        });
        for rule in s.rules.iter_mut().filter(|r| r.label == label) {
            if rule.skip == 0 {
                return rule.answer.clone();
            }
            rule.skip -= 1;
        }
        Ok(match label.as_str() {
            "systemctl show" => units::show(self.units.lock().expect("the units").clone(), &s, run),
            "apt-get update" => update(&s, run),
            "apt-cache show" => show(run),
            "apt-get simulate" => simulate(&mut s, run),
            "apt-get download" => download(&s, run),
            "apt-get install" => install(&mut s, run),
            "dpkg --print-architecture" => out(0, "amd64\n", ""),
            "dpkg --purge" => purge(&mut s, run),
            "dpkg-query" => query(&s, run),
            "apt-mark hold" | "apt-mark unhold" => mark(&mut s, run),
            "dpkg-deb --field" => deb_field(&s, run),
            "dpkg-deb --extract" => deb_extract(&s, run),
            "systemctl daemon-reload" => out(0, "", ""),
            other => out(127, "", &format!("the fake does not know {other}\n")),
        })
    }
}

fn update(s: &State, run: &Run) -> ToolOutput {
    let work = work_of(run);
    let uri = uri_of(&work).unwrap_or_default();
    let unreachable = format!(
        "Err:1 {uri} stable InRelease\n  Could not resolve host\nE: Failed to fetch \
         {uri}/dists/stable/InRelease  Could not resolve host\nE: Some index files failed to \
         download. They have been ignored, or old ones used instead.\n"
    );
    let mirrored = if s.repos.contains_key(&uri) {
        None
    } else {
        apt_mirror::read(&uri, &s.debs)
    };
    let repo = match s.repos.get(&uri).or(mirrored.as_ref()) {
        Some(repo) if repo.health == Health::Up => repo,
        Some(Repo {
            health: Health::Signature(sig),
            ..
        }) => return out(100, "", &sig.stderr(&uri)),
        _ => return out(100, "", &unreachable),
    };
    let stem: String = uri
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let lists = work.join("lists");
    let index: String = repo.entries.iter().map(Entry::stanza).collect();
    let packages = lists.join(format!("{stem}_Packages"));
    let written = match s.faults.index_as_link {
        false => fs::write(&packages, index),
        true => served_index(&work, &stem, &index)
            .and_then(|served| std::os::unix::fs::symlink(served, &packages)),
    };
    let written = written
        .and_then(|()| fs::write(lists.join(format!("{stem}_InRelease")), "signed\n"))
        .and_then(|()| fs::write(lists.join("lock"), ""))
        .and_then(|()| fs::write(lists.join("partial").join("half"), "x"));
    for dir in &s.faults.litter {
        let _ = fs::create_dir_all(work.join(dir));
    }
    match written {
        Ok(()) => out(0, "Reading package lists... Done\n", ""),
        Err(e) => out(100, "", &format!("E: Could not write the lists: {e}\n")),
    }
}

fn show(run: &Run) -> ToolOutput {
    let (package, version) = target(run);
    let found: Vec<String> = list_stanzas(&work_of(run))
        .into_iter()
        .filter(|s| {
            (field(s, "Package") == Some(&package)) & (field(s, "Version") == Some(&version))
        })
        .collect();
    match found.is_empty() {
        true => out(100, "", "E: No packages found\n"),
        false => out(0, &found.join("\n"), ""),
    }
}

/// What a tool prints when the lock is held for one more of the tries of `verb`; `None` once
/// those are spent. dpkg says it its own way, apt and apt-mark theirs.
fn busy(s: &mut State, verb: &'static str) -> Option<ToolOutput> {
    let apt = verb != "purge";
    let left = s.faults.busy.get_mut(verb).filter(|left| **left > 0)?;
    *left -= 1;
    Some(match apt {
        true => out(
            100,
            "",
            "E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 4321 \
             (unattended-upgr)\nE: Unable to acquire the dpkg frontend lock \
             (/var/lib/dpkg/lock-frontend), is another process using it?\n",
        ),
        false => out(
            2,
            "",
            "dpkg: error: dpkg frontend lock was locked by another process with pid 4321\n",
        ),
    })
}

fn busy_or_interrupted(s: &mut State, verb: &'static str) -> Option<ToolOutput> {
    let apt = verb != "purge";
    if let Some(output) = busy(s, verb) {
        return Some(output);
    }
    s.faults.interrupted.then(|| {
        let text = "dpkg was interrupted, you must manually run 'dpkg --configure -a' to \
                    correct the problem.";
        match apt {
            true => out(100, "", &format!("E: {text}\n")),
            false => out(2, "", &format!("dpkg: error: {text}\n")),
        }
    })
}

fn simulate(s: &mut State, run: &Run) -> ToolOutput {
    if let Some(output) = busy_or_interrupted(s, "simulate") {
        return output;
    }
    let (package, version) = target(run);
    let held = s.installed.get(&package).is_some_and(|i| i.held);
    let allowed = run.args.iter().any(|a| a == "--allow-change-held-packages");
    if s.faults.unmet || (held & !allowed) {
        return out(
            100,
            "",
            "E: Unable to correct problems, you have held broken packages.\n",
        );
    }
    let known = list_stanzas(&work_of(run)).iter().any(|st| {
        (field(st, "Package") == Some(&package)) & (field(st, "Version") == Some(&version))
    });
    if !known {
        return out(
            100,
            "",
            &format!("E: Version '{version}' for '{package}' was not found\n"),
        );
    }
    let mut text = String::from("NOTE: This is only a simulation!\n");
    text.push_str(&format!("Inst {package} ({version} main [amd64])\n"));
    for line in &s.faults.sim_extra {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str(&format!("Conf {package} ({version} main [amd64])\n"));
    out(0, &text, "")
}

fn download(s: &State, run: &Run) -> ToolOutput {
    let (package, version) = target(run);
    let work = work_of(run);
    let uri = uri_of(&work).unwrap_or_default();
    let mirrored = if s.repos.contains_key(&uri) {
        None
    } else {
        apt_mirror::read(&uri, &s.debs)
    };
    let entry = s.repos.get(&uri).or(mirrored.as_ref()).and_then(|r| {
        r.entries
            .iter()
            .find(|e| (e.package == package) & (e.version == version))
    });
    if let Some(entry) = entry.filter(|_| mirrored.is_some())
        && !apt_mirror::holds(&uri, entry)
    {
        return out(
            100,
            "",
            &format!(
                "E: Failed to fetch {uri}/{}  File not found\n",
                entry.filename.clone().unwrap_or_default()
            ),
        );
    }
    let (mode, Some(entry), Some(dir)) = (s.faults.download, entry, run.working_dir.as_ref())
    else {
        return out(
            100,
            "",
            &format!("E: Version '{version}' for '{package}' was not found\n"),
        );
    };
    let write = |name: &str, bytes: &[u8]| fs::write(dir.join(name), bytes);
    let name = entry.deb.file_name();
    let done = match mode {
        Download::Right => write(&name, entry.deb.bytes()),
        Download::WrongBytes => write(&name, b"other bytes"),
        Download::HashFailure => {
            return out(
                100,
                "",
                &format!(
                    "E: Failed to fetch {uri}/pool/{name}  Hash Sum mismatch\n   Hashes of \
                     expected file:\n    - SHA256:{}\nE: Unable to fetch some archives.\n",
                    entry.sha256
                ),
            );
        }
        Download::Unreachable => {
            return out(
                100,
                "",
                "E: Failed to fetch the package  Could not connect\n",
            );
        }
        Download::NoFile => Ok(()),
        Download::TwoFiles => {
            write(&name, entry.deb.bytes()).and_then(|()| write("x_other.deb", b"x"))
        }
        Download::Unreadable => write(&name, entry.deb.bytes())
            .and_then(|()| fs::set_permissions(dir.join(&name), fs::Permissions::from_mode(0o000))),
        Download::NoArchivesDir => {
            write(&name, entry.deb.bytes()).and_then(|()| fs::remove_dir_all(dir))
        }
    };
    match done {
        Ok(()) => out(0, "", ""),
        Err(e) => out(100, "", &format!("E: Could not write the package: {e}\n")),
    }
}

/// Whether the work directory's archives hold the file of `package` at `version`.
fn archived(s: &State, work: &Path, package: &str, version: &str) -> bool {
    fs::read_dir(work.join("archives"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| fs::read(e.path()).ok())
        .any(|bytes| {
            s.debs
                .iter()
                .any(|d| (d.bytes == bytes) & (d.package == package) & (d.version == version))
        })
}

fn install(s: &mut State, run: &Run) -> ToolOutput {
    if let Some(output) = busy_or_interrupted(s, "install") {
        return output;
    }
    let (package, version) = target(run);
    let work = work_of(run);
    if s.faults.unmet {
        return out(
            100,
            "",
            "E: Unable to correct problems, you have held broken packages.\n",
        );
    }
    let listed = list_stanzas(&work).iter().any(|st| {
        (field(st, "Package") == Some(&package)) & (field(st, "Version") == Some(&version))
    });
    if !listed {
        return out(
            100,
            "",
            &format!("E: Version '{version}' for '{package}' was not found\n"),
        );
    }
    if !archived(s, &work, &package, &version) {
        return out(
            100,
            "",
            "E: Can't download packages: --no-download is set and the archive is not there\n",
        );
    }
    let held = s.installed.get(&package).is_some_and(|i| i.held);
    let mut set = |version: &str, state: char| {
        s.installed.insert(
            package.clone(),
            Inst {
                version: version.to_owned(),
                state,
                held,
            },
        );
    };
    if s.faults.script_fails_for.as_deref() == Some(version.as_str()) {
        set(&version, 'F');
        return out(
            100,
            &format!("Setting up {package} ({version}) ...\n"),
            "dpkg: error processing package mdm (--configure):\n installed package \
             post-installation script subprocess returned error exit status 1\nE: Sub-process \
             /usr/bin/dpkg returned an error code (1)\n",
        );
    }
    if s.faults.noop {
        return out(0, "", "");
    }
    if let Some(other) = s.faults.leaves.clone() {
        set(&other, 'i');
    } else if s.faults.incomplete {
        set(&version, 'F');
    } else {
        set(&version, 'i');
    }
    out(0, &format!("Setting up {package} ({version}) ...\n"), "")
}

fn purge(s: &mut State, run: &Run) -> ToolOutput {
    if let Some(output) = busy_or_interrupted(s, "purge") {
        return output;
    }
    let package = run.args.last().cloned().unwrap_or_default();
    if s.faults.purge_refused {
        return out(
            1,
            "",
            &format!(
                "dpkg: dependency problems prevent removal of {package}:\n web-ui depends on \
                 {package}.\n\ndpkg: error processing package {package} (--purge):\n dependency \
                 problems - not removing\n"
            ),
        );
    }
    s.installed.remove(&package);
    out(0, "(Reading database ...)\n", "")
}

fn query(s: &State, run: &Run) -> ToolOutput {
    let package = run.args.last().cloned().unwrap_or_default();
    match s.installed.get(&package) {
        Some(i) => {
            let desired = if i.held { 'h' } else { 'i' };
            out(0, &format!("{}\t{desired}{} \n", i.version, i.state), "")
        }
        None => out(
            1,
            "",
            &format!("dpkg-query: no packages found matching {package}\n"),
        ),
    }
}

fn mark(s: &mut State, run: &Run) -> ToolOutput {
    if let Some(output) = busy(s, "hold") {
        return output;
    }
    let package = run.args.last().cloned().unwrap_or_default();
    let hold = run.args.first().is_some_and(|a| a == "hold");
    match s.installed.get_mut(&package) {
        Some(i) => {
            i.held = hold;
            out(0, &format!("{package} set on hold.\n"), "")
        }
        None => out(100, "", &format!("E: Can't find the package {package}\n")),
    }
}

fn model_of<'a>(s: &'a State, run: &Run) -> Option<&'a Deb> {
    let bytes = fs::read(run.args.get(1)?).ok()?;
    s.debs.iter().find(|d| d.bytes == bytes)
}

fn deb_field(s: &State, run: &Run) -> ToolOutput {
    let Some(deb) = model_of(s, run) else {
        return out(2, "", "dpkg-deb: error: not a Debian format archive\n");
    };
    let mut text = String::new();
    for key in &run.args[2..] {
        let value = match key.as_str() {
            "Package" => &deb.package,
            "Version" => &deb.version,
            _ => &deb.arch,
        };
        text.push_str(&format!("{key}: {value}\n"));
    }
    out(0, &text, "")
}

fn deb_extract(s: &State, run: &Run) -> ToolOutput {
    let Some(deb) = model_of(s, run) else {
        return out(2, "", "dpkg-deb: error: not a Debian format archive\n");
    };
    let root = PathBuf::from(&run.args[2]);
    let made = deb.tree.iter().try_for_each(|(path, node)| {
        let at = root.join(path);
        if let Some(parent) = at.parent() {
            fs::create_dir_all(parent)?;
        }
        match node {
            Node::File { mode, text } => {
                fs::write(&at, text)?;
                fs::set_permissions(&at, fs::Permissions::from_mode(*mode))
            }
            Node::Dir(mode) => {
                fs::create_dir_all(&at)?;
                fs::set_permissions(&at, fs::Permissions::from_mode(*mode))
            }
            Node::Link(target) => symlink(target, &at),
            Node::Fifo => hostkeeper_testkit::fifo(&at, 0o644),
        }
    });
    match made {
        Ok(()) => out(0, "", ""),
        Err(e) => out(2, "", &format!("dpkg-deb: error: {e}\n")),
    }
}
