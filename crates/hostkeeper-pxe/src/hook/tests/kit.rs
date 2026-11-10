//! The doubles and fixtures of the hook tests.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_hook::{Class, Outcome, Report, Step};
use hostkeeper_testkit::Scratch;

use crate::hook::{FetchJob, Host, Systemctl, run};
use crate::platform::BootPlatform;
use crate::seal::Seal;
use crate::serve::platforms::linux_of_boot;
use crate::serve::tests::kit::seal_dir;
use crate::testing::scratch;

/// Held by a step while it runs, and by a test that starts a program. A program started while
/// another test's step holds its `flock` would inherit the open directory until it executes,
/// and that step's next run would find the directory locked.
pub static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The version the fake fetch seals.
pub const VERSION: &str = "1.2.3";

/// A `systemctl` that records each call and answers from a queue (0 when it is empty).
#[derive(Default)]
pub struct FakeSystemctl {
    pub calls: RefCell<Vec<Vec<String>>>,
    answers: RefCell<VecDeque<io::Result<i32>>>,
}

impl FakeSystemctl {
    /// A `systemctl` whose next answers are `codes`.
    pub fn answering(codes: &[i32]) -> FakeSystemctl {
        let fake = FakeSystemctl::default();
        fake.answers
            .borrow_mut()
            .extend(codes.iter().map(|c| Ok(*c)));
        fake
    }

    /// A `systemctl` that cannot be run.
    pub fn missing() -> FakeSystemctl {
        let fake = FakeSystemctl::default();
        fake.answers
            .borrow_mut()
            .push_back(Err(io::Error::new(io::ErrorKind::NotFound, "no systemctl")));
        fake
    }

    /// The calls made, each as its words.
    pub fn called(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c.join(" ")).collect()
    }
}

impl Systemctl for FakeSystemctl {
    fn run(&self, args: &[&str]) -> io::Result<i32> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|a| (*a).to_owned()).collect());
        self.answers.borrow_mut().pop_front().unwrap_or(Ok(0))
    }
}

/// The daemon config the host holds.
const DAEMON_CONFIG: &str = "coordinator = \"https://coordinator.example:8443\"\n\
     ca_sha256 = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\
     release_channel = \"https://releases.example\"\n\
     [trusted_signers]\n\
     organisation = \"example-org\"\n\
     workflow = \".github/workflows/artifacts.yml\"\n\
     refs = [\"refs/heads/main\"]\n\
     hosted_runner = true\n\
     [bounds]\n\
     max_package_bytes = 1048576\n\
     fetch_timeout_secs = 2100\n\
     attestation_timeout_secs = 45\n";

/// What a step ended as.
#[derive(Debug)]
pub struct Ended {
    pub code: u8,
    pub out: String,
    pub err: String,
}

impl Ended {
    /// The report on the last line of standard output.
    pub fn report(&self) -> Report {
        Report::from_stdout(&self.out).expect("a report on standard output")
    }

    /// Asserts a failure or deferral of `class` and `reason` with `code`, and the text of
    /// standard error and of the report's detail; returns the detail.
    pub fn refused(&self, code: Outcome, class: Class, reason: &str) -> String {
        assert_eq!(i32::from(self.code), code.code(), "{self:?}");
        let report = self.report();
        assert_eq!(report.class, class, "{self:?}");
        assert_eq!(report.reason.as_deref(), Some(reason), "{self:?}");
        assert!(report.reversible, "{self:?}");
        let detail = report.detail.expect("a detail");
        assert_eq!(self.err.lines().count(), 1, "{self:?}");
        assert!(self.err.ends_with(&format!(": {detail}\n")), "{self:?}");
        detail
    }

    /// Asserts success with nothing on standard output.
    pub fn succeeded(&self) {
        assert_eq!((self.code, self.out.as_str()), (0, ""), "{self:?}");
    }
}

/// A host's files in a scratch directory: the settings file, the daemon config, the CA bundle,
/// and the boot directory (not made).
pub struct Setup {
    pub scratch: Scratch,
    pub owner: u32,
    pub boot: PathBuf,
    pub settings: PathBuf,
}

impl Setup {
    pub fn new(name: &str) -> Setup {
        let scratch = scratch(name);
        let root = scratch.path().to_owned();
        let write = |file: &str, text: &str| {
            let path = root.join(file);
            fs::write(&path, text).expect("write");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
            path
        };
        let config = write("hostkeeper.toml", DAEMON_CONFIG);
        let ca = rcgen::generate_simple_self_signed(vec!["ca.example".to_owned()])
            .expect("a CA certificate");
        let ca = write("ca.pem", &ca.cert.pem());
        let owner = fs::metadata(&config).expect("the config").uid();
        let settings = write("pxe.toml", &settings_text(&root, &config, &ca, ""));
        Setup {
            boot: root.join("boot"),
            scratch,
            owner,
            settings,
        }
    }

    /// The scratch path of `name`.
    pub fn path(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    /// Rewrites the settings file with `extra` after the `[prepare]` header's keys.
    pub fn settings_with(&self, extra: &str) {
        let root = self.scratch.path();
        let text = settings_text(
            root,
            &root.join("hostkeeper.toml"),
            &root.join("ca.pem"),
            extra,
        );
        self.write_settings(&text);
    }

    /// Rewrites the settings file as `text`.
    pub fn write_settings(&self, text: &str) {
        fs::write(&self.settings, text).expect("write the settings");
        fs::set_permissions(&self.settings, fs::Permissions::from_mode(0o600)).expect("mode");
    }

    /// Writes a params file with the package's params, `target` and `previous` versions, and
    /// `extra` lines; returns its path.
    pub fn params(&self, target: Option<&str>, previous: Option<&str>, extra: &str) -> PathBuf {
        let mut text = format!(
            "release = \"https://releases.example/v1.2.3\"\nsettings = \"{}\"\nboot_dir = \"{}\"\nunit = \"pxe.service\"\n",
            self.settings.display(),
            self.boot.display()
        );
        if let Some(version) = target {
            text.push_str(&format!("\"updater.target_version\" = \"{version}\"\n"));
        }
        if let Some(version) = previous {
            text.push_str(&format!(
                "\"updater.previous_version\" = \"{version}\"\n\"updater.previous_release\" = \"/var/lib/hostkeeper/packages/pxe/{version}\"\n"
            ));
        }
        text.push_str(extra);
        self.params_text(&text)
    }

    /// Writes `text` as the params file.
    pub fn params_text(&self, text: &str) -> PathBuf {
        let path = self.path("params.toml");
        fs::write(&path, text).expect("write the params");
        path
    }

    /// A sealed boot set of `platforms` in `<boot>/<version>`, as the fetch makes.
    pub fn seal(&self, version: &str, platforms: &[BootPlatform]) -> Seal {
        let linux: Vec<_> = platforms.iter().map(|p| linux_of_boot(*p)).collect();
        seal_dir(&self.boot.join(version), &linux)
    }

    /// Makes the boot directory and the version directories `versions`, each holding a file.
    pub fn versions(&self, versions: &[&str]) {
        for version in versions {
            fs::create_dir_all(self.boot.join(version)).expect("a version directory");
            fs::write(self.boot.join(version).join("seal.json"), "{}").expect("a file");
        }
    }

    /// What `current` points at, if it is a link.
    pub fn current(&self) -> Option<String> {
        fs::read_link(self.boot.join("current"))
            .ok()
            .map(|t| t.display().to_string())
    }

    /// The names in the boot directory, sorted.
    pub fn listing(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.boot)
            .expect("the boot directory")
            .map(|e| {
                e.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

/// The settings file of a router-mode `serve` and the `[prepare]` table with `extra`.
fn settings_text(root: &Path, config: &Path, ca: &Path, extra: &str) -> String {
    let at = |name: &str| root.join(name).display().to_string();
    format!(
        "[serve]\nmode = \"router\"\naddress = \"192.0.2.10\"\ncoordinator = \"https://192.0.2.1:8444\"\n\
         ca-file = \"{}\"\ntoken-file = \"{}\"\ndaemon-config = \"{}\"\nssh-authorized-keys = \"{}\"\n\
         user-id = 65534\ngroup-id = 65534\n[prepare]\nca-bundle = \"{}\"\n{extra}",
        at("ca.pem"),
        at("token"),
        config.display(),
        at("keys"),
        ca.display(),
    )
}

/// The fetches a test records.
pub type Fetches = RefCell<Vec<(PathBuf, Vec<BootPlatform>, String)>>;

/// A fetch that seals the directory of the job for its platforms as version `version`, and
/// records the directory, the platforms and the release. The directory must not exist when it
/// is called.
pub fn sealing<'a>(
    fetches: &'a Fetches,
    version: &'a str,
) -> impl Fn(&FetchJob, u32) -> Result<Seal, String> + 'a {
    move |job, _| {
        assert!(
            !job.request.dir.exists(),
            "the directory is made by the fetch"
        );
        let platforms: Vec<BootPlatform> = job.request.platforms.iter().copied().collect();
        fetches.borrow_mut().push((
            job.request.dir.clone(),
            platforms.clone(),
            job.request.release.clone(),
        ));
        let linux: Vec<_> = platforms.iter().map(|p| linux_of_boot(*p)).collect();
        let mut seal = seal_dir(&job.request.dir, &linux);
        seal.version = version.to_owned();
        Ok(seal)
    }
}

/// A fetch that must not be called.
pub fn unreached(_: &FetchJob, _: u32) -> Result<Seal, String> {
    panic!("the fetch is not reached")
}

/// Runs `step` with the params file `params`, as `setup`'s owner, over `systemctl` and `fetch`.
pub fn step(
    setup: &Setup,
    step: Step,
    params: &Path,
    systemctl: &FakeSystemctl,
    fetch: &dyn Fn(&FetchJob, u32) -> Result<Seal, String>,
) -> Ended {
    let host = Host {
        owner: setup.owner,
        systemctl,
        fetch,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let _alone = SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let code = run(&host, step, params, None, &mut out, &mut err);
    Ended {
        code,
        out: String::from_utf8(out).expect("UTF-8"),
        err: String::from_utf8(err).expect("UTF-8"),
    }
}

/// Runs a step that uses no fetch.
pub fn plain(setup: &Setup, which: Step, params: &Path, systemctl: &FakeSystemctl) -> Ended {
    step(setup, which, params, systemctl, &unreached)
}
