//! The host the daemon sets sshd up on, behind a port: the sshd unit, `sshd -t` and `sshd -T`,
//! the accounts and their groups, sshd's host keys and the list of revoked serials
//! (docs/design/ssh-through-coordinator.md section 3). [`SystemHost`] is the real one, over the
//! process port of `hostkeeper_observe` (`systemctl`, `sshd`, `ssh-keygen`, `id`) and the files
//! of `/etc`; the setup's tests script the port.
//!
//! Nothing here stops a unit or edits a file of sshd's own (rule 6 and rule 1 of the design):
//! the one change [`SshHost::start`] makes is to enable a disabled unit and start a stopped one.

use std::fs;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_observe::CommandRunner;

use crate::ssh_effective::Effective;

/// The longest a program of the host is given.
pub const RUN_DEADLINE: Duration = Duration::from_secs(20);

/// The longest the banner check waits to connect and to read.
pub const BANNER_WAIT: Duration = Duration::from_secs(5);

/// The unit that runs sshd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshdUnit {
    /// A service unit, by name: reloaded when its configuration changes.
    Service(String),
    /// The socket unit is the active one: sshd is started per connection and reads its
    /// configuration then, so there is nothing to reload.
    Socket,
}

/// Why there is no unit to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitError {
    /// Neither `ssh.service` nor `sshd.service` is known to the service manager.
    Missing,
    /// The service manager cannot be asked (no `systemctl`, or it fails).
    NoManager,
    /// The unit is masked: the daemon does not unmask what an operator masked.
    Masked,
}

/// One account of the host that matters to the lockout rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    /// Its home has a non-empty `authorized_keys`.
    pub keyed: bool,
    /// It has a login shell and a usable password hash (or its hash cannot be read).
    pub password: bool,
}

/// The host, as the sshd setup needs it.
pub trait SshHost: Send + Sync {
    /// The unit that runs sshd, found without changing anything.
    ///
    /// # Errors
    /// Why there is none.
    fn unit(&self) -> Result<SshdUnit, UnitError>;

    /// Enables the unit when it is disabled and starts it when it is stopped; stops nothing.
    ///
    /// # Errors
    /// What failed.
    fn start(&self, unit: &SshdUnit) -> Result<(), String>;

    /// Has sshd read its configuration again, keeping established sessions; nothing for a
    /// socket.
    ///
    /// # Errors
    /// What failed.
    fn reload(&self, unit: &SshdUnit) -> Result<(), String>;

    /// `sshd -t` over the configuration file `candidate`.
    ///
    /// # Errors
    /// What sshd said or how it ended.
    fn check_config(&self, candidate: &Path) -> Result<(), String>;

    /// The effective configuration for a login of `user` from the loopback.
    ///
    /// # Errors
    /// What failed: the configuration does not parse, or sshd cannot run.
    fn effective(&self, user: &str) -> Result<Effective, String>;

    /// The groups `user` belongs to; empty when they cannot be read.
    fn groups(&self, user: &str) -> Vec<String>;

    /// Connects to sshd at `address` and reads its banner.
    ///
    /// # Errors
    /// Why no banner came.
    fn banner(&self, address: &(String, u16)) -> Result<(), String>;

    /// Every account with a key path or a password path.
    fn accounts(&self) -> Vec<Account>;

    /// The `SHA256:` fingerprints of sshd's host keys, sorted by file name.
    fn host_key_fingerprints(&self) -> Vec<String>;

    /// The bytes of the `RevokedKeys` file that revokes `serials` of certificates of any of
    /// `ca_keys`: empty for no serial.
    ///
    /// # Errors
    /// What failed.
    fn revocations(&self, ca_keys: &[String], serials: &[u64]) -> Result<Vec<u8>, String>;
}

/// Where the real host's programs and files are.
#[derive(Clone, Debug)]
pub struct Tools {
    pub systemctl: PathBuf,
    pub sshd: PathBuf,
    pub ssh_keygen: PathBuf,
    pub id: PathBuf,
    /// `/etc`: `passwd`, `shadow` and `ssh/ssh_host_*_key.pub` are read from it.
    pub etc: PathBuf,
    /// A directory the list of revoked serials is built in, made and removed each time.
    pub work: PathBuf,
}

impl Tools {
    /// The places of a Linux host.
    #[must_use]
    pub fn linux() -> Tools {
        Tools {
            systemctl: PathBuf::from("/usr/bin/systemctl"),
            sshd: PathBuf::from("/usr/sbin/sshd"),
            ssh_keygen: PathBuf::from("/usr/bin/ssh-keygen"),
            id: PathBuf::from("/usr/bin/id"),
            etc: PathBuf::from("/etc"),
            work: PathBuf::from("/etc/ssh/hostkeeper/.krl-work"),
        }
    }
}

/// The real host: programs through a [`CommandRunner`], files through the filesystem.
pub struct SystemHost {
    runner: Arc<dyn CommandRunner>,
    tools: Tools,
}

/// What `systemctl show` said of a unit.
struct Props {
    loaded: bool,
    masked: bool,
    active: bool,
    disabled: bool,
}

impl SystemHost {
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>, tools: Tools) -> SystemHost {
        SystemHost { runner, tools }
    }

    /// Runs `program` with `args`; its exit code and standard output when it ran.
    fn run(&self, program: &Path, args: &[&str]) -> Option<(Option<i32>, String)> {
        let mut argv = vec![program.display().to_string()];
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        let output = self.runner.run(&argv, RUN_DEADLINE).ok()?;
        Some((
            output.exit_code,
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    /// Runs `program` and says what failed unless it exits 0.
    fn succeed(&self, program: &Path, args: &[&str]) -> Result<String, String> {
        let name = program
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        match self.run(program, args) {
            Some((Some(0), stdout)) => Ok(stdout),
            Some((code, _)) => Err(format!(
                "{name} ended with {}",
                code.map_or_else(|| "a signal".to_owned(), |code| format!("exit {code}"))
            )),
            None => Err(format!("{name} could not be run")),
        }
    }

    fn show(&self, unit: &str) -> Result<Props, UnitError> {
        let args = ["show", "-p", "LoadState,ActiveState,UnitFileState", unit];
        let Some((Some(0), text)) = self.run(&self.tools.systemctl, &args) else {
            return Err(UnitError::NoManager);
        };
        let value = |key: &str| {
            text.lines()
                .filter_map(|line| line.split_once('='))
                .find(|(name, _)| *name == key)
                .map_or("", |(_, value)| value)
        };
        Ok(Props {
            loaded: value("LoadState") == "loaded",
            masked: value("LoadState") == "masked",
            active: value("ActiveState") == "active",
            disabled: value("UnitFileState") == "disabled",
        })
    }
}

impl SshHost for SystemHost {
    fn unit(&self) -> Result<SshdUnit, UnitError> {
        if self.show("ssh.socket")?.active {
            return Ok(SshdUnit::Socket);
        }
        let mut masked = false;
        for name in ["ssh.service", "sshd.service"] {
            let props = self.show(name)?;
            if props.loaded {
                return Ok(SshdUnit::Service(name.to_owned()));
            }
            masked |= props.masked;
        }
        Err(if masked {
            UnitError::Masked
        } else {
            UnitError::Missing
        })
    }

    fn start(&self, unit: &SshdUnit) -> Result<(), String> {
        let SshdUnit::Service(name) = unit else {
            return Ok(());
        };
        let props = self.show(name).map_err(|_| "systemctl failed".to_owned())?;
        if props.disabled {
            self.succeed(&self.tools.systemctl, &["enable", name.as_str()])?;
        }
        if !props.active {
            self.succeed(&self.tools.systemctl, &["start", name.as_str()])?;
        }
        Ok(())
    }

    fn reload(&self, unit: &SshdUnit) -> Result<(), String> {
        match unit {
            SshdUnit::Socket => Ok(()),
            SshdUnit::Service(name) => self
                .succeed(&self.tools.systemctl, &["reload", name.as_str()])
                .map(drop),
        }
    }

    fn check_config(&self, candidate: &Path) -> Result<(), String> {
        let path = candidate.display().to_string();
        self.succeed(&self.tools.sshd, &["-t", "-f", path.as_str()])
            .map(drop)
    }

    fn effective(&self, user: &str) -> Result<Effective, String> {
        let spec = format!("user={user},host=localhost,addr=127.0.0.1");
        self.succeed(&self.tools.sshd, &["-T", "-C", spec.as_str()])
            .map(|text| Effective::parse(&text))
    }

    fn groups(&self, user: &str) -> Vec<String> {
        self.succeed(&self.tools.id, &["-Gn", user])
            .map(|text| text.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn banner(&self, address: &(String, u16)) -> Result<(), String> {
        let ip: IpAddr = address
            .0
            .parse()
            .map_err(|_| "the listen address is not an IP address".to_owned())?;
        let mut stream = TcpStream::connect_timeout(&SocketAddr::new(ip, address.1), BANNER_WAIT)
            .map_err(|e| format!("no connection: {e}"))?;
        // Fails only for a zero timeout, which `BANNER_WAIT` is not.
        let _ = stream.set_read_timeout(Some(BANNER_WAIT));
        let mut head = [0_u8; 4];
        stream
            .read_exact(&mut head)
            .map_err(|e| format!("no banner: {e}"))?;
        if &head == b"SSH-" {
            Ok(())
        } else {
            Err("the banner is not SSH".to_owned())
        }
    }

    fn accounts(&self) -> Vec<Account> {
        let passwd = fs::read_to_string(self.tools.etc.join("passwd")).unwrap_or_default();
        let shadow = fs::read_to_string(self.tools.etc.join("shadow")).ok();
        passwd
            .lines()
            .filter_map(|line| {
                let mut fields = line.split(':');
                let name = fields.next().filter(|name| !name.is_empty())?;
                let (home, shell) = (fields.nth(4)?, fields.next()?);
                let keyed = fs::metadata(Path::new(home).join(".ssh/authorized_keys"))
                    .is_ok_and(|meta| meta.len() > 0);
                let password = login_shell(shell) && usable_hash(shadow.as_deref(), name);
                (keyed || password).then(|| Account {
                    name: name.to_owned(),
                    keyed,
                    password,
                })
            })
            .collect()
    }

    fn host_key_fingerprints(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.tools.etc.join("ssh")) else {
            return Vec::new();
        };
        let mut keys: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with("ssh_host_") && name.ends_with("_key.pub")
                })
            })
            .collect();
        keys.sort();
        keys.iter()
            .filter_map(|key| {
                let path = key.display().to_string();
                let text = self
                    .succeed(&self.tools.ssh_keygen, &["-lf", path.as_str()])
                    .ok()?;
                text.split_whitespace()
                    .nth(1)
                    .filter(|print| print.starts_with("SHA256:"))
                    .map(str::to_owned)
            })
            .collect()
    }

    fn revocations(&self, ca_keys: &[String], serials: &[u64]) -> Result<Vec<u8>, String> {
        if serials.is_empty() {
            return Ok(Vec::new());
        }
        let work = &self.tools.work;
        let made = self.build_list(work, ca_keys, serials);
        let _ = fs::remove_dir_all(work);
        made
    }
}

impl SystemHost {
    /// The binary list of `serials` revoked under each of `ca_keys`, built in `work`.
    fn build_list(
        &self,
        work: &Path,
        ca_keys: &[String],
        serials: &[u64],
    ) -> Result<Vec<u8>, String> {
        let spec: String = serials
            .iter()
            .map(|serial| format!("serial: {serial}\n"))
            .collect();
        let spec_path = work.join("spec");
        let mut files = vec![(spec_path.clone(), spec)];
        files.extend(
            ca_keys
                .iter()
                .enumerate()
                .map(|(at, key)| (work.join(format!("ca{at}.pub")), format!("{key}\n"))),
        );
        fs::create_dir_all(work)
            .and_then(|()| {
                files
                    .iter()
                    .try_for_each(|(path, text)| fs::write(path, text))
            })
            .map_err(|e| format!("the work directory: {e}"))?;
        let list = work.join("krl");
        for (at, _) in ca_keys.iter().enumerate() {
            let ca = work.join(format!("ca{at}.pub"));
            let (list_arg, ca_arg, spec_arg) = (
                list.display().to_string(),
                ca.display().to_string(),
                spec_path.display().to_string(),
            );
            let mut args = vec!["-k"];
            if at > 0 {
                args.push("-u");
            }
            args.extend([
                "-f",
                list_arg.as_str(),
                "-s",
                ca_arg.as_str(),
                spec_arg.as_str(),
            ]);
            self.succeed(&self.tools.ssh_keygen, &args)?;
        }
        fs::read(&list).map_err(|e| format!("the revocation list: {e}"))
    }
}

/// Whether `shell` lets an account log in at all.
fn login_shell(shell: &str) -> bool {
    !(shell.is_empty() || shell.ends_with("nologin") || shell.ends_with("/false"))
}

/// Whether `name` has a password hash a login can use; true when the shadow file cannot be read,
/// so that an unknown is never taken for no password.
fn usable_hash(shadow: Option<&str>, name: &str) -> bool {
    let Some(shadow) = shadow else {
        return true;
    };
    shadow
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(user, _)| *user == name)
        .any(|(_, rest)| {
            let hash = rest.split(':').next().unwrap_or("");
            !(hash.is_empty() || hash.starts_with('!') || hash.starts_with('*'))
        })
}

#[cfg(test)]
#[path = "ssh_host_tests.rs"]
mod tests;
