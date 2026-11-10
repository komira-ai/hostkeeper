//! hello's failing build on a native install: the farm proof of docs/hello.md section 4 over the
//! apt installer. hello is installed from the repository entry `komira-testing` at 1.2.3, then
//! ordered 1.2.3-fail-ready, whose package is 1.2.3~fail.ready of the entry `komira-testing-fail`.
//! hello's real hook runs in process over a fake host (which slot serves which version), and apt,
//! dpkg and dpkg-deb are the repository installer's fake, answering the daemon's own step
//! launcher.
//!
//! The fake serves one repository per URI, so each suite of the shared apt repository is a
//! `file:` URI of its own here; the host's two entries differ in name, URI and suite as the
//! real ones differ in name and suite.
//!
//! Linux only: hello's `.deb` and its systemd units (`hello@a.service`) exist on Linux alone, and a
//! Mac's daemon refuses a run entry that is no launchd label before any step runs.
#![cfg(target_os = "linux")]

mod support;
mod support_apt;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use hello::hook;
use hello::ports::{Host, Probe, Runner};
use hello::server::{self, Answer};
use hostkeeper_daemon::testing::ScriptedLauncher;
use hostkeeper_daemon::{Daemon, Ports, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_install::repository::config::{Repositories, parse};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, Param, StepBounds, Target, param,
};
use hostkeeper_release::Installer;
use hostkeeper_steps::{LaunchError, StepCall, StepExit, StepLauncher, ToolCall, ToolExit};
use support::{OWN, config, install, update};
use support_apt::seen::{failed, held};
use support_apt::{AptRig, Deb, with_entry};

const SERVICE: &str = "hello";
const PORT: u16 = 9310;
const NORMAL: &str = "komira-testing";
const FAIL: &str = "komira-testing-fail";
const NORMAL_URI: &str = "file:/srv/hostkeeper-apt";
const FAIL_URI: &str = "file:/srv/hostkeeper-apt-fail";
/// The release, the failing build's service version and its Debian version.
const RELEASE: &str = "1.2.3";
const FAIL_SERVICE_VERSION: &str = "1.2.3-fail-ready";
const FAIL_DEB_VERSION: &str = "1.2.3~fail.ready";
/// hello's program, which the entry's hook command names with `update-hook`, as `release targets`
/// writes it.
const PROGRAM: &str = "/usr/lib/hello/hello";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Serving {
    version: String,
    fail_ready: bool,
}

/// The fake host: the slots that serve, as `systemctl` would leave them.
#[derive(Default)]
struct World {
    serving: BTreeMap<String, Serving>,
    clashes: Vec<String>,
}

/// The `Runner` and the `Probe` of the hook over a [`World`]; a started slot serves the version
/// `current` names at that moment.
struct Fake {
    world: Arc<Mutex<World>>,
    current: PathBuf,
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
    code: i32,
    /// The hook's executable.
    executable: PathBuf,
    /// The last line the hook wrote to standard output.
    last_line: Option<String>,
}

/// The step launcher of the test: hello's real hook runs in process for the steps, and the
/// rig's launcher answers the tool runs (apt, dpkg) as it does for every apt test.
struct HelloLauncher {
    fake: Fake,
    tools: Arc<ScriptedLauncher>,
    ran: Mutex<Vec<Ran>>,
}

impl HelloLauncher {
    fn steps(&self) -> Vec<Step> {
        lock(&self.ran).iter().map(|ran| ran.step).collect()
    }
}

impl StepLauncher for HelloLauncher {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        // The entry's hook command is hello's program with `update-hook`, its leading argument.
        assert_eq!(call.hook_args, ["update-hook".to_owned()]);
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
            executable: call.executable.to_path_buf(),
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

    fn run_tool(&self, call: &ToolCall<'_>) -> Result<ToolExit, LaunchError> {
        self.tools.run_tool(call)
    }
}

/// The unit of `slot` in hello's `.deb`.
fn unit(slot: &str, fail_ready: bool) -> String {
    let flag = if fail_ready { " --fail-ready" } else { "" };
    format!(
        "[Unit]\nConditionPathExists=/var/lib/updater/hooks/hello/slots/{slot}\n\
         [Service]\nExecStart=/usr/lib/hello/hello --port {PORT}{flag}\n\
         [Install]\nWantedBy=multi-user.target\n"
    )
}

/// hello's `.deb` at `version`: the program and one unit per slot.
fn hello_deb(version: &str, fail_ready: bool) -> Deb {
    let deb = Deb::new(SERVICE, version, "amd64").file(
        PROGRAM.trim_start_matches('/'),
        0o755,
        "#!/bin/sh\n",
    );
    ["a", "b"].into_iter().fold(deb, |deb, slot| {
        deb.file(
            &format!("usr/lib/systemd/system/hello@{slot}.service"),
            0o644,
            &unit(slot, fail_ready),
        )
    })
}

/// The entry of hello's package `package_version` of `repository`, as `release targets` writes
/// it from hello's archive manifest.
fn entry(repository: &str, package_version: &str, pin: &str, record_dir: &Path) -> Target {
    let text = |value: &str| Param {
        value: Some(param::Value::Text(value.to_owned())),
    };
    let params = [
        ("units".to_owned(), text("systemd")),
        (
            "port".to_owned(),
            Param {
                value: Some(param::Value::Integer(i64::from(PORT))),
            },
        ),
        (
            "record_dir".to_owned(),
            text(&record_dir.display().to_string()),
        ),
    ];
    Target {
        installer: Installer::Apt.as_str().to_owned(),
        digest: pin.to_owned(),
        manifest: Some(Manifest {
            min_hostkeeper: "0.1.0".to_owned(),
            package: Some(SERVICE.to_owned()),
            run: vec!["hello@a.service".to_owned(), "hello@b.service".to_owned()],
            health_url: Some(format!("http://127.0.0.1:{PORT}/readyz")),
            params: params.into_iter().collect(),
            hook: Some(Hook {
                command: vec![PROGRAM.to_owned(), "update-hook".to_owned()],
                timeout_secs: Some(StepBounds {
                    prepare: 30,
                    cutover: 60,
                    verify: 30,
                    rollback: 60,
                    cleanup: 30,
                    ..StepBounds::default()
                }),
                verify_deadline_secs: 120,
                ..Hook::default()
            }),
            installer_bounds: Some(InstallerBounds {
                stage_secs: 60,
                activate_secs: 60,
                remove_secs: 60,
            }),
            ..Manifest::default()
        }),
        repository: Some(repository.to_owned()),
        package_version: Some(package_version.to_owned()),
        ..Target::default()
    }
}

/// The host's repository entries: `komira-testing`, and `komira-testing-fail` when `with_fail`.
fn repositories(keys: &Path, with_fail: bool) -> Repositories {
    fs::create_dir_all(keys).expect("the keys directory");
    let entry = |name: &str, uri: &str, suite: &str| {
        let text = format!(
            "Types: deb\nURIs: {uri}\nSuites: {suite}\nComponents: main\nSigned-By: {}\n",
            keys.join(format!("{name}.gpg")).display()
        );
        parse(name, &text, keys).expect("a repository entry")
    };
    let mut all = vec![entry(NORMAL, NORMAL_URI, "testing")];
    if with_fail {
        all.push(entry(FAIL, FAIL_URI, "testing-fail"));
    }
    Repositories::new(all)
}

/// The host: the apt rig whose daemon runs hello's hook.
struct Native {
    apt: AptRig,
    world: Arc<Mutex<World>>,
    launcher: Arc<HelloLauncher>,
    daemon: Daemon,
    record_dir: PathBuf,
    release_pin: String,
    fail_pin: String,
}

impl Native {
    /// A host with both suites published (the normal suite lists 1.2.3, the failing suite lists
    /// 1.2.3~fail.ready alone) whose configured entries are those of `with_fail`.
    fn new(name: &str, with_fail: bool) -> Native {
        let apt = AptRig::new(name);
        let record_dir = apt.rig.scratch.path().join("hello-record");
        let release_pin = apt.host.publish(NORMAL_URI, hello_deb(RELEASE, false));
        let fail_pin = apt
            .host
            .publish(FAIL_URI, hello_deb(FAIL_DEB_VERSION, true));
        let world = Arc::new(Mutex::new(World::default()));
        let launcher = Arc::new(HelloLauncher {
            fake: Fake {
                world: Arc::clone(&world),
                current: apt.rig.packages.join(SERVICE).join("current"),
                fail_ready: BTreeSet::from([FAIL_SERVICE_VERSION.to_owned()]),
            },
            tools: apt.rig.launcher.clone(),
            ran: Mutex::default(),
        });
        let keys = apt.rig.scratch.path().join("keys");
        let ports = Ports {
            launcher: launcher.clone(),
            repository: apt.rig.repository_ports(repositories(&keys, with_fail)),
            ..apt.ports()
        };
        let daemon = Daemon::new(config(), apt.rig.dirs(), ports, OWN);
        Native {
            apt,
            world,
            launcher,
            daemon,
            record_dir,
            release_pin,
            fail_pin,
        }
    }

    fn install_release(&self) -> Reply {
        let target = entry(NORMAL, RELEASE, &self.release_pin, &self.record_dir);
        self.daemon.order(&with_entry(
            install(SERVICE, RELEASE, &self.release_pin, 1),
            &target,
        ))
    }

    fn order_fail_ready(&self) -> Reply {
        let target = entry(FAIL, FAIL_DEB_VERSION, &self.fail_pin, &self.record_dir);
        self.daemon.order(&with_entry(
            update(SERVICE, FAIL_SERVICE_VERSION, &self.fail_pin, 2),
            &target,
        ))
    }

    fn serving(&self) -> Vec<(String, String)> {
        lock(&self.world)
            .serving
            .iter()
            .map(|(slot, serving)| (slot.clone(), serving.version.clone()))
            .collect()
    }

    fn link(&self, link: &str) -> Option<String> {
        fs::read_link(self.apt.rig.packages.join(SERVICE).join(link))
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }

    /// How many runs of the fake apt carried `label` so far.
    fn runs_of(&self, label: &str) -> usize {
        self.apt.host.calls_of(label).len()
    }
}

fn succeeded() -> Reply {
    support_apt::seen::succeeded()
}

fn slot_a() -> Vec<(String, String)> {
    vec![("a".to_owned(), RELEASE.to_owned())]
}

/// Proves hello's failing build is rolled back on a native install: from hello 1.2.3 installed
/// of `komira-testing`, the order for 1.2.3-fail-ready (package 1.2.3~fail.ready of
/// `komira-testing-fail`) stages and activates the failing package, hello's real hook starts the
/// other slot, `verify` sees `/readyz` answer 503 and reports `not_ready`, and the daemon puts the
/// kept 1.2.3 back with no new fetch: the verdict is `Failed`, `release`, `not_ready` at the
/// release gate, dpkg holds hello 1.2.3 again, `current` names 1.2.3, the old slot serves 1.2.3
/// and answers `/readyz` 200, and no two slots shared the port.
///
/// Catches: a failing build that stays installed (the package or `current` left at the failing
/// version), a rollback that asks a repository again, or a native hook that does not see 503.
#[test]
fn a_fail_ready_build_ordered_to_a_native_host_is_rolled_back_to_the_release() {
    let host = Native::new("hello-native-fail-ready", true);
    assert_eq!(host.install_release(), succeeded());
    assert_eq!(host.apt.host.installed(SERVICE), held(RELEASE));
    assert_eq!(host.serving(), slot_a());
    assert_eq!(host.link("current").as_deref(), Some(RELEASE));
    assert_eq!(
        host.launcher.steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );

    let (updates, downloads) = (
        host.runs_of("apt-get update"),
        host.runs_of("apt-get download"),
    );
    let reply = host.order_fail_ready();
    assert_eq!(
        reply,
        Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::GateRelease,
                class: Class::Release,
            },
            reason: Some("not_ready".to_owned()),
            resent: false,
        }
    );
    let ran = lock(&host.launcher.ran).clone();
    let of_fail: Vec<_> = ran
        .iter()
        .filter(|ran| {
            ran.executable
                .to_string_lossy()
                .contains(FAIL_SERVICE_VERSION)
        })
        .collect();
    assert_eq!(
        of_fail.iter().map(|ran| ran.step).collect::<Vec<_>>(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Rollback]
    );
    assert_eq!(
        of_fail.iter().map(|ran| ran.code).collect::<Vec<_>>(),
        [0, 0, 1, 0]
    );
    assert_eq!(
        of_fail[2].last_line.as_deref(),
        Some(r#"{"class":"release","reason":"not_ready","detail":"GET /readyz answered 503"}"#)
    );
    assert_eq!(host.apt.host.installed(SERVICE), held(RELEASE));
    assert_eq!(host.link("current").as_deref(), Some(RELEASE));
    assert_eq!(host.serving(), slot_a());
    assert_eq!(
        (
            host.runs_of("apt-get update"),
            host.runs_of("apt-get download")
        ),
        (updates + 1, downloads + 1),
        "only the stage of the failing build asked a repository; the rollback asked none"
    );
    assert!(lock(&host.world).clashes.is_empty());
    let (status, body) = Probe::get(&host.launcher.fake, PORT, "/readyz").expect("a slot answers");
    assert_eq!((status, body.as_str()), (200, "ready\n"));
    let (status, body) = Probe::get(&host.launcher.fake, PORT, "/version").expect("a slot answers");
    assert_eq!((status, body.as_str()), (200, "1.2.3\n"));
}

/// Proves a host without the entry `komira-testing-fail` never takes the failing build: the order
/// is `Failed` before the commit, `environment`, `repository_unknown` at the stage, and hello
/// 1.2.3 keeps running untouched (installed, held, `current` and the serving slot unchanged, no
/// step run, no version directory of the failing build).
///
/// Catches: a failing build that is fetched from the normal entry's suite, or a refused order
/// that disturbs the running release.
#[test]
fn a_host_without_the_fail_entry_refuses_the_fail_ready_build_and_keeps_the_release() {
    let host = Native::new("hello-native-no-fail-entry", false);
    assert_eq!(host.install_release(), succeeded());
    let steps = host.launcher.steps();
    let reply = host.order_fail_ready();
    assert_eq!(reply, failed(Class::Environment, "repository_unknown"));
    assert_eq!(
        host.launcher.steps(),
        steps,
        "no step ran for the refused order"
    );
    assert_eq!(host.apt.host.installed(SERVICE), held(RELEASE));
    assert_eq!(host.link("current").as_deref(), Some(RELEASE));
    assert_eq!(host.serving(), slot_a());
    assert!(
        !host
            .apt
            .rig
            .packages
            .join(SERVICE)
            .join(FAIL_SERVICE_VERSION)
            .exists()
    );
}
