//! The example service `hello` through the daemon: its real hook runs in process, over a fake
//! host, for the three updates of the farm proof (install 1.0.0, update to 2.0.0, update to a
//! 3.0.0 that is not ready and is rolled back).
//!
//! The orders are the ones the coordinator sends, given to the daemon as they are. The daemon
//! places a package built from hello's own files (its manifest and its unit template are the
//! goldens of the package), writes the params file of each step and decides from the exit status
//! and the last line of the report `hello::hook::run` printed. The host is a fake world: which
//! slot serves which version, as `systemctl` would leave it.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use hello::hook;
use hello::ports::{Host, Probe, Runner};
use hello::server::{self, Answer};
use hostkeeper_daemon::Reply;
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::{LaunchError, StepCall, StepExit, StepLauncher};
use support::{OWN, Rig, config, d, install, ok, update};

/// The service and the port its package names: only a value the fake probe receives, no socket
/// is opened.
const SERVICE: &str = "hello";
const PORT: u16 = 9310;

/// hello's own manifest and unit template, as `cargo xtask release package` builds them.
const MANIFEST: &str = include_str!("../../../examples/hello/package/linux/manifest.toml");
const UNIT: &str = include_str!("../../../examples/hello/package/linux/units/hello@.service");

/// The version the golden manifest is written for, and the directory it keeps its record in.
const GOLDEN_VERSION: &str = "version = \"1.2.3\"";
const GOLDEN_RECORD_DIR: &str = "record_dir = \"/var/lib/updater/hooks/hello\"";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What one slot serves.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Serving {
    version: String,
    fail_ready: bool,
}

/// The fake host: the slots that serve, and every command the hook ran.
#[derive(Default)]
struct World {
    /// Slot to what it serves; a slot that is not here is stopped.
    serving: BTreeMap<String, Serving>,
    /// Every `systemctl` the hook ran, as `<verb> <unit>`.
    commands: Vec<String>,
    /// Slots started while another slot served: two servers on one port.
    clashes: Vec<String>,
}

/// The `Runner` and the `Probe` of the hook over a [`World`].
struct Fake {
    world: Arc<Mutex<World>>,
    /// `<packages>/hello/current`: a started slot serves the version this names at that moment,
    /// as the unit's `ExecStart=` through `current` runs it.
    current: PathBuf,
    /// The versions whose build answers `/readyz` with 503.
    fail_ready: BTreeSet<String>,
}

impl Runner for Fake {
    fn run(&self, argv: &[&str]) -> Result<i32, String> {
        let [program, verb, unit] = argv else {
            return Err(format!("no command like this is expected: {argv:?}"));
        };
        let slot = unit
            .strip_prefix("hello@")
            .and_then(|rest| rest.strip_suffix(".service"))
            .filter(|_| *program == "/usr/bin/systemctl")
            .ok_or_else(|| format!("no command like this is expected: {argv:?}"))?;
        let mut world = lock(&self.world);
        world.commands.push(format!("{verb} {unit}"));
        match *verb {
            "start" => {
                let version = fs::read_link(&self.current)
                    .map_err(|why| format!("current is not linked: {why}"))?
                    .to_string_lossy()
                    .into_owned();
                if world.serving.keys().any(|serving| serving != slot) {
                    world.clashes.push(slot.to_owned());
                }
                let fail_ready = self.fail_ready.contains(&version);
                world.serving.insert(
                    slot.to_owned(),
                    Serving {
                        version,
                        fail_ready,
                    },
                );
                Ok(0)
            }
            "stop" => {
                world.serving.remove(slot);
                Ok(0)
            }
            other => Err(format!("no verb like {other} is expected")),
        }
    }
}

impl Probe for Fake {
    fn get(&self, port: u16, path: &str) -> Result<(u16, String), String> {
        if port != PORT {
            return Err(format!("nothing listens on port {port}"));
        }
        let world = lock(&self.world);
        let mut slots = world.serving.values();
        let (Some(serving), None) = (slots.next(), slots.next()) else {
            return Err(format!(
                "connection refused: {} slots serve",
                world.serving.len()
            ));
        };
        let head = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\n\r\n");
        let reply = server::answer(
            head.as_bytes(),
            &Answer {
                version: &serving.version,
                fail_ready: serving.fail_ready,
            },
        );
        let text = String::from_utf8(reply).map_err(|why| why.to_string())?;
        let (head, body) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| "no end of head".to_owned())?;
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|status| status.parse().ok())
            .ok_or_else(|| format!("no status in {head:?}"))?;
        Ok((status, body.to_owned()))
    }
}

/// One step as the launcher ran it.
#[derive(Clone, Debug)]
struct Ran {
    step: Step,
    /// The exit status of `hello::hook::run`.
    code: i32,
    /// The hook's executable, relative to the packages directory.
    executable: PathBuf,
    /// The last line the hook wrote to standard output.
    last_line: Option<String>,
}

/// The step launcher of the test: it runs hello's real hook in process.
///
/// The args are `Invocation::to_args` of the call, the ones the real launcher puts after
/// `<executable> update-hook`; the hook gets the fake host; the exit status and the last
/// non-blank line of what the hook printed are the step's exit.
struct HelloLauncher {
    fake: Fake,
    packages: PathBuf,
    ran: Mutex<Vec<Ran>>,
}

impl HelloLauncher {
    fn new(packages: &Path, world: Arc<Mutex<World>>, fail_ready: &[&str]) -> Arc<HelloLauncher> {
        Arc::new(HelloLauncher {
            fake: Fake {
                world,
                current: packages.join(SERVICE).join("current"),
                fail_ready: fail_ready.iter().map(|v| (*v).to_owned()).collect(),
            },
            packages: packages.to_path_buf(),
            ran: Mutex::default(),
        })
    }

    /// The steps run since the last call, in order.
    fn take(&self) -> Vec<Ran> {
        std::mem::take(&mut *lock(&self.ran))
    }
}

impl StepLauncher for HelloLauncher {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        let args: Vec<OsString> = call.invocation.to_args();
        let host = Host {
            runner: &self.fake,
            probe: &self.fake,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = hook::run(&args, &host, &mut out, &mut err);
        let last_line = String::from_utf8_lossy(&out)
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(str::to_owned);
        lock(&self.ran).push(Ran {
            step: call.invocation.step,
            code,
            executable: call
                .executable
                .strip_prefix(&self.packages)
                .unwrap_or(call.executable)
                .to_path_buf(),
            last_line: last_line.clone(),
        });
        Ok(StepExit {
            code: Some(code),
            last_line,
        })
    }

    fn stop(&self, _unit: &str) -> Result<(), LaunchError> {
        Ok(())
    }
}

/// The package of hello at `version`: the golden manifest for that version, keeping its record
/// in `record_dir`, the unit template, and a program the hook finds at `bin/hello`; sealed.
fn add_hello(rig: &Rig, version: &str, record_dir: &Path) {
    let manifest = MANIFEST
        .replace(GOLDEN_VERSION, &format!("version = \"{version}\""))
        .replace(
            GOLDEN_RECORD_DIR,
            &format!("record_dir = \"{}\"", record_dir.display()),
        );
    assert_ne!(
        manifest, MANIFEST,
        "the golden manifest has the lines this test rewrites"
    );
    assert!(
        manifest.contains(&format!("version = \"{version}\"")),
        "{manifest}"
    );
    assert!(
        manifest.contains(&record_dir.display().to_string()),
        "{manifest}"
    );
    let dir = rig.store.add(SERVICE, version, &manifest);
    let program = dir.join("bin").join("hello");
    fs::write(&program, "#!hello\n").expect("the program");
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("its mode");
    let units = dir.join("units");
    fs::create_dir_all(&units).expect("the units directory");
    fs::set_permissions(&units, fs::Permissions::from_mode(0o755)).expect("its mode");
    let unit = units.join("hello@.service");
    fs::write(&unit, UNIT).expect("the unit template");
    fs::set_permissions(&unit, fs::Permissions::from_mode(0o644)).expect("its mode");
    rig.store.seal(SERVICE, version, &d());
}

/// The version `link` (`current`, `previous`) of hello names, `None` when there is no link.
fn link(rig: &Rig, link: &str) -> Option<String> {
    fs::read_link(rig.packages.join(SERVICE).join(link))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

/// What the slots serve, as (slot, version) pairs.
fn serving(world: &Mutex<World>) -> Vec<(String, String)> {
    lock(world)
        .serving
        .iter()
        .map(|(slot, serving)| (slot.clone(), serving.version.clone()))
        .collect()
}

/// The slot the hook's record names as the serving one.
fn active(record_dir: &Path) -> String {
    fs::read_to_string(record_dir.join("active"))
        .expect("the record names the serving slot")
        .trim()
        .to_owned()
}

fn pair(slot: &str, version: &str) -> (String, String) {
    (slot.to_owned(), version.to_owned())
}

/// The hook executable of every step in `ran`, relative to the packages directory.
fn executables(ran: &[Ran]) -> Vec<PathBuf> {
    ran.iter().map(|ran| ran.executable.clone()).collect()
}

/// `count` times the hook of the release `version`.
fn hook_of(version: &str, count: usize) -> Vec<PathBuf> {
    vec![Path::new(SERVICE).join(version).join("bin").join("hello"); count]
}

fn steps(ran: &[Ran]) -> Vec<Step> {
    ran.iter().map(|ran| ran.step).collect()
}

/// The order of the test: the update of hello to `version`.
fn order(version: &str, order_try: u64) -> hostkeeper_proto::hostkeeper::agent::v1::Order {
    update(SERVICE, version, &d(), order_try)
}

/// Catches a hello release that the daemon cannot drive through its life: an install of 1.0.0
/// that is not a success, or leaves no slot serving it; an update to 2.0.0 that does not start
/// the other slot and stop the serving one; and an update to a 3.0.0 that answers 503 that is
/// not rolled back to the 2.0.0 slot, with the step order, the reason and the old release's
/// links as the design says.
#[test]
fn hello_installs_updates_and_rolls_a_failing_release_back() {
    let rig = Rig::new("hello-farm");
    let record_dir = rig.scratch.path().join("hello-record");
    // Absent: the hook makes its own record directory on its first step, as on a host.
    for version in ["1.0.0", "2.0.0", "3.0.0"] {
        add_hello(&rig, version, &record_dir);
    }
    let world = Arc::new(Mutex::new(World::default()));
    let launcher = HelloLauncher::new(&rig.packages, Arc::clone(&world), &["3.0.0"]);
    let ports = hostkeeper_daemon::Ports {
        launcher: launcher.clone(),
        ..rig.ports()
    };
    let daemon = hostkeeper_daemon::Daemon::new(config(), rig.dirs(), ports, OWN);
    let every_step = [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup];

    // 1.0.0: a first install starts slot a.
    let reply = daemon.order(&install(SERVICE, "1.0.0", &d(), 1));
    let ran = launcher.take();
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
            resent: false
        },
        "{ran:#?}"
    );
    assert_eq!(steps(&ran), every_step, "{ran:#?}");
    assert_eq!(executables(&ran), hook_of("1.0.0", 4), "{ran:#?}");
    assert!(ran.iter().all(|ran| ran.code == 0), "{ran:#?}");
    assert_eq!(rig.verdict(SERVICE), Some(ok()));
    assert_eq!(serving(&world), [pair("a", "1.0.0")]);
    assert_eq!(link(&rig, "current").as_deref(), Some("1.0.0"));
    assert_eq!(link(&rig, "previous"), None);
    assert_eq!(active(&record_dir), "a");

    // 2.0.0: an update starts slot b and stops slot a.
    let reply = daemon.order(&order("2.0.0", 2));
    let ran = launcher.take();
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
            resent: false
        },
        "{ran:#?}"
    );
    assert_eq!(steps(&ran), every_step, "{ran:#?}");
    assert_eq!(executables(&ran), hook_of("2.0.0", 4), "{ran:#?}");
    assert_eq!(rig.verdict(SERVICE), Some(ok()));
    assert_eq!(serving(&world), [pair("b", "2.0.0")]);
    assert_eq!(link(&rig, "current").as_deref(), Some("2.0.0"));
    assert_eq!(link(&rig, "previous").as_deref(), Some("1.0.0"));
    assert_eq!(active(&record_dir), "b");

    // 3.0.0 is not ready: its slot a starts, answers 503, and slot b serves 2.0.0 again.
    let reply = daemon.order(&order("3.0.0", 3));
    let ran = launcher.take();
    // No hello step reports `reversible: false`: its rollback can always start the old slot
    // again, so the update is never committed before `cleanup`, and a `verify` that fails is the
    // gate of a release (`gate_release`), not `after_commit`.
    let failed = Reply::Finished {
        verdict: Verdict::Failed {
            phase: Phase::GateRelease,
            class: Class::Release,
        },
        reason: Some("not_ready".to_owned()),
        resent: false,
    };
    assert_eq!(reply, failed, "{ran:#?}");
    assert_eq!(
        steps(&ran),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Rollback],
        "{ran:#?}"
    );
    assert_eq!(executables(&ran), hook_of("3.0.0", 4), "{ran:#?}");
    assert_eq!(
        ran.iter().map(|ran| ran.code).collect::<Vec<_>>(),
        [0, 0, 1, 0],
        "{ran:#?}"
    );
    assert_eq!(
        ran[2].last_line.as_deref(),
        Some(r#"{"class":"release","reason":"not_ready","detail":"GET /readyz answered 503"}"#),
        "{ran:#?}"
    );
    assert_eq!(link(&rig, "current").as_deref(), Some("2.0.0"));
    assert_eq!(serving(&world), [pair("b", "2.0.0")]);
    assert_eq!(active(&record_dir), "b");
    let (status, body) = Fake::get(&launcher.fake, PORT, "/readyz").expect("a slot answers");
    assert_eq!((status, body.as_str()), (200, "ready\n"));
    let (status, body) = Fake::get(&launcher.fake, PORT, "/version").expect("a slot answers");
    assert_eq!((status, body.as_str()), (200, "2.0.0\n"));
    assert_eq!(
        rig.verdict(SERVICE),
        Some(support::finished(
            Verdict::Failed {
                phase: Phase::GateRelease,
                class: Class::Release
            },
            Some("not_ready")
        ))
    );

    // The commands the hook gave the host, in order, and no two slots on the port.
    let world = lock(&world);
    assert_eq!(
        world.commands,
        [
            "start hello@a.service",
            // 2.0.0
            "stop hello@a.service",
            "start hello@b.service",
            // 3.0.0
            "stop hello@b.service",
            "start hello@a.service",
            // its rollback
            "stop hello@a.service",
            "start hello@b.service",
        ]
    );
    assert!(world.clashes.is_empty(), "{:?}", world.clashes);
}
