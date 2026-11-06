//! `cargo xtask release <verb>`: every push to `main` that changes code becomes a hostkeeper
//! release, with no tag step and no manual gate. The release workflows run these verbs; each is
//! in `release/<verb>.rs`, and each reads git and GitHub only through the ports of [`ports`].
//!
//! - `version` ([`version`]): the version of a commit, `X.Y.<patch>`: `X.Y` from the `VERSION`
//!   file, the patch counted on the first-parent history of `main`.
//! - `set-version` ([`set_version`]): writes a version into the workspace manifest and the lock
//!   file before a build.
//! - `stamp` and `check-binary` ([`check_binary`]): the `--version` text of a shipped binary
//!   ([`contract::stamp_of`]) and the check that a built binary prints exactly it.
//! - `package` ([`package`]): the deterministic package of a service (`--service`, hostkeeper
//!   unless it says `hello`; for hello `--fail-ready` builds its failing build), `.tar.zst`, and
//!   its SHA-256.
//! - `index` ([`index`]): the release index of a package, `index.json` for hostkeeper,
//!   `hello-index.json` for hello and `hello-fail-ready-index.json` for its failing build: which
//!   package serves each platform, refused unless it has the package of every released platform.
//! - `macos-pkg` and `check-macos-pkg` ([`macos_pkg`]): the installer pkg of a Mac's first
//!   install, and the check of its payload read back from the built pkg.
//! - `compare-images` ([`compare_images`]): two OCI image tarballs are byte for byte equal.
//! - `plan` ([`plan`]): whether a commit (or, on the scheduled path, each missed commit) is
//!   released, and under which version.
//! - `locate` ([`locate`]): the commit is on `main` and its runs exist; the id of its build run.
//! - `check` ([`check`]): every asset against `SHA256SUMS`, both release indexes against the
//!   packages, every attestation and bundle against the signer identity of [`contract`] and the
//!   commit.
//! - `publish` ([`publish`]): the GitHub release of a checked directory, idempotent.
//! - `channel-stage` ([`channel_stage`]): one version of a service from a directory of release
//!   assets, checked and laid out in a directory as the release channel serves it
//!   (`S/V/index.json` beside each package and its bundle); copies files only, idempotent.
//! - `channel-sync` ([`sync`]): every new published release of one or more repositories,
//!   downloaded, checked (`release check` for hostkeeper's, the sums and the repository's own
//!   attestations for another's) and staged into a release channel by `channel-stage`, a new
//!   version renamed into place whole; never changes a version already there.
//!
//! The names and formats a consumer relies on are in [`contract`]. The adapters that run `git`,
//! `gh` and `pkgbuild` are in [`git`], [`github`] and [`macos_pkg`], over the [`ports::Exec`]
//! port; the only code that starts
//! a process is `real::SystemExec`, built in [`real`] alone.

mod channel_stage;
mod check;
mod check_binary;
mod compare_images;
pub(crate) mod contract;
#[cfg(test)]
mod doc_tests;
#[cfg(test)]
pub(crate) mod fakes;
pub(crate) mod git;
mod github;
mod index;
mod locate;
mod macos_pkg;
#[cfg(test)]
mod no_network;
mod package;
mod plan;
pub(crate) mod ports;
mod publish;
pub(crate) mod real;
mod set_version;
mod sync;
#[cfg(feature = "test-support")]
pub mod test_support;
mod version;

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::advisories::DenyCli;
use check_binary::VersionOf;
use contract::{Commit, Package, Platform, Repo, Service, Version};
use git::GitCli;
use github::Gh;
use macos_pkg::PkgbuildCli;
use ports::{Adapters, Exec, PortError, Ports, Run};

/// Why a release verb failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
    /// A port could not answer.
    #[error(transparent)]
    Port(#[from] PortError),
    /// A file could not be read or written.
    #[error("{what}: {why}")]
    Io { what: String, why: String },
    /// The verb refuses, for one reason.
    #[error("{0}")]
    Failed(String),
    /// The verb refuses, for every reason listed (one per line).
    #[error("{}", .0.join("\n"))]
    Refused(Vec<String>),
}

impl Error {
    /// The error for the file operation `what` that failed.
    pub(crate) fn io(what: impl fmt::Display) -> impl FnOnce(std::io::Error) -> Error {
        let what = what.to_string();
        move |e| Error::Io {
            what,
            why: e.to_string(),
        }
    }
}

/// The verbs of `cargo xtask release`.
#[derive(clap::Subcommand)]
pub(crate) enum Verb {
    /// Print the version of a commit: `X.Y` from the version file at that commit, the patch
    /// counted on its first-parent history.
    Version {
        /// The commit, 40 lowercase hex digits.
        #[arg(long)]
        commit: Commit,
        /// The version file, as a path from the repository root.
        #[arg(long, default_value = "VERSION")]
        version_file: String,
        /// A file the `version=` line is appended to (on Actions, `GITHUB_OUTPUT`).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Write the version into `[workspace.package]` of the root `Cargo.toml` and into the
    /// workspace members of `Cargo.lock`.
    SetVersion {
        /// The version, `X.Y.Z`.
        #[arg(long)]
        version: Version,
        /// The repository root.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Print the `--version` text of a binary of this version and commit.
    Stamp {
        #[arg(long)]
        version: Version,
        #[arg(long)]
        commit: Commit,
    },
    /// Run a binary with `--version` and refuse it unless it prints exactly the stamp of this
    /// version and commit: `<file name> X.Y.Z+<12 hex>`, for a file named `hostkeeper` or
    /// `hostkeeper-coordinator`.
    CheckBinary {
        /// The binary to run, under the name it ships with.
        binary: PathBuf,
        /// The version, `X.Y.Z`.
        version: Version,
        /// The commit, 40 lowercase hex digits.
        commit: Commit,
    },
    /// Build the package of a service for one platform, and its `.sha256`.
    Package {
        /// The service: hostkeeper (the default) or hello. Hello's manifest and units are
        /// rendered for the platform, and it builds for every platform; hostkeeper's package for
        /// macOS has no unit.
        #[arg(long, value_enum, default_value_t)]
        service: Service,
        #[arg(long, value_enum)]
        platform: Platform,
        #[arg(long)]
        version: Version,
        /// The commit built; the archive's times are its committer time.
        #[arg(long)]
        commit: Commit,
        /// The service's binary for that platform: `bin/<service>` in the package.
        #[arg(long)]
        binary: PathBuf,
        /// The systemd unit: `units/hostkeeper.service` in the package. Required for a Linux
        /// platform of hostkeeper; refused for its macos-arm64 package, which carries no unit,
        /// and for hello, whose units are rendered.
        #[arg(long)]
        unit: Option<PathBuf>,
        /// Build hello's failing build: version `<version>-fail-ready`, started with
        /// `--fail-ready`, so it never reports ready. Refused for hostkeeper.
        #[arg(long)]
        fail_ready: bool,
        /// The directory the package and its `.sha256` are written to.
        #[arg(long)]
        out: PathBuf,
    },
    /// Write the release index of a service's packages of a version in a directory, one entry
    /// per released platform, refused when the package of any is missing: `index.json` for
    /// hostkeeper, `hello-index.json` for hello and `hello-fail-ready-index.json` for its failing
    /// build.
    Index {
        /// The service: hostkeeper (the default) or hello.
        #[arg(long, value_enum, default_value_t)]
        service: Service,
        /// Index hello's failing build (`hello-fail-ready-index.json`). Refused for hostkeeper.
        #[arg(long)]
        fail_ready: bool,
        #[arg(long)]
        version: Version,
        /// The directory holding the packages; the index is written there.
        #[arg(long)]
        dir: PathBuf,
    },
    /// Build the installer pkg of a Mac's first install with `pkgbuild`: the daemon binary and
    /// its launchd plist, nothing else, and no scripts.
    MacosPkg {
        #[arg(long)]
        version: Version,
        /// The macos-arm64 hostkeeper binary: `/usr/local/bin/hostkeeper` in the pkg.
        #[arg(long)]
        binary: PathBuf,
        /// A directory to lay the pkg's tree out in; it must not exist.
        #[arg(long)]
        stage: PathBuf,
        /// The directory the pkg is written to.
        #[arg(long)]
        out: PathBuf,
    },
    /// Check a pkg that `pkgutil --expand` expanded into a directory: no scripts, and a payload
    /// of exactly the binary and the daemon's plist with their modes, owners and bytes.
    CheckMacosPkg {
        /// The directory `pkgutil --expand` wrote.
        expanded: PathBuf,
        #[arg(long)]
        version: Version,
        /// The binary the pkg must hold.
        #[arg(long)]
        binary: PathBuf,
    },
    /// Refuse two OCI image tarballs unless they are byte for byte equal.
    CompareImages { a: PathBuf, b: PathBuf },
    /// Whether a commit is released, and as which version.
    Plan {
        #[arg(long)]
        repo: Repo,
        /// The pushed commit, or on the scheduled path the tip of `main`.
        #[arg(long)]
        commit: Commit,
        #[arg(long, default_value = "VERSION")]
        version_file: String,
        /// The value of the repository variable `RELEASE_FREEZE`: anything but empty or
        /// `false` stops releases.
        #[arg(long, default_value = "")]
        freeze: String,
        /// Plan every commit since the last release, oldest first, not only `--commit`.
        #[arg(long)]
        scheduled: bool,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Refuse a commit that is not on `main` or lacks a successful push run of each required
    /// workflow; print the id of its build run.
    Locate {
        #[arg(long)]
        repo: Repo,
        #[arg(long)]
        commit: Commit,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Check a directory of assets: the sums, then every attestation online and from its bundle.
    Check {
        dir: PathBuf,
        #[arg(long)]
        version: Version,
        #[arg(long)]
        commit: Commit,
    },
    /// Stage one version of a service from a directory of release assets into a directory laid
    /// out as the release channel serves it (`<out>/S/V/index.json`, each package and its
    /// `.sigstore.json` bundle beside it), after checking the index, every package's size and
    /// SHA-256 and every bundle; nothing is written unless all pass. Files already staged with the
    /// same bytes are left alone; a different file at a staged path is refused. Copies files only:
    /// no upload, no credentials.
    ChannelStage {
        /// The service: a service name of the daemon's grammar.
        #[arg(long, value_parser = hostkeeper_config::names::ServiceName::new)]
        service: hostkeeper_config::names::ServiceName,
        /// The version: a version of the daemon's grammar (`+` allowed).
        #[arg(long, value_parser = hostkeeper_config::names::Version::new)]
        version: hostkeeper_config::names::Version,
        /// The directory of the release's assets: `S-V-<platform>.tar.zst` and its
        /// `.sigstore.json` for each platform.
        #[arg(long)]
        from: PathBuf,
        /// The release index of the version (default: `index.json` in `--from`); staged as
        /// `S/V/index.json`.
        #[arg(long)]
        index: Option<PathBuf>,
        /// The platforms the index must list a package for, comma-separated.
        #[arg(
            long,
            value_delimiter = ',',
            default_values_t = hostkeeper_config::platform::Platform::ALL.to_vec()
        )]
        require_platforms: Vec<hostkeeper_config::platform::Platform>,
        /// The root of the channel's tree; `S/V/` is made under it.
        #[arg(long)]
        out: PathBuf,
    },
    /// Copy every new published release of each repository into the release channel: download
    /// its assets, check them (`release check` for a hostkeeper release; for another
    /// repository's, its SHA256SUMS and the attestation of every asset with a bundle by that
    /// repository's own artifacts.yml on refs/heads/main) and stage each service version it
    /// carries by `channel-stage`. A version already in the channel is left alone; one there with
    /// other bytes is refused, and the run goes on with the others and then fails. Reads public
    /// release assets only.
    ChannelSync {
        /// A repository whose releases are synced, `<owner>/<name>`; repeat it (or list several)
        /// for more, synced in the order given.
        #[arg(long = "repo", required = true, num_args = 1..)]
        repos: Vec<Repo>,
        /// The root of the channel's tree, as `channel-stage --out` names it. It must exist.
        #[arg(long)]
        channel_dir: PathBuf,
        /// Skip every release older than this tag, `v<X.Y.Z>` (the tag itself is synced).
        #[arg(long, value_parser = sync::parse_tag)]
        since: Option<Version>,
        /// Run again and again, this many seconds after each run ends, and never exit (the
        /// sidecar's loop). Without it the verb runs once.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        every_secs: Option<u64>,
    },
    /// Publish a checked directory as the release of the version, at the commit.
    Publish {
        dir: PathBuf,
        #[arg(long)]
        repo: Repo,
        #[arg(long)]
        version: Version,
        #[arg(long)]
        commit: Commit,
    },
}

/// Runs `verb`. The verbs that read no port run without adapters; the others get theirs from
/// `connect`, which is called only for them.
pub(crate) fn run<A: Adapters>(
    verb: Verb,
    connect: impl FnOnce() -> A,
) -> Result<Vec<String>, Error> {
    match verb {
        Verb::SetVersion { version, root } => set_version::set_version(&root, &version),
        Verb::Stamp { version, commit } => Ok(vec![contract::stamp_string(&version, &commit)]),
        Verb::CompareImages { a, b } => compare_images::compare_images(&a, &b),
        Verb::Index {
            service,
            fail_ready,
            version,
            dir,
        } => match Package::of(service, fail_ready) {
            Some(package) => index::write_index(&dir, package, &version),
            None => Err(Error::Failed(package::FAIL_READY_ONLY_HELLO.to_owned())),
        },
        Verb::CheckMacosPkg {
            expanded,
            version,
            binary,
        } => macos_pkg::check(&expanded, &version, &binary),
        Verb::ChannelStage {
            service,
            version,
            from,
            index,
            require_platforms,
            out,
        } => channel_stage::stage(&channel_stage::Request {
            service,
            version,
            from,
            index,
            require: require_platforms,
            out,
        }),
        verb => run_with(verb, &connect().ports()),
    }
}

/// Runs a verb that reads ports.
fn run_with(verb: Verb, ports: &Ports<'_>) -> Result<Vec<String>, Error> {
    match verb {
        Verb::Version {
            commit,
            version_file,
            output,
        } => {
            let v = version::version(ports.git, &commit, &version_file)?;
            answer(output.as_deref(), vec![format!("version={v}")])
        }
        Verb::CheckBinary {
            binary,
            version,
            commit,
        } => check_binary::check_binary(ports.run_version, &binary, &version, &commit),
        Verb::Package {
            service,
            platform,
            version,
            commit,
            binary,
            unit,
            fail_ready,
            out,
        } => package::package(
            ports.git,
            &package::Request {
                service,
                platform,
                version,
                commit,
                binary,
                unit,
                fail_ready,
                out,
            },
        ),
        Verb::MacosPkg {
            version,
            binary,
            stage,
            out,
        } => macos_pkg::build(
            ports.pkgbuild,
            &macos_pkg::Request {
                version,
                binary,
                stage,
                out,
            },
        ),
        Verb::Plan {
            repo,
            commit,
            version_file,
            freeze,
            scheduled,
            output,
        } => plan::plan(
            ports,
            &plan::Request {
                repo,
                commit,
                version_file,
                freeze,
                scheduled,
                output,
            },
        ),
        Verb::Locate {
            repo,
            commit,
            output,
        } => locate::locate(ports.runs, &repo, &commit, output.as_deref()),
        Verb::Check {
            dir,
            version,
            commit,
        } => check::check(ports.verifier, &dir, &version, &commit),
        Verb::Publish {
            dir,
            repo,
            version,
            commit,
        } => publish::publish(ports, &dir, &repo, &version, &commit),
        Verb::ChannelSync {
            repos,
            channel_dir,
            since,
            every_secs,
        } => {
            let request = sync::Request {
                repos,
                channel: channel_dir,
                since,
            };
            match every_secs {
                None => sync::sync(ports.source, ports.verifier, &request),
                Some(secs) => sync::every(
                    ports.source,
                    ports.verifier,
                    &request,
                    &sync::Sleep(std::time::Duration::from_secs(secs)),
                ),
            }
        }
        Verb::SetVersion { .. }
        | Verb::Stamp { .. }
        | Verb::CompareImages { .. }
        | Verb::Index { .. }
        | Verb::CheckMacosPkg { .. }
        | Verb::ChannelStage { .. } => run(verb, || Lent(*ports)),
    }
}

/// Ports already lent, as [`Adapters`]: [`run_with`] hands a verb that reads no port back to
/// [`run`] with them.
struct Lent<'a>(Ports<'a>);

impl Adapters for Lent<'_> {
    fn ports(&self) -> Ports<'_> {
        self.0
    }
}

/// The adapters wired over one [`Exec`]: git, gh (runs, issues, releases and attestations),
/// the `--version` of a binary, `pkgbuild` and `cargo deny`, each running its program through a copy of `exec`. The real
/// ones (`real::connect`) are these over the process-starting `Exec`; a test wires them over a
/// fake one.
struct Wired<E> {
    git: GitCli<E>,
    gh: Gh<E>,
    version_of: VersionOf<E>,
    pkgbuild: PkgbuildCli<E>,
    deny: DenyCli<E>,
}

impl<E: Exec + Clone> Wired<E> {
    fn new(exec: E) -> Wired<E> {
        Wired {
            git: GitCli::new(exec.clone()),
            gh: Gh::new(exec.clone()),
            version_of: VersionOf::new(exec.clone()),
            pkgbuild: PkgbuildCli::new(exec.clone()),
            deny: DenyCli::new(exec),
        }
    }
}

impl<E: Exec> Adapters for Wired<E> {
    fn ports(&self) -> Ports<'_> {
        Ports {
            git: &self.git,
            runs: &self.gh,
            store: &self.gh,
            source: &self.gh,
            verifier: &self.gh,
            run_version: &self.version_of,
            pkgbuild: &self.pkgbuild,
            advisories: &self.deny,
        }
    }
}

/// Appends `lines` (`key=value`) to `output`, when there is one, and returns them to print.
pub(crate) fn answer(output: Option<&Path>, lines: Vec<String>) -> Result<Vec<String>, Error> {
    if let Some(path) = output {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(Error::io(format!("append to {}", path.display())))?;
    }
    Ok(lines)
}

/// The SHA-256 of the file at `path`, 64 lowercase hex digits, read in one streaming pass.
pub(crate) fn file_sha256(path: &Path) -> Result<String, Error> {
    use sha2::Digest;
    let what = format!("read {}", path.display());
    let mut file = std::fs::File::open(path).map_err(Error::io(&what))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(Error::io(&what))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Where a workflow stands for one commit: its latest push run on `main` for that commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunState {
    /// No push run on `main` for the commit.
    Missing,
    /// The latest such run has not completed.
    Pending(u64),
    /// The latest such run succeeded.
    Succeeded(u64),
    /// The latest such run failed (`failure`, `timed_out` or `startup_failure`).
    Failed(u64),
    /// The latest such run completed otherwise (`cancelled`, `skipped`, ...): not green, not
    /// red.
    Other(u64, String),
}

/// The state of a workflow for `commit` from its `runs`: only push runs on `main` whose head is
/// exactly `commit` count, and of those the latest (highest id). A pull-request run, a run on
/// another branch and a run for another commit never count.
pub(crate) fn state(runs: &[Run], commit: &Commit) -> RunState {
    let latest = runs
        .iter()
        .filter(|r| {
            (r.event == "push") & (r.branch == contract::MAIN) & (r.head_sha == commit.as_str())
        })
        .max_by_key(|r| r.id);
    let Some(run) = latest else {
        return RunState::Missing;
    };
    match (run.status.as_str(), run.conclusion.as_deref()) {
        ("completed", Some("success")) => RunState::Succeeded(run.id),
        ("completed", Some("failure" | "timed_out" | "startup_failure")) => {
            RunState::Failed(run.id)
        }
        ("completed", other) => RunState::Other(run.id, other.unwrap_or("none").to_owned()),
        _ => RunState::Pending(run.id),
    }
}

#[cfg(test)]
mod tests;
