//! The tests of `ssh_host`: the real host over a scripted process port and scratch directories
//! (no real `systemctl`, `sshd` or `/etc`), and the banner read from a listener on the loopback.

use std::io::Write;
use std::net::TcpListener;
use std::sync::Mutex;
use std::thread;

use hostkeeper_observe::{CommandOutput, RunError};
use hostkeeper_testkit::Scratch;

use super::*;

type Answer = Box<dyn Fn(&[String]) -> Result<CommandOutput, RunError> + Send + Sync>;

/// A process port that answers by a function of the argv and records every argv.
struct Runner {
    calls: Mutex<Vec<Vec<String>>>,
    answer: Answer,
}

impl Runner {
    fn new(answer: impl Fn(&[String]) -> Result<CommandOutput, RunError> + Send + Sync + 'static) -> Arc<Runner> {
        Arc::new(Runner {
            calls: Mutex::new(Vec::new()),
            answer: Box::new(answer),
        })
    }

    /// Every run, each argv as one line with the program's file name only.
    fn lines(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("the calls")
            .iter()
            .map(|argv| {
                let program = Path::new(&argv[0]).file_name().expect("a program").to_string_lossy().into_owned();
                std::iter::once(program).chain(argv[1..].iter().cloned()).collect::<Vec<_>>().join(" ")
            })
            .collect()
    }
}

impl CommandRunner for Runner {
    fn run(&self, argv: &[String], _deadline: Duration) -> Result<CommandOutput, RunError> {
        self.calls.lock().expect("the calls").push(argv.to_vec());
        (self.answer)(argv)
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

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn host(runner: &Arc<Runner>, etc: &Path) -> SystemHost {
    SystemHost::new(
        Arc::clone(runner) as Arc<dyn CommandRunner>,
        Tools {
            systemctl: PathBuf::from("/x/systemctl"),
            sshd: PathBuf::from("/x/sshd"),
            ssh_keygen: PathBuf::from("/x/ssh-keygen"),
            id: PathBuf::from("/x/id"),
            etc: etc.to_owned(),
            work: etc.join("krl-work"),
        },
    )
}

/// What `systemctl show` prints of a unit: its load state, active state and unit file state.
fn shown(load: &str, active: &str, file: &str) -> Result<CommandOutput, RunError> {
    ok(&format!("LoadState={load}\nActiveState={active}\nUnitFileState={file}\n"))
}

/// A manager whose units are `(name, load, active, file)`; any other unit is not found.
fn manager(units: &'static [(&'static str, &'static str, &'static str, &'static str)]) -> Arc<Runner> {
    Runner::new(move |argv| {
        if argv[1] != "show" {
            return ok("");
        }
        let name = &argv[4];
        units
            .iter()
            .find(|unit| unit.0 == name)
            .map_or_else(|| shown("not-found", "inactive", ""), |u| shown(u.1, u.2, u.3))
    })
}

/// Proves an active socket unit is the one that listens, found with one question and nothing
/// changed, whatever service units exist.
#[test]
fn an_active_socket_is_the_unit_that_listens() {
    let runner = manager(&[
        ("ssh.socket", "loaded", "active", "enabled"),
        ("ssh.service", "loaded", "inactive", "disabled"),
    ]);
    let host = host(&runner, Path::new("/nowhere"));
    assert_eq!(host.unit(), Ok(SshdUnit::Socket));
    assert_eq!(
        runner.lines(),
        ["systemctl show -p LoadState,ActiveState,UnitFileState ssh.socket"]
    );
}

/// Proves with no active socket the first loaded of `ssh.service` and `sshd.service` is the unit,
/// whether it runs or not.
#[test]
fn the_first_loaded_service_is_the_unit() {
    let ssh = manager(&[
        ("ssh.socket", "loaded", "inactive", "disabled"),
        ("ssh.service", "loaded", "inactive", "disabled"),
        ("sshd.service", "loaded", "active", "enabled"),
    ]);
    assert_eq!(
        host(&ssh, Path::new("/nowhere")).unit(),
        Ok(SshdUnit::Service("ssh.service".to_owned()))
    );
    let sshd = manager(&[("sshd.service", "loaded", "active", "enabled")]);
    assert_eq!(
        host(&sshd, Path::new("/nowhere")).unit(),
        Ok(SshdUnit::Service("sshd.service".to_owned()))
    );
    assert_eq!(sshd.lines().len(), 3);
}

/// Proves a host with no unit of sshd is `Missing`, one whose unit is masked is `Masked` (the
/// daemon does not unmask), and one whose service manager cannot be asked, by its exit or by
/// failing to run, is `NoManager`.
#[test]
fn a_missing_masked_or_unaskable_unit_is_told_apart() {
    let none = manager(&[]);
    assert_eq!(host(&none, Path::new("/nowhere")).unit(), Err(UnitError::Missing));
    let masked = manager(&[("ssh.service", "masked", "inactive", "masked")]);
    assert_eq!(host(&masked, Path::new("/nowhere")).unit(), Err(UnitError::Masked));
    let failing = Runner::new(|_| out(Some(1), ""));
    assert_eq!(host(&failing, Path::new("/nowhere")).unit(), Err(UnitError::NoManager));
    let absent = Runner::new(|_| Err(RunError::Empty));
    assert_eq!(host(&absent, Path::new("/nowhere")).unit(), Err(UnitError::NoManager));
    let late = Runner::new(|argv| {
        if argv[4] == "ssh.socket" {
            shown("loaded", "inactive", "disabled")
        } else {
            out(Some(1), "")
        }
    });
    assert_eq!(host(&late, Path::new("/nowhere")).unit(), Err(UnitError::NoManager));
}

/// Proves starting a disabled, stopped service enables it and starts it, in that order; an
/// enabled running one is left alone; a socket is left alone; and nothing is ever stopped.
#[test]
fn start_enables_and_starts_and_stops_nothing() {
    let down = manager(&[("ssh.service", "loaded", "inactive", "disabled")]);
    let unit = SshdUnit::Service("ssh.service".to_owned());
    host(&down, Path::new("/nowhere")).start(&unit).expect("started");
    assert_eq!(
        down.lines(),
        [
            "systemctl show -p LoadState,ActiveState,UnitFileState ssh.service",
            "systemctl enable ssh.service",
            "systemctl start ssh.service"
        ]
    );
    let up = manager(&[("ssh.service", "loaded", "active", "enabled")]);
    host(&up, Path::new("/nowhere")).start(&unit).expect("left");
    assert_eq!(up.lines().len(), 1);
    let socket = manager(&[]);
    host(&socket, Path::new("/nowhere")).start(&SshdUnit::Socket).expect("left");
    assert!(socket.lines().is_empty());
    let all = down.lines().concat() + &up.lines().concat();
    assert!(!all.contains("stop") && !all.contains("restart") && !all.contains("disable"));
}

/// Proves a start that cannot ask the manager, enable the unit or start it is an error that says
/// which program failed.
#[test]
fn a_start_that_fails_says_what_failed() {
    let unit = SshdUnit::Service("ssh.service".to_owned());
    let blind = Runner::new(|_| out(Some(1), ""));
    assert_eq!(
        host(&blind, Path::new("/nowhere")).start(&unit),
        Err("systemctl failed".to_owned())
    );
    let no_enable = Runner::new(|argv| if argv[1] == "show" { shown("loaded", "inactive", "disabled") } else { out(Some(1), "") });
    assert_eq!(
        host(&no_enable, Path::new("/nowhere")).start(&unit),
        Err("systemctl ended with exit 1".to_owned())
    );
    let no_start = Runner::new(|argv| match argv[1].as_str() {
        "show" => shown("loaded", "inactive", "enabled"),
        _ => out(None, ""),
    });
    assert_eq!(
        host(&no_start, Path::new("/nowhere")).start(&unit),
        Err("systemctl ended with a signal".to_owned())
    );
    let unrunnable = Runner::new(|argv| if argv[1] == "show" { shown("loaded", "inactive", "enabled") } else { Err(RunError::Empty) });
    assert_eq!(
        host(&unrunnable, Path::new("/nowhere")).start(&unit),
        Err("systemctl could not be run".to_owned())
    );
}

/// Proves a reload of a service asks the manager to reload it (never a restart), a socket has
/// none, and a reload that fails is an error.
#[test]
fn a_reload_is_a_reload() {
    let runner = Runner::new(|_| ok(""));
    let unit = SshdUnit::Service("sshd.service".to_owned());
    host(&runner, Path::new("/nowhere")).reload(&unit).expect("reloaded");
    host(&runner, Path::new("/nowhere")).reload(&SshdUnit::Socket).expect("none");
    assert_eq!(runner.lines(), ["systemctl reload sshd.service"]);
    let failing = Runner::new(|_| out(Some(1), ""));
    assert!(host(&failing, Path::new("/nowhere")).reload(&unit).is_err());
}

/// Proves `sshd -t -f` is run over the candidate and a failing exit is an error.
#[test]
fn the_candidate_is_checked_with_sshd_t() {
    let runner = Runner::new(|_| ok(""));
    host(&runner, Path::new("/nowhere")).check_config(Path::new("/etc/ssh/hostkeeper/candidate.conf")).expect("valid");
    assert_eq!(runner.lines(), ["sshd -t -f /etc/ssh/hostkeeper/candidate.conf"]);
    let bad = Runner::new(|_| out(Some(255), ""));
    assert_eq!(
        host(&bad, Path::new("/nowhere")).check_config(Path::new("/c")),
        Err("sshd ended with exit 255".to_owned())
    );
}

/// Proves the effective configuration is asked for a login of the user from the loopback and is
/// parsed, and an sshd that fails gives no configuration; groups are the words of `id -Gn`, and
/// none when it fails.
#[test]
fn the_effective_configuration_and_groups_are_read_from_sshd_and_id() {
    let runner = Runner::new(|argv| {
        if argv[0].ends_with("sshd") {
            ok("port 22\npubkeyauthentication yes\n")
        } else {
            ok("root wheel ops\n")
        }
    });
    let host = host(&runner, Path::new("/nowhere"));
    assert_eq!(host.effective("root").expect("read").first("pubkeyauthentication"), Some("yes"));
    assert_eq!(host.groups("root"), ["root", "wheel", "ops"]);
    assert_eq!(
        runner.lines(),
        ["sshd -T -C user=root,host=localhost,addr=127.0.0.1", "id -Gn root"]
    );
    let failing = Runner::new(|_| out(Some(1), ""));
    let host = super::SystemHost::new(failing, Tools::linux());
    assert!(host.effective("root").is_err());
    assert!(host.groups("root").is_empty());
}

/// Proves the banner check passes on an SSH banner and fails on another banner, an end of stream,
/// a refused connection and an address that is not an IP address.
#[test]
fn the_banner_check_reads_ssh_from_the_listening_address() {
    let runner = Runner::new(|_| ok(""));
    let host = host(&runner, Path::new("/nowhere"));
    let serve = |bytes: &'static [u8]| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let port = listener.local_addr().expect("an address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a connection");
            let _ = stream.write_all(bytes);
        });
        (port, server)
    };
    let (port, server) = serve(b"SSH-2.0-OpenSSH_9.6\r\n");
    host.banner(&("127.0.0.1".to_owned(), port)).expect("a banner");
    server.join().expect("served");
    let (port, server) = serve(b"HTTP/1.1 400\r\n");
    assert_eq!(
        host.banner(&("127.0.0.1".to_owned(), port)),
        Err("the banner is not SSH".to_owned())
    );
    server.join().expect("served");
    let (port, server) = serve(b"");
    assert!(host.banner(&("127.0.0.1".to_owned(), port)).expect_err("no banner").starts_with("no banner"));
    server.join().expect("served");
    let closed = TcpListener::bind("127.0.0.1:0").expect("a listener");
    let port = closed.local_addr().expect("an address").port();
    drop(closed);
    assert!(host.banner(&("127.0.0.1".to_owned(), port)).expect_err("refused").starts_with("no connection"));
    assert_eq!(
        host.banner(&("localhost".to_owned(), 22)),
        Err("the listen address is not an IP address".to_owned())
    );
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
    fs::write(path, text).expect("a file");
}

/// Proves an account matters when its home has a non-empty `authorized_keys` or it has a login
/// shell and a usable password hash: a locked, starred, empty or missing hash and a nologin or
/// false shell are no password path; a line that is not a passwd entry is skipped.
#[test]
fn the_accounts_with_a_key_path_or_a_password_path_are_listed() {
    let dir = scratch("ssh-accounts");
    let etc = dir.path();
    let home = |name: &str| etc.join("home").join(name);
    write(&home("keyed").join(".ssh/authorized_keys"), "ssh-ed25519 AAAA\n");
    write(&home("empty").join(".ssh/authorized_keys"), "");
    write(
        &etc.join("passwd"),
        &format!(
            "root:x:0:0:root:{r}:/bin/bash\n\
             keyed:x:1000:1000::{k}:/usr/sbin/nologin\n\
             empty:x:1001:1001::{e}:/bin/false\n\
             pw:x:1002:1002::/nonexistent:/bin/bash\n\
             locked:x:1003:1003::/nonexistent:/bin/bash\n\
             starred:x:1004:1004::/nonexistent:/bin/bash\n\
             blank:x:1005:1005::/nonexistent:/bin/bash\n\
             noshell:x:1006:1006::/nonexistent:\n\
             absent:x:1007:1007::/nonexistent:/bin/sh\n\
             :x:1008:1008::/nonexistent:/bin/sh\n\
             short:x:1\n\
             noshellfield:x:1:1::/h\n",
            r = home("root").display(),
            k = home("keyed").display(),
            e = home("empty").display(),
        ),
    );
    write(
        &etc.join("shadow"),
        "root:$6$abc:19000:0:99999:7:::\npw:$y$def:1::::::\nlocked:!$6$x:1::::::\nstarred:*:1::::::\nblank::1::::::\nkeyed:!:1::::::\n",
    );
    let runner = Runner::new(|_| ok(""));
    let accounts = host(&runner, etc).accounts();
    let held: Vec<(&str, bool, bool)> = accounts.iter().map(|a| (a.name.as_str(), a.keyed, a.password)).collect();
    assert_eq!(held, [("root", false, true), ("keyed", true, false), ("pw", false, true)]);
}

/// Proves that when the shadow file cannot be read an account with a login shell is taken to have
/// a password, and with no passwd file there is no account.
#[test]
fn an_unreadable_shadow_file_is_taken_for_passwords() {
    let dir = scratch("ssh-accounts-blind");
    write(&dir.path().join("passwd"), "pw:x:1002:1002::/nonexistent:/bin/bash\nsvc:x:5:5::/nonexistent:/sbin/nologin\n");
    let runner = Runner::new(|_| ok(""));
    let accounts = host(&runner, dir.path()).accounts();
    assert_eq!(accounts.len(), 1);
    assert!(accounts[0].password && !accounts[0].keyed);
    let empty = scratch("ssh-accounts-none");
    assert!(host(&runner, empty.path()).accounts().is_empty());
}

/// Proves the host key fingerprints are those `ssh-keygen -lf` prints of each `ssh_host_*_key.pub`
/// in name order, a key it cannot read or prints no `SHA256:` for is left out, and a host with no
/// `ssh` directory has none.
#[test]
fn the_host_key_fingerprints_are_read_with_ssh_keygen() {
    let dir = scratch("ssh-fingerprints");
    let ssh = dir.path().join("ssh");
    for name in ["ssh_host_rsa_key.pub", "ssh_host_ed25519_key.pub", "ssh_host_ecdsa_key.pub", "ssh_host_dsa_key.pub", "ssh_host_ed25519_key", "moduli"] {
        write(&ssh.join(name), "x");
    }
    let runner = Runner::new(|argv| {
        let key = &argv[2];
        if key.ends_with("ed25519_key.pub") {
            ok("256 SHA256:eee root@h (ED25519)\n")
        } else if key.ends_with("rsa_key.pub") {
            ok("3072 SHA256:rrr root@h (RSA)\n")
        } else if key.ends_with("ecdsa_key.pub") {
            ok("256 MD5:aa:bb root@h (ECDSA)\n")
        } else {
            out(Some(1), "")
        }
    });
    assert_eq!(host(&runner, dir.path()).host_key_fingerprints(), ["SHA256:eee", "SHA256:rrr"]);
    let none = scratch("ssh-fingerprints-none");
    assert!(host(&runner, none.path()).host_key_fingerprints().is_empty());
}

/// A process port that plays `ssh-keygen -k`: it writes the list the `-f` argument names and keeps
/// what it was asked.
fn keygen(fail_at: Option<usize>) -> Arc<Runner> {
    let runs = Mutex::new(0_usize);
    Runner::new(move |argv| {
        let mut runs = runs.lock().expect("the runs");
        *runs += 1;
        if fail_at == Some(*runs) {
            return out(Some(1), "");
        }
        let at = argv.iter().position(|arg| arg == "-f").expect("-f");
        let spec = fs::read_to_string(&argv[argv.len() - 1]).expect("the spec");
        let ca = fs::read_to_string(&argv[at + 3]).expect("the ca key");
        fs::write(&argv[at + 1], format!("krl run {runs}\n{spec}{ca}")).expect("the list");
        ok("")
    })
}

/// Proves no revoked serial is an empty list with no program run; serials are written one per line
/// to the spec, each CA key signs a run of its own (the later runs update the list), the work
/// directory is removed after, and the bytes returned are the list `ssh-keygen` made.
#[test]
fn revoked_serials_are_made_into_a_list_with_ssh_keygen() {
    let dir = scratch("ssh-krl");
    let runner = keygen(None);
    let host = host(&runner, dir.path());
    assert_eq!(host.revocations(&["ssh-ed25519 AAAA".to_owned()], &[]), Ok(Vec::new()));
    assert!(runner.lines().is_empty());
    let made = host
        .revocations(&["ssh-ed25519 AAAA".to_owned(), "ssh-ed25519 BBBB".to_owned()], &[3, 9])
        .expect("a list");
    assert_eq!(
        String::from_utf8(made).expect("text"),
        "krl run 2\nserial: 3\nserial: 9\nssh-ed25519 BBBB\n"
    );
    let lines = runner.lines();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("ssh-keygen -k -f ") && !lines[0].contains(" -u "), "{}", lines[0]);
    assert!(lines[1].starts_with("ssh-keygen -k -u -f "), "{}", lines[1]);
    assert!(!dir.path().join("krl-work").exists());
}

/// Proves a failing `ssh-keygen`, a work directory that cannot be made and a list it never wrote
/// are errors, with the work directory removed after.
#[test]
fn a_list_that_cannot_be_made_is_an_error() {
    let dir = scratch("ssh-krl-fail");
    let ca = ["ssh-ed25519 AAAA".to_owned()];
    let failing = keygen(Some(1));
    assert_eq!(
        host(&failing, dir.path()).revocations(&ca, &[1]),
        Err("ssh-keygen ended with exit 1".to_owned())
    );
    assert!(!dir.path().join("krl-work").exists());

    let silent = Runner::new(|_| ok(""));
    assert!(host(&silent, dir.path()).revocations(&ca, &[1]).expect_err("no list").starts_with("the revocation list"));

    let blocked = scratch("ssh-krl-blocked");
    fs::write(blocked.path().join("krl-work"), "a file").expect("a file");
    assert!(host(&silent, blocked.path()).revocations(&ca, &[1]).expect_err("no directory").starts_with("the work directory"));
}

/// Proves the real host's places are those of a Linux host.
#[test]
fn the_linux_tools_are_the_distros_places() {
    let tools = Tools::linux();
    assert_eq!(tools.sshd, Path::new("/usr/sbin/sshd"));
    assert_eq!(tools.systemctl, Path::new("/usr/bin/systemctl"));
    assert_eq!(tools.etc, Path::new("/etc"));
    assert!(tools.work.starts_with("/etc/ssh/hostkeeper"));
}
