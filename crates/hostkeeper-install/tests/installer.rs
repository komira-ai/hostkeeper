//! The installer contract and the registry (packages.md section 5.1): the registry holds exactly
//! the installers this build has the steps of, `hostkeeper-archive` alone; its `stage` is
//! today's unpack, its `activate` and `remove` are the host's switch and unlinking, each asked
//! once and its failure the step's, and its `retire` asks the host nothing.

mod support;

use std::cell::RefCell;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::{
    Host, HostError, REGISTERED, StageEnv, StageError, StageOrder, backend_of, steps_of,
};
use hostkeeper_release::Installer;
use hostkeeper_unpack::{HostFs, Limits, Platform, REASON_DIGEST_MISMATCH};

use support::{SERVICE, Scratch, VERSION, archive, me, sha256_hex};

/// A host that records what it was asked and answers `fail` when set.
#[derive(Default)]
struct Recording {
    asked: RefCell<Vec<String>>,
    fail: bool,
}

impl Recording {
    fn answer(&self, what: String) -> Result<(), HostError> {
        self.asked.borrow_mut().push(what.clone());
        match self.fail {
            true => Err(HostError(format!("{what} failed"))),
            false => Ok(()),
        }
    }
}

impl Host for Recording {
    fn switch(&self, version: &Version) -> Result<(), HostError> {
        self.answer(format!("switch {version}"))
    }

    fn unplace(&self) -> Result<(), HostError> {
        self.answer("unplace".to_owned())
    }
}

fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

/// The registry holds the archive's steps and apt's backend: an installer is registered exactly
/// when it has artifact steps or a repository backend, never both, each naming itself.
#[test]
fn the_registry_holds_the_archive_and_apt() {
    assert_eq!(REGISTERED, [Installer::HostkeeperArchive, Installer::Apt]);
    for installer in Installer::ALL {
        let (steps, backend) = (steps_of(installer), backend_of(installer));
        assert_eq!(
            steps.is_some() | backend.is_some(),
            REGISTERED.contains(&installer),
            "{installer}"
        );
        assert!(!(steps.is_some() & backend.is_some()), "{installer}");
        let repository = installer.is_repository() & REGISTERED.contains(&installer);
        assert_eq!(backend.is_some(), repository, "{installer}");
        if let Some(steps) = steps {
            assert_eq!(steps.installer(), installer);
        }
        if let Some(backend) = backend {
            assert_eq!(backend.installer(), installer);
        }
    }
}

#[test]
fn the_archive_activates_and_removes_through_the_host_and_retires_with_nothing_more() {
    let archive = steps_of(Installer::HostkeeperArchive).expect("the archive's steps");
    let host = Recording::default();
    archive
        .activate(&host, &version("1.4.0"))
        .expect("activate");
    archive
        .activate(&host, &version("1.4.0"))
        .expect("activate again");
    archive.retire(&version("1.2.0")).expect("retire");
    archive.remove(&host).expect("remove");
    assert_eq!(
        *host.asked.borrow(),
        ["switch 1.4.0", "switch 1.4.0", "unplace"]
    );
}

#[test]
fn a_host_that_fails_fails_the_step_with_its_words() {
    let archive = steps_of(Installer::HostkeeperArchive).expect("the archive's steps");
    let host = Recording {
        fail: true,
        ..Recording::default()
    };
    let v = version("1.4.0");
    assert_eq!(
        archive.activate(&host, &v),
        Err(HostError("switch 1.4.0 failed".to_owned()))
    );
    assert_eq!(archive.retire(&v), Ok(()));
    assert_eq!(
        archive.remove(&host),
        Err(HostError("unplace failed".to_owned()))
    );
}

/// Stages `bytes` as `web` at [`VERSION`] whose verified digest is `digest`.
fn stage(
    scratch: &Scratch,
    bytes: &[u8],
    digest: &str,
) -> Result<hostkeeper_install::StagedVersion, StageError> {
    let packages = scratch.path().join("packages");
    std::fs::create_dir_all(&packages).expect("the packages directory");
    let artifact = scratch.path().join("web.tar.zst");
    std::fs::write(&artifact, bytes).expect("the artifact");
    let fs = HostFs::open(&packages, me()).expect("the port");
    let service = ServiceName::new(SERVICE).expect("a name");
    let version = version(VERSION);
    let digest = Sha256Hex::parse(digest).expect("a digest");
    let order = StageOrder {
        service: &service,
        version: &version,
        package_digest: &digest,
        entry: None,
    };
    let env = StageEnv {
        limits: Limits {
            max_unpacked_bytes: 1 << 20,
            max_entries: 100,
        },
        fs: &fs,
        platform: Platform::Linux,
    };
    let steps = steps_of(Installer::HostkeeperArchive).expect("the archive's steps");
    steps.stage(&artifact, &order, env)
}

#[test]
fn the_archive_stages_with_todays_unpack_and_finds_its_version_again() {
    let scratch = Scratch::new("stage");
    let bytes = archive();
    let digest = sha256_hex(&bytes);
    let staged = stage(&scratch, &bytes, &digest).expect("staged");
    assert!(staged.fresh);
    assert!(
        staged.dir.ends_with("web/1.4.0"),
        "{}",
        staged.dir.display()
    );
    assert!(staged.dir.join("bin/web").is_file());
    assert!(staged.dir.join("manifest.toml").is_file());
    let manifest = staged.manifest.as_ref().expect("the archive's manifest");
    assert_eq!(manifest.name.as_str(), SERVICE);
    assert_eq!(manifest.version.as_str(), VERSION);
    let again = stage(&scratch, &bytes, &digest).expect("staged again");
    assert_eq!(again.dir, staged.dir);
    assert!(!again.fresh);
}

#[test]
fn an_archive_the_unpack_refuses_is_the_unpacks_error() {
    let scratch = Scratch::new("stage-refused");
    let bytes = archive();
    let other = sha256_hex(b"other bytes");
    let refused = stage(&scratch, &bytes, &other).expect_err("another digest");
    let StageError::Unpack(error) = &refused;
    assert_eq!(error.outcome().2, REASON_DIGEST_MISMATCH);
    assert_eq!(refused.to_string(), error.to_string());
}
