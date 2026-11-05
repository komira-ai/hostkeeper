//! What the tests of `force` share: replies, packages with units, the links and calls a force
//! leaves, and a daemon on another filesystem or prune.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::prune::Prune;
use hostkeeper_daemon::testing::NoPrune;
use hostkeeper_daemon::{Daemon, Placement, Ports, Reply};
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_placed::placer::is_temporary;
use hostkeeper_placed::testing::{Event, FaultFs, FsWrite, Log as PlacedLog, UnitCall};

use super::{OWN, Rig, config, d, force, roots};

/// The service the force tests switch.
pub const WEB: &str = "web";
/// Its unit, which every version of the simple layout runs.
pub const UNIT: &str = "web.service";

/// A force that ran to `Succeeded`, now.
pub fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// An order that ran to `Failed` in `phase`, blamed on `class`, with `reason`, now.
pub fn failed(phase: Phase, class: Class, reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Failed { phase, class },
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

/// The token of a refusal, or the reason of a failure.
pub fn token_of(reply: &Reply) -> Option<String> {
    match reply {
        Reply::Refused { refusal, .. } | Reply::Interrupted { refusal, .. } => {
            Some(refusal.token().to_owned())
        }
        Reply::Finished {
            verdict: Verdict::Failed { .. },
            reason,
            ..
        } => reason.clone(),
        Reply::Finished { .. } | Reply::Running => None,
    }
}

/// `version` of `web` with the one unit [`UNIT`], which it runs.
pub fn web(rig: &Rig, version: &str) {
    rig.add_units(WEB, version, &[UNIT], &[UNIT], &[]);
}

/// `daemon` forces `web` to `version` with `order_try`, and that runs to `Succeeded`; then the
/// log of placed writes and unit calls is cleared.
pub fn forced(rig: &Rig, daemon: &Daemon, version: &str, order_try: u64) {
    assert_eq!(
        daemon.order(&force(WEB, version, &d(), order_try, 0)),
        succeeded(),
        "the force of {version}"
    );
    rig.placed_log.clear();
}

/// The version the link `link` (`current`, `previous`) of `service` names.
pub fn link_of(rig: &Rig, service: &str, link: &str) -> Option<String> {
    fs::read_link(rig.packages.join(service).join(link))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

/// The calls and writes of a force in the rig's log, coarsely: `stop`, `start` and `reload` of
/// the service manager, `unlink` and `link` of a placed path, and `switch` for the rename of
/// `current`. Writes of `placed.json`, syncs, temporary links and unit queries are left out.
pub fn calls(rig: &Rig) -> Vec<String> {
    rig.placed_log
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Unit(UnitCall::Stop(unit)) => Some(format!("stop {unit}")),
            Event::Unit(UnitCall::Start(unit)) => Some(format!("start {unit}")),
            Event::Unit(UnitCall::DaemonReload) => Some("reload".to_owned()),
            Event::Fs(FsWrite::Remove(path)) => Some(format!("unlink {}", path.display())),
            Event::Fs(FsWrite::Rename { to, .. }) if to.ends_with("current") => {
                Some("switch".to_owned())
            }
            Event::Fs(FsWrite::Symlink { link, .. }) if !temporary(&link) => {
                Some(format!("link {}", link.display()))
            }
            Event::Unit(UnitCall::IsActive(_)) | Event::Fs(_) => None,
        })
        .collect()
}

fn temporary(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| is_temporary(&name.to_string_lossy()))
}

/// How often the service manager was asked whether `unit` is active.
pub fn asked_active(rig: &Rig, unit: &str) -> usize {
    rig.placed_log
        .events()
        .into_iter()
        .filter(|event| *event == Event::Unit(UnitCall::IsActive(unit.to_owned())))
        .count()
}

/// The units the service manager was asked to start, in order.
pub fn started(rig: &Rig) -> Vec<String> {
    rig.placed_log
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Unit(UnitCall::Start(unit)) => Some(unit),
            _ => None,
        })
        .collect()
}

/// The names of `units` as a set.
pub fn set(units: &[&str]) -> BTreeSet<String> {
    units.iter().map(|unit| (*unit).to_owned()).collect()
}

/// A daemon on the rig, linking through `fs` and pruning with `prune`.
pub fn daemon_on(rig: &Rig, fs: Arc<FaultFs>, prune: Arc<dyn Prune>) -> Daemon {
    let ports = Ports {
        placement: Placement { roots: roots(), fs },
        prune,
        ..rig.ports()
    };
    Daemon::new(config(), rig.dirs(), ports, OWN)
}

/// The number of the first write, counted from 1, that a force of `web` to 2.0.0 makes after its
/// switch of `current` (the first write of step 12), and of its link of the file whose name is
/// `file`: what the rig `installed` makes, its force run without a fault, writes.
pub fn writes_of_step_12(installed: fn(&str) -> Rig, file: &str) -> (u64, u64) {
    let rig = installed("force-probe");
    let log = PlacedLog::new();
    let fs = Arc::new(FaultFs::new(rig.scratch.path(), log.clone()));
    let daemon = daemon_on(&rig, fs, Arc::new(NoPrune));
    assert_eq!(daemon.order(&force(WEB, "2.0.0", &d(), 2, 0)), succeeded());
    let writes: Vec<FsWrite> = log
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Fs(write) => Some(write),
            Event::Unit(_) => None,
        })
        .collect();
    let switched = writes
        .iter()
        .position(|write| matches!(write, FsWrite::Rename { to, .. } if to.ends_with("current")))
        .expect("the switch");
    let link = writes
        .iter()
        .position(|write| matches!(write, FsWrite::Symlink { link, .. } if link.ends_with(file)))
        .expect("the link of the file");
    // The switch ends with the sync of the service's directory: the write after that is the
    // first of step 12.
    (switched as u64 + 3, link as u64 + 1)
}

/// What a force leaves on the host: `current` and `previous`, every link under the scratch
/// root's `units` and `etc` with its target, `placed.json`, and the active units.
#[derive(Debug, PartialEq, Eq)]
pub struct HostState {
    pub current: Option<String>,
    pub previous: Option<String>,
    pub links: Vec<(PathBuf, PathBuf)>,
    pub placed: serde_json::Value,
    pub active: BTreeSet<String>,
}

pub fn host_state(rig: &Rig) -> HostState {
    let root = rig.scratch.path();
    let mut links = Vec::new();
    let mut stack = vec![root.join("units"), root.join("etc")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("an entry").path();
            let meta = fs::symlink_metadata(&path).expect("its metadata");
            match (meta.is_dir(), meta.file_type().is_symlink()) {
                (true, _) => stack.push(path),
                (false, true) => {
                    let target = fs::read_link(&path).expect("the link");
                    links.push((
                        path.strip_prefix(root).expect("below").to_path_buf(),
                        target,
                    ));
                }
                (false, false) => {}
            }
        }
    }
    links.sort();
    let placed = fs::read(root.join("placed").join("placed.json")).expect("placed.json");
    HostState {
        current: link_of(rig, WEB, "current"),
        previous: link_of(rig, WEB, "previous"),
        links,
        placed: serde_json::from_slice(&placed).expect("placed.json is JSON"),
        active: rig.units.active(),
    }
}
