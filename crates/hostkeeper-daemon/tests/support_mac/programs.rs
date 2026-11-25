//! The fake Mac's programs, as the maintenance hook's ports and the report's collectors run them:
//! what each answers and what it changes on the Mac.

use super::*;

/// The program `path` of a Mac, its runs recorded as the Mac's effects when `recorded` (the
/// maintenance hook's), not when not (the daemon's report).
pub(super) struct Program {
    pub(super) path: String,
    pub(super) recorded: bool,
    pub(super) machine: Arc<Machine>,
}

impl Launchctl for Program {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        if self.recorded {
            self.machine.effect(line(&self.path, args));
        }
        self.machine.answer(&self.path, args)
    }
}

/// `program` run with `args`, as an effect line.
fn line(program: &str, args: &[String]) -> String {
    let mut words = vec![program.to_owned()];
    words.extend(args.iter().cloned());
    words.join(" ")
}

/// The exit code of `xcode-select -p` when no developer directory is selected.
const NONE_SELECTED: i32 = 2;

impl Machine {
    /// What the program `path` answers to `args`, and what it changes on the Mac.
    fn answer(&self, path: &str, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let ok = |text: &str| Ok(LaunchctlOutput::success(text));
        match (path, words.as_slice()) {
            (SW_VERS, []) => {
                let (version, build) = self.running();
                ok(&format!(
                    "ProductName:\t\tmacOS\nProductVersion:\t\t{version}\nBuildVersion:\t\t{build}\n"
                ))
            }
            (IOREG_PROGRAM, _) if args == ioreg_args() => ok(IOREG),
            (SOFTWAREUPDATE, ["--list"]) => ok(&self.state().list.clone()),
            (SOFTWAREUPDATE, ["--list-full-installers"]) => ok(&self.state().full.clone()),
            (SOFTWAREUPDATE, ["--download", label]) if *label == MINOR_LABEL => {
                ok(&format!("Downloaded: {label}\n"))
            }
            (
                SOFTWAREUPDATE,
                [
                    "--fetch-full-installer",
                    "--full-installer-version",
                    MAJOR_VERSION,
                ],
            ) => {
                self.put(
                    &format!("{INSTALLER_APP}/Contents/Resources/startosinstall"),
                    "",
                );
                ok("Install finished successfully\n")
            }
            (FDESETUP, ["status"]) => match self.state().filevault_on {
                true => ok("FileVault is On.\n"),
                false => ok("FileVault is Off.\n"),
            },
            (FDESETUP, ["supportsauthrestart"]) => ok("true\n"),
            (FDESETUP, ["list"]) => ok(FILEVAULT_USERS),
            (PKGUTIL_PROGRAM, ["--pkg-info=com.apple.pkg.CLTools_Executables"]) => ok(PKGUTIL),
            (DSCL, [".", "-read", user, "RecordName", "IsHidden"])
                if *user == format!("/Users/{ACCOUNT}") =>
            {
                ok(&format!("RecordName: {ACCOUNT}\nIsHidden: 1\n"))
            }
            (DSCL, [".", "-read", "/Groups/admin", "GroupMembership"]) => {
                ok(&format!("GroupMembership: root {ACCOUNT}\n"))
            }
            (SYSADMINCTL, ["-secureTokenStatus", user]) if *user == ACCOUNT => {
                let state = match self.state().secure_token {
                    true => "ENABLED",
                    false => "DISABLED",
                };
                Ok(LaunchctlOutput {
                    code: Some(0),
                    stdout: Vec::new(),
                    stderr: format!(
                        "sysadminctl[512:4096] Secure token is {state} for user {ACCOUNT}\n"
                    )
                    .into_bytes(),
                })
            }
            (XCODE_SELECT, ["-p"]) => match self.selected() {
                Some(dir) => ok(&format!("{dir}\n")),
                None => Ok(LaunchctlOutput::exit(
                    NONE_SELECTED,
                    "xcode-select: error: unable to get active developer directory\n",
                )),
            },
            (XCODE_SELECT, ["-s", app]) => {
                self.state().selected = Some(format!("{app}/Contents/Developer"));
                ok("")
            }
            (program, ["-license", "accept" | "check"] | ["-runFirstLaunch"])
                if self.xcode_of(program).is_some() =>
            {
                ok("")
            }
            (program, ["-version"]) if self.xcode_of(program).is_some() => {
                let (version, build) = self.xcode_of(program).unwrap_or_default();
                ok(&format!("Xcode {version}\nBuild version {build}\n"))
            }
            _ => Err(LaunchctlError::Spawn {
                args: args.join(" "),
                why: format!("the fake Mac has no {path} that takes these arguments"),
            }),
        }
    }

    /// The version and build of the Xcode whose `xcodebuild` is `program`, from its
    /// `version.plist`; none when there is no such Xcode on the Mac.
    fn xcode_of(&self, program: &str) -> Option<(String, String)> {
        let app = program.strip_suffix("/Contents/Developer/usr/bin/xcodebuild")?;
        let text = fs::read_to_string(self.path(&format!("{app}/Contents/version.plist"))).ok()?;
        let value = |key: &str| -> Option<String> {
            let after = text.split(&format!("<key>{key}</key>")).nth(1)?;
            let start = after.find("<string>")? + "<string>".len();
            let end = after.find("</string>")?;
            Some(after[start..end].to_owned())
        };
        Some((
            value("CFBundleShortVersionString")?,
            value("ProductBuildVersion")?,
        ))
    }

    /// The install program `program` run with `args` and `stdin`: recorded, and what it changes
    /// on the Mac. `softwareupdate --install` puts the minor update on the Mac, and restarts it
    /// with `--restart`; `startosinstall` puts the major upgrade on the Mac and restarts it;
    /// `fdesetup authrestart` restarts it.
    fn install(&self, program: &str, args: &[String], stdin: &[u8]) -> LaunchctlOutput {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut state = self.state();
        state.effects.push(line(program, args));
        state.inputs.push(Input {
            program: program.to_owned(),
            args: args.to_vec(),
            stdin: String::from_utf8_lossy(stdin).into_owned(),
        });
        match (program, words.as_slice()) {
            (SOFTWAREUPDATE, ["--install", ..]) if state.install_fails => {
                LaunchctlOutput::exit(1, "Error installing updates.\n")
            }
            (SOFTWAREUPDATE, ["--install", label, ..]) if *label == MINOR_LABEL => {
                state.installed = Some((MINOR_VERSION.to_owned(), MINOR_BUILD.to_owned()));
                state.restart_asked |= words.contains(&"--restart");
                LaunchctlOutput::success("")
            }
            (startosinstall, _)
                if startosinstall
                    == format!("{INSTALLER_APP}/Contents/Resources/startosinstall") =>
            {
                state.installed = Some((MAJOR_VERSION.to_owned(), MAJOR_BUILD.to_owned()));
                state.restart_asked = true;
                LaunchctlOutput::success("")
            }
            (FDESETUP, ["authrestart", "-inputplist"]) => {
                state.restart_asked = true;
                LaunchctlOutput::success("")
            }
            _ => LaunchctlOutput::exit(64, "the fake Mac has no such install\n"),
        }
    }

    /// `xip --expand <archive>` in `cwd`: the Xcode of the store whose bytes the archive holds,
    /// expanded there as `Xcode.app`.
    fn expand(&self, archive: &Path, cwd: &Path) -> Ran {
        let bytes = fs::read(archive).expect("the archive the stage fetched");
        if bytes != NEW_XCODE_ARCHIVE {
            return Ran {
                code: Some(1),
                timed_out: false,
                stderr: b"xip: error: the archive is not an Xcode archive\n".to_vec(),
            };
        }
        let app = self.mac_path(&cwd.join("Xcode.app").to_string_lossy());
        self.xcode_app(&app, NEW_XCODE, NEW_XCODE_BUILD);
        Ran {
            code: Some(0),
            ..Ran::default()
        }
    }
}

/// The programs the maintenance hook's looks run on the Mac: each recorded.
pub(super) struct HookPrograms(pub(super) Arc<Machine>);

impl Programs for HookPrograms {
    fn tool(&self, program: &Path, _deadline: std::time::Duration) -> Box<dyn Launchctl> {
        self.0
            .program(&self.0.mac_path(&program.to_string_lossy()), true)
    }
}

/// The install programs the maintenance hook runs on the Mac, their standard input a pipe.
pub(super) struct HookInstallers(pub(super) Arc<Machine>);

impl Installers for HookInstallers {
    fn runner(&self, program: &Path, _deadline: std::time::Duration) -> Box<dyn StdinRun> {
        Box::new(Installer {
            program: self.0.mac_path(&program.to_string_lossy()),
            machine: Arc::clone(&self.0),
        })
    }
}

/// One install program of the Mac.
struct Installer {
    program: String,
    machine: Arc<Machine>,
}

impl StdinRun for Installer {
    fn run(
        &self,
        _step: &str,
        args: &[String],
        input: &[u8],
    ) -> Result<LaunchctlOutput, StdinError> {
        Ok(self.machine.install(&self.program, args, input))
    }
}

/// The programs the `xcode` kind's stage runs: `xip` and `codesign`, each recorded with its
/// arguments, the scratch paths among them written as the Mac's. `codesign` always passes: a
/// failed signature is not simulated here (the stage's own tests cover it).
pub(super) struct HookRunner(pub(super) Arc<Machine>);

impl Runner for HookRunner {
    fn run(&self, spawn: &Spawn) -> std::io::Result<Ran> {
        let program = spawn.program.to_string_lossy().into_owned();
        let args: Vec<String> = spawn
            .args
            .iter()
            .map(|arg| self.0.mac_path(&arg.to_string_lossy()))
            .collect();
        self.0.effect(line(&program, &args));
        match (program.as_str(), spawn.args.as_slice()) {
            (XIP, [expand, archive]) if expand == "--expand" => {
                Ok(self.0.expand(Path::new(archive), &spawn.cwd))
            }
            (CODESIGN, _) => Ok(Ran {
                code: Some(0),
                ..Ran::default()
            }),
            _ => Err(std::io::Error::other(format!(
                "the fake Mac has no {program} that takes these arguments"
            ))),
        }
    }
}

/// The coordinator's Xcode store as the `xcode` kind's stage reads it: `GET /v1/xcode/<sha256>`
/// answered from the store's file of the archive, from the byte asked for on, each fetch
/// recorded. The call's limit is not simulated: the stage's own tests cover a short or long
/// body.
pub(super) struct XcodeStore(pub(super) Arc<Machine>);

impl Store for XcodeStore {
    fn get_from(
        &self,
        path: &str,
        first: u64,
        _limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
    ) -> Result<Got, xcode::store::StoreError> {
        self.0.effect(format!("GET {path} from byte {first}"));
        let world = self.0.store.lock().expect("not poisoned").clone();
        let Some(sha256) = path.strip_prefix("/v1/xcode/") else {
            return Ok(Got::NotFound);
        };
        let Some(Ok(bytes)) = world.map(|world| fs::read(world.xcode_archive(sha256))) else {
            return Ok(Got::NotFound);
        };
        let rest = &bytes[usize::try_from(first).expect("a small offset")..];
        sink(rest).map_err(|error| {
            xcode::store::StoreError::Unusable(format!("the sink failed: {error}"))
        })?;
        Ok(Got::Body(rest.len() as u64))
    }
}

/// The Mac's data volume, and the volume of its `/Applications`.
pub(super) struct Space(pub(super) Arc<Machine>);

impl FreeSpace for Space {
    fn free_bytes(&self) -> Result<u64, String> {
        Ok(self.0.state().free)
    }
}

/// The maintenance hook's log on the Mac: each line kept.
pub(super) struct HookLog(pub(super) Arc<Machine>);

impl Log for HookLog {
    fn line(&self, text: &str) {
        self.0.state().logged.push(text.to_owned());
    }
}
