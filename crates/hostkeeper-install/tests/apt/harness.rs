//! What the apt suite shares: the host of a test (scratch directory, the fake, the records port,
//! the lock, a recording pause), the order of the service `mdm`, the package models it installs
//! and the assertions on a failure.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::repository::apt::Apt;
use hostkeeper_install::repository::config::parse;
use hostkeeper_install::repository::{
    Bounds, Class, Failure, HostDirs, InstallerLock, Order, Outcome, PackageRecord, Pause, Reason,
    RecordError, Records, Repositories, RepositoryInstaller, Staged,
};

use crate::apt_fake::{Deb, Events, FakeHost, Inst};
use crate::support::Scratch;

pub const SERVICE: &str = "mdm";
pub const PACKAGE: &str = "mdm";
pub const REPO: &str = "main";
pub const URI: &str = "https://repo.example.test/debian";
pub const MAX: u64 = 1_048_576;
pub const STAGE_SECS: u64 = 60;
pub const ACTIVATE_SECS: u64 = 120;
pub const REMOVE_SECS: u64 = 10;
pub const V1: &str = "1.4.0-1";
pub const V2: &str = "1.5.0-1";
pub const V3: &str = "1.6.0-1";

pub fn service(name: &str) -> ServiceName {
    ServiceName::new(name).expect("a service name")
}

/// The records port in memory, with the failures a test asks for.
#[derive(Default)]
pub struct MemRecords {
    pub map: Mutex<BTreeMap<String, PackageRecord>>,
    pub saves: Mutex<Vec<PackageRecord>>,
    pub fail: Mutex<RecordFaults>,
    pub attempts: Mutex<usize>,
    pub events: Events,
}

#[derive(Default, Clone, Copy)]
pub struct RecordFaults {
    pub load: bool,
    pub holder: bool,
    pub clear: bool,
    /// Every save fails.
    pub save: bool,
    /// The save with this index (counting every attempt from 0) fails.
    pub save_nth: Option<usize>,
}

fn broken(what: &str) -> RecordError {
    RecordError(format!("{what} failed"))
}

impl MemRecords {
    pub fn get(&self, service: &str) -> Option<PackageRecord> {
        self.map.lock().expect("records").get(service).cloned()
    }

    pub fn put(&self, service: &str, record: PackageRecord) {
        self.map
            .lock()
            .expect("records")
            .insert(service.to_owned(), record);
    }

    pub fn faults(&self, f: impl FnOnce(&mut RecordFaults)) {
        f(&mut self.fail.lock().expect("faults"));
    }

    pub fn saved(&self) -> Vec<PackageRecord> {
        self.saves.lock().expect("saves").clone()
    }

    fn log(&self, text: String) {
        self.events.lock().expect("events").push(text);
    }
}

impl Records for MemRecords {
    fn load(&self, service: &ServiceName) -> Result<Option<PackageRecord>, RecordError> {
        if self.fail.lock().expect("faults").load {
            return Err(broken("load"));
        }
        Ok(self.get(service.as_str()))
    }

    fn save(&self, service: &ServiceName, record: &PackageRecord) -> Result<(), RecordError> {
        let nth = {
            let mut attempts = self.attempts.lock().expect("attempts");
            *attempts += 1;
            *attempts - 1
        };
        let faults = *self.fail.lock().expect("faults");
        if faults.save | (faults.save_nth == Some(nth)) {
            return Err(broken("save"));
        }
        let state = if record.pending { "pending" } else { "done" };
        self.log(format!("save {state} {}", record.version));
        self.saves.lock().expect("saves").push(record.clone());
        self.put(service.as_str(), record.clone());
        Ok(())
    }

    fn clear(&self, service: &ServiceName) -> Result<(), RecordError> {
        if self.fail.lock().expect("faults").clear {
            return Err(broken("clear"));
        }
        self.log("clear".to_owned());
        self.map.lock().expect("records").remove(service.as_str());
        Ok(())
    }

    fn holder(
        &self,
        package: &str,
        service: &ServiceName,
    ) -> Result<Option<ServiceName>, RecordError> {
        if self.fail.lock().expect("faults").holder {
            return Err(broken("holder"));
        }
        let map = self.map.lock().expect("records");
        Ok(map
            .iter()
            .find(|(name, r)| (r.package == package) & (name.as_str() != service.as_str()))
            .map(|(name, _)| ServiceName::new(name).expect("a service name")))
    }
}

/// A pause that records the seconds it was asked to wait and waits for none.
#[derive(Default)]
pub struct RecPause(pub Mutex<Vec<u64>>);

impl Pause for RecPause {
    fn pause(&self, secs: u64) {
        self.0.lock().expect("pauses").push(secs);
    }
}

impl RecPause {
    pub fn calls(&self) -> Vec<u64> {
        self.0.lock().expect("pauses").clone()
    }
}

/// What a test's host holds, to compare before and after a step.
#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub installed: BTreeMap<String, Inst>,
    pub records: BTreeMap<String, PackageRecord>,
    pub root: Vec<String>,
}

/// One test's host.
pub struct Env {
    pub scratch: Scratch,
    pub host: FakeHost,
    pub records: MemRecords,
    pub pause: RecPause,
    pub lock: InstallerLock,
    pub repositories: Repositories,
    pub dirs: HostDirs,
    pub events: Events,
    staged: Mutex<u32>,
}

impl Env {
    pub fn new(name: &str) -> Env {
        let scratch = Scratch::new(name);
        let base = scratch.path().to_path_buf();
        let (root, work, keys) = (base.join("host"), base.join("work"), base.join("keys"));
        for dir in [&root, &work, &keys] {
            fs::create_dir_all(dir).expect("a directory");
        }
        let events = Events::default();
        let lock_path = base.join("installer.lock");
        let text = format!(
            "Types: deb\nURIs: {URI}\nSuites: stable\nComponents: main\nSigned-By: {}\n",
            keys.join("main.gpg").display()
        );
        let entry = parse(REPO, &text, &keys).expect("a repository entry");
        Env {
            host: FakeHost::new(&lock_path, Arc::clone(&events)),
            records: MemRecords {
                events: Arc::clone(&events),
                ..MemRecords::default()
            },
            pause: RecPause::default(),
            lock: InstallerLock::new(&lock_path),
            repositories: Repositories::new(vec![entry]),
            dirs: HostDirs { root, work },
            events,
            scratch,
            staged: Mutex::new(0),
        }
    }

    pub fn installer(&self) -> RepositoryInstaller<'_> {
        RepositoryInstaller {
            backend: &Apt,
            tools: &self.host,
            records: &self.records,
            lock: &self.lock,
            repositories: &self.repositories,
            dirs: &self.dirs,
            pause: &self.pause,
        }
    }

    /// The work directory of the service `mdm`.
    pub fn work(&self) -> PathBuf {
        self.dirs.work.join(SERVICE)
    }

    /// Publishes `deb` in the repository, answers the order for it.
    pub fn publish(&self, deb: &Deb) -> Order {
        self.host.publish(URI, deb.clone());
        order(deb, &["mdm.service"])
    }

    /// A new empty staging directory.
    pub fn staging(&self) -> PathBuf {
        let mut n = self.staged.lock().expect("staging counter");
        *n += 1;
        let dir = self.scratch.path().join(format!("staging-{n}"));
        fs::create_dir(&dir).expect("a staging directory");
        dir
    }

    /// Stages `order` in a new staging directory.
    pub fn stage(&self, order: &Order) -> (Result<Staged, Failure>, PathBuf) {
        let dir = self.staging();
        (self.installer().stage(order, &dir), dir)
    }

    /// Publishes `deb`, stages it and expects it to be staged.
    pub fn staged(&self, deb: &Deb, run: &[&str]) -> (Order, PathBuf) {
        let mut order = self.publish(deb);
        order.run = run.iter().map(|u| (*u).to_owned()).collect();
        let (staged, dir) = self.stage(&order);
        staged.expect("the version stages");
        (order, dir)
    }

    /// Publishes, stages and activates `deb`.
    pub fn installed(&self, deb: &Deb, run: &[&str]) -> (Order, PathBuf) {
        let (order, dir) = self.staged(deb, run);
        self.installer()
            .activate(&order, &dir)
            .expect("the version activates");
        (order, dir)
    }

    pub fn event_log(&self) -> Vec<String> {
        self.events.lock().expect("events").clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            installed: self.host.all_installed(),
            records: self.records.map.lock().expect("records").clone(),
            root: listing(&self.dirs.root),
        }
    }

    /// Asserts that `before` is what the host still holds, and that no run changed a package.
    pub fn assert_unchanged(&self, before: &Snapshot) {
        assert_eq!(&self.snapshot(), before);
        assert!(self.records.saved().is_empty(), "a record was saved");
        let changing = [
            "apt-get install",
            "apt-mark hold",
            "apt-mark unhold",
            "dpkg --purge",
        ];
        for label in self.host.labels() {
            assert!(!changing.contains(&label.as_str()), "{label} ran");
        }
    }

    pub fn has_run(&self, label: &str) -> bool {
        self.host.labels().iter().any(|l| l == label)
    }
}

/// Every path below `dir`, relative, with a `/` after a directory; links are not followed.
pub fn listing(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, rel: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(base.join(rel)).into_iter().flatten().flatten() {
            let path = rel.join(entry.file_name());
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            out.push(format!(
                "{}{}",
                path.display(),
                if is_dir { "/" } else { "" }
            ));
            if is_dir {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, Path::new(""), &mut out);
    out.sort();
    out
}

/// The order of the service `mdm` for `deb`, with `run` as its run units.
pub fn order_of(service: &str, deb: &Deb, run: &[&str]) -> Order {
    Order {
        service: self::service(service),
        version: Version::new("1.4.0").expect("a version"),
        package: deb.package.clone(),
        package_version: deb.version.clone(),
        repository: REPO.to_owned(),
        pin: deb.pin(),
        run: run.iter().map(|u| (*u).to_owned()).collect(),
        boot: None,
        params: false,
        bounds: Bounds {
            stage: STAGE_SECS,
            activate: ACTIVATE_SECS,
            remove: REMOVE_SECS,
        },
        max_package_bytes: MAX,
    }
}

pub fn order(deb: &Deb, run: &[&str]) -> Order {
    order_of(SERVICE, deb, run)
}

/// A unit file, with an `[Install]` section when `wanted_by` names a target.
pub fn unit(wanted_by: Option<&str>) -> String {
    let mut text = "[Unit]\nDescription=mdm\n[Service]\nExecStart=/usr/bin/mdm\n".to_owned();
    if let Some(target) = wanted_by {
        text.push_str(&format!("[Install]\nWantedBy={target}\n"));
    }
    text
}

pub const UNIT_DIR: &str = "usr/lib/systemd/system";

/// The package `mdm` at `version`: its program, the unit `mdm.service` and a checksum list.
pub fn deb(version: &str) -> Deb {
    Deb::new(PACKAGE, version, "amd64")
        .file("usr/bin/mdm", 0o755, "#!/bin/sh\n")
        .file(
            &format!("{UNIT_DIR}/mdm.service"),
            0o644,
            &unit(Some("multi-user.target")),
        )
        .member("md5sums")
}

/// The same package with the eight units of the package `mdm`; three of them are run units.
pub fn mdm_deb(version: &str) -> Deb {
    let units = [
        ("mdm.socket", Some("sockets.target")),
        ("mdm.timer", Some("timers.target")),
        ("mdm-worker.service", Some("multi-user.target")),
        ("mdm-worker.timer", Some("timers.target")),
        ("mdm@.service", None),
        ("mdm-cleanup.service", None),
        ("mdm-reload.timer", None),
    ];
    units.into_iter().fold(deb(version), |d, (name, target)| {
        d.file(&format!("{UNIT_DIR}/{name}"), 0o644, &unit(target))
    })
}

/// The run units of [`mdm_deb`].
pub const RUN3: [&str; 3] = ["mdm.service", "mdm-worker.service", "mdm.timer"];

/// The enablement links of [`RUN3`], as the record names them.
pub fn run3_links() -> BTreeSet<String> {
    [
        "multi-user.target.wants/mdm.service",
        "multi-user.target.wants/mdm-worker.service",
        "timers.target.wants/mdm.timer",
    ]
    .iter()
    .map(|l| format!("/etc/systemd/system/{l}"))
    .collect()
}

/// Asserts a failure's reason, class and outcome (packages-repositories.md section 4.5).
pub fn assert_failure(failure: &Failure, reason: Reason, class: Class, outcome: Outcome) {
    assert_eq!(failure.reason(), reason, "{failure}");
    assert_eq!(failure.reason().class(), class, "{failure}");
    assert_eq!(failure.reason().outcome(), outcome, "{failure}");
}

/// The path `link` of the host's root.
pub fn at_root(env: &Env, link: &str) -> PathBuf {
    env.dirs.root.join(link.trim_start_matches('/'))
}
