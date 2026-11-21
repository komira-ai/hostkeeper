//! The fake Linux host: its files, its programs and its service.

use super::*;

/// What a fake host has due at first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// Nothing.
    Nothing,
    /// The security update of its kernel image ([`KERNEL_UPDATE`]).
    Kernel,
    /// A restart alone: `run/reboot-required` is there, and no package is due.
    Reboot,
}

/// What a fake host is and does.
struct State {
    /// The number of its boot: its boot id is `boot-<n>`.
    boot: u32,
    /// The kernel it runs.
    running: String,
    /// The newest kernel installed.
    installed: String,
    /// What `apt-get --simulate dist-upgrade` prints.
    simulation: String,
    /// Whether the install fails.
    install_fails: bool,
    /// Whether the host asked for a reboot that has not happened yet.
    reboot_asked: bool,
    /// Whether `alpha` runs work that holds its drain until the drain's deadline.
    busy: bool,
    /// Whether `alpha` is out of service: drained and not resumed.
    cordoned: bool,
    /// Every change the host went through, in order.
    effects: Vec<String>,
}

/// A fake Linux host: its files under a scratch directory, its programs and its service.
pub struct Machine {
    root: Scratch,
    state: Mutex<State>,
}

impl Machine {
    /// A host named `name` running [`OLD_KERNEL`] in the boot `boot-1`, with `due` due.
    pub fn new(name: &str, due: Due) -> Arc<Machine> {
        let root = Scratch::new(&format!("{name}-host"));
        let machine = Machine {
            root,
            state: Mutex::new(State {
                boot: 1,
                running: OLD_KERNEL.to_owned(),
                installed: OLD_KERNEL.to_owned(),
                simulation: match due {
                    Due::Kernel => KERNEL_UPDATE.to_owned(),
                    Due::Nothing | Due::Reboot => String::new(),
                },
                install_fails: false,
                reboot_asked: false,
                busy: false,
                cordoned: false,
                effects: Vec::new(),
            }),
        };
        machine.put("etc/os-release", "ID=ubuntu\nVERSION_ID=\"24.04\"\n");
        machine.put("proc/sys/kernel/random/boot_id", "boot-1\n");
        machine.kernel(OLD_KERNEL);
        if due == Due::Reboot {
            machine.put("run/reboot-required", "*** System restart required ***\n");
        }
        Arc::new(machine)
    }

    fn put(&self, path: &str, text: &str) {
        let file = self.root.path().join(path);
        fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
        fs::write(file, text).expect("the file");
    }

    fn kernel(&self, release: &str) {
        fs::create_dir_all(
            self.root
                .path()
                .join(format!("lib/modules/{release}/kernel")),
        )
        .expect("a kernel");
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("not poisoned")
    }

    pub(super) fn effect(&self, line: String) {
        self.state().effects.push(line);
    }

    /// Makes the install fail, as an unreachable mirror makes it.
    pub fn fail_install(&self) {
        self.state().install_fails = true;
    }

    /// Gives `alpha` work that holds its drain until the drain's deadline passes.
    pub fn keep_busy(&self) {
        self.state().busy = true;
    }

    /// Every change the host went through, in order.
    pub fn effects(&self) -> Vec<String> {
        self.state().effects.clone()
    }

    /// Whether `alpha` is drained and not resumed.
    pub fn cordoned(&self) -> bool {
        self.state().cordoned
    }

    /// Whether the host asked for a reboot that has not happened yet.
    pub fn reboot_asked(&self) -> bool {
        self.state().reboot_asked
    }

    /// The kernel the host runs.
    pub fn running(&self) -> String {
        self.state().running.clone()
    }

    /// The boot id of the boot the host runs.
    pub fn boot_id(&self) -> String {
        format!("boot-{}", self.state().boot)
    }

    /// The reboot the host asked for: the next boot, running the newest kernel installed, with
    /// no restart asked for any more. Returns the new boot's id.
    pub(super) fn boot(&self) -> String {
        let id = {
            let mut state = self.state();
            assert!(state.reboot_asked, "the host asked for no reboot");
            state.reboot_asked = false;
            state.boot += 1;
            state.running = state.installed.clone();
            let id = format!("boot-{}", state.boot);
            state.effects.push(format!("boot {id}"));
            id
        };
        let _gone = fs::remove_file(self.root.path().join("run/reboot-required"));
        self.put("proc/sys/kernel/random/boot_id", &format!("{id}\n"));
        id
    }

    /// The program `name` of the host.
    pub(super) fn program(self: &Arc<Self>, name: &'static str) -> Box<dyn Launchctl> {
        Box::new(Program {
            name,
            machine: Arc::clone(self),
        })
    }

    fn root(&self) -> HostRoot {
        HostRoot::at(self.root.path())
    }

    fn reboot_check(self: &Arc<Self>) -> RebootCheck {
        RebootCheck::new(
            HostFacts::new(self.program("uname"), self.root()),
            RebootNeed::new(
                self.program("needrestart"),
                self.program("needs-restarting"),
                self.root(),
            ),
        )
    }

    /// What the daemon's report looks at: the host's kernels, the reboot it needs and the
    /// package manager's updates.
    pub(super) fn collectors(self: &Arc<Self>) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(HostFacts::new(self.program("uname"), self.root())),
            Box::new(RebootNeed::new(
                self.program("needrestart"),
                self.program("needs-restarting"),
                self.root(),
            )),
            Box::new(Apt::new(self.program("apt-get"), self.program("apt-get"))),
        ]
    }

    /// The maintenance hook's kinds on this host: the real `linux` and `reboot`, checked by the
    /// real check of the Linux kinds, every program and file the host's.
    pub(super) fn kinds(self: &Arc<Self>) -> Registry {
        let apt = AptUpgrade::new(
            self.program("apt-get"),
            self.program("apt-get"),
            self.program("apt-mark"),
            self.program("env"),
            LOCK_WAIT,
        );
        let linux = LinuxKind::new(Some(Box::new(apt)), Box::new(Own), self.reboot_check());
        let reboot = RebootKind::new(self.reboot_check());
        let check = LinuxCheck::new(
            Box::new(HostFacts::new(self.program("uname"), self.root())),
            Box::new(RebootNeed::new(
                self.program("needrestart"),
                self.program("needs-restarting"),
                self.root(),
            )),
            Some(Box::new(Apt::new(
                self.program("apt-get"),
                self.program("apt-get"),
            ))),
            Box::new(Own),
            Box::new(Healthy),
        );
        hook_linux::kinds(check, Box::new(linux), Box::new(reboot))
    }

    /// The program `name` run with `args`.
    fn run(&self, name: &str, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        match name {
            "uname" => Ok(LaunchctlOutput::success(&format!("{}\n", self.running()))),
            "apt-get" if words.contains(&"--simulate") => {
                Ok(LaunchctlOutput::success(&self.state().simulation))
            }
            "apt-get" if words.contains(&"update") => Ok(LaunchctlOutput::success("")),
            "apt-mark" => Ok(LaunchctlOutput::success("")),
            "env" if words.contains(&"--only-upgrade") || words.contains(&"dist-upgrade") => {
                Ok(self.install(&words))
            }
            "systemctl" if words == ["reboot"] => {
                let mut state = self.state();
                state.reboot_asked = true;
                state.effects.push("reboot".to_owned());
                Ok(LaunchctlOutput::success(""))
            }
            _ => Err(LaunchctlError::Spawn {
                args: args.join(" "),
                why: format!("the fake host has no {name} that takes these arguments"),
            }),
        }
    }

    /// The install `words` asks for: the packages named after `--only-upgrade`, logged; a kernel
    /// image among them leaves the new kernel installed and a restart asked for.
    fn install(&self, words: &[&str]) -> LaunchctlOutput {
        let packages: Vec<&str> = words
            .iter()
            .skip_while(|word| **word != "--only-upgrade")
            .skip(1)
            .copied()
            .collect();
        let fails = {
            let mut state = self.state();
            state
                .effects
                .push(format!("install {}", packages.join(" ")));
            state.install_fails
        };
        if fails {
            return LaunchctlOutput {
                code: Some(100),
                stdout: Vec::new(),
                stderr: format!("{INSTALL_ERROR}\n").into_bytes(),
            };
        }
        if packages.contains(&KERNEL_PACKAGE) {
            self.kernel(NEW_KERNEL);
            self.put("run/reboot-required", "*** System restart required ***\n");
            let mut state = self.state();
            state.installed = NEW_KERNEL.to_owned();
            state.simulation.clear();
        }
        LaunchctlOutput::success("")
    }

    /// `alpha`'s `step` (`drain` or `resume`) with the params file `params`: a drain takes the
    /// service out of service and, with a deadline while work holds it, ends deferred (exit 75)
    /// as the hook contract says; a resume puts it back.
    pub(super) fn service_step(&self, service: &str, step: &str, params: &Path) -> i32 {
        let text = fs::read_to_string(params).expect("the service's params");
        let mut state = self.state();
        state.effects.push(format!("{step} {service}"));
        match step {
            "drain" => {
                state.cordoned = true;
                if state.busy && text.contains("drain_deadline") {
                    EXIT_DEFERRED
                } else {
                    0
                }
            }
            "resume" => {
                state.cordoned = false;
                0
            }
            other => panic!("{service} has no step {other}"),
        }
    }
}

impl BootIdSource for Machine {
    fn read(&self) -> Result<BootId, BootIdError> {
        BootId::new("the fake host", &self.boot_id())
    }
}

/// A program of a fake host.
struct Program {
    name: &'static str,
    machine: Arc<Machine>,
}

impl Launchctl for Program {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        self.machine.run(self.name, args)
    }
}

/// The packages hostkeeper installs: `hostkeeper` alone.
struct Own;

impl OwnPackages for Own {
    fn names(&self) -> Result<std::collections::BTreeSet<String>, String> {
        Ok(std::collections::BTreeSet::from(["hostkeeper".to_owned()]))
    }
}

/// The host's services, healthy.
struct Healthy;

impl ServiceHealth for Healthy {
    fn services(&self) -> Result<Vec<(String, pb::Health)>, String> {
        Ok(vec![(SERVICE.to_owned(), pb::Health::Healthy)])
    }
}
