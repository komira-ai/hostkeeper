//! The real adapters, each without the host's real service manager, processes or run directory:
//! the systemd adapter over a scripted runner, the real runner over this test binary as its
//! child, the `/proc` probe over a tree in a scratch directory, and the unlink of the stale
//! registration socket in a scratch directory.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_observe::fake::FakeCommandRunner;
use hostkeeper_observe::proc::digest_open_file;
use hostkeeper_observe::socket::REGISTER_SOCKET;
use hostkeeper_observe::systemd::SHOW_PROPERTIES;
use hostkeeper_observe::{
    ActiveState, CommandOutput, CommandRunner, ExeStat, ObserveError, ProcProbe, ProcessIdentity,
    ProcessProbe, ProcessRunner, RunError, ServiceManager, SystemdManager, UnitState,
    unlink_stale_registration_socket,
};
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

const UNIT: &str = "tierstore@9301.service";

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

fn sha(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

fn printed(stdout: &str) -> CommandOutput {
    CommandOutput {
        exit_code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        truncated: false,
    }
}

/// The systemd adapter over a runner that answers `answer`, asked about [`UNIT`].
fn systemd(answer: Result<CommandOutput, RunError>) -> Result<UnitState, ObserveError> {
    let runner = FakeCommandRunner::default();
    runner.then(answer);
    SystemdManager::new(runner, Duration::from_secs(5)).unit_state(UNIT)
}

fn manager_error(why: &str) -> Result<UnitState, ObserveError> {
    Err(ObserveError::Manager {
        unit: UNIT.to_owned(),
        why: why.to_owned(),
    })
}

/// Catches the adapter asking anything but `systemctl show` of the four properties for the one
/// unit, with the unit after `--` so no name is read as an option, and within its deadline.
#[test]
fn systemd_asks_for_the_four_properties_of_one_unit() {
    let runner = FakeCommandRunner::default();
    runner.then(Ok(printed(
        "ActiveState=active\nSubState=running\nMainPID=4242\nLoadState=loaded\n",
    )));
    let manager = SystemdManager::new(runner.clone(), Duration::from_secs(5));
    assert_eq!(
        manager.unit_state(UNIT),
        Ok(UnitState {
            active: ActiveState::Active,
            sub: "running".to_owned(),
            main_pid: Some(4242),
        })
    );
    let argv = ["systemctl", "show", SHOW_PROPERTIES, "--", UNIT].map(str::to_owned);
    assert_eq!(runner.calls(), [(argv.to_vec(), Duration::from_secs(5))]);
    assert_eq!(
        SHOW_PROPERTIES,
        "--property=ActiveState,SubState,MainPID,LoadState"
    );
}

/// Catches an `ActiveState` or `LoadState` read as the wrong state, and `MainPID=0` read as a
/// process.
#[test]
fn systemd_states_map_to_the_port_states() {
    let show = |load: &str, active: &str, pid: &str| {
        systemd(Ok(printed(&format!(
            "LoadState={load}\nActiveState={active}\nSubState=sub\nMainPID={pid}\n"
        ))))
    };
    let state = |active, main_pid| {
        Ok(UnitState {
            active,
            sub: "sub".to_owned(),
            main_pid,
        })
    };
    let cases = [
        ("loaded", "active", "7", state(ActiveState::Active, Some(7))),
        (
            "loaded",
            "reloading",
            "7",
            state(ActiveState::Active, Some(7)),
        ),
        (
            "loaded",
            "refreshing",
            "7",
            state(ActiveState::Active, Some(7)),
        ),
        ("loaded", "active", "0", state(ActiveState::Active, None)),
        (
            "loaded",
            "inactive",
            "0",
            state(ActiveState::Inactive, None),
        ),
        ("loaded", "failed", "0", state(ActiveState::Failed, None)),
        (
            "loaded",
            "activating",
            "9",
            state(ActiveState::Activating, Some(9)),
        ),
        (
            "loaded",
            "deactivating",
            "9",
            state(ActiveState::Deactivating, Some(9)),
        ),
        (
            "not-found",
            "inactive",
            "0",
            state(ActiveState::NotFound, None),
        ),
        (
            "masked",
            "inactive",
            "0",
            state(ActiveState::Inactive, None),
        ),
    ];
    for (load, active, pid, want) in cases {
        assert_eq!(show(load, active, pid), want, "{load} {active} {pid}");
    }
}

/// Catches an answer systemd did not give, or one this adapter cannot read, being made into a
/// state: every one is an error.
#[test]
fn systemd_failures_are_errors_never_states() {
    let not_utf8 = CommandOutput {
        stdout: vec![b'L', 0xff, b'\n'],
        ..printed("")
    };
    let cases = [
        (
            Err(RunError::TimedOut {
                program: "systemctl".to_owned(),
            }),
            "systemctl did not finish within its deadline and was killed",
        ),
        (
            Ok(CommandOutput {
                exit_code: Some(1),
                ..printed("")
            }),
            "systemctl show exited with code 1",
        ),
        (
            Ok(CommandOutput {
                exit_code: None,
                ..printed("")
            }),
            "systemctl show exited with a signal",
        ),
        (
            Ok(CommandOutput {
                truncated: true,
                ..printed("LoadState=loaded\n")
            }),
            "systemctl show printed more than its cap",
        ),
        (
            Ok(not_utf8),
            "systemctl show printed something that is not UTF-8",
        ),
        (
            Ok(printed("ActiveState=active\nSubState=running\nMainPID=1\n")),
            "systemctl show printed no LoadState",
        ),
        (
            Ok(printed("LoadState=loaded\nSubState=running\nMainPID=1\n")),
            "systemctl show printed no ActiveState",
        ),
        (
            Ok(printed("LoadState=loaded\nActiveState=active\nMainPID=1\n")),
            "systemctl show printed no SubState",
        ),
        (
            Ok(printed(
                "LoadState=loaded\nActiveState=active\nSubState=running\n",
            )),
            "systemctl show printed no MainPID",
        ),
        (
            Ok(printed(
                "LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=-1\n",
            )),
            "MainPID \"-1\" is not a number",
        ),
        (
            Ok(printed(
                "LoadState=loaded\nActiveState=maintenance\nSubState=x\nMainPID=0\n",
            )),
            "ActiveState \"maintenance\" is not one this daemon knows",
        ),
    ];
    for (answer, why) in cases {
        assert_eq!(systemd(answer), manager_error(why), "{why}");
    }
    // A runner with nothing scripted fails, and so does the adapter.
    let runner = FakeCommandRunner::default();
    assert_eq!(
        SystemdManager::new(runner, Duration::from_secs(1)).unit_state(UNIT),
        manager_error("there is no program to run")
    );
}

/// This test binary, run as a child of the real runner with `args`.
fn this_binary(args: &[&str]) -> Vec<String> {
    let exe = std::env::current_exe().expect("the test binary");
    let mut argv = vec![exe.to_string_lossy().into_owned()];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    argv
}

/// Not a test of its own: the child of `the_real_runner_kills_a_child_at_its_deadline`, which
/// gives it a standard input that does not end until the child is killed.
#[test]
#[ignore = "the child process of the_real_runner_kills_a_child_at_its_deadline; run only by it"]
fn child_reads_stdin_to_its_end() {
    let _ = std::io::read_to_string(std::io::stdin());
}

/// Catches the real runner losing a child's output or exit code, or reading past its cap.
#[test]
fn the_real_runner_reads_a_child_to_its_end() {
    let list = this_binary(&["--list"]);
    let runner = ProcessRunner::new(hostkeeper_observe::command::MAX_STDOUT);
    let out = runner
        .run(&list, Duration::from_secs(60))
        .expect("the child ran");
    assert_eq!(out.exit_code, Some(0));
    assert!(!out.truncated);
    let text = String::from_utf8(out.stdout.clone()).expect("UTF-8");
    assert!(
        text.contains("the_real_runner_reads_a_child_to_its_end: test"),
        "{text}"
    );

    let capped = ProcessRunner::new(8)
        .run(&list, Duration::from_secs(60))
        .expect("the child ran");
    assert!(capped.truncated);
    assert_eq!(capped.stdout, out.stdout[..8]);

    let refused = runner
        .run(&this_binary(&["--no-such-option"]), Duration::from_secs(60))
        .expect("the child ran");
    assert_ne!(refused.exit_code, Some(0));
}

/// Catches a child that never ends holding the runner: past the deadline it is killed and the
/// run is an error. The child waits on a standard input that only its death closes, so the
/// deadline (zero) has passed before it could print its end.
#[test]
fn the_real_runner_kills_a_child_at_its_deadline() {
    let argv = this_binary(&["child_reads_stdin_to_its_end", "--exact", "--ignored"]);
    let program = argv[0].clone();
    let result = ProcessRunner::new(1024).run(&argv, Duration::ZERO);
    assert_eq!(result, Err(RunError::TimedOut { program }));
}

/// The variables systemd sets for the daemon's own process (sd_notify(3), sd_listen_fds(3)),
/// each with the value the parent of the runner is started with.
const MANAGER_ONLY: [(&str, &str); 6] = [
    ("NOTIFY_SOCKET", "@hostkeeper-test-notify"),
    ("WATCHDOG_USEC", "30000000"),
    ("WATCHDOG_PID", "1"),
    ("LISTEN_FDS", "1"),
    ("LISTEN_PID", "1"),
    ("LISTEN_FDNAMES", "admin"),
];
/// A variable of the parent that is not the service manager's: the child keeps it.
const KEPT: (&str, &str) = ("HOSTKEEPER_TEST_KEPT", "kept");
/// The prefix of each line the printing child writes for one variable of its environment.
const ENV_LINE: &str = "env:";

/// Not a test of its own: the grandchild of
/// `the_real_runner_gives_its_child_none_of_the_service_managers_variables`, which prints the
/// environment it was started with, one `env:<name>=<value>` line per variable.
#[test]
#[ignore = "the grandchild process of the_real_runner_gives_its_child_none_of_the_service_managers_variables; run only by it"]
#[expect(
    clippy::disallowed_methods,
    reason = "the test reads what environment reached the runner's child; a process of the tests"
)]
fn child_prints_its_environment() {
    let mut out = std::io::stdout();
    for (name, value) in std::env::vars_os() {
        let line = format!("{ENV_LINE}{}={}\n", name.display(), value.display());
        out.write_all(line.as_bytes()).expect("print a variable");
    }
}

/// Not a test of its own: the child of
/// `the_real_runner_gives_its_child_none_of_the_service_managers_variables`, started with the
/// service manager's variables as the daemon is; it runs the printing child through the real
/// runner and prints what that child printed.
#[test]
#[ignore = "the child process of the_real_runner_gives_its_child_none_of_the_service_managers_variables; run only by it"]
fn child_runs_the_printing_child_through_the_real_runner() {
    let argv = this_binary(&[
        "child_prints_its_environment",
        "--exact",
        "--ignored",
        "--nocapture",
    ]);
    let out = ProcessRunner::new(hostkeeper_observe::command::MAX_STDOUT)
        .run(&argv, Duration::from_secs(60))
        .expect("the printing child ran");
    assert_eq!(out.exit_code, Some(0));
    std::io::stdout()
        .write_all(&out.stdout)
        .expect("print what the printing child printed");
}

/// Catches the daemon's notify socket, watchdog and passed sockets reaching a program it runs
/// (`systemctl`), which would then speak to systemd as the daemon: systemd's own tools send
/// `EXIT_STATUS=` to `NOTIFY_SOCKET` when it is set. A parent started with those variables, and
/// one more, runs a child through the real runner: the child has the one more, and none of them.
#[test]
fn the_real_runner_gives_its_child_none_of_the_service_managers_variables() {
    let argv = this_binary(&[
        "child_runs_the_printing_child_through_the_real_runner",
        "--exact",
        "--ignored",
        "--nocapture",
    ]);
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .envs(MANAGER_ONLY)
        .env(KEPT.0, KEPT.1)
        .output()
        .expect("the parent of the runner ran");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8(out.stdout).expect("UTF-8");
    let seen: Vec<(&str, &str)> = printed
        .lines()
        .filter_map(|line| line.strip_prefix(ENV_LINE))
        .filter_map(|variable| variable.split_once('='))
        .filter(|(name, _)| *name == KEPT.0 || MANAGER_ONLY.iter().any(|(m, _)| m == name))
        .collect();
    assert_eq!(seen, [KEPT]);
}

/// Catches a program that cannot start, and an empty argv, being reported as a run.
#[test]
fn the_real_runner_reports_what_it_cannot_run() {
    let runner = ProcessRunner::new(1024);
    assert_eq!(
        runner.run(&[], Duration::from_secs(1)),
        Err(RunError::Empty)
    );
    let dir = scratch("runner-missing");
    let missing = dir
        .path()
        .join("no-such-program")
        .to_string_lossy()
        .into_owned();
    let result = runner.run(std::slice::from_ref(&missing), Duration::from_secs(1));
    assert_eq!(
        result,
        Err(RunError::Failed {
            program: missing,
            why: "No such file or directory (os error 2)".to_owned(),
        })
    );
}

/// A `/proc`-shaped tree: `<root>/<pid>/stat` holding `stat`, and `<root>/<pid>/exe` a link to
/// `exe` when given.
fn proc_entry(root: &Path, pid: u32, stat: &str, exe: Option<&Path>) -> PathBuf {
    let dir = root.join(pid.to_string());
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(dir.join("stat"), stat).expect("write stat");
    exe.into_iter()
        .for_each(|target| symlink(target, dir.join("exe")).expect("link exe"));
    dir
}

/// Catches the start time read from the wrong field, or shifted by a command name holding spaces
/// and parentheses, and a stat line too short to have one read as a time.
#[test]
fn the_proc_probe_reads_the_start_time_from_field_22() {
    let root = scratch("proc-stat");
    let stat = "101 (tier) store)) S 1 101 101 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 4 0 987654 123456 789\n";
    proc_entry(root.path(), 101, stat, None);
    let probe = ProcProbe::new(root.path());
    assert_eq!(
        probe.identity(101),
        Ok(ProcessIdentity {
            pid: 101,
            start_time: 987_654,
        })
    );

    let short = proc_entry(root.path(), 102, "102 (tierstore) S 1 2 3\n", None);
    assert_eq!(
        probe.identity(102),
        Err(ObserveError::Process {
            pid: 102,
            why: format!(
                "{}: no start time in field 22",
                short.join("stat").display()
            ),
        })
    );
    let no_paren = proc_entry(root.path(), 103, "garbage\n", None);
    assert_eq!(
        probe.identity(103),
        Err(ObserveError::Process {
            pid: 103,
            why: format!(
                "{}: no start time in field 22",
                no_paren.join("stat").display()
            ),
        })
    );
    let gone = root.path().join("104").join("stat");
    assert_eq!(
        probe.identity(104),
        Err(ObserveError::Process {
            pid: 104,
            why: format!("{}: No such file or directory (os error 2)", gone.display()),
        })
    );
}

/// Catches the executable's digest or stat taken of anything but the file behind `exe`.
#[test]
fn the_proc_probe_hashes_and_stats_the_file_behind_exe() {
    let root = scratch("proc-exe");
    let binary = root.path().join("tierstore");
    let content = vec![b'r'; 100_000];
    fs::write(&binary, &content).expect("write the binary");
    proc_entry(root.path(), 101, "x", Some(&binary));
    let probe = ProcProbe::new(root.path());
    let meta = fs::metadata(&binary).expect("stat");
    assert_eq!(
        probe.exe_stat(101),
        Ok(ExeStat {
            size: 100_000,
            mtime_secs: meta.mtime(),
            mtime_nanos: meta.mtime_nsec(),
        })
    );
    assert_eq!(probe.exe_digest(101).map(|d| d.to_vec()), Ok(sha(&content)));

    // An `exe` that opens but cannot be read (a directory) is an error, not a digest.
    let dir_exe = proc_entry(root.path(), 106, "x", Some(root.path()));
    assert_eq!(
        probe.exe_digest(106),
        Err(ObserveError::Process {
            pid: 106,
            why: format!(
                "{}: Is a directory (os error 21)",
                dir_exe.join("exe").display()
            ),
        })
    );

    let gone = root.path().join("105").join("exe");
    let missing = ObserveError::Process {
        pid: 105,
        why: format!("{}: No such file or directory (os error 2)", gone.display()),
    };
    assert_eq!(probe.exe_stat(105), Err(missing.clone()));
    assert_eq!(probe.exe_digest(105), Err(missing));
}

/// Catches the hash re-opening the path: the file is replaced on disk after it was opened, as a
/// release switch replaces a binary under a running process, and the digest is still the opened
/// file's.
#[test]
fn a_replaced_binary_on_disk_is_hashed_from_the_running_inode() {
    let dir = scratch("replaced");
    let path = dir.path().join("tierstore");
    let running = vec![b'1'; 70_000];
    fs::write(&path, &running).expect("write the running binary");
    let opened = fs::File::open(&path).expect("open");
    let next = dir.path().join("tierstore-next");
    let mut file = fs::File::create(&next).expect("create");
    file.write_all(b"the next release").expect("write");
    fs::rename(&next, &path).expect("replace");

    assert_eq!(
        digest_open_file(&opened).expect("hash").to_vec(),
        sha(&running)
    );
    let on_disk = fs::File::open(&path).expect("open the new file");
    assert_eq!(
        digest_open_file(&on_disk).expect("hash").to_vec(),
        sha(b"the next release")
    );
}

/// Catches a stale socket left by kbf's agent surviving the start.
#[test]
fn a_stale_register_socket_is_unlinked_at_start() {
    let dir = scratch("sock");
    let path = dir.path().join(REGISTER_SOCKET);
    drop(UnixListener::bind(&path).expect("bind"));
    assert!(
        fs::symlink_metadata(&path).is_ok(),
        "the socket file stays after the listener"
    );
    assert!(unlink_stale_registration_socket(dir.path()).expect("unlinked"));
    assert!(fs::symlink_metadata(&path).is_err());
    assert_eq!(REGISTER_SOCKET, "register.sock");
}

/// Catches a start refused because there was nothing to remove.
#[test]
fn a_missing_one_is_fine() {
    let dir = scratch("sock-missing");
    assert!(!unlink_stale_registration_socket(dir.path()).expect("nothing to do"));
}

/// Catches a file of another kind in the socket's place being left for a client to find.
#[test]
fn a_regular_file_there_is_removed_too() {
    let dir = scratch("sock-file");
    let path = dir.path().join(REGISTER_SOCKET);
    fs::write(&path, b"").expect("write");
    assert!(unlink_stale_registration_socket(dir.path()).expect("unlinked"));
    assert!(fs::symlink_metadata(&path).is_err());
}

/// Catches a failure to remove the socket being swallowed: the daemon refuses to start on it.
/// The run directory is read-only, so the socket in it cannot be removed; a directory in the
/// socket's place cannot be unlinked either.
#[test]
fn a_permission_error_is_surfaced() {
    let dir = scratch("sock-perm");
    let run = dir.path().join("run");
    fs::create_dir(&run).expect("mkdir");
    let path = run.join(REGISTER_SOCKET);
    fs::write(&path, b"").expect("write");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o555)).expect("chmod");
    let refused = unlink_stale_registration_socket(&run).expect_err("refused");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert_eq!(refused.path, path);
    assert_eq!(
        refused.to_string(),
        format!(
            "cannot remove the stale registration socket {}: Permission denied (os error 13)",
            path.display()
        )
    );
    assert!(fs::symlink_metadata(&path).is_ok());

    let blocked = dir.path().join("blocked");
    fs::create_dir_all(blocked.join(REGISTER_SOCKET)).expect("mkdir");
    let refused = unlink_stale_registration_socket(&blocked).expect_err("a directory");
    assert_eq!(refused.path, blocked.join(REGISTER_SOCKET));
}
