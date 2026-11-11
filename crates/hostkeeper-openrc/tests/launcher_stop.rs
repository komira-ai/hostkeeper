//! `StepLauncher::stop` of the OpenRC launcher against real child processes: a stop of a step this
//! launcher runs, from another thread, and a stop of a step an earlier daemon left running, found
//! by the file it kept. Time is a clock that moves when it is read.
#![cfg(target_os = "linux")]

mod launcher_support;

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use hostkeeper_steps::{Bound, LaunchError, StepExit, StepLauncher};
use launcher_support::{
    Rig, UNIT, gone, pid_in, proc_stat, stat_line, step_call, wait_for, wait_gone,
};

/// The exit of a step that failed with no code and printed nothing.
fn failed() -> StepExit {
    StepExit {
        code: None,
        last_line: None,
    }
}

/// Starts `script` as a process group leader of its own, as a daemon that crashed left a step.
fn orphan(script: &Path) -> Child {
    Command::new(script)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the script")
}

/// Writes the file a run of `unit` keeps in `rig`'s steps directory.
fn write_record(rig: &Rig, unit: &str, text: &str) {
    fs::create_dir_all(rig.steps_dir()).expect("the steps directory");
    fs::write(rig.steps_dir().join(unit), text).expect("the file");
}

/// The start time of the live process `pid`.
fn start_of(pid: i32) -> u64 {
    proc_stat(pid).expect("the process is there").1
}

fn pid_of(child: &Child) -> i32 {
    i32::try_from(child.id()).expect("a pid")
}

#[test]
fn stop_ends_the_step_it_runs_and_the_step_reports_no_code_even_if_it_exits_zero() {
    let rig = Rig::new("stop-running", 1);
    let started = rig.path("started");
    let hook = rig.script(
        "hook",
        &format!(
            "trap 'echo stopped; exit 0' TERM\n: > '{}'\nwhile :; do :; done",
            started.display()
        ),
    );
    let quick = rig.script("quick", "exit 0");
    std::thread::scope(|scope| {
        let running = scope.spawn(|| rig.launcher.run(&step_call(&hook, UNIT, Bound::Unbounded)));
        wait_for(&started);
        rig.launcher.stop(UNIT).expect("the stop");
        let got = running
            .join()
            .expect("the run thread")
            .expect("the step ran");
        // It exited 0 after its SIGTERM, and printed what it was asked to.
        assert_eq!(got.code, None);
        assert_eq!(got.last_line.as_deref(), Some("stopped"));
        assert_eq!(rig.log()[0].1.code, Some(0));
    });
    assert!(!rig.steps_dir().join(UNIT).exists());

    // The stop said nothing of a later run under the same name.
    let got = rig
        .launcher
        .run(&step_call(&quick, UNIT, Bound::Unbounded))
        .expect("the step ran");
    assert_eq!(got.code, Some(0));
}

#[test]
fn stop_sends_sigkill_to_a_step_that_ignores_sigterm_at_the_stop_timeout() {
    // The clock moves 30 seconds a read: three reads past the first are the stop timeout.
    let rig = Rig::new("stop-kill", 30);
    let started = rig.path("started");
    let hook = rig.script(
        "hook",
        &format!(
            "trap '' TERM\n: > '{}'\nwhile :; do :; done",
            started.display()
        ),
    );
    std::thread::scope(|scope| {
        let running = scope.spawn(|| rig.launcher.run(&step_call(&hook, UNIT, Bound::Unbounded)));
        wait_for(&started);
        rig.launcher.stop(UNIT).expect("the stop");
        let got = running
            .join()
            .expect("the run thread")
            .expect("the step ran");
        assert_eq!(got, failed());
    });
}

#[test]
fn stop_of_a_unit_that_is_not_running_is_done() {
    let rig = Rig::new("stop-nothing", 1);
    rig.launcher.stop(UNIT).expect("no file, no run");
    assert!(!rig.steps_dir().exists());
}

#[test]
fn stop_refuses_a_name_that_is_no_file_name() {
    let rig = Rig::new("stop-names", 1);
    for unit in ["", "a/b", "../escape", ".hidden"] {
        let err = rig.launcher.stop(unit).expect_err("a bad unit name");
        assert!(
            matches!(&err, LaunchError::Launcher(m) if m.contains("is not a unit name")),
            "{unit:?}: {err}"
        );
    }
}

#[test]
fn stop_after_a_restart_ends_the_process_the_file_names_and_what_it_started() {
    let rig = Rig::new("stop-file", 1);
    let ready = rig.path("ready");
    let started = rig.path("started");
    let grandchild = rig.path("grandchild");
    // The grandchild ignores SIGTERM, so only the kill of the group after the leader's end
    // reaches it.
    let hook = rig.script(
        "hook",
        &format!(
            "(trap '' TERM; : > '{r}'; while :; do :; done) &\necho $! > '{g}'\n\
             while [ ! -e '{r}' ]; do :; done\ntrap 'exit 0' TERM\n: > '{s}'\n\
             while :; do :; done",
            r = ready.display(),
            g = grandchild.display(),
            s = started.display()
        ),
    );
    let mut leader = orphan(&hook);
    let pid = pid_of(&leader);
    wait_for(&started);
    write_record(&rig, UNIT, &format!("{pid} {}\n", start_of(pid)));

    // A launcher that never ran it: the daemon that did is gone.
    let again = rig.launcher_over(Path::new("/proc"));
    again.stop(UNIT).expect("the stop");

    assert!(gone(pid));
    wait_gone(pid_in(&grandchild));
    assert!(!rig.steps_dir().join(UNIT).exists());
    leader.wait().expect("reap the leader");
}

#[test]
fn stop_after_a_restart_sends_sigkill_at_the_stop_timeout() {
    let rig = Rig::new("stop-file-kill", 30);
    let started = rig.path("started");
    let hook = rig.script(
        "hook",
        &format!(
            "trap '' TERM\n: > '{}'\nwhile :; do :; done",
            started.display()
        ),
    );
    let mut leader = orphan(&hook);
    let pid = pid_of(&leader);
    wait_for(&started);
    write_record(&rig, UNIT, &format!("{pid} {}\n", start_of(pid)));

    rig.launcher_over(Path::new("/proc"))
        .stop(UNIT)
        .expect("the stop");

    assert!(gone(pid));
    assert!(!rig.steps_dir().join(UNIT).exists());
    leader.wait().expect("reap the leader");
}

#[test]
fn stop_after_a_restart_leaves_a_process_that_took_the_pid_alone() {
    let rig = Rig::new("stop-reused", 1);
    let started = rig.path("started");
    let hook = rig.script(
        "hook",
        &format!(": > '{}'\nwhile :; do :; done", started.display()),
    );
    let mut stranger = orphan(&hook);
    let pid = pid_of(&stranger);
    wait_for(&started);
    // The file was written for another process, which had this pid before.
    write_record(&rig, UNIT, &format!("{pid} {}\n", start_of(pid) + 1));

    rig.launcher_over(Path::new("/proc"))
        .stop(UNIT)
        .expect("the stop");

    assert!(!gone(pid));
    assert!(!rig.steps_dir().join(UNIT).exists());
    stranger.kill().expect("kill the stranger");
    stranger.wait().expect("reap the stranger");
}

#[test]
fn stop_after_a_restart_of_a_process_that_is_gone_or_a_zombie_is_done() {
    let rig = Rig::new("stop-gone", 1);
    let quick = rig.script("quick", "exit 0");

    // A zombie: it ended, and nobody has waited for it.
    let mut zombie = orphan(&quick);
    let pid = pid_of(&zombie);
    let start = start_of(pid);
    while proc_stat(pid).is_some_and(|(state, _)| state != 'Z') {
        std::thread::yield_now();
    }
    write_record(&rig, UNIT, &format!("{pid} {start}\n"));
    rig.launcher.stop(UNIT).expect("the stop of a zombie");
    assert!(!rig.steps_dir().join(UNIT).exists());
    zombie.wait().expect("reap");

    // Gone: reaped, and not in /proc.
    write_record(&rig, UNIT, &format!("{pid} {start}\n"));
    rig.launcher
        .stop(UNIT)
        .expect("the stop of a process that is gone");
    assert!(!rig.steps_dir().join(UNIT).exists());
}

#[test]
fn stop_after_a_restart_reads_the_fake_process_table_by_its_last_parenthesis() {
    let rig = Rig::new("stop-fake-proc", 1);
    let proc_root = rig.path("proc");
    fs::create_dir_all(proc_root.join("4242")).expect("mkdir");
    let again = rig.launcher_over(&proc_root);
    let stat = proc_root.join("4242/stat");

    // A zombie, whose name has parentheses and spaces in it: nobody is signalled.
    fs::write(&stat, stat_line(4242, "a) b (c", 'Z', 100)).expect("write");
    write_record(&rig, UNIT, "4242 100\n");
    again.stop(UNIT).expect("a zombie is stopped already");
    assert!(!rig.steps_dir().join(UNIT).exists());

    // Another process with the pid.
    fs::write(&stat, stat_line(4242, "sh", 'S', 99)).expect("write");
    write_record(&rig, UNIT, "4242 100\n");
    again.stop(UNIT).expect("another process is not this step");
    assert!(!rig.steps_dir().join(UNIT).exists());
}

#[test]
fn stop_after_a_restart_with_a_process_table_it_cannot_read_is_the_launchers_failure() {
    let rig = Rig::new("stop-bad-proc", 1);
    let proc_root = rig.path("proc");
    fs::create_dir_all(proc_root.join("4242")).expect("mkdir");
    let again = rig.launcher_over(&proc_root);
    let stat = proc_root.join("4242/stat");
    write_record(&rig, UNIT, "4242 100\n");

    // A directory where the stat file is.
    fs::create_dir(&stat).expect("mkdir");
    let err = again.stop(UNIT).expect_err("an unreadable stat");
    assert!(
        matches!(&err, LaunchError::Launcher(m) if m.contains("cannot read")),
        "{err}"
    );
    fs::remove_dir(&stat).expect("rmdir");

    let eighteen = ["0"; 18].join(" ");
    for text in [
        "".to_owned(),
        "4242 no parenthesis S 0".to_owned(),
        "4242 (sh)".to_owned(),
        "4242 (sh) S 0 0".to_owned(),
        format!("4242 (sh) S {eighteen} soon"),
    ] {
        fs::write(&stat, &text).expect("write");
        let err = again.stop(UNIT).expect_err("a stat that is not one");
        assert!(
            matches!(&err, LaunchError::Launcher(m) if m.contains("is not a process's stat")),
            "{text:?}: {err}"
        );
    }
    // The file stays: the step may still be there.
    assert!(rig.steps_dir().join(UNIT).exists());
}

#[test]
fn stop_after_a_restart_with_a_file_it_cannot_use_is_the_launchers_failure() {
    let rig = Rig::new("stop-bad-file", 1);
    for text in [
        "",
        "abc 1",
        "0 1",
        "-5 1",
        "4294967295 1",
        "4242",
        "4242 x",
        "4242 1 extra",
    ] {
        write_record(&rig, UNIT, text);
        let err = rig
            .launcher
            .stop(UNIT)
            .expect_err("a file that is no record");
        assert!(
            matches!(&err, LaunchError::Launcher(m) if m.contains("does not hold a process id and a start time")),
            "{text:?}: {err}"
        );
    }
    assert!(rig.steps_dir().join(UNIT).exists());

    // A directory where the file is cannot be read.
    fs::remove_file(rig.steps_dir().join(UNIT)).expect("remove");
    fs::create_dir(rig.steps_dir().join(UNIT)).expect("mkdir");
    let err = rig.launcher.stop(UNIT).expect_err("an unreadable file");
    assert!(
        matches!(&err, LaunchError::Launcher(m) if m.contains("cannot read")),
        "{err}"
    );
}
