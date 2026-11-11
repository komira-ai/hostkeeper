//! `cargo xtask <verb>`: the CI steps of this repository, as Rust with tests, so a workflow step
//! is one command.
//!
//! - `install-tool`: check a downloaded tool against its pinned SHA-256 and install it
//!   (`install`).
//! - `release <verb>`: the release steps: the version of a commit, the release plan, the check
//!   and publication of a release's assets, the native packages of the services and the
//!   `targets` that deploy them (`release`).
//! - `advisories`: the security advisories against the dependencies in `Cargo.lock`, kept in
//!   one GitHub issue that it opens, rewrites and closes (`advisories`).
//! - `mdm-signing-test <verb>`: the device test of pkg signing of the MDM design (task M11): the
//!   kit's pkgs, built on a Mac, and the sends of its steps to an enrolled Mac through NanoHUB,
//!   with what each did (`mdm_signing_test`). Not a CI step: an operator runs it
//!   (`docs/operations/pkg-signing-test.md`).
//! - `netinstall-report`: the measurement of network install (item N11 of
//!   `docs/design/network-install.md`): one machine's events from power-on to serving and the
//!   intervals between them, from the times `hostkeeper-pxe` and the coordinator recorded
//!   (`netinstall_report`). The end-to-end job `net-install` runs it; an operator
//!   runs it once on the first real machine (`docs/operations/network-install.md`).
//! - `channels <verb>`: a release's Linux packages (deb and rpm, the daemon's and the
//!   coordinator's) and its macOS installer (signed by the release, not notarized) reaching the repositories at
//!   `packages.komira.ai`: testing first, the install checks, then the decision to promote and
//!   the promotion. Copied from komira-ai/tierstore (`channels`).
//! - `debs`: the Debian packages a CI job installs with `dpkg`, pinned by version and SHA-256 in a
//!   file of `.github/debs/`, kept from the actions cache or fetched from the pinned mirrors in
//!   turn (`debs`), so that no job runs `apt-get`.
//! - `netboot-iso`: the Ubuntu ISO of a network-install job, by its pin in
//!   `packaging/netboot/pins.toml`, from the pinned URL or the mirrors given in turn
//!   (`netboot_iso`).
//! - `db-backup`: one dump of the coordinator's database into a directory of dumps, keeping the
//!   newest of them (`db_backup`). Not a CI step: the backup timer of the compose deployment runs
//!   it (`docs/deploy/coordinator/`).
//!
//! The crate also builds one shipped program, `hostkeeper-coordinator-update`
//! ([`coordinator_update_main`]): the machine that runs the coordinator's container installs the
//! newest attested release by itself and replaces the container, with a rollback to the old one
//! when the new one is not ready (`coordinator_update`). It is no CI step: the update timer of
//! that machine runs it (`docs/deploy/coordinator-update.md`), and each release ships it as an
//! attested tarball for the two Linux platforms, so nothing is built on that machine.
//!
//! The workflows that call them are in `.github/workflows`; `tests/workflows` holds each of
//! them to the rules of this repository. The crate's exports are [`main`], the entry point of
//! `xtask`, and [`coordinator_update_main`], the entry point of `hostkeeper-coordinator-update`;
//! with the feature `test-support`, which only this crate's own tests turn on, it also exports
//! `test_support`, what its integration tests need of its internals.

mod advisories;
mod channels;
mod coordinator_update;
mod db_backup;
mod debs;
mod install;
mod mdm_signing_test;
mod mirrored;
mod netboot_iso;
mod netinstall_report;
mod release;
mod stamp;

#[cfg(feature = "test-support")]
pub use release::test_support;

#[cfg(test)]
mod testdir;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use coordinator_update::public::{self, UreqWeb};

/// Why a verb failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Install(#[from] install::Error),
    /// Flags that do not fit together (exit status 2, like a clap error).
    #[error(transparent)]
    Usage(#[from] install::UnpackFlags),
    #[error(transparent)]
    Release(#[from] release::Error),
    #[error(transparent)]
    Advisories(#[from] advisories::Error),
    #[error(transparent)]
    SigningTest(#[from] mdm_signing_test::Error),
    #[error(transparent)]
    NetinstallReport(#[from] netinstall_report::Error),
    #[error(transparent)]
    DbBackup(#[from] db_backup::Error),
    #[error(transparent)]
    Channels(#[from] channels::Error),
    #[error(transparent)]
    Debs(#[from] debs::Error),
    #[error(transparent)]
    NetbootIso(#[from] netboot_iso::Error),
    /// The roots of `--ca-bundle` could not be read.
    #[error(transparent)]
    CaBundle(#[from] release::ports::PortError),
}

#[derive(Parser)]
#[command(name = "cargo xtask", about = "The CI steps of this repository")]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    /// Check a downloaded file against its pinned SHA-256 and install it (mode 0755).
    InstallTool {
        /// The downloaded file (the workflow's `curl --proto =https` step wrote it, or the
        /// actions cache restored it).
        #[arg(long)]
        download: PathBuf,
        /// The SHA-256 of the download (before unpacking), 64 lowercase hex digits.
        #[arg(long)]
        sha256: install::Sha256,
        /// The file to install; its directory is created when missing.
        #[arg(long)]
        out: PathBuf,
        /// How the download is packed (default: it is the file).
        #[arg(long, value_enum)]
        extract: Option<install::Format>,
        /// The file to take out of a tar (`--extract tar.gz`): its path inside the archive.
        #[arg(long)]
        member: Option<PathBuf>,
    },
    /// The release steps: version, package, plan, check and publish.
    Release {
        #[command(subcommand)]
        verb: release::Verb,
    },
    /// Run `cargo deny check advisories` and keep its findings in one issue: open it, rewrite
    /// its body, or comment and close it when none remain. Exits 0 whatever it finds.
    Advisories {
        /// The repository of the issue, `<owner>/<repo>` (on Actions, `github.repository`).
        #[arg(long)]
        repo: release::contract::Repo,
    },
    /// The device test of pkg signing (MDM design, task M11): build the kit on a Mac, send its
    /// steps to an enrolled Mac through NanoHUB, record and report what each did.
    MdmSigningTest {
        #[command(subcommand)]
        verb: mdm_signing_test::Verb,
    },
    /// The measurement of network install: one machine's events from power-on to serving and
    /// the intervals between them, with the person's wait apart from the machine's time, from
    /// the log of `pxe serve` and the operator API's answers about its request and
    /// host, as canonical JSON. Reads no clock.
    NetinstallReport(netinstall_report::Flags),
    /// One backup of the coordinator's database: run the dump command after `--` with its
    /// output into a new numbered file of `--into` (mode 0600), and keep the newest `--keep`
    /// dumps. A dump that fails or writes nothing is not kept, and removes no older one.
    DbBackup(db_backup::Flags),
    /// A release's Linux packages and macOS installer reaching `packages.komira.ai`: the settings
    /// gate of those jobs, the job's AWS role, the read, merge, sign and write of each
    /// repository, the installer's upload and promotion, the install checks, and the decision to
    /// promote.
    Channels {
        #[command(subcommand)]
        verb: channels::Verb,
    },
    /// Make `--dir` hold exactly the Debian packages the pins file pins for this host's
    /// architecture: keep each file already there whose SHA-256 is its pin's, fetch each missing
    /// one from the pinned mirrors in turn (three rounds), and refuse a file of other bytes or
    /// one no pin names. One line per package.
    Debs {
        /// The pins file (`.github/debs/<set>.toml`).
        #[arg(long)]
        pins: PathBuf,
        /// The directory the actions cache restores and saves; created when missing.
        #[arg(long)]
        dir: PathBuf,
        /// The roots a download trusts: every certificate of this PEM file.
        #[arg(long, default_value = coordinator_update::public::CA_BUNDLE)]
        ca_bundle: PathBuf,
    },
    /// Fetch the ISO of a boot set of network install into `--dir`, by its pin in the pins
    /// file: from the pinned URL, then each `--mirror` in turn (three rounds), its size and
    /// SHA-256 checked. A file of its name and pin already there is kept.
    NetbootIso {
        /// The pins of the boot sets (`packaging/netboot/pins.toml`).
        #[arg(long)]
        pins: PathBuf,
        /// The platform whose ISO is fetched (`linux-x86_64`, `linux-arm64`).
        #[arg(long)]
        platform: String,
        /// The directory the ISO is written into, under the file name of its URL.
        #[arg(long)]
        dir: PathBuf,
        /// A base URL that stands for the pinned URL's `https://<host>`, tried after it.
        #[arg(long)]
        mirror: Vec<String>,
        /// The roots a download trusts: every certificate of this PEM file.
        #[arg(long, default_value = coordinator_update::public::CA_BUNDLE)]
        ca_bundle: PathBuf,
    },
}

fn run(cli: Cli) -> Result<Vec<String>, Error> {
    match cli.verb {
        Verb::InstallTool {
            download,
            sha256,
            out,
            extract,
            member,
        } => {
            let unpack = install::Unpack::new(extract, member)?;
            Ok(vec![install::install_tool(
                &download, &sha256, &unpack, &out,
            )?])
        }
        Verb::Release { verb } => Ok(release::run(verb, release::real::connect)?),
        Verb::Advisories { repo } => {
            let adapters = release::real::connect();
            let ports = release::ports::Adapters::ports(&adapters);
            Ok(advisories::sync(ports.advisories, ports.runs, &repo)?)
        }
        Verb::MdmSigningTest { verb } => Ok(mdm_signing_test::run(
            verb,
            &release::real::exec(),
            mdm_signing_test::NanoHubHttp::new,
        )?),
        Verb::NetinstallReport(flags) => Ok(netinstall_report::run(&flags)?),
        Verb::DbBackup(flags) => Ok(db_backup::run(&flags)?),
        Verb::Channels { verb } => Ok(channels::run(verb, &release::real::exec())?),
        Verb::Debs {
            pins,
            dir,
            ca_bundle,
        } => {
            let arch = debs::Arch::of(std::env::consts::ARCH)?;
            let web = UreqWeb::new(&public::roots(&ca_bundle)?);
            Ok(vec![debs::run(
                &pins,
                arch,
                &dir,
                &web,
                &std::thread::sleep,
            )?])
        }
        Verb::NetbootIso {
            pins,
            platform,
            dir,
            mirror,
            ca_bundle,
        } => {
            let web = UreqWeb::new(&public::roots(&ca_bundle)?);
            Ok(vec![netboot_iso::run(
                &pins,
                &platform,
                &mirror,
                &dir,
                &web,
                &std::thread::sleep,
            )?])
        }
    }
}

/// The entry point of the `xtask` binary.
pub fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("xtask: {e}");
            match e {
                Error::Usage(_) | Error::Channels(channels::Error::Usage(_)) => ExitCode::from(2),
                Error::Install(_)
                | Error::Release(_)
                | Error::Advisories(_)
                | Error::SigningTest(_)
                | Error::NetinstallReport(_)
                | Error::DbBackup(_)
                | Error::Channels(_)
                | Error::Debs(_)
                | Error::NetbootIso(_)
                | Error::CaBundle(_) => ExitCode::FAILURE,
            }
        }
    }
}

/// The command line of `hostkeeper-coordinator-update`: the flags of one run, and `--version`.
#[derive(Parser)]
#[command(
    name = "hostkeeper-coordinator-update",
    version = stamp::version(),
    about = "Roll the coordinator's container forward to the newest published release, on the \
             machine that runs it: download the release's image tarball from GitHub's public \
             URLs (no token), check its sum and its attestation offline with the pinned gh, \
             load it, check what docker loaded, create the new container with the old one's \
             command line, mounts and volumes, and replace the old one once the new one is \
             ready; when it is not, the old one is started again and the version is marked \
             failed. Changes nothing when the running version is at least the target, or when \
             GitHub's limit of requests without a token is used up."
)]
struct UpdateCli {
    #[command(flatten)]
    flags: coordinator_update::Flags,
}

/// One run of `hostkeeper-coordinator-update` with its flags, against GitHub's public URLs, the
/// pinned `gh` offline, `curl` and docker.
fn run_update(cli: &UpdateCli) -> Result<Vec<String>, coordinator_update::Error> {
    use coordinator_update::{offline, public};
    let flags = &cli.flags;
    let roots = public::roots(&flags.ca_bundle)?;
    let web = public::UreqWeb::new(&roots);
    let releases = public::PublicReleases {
        web: &web,
        api: &flags.github_api_url,
        site: &flags.github_url,
        cache: &flags.download_dir,
    };
    let exec = release::real::exec();
    let trust = offline::OwnedByRootOr {
        uid: rustix::process::geteuid().as_raw(),
    };
    let verifier = offline::OfflineGh {
        gh: &flags.gh,
        trusted_root: &flags.trusted_root,
        home: &flags.download_dir.join(coordinator_update::GH_HOME),
        exec: &exec,
        trust: &trust,
    };
    coordinator_update::run(flags, &releases, &verifier, &exec)
}

/// The entry point of the `hostkeeper-coordinator-update` binary: the report lines on stdout and
/// exit 0, or the reason on stderr and exit 1 (2 for flags clap refuses).
pub fn coordinator_update_main() -> ExitCode {
    match run_update(&UpdateCli::parse()) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("hostkeeper-coordinator-update: {e}");
            ExitCode::FAILURE
        }
    }
}
