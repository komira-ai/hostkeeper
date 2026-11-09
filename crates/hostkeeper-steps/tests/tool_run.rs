//! A tool run on the launcher of the host (docs/design/packages.md, 5.3): the `systemd-run` line
//! of the tool's unit carries `PrivateNetwork=yes` besides `RuntimeMaxSec=` unless it is a fetch,
//! `ProtectSystem=strict`, `PrivateTmp=yes` and `ReadWritePaths=` when it lists writable paths,
//! a working directory and an environment when it names them; the lines of a hook step and an
//! additive step carry none of it; the exit is read as for a step and both streams are answered.
//! A launcher with no tool runs keeps the default, `NoStart`.

mod support;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_hook::{Invocation, Step};
use hostkeeper_steps::systemd::{additive_command, command, tool_command};
use hostkeeper_steps::{
    AdditiveCall, Captured, LaunchError, Program, RING_BYTES, Ring, Spawner, StepCall, StepExit,
    StepLauncher, SystemdLauncher, ToolCall, ToolExit, ToolNetwork,
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
        network: ToolNetwork::Offline,
        writable: None,
        working_dir: None,
        env: &[],
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

/// Catches a tool's end read differently from a step's: the exit code of `systemd-run` is the
/// tool's exit with its output kept whole, and a tool past its bound (a code that is not 0, or no
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
    let exits: Vec<ToolExit> = (0..3)
        .map(|_| launcher.run_tool(&call(&tool, &[], 5)).unwrap())
        .collect();
    let exit = |code, out: &str| ToolExit {
        code,
        stdout: out.as_bytes().to_vec(),
        stderr: Vec::new(),
        cut: false,
    };
    assert_eq!(
        exits,
        [
            exit(Some(0), "unpacking\ndone\n"),
            exit(Some(1), "timed out\n"),
            exit(None, ""),
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

fn as_strings(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

fn head(max: &str) -> Vec<String> {
    [
        "--system",
        "--quiet",
        "--wait",
        "--collect",
        "--pipe",
        &format!("--unit={UNIT}"),
        &format!("--property=RuntimeMaxSec={max}"),
    ]
    .map(str::to_owned)
    .to_vec()
}

fn with(mut line: Vec<String>, rest: &[&str]) -> Vec<String> {
    line.extend(rest.iter().map(|a| (*a).to_owned()));
    line
}

/// Catches an extract that can write outside its target or reach the network: an offline,
/// confined tool has `PrivateNetwork=yes`, then `ProtectSystem=strict` and `PrivateTmp=yes`, then
/// one `ReadWritePaths=` per path in order, then `--` and the program; `Some(&[])` is
/// `ProtectSystem=strict` and `PrivateTmp=yes` alone.
#[test]
fn a_confined_offline_tool_lists_its_writable_paths() {
    let (_scratch, tool) = tool_file("tool-confined");
    let (a, b) = (Path::new("/srv/one"), Path::new("/srv/two"));
    let paths = [a, b];
    let args = vec![OsString::from("x")];
    let mut c = call(&tool, &args, 30);
    c.writable = Some(&paths);
    assert_eq!(
        strings(tool_command(&c).unwrap()),
        with(
            head("30"),
            &[
                "--property=PrivateNetwork=yes",
                "--property=ProtectSystem=strict",
                "--property=PrivateTmp=yes",
                "--property=ReadWritePaths=/srv/one",
                "--property=ReadWritePaths=/srv/two",
                "--",
                &as_strings(&tool),
                "x",
            ]
        )
    );
    c.writable = Some(&[]);
    assert_eq!(
        strings(tool_command(&c).unwrap()),
        with(
            head("30"),
            &[
                "--property=PrivateNetwork=yes",
                "--property=ProtectSystem=strict",
                "--property=PrivateTmp=yes",
                "--",
                &as_strings(&tool),
                "x",
            ]
        )
    );
}

/// Catches a fetch that is not confined or is cut off from the network: a fetch has no
/// `PrivateNetwork`, has `ProtectSystem=strict`, `PrivateTmp=yes` and its `ReadWritePaths=`,
/// then the working directory and each `--setenv=` in order, all before `--`; the launcher runs
/// that same line.
#[test]
fn a_fetch_has_the_network_a_confinement_a_working_directory_and_an_environment() {
    let (_scratch, tool) = tool_file("tool-fetch");
    let (dir, cache) = (Path::new("/var/cache/hk/dl"), Path::new("/var/lib/apt"));
    let paths = [dir, cache];
    let env = [("LC_ALL", "C"), ("_X1", "a b")];
    let args = vec![OsString::from("download"), OsString::from("pkg")];
    let mut c = call(&tool, &args, 0);
    c.network = ToolNetwork::Fetch;
    c.writable = Some(&paths);
    c.working_dir = Some(dir);
    c.env = &env;
    let line = with(
        head("infinity"),
        &[
            "--property=ProtectSystem=strict",
            "--property=PrivateTmp=yes",
            "--property=ReadWritePaths=/var/cache/hk/dl",
            "--property=ReadWritePaths=/var/lib/apt",
            "--working-directory=/var/cache/hk/dl",
            "--setenv=LC_ALL=C",
            "--setenv=_X1=a b",
            "--",
            &as_strings(&tool),
            "download",
            "pkg",
        ],
    );
    assert_eq!(strings(tool_command(&c).unwrap()), line);

    let recorder = Recorder::answering(vec![captured(Some(0), b"")]);
    SystemdLauncher::new(&recorder).run_tool(&c).unwrap();
    assert_eq!(
        *recorder.calls.lock().unwrap(),
        [(Program::SystemdRun, line)]
    );
}

/// Catches an install or purge that is confined: with `writable: None` there is no
/// `ProtectSystem` or `ReadWritePaths`, the network is off, and the environment is set.
#[test]
fn an_unconfined_offline_install_has_only_its_environment() {
    let (_scratch, tool) = tool_file("tool-install");
    let env = [("DEBIAN_FRONTEND", "noninteractive")];
    let args = vec![OsString::from("install")];
    let mut c = call(&tool, &args, 600);
    c.env = &env;
    assert_eq!(
        strings(tool_command(&c).unwrap()),
        with(
            head("600"),
            &[
                "--property=PrivateNetwork=yes",
                "--setenv=DEBIAN_FRONTEND=noninteractive",
                "--",
                &as_strings(&tool),
                "install",
            ]
        )
    );
}

/// Catches a call with a path, name or value that would be misread by systemd being started:
/// each is a launcher failure with its own text, and nothing is spawned.
#[test]
fn a_call_that_systemd_would_misread_is_refused_before_anything_is_spawned() {
    let (_scratch, tool) = tool_file("tool-refuse-call");
    let recorder = Recorder::default();
    let launcher = SystemdLauncher::new(&recorder);
    let relative = [Path::new("rel/dir")];
    let spaced = [Path::new("/srv/a b")];
    let abs = [Path::new("/srv/a")];
    let bad_name = [("lower", "v")];
    let empty_name = [("", "v")];
    let bad_char = [("A-B", "v")];
    let newline = [("NAME", "a\nb")];
    let nul = [("NAME", "a\0b")];

    let refusals: Vec<(ToolCall<'_>, String)> = vec![
        (
            ToolCall {
                writable: Some(&relative),
                ..call(&tool, &[], 5)
            },
            "the writable path \"rel/dir\" is not an absolute path".to_owned(),
        ),
        (
            ToolCall {
                writable: Some(&spaced),
                ..call(&tool, &[], 5)
            },
            "the writable path \"/srv/a b\" holds whitespace".to_owned(),
        ),
        (
            ToolCall {
                working_dir: Some(Path::new("work")),
                ..call(&tool, &[], 5)
            },
            "the working directory \"work\" is not an absolute path".to_owned(),
        ),
        (
            ToolCall {
                env: &bad_name,
                ..call(&tool, &[], 5)
            },
            "the environment name \"lower\" is not [A-Z_][A-Z0-9_]*".to_owned(),
        ),
        (
            ToolCall {
                env: &empty_name,
                ..call(&tool, &[], 5)
            },
            "the environment name \"\" is not [A-Z_][A-Z0-9_]*".to_owned(),
        ),
        (
            ToolCall {
                env: &bad_char,
                ..call(&tool, &[], 5)
            },
            "the environment name \"A-B\" is not [A-Z_][A-Z0-9_]*".to_owned(),
        ),
        (
            ToolCall {
                env: &newline,
                ..call(&tool, &[], 5)
            },
            "the value of the environment variable NAME holds a NUL or a newline".to_owned(),
        ),
        (
            ToolCall {
                env: &nul,
                ..call(&tool, &[], 5)
            },
            "the value of the environment variable NAME holds a NUL or a newline".to_owned(),
        ),
        (
            ToolCall {
                network: ToolNetwork::Fetch,
                writable: None,
                ..call(&tool, &[], 5)
            },
            "a tool that fetches must run confined, with a writable list".to_owned(),
        ),
    ];
    for (refused, text) in refusals {
        let err = launcher.run_tool(&refused).unwrap_err();
        assert!(
            matches!(&err, LaunchError::Launcher(got) if *got == text),
            "{err:?}"
        );
    }
    // The same fetch with a writable list is accepted.
    let ok = ToolCall {
        network: ToolNetwork::Fetch,
        writable: Some(&abs),
        ..call(&tool, &[], 5)
    };
    assert!(tool_command(&ok).is_ok());
    assert!(recorder.calls.lock().unwrap().is_empty());
}

/// Catches output lost or mislabelled: both streams are answered whole as bytes, and `cut` is
/// false while neither stream outgrew its ring, true when the standard output did, and true when
/// only the standard error did.
#[test]
fn the_output_of_both_streams_and_whether_it_was_cut_are_answered() {
    let (_scratch, tool) = tool_file("tool-output");
    let long = vec![b'x'; RING_BYTES + 10];
    let reply = |out: &[u8], err: &[u8]| {
        let (mut o, mut e) = (Ring::new(), Ring::new());
        o.push(out);
        e.push(err);
        Captured {
            code: Some(3),
            stdout: o,
            stderr: e,
        }
    };
    let recorder = Recorder::answering(vec![
        reply(b"out\n", b"err\n"),
        reply(&long, b"e"),
        reply(b"o", &long),
    ]);
    let launcher = SystemdLauncher::new(&recorder);
    let runs: Vec<ToolExit> = (0..3)
        .map(|_| launcher.run_tool(&call(&tool, &[], 5)).unwrap())
        .collect();

    assert_eq!(
        runs[0],
        ToolExit {
            code: Some(3),
            stdout: b"out\n".to_vec(),
            stderr: b"err\n".to_vec(),
            cut: false,
        }
    );
    assert!(runs[1].cut);
    assert_eq!(runs[1].stdout.len(), RING_BYTES);
    assert_eq!(runs[1].stderr, b"e");
    assert!(runs[2].cut);
    assert_eq!(runs[2].stdout, b"o");
    assert_eq!(runs[2].stderr.len(), RING_BYTES);
}

/// Catches a stopped tool reading as a success: a tool whose unit `stop` ended while it ran has
/// no code even when `systemd-run` exited 0, and its output is still answered.
#[test]
fn a_stopped_tool_has_no_code() {
    let (_scratch, tool) = tool_file("tool-stopped");
    type Hook = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;
    struct StopsItself(Hook);
    impl Spawner for StopsItself {
        fn spawn(&self, program: Program, _: &[OsString]) -> io::Result<Captured> {
            if program == Program::SystemdRun {
                if let Some(stop) = self.0.lock().unwrap().as_ref() {
                    stop();
                }
                Ok(captured(
                    Some(0),
                    b"partial
",
                ))
            } else {
                Ok(captured(Some(0), b""))
            }
        }
    }
    let hook: Hook = Arc::default();
    let launcher = Arc::new(SystemdLauncher::new(StopsItself(Arc::clone(&hook))));
    let inner = Arc::clone(&launcher);
    *hook.lock().unwrap() = Some(Box::new(move || inner.stop(UNIT).unwrap()));
    let exit = launcher.run_tool(&call(&tool, &[], 5)).unwrap();
    assert_eq!(exit.code, None);
    assert_eq!(exit.stdout, b"partial\n");
}
