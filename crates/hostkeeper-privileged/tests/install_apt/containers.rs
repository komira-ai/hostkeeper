//! The apt of each deb release of `linux-releases.toml`, in a container of its image with no
//! network and no pull, against fixture repositories mounted read only at `/repo`: the
//! `apt-get update` the backend runs, with its options of section 4.2 in a `/work` directory, and
//! the simulation of an install. What each prints is the output the backend's readers
//! ([`parse::update`], [`parse::simulate`]) get.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use hostkeeper_install::repository::apt::{self, parse};
use hostkeeper_install::repository::outcome::Failure;
use hostkeeper_install::repository::{Reason, ToolOutput};
use hostkeeper_testkit::Scratch;

use super::repo::{self, DEFAULT, Key, PACKAGE, Signing};

/// What the container runs before `apt-get`: the work directory of section 4.2 and the entry that
/// names the mounted repository and its keyring.
const PREPARE: &str = "mkdir -p /work/sources /work/empty /work/lists/partial /work/archives/partial \
    && printf 'Types: deb\\nURIs: file:/repo\\nSuites: hk\\nComponents: main\\nSigned-By: /repo/keyring.gpg\\n' \
    > /work/sources/hkaptfix.sources && exec apt-get \"$@\"";

/// How the suite of a repository is wrong, or that it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    /// Signed by the key of the entry's keyring, valid.
    Good,
    /// Signed by another key than the keyring's.
    OtherKey,
    /// Signed by the keyring's key, its `Valid-Until` passed.
    Expired,
    /// No signature at all.
    Unsigned,
}

impl Case {
    /// Every case.
    pub const ALL: [Case; 4] = [Case::Good, Case::OtherKey, Case::Expired, Case::Unsigned];
}

/// The repositories of every case, each in a directory of its own, and the scratch they are in.
pub struct Repos {
    dirs: Vec<(Case, PathBuf)>,
    scratch: Scratch,
    _keys: (Key, Key),
}

impl Repos {
    /// Builds the repository of each [`Case`], its keyring beside its suite as `keyring.gpg`.
    pub fn build(name: &str) -> Repos {
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
            .expect("the scratch directory");
        let base = scratch.path();
        fs::set_permissions(base, fs::Permissions::from_mode(0o755)).expect("scratch mode");
        let entry = Key::new(&base.join("gpg-entry"), "entry");
        let other = Key::new(&base.join("gpg-other"), "other");
        let mut dirs = Vec::new();
        for case in Case::ALL {
            let repo = base.join(format!("repo-{case:?}"));
            fs::create_dir(&repo).expect("make a repository");
            let trees = base.join(format!("trees-{case:?}"));
            fs::create_dir(&trees).expect("make a tree directory");
            let signing = match case {
                Case::Good => Signing::Signed(&entry),
                Case::OtherKey => Signing::Signed(&other),
                Case::Expired => Signing::Expired(&entry),
                Case::Unsigned => Signing::Unsigned,
            };
            repo::publish(&repo, &trees, &DEFAULT, signing);
            entry.export(&repo.join("keyring.gpg"));
            for dir in [&repo, &repo.join("pool"), &repo.join("dists")] {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("mode");
            }
            dirs.push((case, repo));
        }
        Repos {
            dirs,
            scratch,
            _keys: (entry, other),
        }
    }

    /// The directory of `case`'s repository.
    fn dir(&self, case: Case) -> &Path {
        self.dirs
            .iter()
            .find(|(c, _)| *c == case)
            .map(|(_, dir)| dir.as_path())
            .expect("every case was built")
    }

    /// A new, empty work directory named `name`, for a container to see as `/work`.
    pub fn work(&self, name: &str) -> PathBuf {
        let dir = self.scratch.path().join(name);
        fs::create_dir(&dir).expect("make a work directory");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("work mode");
        dir
    }

    /// Runs `apt-get` with the options of section 4.2 for `/work`, then `rest`, in a container of
    /// `image` that sees the repository of `case` and the host directory `work`.
    pub fn apt<I, S>(&self, image: &str, case: Case, work: &Path, rest: I) -> ToolOutput
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let (uid, gid) = (rustix::process::geteuid(), rustix::process::getegid());
        let mut command = Command::new("docker");
        command
            .args(["run", "--rm", "--pull=never", "--network=none", "--user"])
            .arg(format!("{}:{}", uid.as_raw(), gid.as_raw()))
            .args(["-e", "LC_ALL=C", "-v"])
            .arg(format!("{}:/repo:ro", self.dir(case).display()))
            .arg("-v")
            .arg(format!("{}:/work", work.display()))
            .args([image, "sh", "-c", PREPARE, "sh"])
            .args(apt::options(Path::new("/work"), 30))
            .args(rest);
        let out = command
            .output()
            .unwrap_or_else(|e| panic!("run docker: {e}"));
        ToolOutput {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
            cut: false,
        }
    }
}

/// What the backend's reader makes of `apt-get update`'s output.
pub fn update(output: &ToolOutput) -> Result<(), Failure> {
    parse::update(output)
}

/// What the backend's reader makes of the simulated install of `version` of the package.
pub fn simulate(output: &ToolOutput) -> Result<(), Failure> {
    parse::simulate(output, PACKAGE)
}

/// The simulated install of `version`, as the backend runs it.
pub fn simulate_args(version: &str) -> Vec<String> {
    vec![
        "install".to_owned(),
        "--simulate".to_owned(),
        "-y".to_owned(),
        "--allow-downgrades".to_owned(),
        "--allow-change-held-packages".to_owned(),
        format!("{PACKAGE}={version}"),
    ]
}

/// Whether `result` failed with `reason`.
pub fn failed_with(result: &Result<(), Failure>, reason: Reason) -> bool {
    result.as_ref().is_err_and(|f| f.reason() == reason)
}
