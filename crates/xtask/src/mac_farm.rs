//! `mac-farm`: runs one Cargo command of this workspace on a Mac of the build farm, from a Linux
//! runner, as one Remote Execution (REAPI) action, and brings the files it names back.
//!
//! A farm Mac runs an action as an ordinary user under a sandbox with no network, and has the
//! Command Line Tools and no Rust. So everything the command needs is an input of the action,
//! gathered here:
//!
//! 1. the files git tracks in the checkout, at their paths, as they are on disk (so a step before
//!    this one, `release set-version`, is seen);
//! 2. the crates of `Cargo.lock` for `aarch64-apple-darwin`, which `cargo fetch` downloads into a
//!    Cargo home of this run's own (in `--scratch`), below [`CARGO_HOME`];
//! 3. the pinned Rust toolchain for `aarch64-apple-darwin` ([`toolchain`], the release
//!    `rust-toolchain.toml` names), below [`TOOLCHAIN`].
//!
//! The Command is the toolchain's `cargo` with the arguments after `--`, from the checkout's
//! root, offline, with the toolchain's `rustc` and `rustdoc`, the Mac's `/usr/bin` (its C
//! compiler and linker) and each `--env NAME=VALUE` (`RELEASE_COMMIT` of a release build). Its
//! `output_paths` are the `--output` paths. The action's platform is `--properties`, the farm's
//! Mac platform.
//!
//! The blobs the farm's CAS lacks are uploaded, the action is executed and followed until it is
//! done (a stage change is printed: queued while no Mac is free, then running), and the
//! command's stdout and stderr are printed. When it exited 0, each `--output` is read from the
//! farm into the checkout at its path ([`outputs`]). The verb ends 0 when the command exited 0
//! and every output was written, and 1 for every other end: the command's own non-zero exit, an
//! output the result does not hold, or the farm's refusal or failure, whose status is printed
//! (with the job's output tail when the farm attached one). A flag that is wrong ends it 2.
//! With `--summary` (a job's `GITHUB_STEP_SUMMARY`), what the run sent, how it ended and the
//! outputs it wrote are appended there as well.
//!
//! The verb's first lines mask the address in a workflow's log (`::add-mask::`), so nothing it
//! prints after them shows it. (A workflow step's own header still shows the values of its
//! `env:`; that is the workflow's to keep out.)
//!
//! With an empty `--address` the verb prints a notice and exits 0: a fork, or a repository with
//! no farm, runs nothing and fails nothing.

mod client;
mod outputs;
mod reapi;
mod toolchain;
mod tree;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod farm_tests;
#[cfg(test)]
mod outputs_tests;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use prost::Message as _;

use self::client::{Farm, FarmError, Progress};
use self::outputs::OutputError;
use self::reapi::google::rpc;
use self::reapi::v2::{
    Action, ActionResult, Command, Digest, ExecuteResponse, Platform, command::EnvironmentVariable,
    platform::Property,
};
use self::toolchain::ToolchainError;
use self::tree::{Entry, Tree, TreeError};
use crate::coordinator_update::public::{self, Get, UreqWeb, Web};

/// The directory of the run's own inputs in the input root, which the checkout must not have.
const OURS: &str = ".mac-farm";

/// Where the toolchain is in the input root.
pub(crate) const TOOLCHAIN: &str = ".mac-farm/toolchain";

/// Where the Cargo home is in the input root.
pub(crate) const CARGO_HOME: &str = ".mac-farm/cargo-home";

/// The target the crates are fetched for, and the Mac's.
pub(crate) const TARGET: &str = "aarch64-apple-darwin";

/// The action's timeout when `--timeout-secs` is not given: under the farm's default limit of
/// 3000 s.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 2400;

/// The Mac's programs a build uses: its C compiler and linker, `git`, `xcrun`.
const MAC_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The entries of a Cargo home's top directory that are not sent: Cargo's locks and its cache
/// tracker, which change on every run, and the installed programs.
const HOME_SKIPPED: [&str; 4] = [
    ".global-cache",
    ".package-cache",
    ".package-cache-mutate",
    "bin",
];

/// The reason of the `ErrorInfo` the farm attaches to a failed job: its output tail.
const OUTPUT_TAIL: &str = "JOB_OUTPUT_TAIL";

/// The most bytes of one toolchain tarball a download reads (rustc's is about 80 MB).
const MAX_TARBALL_BYTES: u64 = 512 << 20;

/// The flags of the verb.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The farm's REAPI address, `grpc://<host>:<port>`. Empty: print a notice and run
    /// nothing.
    #[arg(long)]
    pub(crate) address: String,
    /// The instance name sent on every request. Empty: the empty instance.
    #[arg(long, default_value = "")]
    pub(crate) instance: String,
    /// Whether the address is served over TLS: `true` or `false`.
    #[arg(long, default_value = "")]
    pub(crate) tls: String,
    /// The farm's Mac platform properties, `key=value` separated by commas.
    #[arg(long, default_value = "")]
    pub(crate) properties: String,
    /// A directory of this run's own: the Cargo home of `cargo fetch` is made in it.
    #[arg(long)]
    pub(crate) scratch: PathBuf,
    /// The action's timeout, in seconds.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    /// A file the command writes, relative to the checkout's root, read back into the checkout
    /// at the same path when the command exits 0. Repeated for each file.
    #[arg(long = "output")]
    pub(crate) outputs: Vec<String>,
    /// `NAME=VALUE`, set in the command's environment on the Mac. Repeated for each variable;
    /// the verb's own (`CARGO_HOME`, `PATH`, `RUSTC`, ...) cannot be set.
    #[arg(long = "env")]
    pub(crate) env: Vec<String>,
    /// A file what the run sent, how it ended and the outputs it wrote are appended to, as
    /// Markdown (a job's `GITHUB_STEP_SUMMARY`).
    #[arg(long)]
    pub(crate) summary: Option<PathBuf>,
    /// The roots the toolchain's downloads trust: every certificate of this PEM file.
    #[arg(long, default_value = public::CA_BUNDLE)]
    pub(crate) ca_bundle: PathBuf,
    /// The git program that lists the checkout's files.
    #[arg(long, default_value = "git")]
    pub(crate) git: PathBuf,
    /// The Cargo program that fetches the crates on this machine.
    #[arg(long = "cargo", default_value = "cargo")]
    pub(crate) cargo_program: PathBuf,
    /// The Cargo command to run on the Mac, after `--`: `build --locked --release -p hello`.
    #[arg(last = true, required = true)]
    pub(crate) cargo: Vec<String>,
}

/// The variables of the Command's environment the verb sets itself, which `--env` cannot.
const OWN_ENV: [&str; 7] = [
    "CARGO_HOME",
    "CARGO_INCREMENTAL",
    "CARGO_NET_OFFLINE",
    "CARGO_TERM_COLOR",
    "PATH",
    "RUSTC",
    "RUSTDOC",
];

/// Why the flags do not make a run.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Problem {
    #[error("the address {0:?} is not grpc://<host>:<port>")]
    Address(String),
    #[error("--tls is {0:?}: it must be `true` or `false`")]
    Tls(String),
    #[error("--properties {0:?} is not one or more `key=value` separated by commas")]
    Properties(String),
    #[error("--properties names {0:?} twice")]
    PropertyTwice(String),
    #[error("--timeout-secs is 0")]
    NoTimeout,
    #[error(
        "the Cargo command is empty, or starts with an option ({0:?}): it starts with its subcommand"
    )]
    Cargo(String),
    #[error(
        "--output {0:?} is not a relative path of plain components outside {OURS}, the run's own"
    )]
    Output(String),
    #[error("--output {0:?} is given twice")]
    OutputTwice(String),
    #[error("--env {0:?} is not NAME=VALUE (NAME letters, digits and `_`, not a digit first)")]
    Env(String),
    #[error("--env sets {0}, which the verb sets itself")]
    EnvOwn(String),
    #[error("--env sets {0} twice")]
    EnvTwice(String),
}

/// Why a run failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Usage(#[from] Problem),
    #[error("cannot {what}: {why}")]
    Io { what: String, why: String },
    #[error("the checkout tracks {0}, where the run puts its toolchain and crates")]
    Ours(String),
    #[error("{0} is neither a file nor a symlink")]
    Kind(String),
    #[error("{what} failed: {stderr}")]
    Program { what: String, stderr: String },
    #[error(transparent)]
    Tree(#[from] TreeError),
    #[error(transparent)]
    Toolchain(#[from] ToolchainError),
    #[error(transparent)]
    Farm(#[from] FarmError),
    #[error("the farm did not run `cargo {cargo}` to its end: {code:?}: {message}")]
    NotRun {
        cargo: String,
        code: tonic::Code,
        message: String,
    },
    #[error("the farm answered `cargo {0}` with no result")]
    NoResult(String),
    #[error("`cargo {cargo}` exited {code} on a farm Mac{cached}")]
    Exited {
        cargo: String,
        code: i32,
        cached: &'static str,
    },
    #[error(transparent)]
    Output(#[from] OutputError),
}

fn io(what: String) -> impl FnOnce(std::io::Error) -> Error {
    move |e| Error::Io {
        what,
        why: e.to_string(),
    }
}

/// The checked settings of a run.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The URL of the farm, `http://` or `https://`.
    pub(crate) endpoint: String,
    pub(crate) tls: bool,
    pub(crate) instance: String,
    /// Sorted by key.
    pub(crate) properties: Vec<(String, String)>,
    pub(crate) timeout_secs: u64,
    pub(crate) cargo: Vec<String>,
    /// Sorted, as REAPI wants `output_paths`.
    pub(crate) outputs: Vec<String>,
    /// The `--env` variables, by name.
    pub(crate) env: BTreeMap<String, String>,
}

/// Whether `path` is a relative path of plain components outside [`OURS`].
fn plain_output(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or_default();
    first != OURS
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

/// The properties of `text`, `key=value` separated by commas, sorted by key.
fn properties(text: &str) -> Result<Vec<(String, String)>, Problem> {
    let mut out = BTreeMap::new();
    for pair in text.split(',') {
        let (key, value) = pair
            .split_once('=')
            .map(|(k, v)| (k.trim(), v.trim()))
            .filter(|(k, v)| !k.is_empty() && !v.is_empty())
            .ok_or_else(|| Problem::Properties(text.to_owned()))?;
        if out.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(Problem::PropertyTwice(key.to_owned()));
        }
    }
    Ok(out.into_iter().collect())
}

/// The `--env` variables of `pairs`, by name.
fn environment(pairs: &[String]) -> Result<BTreeMap<String, String>, Problem> {
    let mut out = BTreeMap::new();
    for pair in pairs {
        let (name, value) = pair
            .split_once('=')
            .filter(|(name, _)| {
                name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    && name.bytes().next().is_some_and(|b| !b.is_ascii_digit())
            })
            .ok_or_else(|| Problem::Env(pair.clone()))?;
        if OWN_ENV.contains(&name) {
            return Err(Problem::EnvOwn(name.to_owned()));
        }
        if out.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err(Problem::EnvTwice(name.to_owned()));
        }
    }
    Ok(out)
}

impl Settings {
    /// The settings `args` name.
    ///
    /// # Errors
    /// The [`Problem`] of the first flag that is wrong.
    pub(crate) fn of(args: &Args) -> Result<Settings, Problem> {
        let address = args.address.trim();
        let host = address.strip_prefix("grpc://").unwrap_or_default();
        let port = host.rsplit_once(':').map(|(_, p)| p).unwrap_or_default();
        let plain = !host.contains('/') && !host.chars().any(char::is_whitespace);
        if !plain || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Problem::Address(address.to_owned()));
        }
        let tls = match args.tls.trim() {
            "true" => true,
            "false" => false,
            other => return Err(Problem::Tls(other.to_owned())),
        };
        let properties = properties(&args.properties)?;
        if args.timeout_secs == 0 {
            return Err(Problem::NoTimeout);
        }
        match args.cargo.first() {
            Some(sub) if !sub.starts_with('-') && !sub.starts_with('+') => {}
            first => return Err(Problem::Cargo(first.cloned().unwrap_or_default())),
        }
        let mut outputs = Vec::new();
        for path in &args.outputs {
            if !plain_output(path) {
                return Err(Problem::Output(path.clone()));
            }
            if outputs.contains(path) {
                return Err(Problem::OutputTwice(path.clone()));
            }
            outputs.push(path.clone());
        }
        outputs.sort();
        let scheme = if tls { "https" } else { "http" };
        Ok(Settings {
            endpoint: format!("{scheme}://{host}"),
            tls,
            instance: args.instance.trim().to_owned(),
            properties,
            timeout_secs: args.timeout_secs,
            cargo: args.cargo.clone(),
            outputs,
            env: environment(&args.env)?,
        })
    }
}

/// The action's platform.
fn platform(s: &Settings) -> Platform {
    Platform {
        properties: s
            .properties
            .iter()
            .map(|(name, value)| Property {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
    }
}

/// The Command: the toolchain's `cargo` with the run's arguments, from the input root, its
/// environment (the verb's own and `--env`) sorted by name and its output paths sorted, as
/// REAPI wants both. Its platform is the Action's ([`action`]).
pub(crate) fn command(s: &Settings) -> Command {
    let mut env: BTreeMap<String, String> = [
        ("CARGO_HOME", CARGO_HOME.to_owned()),
        ("CARGO_INCREMENTAL", "0".to_owned()),
        ("CARGO_NET_OFFLINE", "true".to_owned()),
        ("CARGO_TERM_COLOR", "always".to_owned()),
        ("PATH", MAC_PATH.to_owned()),
        ("RUSTC", format!("{TOOLCHAIN}/bin/rustc")),
        ("RUSTDOC", format!("{TOOLCHAIN}/bin/rustdoc")),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect();
    env.extend(s.env.clone());
    Command {
        arguments: std::iter::once(format!("{TOOLCHAIN}/bin/cargo"))
            .chain(s.cargo.iter().cloned())
            .collect(),
        environment_variables: env
            .into_iter()
            .map(|(name, value)| EnvironmentVariable { name, value })
            .collect(),
        output_paths: s.outputs.clone(),
        ..Command::default()
    }
}

/// The Action of `command` over the input root `root`; its blobs go into `tree`.
pub(crate) fn action(s: &Settings, root: &Digest, tree: &mut Tree) -> Digest {
    let command = tree::add(&mut tree.blobs, command(s).encode_to_vec());
    let action = Action {
        command_digest: Some(command),
        input_root_digest: Some(root.clone()),
        timeout: Some(prost_types::Duration {
            seconds: i64::try_from(s.timeout_secs).unwrap_or(i64::MAX),
            nanos: 0,
        }),
        platform: Some(platform(s)),
        ..Action::default()
    };
    tree::add(&mut tree.blobs, action.encode_to_vec())
}

/// The entry of the file or symlink at `path`.
fn entry_at(path: &Path) -> Result<Entry, Error> {
    use std::os::unix::fs::PermissionsExt as _;
    let what = || format!("read {}", path.display());
    let meta = std::fs::symlink_metadata(path).map_err(io(what()))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(io(what()))?;
        return Ok(Entry::Symlink(target.to_string_lossy().into_owned()));
    }
    if !meta.is_file() {
        return Err(Error::Kind(path.display().to_string()));
    }
    let executable = meta.permissions().mode() & 0o111 != 0;
    let bytes = std::fs::read(path).map_err(io(what()))?;
    Ok(Entry::File { bytes, executable })
}

/// The entries of the checkout `root`: each of `tracked` (paths relative to it).
///
/// # Errors
/// [`Error`] for a path that cannot be read, or one in the directory the run's own inputs use.
pub(crate) fn checkout(root: &Path, tracked: &[String]) -> Result<Vec<(String, Entry)>, Error> {
    let mut out = Vec::new();
    for path in tracked {
        if path == OURS || path.starts_with(&format!("{OURS}/")) {
            return Err(Error::Ours(path.clone()));
        }
        out.push((path.clone(), entry_at(&root.join(path))?));
    }
    Ok(out)
}

/// The entries of the Cargo home `home`, below [`CARGO_HOME`], but the top entries of
/// [`HOME_SKIPPED`].
///
/// # Errors
/// [`Error`] for a directory or file that cannot be read.
pub(crate) fn cargo_home(home: &Path) -> Result<Vec<(String, Entry)>, Error> {
    let mut out = Vec::new();
    walk(home, CARGO_HOME, true, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, at: &str, top: bool, out: &mut Vec<(String, Entry)>) -> Result<(), Error> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .and_then(|d| d.map(|e| e.map(|e| e.file_name())).collect())
        .map_err(io(format!("list {}", dir.display())))?;
    names.sort();
    for name in names {
        let name = name.to_string_lossy().into_owned();
        if top && HOME_SKIPPED.contains(&name.as_str()) {
            continue;
        }
        let path = dir.join(&name);
        let inside = format!("{at}/{name}");
        let meta =
            std::fs::symlink_metadata(&path).map_err(io(format!("read {}", path.display())))?;
        if meta.is_dir() {
            walk(&path, &inside, false, out)?;
        } else {
            out.push((inside, entry_at(&path)?));
        }
    }
    Ok(())
}

/// What a run takes from the machine it runs on.
pub(crate) struct Host<'a> {
    /// The paths git tracks in the checkout.
    pub(crate) tracked: &'a mut dyn FnMut() -> Result<Vec<String>, Error>,
    /// Fetches the crates of `Cargo.lock` for [`TARGET`] into the Cargo home it is given.
    pub(crate) fetch: &'a mut dyn FnMut(&Path) -> Result<(), Error>,
    /// The toolchain's pins ([`toolchain::PINS`]).
    pub(crate) pins: toolchain::Pins,
    /// Downloads a toolchain tarball.
    pub(crate) download: &'a mut toolchain::Download<'a>,
}

/// The input root of the run and its action: every blob of both, and the action's digest.
///
/// # Errors
/// [`Error`] for an input that cannot be gathered.
pub(crate) fn inputs(
    s: &Settings,
    root: &Path,
    scratch: &Path,
    host: &mut Host<'_>,
) -> Result<(Tree, Digest), Error> {
    let mut entries = checkout(root, &(host.tracked)()?)?;
    let home = scratch.join("cargo-home");
    std::fs::create_dir_all(&home).map_err(io(format!("create {}", home.display())))?;
    (host.fetch)(&home)?;
    entries.extend(cargo_home(&home)?);
    entries.extend(toolchain::entries(&host.pins, TOOLCHAIN, host.download)?);
    let mut tree = tree::build(entries)?;
    let root = tree.root.clone();
    let action = action(s, &root, &mut tree);
    Ok((tree, action))
}

/// What the farm answered, its stdout and stderr printed into `out` and `err`: the result of a
/// command that exited 0, and whether it came from the farm's cache.
///
/// # Errors
/// [`Error::NotRun`] for a status that is not OK, [`Error::NoResult`] for no result, and
/// [`Error::Exited`] for a non-zero exit.
pub(crate) async fn report(
    farm: &Farm,
    s: &Settings,
    response: ExecuteResponse,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<(ActionResult, bool), Error> {
    let shown = s.cargo.join(" ");
    if let Some(result) = &response.result {
        // A farm may send a small output inline (`stdout_raw`), a copy of the blob its digest
        // names: the inline bytes when there are any, else the blob.
        for (raw, digest, to) in [
            (
                &result.stdout_raw,
                &result.stdout_digest,
                &mut *out as &mut dyn Write,
            ),
            (
                &result.stderr_raw,
                &result.stderr_digest,
                &mut *err as &mut dyn Write,
            ),
        ] {
            if !raw.is_empty() {
                let _ = to.write_all(raw);
            } else if let Some(digest) = digest {
                let bytes = farm.read(digest).await?;
                // A log line that cannot be written must not hide the verdict.
                let _ = to.write_all(&bytes);
            }
        }
    }
    let status = response.status.unwrap_or_default();
    if status.code != 0 {
        let _ = err.write_all(tail(&status).as_bytes());
        return Err(Error::NotRun {
            cargo: shown,
            code: tonic::Code::from_i32(status.code),
            message: status.message,
        });
    }
    let Some(result) = response.result else {
        return Err(Error::NoResult(shown));
    };
    if result.exit_code != 0 {
        return Err(Error::Exited {
            cargo: shown,
            code: result.exit_code,
            cached: if response.cached_result {
                ", a cached result"
            } else {
                ""
            },
        });
    }
    Ok((result, response.cached_result))
}

/// The stdout and stderr tails of a failed job the farm attached to `status`, as text.
fn tail(status: &rpc::Status) -> String {
    let mut text = String::new();
    for detail in &status.details {
        let Ok(info) = rpc::ErrorInfo::decode(detail.value.as_slice()) else {
            continue;
        };
        if info.reason != OUTPUT_TAIL {
            continue;
        }
        for key in ["stdout", "stderr"] {
            if let Some(value) = info.metadata.get(key) {
                text.push_str(&format!("--- the job's {key} (its end) ---\n{value}\n"));
            }
        }
    }
    text
}

/// The line a stage change prints.
pub(crate) fn progress_line(progress: Progress) -> &'static str {
    match progress {
        Progress::Queued => "mac-farm: queued: waiting for a free Mac",
        Progress::Executing => "mac-farm: running on a farm Mac",
    }
}

/// The run against the farm: upload what it lacks, execute, report, and write the outputs into
/// `root`. What it learns on the way is pushed onto `facts`, for the summary.
///
/// # Errors
/// [`Error`], for every end but a command that exited 0 whose outputs were written.
pub(crate) async fn remote(
    s: &Settings,
    root: &Path,
    tree: &Tree,
    action: &Digest,
    streams: (&mut impl Write, &mut impl Write),
    facts: &mut Vec<String>,
) -> Result<Vec<String>, Error> {
    let (out, err) = streams;
    let farm = Farm::connect(&s.endpoint, s.tls, &s.instance).await?;
    let digests: Vec<Digest> = tree
        .blobs
        .iter()
        .map(|(hash, bytes)| Digest {
            hash: hash.clone(),
            size_bytes: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        })
        .collect();
    let missing = farm.missing(&digests).await?;
    let bytes: i64 = missing.iter().map(|d| d.size_bytes).sum();
    let sent = format!(
        "mac-farm: {} blobs, {} missing from the farm ({bytes} bytes to upload)",
        digests.len(),
        missing.len()
    );
    let _ = writeln!(err, "{sent}");
    facts.push(sent);
    farm.upload(&tree.blobs, &missing).await?;
    let response = farm
        .execute(action, &mut |p| eprintln!("{}", progress_line(p)))
        .await?;
    let (result, cached) = report(&farm, s, response, out, err).await?;
    let files = outputs::files(&s.outputs, &result)?;
    let mut lines = vec![format!(
        "ok: `cargo {}` passed on a farm Mac{}",
        s.cargo.join(" "),
        if cached { ", a cached result" } else { "" }
    )];
    lines.extend(outputs::download(&farm, root, &files).await?);
    Ok(lines)
}

/// The Markdown `--summary` appends: the command, what was sent, and the lines of a pass or the
/// failure, with the farm's address left out.
pub(crate) fn summary(
    s: &Settings,
    facts: &[String],
    ended: &Result<Vec<String>, Error>,
) -> String {
    // The summary is a page of its own, outside the log the mask covers: the farm's address, in
    // any failure that names it, is written as `the farm`.
    let host = s.endpoint.split_once("://").map_or("", |(_, h)| h);
    let redact = |line: &str| {
        line.replace(&s.endpoint, "the farm")
            .replace(host, "the farm")
    };
    let mut text = format!("### mac-farm: `cargo {}`\n\n", s.cargo.join(" "));
    let verdict = match ended {
        Ok(lines) => lines.clone(),
        Err(e) => vec![format!("failed: {e}")],
    };
    for line in facts.iter().chain(&verdict) {
        text.push_str(&format!("- {}\n", redact(line)));
    }
    text.push('\n');
    text
}

/// The verb with the machine's inputs given; the command's output is written to `out` and
/// `err`, and its outputs into `root`.
///
/// # Errors
/// [`Error`], for every end but a command that exited 0 on a farm Mac whose outputs were written.
pub(crate) fn run_with(
    args: &Args,
    root: &Path,
    host: &mut Host<'_>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<Vec<String>, Error> {
    // The logs of a public repository's runs are public: keep the farm's address out of what
    // follows, the refusal of a wrong address included.
    let address = args.address.trim();
    let _ = writeln!(out, "::add-mask::{address}");
    let _ = writeln!(out, "::add-mask::{}", address.trim_start_matches("grpc://"));
    let settings = Settings::of(args)?;
    let scratch = std::path::absolute(&args.scratch)
        .map_err(io(format!("resolve {}", args.scratch.display())))?;
    let (tree, action) = inputs(&settings, root, &scratch, host)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(io("start the runtime".to_owned()))?;
    let mut facts = Vec::new();
    let ended = runtime.block_on(remote(
        &settings,
        root,
        &tree,
        &action,
        (out, err),
        &mut facts,
    ));
    if let Some(path) = &args.summary {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(summary(&settings, &facts, &ended).as_bytes()))
            .map_err(io(format!("append to {}", path.display())))?;
    }
    ended
}

/// A toolchain tarball from `url`, through `web`: its bytes, or why not.
pub(crate) fn tarball(web: &dyn Web, url: &str) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let get = Get {
        url,
        accept: "application/octet-stream",
        if_none_match: None,
        limit: Some(MAX_TARBALL_BYTES),
    };
    let head = web.get(&get, &mut body).map_err(|e| e.to_string())?;
    if head.status != 200 {
        return Err(format!("GET {url}: HTTP {}", head.status));
    }
    Ok(body)
}

/// The verb: the lines to print.
///
/// # Errors
/// [`Error`], for every end but a command that exited 0 on a farm Mac whose outputs were written.
pub(crate) fn run(args: &Args) -> Result<Vec<String>, Error> {
    if args.address.trim().is_empty() {
        return Ok(vec![
            "::notice::mac-farm runs nothing: the variable FARM_ADDRESS is not set".to_owned(),
        ]);
    }
    let root = std::env::current_dir().map_err(io("read the working directory".to_owned()))?;
    let web = UreqWeb::new(&public::roots(&args.ca_bundle).map_err(|e| Error::Io {
        what: format!("read the roots of {}", e.what),
        why: e.why,
    })?);
    let mut tracked = || git_files(&args.git, &root);
    let mut fetch = |home: &Path| fetch_crates(&args.cargo_program, &root, home);
    let mut download = |url: &str| tarball(&web, url);
    let mut host = Host {
        tracked: &mut tracked,
        fetch: &mut fetch,
        pins: toolchain::parse(toolchain::PINS)?,
        download: &mut download,
    };
    run_with(
        args,
        &root,
        &mut host,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// The stdout of `process`, named `what`, when it succeeded.
///
/// # Errors
/// [`Error::Io`] when it cannot be started, [`Error::Program`] (with its stderr) when it failed.
pub(crate) fn program(what: &str, process: &mut Process) -> Result<String, Error> {
    let ran = process.output().map_err(io(format!("run {what}")))?;
    if !ran.status.success() {
        return Err(Error::Program {
            what: what.to_owned(),
            stderr: String::from_utf8_lossy(&ran.stderr).trim_end().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
}

/// `<git> ls-files -z` in `root`: the paths git tracks there, NUL-separated.
pub(crate) fn ls_files(git: &Path, root: &Path) -> Process {
    let mut process = Process::new(git);
    process.args(["ls-files", "-z"]).current_dir(root);
    process
}

/// The entries of a NUL-separated list, but the empty ones.
pub(crate) fn split_nul(text: &str) -> Vec<String> {
    text.split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The paths `<git> ls-files -z` lists in `root`.
///
/// # Errors
/// [`Error`] when git cannot be started or fails.
pub(crate) fn git_files(git: &Path, root: &Path) -> Result<Vec<String>, Error> {
    Ok(split_nul(&program(
        "git ls-files",
        &mut ls_files(git, root),
    )?))
}

/// `<cargo> fetch` of the crates of `root`'s lock file for [`TARGET`] into the Cargo home
/// `home`, from `root`.
pub(crate) fn fetch(cargo: &Path, root: &Path, home: &Path) -> Process {
    let mut process = Process::new(cargo);
    process
        .args(["fetch", "--locked", "--target", TARGET])
        .env("CARGO_HOME", home)
        .current_dir(root);
    process
}

/// Runs [`fetch`].
///
/// # Errors
/// [`Error`] when Cargo cannot be started or fails.
pub(crate) fn fetch_crates(cargo: &Path, root: &Path, home: &Path) -> Result<(), Error> {
    program("cargo fetch", &mut fetch(cargo, root, home)).map(drop)
}
