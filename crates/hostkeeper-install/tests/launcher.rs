//! The repository installer's tools (packages.md section 5.1): the `Tools` port over the step
//! launcher's transient units, and the tool, run and output types behind it.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

use hostkeeper_install::repository::launcher::LauncherTools;
use hostkeeper_install::repository::tools::{
    Network, Pause, Run, Sleep, Tool, ToolError, ToolOutput, Tools,
};
use hostkeeper_steps::{
    LaunchError, StepCall, StepExit, StepLauncher, ToolCall, ToolExit, ToolNetwork,
};

/// What one `run_tool` call carried, as owned values.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    service: String,
    unit: String,
    program: PathBuf,
    args: Vec<OsString>,
    timeout_secs: u64,
    network: ToolNetwork,
    writable: Option<Vec<PathBuf>>,
    working_dir: Option<PathBuf>,
    env: Vec<(String, String)>,
}

/// A launcher that runs no step, records each tool call, and answers the next of its answers.
#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    answers: Mutex<VecDeque<Result<ToolExit, LaunchError>>>,
}

impl Fake {
    fn answering(answers: Vec<Result<ToolExit, LaunchError>>) -> Fake {
        Fake {
            seen: Mutex::default(),
            answers: Mutex::new(answers.into()),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the record").clone()
    }
}

impl StepLauncher for Fake {
    fn run(&self, _call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        unreachable!("a tool run is not a step")
    }

    fn stop(&self, _unit: &str) -> Result<(), LaunchError> {
        unreachable!("a tool run is not stopped")
    }

    fn run_tool(&self, call: &ToolCall<'_>) -> Result<ToolExit, LaunchError> {
        self.seen.lock().expect("the record").push(Seen {
            service: call.service.to_owned(),
            unit: call.unit.to_owned(),
            program: call.program.to_path_buf(),
            args: call.args.to_vec(),
            timeout_secs: call.timeout_secs,
            network: call.network,
            writable: call
                .writable
                .map(|paths| paths.iter().map(|p| p.to_path_buf()).collect()),
            working_dir: call.working_dir.map(PathBuf::from),
            env: call
                .env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        });
        self.answers
            .lock()
            .expect("the answers")
            .pop_front()
            .unwrap_or_else(|| Ok(exit(0, "")))
    }
}

fn exit(code: i32, stdout: &str) -> ToolExit {
    ToolExit {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
        cut: false,
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

/// Proves an offline run reaches the launcher as the tool's absolute path with its arguments,
/// its bound, the offline network, no confinement, no working directory and `LC_ALL=C`, for the
/// service, in a unit named for the service and the tool.
#[test]
fn an_offline_run_is_a_confined_free_offline_call() {
    let fake = Fake::default();
    let tools = LauncherTools::new(&fake, "mdm");
    let run = Run::offline(Tool::DpkgQuery, args(&["-W", "mdm"]), 30);
    assert_eq!(tools.run(&run), Ok(ToolOutput::exit(0, "")));
    assert_eq!(
        fake.seen(),
        vec![Seen {
            service: "mdm".to_owned(),
            unit: "hostkeeper-install-mdm-dpkg-query-0".to_owned(),
            program: PathBuf::from("/usr/bin/dpkg-query"),
            args: vec![OsString::from("-W"), OsString::from("mdm")],
            timeout_secs: 30,
            network: ToolNetwork::Offline,
            writable: None,
            working_dir: None,
            env: vec![("LC_ALL".to_owned(), "C".to_owned())],
        }]
    );
}

/// Proves a fetch run carries the network, the paths it may write (also none at all), its working
/// directory and its extra environment after `LC_ALL=C`, in order.
#[test]
fn a_fetch_run_carries_its_network_writable_paths_directory_and_environment() {
    let fake = Fake::default();
    let tools = LauncherTools::new(&fake, "mdm");
    let run = Run::offline(Tool::AptGet, args(&["download", "mdm=1.0"]), 90)
        .fetching()
        .writing(vec![PathBuf::from("/var/lib/hostkeeper/apt/mdm")])
        .in_dir(PathBuf::from("/var/lib/hostkeeper/apt/mdm"))
        .with_env("APT_CONFIG", "/var/lib/hostkeeper/apt/mdm/apt.conf");
    tools.run(&run).expect("the run");
    let nothing = Run::offline(Tool::AptCache, args(&["show"]), 5).writing(Vec::new());
    tools.run(&nothing).expect("the run");
    let seen = fake.seen();
    assert_eq!(seen[0].network, ToolNetwork::Fetch);
    assert_eq!(seen[0].timeout_secs, 90);
    assert_eq!(seen[0].program, PathBuf::from("/usr/bin/apt-get"));
    assert_eq!(
        seen[0].writable,
        Some(vec![PathBuf::from("/var/lib/hostkeeper/apt/mdm")])
    );
    assert_eq!(
        seen[0].working_dir,
        Some(PathBuf::from("/var/lib/hostkeeper/apt/mdm"))
    );
    assert_eq!(
        seen[0].env,
        vec![
            ("LC_ALL".to_owned(), "C".to_owned()),
            (
                "APT_CONFIG".to_owned(),
                "/var/lib/hostkeeper/apt/mdm/apt.conf".to_owned()
            )
        ]
    );
    assert_eq!(seen[1].network, ToolNetwork::Offline);
    assert_eq!(
        seen[1].writable,
        Some(Vec::new()),
        "no path writable is not unconfined"
    );
}

/// Proves each run has a unit name of its own, counting up across tools, so one run's unit is
/// never the next one's.
#[test]
fn each_run_has_a_unit_of_its_own() {
    let fake = Fake::default();
    let tools = LauncherTools::new(&fake, "web");
    for tool in [Tool::Dpkg, Tool::Dpkg, Tool::Systemctl] {
        tools
            .run(&Run::offline(tool, Vec::new(), 1))
            .expect("the run");
    }
    let units: Vec<String> = fake.seen().into_iter().map(|s| s.unit).collect();
    assert_eq!(
        units,
        [
            "hostkeeper-install-web-dpkg-0",
            "hostkeeper-install-web-dpkg-1",
            "hostkeeper-install-web-systemctl-2"
        ]
    );
    let other = Fake::default();
    LauncherTools::new(&other, "web")
        .run(&Run::offline(Tool::Dpkg, Vec::new(), 1))
        .expect("the run");
    assert_eq!(other.seen()[0].unit, "hostkeeper-install-web-dpkg-0");
}

/// Proves two ports that share a counter never name the same unit, whichever of them runs: a
/// daemon makes a port for each step and each heartbeat's read of the same service, which may
/// run at once. The counter goes on from where the caller set it.
///
/// Catches: a heartbeat's `dpkg-query` and an order's asking systemd for the same transient unit.
#[test]
fn ports_that_share_a_counter_never_name_the_same_unit() {
    let fake = Fake::default();
    let runs = std::sync::atomic::AtomicU64::new(7);
    let order = LauncherTools::sharing(&fake, "web", &runs);
    let beat = LauncherTools::sharing(&fake, "web", &runs);
    for tools in [&order, &beat, &order] {
        tools
            .run(&Run::offline(Tool::DpkgQuery, Vec::new(), 1))
            .expect("the run");
    }
    let units: Vec<String> = fake.seen().into_iter().map(|s| s.unit).collect();
    assert_eq!(
        units,
        [
            "hostkeeper-install-web-dpkg-query-7",
            "hostkeeper-install-web-dpkg-query-8",
            "hostkeeper-install-web-dpkg-query-9"
        ]
    );
}

/// Proves the launcher's exit becomes the tool's output: the code (or none), both streams and the
/// cut flag.
#[test]
fn the_exit_maps_to_the_tool_output() {
    let fake = Fake::answering(vec![
        Ok(ToolExit {
            code: Some(100),
            stdout: b"out".to_vec(),
            stderr: b"E: err".to_vec(),
            cut: true,
        }),
        Ok(ToolExit {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            cut: false,
        }),
    ]);
    let tools = LauncherTools::new(&fake, "mdm");
    let run = Run::offline(Tool::AptGet, Vec::new(), 1);
    assert_eq!(
        tools.run(&run),
        Ok(ToolOutput {
            code: Some(100),
            stdout: b"out".to_vec(),
            stderr: b"E: err".to_vec(),
            cut: true,
        })
    );
    assert_eq!(tools.run(&run), Ok(ToolOutput::default()));
}

/// Proves a launcher that fails is a `ToolError` carrying its text.
#[test]
fn a_launcher_error_is_a_tool_error_with_its_text() {
    let fake = Fake::answering(vec![
        Err(LaunchError::Launcher("no unit could be made".to_owned())),
        Err(LaunchError::NoStart("no such program".to_owned())),
        Err(LaunchError::Io(std::io::Error::other("the pipe broke"))),
    ]);
    let tools = LauncherTools::new(&fake, "mdm");
    let run = Run::offline(Tool::AptGet, Vec::new(), 1);
    assert_eq!(
        tools.run(&run),
        Err(ToolError(
            "the launcher failed: no unit could be made".to_owned()
        ))
    );
    assert_eq!(
        tools.run(&run),
        Err(ToolError(
            "the hook did not start: no such program".to_owned()
        ))
    );
    let error = tools.run(&run).expect_err("an I/O error");
    assert_eq!(error, ToolError("the pipe broke".to_owned()));
    assert_eq!(error.to_string(), "the pipe broke");
}

/// Proves the seven tools have their absolute path in `/usr/bin` and their short name, each once.
#[test]
fn every_tool_has_a_path_and_a_name() {
    let want = [
        (Tool::AptGet, "apt-get"),
        (Tool::AptCache, "apt-cache"),
        (Tool::AptMark, "apt-mark"),
        (Tool::Dpkg, "dpkg"),
        (Tool::DpkgDeb, "dpkg-deb"),
        (Tool::DpkgQuery, "dpkg-query"),
        (Tool::Systemctl, "systemctl"),
    ];
    assert_eq!(Tool::ALL.len(), want.len());
    for (tool, name) in want {
        assert!(Tool::ALL.contains(&tool), "{name}");
        assert_eq!(tool.name(), name);
        assert_eq!(tool.path(), format!("/usr/bin/{name}"));
    }
}

/// Proves the run builders: an offline run is unconfined and fetches nothing, in no directory,
/// with `LC_ALL=C`; each builder sets only its own part.
#[test]
fn the_run_builders_set_their_own_part() {
    let base = Run::offline(Tool::Dpkg, args(&["--purge", "mdm"]), 12);
    assert_eq!(
        base,
        Run {
            tool: Tool::Dpkg,
            args: args(&["--purge", "mdm"]),
            network: Network::Offline,
            writable: None,
            working_dir: None,
            env: vec![("LC_ALL".to_owned(), "C".to_owned())],
            bound_secs: 12,
        }
    );
    let fetching = base.clone().fetching();
    assert_eq!(fetching.network, Network::Fetch);
    assert_eq!(
        Run {
            network: Network::Offline,
            ..fetching
        },
        base
    );
    let writing = base.clone().writing(vec![PathBuf::from("/w")]);
    assert_eq!(writing.writable, Some(vec![PathBuf::from("/w")]));
    assert_eq!(writing.network, Network::Offline);
    let in_dir = base.clone().in_dir(PathBuf::from("/d"));
    assert_eq!(in_dir.working_dir, Some(PathBuf::from("/d")));
    let with = base.clone().with_env("A", "1").with_env("B", "2");
    assert_eq!(
        with.env,
        vec![
            ("LC_ALL".to_owned(), "C".to_owned()),
            ("A".to_owned(), "1".to_owned()),
            ("B".to_owned(), "2".to_owned())
        ]
    );
}

/// Proves `ToolOutput::exit` has the code and stdout alone, and only an exit code of 0 succeeded.
#[test]
fn an_output_succeeded_only_with_exit_code_0() {
    let out = ToolOutput::exit(3, "text");
    assert_eq!(
        out,
        ToolOutput {
            code: Some(3),
            stdout: b"text".to_vec(),
            stderr: Vec::new(),
            cut: false
        }
    );
    assert!(ToolOutput::exit(0, "").succeeded());
    assert!(!ToolOutput::exit(1, "").succeeded());
    assert!(!ToolOutput::exit(-1, "").succeeded());
    assert!(!ToolOutput::default().succeeded());
}

fn two(stdout: &str, stderr: &str) -> ToolOutput {
    ToolOutput {
        code: Some(1),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
        cut: false,
    }
}

/// Proves `text` is standard output, a newline, then standard error, lossily decoded.
#[test]
fn the_text_is_stdout_then_stderr() {
    assert_eq!(two("a\n", "b\n").text(), "a\n\nb\n");
    assert_eq!(ToolOutput::default().text(), "\n");
    let binary = ToolOutput {
        stdout: vec![b'x', 0xff],
        stderr: vec![0xfe, b'y'],
        ..ToolOutput::default()
    };
    assert_eq!(binary.text(), "x\u{fffd}\n\u{fffd}y");
}

/// Proves the line a failure quotes: an `E:` line wins over any other (the last one of a stream),
/// else the last non-blank line of standard error, else of standard output, else nothing; lines
/// are trimmed.
#[test]
fn the_last_line_prefers_an_e_line_then_stderr_then_stdout() {
    assert_eq!(
        two("", "x\nE: first\nE: second\nW: later\n").last_line(),
        "E: second"
    );
    assert_eq!(
        two("E: in stdout\n", "W: only a warning\n").last_line(),
        "E: in stdout"
    );
    assert_eq!(
        two("out line\n", "W: err line\n\n  \n").last_line(),
        "W: err line"
    );
    assert_eq!(two("first\nlast out\n\n", "").last_line(), "last out");
    assert_eq!(two("  E: padded  \n", "").last_line(), "E: padded");
    assert_eq!(
        two("x E: not at the start\n", "").last_line(),
        "x E: not at the start"
    );
    assert_eq!(two("\n  \n", "\n").last_line(), "");
    assert_eq!(ToolOutput::default().last_line(), "");
}

/// Proves the host's pause takes seconds and returns, here for none.
#[test]
fn the_sleep_pause_returns() {
    let pause: &dyn Pause = &Sleep;
    pause.pause(0);
    Sleep.pause(0);
}
