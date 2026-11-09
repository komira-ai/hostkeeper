//! The `mdm` package hook as the `.deb` runs it: the daemon runs the tree's
//! `usr/lib/mdm/hostkeeper-hook`, which runs `usr/lib/mdm/mdm update-hook` beside it, with the
//! daemon's version directory as the staged release. Run as that path, the program is the
//! `.deb`'s build: `prepare` checks the staged release's binaries in `tree/usr/lib/mdm`, makes
//! the CA with the `scepserver` there, and the settings may give `MDM_BIN` only as
//! `/usr/lib/mdm`. The same staged release run as the archive's program is refused.

mod common;
mod hook_support;

use std::ffi::OsString;

use common::SVC;
use hook_support::*;
use hostkeeper_hook::Class;
use hostkeeper_mdm_core::commands::scep_ca_init_args;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::package::{self, DEB_BIN, Layout};
use hostkeeper_mdm_step::Host;
use hostkeeper_mdm_step::fake::FakeHost;

/// The daemon's version directory of the `.deb` under test.
const VERSION_DIR: &str = "/var/lib/updater/state/packages/mdm/1.1.0";

/// The program as the `.deb`'s hook runs it: `usr/lib/mdm/mdm` of the staged tree.
fn deb_program() -> String {
    format!("{VERSION_DIR}/tree{DEB_BIN}/mdm")
}

/// A staged binary of the `.deb`.
fn staged(program: &str) -> String {
    format!("{VERSION_DIR}/tree{DEB_BIN}/{program}")
}

/// [`package_host`] with the `.deb`'s version directory staged, its lego answering every flag,
/// and its `scepserver` making the CA.
fn deb_host() -> FakeHost {
    let host = package_host();
    for program in ["scepserver", "lego", "nanohub", "mdm", "hostkeeper-hook"] {
        host.put_file(&staged(program), 0o755, (0, 0), program.as_bytes());
    }
    host.reply(&[&staged("lego"), "run", "--help"], 0, LEGO_HELP);
    host.reply(&[&staged("lego"), "renew", "--help"], 0, LEGO_RENEW_HELP);
    let config = MdmConfig::parse(
        &package::settings_of(&package_example(), Layout::Deb).expect("the example fits"),
    )
    .expect("parses");
    let mut argv: Vec<String> = ["/usr/sbin/runuser", "-u", "mdm", "--"]
        .map(str::to_owned)
        .to_vec();
    argv.push(staged("scepserver"));
    argv.extend(scep_ca_init_args(&config));
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    host.effect(&words, CA_KEY, 0o400, (SVC, SVC), b"KEY");
    host.effect(&words, CA, 0o400, (SVC, SVC), b"CA");
    host
}

/// `<program> update-hook prepare <version dir> --params <file>`, the params naming the version
/// directory as the staged release.
fn prepare_as(run: &Run, host: &FakeHost, program: &str) -> Ended {
    let params = run.scratch.path().join("deb.toml");
    let text = format!(
        "settings = \"{SETTINGS}\"\nrecord_dir = \"{}\"\n\"updater.staged_release\" = \"{VERSION_DIR}\"\n\"updater.target_version\" = \"1.1.0\"\n\"updater.artifact_digest\" = \"sha256:{}\"\n",
        run.record().display(),
        "ab".repeat(32)
    );
    std::fs::write(&params, text).expect("write the params file");
    let args: Vec<OsString> = [
        program,
        "update-hook",
        "prepare",
        VERSION_DIR,
        "--params",
        &params.display().to_string(),
    ]
    .map(OsString::from)
    .to_vec();
    run_mdm(host, &args)
}

/// Catches a `.deb` hook that looks for the staged release's binaries in the archive's `bin/`
/// or makes the CA with another `scepserver`: run as the tree's program, `prepare` succeeds,
/// checks lego in `tree/usr/lib/mdm` and runs that directory's `scepserver` for the CA.
#[test]
fn the_debs_prepare_uses_the_binaries_of_the_staged_tree() {
    let run = Run::new("deb-prepare");
    let host = deb_host();
    let ended = prepare_as(&run, &host, &deb_program());
    assert_eq!(ended.code, 0, "{}", ended.err);
    let calls = host.calls();
    let ran = |program: &str| calls.iter().any(|c| c.iter().any(|w| *w == program));
    assert!(ran(&staged("lego")), "{calls:?}");
    assert!(ran(&staged("scepserver")), "{calls:?}");
    assert!(!ran(SCEPSERVER) && !ran(LEGO), "{calls:?}");
}

/// Catches the build read from anything but the program's path: the same `.deb` staged release
/// run as the archive's program (`bin/mdm`) looks in `<version dir>/bin`, which a `.deb` does
/// not have, and fails `bad_artifact` (class `release`) before it changes anything.
#[test]
fn the_archives_program_refuses_a_debs_staged_release() {
    let run = Run::new("deb-as-archive");
    let host = deb_host();
    let ended = prepare_as(
        &run,
        &host,
        "/var/lib/hostkeeper/packages/mdm/current/bin/mdm",
    );
    assert_eq!(ended.code, 1, "{}", ended.err);
    let report = ended.report();
    assert_eq!(report.class, Class::Release);
    assert_eq!(report.reason.as_deref(), Some("bad_artifact"));
    assert_eq!(
        report.detail.as_deref(),
        Some(
            format!(
                "{VERSION_DIR}/bin/scepserver is not a file: the staged release has no scepserver"
            )
            .as_str()
        )
    );
    assert!(host.stat(STATE).expect("stat").is_none());
}

/// Catches a build that fixes the other build's `MDM_BIN`: a settings file giving
/// `MDM_BIN=/usr/lib/mdm` is the `.deb`'s, so the `.deb`'s `prepare` takes it, and the archive's
/// refuses it as `bad_settings`, naming both values.
#[test]
fn mdm_bin_is_the_debs_directory_for_the_deb_alone() {
    let settings = format!("{}MDM_BIN={DEB_BIN}\n", package_example());

    let run = Run::new("deb-mdm-bin");
    let host = deb_host();
    host.put_file(SETTINGS, 0o600, (0, 0), settings.as_bytes());
    let ended = prepare_as(&run, &host, &deb_program());
    assert_eq!(ended.code, 0, "{}", ended.err);

    let run = Run::new("archive-mdm-bin");
    let host = package_host();
    host.put_file(SETTINGS, 0o600, (0, 0), settings.as_bytes());
    let ended = run.prepare(&host, false);
    assert_eq!(ended.code, 1, "{}", ended.err);
    let report = ended.report();
    assert_eq!(report.reason.as_deref(), Some("bad_settings"));
    assert_eq!(
        report.detail.as_deref(),
        Some(
            format!(
                "MDM_BIN is \"{DEB_BIN}\", but the mdm package fixes it to \"{}\": leave it out or set that value",
                package::BIN
            )
            .as_str()
        )
    );
}
