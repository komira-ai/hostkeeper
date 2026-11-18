#![cfg(feature = "privileged")]
//! The certificate path of "SSH through the coordinator" against a real sshd, as root
//! (`docs/design/ssh-through-coordinator.md` sections 6 and 11): the daemon's own `SshSetup` over
//! the real `SystemHost` (`Tools::linux()`, `SshPaths::linux()`) takes up an `SshAccess` made
//! here, and then the real `/usr/bin/ssh` logs in to the host's sshd on the loopback with a user
//! key and certificates signed by `ssh-keygen -s` under a CA made in the test's scratch directory.
//! The ordinary tests script the port; only a real sshd says that a certificate is accepted, or
//! refused for the reason the rules give.
//!
//! The runner's sshd (`openssh-server`, on GitHub's `ubuntu-24.04` image through the `ssh`
//! package) is the one under test, on the port its own configuration names. Every test starts by
//! taking a snapshot of `/etc/ssh` and of the state of its units, and a drop guard puts them back
//! when the test ends, on a failure too: the files of the tree as they were (bytes, modes and
//! owners), what the test added removed, sshd reloaded, a unit the test started stopped again. A
//! test that adds an account (`useradd`) removes it with its home in the same guard.
//! Certificates are made with validity dates, never waited on: an expired one is dated in the
//! past.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_daemon::ssh_files::SshPaths;
use hostkeeper_daemon::ssh_host::{SshHost, SystemHost, Tools};
use hostkeeper_daemon::ssh_setup::SshSetup;
use hostkeeper_link::{Log, PushedSsh, SshReport, SshSink};
use hostkeeper_observe::ProcessRunner;
use hostkeeper_testkit::Scratch;

/// The host id the certificates of the tests are for.
const PRINCIPAL: &str = "hk-host-privtest01";

/// The sshd of the runner.
const SSHD: &str = "/usr/sbin/sshd";

/// The account the "another account" test makes.
const ALICE: &str = "hkalice";

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

/// Refuses to run as anything but root: only root writes `/etc/ssh` and reloads sshd.
fn as_root() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "the privileged tests run as root (privileged.yml's cargo runner `sudo`)"
    );
}

/// Runs `program` and returns its standard output; a failure to run or a non-zero exit is a
/// failure of the test, with what the program said.
fn must(program: &str, args: &[&str]) -> String {
    let out = Command::new(program).args(args).output().expect("run");
    assert!(
        out.status.success(),
        "{program} {args:?}: {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What `systemctl` prints for `args`, whether or not it succeeds (`is-active` exits 3 for a
/// stopped unit); empty when it cannot be run.
fn systemctl(args: &[&str]) -> String {
    Command::new("/usr/bin/systemctl")
        .args(args)
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// One entry of the tree under `/etc/ssh`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Dir {
        mode: u32,
        uid: u32,
        gid: u32,
    },
    File {
        bytes: Vec<u8>,
        mode: u32,
        uid: u32,
        gid: u32,
    },
    Link(PathBuf),
}

/// Every entry under `root`, by path, with `root` itself.
fn tree(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut found = BTreeMap::new();
    walk(root, &mut found);
    found
}

fn walk(path: &Path, found: &mut BTreeMap<PathBuf, Entry>) {
    let meta = fs::symlink_metadata(path).expect("stat");
    let (mode, uid, gid) = (meta.mode() & 0o7777, meta.uid(), meta.gid());
    if meta.file_type().is_symlink() {
        found.insert(
            path.to_path_buf(),
            Entry::Link(fs::read_link(path).expect("link")),
        );
    } else if meta.is_dir() {
        found.insert(path.to_path_buf(), Entry::Dir { mode, uid, gid });
        for entry in fs::read_dir(path).expect("list") {
            walk(&entry.expect("entry").path(), found);
        }
    } else {
        let bytes = fs::read(path).expect("read");
        found.insert(
            path.to_path_buf(),
            Entry::File {
                bytes,
                mode,
                uid,
                gid,
            },
        );
    }
}

/// Puts `root` back to `before`: what is not in it is removed, what differs is written again.
/// Best effort, and it never panics (it runs in a drop guard).
fn restore(root: &Path, before: &BTreeMap<PathBuf, Entry>) {
    let now = tree(root);
    // Deepest first, so a directory is emptied before it is removed.
    for (path, entry) in now.iter().rev() {
        if before.contains_key(path) {
            continue;
        }
        let _ = match entry {
            Entry::Dir { .. } => fs::remove_dir_all(path),
            _ => fs::remove_file(path),
        };
    }
    for (path, entry) in before {
        if now.get(path) == Some(entry) {
            continue;
        }
        match entry {
            Entry::Dir { mode, uid, gid } => {
                let _ = fs::create_dir_all(path);
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(*mode));
                let _ = chown(path, Some(*uid), Some(*gid));
            }
            Entry::File {
                bytes,
                mode,
                uid,
                gid,
            } => {
                let _ = fs::remove_file(path);
                let _ = fs::write(path, bytes);
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(*mode));
                let _ = chown(path, Some(*uid), Some(*gid));
            }
            Entry::Link(target) => {
                let _ = fs::remove_file(path);
                let _ = symlink(target, path);
            }
        }
    }
}

/// What a unit was before the test.
struct UnitState {
    name: &'static str,
    active: bool,
    enabled: bool,
}

/// The snapshot of the host's sshd that a test puts back when it ends.
struct HostGuard {
    etc_ssh: BTreeMap<PathBuf, Entry>,
    units: Vec<UnitState>,
    /// An account the test made, removed with its home.
    account: Mutex<Option<&'static str>>,
}

impl HostGuard {
    fn take() -> HostGuard {
        as_root();
        assert!(
            Path::new(SSHD).is_file(),
            "{SSHD} is missing: this test needs openssh-server (GitHub's ubuntu-24.04 image has it \
             through the `ssh` package)"
        );
        let units = ["ssh.service", "sshd.service"]
            .into_iter()
            .filter(|name| {
                !matches!(
                    systemctl(&["show", "-p", "LoadState", "--value", name]).as_str(),
                    "not-found" | "" | "masked"
                )
            })
            .map(|name| UnitState {
                name,
                active: systemctl(&["is-active", name]) == "active",
                enabled: systemctl(&["is-enabled", name]) == "enabled",
            })
            .collect();
        HostGuard {
            etc_ssh: tree(Path::new("/etc/ssh")),
            units,
            account: Mutex::new(None),
        }
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        restore(Path::new("/etc/ssh"), &self.etc_ssh);
        let account = self.account.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(name) = *account {
            // --force: the account's last ssh session may still be closing, and a plain
            // userdel refuses an account that a process still runs as.
            let _ = Command::new("/usr/sbin/userdel")
                .args(["--force", "--remove", name])
                .output();
        }
        for unit in &self.units {
            if unit.active {
                let _ = Command::new("/usr/bin/systemctl")
                    .args(["reload", unit.name])
                    .output();
            } else {
                let _ = Command::new("/usr/bin/systemctl")
                    .args(["stop", unit.name])
                    .output();
            }
            if !unit.enabled && systemctl(&["is-enabled", unit.name]) == "enabled" {
                let _ = Command::new("/usr/bin/systemctl")
                    .args(["disable", unit.name])
                    .output();
            }
            // Every test starts the unit again: clear systemd's count of its starts, or the
            // sixth start within its interval is refused (`start-limit-hit`).
            let _ = Command::new("/usr/bin/systemctl")
                .args(["reset-failed", unit.name])
                .output();
        }
    }
}

/// The lines the setup logged.
#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl Log for Lines {
    fn line(&self, text: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.to_owned());
    }
}

/// A key pair made by `ssh-keygen`.
struct Key {
    private: PathBuf,
    public: PathBuf,
}

impl Key {
    fn make(dir: &Path, name: &str) -> Key {
        let private = dir.join(name);
        must(
            "/usr/bin/ssh-keygen",
            &[
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                name,
                "-f",
                &private.display().to_string(),
            ],
        );
        Key {
            public: dir.join(format!("{name}.pub")),
            private,
        }
    }

    /// The `type base64 comment` line.
    fn line(&self) -> String {
        fs::read_to_string(&self.public)
            .expect("public key")
            .trim()
            .to_owned()
    }
}

/// The whole test bench: the guard, a scratch directory, the CA and a user key, and the
/// daemon's real setup.
struct Rig {
    // The guard is dropped last: the scratch directory goes first.
    dir: Scratch,
    ca: Key,
    user: Key,
    host: Arc<SystemHost>,
    setup: SshSetup,
    log: Arc<Lines>,
    generation: u64,
    certs: u32,
    guard: HostGuard,
}

/// The record that turns the setup on for `root`, trusting `ca`.
fn access(ca: &Key, generation: u64, revoked: &[u64]) -> PushedSsh {
    PushedSsh {
        generation,
        enabled: true,
        ca_public_keys: vec![ca.line()],
        login_user: "root".to_owned(),
        principal: PRINCIPAL.to_owned(),
        revoked_serials: revoked.to_vec(),
        password_auth: "keep".to_owned(),
    }
}

impl Rig {
    fn new(name: &str) -> Rig {
        let guard = HostGuard::take();
        let dir = scratch(name);
        let tools = Tools {
            work: dir.path().join("krl-work"),
            ..Tools::linux()
        };
        let host = Arc::new(SystemHost::new(
            Arc::new(ProcessRunner::new(1 << 20)),
            tools,
        ));
        let log = Arc::new(Lines::default());
        let setup = SshSetup::new(host.clone(), SshPaths::linux(), 0, log.clone());
        Rig {
            ca: Key::make(dir.path(), "ca"),
            user: Key::make(dir.path(), "user"),
            dir,
            host,
            setup,
            log,
            generation: 0,
            certs: 0,
            guard,
        }
    }

    /// Takes up a record that trusts the rig's CA for `root` with `revoked` serials, and reports
    /// what the setup says of it.
    fn apply(&mut self, revoked: &[u64]) -> SshReport {
        self.generation += 1;
        self.setup
            .apply(&access(&self.ca, self.generation, revoked));
        self.setup.report().expect("a report after an apply")
    }

    /// `apply`, which must come to `applied` (the lines of the log say what else it came to).
    fn applied(&mut self, revoked: &[u64]) {
        let report = self.apply(revoked);
        let log = self
            .log
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(
            (report.state.as_str(), report.detail.as_str()),
            ("applied", ""),
            "{log:?}"
        );
    }

    /// A certificate for the rig's user key signed by its CA: for `principals`, valid in
    /// `validity` (`ssh-keygen -V`), with `serial`. The path of the certificate.
    fn cert(&mut self, principals: &str, validity: &str, serial: u64) -> PathBuf {
        self.certs += 1;
        let dir = self.dir.path().join(format!("cert{}", self.certs));
        fs::create_dir(&dir).expect("cert dir");
        fs::copy(&self.user.public, dir.join("user.pub")).expect("copy");
        must(
            "/usr/bin/ssh-keygen",
            &[
                "-q",
                "-s",
                &self.ca.private.display().to_string(),
                "-I",
                &format!("test-{serial}"),
                "-n",
                principals,
                "-V",
                validity,
                "-z",
                &serial.to_string(),
                &dir.join("user.pub").display().to_string(),
            ],
        );
        dir.join("user-cert.pub")
    }

    /// A certificate valid from now for an hour, for the host's principal.
    fn good_cert(&mut self, serial: u64) -> PathBuf {
        self.cert(PRINCIPAL, "-5m:+1h", serial)
    }

    /// Logs in as `user` to the sshd on the loopback with `key`, and with `cert` when given:
    /// what the remote `id -un` printed, or what ssh said on standard error.
    fn login(&self, user: &str, key: &Key, cert: Option<&Path>) -> Result<String, String> {
        let (address, port) = self.host.effective(user).expect("sshd -T").listening();
        let known = self.dir.path().join("known_hosts");
        let mut command = Command::new("/usr/bin/ssh");
        command
            .args([
                "-F",
                "/dev/null",
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
            ])
            .args([
                "-o",
                "IdentityAgent=none",
                "-o",
                "PreferredAuthentications=publickey",
            ])
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
            ])
            .args(["-o", "LogLevel=ERROR", "-o", "ConnectTimeout=10"])
            .args(["-o", &format!("UserKnownHostsFile={}", known.display())])
            .args([
                "-p",
                &port.to_string(),
                "-i",
                &key.private.display().to_string(),
            ]);
        if let Some(cert) = cert {
            command.args(["-o", &format!("CertificateFile={}", cert.display())]);
        }
        let out = command
            .arg(format!("{user}@{address}"))
            .arg("id -un")
            .output()
            .expect("run ssh");
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }

    /// Asserts that `login` was refused by sshd's authentication (not by a connection that did
    /// not happen).
    fn refused(&self, user: &str, key: &Key, cert: &Path) {
        let result = self.login(user, key, Some(cert));
        match result {
            Err(said) => assert!(said.contains("Permission denied"), "{said}"),
            Ok(who) => panic!("the login was accepted, as {who}"),
        }
    }

    /// The report of an apply of a record that must not be applied: its state and detail, with
    /// none of the setup's files written.
    fn not_applied(&mut self) -> (String, String) {
        let report = self.apply(&[]);
        let paths = SshPaths::linux();
        for file in [paths.drop_in(), paths.ca_keys(), paths.principals("root")] {
            assert!(!file.exists(), "{} was written", file.display());
        }
        (report.state, report.detail)
    }

    /// Puts a line first in the stock `sshd_config` (the first value of a keyword wins, so it
    /// beats whatever the file or its includes say after it); the guard puts the file back.
    fn stock_line(&self, line: &str) {
        let path = Path::new("/etc/ssh/sshd_config");
        let text = fs::read_to_string(path).expect("sshd_config");
        fs::write(path, format!("{line}\n{text}")).expect("write sshd_config");
        // sshd -t refuses to run without its privilege separation directory, which only sshd's
        // own unit (or the setup, before it runs sshd) makes on a host where sshd never ran.
        fs::create_dir_all(Tools::linux().privsep).expect("the privilege separation directory");
        must(SSHD, &["-t"]);
    }
}

/// Proves that with the setup applied from a record for `root`, a certificate for the host's
/// principal signed by the trusted CA logs `root` in through the real sshd, and that the same
/// user key without the certificate is refused (so the certificate is what let it in).
/// Catches: a drop-in that sshd accepts at `-t` but that does not make it take the CA, the
/// principals file or the revoked list; a setup that reports `applied` for a host where no
/// certificate logs in.
#[test]
fn a_certificate_for_the_host_logs_the_login_user_in() {
    let mut rig = Rig::new("ssh-root-accept");
    rig.applied(&[]);
    let cert = rig.good_cert(1);
    assert_eq!(
        rig.login("root", &rig.user, Some(&cert)),
        Ok("root".to_owned())
    );
    let bare = rig.login("root", &rig.user, None);
    assert!(
        bare.as_ref()
            .is_err_and(|said| said.contains("Permission denied")),
        "{bare:?}"
    );
}

/// Proves that a certificate whose validity ended in the past is refused by sshd while a valid
/// one of the same key and principal, in the same configuration, is accepted. Catches: a trust
/// setup that skips the certificate's validity window.
#[test]
fn an_expired_certificate_is_refused() {
    let mut rig = Rig::new("ssh-root-expired");
    rig.applied(&[]);
    let valid = rig.good_cert(1);
    assert_eq!(
        rig.login("root", &rig.user, Some(&valid)),
        Ok("root".to_owned())
    );
    // Valid from two hours ago to one hour ago: expired, in ssh-keygen's relative times.
    let expired = rig.cert(PRINCIPAL, "-2h:-1h", 2);
    rig.refused("root", &rig.user, &expired);
}

/// Proves that a certificate whose only principal is another host's is refused for `root` while
/// the one for this host is accepted. Catches: an `AuthorizedPrincipalsFile` that is not in
/// effect, so that any certificate of the CA logs in.
#[test]
fn a_certificate_for_another_hosts_principal_is_refused() {
    let mut rig = Rig::new("ssh-root-principal");
    rig.applied(&[]);
    let valid = rig.good_cert(1);
    assert_eq!(
        rig.login("root", &rig.user, Some(&valid)),
        Ok("root".to_owned())
    );
    let other = rig.cert("hk-host-other", "-5m:+1h", 2);
    rig.refused("root", &rig.user, &other);
}

/// Proves that a certificate whose principal is an account's own name, for that account, is
/// refused: a real account is made, its own key logs it in (so it can log in at all), and the
/// CA's certificate with its name as principal does not. Catches: a configuration without the
/// principals file, where sshd takes a CA signature naming the account as a login for any
/// account.
#[test]
fn a_certificate_naming_another_account_does_not_log_that_account_in() {
    let mut rig = Rig::new("ssh-root-alice");
    rig.applied(&[]);
    assert!(
        !Path::new("/home").join(ALICE).exists(),
        "{ALICE} is on the host already"
    );
    must(
        "/usr/sbin/useradd",
        &["--create-home", "--shell", "/bin/sh", "-p", "*", ALICE],
    );
    *rig.guard
        .account
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(ALICE);
    let uid: u32 = must("/usr/bin/id", &["-u", ALICE])
        .trim()
        .parse()
        .expect("uid");
    let gid: u32 = must("/usr/bin/id", &["-g", ALICE])
        .trim()
        .parse()
        .expect("gid");
    let own = Key::make(rig.dir.path(), "alice");
    let ssh_dir = Path::new("/home").join(ALICE).join(".ssh");
    fs::create_dir(&ssh_dir).expect(".ssh");
    let keys = ssh_dir.join("authorized_keys");
    fs::write(&keys, format!("{}\n", own.line())).expect("authorized_keys");
    for (path, mode) in [(&ssh_dir, 0o700), (&keys, 0o600)] {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("mode");
        chown(path, Some(uid), Some(gid)).expect("chown");
    }
    assert_eq!(
        rig.login(ALICE, &own, None),
        Ok(ALICE.to_owned()),
        "the account's own key must log it in, or the refusal below proves nothing"
    );
    let named = rig.cert(ALICE, "-5m:+1h", 1);
    rig.refused(ALICE, &rig.user, &named);
    let hosts = rig.good_cert(2);
    rig.refused(ALICE, &rig.user, &hosts);
}

/// Proves that a certificate whose serial is in the revoked list the record carries is refused,
/// while one of another serial of the same CA is accepted. Catches: a revoked list that is
/// written but not read (`RevokedKeys` not in effect), or one built for the wrong CA.
#[test]
fn a_certificate_whose_serial_is_revoked_is_refused() {
    let mut rig = Rig::new("ssh-root-revoked");
    rig.applied(&[7]);
    let fine = rig.good_cert(8);
    assert_eq!(
        rig.login("root", &rig.user, Some(&fine)),
        Ok("root".to_owned())
    );
    let revoked = rig.good_cert(7);
    rig.refused("root", &rig.user, &revoked);
}

/// Proves that a stock configuration with `PermitRootLogin no` first in it is reported as
/// `cert_login_not_possible` with `permitrootlogin` in the detail, and that none of the setup's
/// files is written. Catches: an `applied` for a root login that sshd refuses whatever the
/// certificate.
#[test]
fn a_stock_permit_root_login_no_is_reported_with_its_keyword() {
    let mut rig = Rig::new("ssh-root-prl");
    rig.stock_line("PermitRootLogin no");
    assert_eq!(
        rig.not_applied(),
        (
            "cert_login_not_possible".to_owned(),
            "permitrootlogin".to_owned()
        )
    );
}

/// Proves that a stock configuration with `AuthenticationMethods publickey,password` first in it
/// is reported as `cert_login_not_possible` with `authenticationmethods` in the detail, and that
/// none of the setup's files is written. `PasswordAuthentication yes` goes first with it: sshd's
/// own `-t` refuses a list naming a method that is off, and a runner's image turns passwords off.
/// Catches: an `applied` for a login where a certificate alone is not enough.
#[test]
fn a_stock_authentication_methods_with_a_password_is_reported_with_its_keyword() {
    let mut rig = Rig::new("ssh-root-am");
    rig.stock_line("PasswordAuthentication yes\nAuthenticationMethods publickey,password");
    assert_eq!(
        rig.not_applied(),
        (
            "cert_login_not_possible".to_owned(),
            "authenticationmethods".to_owned()
        )
    );
}

/// Proves that when sshd's own `-t` refuses the candidate drop-in (the setup's own directory has
/// a space in its name, so the directives take two words), the report is `config_invalid` with
/// `sshd_t`, and the whole of `/etc/ssh`, the live files of an earlier applied record among
/// them, is byte for byte, mode for mode what it was, with the earlier certificate login still
/// accepted. It also shows that sshd itself refuses that text and accepts the same text for the
/// real directory. Catches: a candidate written into the live files before it is checked; a
/// failed check that leaves a file behind.
#[test]
fn a_candidate_that_fails_sshd_t_leaves_the_live_files_as_they_were() {
    let mut rig = Rig::new("ssh-root-candidate");
    rig.applied(&[]);
    let cert = rig.good_cert(1);
    let before = tree(Path::new("/etc/ssh"));

    let own_dir = rig.dir.path().join("own dir");
    fs::create_dir(&own_dir).expect("own dir");
    let paths = SshPaths {
        own_dir,
        ..SshPaths::linux()
    };
    let text = hostkeeper_daemon::ssh_files::render_drop_in(&paths, false);
    let candidate = rig.dir.path().join("candidate.conf");
    fs::write(&candidate, &text).expect("candidate");
    assert!(
        rig.host.check_config(&candidate).is_err(),
        "sshd takes the text"
    );
    let fine = hostkeeper_daemon::ssh_files::render_drop_in(&SshPaths::linux(), false);
    fs::write(&candidate, fine).expect("candidate");
    assert_eq!(rig.host.check_config(&candidate), Ok(()));

    let log = Arc::new(Lines::default());
    let setup = SshSetup::new(rig.host.clone(), paths, 0, log);
    setup.apply(&access(&rig.ca, 100, &[]));
    let report = setup.report().expect("a report");
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "sshd_t")
    );
    assert_eq!(tree(Path::new("/etc/ssh")), before);
    assert_eq!(
        rig.login("root", &rig.user, Some(&cert)),
        Ok("root".to_owned())
    );
}

/// Proves that the revert (a record with `enabled = false`) removes the drop-in, the CA keys,
/// the principals file and the revoked list, leaves `/etc/ssh` as it was before the setup, and
/// that a certificate that logged in before is refused after it. Catches: a revert that leaves
/// trust behind (a file, or a directory sshd still reads).
#[test]
fn the_revert_removes_the_files_and_the_trust() {
    let mut rig = Rig::new("ssh-root-revert");
    let untouched = tree(Path::new("/etc/ssh"));
    rig.applied(&[7]);
    let cert = rig.good_cert(1);
    assert_eq!(
        rig.login("root", &rig.user, Some(&cert)),
        Ok("root".to_owned())
    );
    let paths = SshPaths::linux();
    for file in [
        paths.drop_in(),
        paths.ca_keys(),
        paths.principals("root"),
        paths.revoked(),
    ] {
        assert!(file.is_file(), "{} is missing", file.display());
    }

    rig.generation += 1;
    let off = PushedSsh {
        generation: rig.generation,
        enabled: false,
        ..PushedSsh::default()
    };
    rig.setup.apply(&off);
    let report = rig.setup.report().expect("a report");
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("disabled", "")
    );
    assert_eq!(tree(Path::new("/etc/ssh")), untouched);
    rig.refused("root", &rig.user, &cert);
}
