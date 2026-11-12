//! The `apt` backend of the repository installer (packages-repositories.md sections 4.1 to
//! 4.6): Debian, Ubuntu and their derivatives.
//!
//! Every apt run works in the service's work directory and passes, on its command line (which
//! wins over `/etc/apt/apt.conf.d/`), the options of section 4.2 ([`options`]): the work
//! directory's copy of the one entry as the only source, no trusted key but the entry's
//! `Signed-By` keyring, no preference, the lists and the archives in the work directory and no
//! package cache, no unauthenticated or insecure repository, no recommends or suggests, and the
//! dpkg lock's timeout: the step's bound, but for the install, which waits for the lock up to
//! the installer's `lock_wait_secs` and is bounded by the step's bound and that wait together.
//! Only `apt-get update` and `apt-get download`
//! reach the network, confined to the work directory; every other run is offline.
//!
//! The two runs that run package scripts, the install and the purge, are given the environment
//! of `scripts`: no question asked, and needrestart's apt hook suspended, so that no hook of
//! the package manager restarts a service while the update moves it (the update starts and stops
//! the service's units itself, in its own order). The download writes into the archives
//! directory, which is first given the owner, group and mode apt gave its own download directory,
//! the lists' `partial/` (`sandbox_archives`): apt run as root downloads as its sandbox user
//! (`_apt`) only into a directory that user can write, and otherwise downloads as root.
//!
//! | Work directory              | Version directory's `apt/` |
//! |-----------------------------|----------------------------|
//! | `sources/<repository>.sources` | `<repository>.sources` |
//! | `lists/` (the suite's `InRelease` and `Packages`) | `lists/` |
//! | `archives/<file>.deb`        | `archives/<file>.deb` |
//! | `empty/` (no key, no preference) | |

pub mod parse;
pub mod version;

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_release::Installer;

use super::backend::{Backend, Fields, Installed, Purged, Stanza};
use super::config::Repository;
use super::outcome::{Failure, Reason};
use super::tools::{Run, Tool, ToolOutput, Tools};

/// The directories of a work directory.
pub const SOURCES: &str = "sources";
pub const EMPTY: &str = "empty";
pub const LISTS: &str = "lists";
pub const ARCHIVES: &str = "archives";
pub const PARTIAL: &str = "partial";

/// What apt keeps in its lists directory beside the indexes, which is not copied.
pub const NOT_KEPT: [&str; 3] = ["lock", "partial", "auxfiles"];

/// The `apt` backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct Apt;

/// The options of section 4.2 every apt run in `work` passes, the dpkg lock's timeout
/// `lock_secs`.
pub fn options(work: &Path, lock_secs: u64) -> Vec<String> {
    let w = work.display();
    [
        "Dir::Etc::SourceList=/dev/null".to_owned(),
        format!("Dir::Etc::SourceParts={w}/{SOURCES}"),
        "Dir::Etc::Trusted=/dev/null".to_owned(),
        format!("Dir::Etc::TrustedParts={w}/{EMPTY}"),
        "Dir::Etc::Preferences=/dev/null".to_owned(),
        format!("Dir::Etc::PreferencesParts={w}/{EMPTY}"),
        format!("Dir::State::Lists={w}/{LISTS}"),
        format!("Dir::Cache::Archives={w}/{ARCHIVES}"),
        "Dir::Cache::pkgcache=".to_owned(),
        "Dir::Cache::srcpkgcache=".to_owned(),
        "APT::Get::AllowUnauthenticated=false".to_owned(),
        "Acquire::AllowInsecureRepositories=false".to_owned(),
        "Acquire::AllowDowngradeToInsecureRepositories=false".to_owned(),
        "APT::Install-Recommends=false".to_owned(),
        "APT::Install-Suggests=false".to_owned(),
        format!("DPkg::Lock::Timeout={lock_secs}"),
    ]
    .into_iter()
    .flat_map(|option| ["-o".to_owned(), option])
    .collect()
}

/// `package=version`, as apt names one version.
fn pinned(package: &str, version: &str) -> String {
    format!("{package}={version}")
}

/// An apt run in `work`: the options, then `rest`, the lock waited for within its `bound`.
fn apt(tool: Tool, work: &Path, bound: u64, rest: &[&str]) -> Run {
    with_options(tool, work, bound, bound, rest)
}

/// An apt run in `work` that waits up to `lock_secs` for the dpkg lock on top of its `bound`.
fn locking(tool: Tool, work: &Path, bound: u64, lock_secs: u64, rest: &[&str]) -> Run {
    with_options(tool, work, lock_secs, bound.saturating_add(lock_secs), rest)
}

/// An apt run in `work` bounded by `bound`, whose options wait `lock_secs` for the lock.
fn with_options(tool: Tool, work: &Path, lock_secs: u64, bound: u64, rest: &[&str]) -> Run {
    let mut args = options(work, lock_secs);
    args.extend(rest.iter().map(|s| (*s).to_owned()));
    Run::offline(tool, args, bound)
}

/// `run`, a run that runs package scripts (the install, the purge), with their environment:
/// `DEBIAN_FRONTEND=noninteractive`, so that no script asks a question; `NEEDRESTART_SUSPEND=1`,
/// which needrestart's apt hook (`DPkg::Post-Invoke`, `/usr/lib/needrestart/apt-pinvoke`) reads
/// as "do not run", so it does not restart the services whose binaries the install replaced while
/// the update drains one of them and starts another; and `NEEDRESTART_MODE=l`, so that a
/// needrestart that runs all the same (one that does not read the first) lists what it would
/// restart and restarts nothing.
fn scripts(run: Run) -> Run {
    run.with_env("DEBIAN_FRONTEND", "noninteractive")
        .with_env("NEEDRESTART_SUSPEND", "1")
        .with_env("NEEDRESTART_MODE", "l")
}

/// Gives the archives directory of `work` the owner, group and mode of the lists' `partial/`,
/// which `apt-get update` made its own download directory (as root: its sandbox user `_apt`,
/// group root, mode 0700). `apt-get download` writes into the archives directory, its working
/// directory, and apt downloads as its sandbox user only when that user can write there; else it
/// downloads as root and warns that it does.
fn sandbox_archives(work: &Path) -> io::Result<()> {
    let partial = fs::metadata(work.join(LISTS).join(PARTIAL))?;
    let archives = work.join(ARCHIVES);
    std::os::unix::fs::chown(&archives, Some(partial.uid()), Some(partial.gid()))?;
    fs::set_permissions(
        &archives,
        fs::Permissions::from_mode(partial.mode() & 0o7777),
    )
}

/// Runs `run`, a tool that could not run being the host's failure.
fn run(tools: &dyn Tools, run: &Run) -> Result<ToolOutput, Failure> {
    tools.run(run).map_err(|e| {
        Failure::new(
            Reason::IoError,
            format!("{} could not be run: {e}", run.tool.name()),
        )
    })
}

/// Makes the directories of `dirs`, each with its parents.
fn make_dirs(dirs: &[PathBuf]) -> io::Result<()> {
    dirs.iter()
        .try_for_each(|dir| fs::DirBuilder::new().recursive(true).create(dir))
}

/// Copies every regular file of `from` that is not in [`NOT_KEPT`] into `to`, a link to a regular
/// file as the file's bytes: apt leaves an uncompressed index of a `file:` repository as a link
/// to the repository's own file, which a later `activate` must not need.
fn copy_files(from: &Path, to: &Path) -> io::Result<()> {
    // An entry that cannot be read is not copied, and the install that needs it then fails.
    let mut names: Vec<_> = fs::read_dir(from)?
        .flatten()
        .filter(|e| fs::metadata(e.path()).is_ok_and(|m| m.is_file()))
        .map(|e| e.file_name())
        .filter(|n| !NOT_KEPT.iter().any(|k| n == *k))
        .collect();
    names.sort();
    for name in names {
        fs::copy(from.join(&name), to.join(&name))?;
    }
    Ok(())
}

impl Backend for Apt {
    fn installer(&self) -> Installer {
        Installer::Apt
    }

    fn check_names(&self, package: &str, version: &str) -> Result<(), String> {
        if !version::is_package(package) {
            return Err(format!(
                "the package {package:?} is not a dpkg package name"
            ));
        }
        match version::is_version(version) {
            true => Ok(()),
            false => Err(format!(
                "the package version {version:?} is not a dpkg version"
            )),
        }
    }

    fn architecture(&self, tools: &dyn Tools, bound: u64) -> Result<String, Failure> {
        let call = Run::offline(Tool::Dpkg, vec!["--print-architecture".to_owned()], bound)
            .writing(Vec::new());
        let output = run(tools, &call)?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let named = !text.is_empty()
            && text
                .bytes()
                .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | (b == b'-'));
        match output.succeeded() & named {
            true => Ok(text),
            false => Err(Failure::new(
                Reason::IoError,
                format!("dpkg --print-architecture: {}", output.last_line()),
            )),
        }
    }

    fn prepare(&self, work: &Path, repository: &Repository) -> io::Result<()> {
        make_dirs(&[
            work.join(SOURCES),
            work.join(EMPTY),
            work.join(LISTS).join(PARTIAL),
            work.join(ARCHIVES).join(PARTIAL),
        ])?;
        fs::write(
            work.join(SOURCES)
                .join(format!("{}.sources", repository.name)),
            &repository.text,
        )
    }

    fn update(&self, tools: &dyn Tools, work: &Path, bound: u64) -> Result<(), Failure> {
        let call = apt(Tool::AptGet, work, bound, &["update", "--error-on=any"])
            .fetching()
            .writing(vec![work.to_path_buf()]);
        parse::update(&run(tools, &call)?)
    }

    fn stanza(
        &self,
        tools: &dyn Tools,
        work: &Path,
        package: &str,
        version: &str,
        architecture: &str,
        bound: u64,
    ) -> Result<Option<Stanza>, Failure> {
        let one = pinned(package, version);
        let call =
            apt(Tool::AptCache, work, bound, &["show", &one]).writing(vec![work.to_path_buf()]);
        parse::stanza(&run(tools, &call)?, package, version, architecture)
    }

    fn simulate(
        &self,
        tools: &dyn Tools,
        work: &Path,
        package: &str,
        version: &str,
        bound: u64,
    ) -> Result<(), Failure> {
        let one = pinned(package, version);
        let rest = [
            "install",
            "--simulate",
            "-y",
            "--allow-downgrades",
            "--allow-change-held-packages",
            &one,
        ];
        let call = apt(Tool::AptGet, work, bound, &rest).writing(vec![work.to_path_buf()]);
        parse::simulate(&run(tools, &call)?, package)
    }

    fn download(
        &self,
        tools: &dyn Tools,
        work: &Path,
        package: &str,
        version: &str,
        bound: u64,
    ) -> Result<PathBuf, Failure> {
        let one = pinned(package, version);
        let archives = work.join(ARCHIVES);
        sandbox_archives(work).map_err(|e| {
            Failure::new(
                Reason::IoError,
                format!("the archives directory could not be given apt's download owner: {e}"),
            )
        })?;
        let call = apt(Tool::AptGet, work, bound, &["download", &one])
            .fetching()
            .writing(vec![work.to_path_buf()])
            .in_dir(archives.clone());
        parse::download(&run(tools, &call)?)?;
        let debs: Vec<PathBuf> = fs::read_dir(&archives)
            .and_then(|listing| listing.collect::<io::Result<Vec<_>>>())
            .map_err(|e| Failure::new(Reason::IoError, format!("the archives directory: {e}")))?
            .into_iter()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "deb") && p.is_file())
            .collect();
        match debs.as_slice() {
            [one] => Ok(one.clone()),
            many => Err(Failure::new(
                Reason::FetchFailed,
                format!(
                    "apt-get download left {} package files, not one",
                    many.len()
                ),
            )),
        }
    }

    fn fields(&self, tools: &dyn Tools, file: &Path, bound: u64) -> Result<Fields, Failure> {
        let args = vec![
            "--field".to_owned(),
            file.display().to_string(),
            "Package".to_owned(),
            "Version".to_owned(),
            "Architecture".to_owned(),
        ];
        let call = Run::offline(Tool::DpkgDeb, args, bound).writing(Vec::new());
        parse::fields(&run(tools, &call)?)
    }

    fn extract(
        &self,
        tools: &dyn Tools,
        file: &Path,
        tree: &Path,
        bound: u64,
    ) -> Result<(), Failure> {
        let args = vec![
            "--extract".to_owned(),
            file.display().to_string(),
            tree.display().to_string(),
        ];
        let call = Run::offline(Tool::DpkgDeb, args, bound).writing(vec![tree.to_path_buf()]);
        let output = run(tools, &call)?;
        match output.succeeded() {
            true => Ok(()),
            false => Err(Failure::new(
                Reason::ManifestInvalid,
                format!("the package cannot be extracted: {}", output.last_line()),
            )),
        }
    }

    fn meta_dir(&self) -> &'static str {
        "apt"
    }

    fn keep(&self, work: &Path, file: &Path, meta: &Path, repository: &str) -> io::Result<()> {
        make_dirs(&[meta.join(LISTS), meta.join(ARCHIVES)])?;
        let sources = format!("{repository}.sources");
        fs::copy(work.join(SOURCES).join(&sources), meta.join(&sources))?;
        copy_files(&work.join(LISTS), &meta.join(LISTS))?;
        let name = file
            .file_name()
            .ok_or_else(|| io::Error::other("the package file has no name"))?;
        fs::copy(file, meta.join(ARCHIVES).join(name))?;
        Ok(())
    }

    fn restore(&self, meta: &Path, work: &Path) -> io::Result<()> {
        make_dirs(&[
            work.join(SOURCES),
            work.join(EMPTY),
            work.join(LISTS).join(PARTIAL),
            work.join(ARCHIVES).join(PARTIAL),
        ])?;
        copy_files(meta, &work.join(SOURCES))?;
        copy_files(&meta.join(LISTS), &work.join(LISTS))?;
        copy_files(&meta.join(ARCHIVES), &work.join(ARCHIVES))
    }

    fn install(
        &self,
        tools: &dyn Tools,
        work: &Path,
        package: &str,
        version: &str,
        bound: u64,
        lock_wait_secs: u64,
    ) -> Result<(), Failure> {
        let one = pinned(package, version);
        let rest = [
            "-o",
            "Acquire::Check-Valid-Until=false",
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
            "install",
            "-y",
            "--no-download",
            "--allow-downgrades",
            "--allow-change-held-packages",
            &one,
        ];
        let call = scripts(locking(Tool::AptGet, work, bound, lock_wait_secs, &rest));
        parse::install(&run(tools, &call)?)
    }

    fn installed(
        &self,
        tools: &dyn Tools,
        package: &str,
        bound: u64,
    ) -> Result<Option<Installed>, Failure> {
        let args = vec![
            "-W".to_owned(),
            "-f=${Version}\\t${db:Status-Abbrev}\\n".to_owned(),
            package.to_owned(),
        ];
        let call = Run::offline(Tool::DpkgQuery, args, bound).writing(Vec::new());
        parse::installed(&run(tools, &call)?)
    }

    fn hold(
        &self,
        tools: &dyn Tools,
        package: &str,
        hold: bool,
        bound: u64,
    ) -> Result<(), Failure> {
        let verb = if hold { "hold" } else { "unhold" };
        let call = Run::offline(
            Tool::AptMark,
            vec![verb.to_owned(), package.to_owned()],
            bound,
        );
        parse::hold(&run(tools, &call)?)
    }

    fn purge(&self, tools: &dyn Tools, package: &str, bound: u64) -> Result<Purged, Failure> {
        let call = scripts(Run::offline(
            Tool::Dpkg,
            vec!["--purge".to_owned(), package.to_owned()],
            bound,
        ));
        let output = run(tools, &call)?;
        match parse::purge(&output)? {
            true => Ok(Purged::Done),
            false => Ok(Purged::Busy(output.last_line())),
        }
    }
}
