//! The tests of the `linux` kind: its `prepare` is the stage, end to end through the hook's step
//! (a short disk is a deferral, exit 75); its other steps are not written yet; and the host's
//! registry holds it on Linux alone.

use hostkeeper_hook::Outcome;
use hostkeeper_launchd::PlatformKind;
use hostkeeper_proto::hostkeeper::agent::v1::LinuxScope as WireScope;
use hostkeeper_steps::ProcBootId;

use super::*;
use crate::maintenance_hook::linux::stage::REASON_SPACE_SHORT;
use crate::maintenance_hook::linux::testing::{FakeHost, args, host_with, params, ran};
use crate::maintenance_hook::{
    Ending, HookPorts, HookStep, REASON_UNSUPPORTED_KIND, Registry, Unwired, run_step,
};

const APT: &str = "/usr/bin/apt-get";

/// A fake host whose apt-get has one security update of 10 bytes to download.
fn apt_host() -> FakeHost {
    let fake = FakeHost::new();
    fake.always(&args(&["update", "-qq"]), ran(0, "", ""));
    fake.always(
        &args(&["--simulate", "-qq", "dist-upgrade"]),
        ran(
            0,
            "Inst openssl [3.0.13-0ubuntu3.2] (3.0.13-0ubuntu3.4 Ubuntu:24.04/noble-security [amd64])\n",
            "",
        ),
    );
    fake.always(
        &args(&[
            "--print-uris",
            "-qq",
            "install",
            "--only-upgrade",
            "openssl",
        ]),
        ran(
            0,
            "'http://archive.example.com/openssl.deb' openssl.deb 10 SHA512:1\n",
            "",
        ),
    );
    fake.always(
        &args(&[
            "--download-only",
            "-y",
            "-qq",
            "-o",
            "Dpkg::Options::=--force-confold",
            "install",
            "--only-upgrade",
            "openssl",
        ]),
        ran(0, "", ""),
    );
    fake
}

/// Runs the hook's `prepare` of a `linux` run over a registry whose `linux` kind is `kind`, the
/// boot id read (never, by `prepare`) from `boot_file`.
fn prepare(kind: Linux, boot_file: &std::path::Path) -> Ending {
    let registry = Registry::new().with(Kind::Linux, Box::new(kind));
    let boot = ProcBootId::at(boot_file);
    let ports = HookPorts {
        kinds: &registry,
        record: &Unwired,
        rebooter: &Unwired,
        boot_id: &boot,
    };
    run_step(HookStep::Prepare, &params(WireScope::Security), &ports)
}

/// Proves: the hook's `prepare` of a `linux` run is the stage: with room it downloads and is
/// done; one byte short of twice the download it is deferred (exit 75), reason
/// `free_space_short`, with no download run.
///
/// Catches: the `linux` kind's `prepare` left a stub, or a short disk that fails the step and so
/// halts a host that was never touched.
#[test]
fn prepare_of_the_linux_kind_is_its_stage() {
    let root = host_with("linux-kind-prepare", APT);
    let fake = apt_host();
    fake.free(Ok(20));
    let boot_file = root.path().join("boot_id");
    let ending = prepare(Linux::new(fake.stage(&root, 300)), &boot_file);
    assert_eq!(ending.outcome, Outcome::Success);
    assert_eq!(fake.arg_lists().len(), 4);

    let fake = apt_host();
    fake.free(Ok(19));
    let ending = prepare(Linux::new(fake.stage(&root, 300)), &boot_file);
    assert_eq!(
        ending,
        Ending {
            outcome: Outcome::Deferred,
            reason: Some(REASON_SPACE_SHORT),
            detail: Some(
                "/var/cache/apt/archives has 19 bytes free; the download of 10 bytes needs 20"
                    .to_owned()
            ),
        }
    );
    assert_eq!(fake.arg_lists().len(), 3);
}

/// Proves: the `linux` kind's `cutover`, `after_reboot` and `verify` fail `unsupported_kind`
/// and run nothing: only the stage is written.
///
/// Catches: a cutover that installs before its work is written.
#[test]
fn the_other_steps_of_the_linux_kind_are_not_written_yet() {
    let root = host_with("linux-kind-others", APT);
    let fake = apt_host();
    let kind = Linux::new(fake.stage(&root, 300));
    let run = params(WireScope::Security);
    let unsupported = KindError::Failed {
        reason: REASON_UNSUPPORTED_KIND,
        detail: "linux is not supported on this host".to_owned(),
    };
    assert_eq!(kind.cutover(&run), Err(unsupported.clone()));
    assert_eq!(kind.after_reboot(&run), Err(unsupported.clone()));
    assert_eq!(kind.verify(&run), Err(unsupported));
    assert_eq!(fake.calls(), Vec::new());
}

/// Proves: the host's registry holds the kinds of its platform, `linux` and `reboot` on Linux and
/// under OpenRC, `macos`, `xcode` and `reboot` on a Mac; Linux's `linux` kind is the stage's,
/// whose other steps still fail `unsupported_kind` (nothing is run to tell it apart further on a
/// test host).
///
/// Catches: the stage left out of the host's registry, or put in a Mac's.
#[test]
fn the_hosts_registry_holds_the_linux_kind_on_linux() {
    for platform in [PlatformKind::Linux, PlatformKind::Openrc] {
        let registry = Registry::at_host(platform, 300);
        assert_eq!(
            registry.kinds().collect::<Vec<_>>(),
            vec![Kind::Linux, Kind::Reboot]
        );
        let linux = registry.get(Kind::Linux).expect("the linux kind");
        assert_eq!(
            linux.verify(&params(WireScope::Security)),
            Err(KindError::unsupported(Kind::Linux))
        );
    }
    let mac = Registry::at_host(PlatformKind::Macos, 300);
    assert_eq!(
        mac.kinds().collect::<Vec<_>>(),
        vec![Kind::Macos, Kind::Xcode, Kind::Reboot]
    );
}
