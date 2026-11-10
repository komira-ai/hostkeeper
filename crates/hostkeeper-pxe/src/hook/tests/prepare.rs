//! The `prepare` step: what it checks before it fetches, what it hands the fetch, and how it
//! ends for a sealed, a refused and a wrong-version boot set.

use std::cell::RefCell;
use std::fs;

use hostkeeper_hook::{Class, Outcome, Step};

use super::kit::{FakeSystemctl, Fetches, Setup, VERSION, sealing, step, unreached};
use crate::platform::BootPlatform;

const ARM: BootPlatform = BootPlatform::LinuxArm64;
const X86: BootPlatform = BootPlatform::LinuxX86_64;

/// Prepare fetches both platforms from the release of the params into `<boot>/<version>`, hands
/// the fetch the signer and bounds of the daemon config and the defaults of `[prepare]`, logs
/// the sealed line, prints nothing on standard output, and changes no `current` link.
///
/// Catches a fetch started with another release or directory than the params name, and a
/// prepare that points the served directory at a set before cutover.
#[test]
fn prepare_fetches_and_seals_the_target_version() {
    let setup = Setup::new("prepare");
    let fetches = Fetches::default();
    let params = setup.params(Some(VERSION), None, "");
    let ended = step(
        &setup,
        Step::Prepare,
        &params,
        &FakeSystemctl::default(),
        &sealing(&fetches, VERSION),
    );
    assert_eq!(
        ended.err,
        format!(
            "pxe prepare: sealed {}: hostkeeper 1.2.3, Ubuntu 24.04.5, linux-arm64 and linux-x86_64\n",
            setup.boot.join(VERSION).display()
        )
    );
    assert_eq!((ended.code, ended.out.as_str()), (0, ""));
    assert_eq!(
        *fetches.borrow(),
        [(
            setup.boot.join(VERSION),
            vec![ARM, X86],
            "https://releases.example/v1.2.3".to_owned()
        )]
    );
    assert_eq!(setup.listing(), [VERSION]);
}

/// A rerun finds the directory sealed with the version and a boot set of each platform asked
/// for and does not fetch again.
///
/// Catches a rerun after a timeout that downloads several GiB a second time.
#[test]
fn a_sealed_directory_of_the_version_is_done() {
    let setup = Setup::new("prepare-again");
    let fetches = Fetches::default();
    let params = setup.params(Some(VERSION), None, "");
    let systemctl = FakeSystemctl::default();
    step(
        &setup,
        Step::Prepare,
        &params,
        &systemctl,
        &sealing(&fetches, VERSION),
    )
    .succeeded();
    let again = step(&setup, Step::Prepare, &params, &systemctl, &unreached);
    assert_eq!(
        again.err,
        format!(
            "pxe prepare: {} is already sealed\n",
            setup.boot.join(VERSION).display()
        )
    );
    assert_eq!((again.code, again.out.as_str()), (0, ""));
    assert_eq!(fetches.borrow().len(), 1);
    assert!(systemctl.called().is_empty());
}

/// `[prepare] platform` names the platforms fetched; a sealed directory that lacks one the
/// settings now ask for is removed and fetched again.
///
/// Catches a farm of one architecture made to download both, and an old directory kept that
/// lacks the boot set a machine needs.
#[test]
fn the_platforms_of_the_settings_decide_what_is_fetched() {
    let setup = Setup::new("prepare-platforms");
    let fetches = Fetches::default();
    let params = setup.params(Some(VERSION), None, "");
    let systemctl = FakeSystemctl::default();
    setup.settings_with("platform = [\"linux-arm64\"]\n");
    step(
        &setup,
        Step::Prepare,
        &params,
        &systemctl,
        &sealing(&fetches, VERSION),
    )
    .succeeded();
    step(&setup, Step::Prepare, &params, &systemctl, &unreached).succeeded();
    setup.settings_with("platform = [\"linux-x86_64\", \"linux-arm64\"]\n");
    step(
        &setup,
        Step::Prepare,
        &params,
        &systemctl,
        &sealing(&fetches, VERSION),
    )
    .succeeded();
    setup.settings_with("");
    step(&setup, Step::Prepare, &params, &systemctl, &unreached).succeeded();
    let platforms: Vec<Vec<BootPlatform>> = fetches.borrow().iter().map(|f| f.1.clone()).collect();
    assert_eq!(platforms, [vec![ARM], vec![ARM, X86]]);
}

/// A directory that is there but not sealed is removed before the fetch, which then makes it.
///
/// Catches a rerun after a crash that is refused for a half-made directory.
#[test]
fn a_directory_that_is_not_sealed_is_removed_and_fetched() {
    let setup = Setup::new("prepare-stale");
    setup.versions(&[VERSION]);
    let fetches = Fetches::default();
    let params = setup.params(Some(VERSION), None, "");
    step(
        &setup,
        Step::Prepare,
        &params,
        &FakeSystemctl::default(),
        &sealing(&fetches, VERSION),
    )
    .succeeded();
    assert_eq!(fetches.borrow().len(), 1);
}

/// A fetch that is refused is `boot_set_refused` of class `release`, with the fetch's own text
/// as the detail, and nothing is left in the boot directory.
///
/// Catches a refused boot set reported as done, and a refusal blamed on the host.
#[test]
fn a_refused_fetch_is_boot_set_refused() {
    let setup = Setup::new("prepare-refused");
    let params = setup.params(Some(VERSION), None, "");
    let refusing = |_: &_, _: u32| Err("netboot.json: not attested by the signer".to_owned());
    let ended = step(
        &setup,
        Step::Prepare,
        &params,
        &FakeSystemctl::default(),
        &refusing,
    );
    let detail = ended.refused(Outcome::Failure, Class::Release, "boot_set_refused");
    assert_eq!(detail, "netboot.json: not attested by the signer");
    assert_eq!(
        ended.err,
        "pxe prepare: netboot.json: not attested by the signer\n"
    );
    assert_eq!(setup.listing(), Vec::<String>::new());
}

/// A directory sealed as another version than the target, and a fetch that seals another
/// version, are `wrong_version` of class `release` and the directory is removed.
///
/// Catches a boot set of one release served as the next one's.
#[test]
fn a_boot_set_of_another_version_is_wrong_version() {
    let setup = Setup::new("prepare-wrong");
    setup.seal("2.0.0", &[ARM, X86]);
    let params = setup.params(Some("2.0.0"), None, "");
    let ended = step(
        &setup,
        Step::Prepare,
        &params,
        &FakeSystemctl::default(),
        &unreached,
    );
    let detail = ended.refused(Outcome::Failure, Class::Release, "wrong_version");
    assert_eq!(
        detail,
        format!(
            "{} holds the boot sets of hostkeeper 1.2.3, not 2.0.0",
            setup.boot.join("2.0.0").display()
        )
    );
    assert!(!setup.boot.join("2.0.0").exists());

    let fetches = Fetches::default();
    let params = setup.params(Some("1.2.4"), None, "");
    let ended = step(
        &setup,
        Step::Prepare,
        &params,
        &FakeSystemctl::default(),
        &sealing(&fetches, VERSION),
    );
    let detail = ended.refused(Outcome::Failure, Class::Release, "wrong_version");
    assert_eq!(
        detail,
        format!(
            "{} holds the boot sets of hostkeeper 1.2.3, not 1.2.4",
            setup.boot.join("1.2.4").display()
        )
    );
    assert!(!setup.boot.join("1.2.4").exists());
}

/// The fetch is handed the settings: the `[prepare]` keys, the daemon config's signer and
/// bounds, and the connect bound; its defaults are the paths the hostkeeper deb installs.
///
/// Catches a `[prepare]` key that is parsed and then ignored, and a config bound passed to the
/// wrong field.
#[test]
fn the_fetch_carries_the_settings_and_the_config_bounds() {
    let setup = Setup::new("prepare-job");
    let params = setup.params(Some(VERSION), None, "");
    let seen: RefCell<Vec<String>> = RefCell::default();
    let record = |job: &crate::hook::FetchJob, _: u32| {
        seen.borrow_mut().push(format!(
            "gh {} root {} connect {} total {} attestation {} deb {} roots {}",
            job.gh.display(),
            job.trusted_root.display(),
            job.request.timeouts.connect.as_secs(),
            job.request.timeouts.total.as_secs(),
            job.request.attestation_timeout_secs,
            job.request.max_deb_bytes,
            job.roots.len(),
        ));
        assert_eq!(job.request.signers, crate::testing::bundle::signers());
        Err("stop".to_owned())
    };
    step(
        &setup,
        Step::Prepare,
        &params,
        &FakeSystemctl::default(),
        &record,
    );
    setup.settings_with(
        "gh = \"/opt/pinned/gh\"\ntrusted-root = \"/opt/pinned/root.json\"\ndownload-timeout-secs = 123\n",
    );
    step(
        &setup,
        Step::Prepare,
        &params,
        &FakeSystemctl::default(),
        &record,
    );
    assert_eq!(
        *seen.borrow(),
        [
            format!(
                "gh {} root {} connect 30 total 14400 attestation 45 deb 1048576 roots 1",
                crate::cli::GH,
                crate::cli::TRUSTED_ROOT
            ),
            "gh /opt/pinned/gh root /opt/pinned/root.json connect 30 total 123 attestation 45 deb 1048576 roots 1"
                .to_owned(),
        ]
    );
}

/// Each settings problem is `bad_settings` of class `environment`, found before anything is
/// fetched or removed: a file that is not there, one another user could write, a key `serve`
/// does not take, a flag it needs that is missing, a `dir` or `settings` the file may not set,
/// a `[prepare]` key that is not a flag, a daemon config that does not load, and a CA bundle
/// holding no certificate.
///
/// Catches a prepare that fetches gigabytes for an install whose serve would then refuse its
/// settings.
#[test]
fn each_settings_problem_is_bad_settings() {
    let setup = Setup::new("prepare-settings");
    setup.versions(&["keep"]);
    let params = setup.params(Some(VERSION), None, "");
    let original = fs::read_to_string(&setup.settings).expect("the settings");
    let shown = setup.settings.display().to_string();
    let config = setup.path("hostkeeper.toml").display().to_string();
    let empty = setup.path("empty.pem");
    fs::write(&empty, "").expect("an empty bundle");
    let with = |line: &str| original.replacen("[prepare]", &format!("{line}\n[prepare]"), 1);
    let cases: Vec<(String, String)> = vec![
        (
            original.replace("mode = \"router\"\n", ""),
            format!(
                "{shown}: [serve]: the following required arguments were not provided: --mode <MODE>"
            ),
        ),
        (
            with("bogus = 1"),
            format!("{shown}: [serve]: unexpected argument '--bogus' found"),
        ),
        (
            with("dir = \"/x\""),
            format!("{shown}: [serve]: the argument '--dir <DIR>' cannot be used multiple times"),
        ),
        (
            with("settings = \"/x\""),
            format!(
                "{shown}: [serve]: the argument '--settings <SETTINGS>' cannot be used multiple times"
            ),
        ),
        (
            original.replace("mode = \"router\"", "mode = \"both\""),
            format!(
                "{shown}: [serve]: invalid value 'both' for '--mode <MODE>' [possible values: proxy, router]"
            ),
        ),
        (
            with("tftp-timeout-ms = true"),
            format!(
                "{shown}: [serve] \"tftp-timeout-ms\": a value is a string or an integer, or an array of them"
            ),
        ),
        (
            format!("{original}bogus = 1\n"),
            format!("{shown}: [prepare]: unexpected argument '--bogus' found"),
        ),
        (
            format!("{original}platform = [\"macos-arm64\"]\n"),
            format!(
                "{shown}: [prepare]: invalid value 'macos-arm64' for '--platform <PLATFORMS>': \"macos-arm64\" is not a platform with a boot set: linux-x86_64 or linux-arm64"
            ),
        ),
        (
            original.replace(
                &format!("daemon-config = \"{config}\""),
                "daemon-config = \"/nonexistent/daemon.toml\"",
            ),
            String::new(),
        ),
        (
            original.replace(
                &format!("ca-bundle = \"{}\"", setup.path("ca.pem").display()),
                &format!("ca-bundle = \"{}\"", empty.display()),
            ),
            format!(
                "ca-bundle {}: no certificate could be read",
                empty.display()
            ),
        ),
    ];
    for (text, detail) in cases {
        setup.write_settings(&text);
        let ended = step(
            &setup,
            Step::Prepare,
            &params,
            &FakeSystemctl::default(),
            &unreached,
        );
        let found = ended.refused(Outcome::Failure, Class::Environment, "bad_settings");
        match detail.is_empty() {
            true => assert!(found.starts_with("/nonexistent/daemon.toml: "), "{found}"),
            false => assert_eq!(found, detail),
        }
        assert_eq!(setup.listing(), ["keep"]);
    }
    fs::remove_file(&setup.settings).expect("remove");
    let ended = step(
        &setup,
        Step::Prepare,
        &params,
        &FakeSystemctl::default(),
        &unreached,
    );
    let found = ended.refused(Outcome::Failure, Class::Environment, "bad_settings");
    assert!(found.starts_with(&format!("{shown}: ")), "{found}");
}

/// A settings file owned by another user than the host's owner is refused.
///
/// Catches settings that anyone could write being obeyed as root.
#[test]
fn a_settings_file_of_another_owner_is_refused() {
    let setup = Setup::new("prepare-owner");
    let params = setup.params(Some(VERSION), None, "");
    let host = crate::hook::Host {
        owner: setup.owner + 1,
        systemctl: &FakeSystemctl::default(),
        fetch: &unreached,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        crate::hook::run(&host, Step::Prepare, &params, None, &mut out, &mut err),
        1
    );
    let text = String::from_utf8(err).expect("UTF-8");
    assert!(
        text.starts_with(&format!("pxe prepare: {}: ", setup.settings.display())),
        "{text}"
    );
}
