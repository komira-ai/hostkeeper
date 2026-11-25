//! The tests of `serial_baseline`: the real baseline over a scripted process port (a `systemctl`
//! and an `update-grub` that keep their own state) and scratch directories (no real `/etc`,
//! `/boot` or `/proc`), looked at on a clock a test sets.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use hostkeeper_link::testing::RecordingLog;
use hostkeeper_observe::{CommandOutput, RunError};
use hostkeeper_testkit::Scratch;

use super::*;

/// The scripted `systemctl` and `update-grub`: the getty's enabled and active state, which
/// command fails, and whether `update-grub` makes the menu hold the argument.
struct Sim {
    enabled: AtomicBool,
    active: AtomicBool,
    fail: Mutex<&'static str>,
    menu_made: AtomicBool,
    /// Whether systemd says the getty's unit files changed since it last read them.
    stale: AtomicBool,
    menu: PathBuf,
    menu_text: String,
    calls: Mutex<Vec<String>>,
}

impl Sim {
    /// Every run, as the program's file name and its arguments on one line.
    fn lines(&self) -> Vec<String> {
        self.calls.lock().expect("the calls").clone()
    }

    fn answer(&self, argv: &[String]) -> Result<CommandOutput, RunError> {
        let program = Path::new(&argv[0])
            .file_name()
            .expect("a program")
            .to_string_lossy()
            .into_owned();
        let line = std::iter::once(program.clone())
            .chain(argv[1..].iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        self.calls.lock().expect("the calls").push(line);
        let word = if program == "update-grub" {
            "update-grub"
        } else {
            argv[1].as_str()
        };
        let fail = *self.fail.lock().expect("fail");
        if fail == word {
            return out(Some(1), "");
        }
        match word {
            "update-grub" => {
                if self.menu_made.load(Ordering::SeqCst) {
                    fs::write(&self.menu, &self.menu_text).expect("the menu");
                }
                ok("")
            }
            "show" if fail == "blank-show" => ok(""),
            "show" => ok(&format!(
                "junk line\nActiveState={}\nUnitFileState={}\nNeedDaemonReload={}\n",
                if self.active.load(Ordering::SeqCst) {
                    "active"
                } else {
                    "inactive"
                },
                if self.enabled.load(Ordering::SeqCst) {
                    "enabled"
                } else {
                    "disabled"
                },
                if self.stale.load(Ordering::SeqCst) {
                    "yes"
                } else {
                    "no"
                },
            )),
            "daemon-reload" => {
                self.stale.store(false, Ordering::SeqCst);
                ok("")
            }
            "enable" => {
                self.enabled.store(true, Ordering::SeqCst);
                ok("")
            }
            "start" | "restart" => {
                self.active.store(true, Ordering::SeqCst);
                ok("")
            }
            _ => ok(""),
        }
    }
}

struct Runner(Arc<Sim>);

impl CommandRunner for Runner {
    fn run(&self, argv: &[String], _deadline: Duration) -> Result<CommandOutput, RunError> {
        self.0.answer(argv)
    }
}

fn out(code: Option<i32>, text: &str) -> Result<CommandOutput, RunError> {
    Ok(CommandOutput {
        exit_code: code,
        stdout: text.as_bytes().to_vec(),
        truncated: false,
    })
}

fn ok(text: &str) -> Result<CommandOutput, RunError> {
    out(Some(0), text)
}

/// A unix clock a test sets.
#[derive(Default)]
struct Clock(AtomicU64);

impl UnixClock for Clock {
    fn unix_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A host in scratch: no baseline file, a getty that is disabled and stopped, an `update-grub`
/// that makes the menu, and a running kernel whose command line holds the port's argument.
struct Host {
    _scratch: Scratch,
    root: PathBuf,
    port: SerialConsole,
    paths: BaselinePaths,
    tools: BaselineTools,
    sim: Arc<Sim>,
    clock: Arc<Clock>,
    log: Arc<RecordingLog>,
    baseline: SerialBaseline,
}

impl Host {
    fn new(name: &str, port: &str) -> Host {
        let scratch = scratch(name);
        let root = scratch.path().to_owned();
        let port = SerialConsole::parse(port).expect("a port");
        let paths = BaselinePaths {
            grub_file: root.join("etc/default/grub.d/99_hostkeeper.cfg"),
            getty_drop_in: root.join("etc/systemd/system/getty.d/99_hostkeeper.conf"),
            grub_cfg: root.join("boot/grub/grub.cfg"),
            cmdline: root.join("proc-cmdline"),
        };
        let tools = BaselineTools {
            systemctl: root.join("systemctl"),
            update_grub: root.join("update-grub"),
        };
        fs::write(&tools.update_grub, "").expect("the update-grub file");
        fs::create_dir_all(root.join("boot/grub")).expect("the boot directory");
        fs::write(
            &paths.cmdline,
            format!("BOOT_IMAGE=/vmlinuz ro {}\n", port.kernel_argument()),
        )
        .expect("the command line");
        let sim = Arc::new(Sim {
            enabled: AtomicBool::new(false),
            active: AtomicBool::new(false),
            fail: Mutex::new(""),
            menu_made: AtomicBool::new(true),
            stale: AtomicBool::new(false),
            menu: paths.grub_cfg.clone(),
            menu_text: format!(
                "linux /vmlinuz console=tty0 {} ro\n",
                port.kernel_argument()
            ),
            calls: Mutex::new(Vec::new()),
        });
        let clock = Arc::new(Clock::default());
        let log = Arc::new(RecordingLog::default());
        let baseline = Host::baseline_of(&port, &paths, &tools, &sim, &clock, &log);
        Host {
            _scratch: scratch,
            root,
            port,
            paths,
            tools,
            sim,
            clock,
            log,
            baseline,
        }
    }

    fn baseline_of(
        port: &SerialConsole,
        paths: &BaselinePaths,
        tools: &BaselineTools,
        sim: &Arc<Sim>,
        clock: &Arc<Clock>,
        log: &Arc<RecordingLog>,
    ) -> SerialBaseline {
        SerialBaseline::new(
            port.clone(),
            paths.clone(),
            tools.clone(),
            Arc::new(Runner(Arc::clone(sim))),
            rustix::process::geteuid().as_raw(),
            Arc::clone(clock) as Arc<dyn UnixClock>,
            Arc::clone(log) as Arc<dyn Log>,
        )
    }

    /// Puts the whole baseline in place: both files, the menu, an enabled and running getty.
    fn in_place(&self) {
        for (path, text) in [
            (&self.paths.grub_file, self.port.grub_defaults()),
            (&self.paths.getty_drop_in, GETTY_DROP_IN.to_owned()),
            (
                &self.paths.grub_cfg,
                format!("linux /vmlinuz {}\n", self.port.kernel_argument()),
            ),
        ] {
            fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
            fs::write(path, text).expect("a file");
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("a mode");
        }
        self.sim.enabled.store(true, Ordering::SeqCst);
        self.sim.active.store(true, Ordering::SeqCst);
    }

    fn fail(&self, word: &'static str) {
        *self.sim.fail.lock().expect("fail") = word;
    }

    /// The state and detail of one look at `now`.
    fn look(&self, now: u64) -> (String, String) {
        self.clock.0.store(now, Ordering::SeqCst);
        let report = self.baseline.report().expect("a report");
        assert_eq!(report.port, self.port.as_str());
        (report.state, report.detail)
    }

    fn lines(&self) -> Vec<String> {
        self.sim.lines()
    }

    fn written(&self, path: &Path) -> bool {
        path.exists()
    }
}

fn pair(state: &str, detail: &str) -> (String, String) {
    (state.to_owned(), detail.to_owned())
}

const UNIT: &str = "serial-getty@ttyS1.service";

fn show() -> String {
    format!("systemctl show -p ActiveState,UnitFileState,NeedDaemonReload {UNIT}")
}

/// Proves a fresh host gets both files with exactly the bytes the config crate defines, mode 0644,
/// and is `applied` when the running kernel's console is already on the port.
#[test]
fn a_fresh_host_gets_the_two_files_with_the_exact_bytes() {
    let host = Host::new("serial-fresh", "ttyS1");
    assert_eq!(host.look(1000), pair("applied", ""));
    let grub = fs::read_to_string(&host.paths.grub_file).expect("the grub file");
    assert_eq!(grub, host.port.grub_defaults());
    assert_eq!(
        grub,
        "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 console=ttyS1,115200n8\"\n\
         GRUB_TERMINAL=\"console serial\"\n\
         GRUB_SERIAL_COMMAND=\"serial --unit=1 --speed=115200\"\n"
    );
    assert_eq!(
        fs::read_to_string(&host.paths.getty_drop_in).expect("the drop-in"),
        GETTY_DROP_IN
    );
    for path in [&host.paths.grub_file, &host.paths.getty_drop_in] {
        let meta = fs::metadata(path).expect("metadata");
        assert_eq!(meta.mode() & 0o7777, 0o644);
        assert_eq!(meta.uid(), rustix::process::geteuid().as_raw());
    }
}

/// Proves one full fresh look runs update-grub, reloads, enables and starts in that order, and
/// logs exactly those steps and the state.
#[test]
fn a_fresh_look_runs_its_steps_in_order_and_logs_them() {
    let host = Host::new("serial-order", "ttyS1");
    assert_eq!(host.look(1000), pair("applied", ""));
    assert_eq!(
        host.lines(),
        [
            "update-grub".to_owned(),
            show(),
            "systemctl daemon-reload".to_owned(),
            format!("systemctl enable {UNIT}"),
            format!("systemctl start {UNIT}"),
        ]
    );
    assert_eq!(
        host.log.lines(),
        [
            format!(
                "serial_console ttyS1: wrote {}",
                host.paths.grub_file.display()
            ),
            "serial_console ttyS1: ran update-grub".to_owned(),
            format!(
                "serial_console ttyS1: wrote {}",
                host.paths.getty_drop_in.display()
            ),
            format!("serial_console ttyS1: enabled {UNIT}"),
            format!("serial_console ttyS1: started {UNIT}"),
            "serial_console ttyS1: applied".to_owned(),
        ]
    );
}

/// Proves a second look after the recheck interval rewrites nothing (the files keep their inodes),
/// runs only the read-only `systemctl show`, and logs nothing new for an unchanged state.
#[test]
fn a_second_look_writes_nothing_and_only_shows() {
    let host = Host::new("serial-idempotent", "ttyS1");
    assert_eq!(host.look(1000), pair("applied", ""));
    let inodes = |host: &Host| {
        [&host.paths.grub_file, &host.paths.getty_drop_in]
            .map(|p| fs::metadata(p).expect("meta").ino())
    };
    let before = inodes(&host);
    let runs = host.lines().len();
    let logged = host.log.lines();
    assert_eq!(host.look(1000 + RECHECK_SECS), pair("applied", ""));
    assert_eq!(inodes(&host), before);
    assert_eq!(host.lines()[runs..], [show()]);
    assert_eq!(host.log.lines(), logged);
}

/// Proves a host whose baseline is already present is left alone: no write, no update-grub, no
/// daemon-reload, enable, start or restart; one `systemctl show` only.
#[test]
fn a_host_with_the_baseline_present_is_left_alone() {
    let host = Host::new("serial-present", "ttyS1");
    host.in_place();
    let inode = fs::metadata(&host.paths.grub_file).expect("meta").ino();
    assert_eq!(host.look(5), pair("applied", ""));
    assert_eq!(host.lines(), [show()]);
    assert_eq!(
        fs::metadata(&host.paths.grub_file).expect("meta").ino(),
        inode
    );
    assert_eq!(host.log.lines(), ["serial_console ttyS1: applied"]);
}

/// Proves the state is `reboot_pending` while the running kernel's command line lacks the port's
/// argument, and `applied` once it holds it.
#[test]
fn the_state_follows_the_running_kernels_command_line() {
    let host = Host::new("serial-cmdline", "ttyS1");
    fs::write(&host.paths.cmdline, "BOOT_IMAGE=/vmlinuz ro console=tty0\n").expect("cmdline");
    assert_eq!(host.look(0), pair("reboot_pending", ""));
    fs::write(&host.paths.cmdline, "ro console=ttyS1,115200n8\n").expect("cmdline");
    assert_eq!(host.look(RECHECK_SECS), pair("applied", ""));
    assert_eq!(
        host.log.lines().last().map(String::as_str),
        Some("serial_console ttyS1: applied")
    );
}

/// Proves a running getty is restarted when its drop-in was written, and left running when it was
/// not.
#[test]
fn a_running_getty_is_restarted_only_when_its_drop_in_was_written() {
    let host = Host::new("serial-restart", "ttyS1");
    host.in_place();
    fs::write(&host.paths.getty_drop_in, "old\n").expect("old drop-in");
    assert_eq!(host.look(0), pair("applied", ""));
    assert_eq!(
        host.lines(),
        [
            show(),
            "systemctl daemon-reload".to_owned(),
            format!("systemctl restart {UNIT}"),
        ]
    );
    assert!(
        host.log
            .lines()
            .contains(&format!("serial_console ttyS1: restarted {UNIT}"))
    );
}

/// Proves a stopped getty is started and a disabled one enabled when nothing was written.
#[test]
fn a_stopped_getty_is_started_and_a_disabled_one_enabled() {
    let host = Host::new("serial-start", "ttyS1");
    host.in_place();
    host.sim.active.store(false, Ordering::SeqCst);
    host.sim.enabled.store(false, Ordering::SeqCst);
    assert_eq!(host.look(0), pair("applied", ""));
    assert_eq!(
        host.lines(),
        [
            show(),
            format!("systemctl enable {UNIT}"),
            format!("systemctl start {UNIT}")
        ]
    );
}

/// Proves a show that prints nothing reads as disabled and stopped, so the getty is enabled and
/// started.
#[test]
fn a_show_without_the_keys_reads_as_disabled_and_stopped() {
    let host = Host::new("serial-blank", "ttyS1");
    host.in_place();
    host.fail("blank-show");
    assert_eq!(host.look(0), pair("applied", ""));
    assert_eq!(
        host.lines(),
        [
            show(),
            format!("systemctl enable {UNIT}"),
            format!("systemctl start {UNIT}")
        ]
    );
}

/// Proves a restart that failed is owed to the next look: the drop-in is not written again, yet
/// the running getty is restarted then, and the look is applied only after it.
///
/// Catches a getty left on the stock command (and a port that falls to 9600 baud) after a failed
/// restart, reported applied because the second look found its drop-in in place.
#[test]
fn a_failed_restart_is_owed_to_the_next_look() {
    let host = Host::new("serial-owed-restart", "ttyS1");
    host.in_place();
    fs::write(&host.paths.getty_drop_in, "old\n").expect("old drop-in");
    host.fail("restart");
    assert_eq!(host.look(0), pair("failed", "getty_start"));
    host.fail("");
    assert_eq!(host.look(RECHECK_SECS), pair("applied", ""));
    assert_eq!(
        host.lines()[3..],
        [show(), format!("systemctl restart {UNIT}")]
    );
    assert_eq!(host.look(2 * RECHECK_SECS), pair("applied", ""));
    assert_eq!(host.lines()[5..], [show()]);
}

/// Proves a drop-in systemd has not read (`NeedDaemonReload=yes`: its reload failed, or the daemon
/// stopped between the write and the reload) is read and the running getty restarted, although
/// the drop-in holds the baseline's bytes and is not written again.
///
/// Catches a getty still on its old command after a failed daemon-reload or a daemon restart,
/// reported applied.
#[test]
fn a_drop_in_systemd_has_not_read_is_reloaded_and_the_getty_restarted() {
    let host = Host::new("serial-owed-reload", "ttyS1");
    host.in_place();
    fs::write(&host.paths.getty_drop_in, "old\n").expect("old drop-in");
    host.fail("daemon-reload");
    assert_eq!(host.look(0), pair("failed", "daemon_reload"));
    host.fail("");
    host.sim.stale.store(true, Ordering::SeqCst);
    let fresh = Host::baseline_of(
        &host.port,
        &host.paths,
        &host.tools,
        &host.sim,
        &host.clock,
        &host.log,
    );
    let report = fresh.report().expect("a report");
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("applied", "")
    );
    assert_eq!(
        host.lines()[2..],
        [
            show(),
            "systemctl daemon-reload".to_owned(),
            format!("systemctl restart {UNIT}"),
        ]
    );
    assert!(!host.sim.stale.load(Ordering::SeqCst));
}

/// Proves update-grub runs when the menu lacks the argument although the GRUB file is unchanged
/// (a repair), and not otherwise.
#[test]
fn update_grub_runs_when_the_menu_lacks_the_argument() {
    let host = Host::new("serial-repair", "ttyS1");
    host.in_place();
    fs::write(
        &host.paths.grub_cfg,
        "linux /vmlinuz console=ttyS10,115200n8\n",
    )
    .expect("menu");
    assert_eq!(host.look(0), pair("applied", ""));
    assert_eq!(host.lines(), ["update-grub".to_owned(), show()]);
    assert!(
        host.log
            .lines()
            .contains(&"serial_console ttyS1: ran update-grub".to_owned())
    );
}

/// Proves a look is not repeated before the recheck interval, and is repeated when the clock goes
/// back.
#[test]
fn a_look_waits_for_the_recheck_interval_and_repeats_when_the_clock_goes_back() {
    let host = Host::new("serial-clock", "ttyS1");
    host.in_place();
    host.look(1000);
    assert_eq!(host.lines().len(), 1);
    host.look(1000 + RECHECK_SECS - 1);
    assert_eq!(host.lines().len(), 1);
    host.look(1000 + RECHECK_SECS);
    assert_eq!(host.lines().len(), 2);
    host.look(500);
    assert_eq!(host.lines().len(), 3);
}

/// Proves a GRUB file edited away is put back at the next due look, with update-grub.
#[test]
fn a_drifted_grub_file_is_put_back_with_update_grub() {
    let host = Host::new("serial-drift", "ttyS1");
    host.in_place();
    host.look(0);
    fs::write(&host.paths.grub_file, "GRUB_TIMEOUT=0\n").expect("an edit");
    assert_eq!(host.look(RECHECK_SECS), pair("applied", ""));
    assert_eq!(
        fs::read_to_string(&host.paths.grub_file).expect("grub"),
        host.port.grub_defaults()
    );
    assert!(host.lines().contains(&"update-grub".to_owned()));
}

/// Proves a `ttyAMA0` port gets the kernel line alone, with no GRUB terminal.
#[test]
fn a_ttyama_port_writes_the_kernel_line_alone() {
    let host = Host::new("serial-ama", "ttyAMA0");
    fs::write(
        &host.paths.grub_cfg,
        "linux /vmlinuz console=ttyAMA0,115200n8\n",
    )
    .expect("menu");
    assert_eq!(host.look(0), pair("applied", ""));
    assert_eq!(
        fs::read_to_string(&host.paths.grub_file).expect("grub"),
        "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 console=ttyAMA0,115200n8\"\n"
    );
}

/// Proves a host with no `update-grub` is `unsupported` (`update_grub`) and nothing is written or
/// run.
#[test]
fn a_host_without_update_grub_is_unsupported_and_untouched() {
    let mut host = Host::new("serial-no-grub", "ttyS1");
    fs::remove_file(&host.tools.update_grub).expect("remove");
    host.baseline = Host::baseline_of(
        &host.port,
        &host.paths,
        &host.tools,
        &host.sim,
        &host.clock,
        &host.log,
    );
    assert_eq!(host.look(0), pair("unsupported", "update_grub"));
    assert!(host.lines().is_empty());
    assert!(!host.written(&host.paths.grub_file));
    assert!(!host.written(&host.paths.getty_drop_in));
    assert_eq!(
        host.log.lines(),
        ["serial_console ttyS1: unsupported (update_grub)"]
    );
}

/// Proves a host marked unsupported for its service manager runs nothing and writes nothing.
#[test]
fn an_unsupported_service_manager_runs_and_writes_nothing() {
    let mut host = Host::new("serial-manager", "ttyS1");
    host.baseline = Host::baseline_of(
        &host.port,
        &host.paths,
        &host.tools,
        &host.sim,
        &host.clock,
        &host.log,
    )
    .unsupported("service_manager");
    assert_eq!(host.look(0), pair("unsupported", "service_manager"));
    assert!(host.lines().is_empty());
    assert!(!host.written(&host.paths.grub_file));
}

/// Proves a GRUB file path that is a directory fails the look at `grub_file`, logged as an error
/// once however many looks repeat it.
#[test]
fn a_grub_file_that_is_a_directory_fails_at_grub_file() {
    let host = Host::new("serial-grub-dir", "ttyS1");
    fs::create_dir_all(&host.paths.grub_file).expect("a directory");
    assert_eq!(host.look(0), pair("failed", "grub_file"));
    assert_eq!(host.look(RECHECK_SECS), pair("failed", "grub_file"));
    assert_eq!(
        host.log.lines(),
        ["error: serial_console ttyS1: failed (grub_file)"]
    );
}

/// Proves a getty drop-in that cannot be written (its directory's parent is a file) fails at
/// `getty_drop_in`.
#[test]
fn an_unwritable_drop_in_fails_at_getty_drop_in() {
    let host = Host::new("serial-dropin", "ttyS1");
    let parent = host.paths.getty_drop_in.parent().expect("a parent");
    fs::create_dir_all(parent.parent().expect("a grandparent")).expect("a directory");
    fs::write(parent, "").expect("a file in the way");
    assert_eq!(host.look(0), pair("failed", "getty_drop_in"));
}

/// Proves a failing update-grub fails the look at `update_grub`.
#[test]
fn an_update_grub_that_exits_1_fails_at_update_grub() {
    let host = Host::new("serial-grub-fails", "ttyS1");
    host.fail("update-grub");
    assert_eq!(host.look(0), pair("failed", "update_grub"));
}

/// Proves a menu that still lacks the argument after update-grub fails the look at `grub_cfg`.
#[test]
fn a_menu_still_lacking_the_argument_fails_at_grub_cfg() {
    let host = Host::new("serial-menu", "ttyS1");
    host.sim.menu_made.store(false, Ordering::SeqCst);
    assert_eq!(host.look(0), pair("failed", "grub_cfg"));
}

/// Proves each systemctl failure names its step: daemon-reload, show, enable, start and restart.
#[test]
fn each_systemctl_failure_names_its_step() {
    for (word, detail) in [
        ("daemon-reload", "daemon_reload"),
        ("show", "getty"),
        ("enable", "getty_enable"),
        ("start", "getty_start"),
    ] {
        let host = Host::new(&format!("serial-fail-{word}"), "ttyS1");
        host.fail(word);
        assert_eq!(host.look(0), pair("failed", detail), "{word}");
    }
    let host = Host::new("serial-fail-restart", "ttyS1");
    host.in_place();
    fs::write(&host.paths.getty_drop_in, "old\n").expect("old drop-in");
    host.fail("restart");
    assert_eq!(host.look(0), pair("failed", "getty_start"));
}

/// Proves an unreadable command line fails the look at `cmdline`.
#[test]
fn an_unreadable_command_line_fails_at_cmdline() {
    let host = Host::new("serial-no-cmdline", "ttyS1");
    fs::remove_file(&host.paths.cmdline).expect("remove");
    assert_eq!(host.look(0), pair("failed", "cmdline"));
}

/// Proves a lock poisoned by a panic in another thread does not stop the look.
#[test]
fn a_poisoned_lock_does_not_stop_the_look() {
    let host = Host::new("serial-poison", "ttyS1");
    host.in_place();
    let _ = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _held = host.baseline.last.lock().expect("the lock");
                panic!("poisoning the lock on purpose");
            })
            .join()
    });
    assert_eq!(host.look(0), pair("applied", ""));
}

/// Proves the state names are the closed set of the wire.
#[test]
fn the_state_names_are_the_wire_names() {
    assert_eq!(
        [
            BaselineState::Applied,
            BaselineState::RebootPending,
            BaselineState::Failed,
            BaselineState::Unsupported
        ]
        .map(BaselineState::as_str),
        ["applied", "reboot_pending", "failed", "unsupported"]
    );
}

/// Proves the Linux places and tools are the host's real ones.
#[test]
fn the_linux_places_are_the_hosts() {
    let port = SerialConsole::parse("ttyS1").expect("a port");
    let paths = BaselinePaths::linux(&port);
    assert_eq!(paths.grub_file, Path::new(GRUB_CONSOLE_FILE));
    assert_eq!(paths.getty_drop_in, Path::new(&port.getty_drop_in_path()));
    assert_eq!(paths.grub_cfg, Path::new("/boot/grub/grub.cfg"));
    assert_eq!(paths.cmdline, Path::new("/proc/cmdline"));
    let tools = BaselineTools::linux();
    assert_eq!(tools.systemctl, Path::new("/usr/bin/systemctl"));
    assert_eq!(tools.update_grub, Path::new("/usr/sbin/update-grub"));
}

/// Proves `at_host` gives no source without a port, and a source for each platform with one.
#[test]
fn at_host_reports_only_with_a_port() {
    let clock = Arc::new(Clock::default());
    let log = Arc::new(RecordingLog::default());
    let port = SerialConsole::parse("ttyS1").expect("a port");
    for kind in [
        PlatformKind::Linux,
        PlatformKind::Openrc,
        PlatformKind::Macos,
    ] {
        assert!(
            at_host(
                kind,
                None,
                Arc::clone(&clock) as Arc<dyn UnixClock>,
                Arc::clone(&log) as Arc<dyn Log>
            )
            .is_none()
        );
        assert!(
            at_host(
                kind,
                Some(&port),
                Arc::clone(&clock) as Arc<dyn UnixClock>,
                Arc::clone(&log) as Arc<dyn Log>
            )
            .is_some()
        );
    }
}

/// Proves under OpenRC and on a Mac the report is `unsupported` (`service_manager`), through the
/// baseline `at_host` builds with the unsupported mark.
#[test]
fn openrc_and_macos_report_service_manager_unsupported() {
    let host = Host::new("serial-openrc", "ttyS1");
    for kind in [PlatformKind::Openrc, PlatformKind::Macos] {
        let source = at_host(
            kind,
            Some(&host.port),
            Arc::clone(&host.clock) as Arc<dyn UnixClock>,
            Arc::clone(&host.log) as Arc<dyn Log>,
        )
        .expect("a source");
        assert_eq!(
            source.report(),
            Some(SerialConsoleReport {
                port: "ttyS1".to_owned(),
                state: "unsupported".to_owned(),
                detail: "service_manager".to_owned(),
            })
        );
    }
    let _ = &host.root;
}
