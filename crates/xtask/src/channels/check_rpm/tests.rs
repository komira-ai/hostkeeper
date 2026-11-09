use std::cell::RefCell;

use super::*;
use crate::channels::fixtures::FakeExec;
use crate::channels::spec::HOSTKEEPER;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const URL: &str = "https://packages.komira.ai";

fn daemon() -> &'static Package {
    &HOSTKEEPER.packages[0]
}

fn coordinator() -> &'static Package {
    &HOSTKEEPER.packages[1]
}

/// A container that answers for `uname`, `sh`, `dnf`, `rpm`, `curl`, `getent` and the installed
/// binary, as one of a clean image on which the package was installed.
struct Machine {
    package: &'static Package,
    calls: RefCell<Vec<String>>,
    machine: &'static str,
    installed: &'static str,
    checked: &'static str,
    says: Option<String>,
    /// The files the install does not create.
    omit: Vec<&'static str>,
    /// What listing the unit's links prints.
    links: &'static str,
    /// What looking for a running process prints.
    running: &'static str,
    user: bool,
    /// A command line (joined) containing this text is refused.
    refuse: Option<&'static str>,
}

impl Machine {
    fn new(package: &'static Package) -> Machine {
        Machine {
            package,
            calls: RefCell::new(Vec::new()),
            machine: "x86_64",
            installed: "1.2.3-1.x86_64",
            checked: "/work/p.rpm: digests signatures OK\n",
            says: None,
            omit: Vec::new(),
            links: "",
            running: "",
            user: true,
            refuse: None,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

fn answer(stdout: &str) -> Result<Answer, Error> {
    Ok(Answer {
        ok: true,
        stdout: stdout.to_owned(),
        stderr: String::new(),
    })
}

impl Host for Machine {
    fn run(&self, program: &Path, args: &[String]) -> Result<Answer, Error> {
        let name = program.display().to_string();
        let line = format!("{name} {}", args.join(" "));
        self.calls.borrow_mut().push(line.clone());
        if let Some(text) = self.refuse.filter(|text| line.contains(text)) {
            return Ok(Answer {
                ok: false,
                stdout: String::new(),
                stderr: format!("refused: {text}\n"),
            });
        }
        let script = args.get(1).map_or("", String::as_str);
        match name.as_str() {
            "uname" => answer(&format!("{}\n", self.machine)),
            "rpm" if args[0] == "-q" => answer(self.installed),
            "rpm" if args[0] == "-K" => answer(self.checked),
            "sh" if script.starts_with("test -f") => Ok(Answer {
                ok: !self.omit.contains(&args[3].as_str()),
                stdout: String::new(),
                stderr: String::new(),
            }),
            "sh" if script.starts_with("ls ") => answer(self.links),
            "sh" if script.starts_with("grep ") => answer(self.running),
            "getent" => Ok(Answer {
                ok: self.user,
                stdout: String::new(),
                stderr: String::new(),
            }),
            program if program == self.package.binary => answer(
                &self
                    .says
                    .clone()
                    .unwrap_or_else(|| format!("{} 1.2.3+0123456789ab\n", self.package.program())),
            ),
            _ => answer(""),
        }
    }
}

fn rpm(package: &'static Package) -> Rpm<'static> {
    Rpm {
        package,
        arch: "x86_64",
        version: "1.2.3",
        commit: COMMIT,
        url: URL,
        work: "/work",
    }
}

#[test]
fn the_daemon_is_installed_with_dnf_from_the_testing_tree_after_systemd() {
    let mut machine = Machine::new(daemon());
    machine.links = "/etc/systemd/system/multi-user.target.wants/hostkeeper.service\n";
    let said = check_rpm(&machine, &rpm(daemon())).expect("checked");
    let calls = machine.calls();
    let order: Vec<&str> = calls
        .iter()
        .map(|c| c.split(' ').next().unwrap_or_default())
        .collect();
    assert_eq!(
        order,
        [
            "uname",
            "sh",
            "dnf",
            "dnf",
            "rpm",
            "rpm",
            "curl",
            "rpm",
            "/usr/bin/hostkeeper",
            "sh",
            "sh",
            "sh",
            "sh",
            "sh",
            "sh",
            "sh",
            "sh",
            "sh"
        ]
    );
    assert_eq!(calls[2], "dnf install -y systemd");
    assert_eq!(calls[3], "dnf install -y hostkeeper-1.2.3");
    assert_eq!(
        calls[4],
        "rpm -q --qf %{VERSION}-%{RELEASE}.%{ARCH} hostkeeper"
    );
    assert_eq!(
        calls[5],
        "rpm --import https://packages.komira.ai/apt/komira-archive-keyring.asc"
    );
    assert_eq!(
        calls[6],
        "curl --proto =https --tlsv1.2 --fail --silent --show-error --output /work/hostkeeper-1.2.3-1.x86_64.rpm https://packages.komira.ai/rpm/pool/hostkeeper/hostkeeper-1.2.3-1.x86_64.rpm"
    );
    assert_eq!(calls[7], "rpm -K /work/hostkeeper-1.2.3-1.x86_64.rpm");
    assert!(
        calls[1].contains("gpgcheck=1")
            && calls[1].ends_with("/etc/yum.repos.d/komira-testing.repo")
    );
    assert!(calls[1].contains("baseurl=https://packages.komira.ai/rpm/testing/$basearch"));
    assert!(calls[1].contains("repo_gpgcheck=1"));
    assert_eq!(
        said,
        [
            "check-rpm: hostkeeper 1.2.3 installed from the suite testing on x86_64; rpm -K says OK; it says \"hostkeeper 1.2.3+0123456789ab\"; its 7 files are there; the service is enabled and not running"
        ]
    );
}

#[test]
fn the_coordinator_needs_no_prerequisite_and_its_user_and_unit_are_checked() {
    let machine = Machine::new(coordinator());
    let said = check_rpm(&machine, &rpm(coordinator())).expect("checked");
    let calls = machine.calls();
    assert_eq!(calls[2], "dnf install -y hostkeeper-coordinator-1.2.3");
    assert!(calls.contains(&"getent passwd hostkeeper-coordinator".to_owned()));
    assert!(
        calls
            .iter()
            .any(|c| c.ends_with("sh /usr/share/hostkeeper-coordinator/ui/index.html"))
    );
    assert!(
        calls
            .iter()
            .any(|c| c.ends_with("sh hostkeeper-coordinator.service"))
    );
    assert!(
        calls
            .iter()
            .any(|c| c.ends_with("sh hostkeeper-coord") || c.ends_with("sh hostkeeper-coor"))
    );
    assert_eq!(
        said,
        [
            "check-rpm: hostkeeper-coordinator 1.2.3 installed from the suite testing on x86_64; rpm -K says OK; it says \"hostkeeper-coordinator 1.2.3+0123456789ab\"; its 6 files are there; the user hostkeeper-coordinator exists; the service is not enabled and not running"
        ]
    );
}

#[test]
fn the_other_architecture_is_installed_by_its_file_name() {
    let mut machine = Machine::new(coordinator());
    machine.machine = "aarch64";
    machine.installed = "1.2.3-1.aarch64";
    let mut r = rpm(coordinator());
    r.arch = "aarch64";
    check_rpm(&machine, &r).expect("checked");
    assert!(
        machine
            .calls()
            .contains(&"rpm -K /work/hostkeeper-coordinator-1.2.3-1.aarch64.rpm".to_owned())
    );
}

#[test]
fn a_package_missing_files_or_its_user_is_named() {
    let mut machine = Machine::new(coordinator());
    machine.omit = coordinator().files.to_vec();
    machine.user = false;
    let Err(Error::Refused(problems)) = check_rpm(&machine, &rpm(coordinator())) else {
        panic!("not refused");
    };
    let mut want: Vec<String> = coordinator()
        .files
        .iter()
        .map(|f| format!("{f} is not installed"))
        .collect();
    want.push("the user hostkeeper-coordinator does not exist".to_owned());
    assert_eq!(problems, want);
}

#[test]
fn a_unit_enabled_against_the_spec_and_a_process_that_runs_are_named() {
    let mut machine = Machine::new(coordinator());
    machine.links = "/etc/systemd/system/multi-user.target.wants/hostkeeper-coordinator.service\n";
    machine.running = "42\n";
    assert_eq!(
        check_rpm(&machine, &rpm(coordinator())).unwrap_err(),
        Error::Refused(vec![
            "the service is enabled: /etc/systemd/system/multi-user.target.wants/hostkeeper-coordinator.service exists, and the package enables nothing".to_owned(),
            "the service is running (process 42), and the package starts nothing".to_owned(),
        ])
    );
    // The daemon is enabled by its postinstall: no link is a failure.
    let machine = Machine::new(daemon());
    assert_eq!(
        check_rpm(&machine, &rpm(daemon())).unwrap_err(),
        Error::Refused(vec![
            "the service is not enabled: no link to hostkeeper.service is in etc/systemd/system, and the package enables it on a first install".to_owned()
        ])
    );
}

#[test]
fn a_package_at_another_version_or_an_rpm_whose_signature_is_not_ok_is_named() {
    let mut machine = Machine::new(coordinator());
    machine.installed = "1.2.2-1.x86_64";
    machine.checked = "/work/p.rpm: digests SIGNATURES NOT OK\n";
    let Err(Error::Refused(problems)) = check_rpm(&machine, &rpm(coordinator())) else {
        panic!("not refused");
    };
    assert_eq!(
        problems,
        [
            "rpm says hostkeeper-coordinator \"1.2.2-1.x86_64\" is installed, not \"1.2.3-1.x86_64\"",
            "rpm -K says \"/work/p.rpm: digests SIGNATURES NOT OK\", not that the digests and signatures are OK"
        ]
    );
    // Neither a missing "signatures OK" nor a "NOT OK" anywhere in the answer passes.
    for checked in [
        "/work/p.rpm: digests OK\n",
        "/work/p.rpm: digests signatures OK\n/work/q.rpm: signatures NOT OK\n",
    ] {
        let mut machine = Machine::new(coordinator());
        machine.checked = checked;
        assert!(matches!(
            check_rpm(&machine, &rpm(coordinator())),
            Err(Error::Refused(_))
        ));
    }
}

#[test]
fn a_binary_that_says_another_release_is_refused() {
    let mut machine = Machine::new(coordinator());
    machine.says = Some("hostkeeper-coordinator 1.2.3+ffffffffffff\n".to_owned());
    assert_eq!(
        check_rpm(&machine, &rpm(coordinator()))
            .unwrap_err()
            .to_string(),
        "the binary says \"hostkeeper-coordinator 1.2.3+ffffffffffff\\n\", not \"hostkeeper-coordinator 1.2.3+0123456789ab\""
    );
}

#[test]
fn a_program_that_fails_is_named_with_what_it_said_and_a_machine_of_another_architecture_is_refused()
 {
    for (refuse, program) in [
        ("uname", "uname -m"),
        ("printf", "printf"),
        ("install -y hostkeeper-coordinator-1.2.3", "dnf install"),
        ("--qf", "rpm -q"),
        ("--import", "rpm --import"),
        ("--output", "--output"),
        ("-K", "rpm -K"),
        ("--version", "hostkeeper-coordinator --version"),
        ("ls /etc", "ls /etc"),
        ("grep -l", "grep -l"),
    ] {
        let mut machine = Machine::new(coordinator());
        machine.refuse = Some(refuse);
        let e = check_rpm(&machine, &rpm(coordinator()))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains(program) && e.contains(&format!("failed: refused: {refuse}")),
            "{refuse}: {e}"
        );
    }
    let mut machine = Machine::new(daemon());
    machine.refuse = Some("install -y systemd");
    assert!(
        check_rpm(&machine, &rpm(daemon()))
            .unwrap_err()
            .to_string()
            .starts_with("dnf install -y systemd failed: refused")
    );
    let mut machine = Machine::new(coordinator());
    machine.machine = "aarch64";
    assert_eq!(
        check_rpm(&machine, &rpm(coordinator()))
            .unwrap_err()
            .to_string(),
        "this machine is aarch64, not x86_64: the check would install another package"
    );
}

#[test]
fn flags_out_of_their_form_are_usage_errors_before_anything_runs() {
    let machine = Machine::new(coordinator());
    for change in [
        |r: &mut Rpm| r.version = "1.2",
        |r: &mut Rpm| r.arch = "i386",
        |r: &mut Rpm| r.url = "http://packages.komira.ai",
        |r: &mut Rpm| r.url = "https://packages.komira.ai/",
        |r: &mut Rpm| r.work = "work",
        |r: &mut Rpm| r.work = "/work/",
        |r: &mut Rpm| r.commit = "abc",
    ] {
        let mut r = rpm(coordinator());
        change(&mut r);
        assert!(matches!(check_rpm(&machine, &r), Err(Error::Usage(_))));
    }
    assert!(machine.calls().is_empty());
}

#[test]
fn every_program_runs_in_the_container_through_docker_exec() {
    let exec = FakeExec::says("x86_64\n");
    let container = Container {
        exec: &exec,
        docker: PathBuf::from("docker"),
        name: "check-rpm".to_owned(),
    };
    let answer = container
        .run(Path::new("uname"), &["-m".to_owned()])
        .expect("answer");
    assert_eq!(answer.stdout, "x86_64\n");
    assert_eq!(
        *exec.calls.borrow(),
        [(
            "docker".to_owned(),
            vec![
                "exec".to_owned(),
                "check-rpm".to_owned(),
                "uname".to_owned(),
                "-m".to_owned()
            ]
        )]
    );
    let missing = FakeExec::missing();
    let container = Container {
        exec: &missing,
        docker: PathBuf::from("/nowhere/docker"),
        name: "c".to_owned(),
    };
    let e = container.run(Path::new("uname"), &[]).unwrap_err();
    assert!(e.to_string().starts_with("/nowhere/docker: "), "{e}");
}
