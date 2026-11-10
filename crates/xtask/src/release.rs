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
//! - `mdm-fetch` ([`mdm_fetch`]): the build-time fetch of the mdm `.deb`'s tools, each pinned archive of
//!   its tools downloaded with `curl` over HTTPS and checked against its pin
//!   (`crates/hostkeeper-mdm-core/pins.txt`), a mismatch kept as `<archive>.rejected`.
//! - `nfpm-stage` ([`package::nfpm_stage`]): the pinned `gh` of one architecture, out of its
//!   release archive checked against its pinned SHA-256, the Sigstore trusted root, and the
//!   static `hostkeeper` of `--binaries`, checked and staged for the
//!   deb and the rpm.
//! - `check-linux-pkg` ([`package::linux_check`]): a built deb, rpm or tarball,
//!   read back, holds no ELF program with an interpreter or a needed library and declares no
//!   dependency; with `--staged` it also installs the staged `gh`, trusted root and daemon at
//!   the daemon's paths, with their modes, root's, and the console's issue link pointing at the
//!   daemon's issue file; with `--service hello`, `--service mdm` or `--service pxe` it reads a
//!   service's `.deb` against what this repository's builder writes instead.
//! - `deb` ([`native::deb`]): the `.deb` of hello, of its failing build, of mdm or of pxe, with the
//!   program at `/usr/lib/<service>/`, the units in `/usr/lib/systemd/system/` and no hook
//!   wrapper (the manifest's `hook.command` names the program).
//! - `netboot` ([`netboot`]): `netboot.json`, the boot sets of network install for the two Linux
//!   platforms, rendered from the committed pins (`packaging/netboot/pins.toml`), which it
//!   refuses unless each platform has its ISO, shim, GRUB, kernel and initrd, each pinned well
//!   formed.
//! - `hkctl` ([`hkctl`]): `hkctl-<version>`, the command line of the coordinator, one bash
//!   script for every platform, generated from the protos' command table.
//! - `macos-pkg` and `check-macos-pkg` ([`macos_pkg`]): the installer pkg of a Mac's first
//!   install, and the check of its payload read back from the built pkg; with `--service hello`,
//!   hello's `.pkg` ([`native::pkg`]), with no script and its jobs disabled.
//! - `compare-images` ([`compare_images`]): two OCI image tarballs are byte for byte equal.
//! - `image-index`, `image-layout` ([`image_index`]): the coordinator's multi-platform image
//!   index, made of the two image tarballs, and the OCI layout a registry push copies from.
//! - `push-image` ([`image_push`]): that layout pushed to the registry and read back by its
//!   digest, byte for byte, an attempt that fails tried again after a wait.
//! - `check-image-ui` ([`check_image_ui`]): the coordinator's image holds the operator web app
//!   (`ui/`) at the path the coordinator serves by default, file for file, owned by root.
//! - `plan` ([`plan`]): which commits since the last release are released, oldest first, and
//!   under which versions.
//! - `locate` ([`locate`]): the commit is on `main` and its runs exist; the id of its build run.
//! - `check` ([`check`]): every asset against `SHA256SUMS`, every attestation and bundle against
//!   the signer identity of [`contract`] and the commit.
//! - `publish` ([`publish`]): the GitHub release of a checked directory, idempotent.
//! - `targets` ([`targets`]): the `targets` of the set that deploys a package of a release, one
//!   native entry per Linux platform the package is released for and nothing else, its manifest
//!   compiled in and its `.deb` read from the release's assets, checked as the coordinator and
//!   the daemon check them, written as one line of JSON for the CI client (`set-desired.yml`,
//!   input `targets`).
//!
//! The names and formats a consumer relies on are in [`contract`]. The adapters that run `git`,
//! `gh`, `pkgbuild`, `productbuild` and `curl` are in [`git`], [`github`], [`macos_pkg`] and
//! [`mdm_fetch`], over the [`ports::Exec`]
//! port; the only code that starts
//! a process is `real::SystemExec`, built in [`real`] alone.

pub(crate) mod check;
mod check_binary;
mod check_image_ui;
pub(crate) mod compare_images;
pub(crate) mod contract;
#[cfg(test)]
mod doc_tests;
#[cfg(test)]
pub(crate) mod fakes;
mod files;
pub(crate) mod git;
mod github;
mod hkctl;
pub(crate) mod image_index;
mod image_push;
mod locate;
mod macos_pkg;
mod mdm_fetch;
pub(crate) mod native;
mod netboot;
#[cfg(test)]
mod no_network;
mod package;
mod pkg_signature;
mod plan;
pub(crate) mod ports;
mod publish;
pub(crate) mod real;
mod run_state;
mod set_version;
mod targets;
#[cfg(feature = "test-support")]
pub mod test_support;
mod version;
mod wired;

use std::fmt;
use std::path::PathBuf;

use contract::{Commit, Package, Platform, Repo, Service, Version};
pub(crate) use files::{answer, file_sha256};
use ports::{Adapters, PortError, Ports};
pub(crate) use run_state::{RunState, state};
use wired::Wired;

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
    /// version and commit: `<file name> X.Y.Z+<12 hex>`, for a file named `hostkeeper`,
    /// `hostkeeper-coordinator`, `hostkeeper-mdm` or `mdm` (the mdm package's program).
    CheckBinary {
        /// The binary to run, under the name it ships with.
        binary: PathBuf,
        /// The version, `X.Y.Z`.
        version: Version,
        /// The commit, 40 lowercase hex digits.
        commit: Commit,
    },
    /// Download the pinned archives of the tools the mdm package carries into a directory, with
    /// `curl` over HTTPS, and refuse any whose SHA-256 is not its pin (kept beside as
    /// `<file>.rejected`). An archive already there with its pin's SHA-256 is kept, not
    /// downloaded again.
    MdmFetch {
        /// The directory the archives go to, made if missing.
        #[arg(long)]
        dir: PathBuf,
    },
    /// Stage the pinned `gh`, the Sigstore trusted root and the static daemon for the deb
    /// and the rpm: `gh` out of its release archive, refused unless the archive
    /// has the pinned SHA-256 and `gh` is a program of the architecture; the trusted root
    /// refused unless each line is one; `hostkeeper` of `--binaries`,
    /// refused unless it is a 64-bit little-endian ELF of the architecture with no
    /// interpreter and no needed library. Writes `<out>/gh`, `<out>/hostkeeper`
    /// (0755) and `<out>/trusted_root.json` (0644) only when all pass.
    NfpmStage {
        /// The architecture of the packages, as nfpm's `ARCH`.
        #[arg(long, value_enum)]
        arch: package::nfpm_stage::Arch,
        /// The pinned version of `gh`, `X.Y.Z`.
        #[arg(long)]
        gh_version: Version,
        /// The downloaded `gh_<version>_linux_<arch>.tar.gz`.
        #[arg(long)]
        gh_archive: PathBuf,
        /// The pinned SHA-256 of that archive, 64 lowercase hex digits.
        #[arg(long)]
        gh_sha256: crate::install::Sha256,
        /// The output of `gh attestation trusted-root`.
        #[arg(long)]
        trusted_root: PathBuf,
        /// The directory holding the static `hostkeeper`
        /// (`target/<musl target>/release`).
        #[arg(long)]
        binaries: PathBuf,
        /// The directory nfpm.yaml takes the files from.
        #[arg(long)]
        out: PathBuf,
    },
    /// Read a built deb, rpm or tarball and refuse it if it holds an ELF program
    /// with an interpreter or a needed library, or (not a tarball) declares a dependency. With
    /// `--staged` (not for a tarball) also refuse it unless it installs `/usr/lib/hostkeeper/gh`,
    /// `/usr/bin/hostkeeper` (a regular file, 0755) and
    /// `/usr/lib/hostkeeper/trusted_root.json` (0644), all root:root, with the bytes of the files
    /// `nfpm-stage` wrote, and the console's issue link.
    ///
    /// With `--service hello`, `mdm` or `pxe` (not with `--staged`), read a service's `.deb`
    /// (`release deb`) instead and refuse it unless its `Package` is the service, every entry is
    /// root's and writable by root alone and its units' programs are files of the package; it
    /// declares no dependency. These are checks of this repository's own build: nothing is
    /// checked about its scripts, paths or unit names.
    CheckLinuxPkg {
        /// The `.deb`, `.rpm` or tarball.
        package: PathBuf,
        /// The directory `nfpm-stage` wrote (its `--out`).
        #[arg(long)]
        staged: Option<PathBuf>,
        /// For the coordinator's deb or rpm instead: the directory its program and `ui/` were
        /// staged in.
        #[arg(long, conflicts_with_all = ["staged", "service"])]
        coordinator_staged: Option<PathBuf>,
        /// The service whose `.deb` this is: hello, mdm or pxe.
        #[arg(long, value_enum, conflicts_with = "coordinator_staged")]
        service: Option<Service>,
    },
    /// Build the `.deb` of hello (and of its failing build, `--fail-ready`), of mdm or of pxe for
    /// one Linux platform: the program at `/usr/lib/<service>/` and the units in
    /// `/usr/lib/systemd/system/` (docs/design/packages-native.md section 10.2). mdm's is built
    /// for linux-x86_64 only, with the pinned tools out of `--archives`; pxe's holds its program and
    /// unit. Writes
    /// `<service>_<version>_<arch>.deb` (the failing build's control: `X.Y.Z~fail.ready`).
    Deb {
        /// The service: hello, mdm or pxe (hostkeeper's `.deb` is nfpm's).
        #[arg(long, value_enum)]
        service: Service,
        #[arg(long, value_enum)]
        platform: Platform,
        #[arg(long)]
        version: Version,
        /// The commit built; every time in the package is its committer time.
        #[arg(long)]
        commit: Commit,
        /// The service's program for that platform.
        #[arg(long)]
        binary: PathBuf,
        /// Hello's failing build: version `<version>-fail-ready`, its units start the program
        /// with `--fail-ready`. Refused for mdm and pxe.
        #[arg(long)]
        fail_ready: bool,
        /// The directory of the pinned archives of mdm's tools, as `release mdm-fetch` leaves
        /// it. Required for mdm; refused for hello and pxe.
        #[arg(long)]
        archives: Option<PathBuf>,
        /// The directory the `.deb` is written to.
        #[arg(long)]
        out: PathBuf,
    },
    /// Write `netboot.json`, the boot sets of network install, from the pins: for each of
    /// linux-x86_64 and linux-arm64 the Ubuntu ISO, shim, network GRUB, kernel and initrd, each
    /// with the name it is served as, its size, its SHA-256 and where it comes from. Pins that
    /// miss a platform, a file or a key, or hold one of the wrong form, are refused, every
    /// problem named, and nothing is written.
    Netboot {
        /// The pins, `packaging/netboot/pins.toml`.
        #[arg(long)]
        pins: PathBuf,
        /// The release the asset belongs to.
        #[arg(long)]
        version: Version,
        /// The directory of the release's assets; `netboot.json` is written there.
        #[arg(long)]
        dir: PathBuf,
    },
    /// Write `hkctl-<version>` (0755), the command line of the coordinator: one bash script
    /// for every platform, generated from the command table of the protos, whose `--version`
    /// prints `hkctl <version>+<first 12 hex digits of the commit>`.
    Hkctl {
        #[arg(long)]
        version: Version,
        /// The commit built.
        #[arg(long)]
        commit: Commit,
        /// The directory of the release's assets, made if missing; the script is written there.
        #[arg(long)]
        out: PathBuf,
    },
    /// Build the installer pkg of a Mac's first install: `pkgbuild` builds a component pkg of the
    /// daemon binary, its launchd plist, its log directory and newsyslog file, the pinned `gh`
    /// and the Sigstore trusted root, nothing else, and one script, a postinstall that loads the
    /// daemon's job when launchd does not have it loaded (it never boots a loaded one out), whose
    /// program is the binary the pkg installs; `productbuild` wraps the component as the unsigned distribution pkg.
    ///
    /// With `--service hello`, hello's `.pkg` instead (`hello-<version>-macos-arm64.pkg`, or of
    /// its failing build with `--fail-ready`): the program under
    /// `/Library/Application Support/hello/`, the two slots' plists in `/Library/LaunchDaemons/`
    /// with `Disabled` true, and no script; `--gh` and `--trusted-root` are refused.
    MacosPkg {
        /// The service: hostkeeper (the default) or hello.
        #[arg(long, value_enum, default_value_t)]
        service: Service,
        /// Hello's failing build. Refused for hostkeeper.
        #[arg(long)]
        fail_ready: bool,
        #[arg(long)]
        version: Version,
        /// The macos-arm64 binary: `/usr/local/bin/hostkeeper` in hostkeeper's pkg,
        /// `/Library/Application Support/hello/bin/hello` in hello's.
        #[arg(long)]
        binary: PathBuf,
        /// The pinned darwin-arm64 `gh`: `/usr/local/lib/hostkeeper/gh` in hostkeeper's pkg.
        /// Required for hostkeeper; refused for hello.
        #[arg(long)]
        gh: Option<PathBuf>,
        /// The Sigstore trusted root, as the pinned `gh attestation trusted-root` prints it:
        /// `/usr/local/lib/hostkeeper/trusted_root.json` in hostkeeper's pkg. Required for
        /// hostkeeper; refused for hello.
        #[arg(long)]
        trusted_root: Option<PathBuf>,
        /// A directory to lay the pkg's tree and its scripts out in, and to write the component
        /// pkg to; it must not exist.
        #[arg(long)]
        stage: PathBuf,
        /// The directory the distribution pkg is written to.
        #[arg(long)]
        out: PathBuf,
    },
    /// Check a distribution pkg that `pkgutil --expand` expanded into a directory: a
    /// `Distribution` that names the one component under the pkg's identifier and version and
    /// runs no installer script, and the component, whose one script is the postinstall
    /// `macos-pkg` writes (byte for byte) and whose payload is exactly the binary, the daemon's
    /// plist, its log directory and newsyslog file, `gh` and the trusted root with their modes,
    /// owners and bytes.
    ///
    /// With `--service hello` (and `--fail-ready` for its failing build), check hello's `.pkg`
    /// instead: one component with no script, whose payload is exactly the program and the two
    /// slots' disabled plists at hello's paths, as its builder writes them; `--gh` and
    /// `--trusted-root` are refused.
    CheckMacosPkg {
        /// The directory `pkgutil --expand` wrote of the distribution pkg.
        expanded: PathBuf,
        /// The service: hostkeeper (the default) or hello.
        #[arg(long, value_enum, default_value_t)]
        service: Service,
        /// Hello's failing build. Refused for hostkeeper.
        #[arg(long)]
        fail_ready: bool,
        #[arg(long)]
        version: Version,
        /// The binary the pkg must hold.
        #[arg(long)]
        binary: PathBuf,
        /// The `gh` hostkeeper's pkg must hold. Required for hostkeeper; refused for hello.
        #[arg(long)]
        gh: Option<PathBuf>,
        /// The trusted root hostkeeper's pkg must hold. Required for hostkeeper; refused for
        /// hello.
        #[arg(long)]
        trusted_root: Option<PathBuf>,
    },
    /// Refuse a signed installer pkg unless `pkgutil --check-signature` says the Mac trusts its
    /// signature, signed with a trusted timestamp, by exactly the identity, Apple's Developer ID
    /// intermediate and Apple's root.
    CheckPkgSignature {
        pkg: PathBuf,
        /// The Developer ID Installer identity that must have signed it.
        #[arg(long)]
        identity: String,
    },
    /// Refuse two OCI image tarballs unless they are byte for byte equal.
    CompareImages { a: PathBuf, b: PathBuf },
    /// Write hostkeeper-coordinator-<version>.image.json into `dir`: the OCI image index
    /// of the two image tarballs of the version there, linux/amd64 then linux/arm64.
    ImageIndex {
        /// The release's files.
        dir: PathBuf,
        /// The release's version.
        #[arg(long)]
        version: Version,
    },
    /// Write the OCI image layout of the coordinator's multi-platform image into the new
    /// directory `out`: the blobs of the two image tarballs in `dir` and the image index there,
    /// which must be theirs, named under the version.
    ImageLayout {
        /// The release's files.
        dir: PathBuf,
        /// The release's version.
        #[arg(long)]
        version: Version,
        /// The layout's directory, which must not exist.
        #[arg(long)]
        out: PathBuf,
        /// An env file (`GITHUB_ENV`) to append `IMAGE_DIGEST=<the index's digest>` to.
        #[arg(long)]
        env_file: Option<PathBuf>,
    },
    /// Push the layout of `image-layout` to the registry, tagged with the version, and refuse
    /// it unless the registry then serves the release's image index by its digest, byte for
    /// byte; an attempt that fails is tried again after a wait.
    PushImage {
        /// The release's files, which hold the image index.
        dir: PathBuf,
        #[arg(long)]
        version: Version,
        /// The layout `image-layout` wrote of those files.
        #[arg(long)]
        layout: PathBuf,
    },
    /// Refuse the coordinator's OCI image tarball unless it holds the operator web app at
    /// /usr/share/hostkeeper-coordinator/ui: exactly the directories and files of `ui`, with
    /// their bytes, owned by root, files 0644 and directories 0755.
    CheckImageUi {
        /// The image tarball.
        image: PathBuf,
        /// The web app's directory, `ui` of the commit.
        ui: PathBuf,
    },
    /// Which commits since the last release are released, oldest first, and as which versions.
    Plan {
        #[arg(long)]
        repo: Repo,
        /// The tip of `main`: every commit from it down to the last release is planned,
        /// oldest first.
        #[arg(long)]
        commit: Commit,
        #[arg(long, default_value = "VERSION")]
        version_file: String,
        /// The value of the repository variable `RELEASE_FREEZE`: anything but empty or
        /// `false` stops releases.
        #[arg(long, default_value = "")]
        freeze: String,
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
    /// Write the `targets` of the set that deploys a package of the release: per platform the
    /// package is released for, its native package alone, whose manifest and `min_hostkeeper`
    /// the verb compiles in: a Linux `.deb`'s `apt` entry of the testing suite (with
    /// `--fail-ready`, hello's failing build's, of `testing-fail` at `X.Y.Z~fail.ready`), the
    /// Mac's signed `.pkg`'s `macos-pkg` entry. Refused, every problem named, unless every
    /// native package and its bundle check out and the entries pass the coordinator's checks.
    /// Prints `targets=<JSON>`.
    Targets {
        /// The service: hostkeeper (the default), hello, mdm or pxe.
        #[arg(long, value_enum, default_value_t)]
        service: Service,
        /// The entries of hello's failing build, asked for only here. Refused for hostkeeper, mdm
        /// and pxe.
        #[arg(long)]
        fail_ready: bool,
        /// The release, `X.Y.Z`: the tag `v<X.Y.Z>` holds the assets.
        #[arg(long)]
        version: Version,
        /// The directory of the release's assets: the package's `.deb`s and their bundles.
        #[arg(long)]
        dir: PathBuf,
        /// The repository of the release, `owner/name`. Read and not used: a native entry names
        /// no release asset. Kept so that the one command line `release.yml` runs is taken by
        /// the xtask of every release commit it may release, older ones included.
        #[arg(long)]
        repo: Repo,
        /// A file the `targets=` line is appended to (on Actions, `GITHUB_OUTPUT`).
        #[arg(long)]
        output: Option<PathBuf>,
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
        Verb::ImageIndex { dir, version } => image_index::write_index(&dir, &version),
        Verb::ImageLayout {
            dir,
            version,
            out,
            env_file,
        } => image_index::write_layout(&dir, &version, &out, env_file.as_deref()),
        Verb::CheckImageUi { image, ui } => check_image_ui::check_image_ui(&image, &ui),
        Verb::Netboot { pins, version, dir } => netboot::write_netboot(&pins, &version, &dir),
        Verb::Hkctl {
            version,
            commit,
            out,
        } => hkctl::write(hostkeeper_proto::ctl::COMMANDS, &version, &commit, &out),
        Verb::NfpmStage {
            arch,
            gh_version,
            gh_archive,
            gh_sha256,
            trusted_root,
            binaries,
            out,
        } => package::nfpm_stage::stage(&package::nfpm_stage::Request {
            arch,
            gh_version,
            gh_archive,
            gh_sha256,
            trusted_root,
            binaries,
            out,
        }),
        Verb::CheckLinuxPkg {
            package,
            staged,
            coordinator_staged,
            service,
        } => match service {
            None => {
                use package::linux_check::Staged;
                let staged = match (&staged, &coordinator_staged) {
                    (Some(dir), _) => Some(Staged::Daemon(dir)),
                    (None, Some(dir)) => Some(Staged::Coordinator(dir)),
                    (None, None) => None,
                };
                package::linux_check::check(&package, staged)
            }
            Some(_) if staged.is_some() => Err(Error::Failed(
                package::linux_check::SERVICE_AND_STAGED.to_owned(),
            )),
            Some(service) => package::linux_check::check_service(&package, service),
        },
        Verb::CheckMacosPkg {
            expanded,
            service,
            fail_ready,
            version,
            binary,
            gh,
            trusted_root,
        } => match native::macos_inputs(service, fail_ready, gh, trusted_root)? {
            None => native::pkg::check(&expanded, &version, fail_ready, &binary),
            Some((gh, trusted_root)) => macos_pkg::check(
                &expanded,
                &version,
                &macos_pkg::Sources {
                    binary,
                    gh,
                    trusted_root,
                },
            ),
        },
        Verb::Targets {
            service,
            fail_ready,
            version,
            dir,
            repo: _,
            output,
        } => match Package::of(service, fail_ready) {
            Some(package) => targets::targets(&targets::Request {
                package,
                version,
                dir,
                output,
            }),
            None => Err(Error::Failed(package::FAIL_READY_ONLY_HELLO.to_owned())),
        },
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
        Verb::MdmFetch { dir } => mdm_fetch::fetch(ports.download, &dir),
        Verb::CheckPkgSignature { pkg, identity } => {
            pkg_signature::check(ports.pkgbuild, &pkg, &identity)
        }
        Verb::MacosPkg {
            service,
            fail_ready,
            version,
            binary,
            gh,
            trusted_root,
            stage,
            out,
        } => match native::macos_inputs(service, fail_ready, gh, trusted_root)? {
            None => native::pkg::build(
                ports.pkgbuild,
                &native::pkg::Request {
                    version,
                    fail_ready,
                    binary,
                    stage,
                    out,
                },
            ),
            Some((gh, trusted_root)) => macos_pkg::build(
                ports.pkgbuild,
                &macos_pkg::Request {
                    version,
                    sources: macos_pkg::Sources {
                        binary,
                        gh,
                        trusted_root,
                    },
                    stage,
                    out,
                },
            ),
        },
        Verb::Deb {
            service,
            platform,
            version,
            commit,
            binary,
            fail_ready,
            archives,
            out,
        } => native::deb::deb(
            ports.git,
            &native::deb::Request {
                service,
                platform,
                version,
                commit,
                binary,
                fail_ready,
                archives,
                out,
            },
        ),
        Verb::Plan {
            repo,
            commit,
            version_file,
            freeze,
            output,
        } => plan::plan(
            ports,
            &plan::Request {
                repo,
                commit,
                version_file,
                freeze,
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
        Verb::PushImage {
            dir,
            version,
            layout,
        } => image_push::push(
            ports.registry,
            ports.wait,
            &image_push::Request {
                dir,
                version,
                layout,
            },
        ),
        Verb::SetVersion { .. }
        | Verb::Stamp { .. }
        | Verb::CompareImages { .. }
        | Verb::ImageIndex { .. }
        | Verb::ImageLayout { .. }
        | Verb::CheckImageUi { .. }
        | Verb::Netboot { .. }
        | Verb::Hkctl { .. }
        | Verb::NfpmStage { .. }
        | Verb::CheckLinuxPkg { .. }
        | Verb::CheckMacosPkg { .. }
        | Verb::Targets { .. } => run(verb, || Lent(*ports)),
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

#[cfg(test)]
mod macos_pkg_verb_tests;
#[cfg(test)]
mod native_verb_tests;
#[cfg(test)]
mod tests;
