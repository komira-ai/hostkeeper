//! The tests of the collector `mac`: over a scripted `fdesetup` and `pkgutil`, a fake data volume
//! and a scratch directory standing for the Mac's files, FileVault's state, the authenticated
//! restart, the free space and the Command Line Tools as reported, and why it fails; each section
//! and reason asserted by equality.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::maintenance_collect::LinuxScope;

const ON: &str = include_str!("../tests/fixtures/maintenance/mac/fdesetup_on.txt");
const OFF: &str = include_str!("../tests/fixtures/maintenance/mac/fdesetup_off.txt");
const ENCRYPTING: &str = include_str!("../tests/fixtures/maintenance/mac/fdesetup_encrypting.txt");
const DECRYPTING: &str = include_str!("../tests/fixtures/maintenance/mac/fdesetup_decrypting.txt");
const DEFERRED: &str = include_str!("../tests/fixtures/maintenance/mac/fdesetup_deferred.txt");
const PKGUTIL_CLT: &str = include_str!("../tests/fixtures/maintenance/mac/pkgutil_clt.txt");

/// The free bytes the fake data volume reports.
const FREE: u64 = 123_456_789_012;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

fn pkg_info_args() -> Vec<String> {
    vec![format!("--pkg-info={CLT_PACKAGE}")]
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A data volume whose free space is what the test says.
struct FakeSpace(Result<u64, String>);

impl FreeSpace for FakeSpace {
    fn free_bytes(&self) -> Result<u64, String> {
        self.0.clone()
    }
}

/// The scripted programs and files of a Mac: `fdesetup status` writes `status`,
/// `supportsauthrestart` writes `authrestart`, and `pkgutil` knows the tools' receipt; the
/// compiler is on disk when `clang` is.
struct Mac {
    fdesetup: FakeLaunchctl,
    pkgutil: FakeLaunchctl,
    scratch: Scratch,
}

impl Mac {
    fn new(name: &str, status: &str, authrestart: &str, clang: bool) -> Mac {
        let mac = Mac {
            fdesetup: FakeLaunchctl::new(),
            pkgutil: FakeLaunchctl::new(),
            scratch: scratch(name),
        };
        mac.fdesetup
            .always(&args(&STATUS_ARGS), Ok(LaunchctlOutput::success(status)));
        mac.fdesetup.always(
            &args(&AUTHRESTART_ARGS),
            Ok(LaunchctlOutput::success(authrestart)),
        );
        mac.pkgutil
            .always(&pkg_info_args(), Ok(LaunchctlOutput::success(PKGUTIL_CLT)));
        if clang {
            let file = mac.scratch.path().join(CLANG.trim_start_matches('/'));
            fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
            fs::write(file, "").expect("the compiler");
        }
        mac
    }

    fn collect_with(&self, space: Result<u64, String>) -> Result<Section, String> {
        let own = BTreeSet::new();
        MacReadiness::new(MacPorts {
            fdesetup: Box::new(self.fdesetup.clone()),
            pkgutil: Box::new(self.pkgutil.clone()),
            space: Box::new(FakeSpace(space)),
            root: HostRoot::at(self.scratch.path()),
        })
        .collect(&Look {
            scope: LinuxScope::Security,
            own_packages: &own,
        })
    }

    fn collect(&self) -> Result<Section, String> {
        self.collect_with(Ok(FREE))
    }
}

fn tools(receipt: bool, files: bool, version: &str) -> pb::CommandLineToolsState {
    pb::CommandLineToolsState {
        receipt,
        files,
        version: version.to_owned(),
    }
}

fn facts(
    filevault: pb::FileVaultState,
    authrestart_supported: bool,
    command_line_tools: pb::CommandLineToolsState,
) -> pb::MacUpdateFacts {
    pb::MacUpdateFacts {
        filevault: filevault.into(),
        authrestart_supported,
        data_free_bytes: Some(FREE),
        command_line_tools: Some(command_line_tools),
        ..pb::MacUpdateFacts::default()
    }
}

/// Proves a Mac with FileVault on that supports an authenticated restart, the tools' receipt and
/// their compiler reports exactly that, the free bytes the data volume gives, and the receipt's
/// version; no host fact and nothing due.
#[test]
fn filevault_on_with_authrestart_is_reported_exactly() {
    let mac = Mac::new("mac-on", ON, "true\n", true);
    assert_eq!(
        mac.collect(),
        Ok(Section {
            mac: Some(facts(
                pb::FileVaultState::On,
                true,
                tools(true, true, "26.4.0.0.1.1771234567"),
            )),
            ..Section::default()
        })
    );
    assert_eq!(
        mac.fdesetup.calls(),
        [args(&STATUS_ARGS), args(&AUTHRESTART_ARGS)]
    );
    assert_eq!(mac.pkgutil.calls(), [pkg_info_args()]);
}

/// Proves a Mac with FileVault off and no authenticated restart reports both off; a Mac with no
/// receipt (`pkgutil` exits 1) and no compiler reports no tools and no version.
#[test]
fn filevault_off_without_authrestart_is_reported_exactly() {
    let mac = Mac::new("mac-off", OFF, "false\n", false);
    mac.pkgutil.always(
        &pkg_info_args(),
        Ok(LaunchctlOutput::exit(
            1,
            "No receipt for 'com.apple.pkg.CLTools_Executables' found at '/'.\n",
        )),
    );
    assert_eq!(
        mac.collect(),
        Ok(Section {
            mac: Some(facts(
                pb::FileVaultState::Off,
                false,
                tools(false, false, "")
            )),
            ..Section::default()
        })
    );
}

/// Proves the compiler on disk without a receipt is reported as files and no receipt.
#[test]
fn tools_on_disk_without_a_receipt_are_files_alone() {
    let mac = Mac::new("mac-files", OFF, "false\n", true);
    mac.pkgutil
        .always(&pkg_info_args(), Ok(LaunchctlOutput::exit(1, "")));
    assert_eq!(
        mac.collect().map(|section| section.mac),
        Ok(Some(facts(
            pb::FileVaultState::Off,
            false,
            tools(false, true, "")
        )))
    );
}

/// Proves each state `fdesetup status` writes is read, a conversion winning over the line before
/// it, and the deferred enablement's lines known.
#[test]
fn each_filevault_state_is_read() {
    let cases = [
        (ON, pb::FileVaultState::On),
        (OFF, pb::FileVaultState::Off),
        (ENCRYPTING, pb::FileVaultState::Encrypting),
        (DECRYPTING, pb::FileVaultState::Decrypting),
        (DEFERRED, pb::FileVaultState::Off),
        ("", pb::FileVaultState::Unspecified),
    ];
    for (text, state) in cases {
        assert_eq!(parse_fdesetup_status(text), (state, 0), "{text}");
    }
    assert_eq!(
        parse_fdesetup_status("Decryption in progress: 3\nFileVault is On.\n"),
        (pb::FileVaultState::Decrypting, 0)
    );
}

/// Proves a line `fdesetup` writes that is not known is counted, and with no state line the state
/// is unspecified; an answer of `supportsauthrestart` that is neither `true` nor `false` is
/// counted and read as no.
#[test]
fn unknown_fdesetup_answers_are_counted() {
    assert_eq!(
        parse_fdesetup_status("FileVault is maybe.\n"),
        (pb::FileVaultState::Unspecified, 1)
    );
    assert_eq!(
        parse_fdesetup_status("FileVault is On.\nSomething else.\n"),
        (pb::FileVaultState::On, 1)
    );
    assert_eq!(parse_supportsauthrestart("true"), (true, 0));
    assert_eq!(parse_supportsauthrestart(" false\n"), (false, 0));
    assert_eq!(parse_supportsauthrestart("yes\n"), (false, 1));

    let mac = Mac::new("mac-unknown", "FileVault is maybe.\n", "yes\n", false);
    let section = mac.collect().expect("collects");
    assert_eq!(section.unparsed, 2);
    assert_eq!(
        section
            .mac
            .map(|mac| (mac.filevault, mac.authrestart_supported)),
        Some((i32::from(pb::FileVaultState::Unspecified), false))
    );
}

/// Proves an `fdesetup` that fails or does not finish by its deadline, a `pkgutil` that does not
/// run, and a data volume whose free space cannot be read each fail the collector with its reason.
#[test]
fn what_cannot_be_read_fails_the_collector() {
    let mac = Mac::new("mac-fail-status", ON, "true\n", true);
    mac.fdesetup.always(
        &args(&STATUS_ARGS),
        Ok(LaunchctlOutput::exit(1, "Error: not root\n")),
    );
    assert_eq!(
        mac.collect(),
        Err("fdesetup status failed with exit code 1: Error: not root".to_owned())
    );

    let mac = Mac::new("mac-fail-authrestart", ON, "true\n", true);
    mac.fdesetup.always(
        &args(&AUTHRESTART_ARGS),
        Err(LaunchctlError::TimedOut {
            args: "supportsauthrestart".to_owned(),
        }),
    );
    assert_eq!(
        mac.collect(),
        Err("fdesetup supportsauthrestart did not finish within its deadline".to_owned())
    );

    let mac = Mac::new("mac-fail-space", ON, "true\n", true);
    assert_eq!(
        mac.collect_with(Err(
            "statvfs of /System/Volumes/Data failed: gone".to_owned()
        )),
        Err("statvfs of /System/Volumes/Data failed: gone".to_owned())
    );
    assert_eq!(mac.pkgutil.calls(), Vec::<Vec<String>>::new());

    let mac = Mac::new("mac-fail-pkgutil", ON, "true\n", true);
    mac.pkgutil.always(
        &pkg_info_args(),
        Err(LaunchctlError::Spawn {
            args: "--pkg-info".to_owned(),
            why: "no such file".to_owned(),
        }),
    );
    assert_eq!(
        mac.collect(),
        Err("pkgutil --pkg-info could not be run: no such file".to_owned())
    );
}

/// Proves a receipt's version longer than the wire's bound is cut to it.
#[test]
fn the_receipt_version_is_cut_to_its_bound() {
    let mac = Mac::new("mac-long-version", ON, "true\n", false);
    let long = "9".repeat(MAX_MAC_TEXT + 5);
    mac.pkgutil.always(
        &pkg_info_args(),
        Ok(LaunchctlOutput::success(&format!("version: {long}\n"))),
    );
    let tools = mac
        .collect()
        .expect("collects")
        .mac
        .and_then(|mac| mac.command_line_tools)
        .expect("the tools");
    assert_eq!(tools.version.as_str(), &long[..MAX_MAC_TEXT]);
}

/// Proves the free space of a real directory is read through `statvfs`, and a path that is not
/// there fails, naming it.
#[test]
fn statvfs_reads_a_volume_and_names_a_missing_one() {
    let scratch = scratch("mac-statvfs");
    assert!(Statvfs::at(scratch.path()).free_bytes().is_ok());
    let missing = scratch.path().join("missing");
    assert_eq!(
        Statvfs::at(&missing).free_bytes(),
        Err(format!(
            "statvfs of {} failed: {}",
            missing.display(),
            rustix::io::Errno::NOENT
        ))
    );
    assert_eq!(
        Statvfs::data_volume(),
        Statvfs::at(Path::new("/System/Volumes/Data"))
    );
}

/// Proves the collector is named `mac`, which a failed look's reason starts with, and the host's
/// is made over the programs the module names.
#[test]
fn the_collector_is_named_mac_and_runs_the_macs_programs() {
    assert_eq!(MacReadiness::at_host().name(), "mac");
    assert_eq!(FDESETUP, "/usr/bin/fdesetup");
    assert_eq!(PKGUTIL, "/usr/sbin/pkgutil");
    assert_eq!(DATA_VOLUME, "/System/Volumes/Data");
    assert_eq!(CLANG, "/Library/Developer/CommandLineTools/usr/bin/clang");
}
