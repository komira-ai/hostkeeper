//! `xtask channels check-apt --package hello|mdm` end to end: the verb is run as a process whose
//! `PATH` holds the test program `tests/bin/apt_fixture.rs` as `env`, so each program of the
//! checked system (`dpkg`, `apt-get`, `curl`, `dpkg-query`, the installed binary) is answered from
//! scratch files, and `--root` is a scratch directory. Its exit status, words, the files it wrote
//! and the calls it made are read. The checks themselves are tested beside the verb, in the
//! library; these tests hold the parsing of the services' native packages as `--package` choices
//! and their dispatch to the check.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use testdir::TestDir;

const FAKE: &str = env!("CARGO_BIN_EXE_xtask-apt-fixture");
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const APT_URL: &str = "https://packages.example.com/apt";
/// A binary OpenPGP keyring starts with a packet header, bit 7 set.
const KEYRING: &[u8] = b"\x99\x01\x0dkey";

/// A scratch directory: `bin/env` (the fixture), `bin/apt/` (what it answers), `root/` (the
/// checked system).
struct Machine {
    _dir: TestDir,
    bin: PathBuf,
    root: PathBuf,
}

impl Machine {
    /// A machine of Debian architecture `machine` on which `package` installs at `version`, its
    /// binary saying `says`, with the package's `files` in place below the root.
    fn new(name: &str, machine: &str, version: &str, says: &str, files: &[&str]) -> Machine {
        let dir = TestDir::under(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!("channels-check-apt-{name}"),
        );
        let bin = dir.join("bin");
        let root = dir.join("root");
        let data = bin.join("apt");
        std::fs::create_dir_all(&data).expect("make the fixture's directory");
        std::fs::create_dir_all(&root).expect("make the root");
        // A hard link, not a copy: a copy holds the file open for writing, and a process another
        // test forks meanwhile keeps it open until it execs ("text file busy").
        std::fs::hard_link(FAKE, bin.join("env")).expect("install the fake env");
        for (file, bytes) in [
            ("machine", format!("{machine}\n").into_bytes()),
            ("version", version.as_bytes().to_vec()),
            ("says", format!("{says}\n").into_bytes()),
            ("keyring", KEYRING.to_vec()),
        ] {
            std::fs::write(data.join(file), bytes).expect("write the fixture's answer");
        }
        for file in files {
            let path = root.join(file.trim_start_matches('/'));
            std::fs::create_dir_all(path.parent().expect("a directory")).expect("make its dir");
            std::fs::write(&path, b"installed").expect("install the file");
        }
        Machine {
            _dir: dir,
            bin,
            root,
        }
    }

    fn check_apt(&self, package: &str, arch: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .current_dir(&self.root)
            .env("PATH", &self.bin)
            .args([
                "channels",
                "check-apt",
                "--package",
                package,
                "--arch",
                arch,
                "--version",
                "1.2.3",
                "--commit",
                COMMIT,
                "--apt-url",
                APT_URL,
                "--root",
                self.root.to_str().expect("utf8 path"),
            ])
            .output()
            .expect("run xtask")
    }

    /// The command lines the fixture was started with, one per call, `DEBIAN_FRONTEND=...`
    /// dropped; empty when it was never started.
    fn calls(&self) -> Vec<String> {
        let calls = std::fs::read_to_string(self.bin.join("apt/calls")).unwrap_or_default();
        calls
            .lines()
            .map(|line| {
                line.strip_prefix("DEBIAN_FRONTEND=noninteractive\t")
                    .unwrap_or(line)
                    .replace('\t', " ")
            })
            .collect()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const HELLO_FILES: [&str; 4] = [
    "/usr/lib/hello/hello",
    "/usr/lib/hello/hostkeeper-hook",
    "/usr/lib/systemd/system/hello@a.service",
    "/usr/lib/systemd/system/hello@b.service",
];

const MDM_FILES: [&str; 10] = [
    "/usr/lib/mdm/mdm",
    "/usr/lib/mdm/hostkeeper-hook",
    "/usr/lib/systemd/system/mdm-cert-renew.service",
    "/usr/lib/systemd/system/mdm-cert.service",
    "/usr/lib/systemd/system/mdm-cert.timer",
    "/usr/lib/systemd/system/mdm-depserver.service",
    "/usr/lib/systemd/system/mdm-depsyncer.service",
    "/usr/lib/systemd/system/mdm-nanohub.service",
    "/usr/lib/systemd/system/mdm-proxy.service",
    "/usr/lib/systemd/system/mdm-scep.service",
];

/// The calls of a check that installs `package` and runs its `binary`, in order.
fn install_calls(root: &Path, package: &str, binary: &str) -> Vec<String> {
    let keyring = root.join("usr/share/keyrings/komira-archive-keyring.gpg");
    vec![
        "dpkg --print-architecture".to_owned(),
        "apt-get update".to_owned(),
        "apt-get install -y --no-install-recommends ca-certificates curl".to_owned(),
        format!(
            "curl --proto =https --tlsv1.2 --fail --silent --show-error --create-dirs --output {} {APT_URL}/komira-archive-keyring.gpg",
            keyring.display()
        ),
        "apt-get update".to_owned(),
        format!("apt-get install -y --no-install-recommends {package}=1.2.3"),
        format!("dpkg-query --show --showformat=${{Version}} {package}"),
        format!("{binary} --version"),
    ]
}

#[test]
fn check_apt_of_hello_on_arm64_installs_hello_from_testing_and_holds_it_to_its_version_alone() {
    let m = Machine::new("hello-arm64", "arm64", "1.2.3", "hello 1.2.3", &HELLO_FILES);
    let o = m.check_apt("hello", "arm64");
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "check-apt: hello 1.2.3 installed from the suite testing on arm64; it says \"hello 1.2.3\"; its 4 files are there; the service is not enabled and not running\n"
    );
    assert_eq!(
        m.calls(),
        install_calls(&m.root, "hello", "/usr/lib/hello/hello")
    );
    assert_eq!(
        std::fs::read_to_string(m.root.join("etc/apt/sources.list.d/komira.sources"))
            .expect("the source"),
        format!(
            "Types: deb\nURIs: {APT_URL}\nSuites: testing\nComponents: main\nArchitectures: arm64\nSigned-By: /usr/share/keyrings/komira-archive-keyring.gpg\n"
        )
    );
}

#[test]
fn check_apt_of_mdm_on_amd64_installs_mdm_from_testing_and_holds_it_to_its_version_and_commit() {
    let m = Machine::new(
        "mdm-amd64",
        "amd64",
        "1.2.3",
        "mdm 1.2.3+0123456789ab",
        &MDM_FILES,
    );
    let o = m.check_apt("mdm", "amd64");
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "check-apt: mdm 1.2.3 installed from the suite testing on amd64; it says \"mdm 1.2.3+0123456789ab\"; its 10 files are there; the service is not enabled and not running\n"
    );
    assert_eq!(m.calls(), install_calls(&m.root, "mdm", "/usr/lib/mdm/mdm"));
}

#[test]
fn check_apt_of_mdm_on_arm64_is_status_2_naming_the_architecture_before_any_program_runs() {
    let m = Machine::new("mdm-arm64", "arm64", "1.2.3", "mdm 1.2.3+0123456789ab", &[]);
    let o = m.check_apt("mdm", "arm64");
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(
        stderr(&o),
        "xtask: --arch \"arm64\": mdm is not built for it\n"
    );
    assert_eq!(m.calls(), Vec::<String>::new());
}

#[test]
fn check_apt_of_hello_whose_binary_says_a_commit_is_status_1_naming_what_it_said() {
    // hello's program says its version alone; a binary carrying the commit is another build.
    let m = Machine::new(
        "hello-commit",
        "amd64",
        "1.2.3",
        "hello 1.2.3+0123456789ab",
        &HELLO_FILES,
    );
    let o = m.check_apt("hello", "amd64");
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(
        stderr(&o),
        "xtask: the binary says \"hello 1.2.3+0123456789ab\\n\", not \"hello 1.2.3\"\n"
    );
}

#[test]
fn check_apt_of_mdm_with_a_file_missing_an_enabled_unit_and_another_version_is_status_1_naming_each()
 {
    let m = Machine::new(
        "mdm-problems",
        "amd64",
        "1.2.2",
        "mdm 1.2.3+0123456789ab",
        &MDM_FILES[1..],
    );
    let wants = m.root.join("etc/systemd/system/multi-user.target.wants");
    std::fs::create_dir_all(&wants).expect("make the wants directory");
    std::fs::write(wants.join("mdm-proxy.service"), b"").expect("enable a unit");
    let o = m.check_apt("mdm", "amd64");
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(
        stderr(&o),
        format!(
            "xtask: dpkg says mdm \"1.2.2\" is installed, not \"1.2.3\"\n/usr/lib/mdm/mdm is not installed\nthe service is enabled: {} exists, and the package enables nothing\n",
            wants.join("mdm-proxy.service").display()
        )
    );
}
