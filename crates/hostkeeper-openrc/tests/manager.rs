//! `OpenrcServiceManager`: what each exit of `rc-service <service> status` reads as, the child
//! pid supervise-daemon keeps, and every way a state cannot be read. The command runner is the
//! fake of `hostkeeper_observe::fake`; the init directory and the run directory are scratch
//! directories, so no OpenRC and no host path is touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_observe::fake::FakeCommandRunner;
use hostkeeper_observe::{
    ActiveState, CommandOutput, ObserveError, RunError, ServiceManager, UnitState,
};
use hostkeeper_openrc::OpenrcServiceManager;
use hostkeeper_openrc::manager::{CHILD_PID, INIT_DIR, OPENRC_RUN_DIR, parse_child_pid, state_of};
use hostkeeper_testkit::Scratch;

const DEADLINE: Duration = Duration::from_secs(9);

struct Host {
    scratch: Scratch,
    runner: FakeCommandRunner,
    manager: OpenrcServiceManager<FakeCommandRunner>,
}

impl Host {
    fn new(name: &str) -> Host {
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
            .expect("a scratch directory");
        fs::create_dir(scratch.path().join("init.d")).expect("init dir");
        fs::create_dir(scratch.path().join("run")).expect("run dir");
        let runner = FakeCommandRunner::default();
        let manager = OpenrcServiceManager::new(
            runner.clone(),
            DEADLINE,
            &scratch.path().join("init.d"),
            &scratch.path().join("run"),
        );
        Host {
            scratch,
            runner,
            manager,
        }
    }

    fn init_script(&self, service: &str) -> PathBuf {
        let path = self.scratch.path().join("init.d").join(service);
        fs::write(&path, "#!/sbin/openrc-run\n").expect("init script");
        path
    }

    fn options_dir(&self, service: &str) -> PathBuf {
        let dir = self.scratch.path().join("run/options").join(service);
        fs::create_dir_all(&dir).expect("options dir");
        dir
    }

    fn child_pid_file(&self, service: &str, text: &str) -> PathBuf {
        let path = self.options_dir(service).join(CHILD_PID);
        fs::write(&path, text).expect("child_pid");
        path
    }

    fn exits(&self, code: Option<i32>) {
        self.runner.then(Ok(CommandOutput {
            exit_code: code,
            stdout: Vec::new(),
            truncated: false,
        }));
    }
}

fn status_argv(service: &str) -> Vec<String> {
    ["/sbin/rc-service", service, "status"]
        .map(str::to_owned)
        .to_vec()
}

fn manager_error(unit: &str, why: String) -> ObserveError {
    ObserveError::Manager {
        unit: unit.to_owned(),
        why,
    }
}

#[test]
fn the_host_paths_are_openrcs() {
    assert_eq!(INIT_DIR, "/etc/init.d");
    assert_eq!(OPENRC_RUN_DIR, "/run/openrc");
    assert_eq!(CHILD_PID, "child_pid");
}

#[test]
fn state_of_reads_each_exit_openrc_gives() {
    assert_eq!(state_of(0), Some((ActiveState::Active, "started")));
    assert_eq!(state_of(3), Some((ActiveState::Inactive, "stopped")));
    assert_eq!(state_of(4), Some((ActiveState::Deactivating, "stopping")));
    assert_eq!(state_of(8), Some((ActiveState::Activating, "starting")));
    assert_eq!(state_of(16), Some((ActiveState::Inactive, "inactive")));
    assert_eq!(state_of(32), Some((ActiveState::Failed, "crashed")));
    assert_eq!(state_of(64), Some((ActiveState::Failed, "unsupervised")));
}

#[test]
fn state_of_reads_no_other_exit() {
    for code in [1, 2, 5, 7, 9, 15, 17, 31, 33, 63, 65, 127, 128, 255, -1] {
        assert_eq!(state_of(code), None, "{code}");
    }
}

#[test]
fn parse_child_pid_takes_digits_and_one_line_feed() {
    assert_eq!(parse_child_pid("123"), Some(123));
    assert_eq!(parse_child_pid("123\n"), Some(123));
    assert_eq!(parse_child_pid("1"), Some(1));
    assert_eq!(parse_child_pid("007"), Some(7));
    assert_eq!(parse_child_pid("4294967295"), Some(u32::MAX));
}

#[test]
fn parse_child_pid_refuses_all_else() {
    for text in [
        "",
        "\n",
        "0",
        "0\n",
        "000",
        "abc",
        "12 \n",
        " 12",
        "12\n\n",
        "12\r\n",
        "-5",
        "+5",
        "1 2",
        "4294967296",
        "99999999999999999999",
        "\u{663}",
    ] {
        assert_eq!(parse_child_pid(text), None, "{text:?}");
    }
}

#[test]
fn a_service_with_no_init_script_is_not_found_and_nothing_runs() {
    let host = Host::new("no-script");
    assert_eq!(
        host.manager.unit_state("hostkeeper"),
        Ok(UnitState {
            active: ActiveState::NotFound,
            sub: "no init script".to_owned(),
            main_pid: None,
        })
    );
    assert!(host.runner.calls().is_empty());
}

#[test]
fn a_child_pid_file_is_not_read_for_a_service_with_no_init_script() {
    let host = Host::new("no-script-pid");
    host.child_pid_file("hostkeeper", "123\n");
    let state = host.manager.unit_state("hostkeeper").expect("state");
    assert_eq!(state.active, ActiveState::NotFound);
    assert_eq!(state.main_pid, None);
    assert!(host.runner.calls().is_empty());
}

#[test]
fn a_dangling_symlink_is_an_init_script() {
    let host = Host::new("dangling");
    let link = host.scratch.path().join("init.d/net.eth0");
    std::os::unix::fs::symlink(host.scratch.path().join("missing-target"), &link).expect("symlink");
    host.exits(Some(3));
    assert_eq!(
        host.manager.unit_state("net.eth0"),
        Ok(UnitState {
            active: ActiveState::Inactive,
            sub: "stopped".to_owned(),
            main_pid: None,
        })
    );
    assert_eq!(
        host.runner.calls(),
        vec![(status_argv("net.eth0"), DEADLINE)]
    );
}

#[test]
fn each_exit_code_is_the_state_rc_service_reports() {
    let table: [(i32, ActiveState, &str); 7] = [
        (0, ActiveState::Active, "started"),
        (3, ActiveState::Inactive, "stopped"),
        (4, ActiveState::Deactivating, "stopping"),
        (8, ActiveState::Activating, "starting"),
        (16, ActiveState::Inactive, "inactive"),
        (32, ActiveState::Failed, "crashed"),
        (64, ActiveState::Failed, "unsupervised"),
    ];
    for (code, active, sub) in table {
        let host = Host::new("codes");
        host.init_script("hostkeeper");
        host.exits(Some(code));
        assert_eq!(
            host.manager.unit_state("hostkeeper"),
            Ok(UnitState {
                active,
                sub: sub.to_owned(),
                main_pid: None,
            }),
            "exit {code}"
        );
        assert_eq!(
            host.runner.calls(),
            vec![(status_argv("hostkeeper"), DEADLINE)],
            "exit {code}"
        );
    }
}

#[test]
fn an_unknown_exit_code_is_an_error() {
    let host = Host::new("unknown-code");
    host.init_script("hostkeeper");
    host.exits(Some(5));
    assert_eq!(
        host.manager.unit_state("hostkeeper"),
        Err(manager_error(
            "hostkeeper",
            "/sbin/rc-service hostkeeper status exited 5, which is no state".to_owned()
        ))
    );
}

#[test]
fn an_end_by_a_signal_is_an_error() {
    let host = Host::new("signal");
    host.init_script("hostkeeper");
    host.exits(None);
    assert_eq!(
        host.manager.unit_state("hostkeeper"),
        Err(manager_error(
            "hostkeeper",
            "/sbin/rc-service hostkeeper status ended by a signal".to_owned()
        ))
    );
}

#[test]
fn a_command_that_could_not_run_is_an_error() {
    let host = Host::new("runner-error");
    host.init_script("hostkeeper");
    host.runner.then(Err(RunError::Failed {
        program: "/sbin/rc-service".to_owned(),
        why: "No such file or directory".to_owned(),
    }));
    assert_eq!(
        host.manager.unit_state("hostkeeper"),
        Err(manager_error(
            "hostkeeper",
            "cannot run /sbin/rc-service: No such file or directory".to_owned()
        ))
    );
}

#[test]
fn a_command_that_timed_out_is_an_error() {
    let host = Host::new("timed-out");
    host.init_script("hostkeeper");
    host.runner.then(Err(RunError::TimedOut {
        program: "/sbin/rc-service".to_owned(),
    }));
    assert_eq!(
        host.manager.unit_state("hostkeeper"),
        Err(manager_error(
            "hostkeeper",
            "/sbin/rc-service did not finish within its deadline and was killed".to_owned()
        ))
    );
}

#[test]
fn a_started_service_has_the_pid_in_its_child_pid_file() {
    let host = Host::new("pid");
    host.init_script("hostkeeper");
    host.child_pid_file("hostkeeper", "123\n");
    host.exits(Some(0));
    assert_eq!(
        host.manager.unit_state("hostkeeper"),
        Ok(UnitState {
            active: ActiveState::Active,
            sub: "started".to_owned(),
            main_pid: Some(123),
        })
    );
}

#[test]
fn a_child_pid_file_with_no_line_feed_is_a_pid() {
    let host = Host::new("pid-bare");
    host.init_script("hostkeeper");
    host.child_pid_file("hostkeeper", "4567");
    host.exits(Some(0));
    assert_eq!(
        host.manager
            .unit_state("hostkeeper")
            .expect("state")
            .main_pid,
        Some(4567)
    );
}

#[test]
fn a_started_service_without_a_child_pid_file_has_no_main_pid() {
    let host = Host::new("pid-absent");
    host.init_script("hostkeeper");
    host.exits(Some(0));
    assert_eq!(
        host.manager.unit_state("hostkeeper"),
        Ok(UnitState {
            active: ActiveState::Active,
            sub: "started".to_owned(),
            main_pid: None,
        })
    );
}

#[test]
fn a_started_service_with_an_options_directory_but_no_file_has_no_main_pid() {
    let host = Host::new("pid-no-file");
    host.init_script("hostkeeper");
    host.options_dir("hostkeeper");
    host.exits(Some(0));
    assert_eq!(
        host.manager
            .unit_state("hostkeeper")
            .expect("state")
            .main_pid,
        None
    );
}

#[test]
fn a_child_pid_file_that_holds_no_pid_is_an_error_naming_the_file() {
    for text in ["0", "", "abc", "12 \n"] {
        let host = Host::new("pid-bad");
        host.init_script("hostkeeper");
        let path = host.child_pid_file("hostkeeper", text);
        host.exits(Some(0));
        assert_eq!(
            host.manager.unit_state("hostkeeper"),
            Err(manager_error(
                "hostkeeper",
                format!("{} holds no pid: {text:?}", path.display())
            )),
            "{text:?}"
        );
    }
}

#[test]
fn an_unreadable_child_pid_is_an_error_naming_the_file() {
    let host = Host::new("pid-dir");
    host.init_script("hostkeeper");
    let path = host.options_dir("hostkeeper").join(CHILD_PID);
    fs::create_dir(&path).expect("a directory in the file's place");
    let cause = fs::read_to_string(&path).expect_err("a directory is not readable as text");
    host.exits(Some(0));
    assert_eq!(
        host.manager.unit_state("hostkeeper"),
        Err(manager_error(
            "hostkeeper",
            format!("{}: {cause}", path.display())
        ))
    );
}

#[test]
fn the_child_pid_is_not_read_unless_the_service_is_started() {
    // A file that would be an error if it were read, for every state that is not Active.
    for code in [3, 4, 8, 16, 32, 64] {
        let host = Host::new("pid-not-active");
        host.init_script("hostkeeper");
        host.child_pid_file("hostkeeper", "not a pid");
        host.exits(Some(code));
        let state = host.manager.unit_state("hostkeeper").expect("state");
        assert_eq!(state.main_pid, None, "exit {code}");
        assert_ne!(state.active, ActiveState::Active, "exit {code}");
    }
}

#[test]
fn each_service_reads_its_own_child_pid() {
    let host = Host::new("pid-per-service");
    host.init_script("a");
    host.init_script("b");
    host.child_pid_file("a", "11\n");
    host.child_pid_file("b", "22\n");
    host.exits(Some(0));
    host.exits(Some(0));
    assert_eq!(host.manager.unit_state("b").expect("b").main_pid, Some(22));
    assert_eq!(host.manager.unit_state("a").expect("a").main_pid, Some(11));
    assert_eq!(
        host.runner.calls(),
        vec![(status_argv("b"), DEADLINE), (status_argv("a"), DEADLINE)]
    );
}

#[test]
fn a_name_that_is_no_service_name_is_an_error_before_the_disk_or_a_command() {
    let host = Host::new("bad-name");
    // A file the name would reach if it were joined to the init directory.
    fs::write(host.scratch.path().join("outside"), "").expect("file");
    for unit in ["", "-s", ".hidden", "../outside", "a/b", "caf\u{e9}", "a@b"] {
        assert_eq!(
            host.manager.unit_state(unit),
            Err(manager_error(
                unit,
                format!("{unit:?} is not the name of an OpenRC service")
            )),
            "{unit:?}"
        );
    }
    assert!(host.runner.calls().is_empty());
}

#[test]
fn a_name_of_255_bytes_is_looked_up_and_one_of_256_is_refused() {
    let host = Host::new("name-length");
    let longest = "a".repeat(255);
    host.init_script(&longest);
    host.exits(Some(3));
    assert_eq!(
        host.manager.unit_state(&longest).expect("state").active,
        ActiveState::Inactive
    );
    let too_long = "a".repeat(256);
    assert_eq!(
        host.manager.unit_state(&too_long),
        Err(manager_error(
            &too_long,
            format!("{too_long:?} is not the name of an OpenRC service")
        ))
    );
    assert_eq!(host.runner.calls().len(), 1);
}
