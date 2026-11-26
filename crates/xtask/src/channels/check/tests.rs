use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

use super::*;
use crate::channels::fixtures::FakeExec;
use crate::channels::spec::{AMD64, HOSTKEEPER};
use crate::testdir::TestDir;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const URL: &str = "https://packages.komira.ai/apt";

fn daemon() -> &'static Package {
    &HOSTKEEPER.packages[0]
}

fn coordinator() -> &'static Package {
    &HOSTKEEPER.packages[1]
}

fn hello() -> &'static Package {
    HOSTKEEPER.package("hello").expect("hello")
}

/// A package built for amd64 alone. No package of the product is now (the one that was, mdm's,
/// is withdrawn), so the refusal of an architecture a package is not built for is held with this.
fn amd64_only() -> &'static Package {
    static ONLY: Package = Package {
        name: "amd64-only",
        arches: &[AMD64],
        rpm: false,
        postun_required: false,
        binary: "/usr/lib/amd64-only/amd64-only",
        stamp_commit: false,
        units: &[],
        user: None,
        enabled_after_install: false,
        prerequisites: &[],
        files: &["/usr/lib/amd64-only/amd64-only"],
    };
    &ONLY
}

fn scratch(name: &str) -> TestDir {
    let beside = std::env::current_exe().expect("test binary");
    TestDir::under(
        &beside.parent().expect("deps").join("xtask-check-unit"),
        name,
    )
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, bytes).expect("write");
}

/// A machine that answers for `apt-get`, `dpkg`, `dpkg-query`, `getent`, `curl` and the
/// installed binary, and does to a scratch root what each would do to a real one.
struct Machine<'a> {
    root: PathBuf,
    package: &'a Package,
    calls: RefCell<Vec<String>>,
    arch: &'static str,
    keyring: Vec<u8>,
    dpkg_version: &'static str,
    says: Option<String>,
    /// Files below the root that the install does not create.
    omit: Vec<&'static str>,
    /// Does the install enable the unit?
    enable: bool,
    /// Links below `etc/systemd/system` the install creates besides the enabling one.
    extra_links: Vec<&'static str>,
    /// Does a process named like the program run?
    running: bool,
    /// Does `getent` know the user?
    user: bool,
    /// A command line (joined) containing this text is refused.
    refuse: Option<&'static str>,
    /// What each `apt-get update` says, in turn: the standard error of a failure, or `None` for
    /// one that succeeds. Once they are used up, every update succeeds.
    updates: RefCell<VecDeque<Option<&'static str>>>,
}

impl<'a> Machine<'a> {
    fn new(root: &Path, package: &'a Package) -> Machine<'a> {
        Machine {
            root: root.to_path_buf(),
            package,
            calls: RefCell::new(Vec::new()),
            arch: "amd64",
            keyring: vec![0x99, 0x01, 0x0d],
            dpkg_version: "1.2.3",
            says: None,
            omit: Vec::new(),
            enable: package.enabled_after_install,
            extra_links: Vec::new(),
            running: false,
            user: true,
            refuse: None,
            updates: RefCell::new(VecDeque::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn install(&self) {
        for file in self.package.files {
            if !self.omit.contains(file) {
                put(&self.root, file.trim_start_matches('/'), b"file");
            }
        }
        if self.enable {
            for unit in self.package.units {
                put(
                    &self.root,
                    &format!("etc/systemd/system/multi-user.target.wants/{unit}"),
                    b"link",
                );
            }
        }
        for link in &self.extra_links {
            put(&self.root, link, b"link");
        }
        // What else an administrator leaves in the directory: a file that is no directory of
        // links, and a directory of links to other units.
        put(&self.root, "etc/systemd/system/notes.txt", b"notes");
        put(
            &self.root,
            "etc/systemd/system/getty.target.wants/getty@tty1.service",
            b"link",
        );
        if self.running {
            put(
                &self.root,
                "proc/42/comm",
                format!("{}\n", self.package.comm()).as_bytes(),
            );
        }
        put(&self.root, "proc/43/comm", b"bash\n");
    }
}

fn answer(stdout: &str) -> Result<Answer, Error> {
    Ok(Answer {
        ok: true,
        stdout: stdout.to_owned(),
        stderr: String::new(),
    })
}

impl Host for Machine<'_> {
    fn run(&self, program: &Path, args: &[String]) -> Result<Answer, Error> {
        let name = program
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        let line = format!("{name} {}", args.join(" "));
        self.calls.borrow_mut().push(line.clone());
        if let Some(text) = self.refuse.filter(|text| line.contains(text)) {
            return Ok(Answer {
                ok: false,
                stdout: String::new(),
                stderr: format!("refused: {text}\n"),
            });
        }
        match (name.as_str(), args.first().map(String::as_str)) {
            ("apt-get", Some("update")) => match self.updates.borrow_mut().pop_front().flatten() {
                Some(stderr) => Ok(Answer {
                    ok: false,
                    stdout: String::new(),
                    stderr: stderr.to_owned(),
                }),
                None => answer(""),
            },
            ("dpkg", _) => answer(&format!("{}\n", self.arch)),
            ("dpkg-query", _) => answer(self.dpkg_version),
            ("apt-get", Some("install")) if line.contains(&format!("{}=", self.package.name)) => {
                self.install();
                answer("")
            }
            ("getent", _) => Ok(Answer {
                ok: self.user,
                stdout: String::new(),
                stderr: String::new(),
            }),
            ("curl", _) => {
                let at = args.iter().position(|a| a == "--output").expect("--output");
                let path = Path::new(&args[at + 1]);
                std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
                std::fs::write(path, &self.keyring).expect("keyring");
                answer("")
            }
            (program, Some("--version")) if program == self.package.program() => answer(
                &self
                    .says
                    .clone()
                    .unwrap_or_else(|| format!("{program} 1.2.3+0123456789ab\n")),
            ),
            _ => answer(""),
        }
    }
}

fn apt<'a>(root: &'a Path, package: &'a Package) -> Apt<'a> {
    Apt {
        root,
        package,
        arch: "amd64",
        version: "1.2.3",
        commit: COMMIT,
        apt_url: URL,
        pause: &no_pause,
    }
}

/// A pause no test waits out (the tests that hold the pauses record them).
fn no_pause(_: Duration) {}

fn program_names(machine: &Machine) -> Vec<String> {
    machine
        .calls()
        .iter()
        .map(|c| c.split(' ').next().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn a_clean_install_of_the_daemon_from_testing_adds_the_repository_and_installs_systemd_first() {
    let dir = scratch("daemon");
    let machine = Machine::new(&dir, daemon());
    let said = check_apt(&machine, &apt(&dir, daemon())).expect("checked");
    assert_eq!(
        machine.calls(),
        [
            "dpkg --print-architecture",
            "apt-get update",
            "apt-get install -y --no-install-recommends ca-certificates curl",
            &format!(
                "curl --proto =https --tlsv1.2 --fail --silent --show-error --create-dirs --output {}/{KEYRING} https://packages.komira.ai/apt/komira-archive-keyring.gpg",
                dir.display()
            ),
            "apt-get update",
            "apt-get install -y --no-install-recommends systemd",
            "apt-get install -y --no-install-recommends hostkeeper=1.2.3",
            "dpkg-query --show --showformat=${Version} hostkeeper",
            "hostkeeper --version",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(SOURCES)).expect("source"),
        "Types: deb\nURIs: https://packages.komira.ai/apt\nSuites: testing\nComponents: main\nArchitectures: amd64\nSigned-By: /usr/share/keyrings/komira-archive-keyring.gpg\n"
    );
    assert_eq!(
        said,
        [
            "check-apt: hostkeeper 1.2.3 installed from the suite testing on amd64; it says \"hostkeeper 1.2.3+0123456789ab\"; its 6 files are there; the service is enabled and not running"
        ]
    );
}

#[test]
fn a_clean_install_of_the_coordinator_installs_no_prerequisite_and_finds_its_user() {
    let dir = scratch("coordinator");
    let machine = Machine::new(&dir, coordinator());
    let said = check_apt(&machine, &apt(&dir, coordinator())).expect("checked");
    assert_eq!(
        program_names(&machine),
        [
            "dpkg",
            "apt-get",
            "apt-get",
            "curl",
            "apt-get",
            "apt-get",
            "dpkg-query",
            "hostkeeper-coordinator",
            "getent",
        ]
    );
    assert!(
        machine
            .calls()
            .contains(&"getent passwd hostkeeper-coordinator".to_owned())
    );
    assert!(machine.calls().contains(
        &"apt-get install -y --no-install-recommends hostkeeper-coordinator=1.2.3".to_owned()
    ));
    assert_eq!(
        said,
        [
            "check-apt: hostkeeper-coordinator 1.2.3 installed from the suite testing on amd64; it says \"hostkeeper-coordinator 1.2.3+0123456789ab\"; its 6 files are there; the user hostkeeper-coordinator exists; the service is not enabled and not running"
        ]
    );
}

#[test]
fn the_daemon_has_no_user_to_look_for() {
    let dir = scratch("no-user");
    let machine = Machine::new(&dir, daemon());
    check_apt(&machine, &apt(&dir, daemon())).expect("checked");
    assert!(!program_names(&machine).contains(&"getent".to_owned()));
}

#[test]
fn a_package_missing_files_is_named_file_by_file() {
    for package in [daemon(), coordinator()] {
        let dir = scratch("files");
        let mut machine = Machine::new(&dir, package);
        machine.omit = package.files.to_vec();
        let Err(Error::Refused(problems)) = check_apt(&machine, &apt(&dir, package)) else {
            panic!("not refused");
        };
        let want: Vec<String> = package
            .files
            .iter()
            .map(|f| format!("{f} is not installed"))
            .collect();
        assert_eq!(problems[..want.len()], want[..], "{}", package.name);
    }
}

#[test]
fn a_user_the_install_did_not_create_is_named() {
    let dir = scratch("user");
    let mut machine = Machine::new(&dir, coordinator());
    machine.user = false;
    let Err(Error::Refused(problems)) = check_apt(&machine, &apt(&dir, coordinator())) else {
        panic!("not refused");
    };
    assert_eq!(problems, ["the user hostkeeper-coordinator does not exist"]);
}

#[test]
fn a_coordinator_that_the_install_enabled_or_started_is_named() {
    // The coordinator's postinstall enables and starts nothing: it cannot start unconfigured.
    let dir = scratch("coordinator-enabled");
    let mut machine = Machine::new(&dir, coordinator());
    machine.enable = true;
    machine.extra_links = vec!["etc/systemd/system/hostkeeper-coordinator.service"];
    machine.running = true;
    let Err(Error::Refused(problems)) = check_apt(&machine, &apt(&dir, coordinator())) else {
        panic!("not refused");
    };
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(
        problems[0].starts_with("the service is enabled: "),
        "{problems:?}"
    );
    assert!(
        problems[0].ends_with("etc/systemd/system/hostkeeper-coordinator.service exists, and the package enables nothing"),
        "{problems:?}"
    );
    assert!(
        problems[1].ends_with("etc/systemd/system/multi-user.target.wants/hostkeeper-coordinator.service exists, and the package enables nothing"),
        "{problems:?}"
    );
    assert_eq!(
        problems[2],
        "the service is running (process 42), and the package starts nothing"
    );
}

#[test]
fn a_daemon_that_the_install_did_not_enable_is_named_and_one_that_runs_is_too() {
    // The daemon's postinstall enables the unit on a first install, and never starts it. The
    // process is found by the 15 bytes the kernel keeps of a longer name too.
    let dir = scratch("daemon-not-enabled");
    let mut machine = Machine::new(&dir, daemon());
    machine.enable = false;
    machine.running = true;
    let Err(Error::Refused(problems)) = check_apt(&machine, &apt(&dir, daemon())) else {
        panic!("not refused");
    };
    assert_eq!(
        problems,
        [
            "the service is not enabled: no link to hostkeeper.service is in etc/systemd/system, and the package enables it on a first install",
            "the service is running (process 42), and the package starts nothing"
        ]
    );
    let dir = scratch("coordinator-running");
    let mut machine = Machine::new(&dir, coordinator());
    machine.running = true;
    assert_eq!(coordinator().comm(), "hostkeeper-coor");
    let Err(Error::Refused(problems)) = check_apt(&machine, &apt(&dir, coordinator())) else {
        panic!("not refused");
    };
    assert_eq!(
        problems,
        ["the service is running (process 42), and the package starts nothing"]
    );
}

#[test]
fn a_package_at_another_version_or_a_binary_that_says_another_is_refused() {
    let dir = scratch("version");
    let mut machine = Machine::new(&dir, daemon());
    machine.dpkg_version = "1.2.2";
    let Err(Error::Refused(problems)) = check_apt(&machine, &apt(&dir, daemon())) else {
        panic!("not refused");
    };
    assert_eq!(
        problems,
        ["dpkg says hostkeeper \"1.2.2\" is installed, not \"1.2.3\""]
    );
    for (says, shown) in [
        (
            "hostkeeper 1.2.3+ffffffffffff\n",
            "hostkeeper 1.2.3+ffffffffffff\\n",
        ),
        (
            "hostkeeper-coordinator 1.2.3+0123456789ab\n",
            "hostkeeper-coordinator 1.2.3+0123456789ab\\n",
        ),
        (
            "hostkeeper 1.2.3+0123456789ab\n\n",
            "hostkeeper 1.2.3+0123456789ab\\n\\n",
        ),
    ] {
        let dir = scratch("says");
        let mut machine = Machine::new(&dir, daemon());
        machine.says = Some(says.to_owned());
        let e = check_apt(&machine, &apt(&dir, daemon()))
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            format!("the binary says \"{shown}\", not \"hostkeeper 1.2.3+0123456789ab\"")
        );
    }
    // No trailing line break is as good as one.
    let dir = scratch("says-bare");
    let mut machine = Machine::new(&dir, daemon());
    machine.says = Some("hostkeeper 1.2.3+0123456789ab".to_owned());
    check_apt(&machine, &apt(&dir, daemon())).expect("checked");
}

#[test]
fn an_armored_or_empty_keyring_is_refused_because_signed_by_takes_a_gpg_file() {
    for keyring in [b"-----BEGIN PGP PUBLIC KEY BLOCK-----".to_vec(), Vec::new()] {
        let dir = scratch("keyring");
        let mut machine = Machine::new(&dir, daemon());
        machine.keyring = keyring;
        let e = check_apt(&machine, &apt(&dir, daemon()))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("komira-archive-keyring.gpg is not a binary keyring"),
            "{e}"
        );
        assert!(!dir.join(SOURCES).exists());
    }
}

#[test]
fn a_program_that_fails_is_named_with_what_it_said_and_a_machine_of_another_architecture_is_refused()
 {
    for (refuse, program) in [
        ("update", "apt-get update"),
        ("ca-certificates", "ca-certificates"),
        ("--output", "--output"),
        ("systemd", "systemd"),
        ("hostkeeper=", "hostkeeper=1.2.3"),
        ("--showformat", "dpkg-query"),
        ("--version", "hostkeeper --version"),
        ("--print-architecture", "dpkg --print-architecture"),
    ] {
        let dir = scratch("refused");
        let mut machine = Machine::new(&dir, daemon());
        machine.refuse = Some(refuse);
        let e = check_apt(&machine, &apt(&dir, daemon()))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains(program) && e.contains(&format!("failed: refused: {refuse}")),
            "{refuse}: {e}"
        );
    }
    let dir = scratch("arch");
    let mut machine = Machine::new(&dir, daemon());
    machine.arch = "arm64";
    assert_eq!(
        check_apt(&machine, &apt(&dir, daemon()))
            .unwrap_err()
            .to_string(),
        "this machine is arm64, not amd64: the check would install another package"
    );
}

#[test]
fn flags_out_of_their_form_are_usage_errors_before_anything_runs() {
    let dir = scratch("usage");
    let machine = Machine::new(&dir, daemon());
    for change in [
        |r: &mut Apt| r.version = "v1.2.3",
        |r: &mut Apt| r.arch = "i386",
        |r: &mut Apt| r.apt_url = "http://packages.komira.ai/apt",
        |r: &mut Apt| r.apt_url = "https://packages.komira.ai/apt/",
        |r: &mut Apt| r.commit = "0123",
    ] {
        let mut r = apt(&dir, daemon());
        change(&mut r);
        assert!(matches!(check_apt(&machine, &r), Err(Error::Usage(_))));
    }
    assert!(machine.calls().is_empty());
}

#[test]
fn a_package_is_one_of_the_products() {
    assert_eq!(
        package_flag("hostkeeper").expect("daemon").name,
        "hostkeeper"
    );
    assert_eq!(
        package_flag("other-product").unwrap_err(),
        Error::Usage(
            "--package \"other-product\" is none of [\"hostkeeper\", \"hostkeeper-coordinator\", \"hello\", \"pxe\"]"
                .to_owned()
        )
    );
}

#[test]
fn the_programs_of_the_checked_system_run_through_the_port_with_a_noninteractive_frontend() {
    let exec = FakeExec::says("done");
    let answer = Noninteractive(&exec)
        .run(
            Path::new("apt-get"),
            &["install".to_owned(), "-y".to_owned()],
        )
        .expect("answer");
    assert_eq!(answer.stdout, "done");
    assert_eq!(
        *exec.calls.borrow(),
        [(
            "env".to_owned(),
            vec![
                "DEBIAN_FRONTEND=noninteractive".to_owned(),
                "apt-get".to_owned(),
                "install".to_owned(),
                "-y".to_owned()
            ]
        )]
    );
}

#[test]
fn hellos_clean_install_says_its_version_without_a_commit_and_enables_neither_slot() {
    // Catches: hello held to a `--version` with the commit (hello prints `hello <version>`), a
    // prerequisite installed first for a package that declares none, and hello's files counted
    // from another package.
    let dir = scratch("hello");
    let mut machine = Machine::new(&dir, hello());
    machine.says = Some("hello 1.2.3\n".to_owned());
    let said = check_apt(&machine, &apt(&dir, hello())).expect("checked");
    assert_eq!(
        program_names(&machine),
        [
            "dpkg",
            "apt-get",
            "apt-get",
            "curl",
            "apt-get",
            "apt-get",
            "dpkg-query",
            "hello",
        ]
    );
    assert!(
        machine
            .calls()
            .contains(&"apt-get install -y --no-install-recommends hello=1.2.3".to_owned())
    );
    assert_eq!(
        said,
        [
            "check-apt: hello 1.2.3 installed from the suite testing on amd64; it says \"hello 1.2.3\"; its 3 files are there; the service is not enabled and not running"
        ]
    );
    // The stamp of the shipped binaries, with a commit, is not what hello says.
    let dir = scratch("hello-commit");
    let mut machine = Machine::new(&dir, hello());
    machine.says = Some("hello 1.2.3+0123456789ab\n".to_owned());
    assert_eq!(
        check_apt(&machine, &apt(&dir, hello())).unwrap_err(),
        Error::Failed(
            "the binary says \"hello 1.2.3+0123456789ab\\n\", not \"hello 1.2.3\"".to_owned()
        )
    );
}

#[test]
fn each_unit_of_hello_is_checked_and_an_enabled_second_slot_is_named() {
    // Catches: only the first of a package's units held to the spec, so the install enabling
    // `hello@b.service` would pass.
    let dir = scratch("hello-b-enabled");
    let mut machine = Machine::new(&dir, hello());
    machine.says = Some("hello 1.2.3\n".to_owned());
    machine.extra_links = vec!["etc/systemd/system/multi-user.target.wants/hello@b.service"];
    let Err(Error::Refused(problems)) = check_apt(&machine, &apt(&dir, hello())) else {
        panic!("not refused");
    };
    assert_eq!(
        problems,
        [format!(
            "the service is enabled: {}/etc/systemd/system/multi-user.target.wants/hello@b.service exists, and the package enables nothing",
            dir.display()
        )]
    );
}

#[test]
fn an_architecture_the_package_is_not_built_for_is_a_usage_error_before_anything_runs() {
    // Catches: an arm64 check of a package built for amd64 alone installing nothing and failing
    // as if the repository were wrong.
    let dir = scratch("amd64-only-arm64");
    let mut machine = Machine::new(&dir, amd64_only());
    machine.arch = "arm64";
    let mut r = apt(&dir, amd64_only());
    r.arch = "arm64";
    assert_eq!(
        check_apt(&machine, &r).unwrap_err(),
        Error::Usage("--arch \"arm64\": amd64-only is not built for it".to_owned())
    );
    assert!(machine.calls().is_empty());
}

#[test]
fn the_install_check_matrix_is_every_package_on_each_of_its_architectures() {
    // Catches: a package the release publishes to `testing` that no install check installs
    // (hello's or pxe's would be promoted to `stable` unchecked), or a check asked for an
    // architecture its package is not built for (which the verb refuses).
    const RELEASE_YML: &str = include_str!("../../../../../.github/workflows/release.yml");
    let job = RELEASE_YML
        .split("\n  check-apt:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\n").next())
        .expect("release.yml has the job check-apt");
    let list = |key: &str| -> Vec<&str> {
        job.lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{key}: [")))
            .and_then(|l| l.strip_suffix(']'))
            .expect("a flow list")
            .split(", ")
            .collect()
    };
    let excluded: Vec<(&str, &str)> = job
        .split("exclude:\n")
        .nth(1)
        .and_then(|rest| rest.split("include:").next())
        .unwrap_or_default()
        .split("- package: ")
        .skip(1)
        .filter_map(|e| {
            let (package, rest) = e.split_once('\n')?;
            Some((package.trim(), rest.trim().strip_prefix("arch: ")?.trim()))
        })
        .collect();
    let mut checked: Vec<(&str, &str)> = list("package")
        .into_iter()
        .flat_map(|p| list("arch").into_iter().map(move |a| (p, a)))
        .filter(|pair| !excluded.contains(pair))
        .collect();
    checked.sort_unstable();
    let mut want: Vec<(&str, &str)> = spec::HOSTKEEPER
        .packages
        .iter()
        .flat_map(|p| p.arches.iter().map(move |a| (p.name, a.deb)))
        .collect();
    want.sort_unstable();
    assert_eq!(checked, want);
}

/// What `apt-get update` said on a hosted runner when security.ubuntu.com served an older
/// `Packages.gz` than its `InRelease` names.
const MIRROR_SYNC: &str = "E: Failed to fetch http://security.ubuntu.com/ubuntu/dists/noble-security/universe/binary-amd64/Packages.gz  File has unexpected size (1563110 != 1563114). Mirror sync in progress? [IP: 192.0.2.82 80]
   Hashes of expected file:
    - Filesize:1563114 [weak]
    - SHA256:516e96ddd1615703858a543cf31e4a54212c7e7252c57285c14065709683c8ee
   Release file created at: Thu, 26 Nov 2026 07:24:15 +0000
E: Some index files failed to download. They have been ignored, or old ones used instead.
";

/// The same failure as apt words a hash mismatch, on two Ubuntu mirrors at once.
const HASH_MISMATCH: &str = "E: Failed to fetch http://archive.ubuntu.com/ubuntu/dists/noble-updates/main/binary-amd64/Packages.gz  Hash Sum mismatch
   Hashes of expected file:
    - SHA256:516e96ddd1615703858a543cf31e4a54212c7e7252c57285c14065709683c8ee
E: Failed to fetch http://security.ubuntu.com/ubuntu/dists/noble-security/main/binary-amd64/Packages.gz  File has unexpected size (10 != 14). Mirror sync in progress?
W: Some warning apt gives that decides nothing
E: Some index files failed to download. They have been ignored, or old ones used instead.
";

/// Runs the check of the daemon on a machine whose `apt-get update`s answer `updates` in turn:
/// the machine's calls, the pauses waited out, and what the check returned.
fn with_updates(
    name: &str,
    updates: &[Option<&'static str>],
) -> (Vec<String>, Vec<Duration>, Result<Vec<String>, Error>) {
    let dir = scratch(name);
    let machine = Machine::new(&dir, daemon());
    machine.updates.borrow_mut().extend(updates.iter().copied());
    let pauses = RefCell::new(Vec::new());
    let pause = |wait| pauses.borrow_mut().push(wait);
    let checked = check_apt(
        &machine,
        &Apt {
            pause: &pause,
            ..apt(&dir, daemon())
        },
    );
    (machine.calls(), pauses.into_inner(), checked)
}

fn updates_of(calls: &[String]) -> usize {
    calls.iter().filter(|c| *c == "apt-get update").count()
}

#[test]
fn an_ubuntu_mirror_out_of_sync_is_updated_again_after_each_pause_and_the_check_goes_on() {
    let (calls, pauses, checked) =
        with_updates("mirror-sync", &[Some(MIRROR_SYNC), Some(HASH_MISMATCH)]);
    assert_eq!(pauses, [Duration::from_secs(30), Duration::from_secs(60)]);
    assert_eq!(
        calls[..5],
        [
            "dpkg --print-architecture",
            "apt-get update",
            "apt-get update",
            "apt-get update",
            "apt-get install -y --no-install-recommends ca-certificates curl",
        ]
    );
    assert_eq!(updates_of(&calls), 4);
    assert_eq!(
        checked.expect("checked"),
        [
            "::notice title=check-apt::apt-get update found an Ubuntu mirror out of sync with its Release file; run again after 30s",
            "::notice title=check-apt::apt-get update found an Ubuntu mirror out of sync with its Release file; run again after 60s",
            "check-apt: hostkeeper 1.2.3 installed from the suite testing on amd64; it says \"hostkeeper 1.2.3+0123456789ab\"; its 6 files are there; the service is enabled and not running",
        ]
    );
}

#[test]
fn the_update_after_the_repository_is_added_is_updated_again_too_and_its_pauses_start_over() {
    let (calls, pauses, checked) = with_updates("second-update", &[None, Some(MIRROR_SYNC), None]);
    assert_eq!(pauses, [Duration::from_secs(30)]);
    assert_eq!(updates_of(&calls), 3);
    let after_keyring = calls
        .iter()
        .position(|c| c.starts_with("curl "))
        .expect("curl");
    assert_eq!(
        calls[after_keyring + 1..after_keyring + 4],
        [
            "apt-get update",
            "apt-get update",
            "apt-get install -y --no-install-recommends systemd",
        ]
    );
    assert_eq!(checked.expect("checked").len(), 2);
}

#[test]
fn a_mirror_out_of_sync_through_every_pause_fails_saying_how_many_times_it_ran() {
    let (calls, pauses, checked) = with_updates("always-out-of-sync", &[Some(MIRROR_SYNC); 4]);
    assert_eq!(pauses, UPDATE_PAUSES);
    assert_eq!(updates_of(&calls), 4);
    assert!(!calls.iter().any(|c| c.contains("install")));
    assert_eq!(
        checked.unwrap_err().to_string(),
        format!(
            "apt-get update failed (run 4 times): {}",
            MIRROR_SYNC.trim_end()
        )
    );
}

#[test]
fn a_failed_update_that_is_no_ubuntu_mirror_out_of_sync_fails_at_once_without_a_pause() {
    for (case, stderr) in [
        (
            "the repository's own index out of sync is what the check is for",
            "E: Failed to fetch https://packages.komira.ai/apt/dists/testing/main/binary-amd64/Packages.gz  Hash Sum mismatch\nE: Some index files failed to download. They have been ignored, or old ones used instead.",
        ),
        (
            "the repository's index beside a mirror out of sync",
            "E: Failed to fetch http://security.ubuntu.com/ubuntu/dists/noble-security/main/binary-amd64/Packages.gz  File has unexpected size (10 != 14). Mirror sync in progress?\nE: Failed to fetch https://packages.komira.ai/apt/dists/testing/main/binary-amd64/Packages.gz  File has unexpected size (10 != 14). Mirror sync in progress?",
        ),
        (
            "a fetch that failed for another reason",
            "E: Failed to fetch http://archive.ubuntu.com/ubuntu/dists/noble/InRelease  Could not resolve 'archive.ubuntu.com'\nE: Some index files failed to download. They have been ignored, or old ones used instead.",
        ),
        (
            "another error beside a mirror out of sync",
            "E: Failed to fetch http://security.ubuntu.com/ubuntu/dists/noble-security/main/binary-amd64/Packages.gz  Hash Sum mismatch\nE: The repository 'https://packages.komira.ai/apt testing InRelease' is not signed.",
        ),
        (
            "an error with no failed fetch",
            "E: Could not get lock /var/lib/apt/lists/lock. It is held by process 7 (apt-get)",
        ),
        (
            "apt's closing line alone",
            "E: Some index files failed to download. They have been ignored, or old ones used instead.",
        ),
        ("warnings alone", "W: Some warning apt gives"),
        ("nothing said", ""),
    ] {
        let (calls, pauses, checked) = with_updates("final", &[Some(stderr)]);
        assert!(pauses.is_empty(), "{case}");
        assert_eq!(updates_of(&calls), 1, "{case}");
        assert_eq!(
            checked.unwrap_err().to_string(),
            format!("apt-get update failed: {stderr}"),
            "{case}"
        );
    }
}

#[test]
fn a_mirror_out_of_sync_then_another_failure_fails_with_the_last_answer() {
    let (calls, pauses, checked) = with_updates(
        "then-final",
        &[Some(MIRROR_SYNC), Some("E: Could not get lock")],
    );
    assert_eq!(pauses, [Duration::from_secs(30)]);
    assert_eq!(updates_of(&calls), 2);
    assert_eq!(
        checked.unwrap_err().to_string(),
        "apt-get update failed (run 2 times): E: Could not get lock"
    );
}

#[test]
fn both_updates_out_of_sync_each_have_all_the_pauses() {
    let (calls, pauses, checked) = with_updates(
        "both-updates",
        &[
            Some(MIRROR_SYNC),
            Some(MIRROR_SYNC),
            None,
            Some(MIRROR_SYNC),
            Some(MIRROR_SYNC),
            Some(MIRROR_SYNC),
            None,
        ],
    );
    assert_eq!(pauses, [30, 60, 30, 60, 120].map(Duration::from_secs));
    assert_eq!(updates_of(&calls), 7);
    assert_eq!(checked.expect("checked").len(), 6);
}

#[test]
fn a_mirror_whose_url_only_starts_like_the_repository_is_another_mirror() {
    let (calls, pauses, checked) = with_updates(
        "prefix",
        &[Some(
            "E: Failed to fetch https://packages.komira.ai/apt-other/dists/testing/main/binary-amd64/Packages.gz  Hash Sum mismatch",
        )],
    );
    assert_eq!(pauses, [Duration::from_secs(30)]);
    assert_eq!(updates_of(&calls), 3);
    checked.expect("checked");
}
