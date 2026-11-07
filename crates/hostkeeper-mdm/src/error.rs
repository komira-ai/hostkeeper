//! Why a verb refused or failed.

use std::io;

use hostkeeper_mdm_core::config::ConfigError;
use hostkeeper_mdm_core::path::PathError;
use hostkeeper_mdm_core::template::TemplateError;
use hostkeeper_mdm_step::{ExecError, PlanError};

use crate::archive::ArchiveError;
use crate::dep::DepError;
use crate::migrate::MoveError;
use crate::net::DownloadError;
use crate::push_cert::PushCertError;

/// Why a verb refused (exit 2: fix the input or the host) or failed (exit 1: a step, a
/// download or a check did not work).
#[derive(Debug, thiserror::Error)]
pub enum MdmError {
    /// The settings file was refused.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A unit template did not render.
    #[error(transparent)]
    Template(#[from] TemplateError),
    /// A path is not an absolute path.
    #[error(transparent)]
    Path(#[from] PathError),
    /// An archive could not be read.
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    /// A plan could not be printed or carried out.
    #[error(transparent)]
    Exec(#[from] ExecError),
    /// A plan could not be made: the host could not be looked at, or the service user
    /// resolves to root.
    #[error(transparent)]
    Plan(#[from] PlanError),
    /// The host is not of the architecture the pinned tools are built for.
    #[error(
        "this host is {machine} (uname -m): the pinned tools are linux-amd64 builds, for x86_64 only"
    )]
    Architecture {
        /// What `uname -m` printed.
        machine: String,
    },
    /// The SCEP CA's depot holds part of a CA.
    #[error(
        "{depot} holds {present} but no {missing}: a CA is never made over part of one (it would re-enroll every Mac); restore {missing}, or move the depot aside to make a new CA"
    )]
    DepotPartial {
        /// The depot.
        depot: String,
        /// The file that is there.
        present: &'static str,
        /// The file that is not.
        missing: &'static str,
    },
    /// The SCEP CA's depot holds a CA that is damaged.
    #[error("{depot}: {why}: restore the depot from a backup, or move it aside to make a new CA")]
    DepotDamaged {
        /// The depot.
        depot: String,
        /// What is wrong.
        why: String,
    },
    /// `lego run --help` failed.
    #[error("{lego} run --help exited with status {code}: not a lego that runs here")]
    LegoHelp {
        /// The binary.
        lego: String,
        /// Its status.
        code: i32,
    },
    /// Something other than a regular file is where the certificate goes.
    #[error("{0} is not a regular file (a directory or a link): remove it, and run tls again")]
    CertNotFile(String),
    /// A file or program of the host could not be reached.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// A file that must be there is not.
    #[error("{0} is not there")]
    NoFile(String),
    /// `fetch` was run as root.
    #[error("run fetch as yourself, not root: downloads never run as root")]
    FetchAsRoot,
    /// `--dir` is inside a git work tree.
    #[error(
        "--dir {dir} is inside a git work tree ({git} exists): keep archives and keys outside every repository"
    )]
    InGitTree {
        /// The directory.
        dir: String,
        /// The `.git` found.
        git: String,
    },
    /// A kept archive is not the pin.
    #[error("{file}: sha256 {got} is not the pin {want}: delete it and re-run")]
    KeptDiffers {
        /// The file.
        file: String,
        /// Its sha256.
        got: String,
        /// The pin's.
        want: String,
    },
    /// A download is not the pin.
    #[error(
        "{tool}: the download's sha256 {got} is not the pin {want} (kept as {kept} for inspection)"
    )]
    Rejected {
        /// The tool.
        tool: String,
        /// The download's sha256.
        got: String,
        /// The pin's.
        want: String,
        /// Where the download was kept.
        kept: String,
    },
    /// A download failed.
    #[error("download of {url} failed: {source}")]
    Download {
        /// The URL.
        url: String,
        /// Why.
        #[source]
        source: DownloadError,
    },
    /// A verb that reads root-only files was run by someone else.
    #[error(
        "{verb} reads root-only files: run it as root, even for a dry run (effective uid is {euid})"
    )]
    NeedsRoot {
        /// The verb.
        verb: &'static str,
        /// The effective uid.
        euid: u32,
    },
    /// An archive `fetch` should have downloaded is missing.
    #[error("{0} is missing (hostkeeper-mdm fetch --apply downloads it)")]
    NoArchive(String),
    /// An archive is not its pin.
    #[error("{path}: sha256 {got} is not the pin {want}")]
    ArchiveDiffers {
        /// The file.
        path: String,
        /// Its sha256.
        got: String,
        /// The pin's.
        want: String,
    },
    /// A directory the kit owns is a symbolic link.
    #[error("{0} is a symbolic link: name the real directory")]
    IsLink(String),
    /// An archive is larger than the cap.
    #[error("{path} is larger than the {max} bytes an archive may be")]
    ArchiveTooBig {
        /// The file.
        path: String,
        /// The cap.
        max: u64,
    },
    /// A secret file would lose other lines when it is made again.
    #[error(
        "{path} has no value for {key} but holds other lines: remove them or give {key} a value, and run again"
    )]
    SecretOther {
        /// The file.
        path: String,
        /// The key.
        key: String,
    },
    /// An `--ignore` value that cannot be one whole line.
    #[error("--ignore {0:?} must be one non-empty line")]
    BadIgnore(String),
    /// The bind address is on no interface.
    #[error("no interface carries MDM_BIND={0} (ip -4 addr)")]
    NotCarried(String),
    /// A port is held by something that is not the kit.
    #[error("port {0} is held by another process (ss -ltnp 'sport = :{0}')")]
    PortHeld(u16),
    /// A secret file is not a 0600 file of root's.
    #[error(
        "{path} has mode {mode:04o} and uid {uid}, not 0600 and root: chmod 600 {path}; chown root:root {path}"
    )]
    SecretMode {
        /// The file.
        path: String,
        /// Its mode.
        mode: u32,
        /// Its owner.
        uid: u32,
    },
    /// A secret file lacks a value.
    #[error("{path} has no {key}= line with a value")]
    NoSecret {
        /// The file.
        path: String,
        /// The key.
        key: String,
    },
    /// A secret value holds a control character.
    #[error("{path}: the value of {key} holds a control character")]
    SecretValue {
        /// The file.
        path: String,
        /// The key.
        key: String,
    },
    /// A secret file holds a construct whose reading by systemd is not certain.
    #[error(
        "{path} line {line}: {why}: systemd might read it differently from this tool; write the value on one line as KEY=value"
    )]
    SecretSyntax {
        /// The file.
        path: String,
        /// The line where the construct starts, from 1.
        line: usize,
        /// What was refused.
        why: &'static str,
    },
    /// A secret file is not UTF-8 text.
    #[error("{path} is not UTF-8 text")]
    SecretEncoding {
        /// The file.
        path: String,
    },
    /// lego is not installed.
    #[error("{0} is missing: hostkeeper-mdm install --apply installs it")]
    NoLego(String),
    /// lego lacks a flag the certificate unit passes.
    #[error("{lego} run --help lists no {flag}: not the lego the pins name")]
    LegoFlag {
        /// The binary.
        lego: String,
        /// The flag.
        flag: &'static str,
    },
    /// A program the verb relies on failed.
    #[error("{line} exited with status {code}")]
    Command {
        /// The command line.
        line: String,
        /// The status.
        code: i32,
    },
    /// There is no certificate after the issuing unit ran.
    #[error("no certificate at {0} after the certificate unit ran: journalctl -u on its unit")]
    NoCertificate(String),
    /// The certificate file is not one openssl reads.
    #[error("{0} is there and is not a PEM certificate")]
    NotCertificate(String),
    /// A call to a service's API on loopback did not complete, or was not answered 2xx.
    #[error("{call}: {why}")]
    Api {
        /// The call, as its plan line shows it (no body, no key).
        call: String,
        /// Why it failed: the transport's error, or the status and the start of the answer.
        why: String,
    },
    /// A file the operator names (a key, a token, a certificate) is inside a git work tree.
    #[error(
        "{path} is inside a git work tree ({git} exists): keep keys, tokens and certificates outside every repository"
    )]
    FileInGitTree {
        /// The file.
        path: String,
        /// The `.git` found.
        git: String,
    },
    /// `migrate`'s settings, old or new, were refused.
    #[error(transparent)]
    Settings(#[from] hostkeeper_mdm_core::migrate::MigrateError),
    /// `push-cert` refused or failed.
    #[error(transparent)]
    PushCert(#[from] PushCertError),
    /// `dep` refused or failed.
    #[error(transparent)]
    Dep(#[from] DepError),
    /// `migrate` refused or failed.
    #[error(transparent)]
    Move(#[from] MoveError),
    /// The certificate file that is already there is not one openssl reads; nothing was changed.
    #[error(
        "{0} is there and openssl cannot read it as a PEM certificate: move it aside, and run tls again"
    )]
    CertUnreadable(String),
}

impl MdmError {
    /// The exit code: 1 when a step, a download or a program failed, 2 when the input or
    /// the host was refused.
    pub fn exit_code(&self) -> u8 {
        match self {
            MdmError::Exec(ExecError::Step { .. } | ExecError::Write(_))
            | MdmError::Plan(PlanError::Host { .. })
            | MdmError::Io(_)
            | MdmError::Rejected { .. }
            | MdmError::Download { .. }
            | MdmError::Command { .. }
            | MdmError::NoCertificate(_)
            | MdmError::NotCertificate(_)
            | MdmError::Api { .. } => 1,
            MdmError::PushCert(e) => e.exit_code(),
            MdmError::Dep(e) => e.exit_code(),
            MdmError::Move(e) => e.exit_code(),
            _ => 2,
        }
    }
}
