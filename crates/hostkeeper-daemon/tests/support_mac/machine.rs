//! The fake Mac: what it is, its files, the hook's kinds and the daemon's report on it.

use super::*;

/// The update account a fake Mac has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Account {
    /// Its file names [`ACCOUNT`], an administrator holding a Secure Token.
    Ready,
    /// No file.
    Missing,
    /// Its file names [`ACCOUNT`], an administrator, who holds no Secure Token.
    NoSecureToken,
}

/// How a fake Mac is set up at first.
#[derive(Clone, Debug)]
pub struct Mac {
    pub account: Account,
    /// `fdesetup status` says FileVault is on.
    pub filevault_on: bool,
    /// Device management sets [`MDM_SETTING`].
    pub mdm_conflict: bool,
    /// [`OLD_XCODE`] is installed and selected.
    pub old_xcode: bool,
    /// The free bytes of the data volume.
    pub free: u64,
    /// `softwareupdate` offers the minor update and the major upgrade; when not, nothing.
    pub offered: bool,
    /// With `offered`, `softwareupdate --list` lists the minor update and the major upgrade; when
    /// not, it lists nothing, and only `--list-full-installers` offers the major upgrade.
    pub minor_listed: bool,
}

impl Default for Mac {
    /// A Mac ready for the daemon's install: the account ready, FileVault off, no device
    /// management, no Xcode, [`FREE`] bytes free, offered the minor update and the major upgrade.
    fn default() -> Mac {
        Mac {
            account: Account::Ready,
            filevault_on: false,
            mdm_conflict: false,
            old_xcode: false,
            free: FREE,
            offered: true,
            minor_listed: true,
        }
    }
}

impl Mac {
    /// A Mac ready for the daemon's install that has nothing to install.
    pub fn up_to_date() -> Mac {
        Mac {
            offered: false,
            ..Mac::default()
        }
    }
}

/// What an install program got: its path on the Mac, its arguments and its standard input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: String,
}

/// What a fake Mac is and does.
pub(super) struct State {
    /// The number of its boot: its boot id is `boot-<n>`.
    pub(super) boot: u32,
    /// What `sw_vers` names.
    pub(super) version: String,
    pub(super) build: String,
    /// What `softwareupdate --list` and `--list-full-installers` print.
    pub(super) list: String,
    pub(super) full: String,
    pub(super) filevault_on: bool,
    pub(super) secure_token: bool,
    pub(super) free: u64,
    /// Whether `softwareupdate --install` fails.
    pub(super) install_fails: bool,
    /// The version and build the Mac comes back on, in place of what it installed.
    pub(super) comes_back_on: Option<(String, String)>,
    /// The version and build the install put on the Mac, which the next boot runs.
    pub(super) installed: Option<(String, String)>,
    /// Whether the Mac asked for a restart that has not happened yet.
    pub(super) restart_asked: bool,
    /// Whether `alpha` is out of service: drained and not resumed.
    pub(super) cordoned: bool,
    /// The developer directory `xcode-select -p` prints; none: none is selected.
    pub(super) selected: Option<String>,
    /// Every change the Mac went through, in order.
    pub(super) effects: Vec<String>,
    /// What each install program got, in order.
    pub(super) inputs: Vec<Input>,
    /// Each line the maintenance hook logged.
    pub(super) logged: Vec<String>,
}

/// A fake Mac: its files under a scratch directory, its programs and its service.
pub struct Machine {
    pub(super) root: Scratch,
    pub(super) state: Mutex<State>,
    /// The coordinator whose Xcode store the hook's stage reads.
    pub(super) store: Mutex<Option<Arc<World>>>,
}

impl Machine {
    /// A Mac named `name` running [`OLD_VERSION`] in the boot `boot-1`, offered [`MINOR_LABEL`]
    /// and the major upgrade to [`MAJOR_VERSION`] unless `mac` says not, set up as `mac` says.
    pub fn new(name: &str, mac: Mac) -> Arc<Machine> {
        let root = Scratch::new(&format!("{name}-mac"));
        let machine = Machine {
            root,
            state: Mutex::new(State {
                boot: 1,
                version: OLD_VERSION.to_owned(),
                build: OLD_BUILD.to_owned(),
                list: if mac.offered && mac.minor_listed {
                    LIST
                } else {
                    NONE
                }
                .to_owned(),
                full: if mac.offered { FULL } else { NONE }.to_owned(),
                filevault_on: mac.filevault_on,
                secure_token: mac.account != Account::NoSecureToken,
                free: mac.free,
                install_fails: false,
                comes_back_on: None,
                installed: None,
                restart_asked: false,
                cordoned: false,
                selected: None,
                effects: Vec::new(),
                inputs: Vec::new(),
                logged: Vec::new(),
            }),
            store: Mutex::new(None),
        };
        fs::create_dir_all(machine.path("/Applications")).expect("/Applications");
        if mac.account != Account::Missing {
            let file = machine.put(
                ACCOUNT_FILE,
                &format!("name={ACCOUNT}\npassword={PASSWORD}\n"),
            );
            fs::set_permissions(file, fs::Permissions::from_mode(0o600)).expect("mode 0600");
        }
        if mac.mdm_conflict {
            machine.put(
                MDM_FILE,
                &format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
                     \t<key>{MDM_SETTING}</key>\n\t<true/>\n</dict>\n</plist>\n"
                ),
            );
        }
        if mac.old_xcode {
            machine.xcode_app(
                &format!("/Applications/Xcode-{OLD_XCODE}.app"),
                OLD_XCODE,
                OLD_XCODE_BUILD,
            );
            machine.state().selected = Some(format!(
                "/Applications/Xcode-{OLD_XCODE}.app/Contents/Developer"
            ));
        }
        Arc::new(machine)
    }

    /// The scratch path of the Mac's path `path`.
    pub fn path(&self, path: &str) -> PathBuf {
        HostRoot::at(self.root.path()).path(path)
    }

    /// `text` written to the Mac's file `path`: the scratch file.
    pub(super) fn put(&self, path: &str, text: &str) -> PathBuf {
        let file = self.path(path);
        fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
        fs::write(&file, text).expect("the file");
        file
    }

    /// An Xcode app of `version` and `build` at the Mac's path `app`, as `xip` expands one: its
    /// `version.plist` and its `xcodebuild`, every directory and the program closed to group and
    /// others' writes.
    pub(super) fn xcode_app(&self, app: &str, version: &str, build: &str) {
        self.put(
            &format!("{app}/Contents/version.plist"),
            &version_plist(version, build),
        );
        let program = self.put(&format!("{app}/Contents/Developer/usr/bin/xcodebuild"), "");
        let top = self.path(app);
        let mut at = program.as_path();
        loop {
            fs::set_permissions(at, fs::Permissions::from_mode(0o755)).expect("mode 0755");
            if at == top {
                break;
            }
            at = at.parent().expect("inside the app");
        }
    }

    /// The Mac's path of the scratch path `path`, or `path` when it is not under the scratch
    /// directory.
    pub(super) fn mac_path(&self, path: &str) -> String {
        let root = self.root.path().to_string_lossy().into_owned();
        match path.strip_prefix(&root) {
            Some(rest) => rest.to_owned(),
            None => path.to_owned(),
        }
    }

    pub(super) fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("not poisoned")
    }

    pub(super) fn effect(&self, line: String) {
        self.state().effects.push(line);
    }

    /// The hook's stage reads the Xcode store of `world`.
    pub(super) fn serve_xcode_store(&self, world: &Arc<World>) {
        *self.store.lock().expect("not poisoned") = Some(Arc::clone(world));
    }

    /// Makes `softwareupdate --install` fail.
    pub fn fail_install(&self) {
        self.state().install_fails = true;
    }

    /// Makes the Mac come back from its restart on `version` (`build`), not on what it installed.
    pub fn come_back_on(&self, version: &str, build: &str) {
        self.state().comes_back_on = Some((version.to_owned(), build.to_owned()));
    }

    /// FileVault is turned on.
    pub fn turn_filevault_on(&self) {
        self.state().filevault_on = true;
    }

    /// Every change the Mac went through, in order.
    pub fn effects(&self) -> Vec<String> {
        self.state().effects.clone()
    }

    /// What each install program got, in order.
    pub fn inputs(&self) -> Vec<Input> {
        self.state().inputs.clone()
    }

    /// Each line the maintenance hook logged.
    pub fn logged(&self) -> Vec<String> {
        self.state().logged.clone()
    }

    /// Whether `alpha` is drained and not resumed.
    pub fn cordoned(&self) -> bool {
        self.state().cordoned
    }

    /// Whether the Mac asked for a restart that has not happened yet.
    pub fn restart_asked(&self) -> bool {
        self.state().restart_asked
    }

    /// The version and build `sw_vers` names.
    pub fn running(&self) -> (String, String) {
        let state = self.state();
        (state.version.clone(), state.build.clone())
    }

    /// The developer directory `xcode-select -p` prints; none: none is selected.
    pub fn selected(&self) -> Option<String> {
        self.state().selected.clone()
    }

    /// The names in the Mac's `/Applications`, sorted.
    pub fn applications(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.path("/Applications"))
            .expect("/Applications")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// The names in the Mac's directory `path`, sorted; none when it is not there.
    pub fn listed(&self, path: &str) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.path(path)) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// The program `path` of the Mac, its runs recorded as effects when `recorded`.
    pub(super) fn program(self: &Arc<Self>, path: &str, recorded: bool) -> Box<dyn Launchctl> {
        Box::new(programs::Program {
            path: path.to_owned(),
            recorded,
            machine: Arc::clone(self),
        })
    }

    /// The owner the update account's file must have: the test's user, as root on a Mac.
    fn owner() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn root(&self) -> HostRoot {
        HostRoot::at(self.root.path())
    }

    /// The `macos` kind on this Mac, keeping its plan in `run_dir`.
    fn macos(self: &Arc<Self>, run_dir: &Path) -> MacosKind {
        let programs =
            || -> Box<dyn Programs> { Box::new(programs::HookPrograms(Arc::clone(self))) };
        MacosKind::new(
            Stage::new(programs(), Box::new(programs::Space(Arc::clone(self)))),
            Apply::new(
                programs(),
                Box::new(programs::HookInstallers(Arc::clone(self))),
                AccountFile::new(&self.path(ACCOUNT_FILE), Self::owner()),
                Some(run_dir),
                self.root(),
                Box::new(programs::HookLog(Arc::clone(self))),
            ),
        )
    }

    /// The `xcode` kind on this Mac.
    fn xcode(self: &Arc<Self>) -> XcodeKind {
        XcodeKind::new(
            xcode::stage::Stage {
                store: Box::new(programs::XcodeStore(Arc::clone(self))),
                runner: Box::new(programs::HookRunner(Arc::clone(self))),
                space: Box::new(programs::Space(Arc::clone(self))),
                plists: Box::new(HostPlists::new(self.root())),
                root: self.root(),
                clock: hostkeeper_daemon::testing::FakeClock::new(),
            },
            xcode::apply::Apply::new(
                Box::new(programs::HookPrograms(Arc::clone(self))),
                Box::new(HostPlists::new(self.root())),
                self.root(),
            ),
        )
    }
}

impl Host for Machine {
    fn platform(&self) -> Option<pb::Platform> {
        Some(pb::Platform::DarwinArm64)
    }

    /// Each hook step is logged by its name before it runs.
    fn hook_started(&self, step: HookStep) {
        let name = match step {
            HookStep::Prepare => "prepare",
            HookStep::Cutover => "cutover",
            HookStep::Verify => "verify",
        };
        self.effect(name.to_owned());
    }

    fn hook_ended(&self, _step: HookStep, _ending: &HookEnding) {}

    /// The kinds of a Mac, as `hostkeeper maintenance-hook` holds them there: the real `macos`
    /// and `xcode` over this Mac, `reboot` the stub.
    fn kinds(self: &Arc<Self>, run_dir: &Path) -> Registry {
        Registry::for_platform(PlatformKind::Macos)
            .with(Kind::Macos, Box::new(self.macos(run_dir)))
            .with(Kind::Xcode, Box::new(self.xcode()))
            .with(
                Kind::Probe,
                Box::new(ProbeKind::new(
                    Box::new(programs::HookPrograms(Arc::clone(self))),
                    Box::new(ProbeClock),
                    Some(run_dir),
                )),
            )
    }

    /// A Mac's: none, as the hook holds it there; the `macos` kind restarts the Mac itself.
    fn rebooter(self: &Arc<Self>) -> Box<dyn Rebooter> {
        Box::new(Unwired)
    }

    /// The Mac collectors, in the order the daemon runs them.
    fn collectors(self: &Arc<Self>) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(Macos::new(MacosPorts {
                sw_vers: self.program(SW_VERS, false),
                ioreg: self.program(IOREG_PROGRAM, false),
                list: self.program(SOFTWAREUPDATE, false),
                full: self.program(SOFTWAREUPDATE, false),
            })),
            Box::new(MacReadiness::new(MacPorts {
                fdesetup: self.program(FDESETUP, false),
                pkgutil: self.program(PKGUTIL_PROGRAM, false),
                space: Box::new(programs::Space(Arc::clone(self))),
                root: self.root(),
            })),
            Box::new(Xcodes::new(
                self.program(XCODE_SELECT, false),
                Box::new(HostPlists::new(self.root())),
                self.root(),
            )),
            Box::new(UpdateAccountFacts::new(
                Box::new(maintenance_account::AccountFile::new(
                    self.path(ACCOUNT_FILE),
                    Self::owner(),
                )),
                self.program(DSCL, false),
                self.program(SYSADMINCTL, false),
            )),
            Box::new(MdmConflicts::new(
                Box::new(HostPlists::new(self.root())),
                self.root(),
            )),
        ]
    }

    /// `alpha`'s `drain` takes it out of service, its `resume` puts it back.
    fn service_step(&self, service: &str, step: &str, _params: &Path) -> i32 {
        let mut state = self.state();
        state.effects.push(format!("{step} {service}"));
        match step {
            "drain" => state.cordoned = true,
            "resume" => state.cordoned = false,
            other => panic!("{service} has no step {other}"),
        }
        0
    }

    /// The restart the Mac asked for: the next boot, running what the install put on it, or what
    /// [`Machine::come_back_on`] said; `softwareupdate` then offers nothing.
    fn boot(&self) -> String {
        let mut state = self.state();
        assert!(state.restart_asked, "the Mac asked for no restart");
        state.restart_asked = false;
        state.boot += 1;
        let back = state.comes_back_on.take();
        let installed = state.installed.take();
        match (back, installed) {
            (Some((version, build)), _) => {
                state.version = version;
                state.build = build;
            }
            (None, Some((version, build))) => {
                state.version = version;
                state.build = build;
                state.list = NONE.to_owned();
                state.full = NONE.to_owned();
            }
            (None, None) => {}
        }
        let id = format!("boot-{}", state.boot);
        state.effects.push(format!("boot {id}"));
        id
    }
}

impl BootIdSource for Machine {
    fn read(&self) -> Result<BootId, BootIdError> {
        BootId::new("the fake Mac", &format!("boot-{}", self.state().boot))
    }
}

/// The probe's clock: [`PROBED_UNIX`].
struct ProbeClock;

impl UnixClock for ProbeClock {
    fn unix_secs(&self) -> u64 {
        PROBED_UNIX
    }
}
