//! The Remote Login probe a Mac's daemon runs at its start ([`hostkeeper_daemon::remote_login_probe`]):
//! the report of its sixteen steps, from scripted programs, a loopback listener that plays sshd and
//! files in a scratch directory; the cut of long text; the banner read against a listener that
//! closes or stays silent; the start on a thread of its own, on a Mac alone.

use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use hostkeeper_daemon::mac::CODESIGN;
use hostkeeper_daemon::remote_login_probe::{
    ACCESS_SSH_GROUP, BANNER_DEADLINE, DSCL, FULL_DISK_ACCESS_QUERY, INSTALLED_BINARY, Places,
    ProbeSlot, SQLITE3, SSHD_CONFIG, SSHD_PLIST, SSHD_SERVICE, SYSTEMSETUP, TCC_DB, Tools,
    at_host_start, cut, run, start,
};
use hostkeeper_launchd::{FakeLaunchctl, LAUNCHCTL, LaunchctlError, LaunchctlOutput, PlatformKind};
use hostkeeper_link::testing::FakeClock;
use hostkeeper_link::{ProbeStep, RemoteLoginProbe, RemoteLoginProbeSource};
use hostkeeper_proto::MAX_PROBE_TEXT;
use hostkeeper_testkit::Scratch;

/// What `systemsetup -setremotelogin on` says on a Mac whose caller has no Full Disk Access: the
/// message Apple documents for it.
const FDA_REQUIRED: &str = "Turning Remote Login on or off requires Full Disk Access privileges.\n";

/// What `launchctl print` says for a service that is not loaded, as the Mac prints it.
const NOT_LOADED: &str = "Could not find service \"com.openssh.sshd\" in domain for system\n";

/// What `dscl` says for a record that does not exist.
const NO_RECORD: &str = "<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)\n";

/// What `launchctl print` says once sshd is bootstrapped.
const SSHD_LOADED: &str = "system/com.openssh.sshd = {\n\tstate = running\n\tprogram = /usr/libexec/sshd-keygen-wrapper\n}\n";

/// The designated requirement `codesign -dr -` prints for the installed binary.
const REQUIREMENT: &str = "designated => cdhash H\"0123456789abcdef0123456789abcdef01234567\"\n";

/// The banner sshd writes first.
const BANNER: &str = "SSH-2.0-OpenSSH_9.8\r\n";

/// The probe starts at this unix time.
const STARTED: u64 = 1_700_000_000;

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The five programs of a probe, none with an answer yet; each handle shares its script with the
/// fake inside `tools`.
struct Fakes {
    systemsetup: FakeLaunchctl,
    launchctl: FakeLaunchctl,
    dscl: FakeLaunchctl,
    sqlite3: FakeLaunchctl,
    codesign: FakeLaunchctl,
}

impl Fakes {
    fn new() -> Fakes {
        Fakes {
            systemsetup: FakeLaunchctl::new(),
            launchctl: FakeLaunchctl::new(),
            dscl: FakeLaunchctl::new(),
            sqlite3: FakeLaunchctl::new(),
            codesign: FakeLaunchctl::new(),
        }
    }

    fn tools(&self) -> Tools {
        Tools {
            systemsetup: Box::new(self.systemsetup.clone()),
            launchctl: Box::new(self.launchctl.clone()),
            dscl: Box::new(self.dscl.clone()),
            sqlite3: Box::new(self.sqlite3.clone()),
            codesign: Box::new(self.codesign.clone()),
        }
    }
}

/// What a listener on loopback does with the two connections the probe makes to it.
enum Server {
    /// Writes the banner, then closes.
    Banner,
    /// Closes at once, writing nothing.
    Closing,
    /// Holds the connections open, writing nothing, until the sender of the channel is dropped or
    /// sends.
    Silent(Receiver<()>),
}

/// A listener on `127.0.0.1` that serves two connections as `kind` says: its address, and the
/// thread that ends after the second.
fn listen(kind: Server) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
    let addr = listener.local_addr().expect("its address");
    let thread = thread::spawn(move || {
        let mut held = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("a connection");
            match &kind {
                Server::Banner => {
                    stream.write_all(BANNER.as_bytes()).expect("the banner");
                }
                Server::Closing => {}
                Server::Silent(_) => held.push(stream),
            }
        }
        if let Server::Silent(release) = &kind {
            let _ = release.recv();
        }
    });
    (addr, thread)
}

/// An address nothing listens on: a loopback port that was bound, then released.
fn refused() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
    listener.local_addr().expect("its address")
}

/// The places of a probe under `dir`, sshd at `ssh`.
fn places(dir: &Path, ssh: SocketAddr) -> Places {
    Places {
        ssh,
        banner_deadline: std::time::Duration::from_secs(2),
        sshd_config: dir.join("sshd_config"),
        tcc_db: dir.join("TCC.db"),
        running: dir.join("running-hostkeeper"),
        installed: dir.join("installed-hostkeeper"),
    }
}

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch dir")
}

fn clock() -> FakeClock {
    FakeClock::new(STARTED, 5)
}

/// A step of `name` that ran `program` with `args`, nothing else known.
fn command(name: &str, program: &str, args: &[&str]) -> ProbeStep {
    let mut argv = vec![program.to_owned()];
    argv.extend(strings(args));
    ProbeStep {
        name: name.to_owned(),
        argv,
        ..ProbeStep::default()
    }
}

/// A step of `name` that read `what` at `path`.
fn reading(name: &str, what: &str, path: &Path) -> ProbeStep {
    ProbeStep {
        name: name.to_owned(),
        argv: vec![what.to_owned(), path.display().to_string()],
        ..ProbeStep::default()
    }
}

/// The step of the banner read at `ssh`.
fn connecting(name: &str, ssh: SocketAddr) -> ProbeStep {
    ProbeStep {
        name: name.to_owned(),
        argv: vec!["connect".to_owned(), ssh.to_string()],
        ..ProbeStep::default()
    }
}

/// `step` ended with `code`, `stdout` and `stderr`.
fn ended(step: ProbeStep, code: i32, stdout: &str, stderr: &str) -> ProbeStep {
    ProbeStep {
        exit_code: Some(code),
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
        ..step
    }
}

/// `step` that could not be run because nothing answered it, as a [`FakeLaunchctl`] with no script
/// says.
fn unscripted(step: ProbeStep) -> ProbeStep {
    let error = LaunchctlError::Spawn {
        args: step.argv[1..].join(" "),
        why: "the fake has no answer".to_owned(),
    }
    .to_string();
    ProbeStep { error, ..step }
}

/// Every step of the probe in its order, none with an outcome yet, for the places under `dir`.
fn skeleton(places: &Places) -> Vec<ProbeStep> {
    let tcc_db = places.tcc_db.display().to_string();
    let running = places.running.display().to_string();
    let installed = places.installed.display().to_string();
    vec![
        command("remote_login_get_before", SYSTEMSETUP, &["-getremotelogin"]),
        command("sshd_print_before", LAUNCHCTL, &["print", SSHD_SERVICE]),
        connecting("ssh_banner_before", places.ssh),
        command(
            "remote_login_set_on",
            SYSTEMSETUP,
            &["-setremotelogin", "on"],
        ),
        command(
            "remote_login_get_after_set",
            SYSTEMSETUP,
            &["-getremotelogin"],
        ),
        command("sshd_enable", LAUNCHCTL, &["enable", SSHD_SERVICE]),
        command(
            "sshd_bootstrap",
            LAUNCHCTL,
            &["bootstrap", "system", SSHD_PLIST],
        ),
        command("sshd_print_after", LAUNCHCTL, &["print", SSHD_SERVICE]),
        command("remote_login_get_after", SYSTEMSETUP, &["-getremotelogin"]),
        connecting("ssh_banner_after", places.ssh),
        reading("sshd_config_include", "read", &places.sshd_config),
        command("access_ssh_group", DSCL, &[".", "-read", ACCESS_SSH_GROUP]),
        reading("tcc_db_open", "open", &places.tcc_db),
        command(
            "tcc_full_disk_access",
            SQLITE3,
            &["-readonly", &tcc_db, FULL_DISK_ACCESS_QUERY],
        ),
        command("codesign_running", CODESIGN, &["-dvv", &running]),
        command(
            "codesign_installed_requirement",
            CODESIGN,
            &["-dr", "-", &installed],
        ),
    ]
}

/// Proves a Mac that has no Full Disk Access gets the full report of sixteen steps, in order, each
/// with the exact exit code and text its program gave: Remote Login off before and after the set
/// that Full Disk Access refuses, sshd not loaded until the bootstrap, the banner read twice from
/// the listener, the `Include` lines of the configuration kept (a line is kept when its first seven
/// bytes are `include` in any case, so `IncludeNot` is kept too, and a line of fewer than seven
/// bytes never is), the privacy database readable and its rows empty, the signature of the running
/// binary and the requirement of the installed one.
#[test]
fn a_mac_without_full_disk_access_reports_all_sixteen_steps() {
    let dir = scratch("probe-fda");
    let (ssh, server) = listen(Server::Banner);
    let places = places(dir.path(), ssh);
    fs::write(
        &places.sshd_config,
        "# Include commented\nInclude /etc/ssh/sshd_config.d/*\n  INCLUDE /x/y\nIncludeNot z\nIncl\n\
         PasswordAuthentication no\n",
    )
    .expect("an sshd_config");
    fs::write(&places.tcc_db, "").expect("a privacy database");

    let fakes = Fakes::new();
    let get = strings(&["-getremotelogin"]);
    fakes
        .systemsetup
        .then(&get, Ok(LaunchctlOutput::success("Remote Login: Off\n")));
    fakes
        .systemsetup
        .then(&get, Ok(LaunchctlOutput::success("Remote Login: Off\n")));
    fakes
        .systemsetup
        .then(&get, Ok(LaunchctlOutput::success("Remote Login: On\n")));
    fakes.systemsetup.always(
        &strings(&["-setremotelogin", "on"]),
        Ok(LaunchctlOutput::exit(1, FDA_REQUIRED)),
    );
    let print = strings(&["print", SSHD_SERVICE]);
    fakes
        .launchctl
        .then(&print, Ok(LaunchctlOutput::exit(113, NOT_LOADED)));
    fakes
        .launchctl
        .then(&print, Ok(LaunchctlOutput::success(SSHD_LOADED)));
    fakes.launchctl.always(
        &strings(&["enable", SSHD_SERVICE]),
        Ok(LaunchctlOutput::success("")),
    );
    fakes.launchctl.always(
        &strings(&["bootstrap", "system", SSHD_PLIST]),
        Ok(LaunchctlOutput::success("")),
    );
    fakes.dscl.always(
        &strings(&[".", "-read", ACCESS_SSH_GROUP]),
        Ok(LaunchctlOutput::exit(56, NO_RECORD)),
    );
    fakes.sqlite3.always(
        &[
            "-readonly".to_owned(),
            places.tcc_db.display().to_string(),
            FULL_DISK_ACCESS_QUERY.to_owned(),
        ],
        Ok(LaunchctlOutput::success("")),
    );
    fakes.codesign.always(
        &["-dvv".to_owned(), places.running.display().to_string()],
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: Vec::new(),
            stderr: b"Identifier=hostkeeper\nSignature=adhoc\n".to_vec(),
        }),
    );
    fakes.codesign.always(
        &[
            "-dr".to_owned(),
            "-".to_owned(),
            places.installed.display().to_string(),
        ],
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: REQUIREMENT.as_bytes().to_vec(),
            stderr: Vec::new(),
        }),
    );

    let probe = run(&fakes.tools(), &places, &clock());
    server.join().expect("the listener ended");

    let mut steps = skeleton(&places).into_iter();
    let mut next = || steps.next().expect("a step");
    let expected = vec![
        ended(next(), 0, "Remote Login: Off\n", ""),
        ended(next(), 113, "", NOT_LOADED),
        ProbeStep {
            stdout: BANNER.to_owned(),
            ..next()
        },
        ended(next(), 1, "", FDA_REQUIRED),
        ended(next(), 0, "Remote Login: Off\n", ""),
        ended(next(), 0, "", ""),
        ended(next(), 0, "", ""),
        ended(next(), 0, SSHD_LOADED, ""),
        ended(next(), 0, "Remote Login: On\n", ""),
        ProbeStep {
            stdout: BANNER.to_owned(),
            ..next()
        },
        ProbeStep {
            stdout: "Include /etc/ssh/sshd_config.d/*\nINCLUDE /x/y\nIncludeNot z".to_owned(),
            ..next()
        },
        ended(next(), 56, "", NO_RECORD),
        ProbeStep {
            stdout: "readable".to_owned(),
            ..next()
        },
        ended(next(), 0, "", ""),
        ended(next(), 0, "", "Identifier=hostkeeper\nSignature=adhoc\n"),
        ended(next(), 0, REQUIREMENT, ""),
    ];
    assert_eq!(
        probe,
        RemoteLoginProbe {
            started_unix: STARTED,
            steps: expected,
        }
    );
    // Each program was asked exactly what its steps say, in order.
    assert_eq!(
        fakes.systemsetup.calls(),
        vec![
            get.clone(),
            strings(&["-setremotelogin", "on"]),
            get.clone(),
            get,
        ]
    );
    assert_eq!(
        fakes.launchctl.calls(),
        vec![
            print.clone(),
            strings(&["enable", SSHD_SERVICE]),
            strings(&["bootstrap", "system", SSHD_PLIST]),
            print,
        ]
    );
}

/// Proves a host where no program can be started reports each command step with no exit code, no
/// text, and the error the program's port gave; the steps that read the machine itself (the
/// configuration, the privacy database, the banner) still run and say why they could not: the
/// operating system's own error for the two files, a refused connect for the banner.
#[test]
fn programs_that_cannot_run_are_reported_by_their_errors_and_the_files_are_still_read() {
    let dir = scratch("probe-unscripted");
    let ssh = refused();
    let places = places(dir.path(), ssh);
    let fakes = Fakes::new();

    let mut probe = run(&fakes.tools(), &places, &clock());

    let config_error = fs::read_to_string(&places.sshd_config)
        .expect_err("no sshd_config")
        .to_string();
    let tcc_error = fs::File::open(&places.tcc_db)
        .expect_err("no privacy database")
        .to_string();
    for step in &mut probe.steps {
        if step.name.starts_with("ssh_banner") {
            assert_ne!(
                step.error, "",
                "{}: a refused connect has a reason",
                step.name
            );
            assert_eq!(step.stdout, "");
            step.error.clear();
        }
    }
    let expected: Vec<ProbeStep> = skeleton(&places)
        .into_iter()
        .map(|step| match step.name.as_str() {
            "ssh_banner_before" | "ssh_banner_after" => step,
            "sshd_config_include" => ProbeStep {
                error: config_error.clone(),
                ..step
            },
            "tcc_db_open" => ProbeStep {
                error: tcc_error.clone(),
                ..step
            },
            _ => unscripted(step),
        })
        .collect();
    assert_eq!(
        probe,
        RemoteLoginProbe {
            started_unix: STARTED,
            steps: expected,
        }
    );
    assert_eq!(
        probe.steps[0].error,
        "cannot run launchctl -getremotelogin: the fake has no answer"
    );
    assert_eq!(fakes.codesign.calls().len(), 2);
    assert_eq!(fakes.dscl.calls().len(), 1);
    assert_eq!(fakes.sqlite3.calls().len(), 1);
    assert_eq!(fakes.systemsetup.calls().len(), 4);
    assert_eq!(fakes.launchctl.calls().len(), 4);
}

/// Proves a command that timed out reports the port's own timeout text as its error, with no exit
/// code, and the other steps go on.
#[test]
fn a_command_killed_at_its_deadline_is_reported_with_the_deadline_text() {
    let dir = scratch("probe-deadline");
    let places = places(dir.path(), refused());
    let fakes = Fakes::new();
    let timed_out = LaunchctlError::TimedOut {
        args: "-getremotelogin".to_owned(),
    };
    fakes
        .systemsetup
        .always(&strings(&["-getremotelogin"]), Err(timed_out));
    let probe = run(&fakes.tools(), &places, &clock());
    assert_eq!(
        probe.steps[0],
        ProbeStep {
            error: "launchctl -getremotelogin did not finish within its deadline and was killed"
                .to_owned(),
            ..command("remote_login_get_before", SYSTEMSETUP, &["-getremotelogin"])
        }
    );
    assert_eq!(probe.steps.len(), 16);
}

/// Proves the cut of a text: exactly the limit is kept whole and not marked, one byte more is cut
/// to the limit and marked, and a two-byte character that straddles the limit is dropped whole, so
/// the result is one byte short of the limit and valid text.
#[test]
fn a_text_is_cut_at_the_limit_on_a_character_boundary() {
    let exact = "a".repeat(MAX_PROBE_TEXT);
    assert_eq!(cut(&exact), (exact.clone(), false));
    assert_eq!(cut(""), (String::new(), false));

    let over = "a".repeat(MAX_PROBE_TEXT + 1);
    assert_eq!(cut(&over), (exact, true));

    let straddling = format!("{}é", "a".repeat(MAX_PROBE_TEXT - 1));
    assert_eq!(straddling.len(), MAX_PROBE_TEXT + 1);
    let (kept, was_cut) = cut(&straddling);
    assert_eq!(kept, "a".repeat(MAX_PROBE_TEXT - 1));
    assert_eq!(kept.len(), MAX_PROBE_TEXT - 1);
    assert!(was_cut);
}

/// Proves the text of a command is cut to the limit and the step says so: standard output over the
/// limit alone marks the step, standard error over the limit alone does, a text of exactly the
/// limit does not, and bytes that are not UTF-8 become U+FFFD.
#[test]
fn a_long_output_is_cut_and_marks_its_step_truncated() {
    let dir = scratch("probe-cut");
    let places = places(dir.path(), refused());
    let fakes = Fakes::new();
    fakes.systemsetup.always(
        &strings(&["-getremotelogin"]),
        Ok(LaunchctlOutput::success(&"o".repeat(2000))),
    );
    fakes.dscl.always(
        &strings(&[".", "-read", ACCESS_SSH_GROUP]),
        Ok(LaunchctlOutput::exit(1, &"e".repeat(MAX_PROBE_TEXT + 1))),
    );
    fakes.launchctl.always(
        &strings(&["print", SSHD_SERVICE]),
        Ok(LaunchctlOutput::success(&"x".repeat(MAX_PROBE_TEXT))),
    );
    fakes.launchctl.always(
        &strings(&["enable", SSHD_SERVICE]),
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: vec![b'o', 0xFF, b'k'],
            stderr: Vec::new(),
        }),
    );
    let probe = run(&fakes.tools(), &places, &clock());
    let step = |name: &str| {
        probe
            .steps
            .iter()
            .find(|step| step.name == name)
            .expect("a step")
            .clone()
    };

    let long_out = step("remote_login_get_before");
    assert_eq!(long_out.stdout, "o".repeat(MAX_PROBE_TEXT));
    assert_eq!(long_out.stderr, "");
    assert!(long_out.truncated);

    let long_err = step("access_ssh_group");
    assert_eq!(long_err.exit_code, Some(1));
    assert_eq!(long_err.stdout, "");
    assert_eq!(long_err.stderr, "e".repeat(MAX_PROBE_TEXT));
    assert!(long_err.truncated);

    let exact = step("sshd_print_before");
    assert_eq!(exact.stdout, "x".repeat(MAX_PROBE_TEXT));
    assert!(!exact.truncated);

    let lossy = step("sshd_enable");
    assert_eq!(lossy.stdout, "o\u{FFFD}k");
    assert!(!lossy.truncated);
}

/// Proves the `Include` lines of a configuration longer than the limit are cut and the step is
/// marked truncated.
#[test]
fn a_long_list_of_include_lines_is_cut() {
    let dir = scratch("probe-include-cut");
    let places = places(dir.path(), refused());
    let line = format!("Include /etc/ssh/{}\n", "d".repeat(100));
    fs::write(&places.sshd_config, line.repeat(20)).expect("an sshd_config");
    let probe = run(&Fakes::new().tools(), &places, &clock());
    let step = &probe.steps[10];
    assert_eq!(step.name, "sshd_config_include");
    assert_eq!(step.stdout.len(), MAX_PROBE_TEXT);
    assert!(step.truncated);
    assert_eq!(step.error, "");
}

/// Proves a listener that accepts and closes at once gives a banner step with empty text and no
/// error: the read ended with nothing, which is not a failure.
#[test]
fn a_listener_that_closes_at_once_gives_an_empty_banner_and_no_error() {
    let dir = scratch("probe-closing");
    let (ssh, server) = listen(Server::Closing);
    let places = places(dir.path(), ssh);
    let probe = run(&Fakes::new().tools(), &places, &clock());
    server.join().expect("the listener ended");
    for index in [2, 9] {
        let step = &probe.steps[index];
        assert_eq!(step.argv, vec!["connect".to_owned(), ssh.to_string()]);
        assert_eq!(step.stdout, "");
        assert_eq!(step.error, "");
        assert_eq!(step.exit_code, None);
    }
}

/// Proves a listener that accepts and never writes gives, once the read deadline has passed, a
/// banner step with an error and empty text: the probe does not wait for sshd for ever.
#[test]
fn a_listener_that_never_writes_gives_a_banner_error() {
    let dir = scratch("probe-silent");
    let (release, held) = mpsc::channel();
    let (ssh, server) = listen(Server::Silent(held));
    let mut places = places(dir.path(), ssh);
    places.banner_deadline = std::time::Duration::from_millis(50);
    let probe = run(&Fakes::new().tools(), &places, &clock());
    drop(release);
    server.join().expect("the listener ended");
    for index in [2, 9] {
        let step = &probe.steps[index];
        assert_eq!(step.stdout, "");
        assert_ne!(step.error, "");
    }
}

/// Proves a host that is not a Mac starts no probe and never calls it, and a Mac starts it on a
/// thread of its own: the slot is empty until the probe is done, then holds exactly what it
/// returned, for every read after.
#[test]
fn the_probe_starts_on_a_mac_alone_and_its_slot_fills_when_it_is_done() {
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let seen = called.clone();
        let started = start(kind, move || {
            seen.store(true, Ordering::SeqCst);
            RemoteLoginProbe::default()
        });
        assert!(started.is_none());
        assert!(!called.load(Ordering::SeqCst));
    }

    let probe = RemoteLoginProbe {
        started_unix: STARTED,
        steps: vec![ProbeStep {
            name: "remote_login_get_before".to_owned(),
            exit_code: Some(0),
            stdout: "Remote Login: Off\n".to_owned(),
            ..ProbeStep::default()
        }],
    };
    let (go, wait) = mpsc::channel::<()>();
    let result = probe.clone();
    let (slot, handle) = start(PlatformKind::Macos, move || {
        let _ = wait.recv();
        result
    })
    .expect("a Mac starts the probe");
    assert_eq!(slot.remote_login_probe(), None);
    go.send(()).expect("the probe waits");
    handle.join().expect("the probe thread ended");
    assert_eq!(slot.remote_login_probe(), Some(probe.clone()));
    assert_eq!(slot.remote_login_probe(), Some(probe));
    assert_eq!(ProbeSlot::default().remote_login_probe(), None);
}

/// Proves the start on the host's own programs runs nothing on a host that is not a Mac: it gives
/// no source for the heartbeats.
#[test]
fn the_host_start_gives_no_source_on_a_host_that_is_not_a_mac() {
    assert!(at_host_start(PlatformKind::Linux).is_none());
    assert!(at_host_start(PlatformKind::Openrc).is_none());
}

/// Proves the host's places are the documented ones: sshd on `127.0.0.1:22` with the banner
/// deadline, the system's `sshd_config` and privacy database, the installed binary's path, and the
/// running binary that of this process.
#[test]
fn the_host_places_are_the_documented_ones() {
    let places = Places::host();
    assert_eq!(
        places.ssh,
        "127.0.0.1:22".parse::<SocketAddr>().expect("an address")
    );
    assert_eq!(places.banner_deadline, BANNER_DEADLINE);
    assert_eq!(places.sshd_config, Path::new(SSHD_CONFIG));
    assert_eq!(places.tcc_db, Path::new(TCC_DB));
    assert_eq!(places.installed, Path::new(INSTALLED_BINARY));
    assert_eq!(
        places.running,
        std::env::current_exe().expect("the test binary's path")
    );
    assert_ne!(places.running.as_os_str(), "");
}
