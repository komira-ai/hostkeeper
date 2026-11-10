//! The fakes of the `coordinator-update` tests: Docker held in memory (containers, images,
//! restart counts, failures a test names), the coordinator's version route, and the pause, which
//! counts what it was asked and never waits.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use super::engine::{Engine, Probe, Probed};
use super::{Pause, Ports, Releases};
use crate::release::contract::{Repo, Version};
use crate::release::ports::{PortError, Release, StoredAsset};

/// The id of the container the tests start with, and of the one the verb creates.
pub(super) const OLD_ID: &str = "oldid0000000000";
pub(super) const NEW_ID: &str = "newid0000000000";

/// One container of the fake: its inspect JSON and what it is doing.
#[derive(Clone)]
pub(super) struct Container {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) inspect: Value,
    pub(super) running: bool,
    pub(super) restarting: bool,
    pub(super) restart_count: u64,
    pub(super) logs: String,
}

/// What the new container does once started.
#[derive(Clone, Copy, Default)]
pub(super) struct NewBehaviour {
    /// It stops again at once.
    pub(super) exits: bool,
    pub(super) restarting: bool,
    pub(super) restart_count: u64,
}

/// Docker, in memory. Every call is recorded as `<operation> <argument>...`; a call listed in
/// `fail` (by that text) fails.
#[derive(Default)]
pub(super) struct FakeEngine {
    pub(super) containers: RefCell<Vec<Container>>,
    /// Images by tag or id.
    pub(super) images: RefCell<BTreeMap<String, Value>>,
    pub(super) calls: RefCell<Vec<String>>,
    pub(super) fail: RefCell<BTreeSet<String>>,
    /// What the load puts in the images: the tag and the id it gets.
    pub(super) loads_as: RefCell<Option<(String, String)>>,
    /// The bodies of the creates.
    pub(super) created: RefCell<Vec<(String, Value)>>,
    pub(super) new_behaviour: Cell<NewBehaviour>,
    /// Containers that start and are not running afterwards.
    pub(super) stay_down: RefCell<BTreeSet<String>>,
    /// The inspect of this container answers a 404 (a container that vanished).
    pub(super) vanish: RefCell<BTreeSet<String>>,
}

impl FakeEngine {
    pub(super) fn failing(&self, call: &str) {
        self.fail.borrow_mut().insert(call.to_owned());
    }

    fn record(&self, call: String) -> Result<(), PortError> {
        let fails = self.fail.borrow().contains(&call);
        self.calls.borrow_mut().push(call.clone());
        if fails {
            Err(PortError::new(
                format!("docker {call}"),
                "HTTP 500: it failed",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn with<T>(&self, key: &str, f: impl FnOnce(&mut Container) -> T) -> Option<T> {
        let mut all = self.containers.borrow_mut();
        all.iter_mut()
            .find(|c| (c.id == key) | (c.name == key))
            .map(f)
    }

    /// `name`'s running state.
    pub(super) fn is_running(&self, key: &str) -> bool {
        self.with(key, |c| c.running).unwrap_or(false)
    }

    pub(super) fn exists(&self, key: &str) -> bool {
        self.with(key, |_| ()).is_some()
    }
}

impl Engine for FakeEngine {
    fn container(&self, key: &str) -> Result<Option<Value>, PortError> {
        self.record(format!("inspect {key}"))?;
        if self.vanish.borrow().contains(key) {
            return Ok(None);
        }
        Ok(self.with(key, |c| {
            let mut inspect = c.inspect.clone();
            inspect["Id"] = json!(c.id);
            inspect["Name"] = json!(format!("/{}", c.name));
            inspect["State"] = json!({"Running": c.running, "Restarting": c.restarting});
            inspect["RestartCount"] = json!(c.restart_count);
            inspect
        }))
    }

    fn image(&self, reference: &str) -> Result<Option<Value>, PortError> {
        self.record(format!("image {reference}"))?;
        Ok(self.images.borrow().get(reference).cloned())
    }

    fn load(&self, tar: &Path) -> Result<(), PortError> {
        self.record(format!(
            "load {}",
            tar.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ))?;
        if let Some((tag, id)) = self.loads_as.borrow().clone() {
            self.images.borrow_mut().insert(tag, json!({ "Id": id }));
        }
        Ok(())
    }

    fn create(&self, name: &str, body: &Value) -> Result<String, PortError> {
        self.record(format!("create {name}"))?;
        self.created
            .borrow_mut()
            .push((name.to_owned(), body.clone()));
        self.containers.borrow_mut().push(Container {
            id: NEW_ID.to_owned(),
            name: name.to_owned(),
            inspect: json!({}),
            running: false,
            restarting: false,
            restart_count: 0,
            logs: "the new coordinator's last words".to_owned(),
        });
        Ok(NEW_ID.to_owned())
    }

    fn stop(&self, id: &str, timeout_secs: u64) -> Result<(), PortError> {
        self.record(format!("stop {id} {timeout_secs}"))?;
        self.with(id, |c| c.running = false);
        Ok(())
    }

    fn start(&self, id: &str) -> Result<(), PortError> {
        self.record(format!("start {id}"))?;
        let behaviour = self.new_behaviour.get();
        let down = self.stay_down.borrow().contains(id);
        self.with(id, |c| {
            c.running = !down;
            if c.id == NEW_ID {
                c.running = !behaviour.exits;
                c.restarting = behaviour.restarting;
                c.restart_count = behaviour.restart_count;
            }
        });
        Ok(())
    }

    fn remove(&self, id: &str) -> Result<(), PortError> {
        self.record(format!("remove {id}"))?;
        self.containers.borrow_mut().retain(|c| c.id != id);
        Ok(())
    }

    fn rename(&self, id: &str, name: &str) -> Result<(), PortError> {
        self.record(format!("rename {id} {name}"))?;
        self.with(id, |c| c.name = name.to_owned());
        Ok(())
    }

    fn logs(&self, id: &str, tail: u32) -> Result<String, PortError> {
        self.record(format!("logs {id} {tail}"))?;
        Ok(self.with(id, |c| c.logs.clone()).unwrap_or_default())
    }
}

/// The coordinator's version route: the answer to a plain query, and the answers to the
/// `at_least` queries in order (the last repeats).
pub(super) struct FakeProbe {
    pub(super) plain: Probed,
    pub(super) at_least: RefCell<Vec<Probed>>,
    pub(super) calls: RefCell<Vec<Option<Version>>>,
}

impl FakeProbe {
    pub(super) fn new(plain: Probed, at_least: Vec<Probed>) -> FakeProbe {
        FakeProbe {
            plain,
            at_least: RefCell::new(at_least),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Probe for FakeProbe {
    fn version(&self, at_least: Option<&Version>) -> Probed {
        self.calls.borrow_mut().push(at_least.copied());
        if at_least.is_none() {
            return self.plain.clone();
        }
        let mut answers = self.at_least.borrow_mut();
        if answers.len() > 1 {
            answers.remove(0)
        } else {
            answers
                .first()
                .cloned()
                .unwrap_or_else(|| Probed::Failed("not scripted".to_owned()))
        }
    }
}

/// A pause that records the seconds it was asked to wait.
#[derive(Default)]
pub(super) struct FakePause {
    pub(super) waits: RefCell<Vec<u64>>,
}

impl Pause for FakePause {
    fn pause(&self, secs: u64) {
        self.waits.borrow_mut().push(secs);
    }
}

/// `Ports` over the fakes.
pub(super) fn ports<'a>(
    source: &'a FakeSource,
    verifier: &'a crate::release::fakes::FakeVerifier,
    engine: &'a FakeEngine,
    probe: &'a FakeProbe,
    pause: &'a FakePause,
) -> Ports<'a> {
    Ports {
        source,
        verifier,
        engine,
        probe,
        pause,
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

impl Releases for FakeSource {
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
