//! The machine being installed: a QEMU guest with UEFI firmware, an empty scratch disk, a fixed
//! `-uuid` and SMBIOS serial (the claims a person compares), and one NIC on the LAN's tap. The
//! NIC is first in QEMU's boot order (`bootindex=0`) and the disk second, so, like the farm's
//! boards, the guest tries network boot first at every boot. That OVMF applies QEMU's boot order
//! over the one in its variables, whatever curtin writes there, is a fact of design section 11
//! marked to verify: the x86-64 test checks it, by the DISCOVER of the reboot after the install.
//!
//! - x86-64: `qemu-system-x86_64` under KVM, q35, with Ubuntu's OVMF (`ovmf`).
//! - arm64: `qemu-system-aarch64` under TCG, `virt`, with Ubuntu's AAVMF (`qemu-efi-aarch64`):
//!   the hosted runners have no arm64 KVM.
//!
//! The NIC has no option ROM (`romfile=`): the firmware's own virtio-net driver and network
//! stack boot it, as a board's firmware boots its NIC, and QEMU's iPXE ROMs (a package the job
//! does not install) are not used.
//!
//! The serial console is the program's standard output: a line on it is an event of the test.
//! The disk is a sparse raw file (no `qemu-img` needed), on a SCSI disk that reports itself not
//! rotational, so the seed's default `--install-disk` (the smallest SSD) takes it.

use std::fs::{self, File};
use std::io::Write as _;
use std::net::Ipv4Addr;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::events::{Bus, Source};
use super::program::Program;

/// The size of the guest's disk: room for the direct layout of a live-server install.
const DISK_BYTES: u64 = 20 * 1024 * 1024 * 1024;
/// The guest's memory, in MiB: the live system copies the whole ISO (3.8 GiB) into a file
/// system in memory, which is given half of it by default, so 8 GiB is not enough.
const MEMORY_MIB: &str = "12288";
/// How often the guest's screen is saved for the job's artifacts: the live installer writes to
/// the screen only (its kernel line names no serial console), so the screens are what a failed
/// or timed-out job shows of it. A pace of the evidence, which no test reads.
const SCREEN_EVERY: Duration = Duration::from_secs(120);
/// How many of the latest screens are kept (each a few MiB, as PPM, the format every QEMU
/// writes).
const SCREENS_KEPT: u32 = 8;

/// The firmware of a platform: its code (read only) and the template of its variables, which the
/// guest gets a writable copy of.
struct Firmware {
    code: &'static str,
    vars: &'static str,
}

/// The guest's platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Arm64,
}

impl Arch {
    fn firmware(self) -> Firmware {
        match self {
            Arch::X86_64 => Firmware {
                code: "/usr/share/OVMF/OVMF_CODE_4M.fd",
                vars: "/usr/share/OVMF/OVMF_VARS_4M.fd",
            },
            Arch::Arm64 => Firmware {
                code: "/usr/share/AAVMF/AAVMF_CODE.fd",
                vars: "/usr/share/AAVMF/AAVMF_VARS.fd",
            },
        }
    }

    fn program(self) -> &'static str {
        match self {
            Arch::X86_64 => "qemu-system-x86_64",
            Arch::Arm64 => "qemu-system-aarch64",
        }
    }

    /// The display device, whose screen the rig saves.
    fn display(self) -> &'static str {
        match self {
            Arch::X86_64 => "VGA",
            Arch::Arm64 => "virtio-gpu-pci",
        }
    }

    /// The machine, its CPU and its accelerator.
    fn machine(self) -> [&'static str; 4] {
        match self {
            Arch::X86_64 => ["-machine", "q35,accel=kvm", "-cpu", "host"],
            Arch::Arm64 => ["-machine", "virt,accel=tcg", "-cpu", "max"],
        }
    }

    /// The architecture GRUB names in a menu's path (`${grub_cpu}`).
    pub fn grub_cpu(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Arm64 => "arm64",
        }
    }

    /// The platform of the boot set.
    pub fn platform(self) -> &'static str {
        match self {
            Arch::X86_64 => "linux-x86_64",
            Arch::Arm64 => "linux-arm64",
        }
    }
}

/// What the guest is.
pub struct Machine {
    pub arch: Arch,
    pub mac: &'static str,
    pub uuid: &'static str,
    pub serial: &'static str,
}

/// The guest's disk and firmware variables: kept across its power cycles.
pub struct Guest {
    machine: Machine,
    disk: PathBuf,
    vars: PathBuf,
    log_dir: PathBuf,
    monitor: PathBuf,
    ns: String,
    run: Option<Program>,
    screens: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

impl Guest {
    /// Makes the guest's empty disk and firmware variables under `dir`, the guest to be run in
    /// the bridge's namespace `ns`, its console kept in `log_dir`. It is not started.
    pub fn new(machine: Machine, dir: &Path, ns: &str, log_dir: &Path) -> Guest {
        let firmware = machine.arch.firmware();
        for file in [firmware.code, firmware.vars] {
            assert!(
                Path::new(file).is_file(),
                "{file} is missing: the job installs the firmware package before the test"
            );
        }
        fs::create_dir_all(dir).expect("the guest's directory");
        let disk = dir.join("disk.raw");
        File::create(&disk)
            .and_then(|file| file.set_len(DISK_BYTES))
            .expect("the guest's empty disk");
        let vars = dir.join("vars.fd");
        fs::copy(firmware.vars, &vars).expect("the firmware's variables");
        Guest {
            machine,
            disk,
            vars,
            log_dir: log_dir.to_path_buf(),
            monitor: dir.join("monitor.sock"),
            ns: ns.to_owned(),
            run: None,
            screens: None,
        }
    }

    pub fn machine(&self) -> &Machine {
        &self.machine
    }

    /// The QEMU command line.
    fn args(&self) -> Vec<String> {
        let firmware = self.machine.arch.firmware();
        let mut args: Vec<String> = self.machine.arch.machine().map(String::from).to_vec();
        args.extend(
            [
                "-name",
                "hostkeeper-net-install",
                "-no-user-config",
                "-nodefaults",
                "-display",
                "none",
                "-device",
                self.machine.arch.display(),
                "-monitor",
                &format!("unix:{},server=on,wait=off", self.monitor.display()),
                "-serial",
                "stdio",
                "-m",
                MEMORY_MIB,
                "-smp",
                "4",
                "-uuid",
                self.machine.uuid,
                "-smbios",
                &format!("type=1,serial={}", self.machine.serial),
                "-drive",
                &format!(
                    "if=pflash,format=raw,unit=0,readonly=on,file={}",
                    firmware.code
                ),
                "-drive",
                &format!("if=pflash,format=raw,unit=1,file={}", self.vars.display()),
                "-netdev",
                &format!(
                    "tap,id=lan,ifname={},script=no,downscript=no",
                    super::lan::TAP
                ),
                "-device",
                &format!(
                    "virtio-net-pci,netdev=lan,mac={},romfile=,bootindex=0",
                    self.machine.mac
                ),
                "-device",
                "virtio-scsi-pci,id=scsi",
                "-drive",
                &format!("if=none,id=disk,format=raw,file={}", self.disk.display()),
                "-device",
                "scsi-hd,drive=disk,bus=scsi.0,rotation_rate=1,bootindex=1",
            ]
            .map(String::from),
        );
        args
    }

    /// Powers the guest on.
    pub fn start(&mut self, bus: &Bus) {
        assert!(self.run.is_none(), "the guest is running");
        self.run = Some(Program::start(
            bus,
            Source::Guest,
            "the guest",
            &self.ns,
            self.machine.arch.program(),
            &self.args(),
            &self.log_dir.join("guest-console.log"),
        ));
        if self.screens.is_none() {
            let stop = Arc::new(AtomicBool::new(false));
            let (stopped, monitor, dir) = (
                Arc::clone(&stop),
                self.monitor.clone(),
                self.log_dir.clone(),
            );
            let thread = thread::spawn(move || {
                let mut n = 0u32;
                while !stopped.load(Ordering::SeqCst) {
                    thread::sleep(SCREEN_EVERY);
                    n += 1;
                    let file = dir.join(format!("screen-{n:03}.ppm"));
                    // A guest that is down between two runs has no monitor: no screen then.
                    let Ok(mut monitor) = UnixStream::connect(&monitor) else {
                        continue;
                    };
                    let _ = writeln!(monitor, "screendump {}", file.display());
                    let _ = monitor.flush();
                    // Read to the prompt after the command (the second one, after the banner's),
                    // so that the command is done before the connection closes.
                    let _ = monitor.set_read_timeout(Some(Duration::from_secs(30)));
                    let mut seen = Vec::new();
                    let mut answer = [0u8; 4096];
                    while seen.windows(6).filter(|w| *w == b"(qemu)").count() < 2 {
                        match std::io::Read::read(&mut monitor, &mut answer) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => seen.extend_from_slice(&answer[..read]),
                        }
                    }
                    // What the monitor said, for a screen that is missing.
                    if let Ok(mut log) = File::options()
                        .create(true)
                        .append(true)
                        .open(dir.join("guest-monitor.log"))
                    {
                        let _ = log.write_all(&seen);
                        let _ = writeln!(log);
                    }
                    // The last screens are what a failure needs; the older ones go.
                    if let Some(old) = n.checked_sub(SCREENS_KEPT) {
                        let _ = fs::remove_file(dir.join(format!("screen-{old:03}.ppm")));
                    }
                }
            });
            self.screens = Some((stop, thread));
        }
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        // The screen thread is left to end with the process: it sleeps between two screens.
        if let Some((stop, _)) = &self.screens {
            stop.store(true, Ordering::SeqCst);
        }
    }
}

/// The guest's operator account over SSH, from the host's namespace: the key the seed's
/// `--ssh-authorized-keys` names, and no host key kept (each install makes a new one).
pub struct Ssh {
    key: PathBuf,
    pub public: PathBuf,
    address: Ipv4Addr,
}

impl Ssh {
    /// Makes a key pair under `dir` for the guest at `address`.
    pub fn new(dir: &Path, address: Ipv4Addr) -> Ssh {
        fs::create_dir_all(dir).expect("the SSH directory");
        let key = dir.join("id_ed25519");
        let out = crate::support::run(
            "ssh-keygen",
            &[
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "hostkeeper-net-install",
                "-f",
                &key.display().to_string(),
            ],
        );
        assert!(out.status.success(), "ssh-keygen: {out:?}");
        let public = dir.join("id_ed25519.pub");
        Ssh {
            key,
            public,
            address,
        }
    }

    /// Runs `command` on the guest as `operator`, with `input` on its standard input.
    pub fn run(&self, command: &str, input: &[u8]) -> Output {
        let mut child = Command::new("ssh")
            .args([
                "-F",
                "/dev/null",
                "-i",
                &self.key.display().to_string(),
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "LogLevel=ERROR",
                &format!("operator@{}", self.address),
                command,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ssh");
        let mut stdin = child.stdin.take().expect("a piped standard input");
        // A command that reads nothing closes its input early; that is no failure here.
        let _ = stdin.write_all(input);
        drop(stdin);
        child.wait_with_output().expect("ssh's output")
    }

    /// [`Self::run`], giving the standard output, or what failed.
    pub fn output(&self, command: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        let out = self.run(command, input);
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!(
                "ssh {command:?} ended with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }

    /// Reboots the guest. The connection drops as it goes down, so how ssh ends is no answer.
    pub fn reboot(&self) {
        let _ = self.run("sudo systemctl reboot", b"");
    }
}
