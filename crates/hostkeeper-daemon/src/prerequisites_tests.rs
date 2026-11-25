//! The tests of `prerequisites`: the sink over a scripted Mac, whose programs count their calls, an
//! injected clock and dispatchers that run a pass at once or keep it; the report, the reasons, the
//! waits and the log lines are asserted by equality.

use std::collections::VecDeque;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use hostkeeper_launchd::{FakeLaunchctl, Launchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::Job;
use hostkeeper_link::testing::{FakeClock, ManualDispatch, RecordingLog};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::xcode_clt::{CLANG, FileCheck, Ports};

const START: u64 = 2_000_000_000;
const NAME: &str = "xcode-command-line-tools";
const LIST: &str = "* Label: Command Line Tools for Xcode-15.3\n";
const VERSION: &str = "15.3.0.0.1.1708646388";
const LOOK: &str = "pkgutil --pkg-info=com.apple.pkg.CLTools_Executables";
const SELECTED: &str = "xcode-select -p";
const LISTING: &str = "softwareupdate --list";
const INSTALLING: &str = "softwareupdate --install Command Line Tools for Xcode-15.3";
const INSTALL_FAILURE: &str = "softwareupdate --install failed with exit code 1: Install failed";

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A dispatcher that runs each job where it is started.
struct Inline;

impl Dispatch for Inline {
    fn start(&self, job: Job) {
        job();
    }
}

type Hook = Arc<dyn Fn() + Send + Sync>;

/// What the scripted Mac does.
struct Mac {
    installed: bool,
    /// Whether the compiler of the tools is on disk.
    clang: bool,
    version: String,
    pkgutil_runs: bool,
    select: VecDeque<i32>,
    listing: String,
    install_code: i32,
    install_stderr: String,
    calls: Vec<String>,
    /// Whether the marker was there at each call of `softwareupdate`.
    markers: Vec<bool>,
    on_install: Option<Hook>,
}

/// The Mac's programs, which count their calls in one list.
struct Machine {
    mac: Mutex<Mac>,
    marker: PathBuf,
}

impl Machine {
    fn mac(&self) -> MutexGuard<'_, Mac> {
        self.mac.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn calls(&self) -> Vec<String> {
        self.mac().calls.clone()
    }

    fn run(&self, program: &str, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        let hook;
        let answer = {
            let mut mac = self.mac();
            mac.calls.push(format!("{program} {}", args.join(" ")));
            match (program, args[0].as_str()) {
                ("pkgutil", _) if !mac.pkgutil_runs => {
                    return Err(LaunchctlError::Spawn {
                        args: args.join(" "),
                        why: "boom".to_owned(),
                    });
                }
                ("pkgutil", _) if mac.installed => {
                    hook = None;
                    LaunchctlOutput::success(&format!(
                        "package-id: com.apple.pkg.CLTools_Executables\nversion: {}\n",
                        mac.version
                    ))
                }
                ("pkgutil", _) => {
                    hook = None;
                    LaunchctlOutput::exit(1, "No receipt\n")
                }
                ("xcode-select", "-p") => {
                    hook = None;
                    LaunchctlOutput::exit(mac.select.pop_front().unwrap_or(0), "")
                }
                ("xcode-select", _) => {
                    hook = None;
                    LaunchctlOutput::success("")
                }
                ("softwareupdate", "--list") => {
                    hook = None;
                    let marker = self.marker.exists();
                    mac.markers.push(marker);
                    LaunchctlOutput::success(&mac.listing)
                }
                _ => {
                    let marker = self.marker.exists();
                    mac.markers.push(marker);
                    hook = mac.on_install.clone();
                    mac.installed = mac.install_code == 0;
                    LaunchctlOutput::exit(mac.install_code, &mac.install_stderr)
                }
            }
        };
        if let Some(hook) = hook {
            hook();
        }
        Ok(answer)
    }
}

/// One program of the Mac.
struct Port(Arc<Machine>, &'static str);

/// The Mac's disk: the compiler of the tools is a file there when the Mac says so.
struct Disk(Arc<Machine>);

impl FileCheck for Disk {
    fn is_file(&self, path: &std::path::Path) -> bool {
        path == std::path::Path::new(CLANG) && self.0.mac().clang
    }
}

impl Launchctl for Port {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        self.0.run(self.1, args)
    }
}

/// The sink, the Mac under it and the fakes it was given.
struct Rig {
    sink: Arc<Prerequisites>,
    machine: Arc<Machine>,
    clock: Arc<FakeClock>,
    log: Arc<RecordingLog>,
    _scratch: Scratch,
}

fn mac_over(name: &str, dispatch: Arc<dyn Dispatch>) -> Rig {
    let scratch = scratch(name);
    let marker = scratch.path().join("marker");
    let machine = Arc::new(Machine {
        mac: Mutex::new(Mac {
            installed: false,
            clang: false,
            version: VERSION.to_owned(),
            pkgutil_runs: true,
            select: VecDeque::new(),
            listing: LIST.to_owned(),
            install_code: 0,
            install_stderr: "Install failed\n".to_owned(),
            calls: Vec::new(),
            markers: Vec::new(),
            on_install: None,
        }),
        marker: marker.clone(),
    });
    let sw_vers = FakeLaunchctl::new();
    sw_vers.always(
        &[],
        Ok(LaunchctlOutput::success(
            "ProductVersion:\t15.1\nBuildVersion:\t24B83\n",
        )),
    );
    let tools = CommandLineTools::new(Ports {
        pkgutil: Box::new(Port(Arc::clone(&machine), "pkgutil")),
        xcode_select: Box::new(Port(Arc::clone(&machine), "xcode-select")),
        xcodebuild: Box::new(FakeLaunchctl::new()),
        sw_vers: Box::new(sw_vers),
        list: Box::new(Port(Arc::clone(&machine), "softwareupdate")),
        install: Box::new(Port(Arc::clone(&machine), "softwareupdate")),
        files: Box::new(Disk(Arc::clone(&machine))),
        marker,
    });
    let clock = Arc::new(FakeClock::new(START, 7));
    let log = Arc::new(RecordingLog::default());
    Rig {
        sink: Arc::new(Prerequisites::new(
            Some(tools),
            clock.clone(),
            dispatch,
            log.clone(),
        )),
        machine,
        clock,
        log,
        _scratch: scratch,
    }
}

/// A Mac whose passes run where they are started.
fn mac(name: &str) -> Rig {
    mac_over(name, Arc::new(Inline))
}

/// A Linux host: no programs, passes that would run at once.
fn linux() -> Prerequisites {
    Prerequisites::new(
        None,
        Arc::new(FakeClock::new(START, 7)),
        Arc::new(Inline),
        Arc::new(RecordingLog::default()),
    )
}

fn set(names: &[&str]) -> PushedPrerequisites {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    PushedPrerequisites {
        digest: rules::digest(&names),
        names,
    }
}

impl Rig {
    fn calls(&self) -> Vec<String> {
        self.machine.calls()
    }

    /// The state of the one prerequisite `NAME`, from a report.
    fn state(&self) -> PrerequisiteState {
        self.sink.report().states[0].clone()
    }

    fn mac(&self) -> MutexGuard<'_, Mac> {
        self.machine.mac()
    }

    /// Moves the clock on to `unix`.
    fn at(&self, unix: u64) {
        self.clock.set_unix(unix);
    }
}

fn short(set: &PushedPrerequisites) -> &str {
    &set.digest[..12]
}

/// Proves a Mac without the tools runs the install's steps once, in order, with the marker there
/// for both `softwareupdate` calls and gone after, and reports the tools present with their
/// version, the day and hour of the look; a later report runs nothing.
#[test]
fn a_mac_without_the_tools_installs_them_once_and_reports_them_present() {
    let rig = mac("prereq-install");
    let held = set(&[NAME]);
    rig.sink.apply(&held);
    assert_eq!(
        rig.calls(),
        [LOOK, SELECTED, LISTING, INSTALLING, LOOK, SELECTED].map(str::to_owned)
    );
    assert_eq!(rig.mac().markers, [true, true]);
    assert!(!rig.machine.marker.exists());
    let report = rig.sink.report();
    assert_eq!(report.digest, held.digest);
    assert_eq!(report.refused, None);
    assert_eq!(
        report.states,
        [PrerequisiteState {
            name: NAME.to_owned(),
            status: PrerequisiteStatus::Present,
            version: VERSION.to_owned(),
            reason: String::new(),
            failures: 0,
            next_try_unix: 0,
            seen_unix: START,
        }]
    );
    rig.sink.report();
    rig.sink.apply(&held);
    assert_eq!(rig.calls().len(), 6);
    assert_eq!(
        rig.log.lines(),
        [
            format!("host prerequisites {}: 1 prerequisite held", short(&held)),
            format!("host prerequisites: {NAME} is not on the host, installing it"),
            format!("host prerequisites: {NAME} is present, version {VERSION}"),
        ]
    );
}

/// Proves a Mac whose tools are on disk without their receipt, with a developer directory, reports
/// them present with no version and never runs `softwareupdate`, so it raises no
/// `prerequisite_failed`. Catches a look that knows only the receipt.
#[test]
fn a_mac_with_the_tools_on_disk_and_no_receipt_only_looks() {
    let rig = mac("prereq-on-disk");
    rig.mac().clang = true;
    rig.sink.apply(&set(&[NAME]));
    assert_eq!(rig.calls(), [LOOK, SELECTED].map(str::to_owned));
    let state = rig.state();
    assert_eq!(
        (state.status, state.version.as_str(), state.failures),
        (PrerequisiteStatus::Present, "", 0)
    );
    assert!(rig.mac().markers.is_empty());
}

/// Proves a Mac that has the tools runs only the look: no `softwareupdate` call, and the tools are
/// reported present.
#[test]
fn a_mac_with_the_tools_only_looks() {
    let rig = mac("prereq-present");
    rig.mac().installed = true;
    rig.sink.apply(&set(&[NAME]));
    assert_eq!(rig.calls(), [LOOK, SELECTED].map(str::to_owned));
    assert_eq!(rig.state().status, PrerequisiteStatus::Present);
    assert_eq!(rig.state().version, VERSION);
    assert!(rig.mac().markers.is_empty());
}

/// Proves a report is what starts the pass that comes due with the hour: the set's first pass is
/// started by `apply` and kept by its dispatcher until the test runs it, and the report an hour
/// after the look starts the next.
#[test]
fn a_report_starts_the_pass_that_comes_due() {
    let dispatch = Arc::new(ManualDispatch::default());
    let rig = mac_over("prereq-report-starts", dispatch.clone());
    rig.mac().installed = true;
    rig.sink.apply(&set(&[NAME]));
    assert_eq!(dispatch.pending(), 1);
    assert!(dispatch.run_next());
    assert_eq!(rig.state().status, PrerequisiteStatus::Present);
    assert_eq!(dispatch.pending(), 0);
    rig.at(START + RECHECK_SECS);
    assert_eq!(rig.state().status, PrerequisiteStatus::Present);
    assert_eq!(dispatch.pending(), 1);
}

/// Proves a failed install is `FAILED` with the step's reason, one failure, the next try 30
/// minutes on, and that no report or set before then runs a program; at the time one more try
/// runs, and the wait after each next failure doubles to the 24 hours' cap.
#[test]
fn a_failed_install_waits_and_doubles_its_wait_to_the_cap() {
    let rig = mac("prereq-backoff");
    rig.mac().install_code = 1;
    let held = set(&[NAME]);
    rig.sink.apply(&held);
    assert_eq!(rig.calls().len(), 4);
    assert_eq!(
        rig.state(),
        PrerequisiteState {
            name: NAME.to_owned(),
            status: PrerequisiteStatus::Failed,
            version: String::new(),
            reason: INSTALL_FAILURE.to_owned(),
            failures: 1,
            next_try_unix: START + 1800,
            seen_unix: START,
        }
    );
    assert_eq!(
        rig.log.lines()[2],
        format!(
            "error: host prerequisites: {NAME} failed (1 in a row), next try in 1800 s: {INSTALL_FAILURE}"
        )
    );
    let mut tries = 1;
    for (failures, wait) in [
        (2, 3600),
        (3, 7200),
        (4, 14400),
        (5, 28800),
        (6, 57600),
        (7, 86400),
        (8, 86400),
    ] {
        let previous = rig.state().next_try_unix;
        rig.at(previous - 1);
        rig.sink.report();
        rig.sink.apply(&held);
        assert_eq!(rig.calls().len(), 4 * tries, "no try before its time");
        let now = previous;
        rig.at(now);
        let state = rig.state();
        tries += 1;
        assert_eq!(rig.calls().len(), 4 * tries, "one try at its time");
        assert_eq!(state.status, PrerequisiteStatus::Failed);
        assert_eq!(state.failures, failures);
        assert_eq!(state.next_try_unix, now + wait);
        assert_eq!(state.seen_unix, now);
    }
}

/// Proves a success after failures counts them back to 0 and clears the reason, and the next
/// failure waits 30 minutes again.
#[test]
fn a_success_resets_the_failures() {
    let rig = mac("prereq-reset");
    rig.mac().install_code = 1;
    rig.sink.apply(&set(&[NAME]));
    rig.at(START + 1800);
    assert_eq!(rig.state().failures, 2);
    rig.mac().install_code = 0;
    rig.at(START + 1800 + 3600);
    let state = rig.state();
    assert_eq!(state.status, PrerequisiteStatus::Present);
    assert_eq!((state.failures, state.next_try_unix), (0, 0));
    assert_eq!(state.reason, "");
    rig.mac().installed = false;
    rig.mac().install_code = 1;
    let later = START + 1800 + 3600 + RECHECK_SECS;
    rig.at(later);
    let state = rig.state();
    assert_eq!(state.status, PrerequisiteStatus::Failed);
    assert_eq!((state.failures, state.next_try_unix), (1, later + 1800));
}

/// Proves the wait is 30 minutes doubled for each next failure, never above 24 hours, and does not
/// overflow for a count that is far too high.
#[test]
fn the_wait_is_capped_and_never_overflows() {
    assert_eq!(wait_secs(0), 1800);
    assert_eq!(wait_secs(1), 1800);
    assert_eq!(wait_secs(2), 3600);
    assert_eq!(wait_secs(6), 57_600);
    assert_eq!(wait_secs(7), MAX_WAIT_SECS);
    assert_eq!(wait_secs(u32::MAX), MAX_WAIT_SECS);
}

/// Proves a Linux host, systemd or OpenRC, given the set reports a name of the Mac not applicable
/// and one it does not know unknown, holds the digest, and runs nothing.
#[test]
fn a_linux_host_reports_not_applicable_and_runs_nothing() {
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        let sink = Prerequisites::at_host(kind, Arc::new(RecordingLog::default()));
        assert_eq!(sink.report(), PrerequisitesReport::default());
        let held = set(&[NAME, "newer-thing"]);
        sink.apply(&held);
        let report = sink.report();
        assert_eq!(report.digest, held.digest);
        let statuses: Vec<_> = report.states.iter().map(|state| state.status).collect();
        assert_eq!(
            statuses,
            [
                PrerequisiteStatus::NotApplicable,
                PrerequisiteStatus::Unknown
            ]
        );
    }
    let sink = linux();
    sink.apply(&set(&[NAME]));
    assert_eq!(
        sink.report().states[0].status,
        PrerequisiteStatus::NotApplicable
    );
}

/// Proves a Mac's own sink is built with no set, and so with nothing due to run.
#[test]
fn a_macs_sink_with_no_set_runs_nothing() {
    let sink = Prerequisites::at_host(PlatformKind::Macos, Arc::new(RecordingLog::default()));
    assert_eq!(sink.report(), PrerequisitesReport::default());
}

/// Proves a name this release does not know is `UNKNOWN` on a Mac, never looked at, and the set
/// is held.
#[test]
fn an_unknown_name_is_unknown_and_never_run() {
    let rig = mac("prereq-unknown");
    let held = set(&["newer-thing"]);
    rig.sink.apply(&held);
    rig.at(START + 10 * RECHECK_SECS);
    let report = rig.sink.report();
    assert_eq!(report.digest, held.digest);
    assert_eq!(report.states[0].status, PrerequisiteStatus::Unknown);
    assert!(rig.calls().is_empty());
}

/// Proves a set whose digest is not the digest of its names is refused with the reason and the
/// digest, the set held stays in force, and the log says so; a good set after it clears the
/// refusal.
#[test]
fn a_set_with_a_wrong_digest_is_refused_and_the_held_set_stays() {
    let rig = mac("prereq-bad-digest");
    rig.mac().installed = true;
    let held = set(&[NAME]);
    rig.sink.apply(&held);
    let bad = PushedPrerequisites {
        digest: "0".repeat(64),
        names: vec![NAME.to_owned(), "other-thing".to_owned()],
    };
    rig.sink.apply(&bad);
    let report = rig.sink.report();
    assert_eq!(report.digest, held.digest);
    assert_eq!(report.states.len(), 1);
    assert_eq!(
        report.refused,
        Some(PrerequisitesRefusal {
            digest: bad.digest.clone(),
            reason: "the coordinator's digest is not the digest of its names".to_owned(),
        })
    );
    assert_eq!(
        rig.log.lines().last().cloned(),
        Some(
            "error: host prerequisites 000000000000 are refused: the coordinator's digest is not the digest of its names"
                .to_owned()
        )
    );
    rig.sink.apply(&set(&[NAME, "other-thing"]));
    assert_eq!(rig.sink.report().refused, None);
}

/// Proves the set held, sent again after a refused one, clears the refusal and changes nothing
/// else: the report holds the held digest and no refusal, and no program runs again.
#[test]
fn the_held_set_sent_again_after_a_refusal_clears_it() {
    let rig = mac("prereq-refusal-cleared");
    rig.mac().installed = true;
    let held = set(&[NAME]);
    rig.sink.apply(&held);
    let calls = rig.calls();
    rig.sink.apply(&PushedPrerequisites {
        digest: "0".repeat(64),
        names: vec![NAME.to_owned()],
    });
    assert!(rig.sink.report().refused.is_some());
    rig.sink.apply(&held);
    let report = rig.sink.report();
    assert_eq!(report.refused, None);
    assert_eq!(report.digest, held.digest);
    assert_eq!(report.states[0].status, PrerequisiteStatus::Present);
    assert_eq!(rig.calls(), calls);
}

/// Proves a pass that ends without an outcome, as a panic in the install ends it, leaves no
/// prerequisite installing: it is failed once, with its wait, and the sink starts the next pass
/// when that wait is over.
#[test]
fn a_pass_that_ends_without_an_outcome_is_a_failure_with_its_wait() {
    let rig = mac("prereq-panic");
    rig.mac().listing = "* Label: Command Line Tools for Xcode-16.2\n".to_owned();
    rig.mac().on_install = Some(Arc::new(|| panic!("the install panics")));
    let pushed = set(&[NAME]);
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rig.sink.apply(&pushed);
    }));
    assert!(ended.is_err());
    let state = rig.state();
    assert_eq!(
        (
            state.status,
            state.failures,
            state.next_try_unix,
            state.reason.as_str()
        ),
        (
            PrerequisiteStatus::Failed,
            1,
            START + FIRST_WAIT_SECS,
            "the install ended without an outcome"
        )
    );
    rig.mac().on_install = None;
    let before = rig.calls().len();
    let _ = rig.sink.report();
    assert_eq!(rig.calls().len(), before);
    rig.at(START + FIRST_WAIT_SECS);
    assert_eq!(
        rig.sink.report().states[0].status,
        PrerequisiteStatus::Present
    );
}

/// Proves a set that breaks the rules of a set (too many names, a name of no grammar, a name
/// twice) is refused with the rule's own words, whatever its digest.
#[test]
fn a_set_that_breaks_the_rules_is_refused() {
    let rig = mac("prereq-bad-rules");
    let many: Vec<String> = (0..17)
        .map(|n| format!("thing-{}", char::from(b'a' + n)))
        .collect();
    let cases = [
        many.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["Bad_Name"],
        vec![NAME, NAME],
    ];
    for names in cases {
        let bad = set(&names);
        let reason = rules::check_set(&bad.names)
            .expect_err("breaks a rule")
            .to_string();
        rig.sink.apply(&bad);
        let report = rig.sink.report();
        assert_eq!(report.digest, "");
        assert_eq!(
            report.refused,
            Some(PrerequisitesRefusal {
                digest: bad.digest.clone(),
                reason: reason.clone(),
            })
        );
        assert_eq!(
            rig.log.lines().last().cloned(),
            Some(format!(
                "error: host prerequisites {} are refused: {reason}",
                short(&bad)
            ))
        );
    }
    assert!(rig.calls().is_empty());
}

/// Proves a set whose digest is the one held changes nothing: no log line, no look, no state
/// change.
#[test]
fn a_set_with_the_digest_held_is_a_no_op() {
    let rig = mac("prereq-same");
    rig.mac().installed = true;
    let held = set(&[NAME]);
    rig.sink.apply(&held);
    let before = rig.sink.report();
    let lines = rig.log.lines();
    rig.at(START + 100);
    rig.sink.apply(&held);
    assert_eq!(rig.sink.report(), before);
    assert_eq!(rig.log.lines(), lines);
    assert_eq!(rig.calls().len(), 2);
}

/// Proves a new set keeps the state of a name it shares with the old one, so a failure's wait is
/// not reset, a name added is new, and a name that left the set and came back starts again.
#[test]
fn a_new_set_carries_over_the_state_of_a_shared_name() {
    let rig = mac("prereq-carry");
    rig.mac().install_code = 1;
    rig.sink.apply(&set(&[NAME]));
    let failed = rig.state();
    assert_eq!(failed.failures, 1);
    let calls = rig.calls().len();
    let bigger = set(&[NAME, "newer-thing"]);
    rig.sink.apply(&bigger);
    let report = rig.sink.report();
    assert_eq!(report.digest, bigger.digest);
    assert_eq!(report.states[0], failed);
    assert_eq!(report.states[1].status, PrerequisiteStatus::Unknown);
    assert_eq!(rig.calls().len(), calls);
    rig.sink.apply(&set(&[]));
    assert_eq!(rig.sink.report().states, []);
    rig.sink.apply(&bigger);
    assert_eq!(
        rig.calls().len(),
        calls + 4,
        "a name that left starts again"
    );
    assert_eq!(rig.sink.report().states[0].failures, 1);
}

/// Proves a present prerequisite is not looked at before an hour is out and is looked at once
/// when it is, and the version is the one of the later look.
#[test]
fn a_present_prerequisite_is_looked_at_again_every_hour() {
    let rig = mac("prereq-hourly");
    rig.mac().installed = true;
    rig.sink.apply(&set(&[NAME]));
    rig.at(START + RECHECK_SECS - 1);
    rig.sink.report();
    assert_eq!(rig.calls().len(), 2);
    rig.mac().version = "16.0".to_owned();
    rig.at(START + RECHECK_SECS);
    let state = rig.state();
    assert_eq!(rig.calls().len(), 4);
    assert_eq!(state.version, "16.0");
    assert_eq!(state.seen_unix, START + RECHECK_SECS);
    rig.sink.report();
    assert_eq!(rig.calls().len(), 4);
}

/// Proves a look that finds the tools gone installs them again.
#[test]
fn tools_that_are_gone_at_the_next_look_are_installed_again() {
    let rig = mac("prereq-gone");
    rig.mac().installed = true;
    rig.sink.apply(&set(&[NAME]));
    rig.mac().installed = false;
    rig.at(START + RECHECK_SECS);
    let state = rig.state();
    assert_eq!(state.status, PrerequisiteStatus::Present);
    assert_eq!(
        rig.calls()[2..],
        [LOOK, SELECTED, LISTING, INSTALLING, LOOK, SELECTED].map(str::to_owned)
    );
}

/// Proves a listing with no tools in it fails the prerequisite with the reason, the marker is
/// removed and `softwareupdate --install` is never run.
#[test]
fn no_label_offered_is_a_failure() {
    let rig = mac("prereq-no-label");
    rig.mac().listing = "No new software available.\n".to_owned();
    rig.sink.apply(&set(&[NAME]));
    let state = rig.state();
    assert_eq!(state.status, PrerequisiteStatus::Failed);
    assert_eq!(
        state.reason,
        "softwareupdate --list offered no Command Line Tools for Xcode; macOS 15.1 24B83; stdout: No new software available.; stderr empty"
    );
    assert_eq!(rig.calls(), [LOOK, SELECTED, LISTING].map(str::to_owned));
    assert!(!rig.machine.marker.exists());
}

/// Proves a marker that is a link is refused: the prerequisite fails, and `softwareupdate` is never
/// run.
#[test]
fn a_marker_that_is_a_link_installs_nothing() {
    let rig = mac("prereq-link");
    let target = rig.machine.marker.with_file_name("elsewhere");
    symlink(&target, &rig.machine.marker).expect("the link");
    rig.sink.apply(&set(&[NAME]));
    let state = rig.state();
    assert_eq!(state.status, PrerequisiteStatus::Failed);
    assert!(
        state.reason.starts_with(&format!(
            "the install marker {} could not be made: ",
            rig.machine.marker.display()
        )),
        "{}",
        state.reason
    );
    assert_eq!(rig.calls(), [LOOK, SELECTED]);
    assert!(!target.exists());
}

/// Proves a program that cannot be run fails the look with the step named, and nothing is
/// installed.
#[test]
fn a_look_that_cannot_run_is_a_failure() {
    let rig = mac("prereq-look-fails");
    rig.mac().pkgutil_runs = false;
    rig.sink.apply(&set(&[NAME]));
    let state = rig.state();
    assert_eq!(state.status, PrerequisiteStatus::Failed);
    assert_eq!(state.reason, "pkgutil --pkg-info could not be run: boom");
    assert_eq!(rig.calls(), [LOOK]);
}

/// Proves a reason and a version are cut to what the coordinator reads, at a character boundary.
#[test]
fn a_long_reason_and_a_long_version_are_clipped() {
    let rig = mac("prereq-clip");
    rig.mac().install_code = 1;
    rig.mac().install_stderr = "é".repeat(300);
    rig.sink.apply(&set(&[NAME]));
    let reason = rig.state().reason;
    assert!(reason.len() <= MAX_PREREQUISITE_REASON && reason.len() > 250);
    assert!(reason.starts_with("softwareupdate --install failed with exit code 1: éé"));
    let rig = mac("prereq-clip-version");
    rig.mac().installed = true;
    rig.mac().version = "9".repeat(100);
    rig.sink.apply(&set(&[NAME]));
    assert_eq!(rig.state().version, "9".repeat(MAX_PREREQUISITE_VERSION));
    assert_eq!(clip("éé", 3), "é");
    assert_eq!(clip("ab", 9), "ab");
}

/// Proves a report made during an install says `INSTALLING`, with no reason, and starts no second
/// pass; the install then ends `PRESENT`.
#[test]
fn a_report_during_an_install_says_installing() {
    let rig = mac("prereq-installing");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (sink, note) = (Arc::clone(&rig.sink), Arc::clone(&seen));
    rig.mac().on_install = Some(Arc::new(move || {
        note.lock().expect("the note").push(sink.report());
    }));
    rig.sink.apply(&set(&[NAME]));
    let seen = seen.lock().expect("the note");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].states[0].status, PrerequisiteStatus::Installing);
    assert_eq!(seen[0].states[0].reason, "");
    assert_eq!(rig.calls().len(), 6);
    assert_eq!(rig.state().status, PrerequisiteStatus::Present);
}

/// Proves a set that arrives while a pass runs is held, does not start a second pass, and a name
/// it leaves out gets no state from the pass that was looking at it.
#[test]
fn a_set_sent_during_a_pass_is_held_and_drops_a_name_the_pass_was_on() {
    let rig = mac("prereq-during");
    let sink = Arc::clone(&rig.sink);
    rig.mac().on_install = Some(Arc::new(move || sink.apply(&set(&[]))));
    rig.sink.apply(&set(&[NAME]));
    assert_eq!(rig.calls().len(), 6);
    let report = rig.sink.report();
    assert_eq!(report.digest, set(&[]).digest);
    assert_eq!(report.states, []);
    assert_eq!(
        rig.log.lines().last().cloned(),
        Some(format!(
            "host prerequisites {}: 0 prerequisites held",
            &set(&[]).digest[..12]
        ))
    );
}

/// Proves a pass is not started twice while one is held by its dispatcher, and a job the
/// dispatcher drops without running leaves the sink free to start another.
#[test]
fn one_pass_runs_at_a_time_and_a_dropped_job_frees_the_sink() {
    let dispatch = Arc::new(ManualDispatch::default());
    let rig = mac_over("prereq-one-at-a-time", dispatch.clone());
    rig.mac().installed = true;
    let held = set(&[NAME]);
    rig.sink.apply(&held);
    rig.sink.report();
    rig.sink.apply(&held);
    assert_eq!(dispatch.pending(), 1);
    assert_eq!(
        rig.sink.report().states[0].status,
        PrerequisiteStatus::Pending
    );
    assert!(rig.calls().is_empty());
    assert!(dispatch.drop_next());
    rig.sink.report();
    assert_eq!(dispatch.pending(), 1);
    assert!(dispatch.run_next());
    assert_eq!(rig.calls().len(), 2);
    assert_eq!(
        rig.sink.report().states[0].status,
        PrerequisiteStatus::Present
    );
    assert_eq!(dispatch.pending(), 0);
}

/// Proves a sink that was sent nothing reports nothing: no digest, no refusal, no state.
#[test]
fn a_sink_sent_no_set_reports_nothing() {
    let rig = mac("prereq-empty");
    assert_eq!(rig.sink.report(), PrerequisitesReport::default());
    assert!(rig.calls().is_empty());
}
