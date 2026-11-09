//! A tool run on the launcher of the host (docs/design/packages.md, 5.3): the `systemd-run` line
//! of the tool's unit carries `PrivateNetwork=yes` besides `RuntimeMaxSec=`, the lines of a hook
//! step and an additive step do not, and the exit and last line are read as for a step. A launcher
//! with no tool runs keeps the default, `NoStart`.

mod support;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hostkeeper_hook::{Invocation, Step};
use hostkeeper_steps::systemd::{additive_command, command, tool_command};
use hostkeeper_steps::{
    AdditiveCall, Captured, LaunchError, Program, Ring, Spawner, StepCall, StepExit, StepLauncher,
    SystemdLauncher, ToolCall,
};
use support::{Scratch, secs};

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(Program, Vec<String>)>>,
    replies: Mutex<VecDeque<Captured>>,
}

impl Recorder {
    fn answering(replies: Vec<Captured>) -> Recorder {
        Recorder {
            calls: Mutex::default(),
            replies: Mutex::new(replies.into()),
        }
    }
}

impl Spawner for &Recorder {
    fn spawn(&self, program: Program, args: &[OsString]) -> io::Result<Captured> {
        let args = args
            .iter()
            .map(|a| a.to_str().unwrap().to_owned())
            .collect();
        self.calls.lock().unwrap().push((program, args));
        Ok(self.replies.lock().unwrap().pop_front().expect("a reply"))
    }
}

fn captured(code: Option<i32>, stdout: &[u8]) -> Captured {
    let mut out = Ring::new();
    out.push(stdout);
    Captured {
        code,
        stdout: out,
        stderr: Ring::new(),
    }
}

const UNIT: &str = "updater-svc-install-1";

/// A scratch directory with an executable `tool` in it.
fn tool_file(name: &str) -> (Scratch, PathBuf) {
    let scratch = Scratch::new(name);
    let tool = scratch.path().join("tool");
    std::fs::write(&tool, "#!tool\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    (scratch, tool)
}

fn call<'a>(program: &'a Path, args: &'a [OsString], timeout_secs: u64) -> ToolCall<'a> {
    ToolCall {
        service: "svc",
        unit: UNIT,
        program,
        args,
        timeout_secs,
    }
}

fn strings(args: Vec<OsString>) -> Vec<String> {
    args.into_iter().map(|a| a.into_string().unwrap()).collect()
}

/// Catches a tool that runs with the network, or with another bound or unit: the line of the
/// tool's unit is exactly the step line plus `--property=PrivateNetwork=yes` before `--`, then
/// the program and its arguments; a timeout of 0 is `infinity`; the launcher runs the same line
/// `tool_command` makes.
#[test]
fn the_tool_unit_has_no_network_and_a_bound() {
    let (_scratch, tool) = tool_file("tool-line");
    let args = vec![OsString::from("-x"), OsString::from("pkg.tar")];
    let line = |max: &str| -> Vec<String> {
        [
            "--system",
            "--quiet",
            "--wait",
            "--collect",
            "--pipe",
            &format!("--unit={UNIT}"),
            &format!("--property=RuntimeMaxSec={max}"),
            "--property=PrivateNetwork=yes",
            "--",
            tool.to_str().unwrap(),
            "-x",
            "pkg.tar",
        ]
        .map(str::to_owned)
        .to_vec()
    };
    assert_eq!(
        strings(tool_command(&call(&tool, &args, 60)).unwrap()),
        line("60")
    );
    assert_eq!(
        strings(tool_command(&call(&tool, &args, 0)).unwrap()),
        line("infinity")
    );

    let recorder = Recorder::answering(vec![captured(Some(0), b"")]);
    SystemdLauncher::new(&recorder)
        .run_tool(&call(&tool, &args, 60))
        .unwrap();
    assert_eq!(
        *recorder.calls.lock().unwrap(),
        [(Program::SystemdRun, line("60"))]
    );
}

/// Catches the network confinement leaking onto hook units: neither a step's line nor an
/// additive step's line names `PrivateNetwork`, and each keeps `RuntimeMaxSec=` as its one
/// property.
#[test]
fn the_hook_and_additive_lines_have_no_private_network() {
    let (_scratch, hook) = tool_file("tool-hook-lines");
    let params = hook.with_file_name("params.toml");
    let step = command(&StepCall {
        service: "svc",
        invocation: Invocation {
            step: Step::Cutover,
            artifact: None,
            params: Some(params.clone()),
        },
        executable: &hook,
        unit: UNIT,
        bound: secs(5),
    })
    .unwrap();
    let additive = additive_command(&AdditiveCall {
        service: "svc",
        step: "uninstall",
        hook_args: &[],
        params: &params,
        executable: &hook,
        unit: UNIT,
        timeout_secs: 5,
    })
    .unwrap();
    for line in [strings(step), strings(additive)] {
        assert!(
            !line.iter().any(|a| a.contains("PrivateNetwork")),
            "{line:?}"
        );
        let properties: Vec<&String> = line
            .iter()
            .filter(|a| a.starts_with("--property="))
            .collect();
        assert_eq!(properties, ["--property=RuntimeMaxSec=5"]);
    }
}

/// Catches a tool that cannot run being started: a relative program is a launcher failure, a
/// non-executable one is `NoStart`, each before anything is spawned.
#[test]
fn a_tool_that_cannot_run_is_refused_before_anything_is_spawned() {
    let (scratch, _tool) = tool_file("tool-refused");
    let plain = scratch.path().join("plain");
    std::fs::write(&plain, "data").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let recorder = Recorder::default();
    let launcher = SystemdLauncher::new(&recorder);

    let err = launcher
        .run_tool(&call(Path::new("bin/tool"), &[], 5))
        .unwrap_err();
    assert!(
        matches!(&err, LaunchError::Launcher(text)
            if text == "the tool \"bin/tool\" is not an absolute path"),
        "{err:?}"
    );
    let err = launcher.run_tool(&call(&plain, &[], 5)).unwrap_err();
    assert!(
        matches!(&err, LaunchError::NoStart(text)
            if *text == format!("{} is not an executable file", plain.display())),
        "{err:?}"
    );
    assert!(recorder.calls.lock().unwrap().is_empty());
}

/// Catches a tool's end read differently from a step's: the exit code and last line of
/// `systemd-run` are the tool's exit, and a tool past its bound (a code that is not 0, or no
/// code) is a failed exit exactly as for a step.
#[test]
fn the_exit_and_last_line_are_read_as_for_a_step() {
    let (_scratch, tool) = tool_file("tool-exit");
    let recorder = Recorder::answering(vec![
        captured(Some(0), b"unpacking\ndone\n"),
        captured(Some(1), b"timed out\n"),
        captured(None, b""),
    ]);
    let launcher = SystemdLauncher::new(&recorder);
    let exits: Vec<StepExit> = (0..3)
        .map(|_| launcher.run_tool(&call(&tool, &[], 5)).unwrap())
        .collect();
    let exit = |code, line: Option<&str>| StepExit {
        code,
        last_line: line.map(str::to_owned),
    };
    assert_eq!(
        exits,
        [
            exit(Some(0), Some("done")),
            exit(Some(1), Some("timed out")),
            exit(None, None),
        ]
    );
}

/// Catches a launcher without tool runs starting one: the default `run_tool` starts nothing and
/// answers `NoStart` naming the program and the service.
#[test]
fn the_default_run_tool_is_no_start() {
    struct Plain;
    impl StepLauncher for Plain {
        fn run(&self, _: &StepCall<'_>) -> Result<StepExit, LaunchError> {
            panic!("a step was run");
        }
        fn stop(&self, _: &str) -> Result<(), LaunchError> {
            Ok(())
        }
    }
    let err = Plain
        .run_tool(&call(Path::new("/usr/bin/tar"), &[], 5))
        .unwrap_err();
    assert!(
        matches!(&err, LaunchError::NoStart(text)
            if text == "this launcher runs no tool, so /usr/bin/tar for svc did not start"),
        "{err:?}"
    );
}
