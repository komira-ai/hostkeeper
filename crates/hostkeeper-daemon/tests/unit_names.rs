//! The run units the daemon's tests give a service, spelled for the platform the daemon checks
//! them on: each spelling is a unit name of the service under the rule of its platform.

mod support;
mod support_apt;
mod support_host;

use hostkeeper_config::names::{UnitName, launchd_label};
use hostkeeper_config::platform::Platform;
use support_host::{unit, unit_for};

/// A Mac's run unit of the service is a launchd label of it, and a Linux host's is a systemd
/// unit of it that is not a template, so that the daemon's check of a manifest accepts each on
/// the platform it is spelled for.
#[test]
fn a_run_unit_is_spelled_as_a_unit_of_the_service_on_each_platform() {
    for part in ["gone", "new", "two", "a1"] {
        let label = unit_for(Platform::MacosArm64, "web", part);
        assert_eq!(label, format!("web.{part}"));
        assert_eq!(launchd_label("web", &label), Ok(label.as_str()));
        for platform in [Platform::LinuxX86_64, Platform::LinuxArm64] {
            let name = unit_for(platform, "web", part);
            assert_eq!(name, format!("web-{part}.service"));
            let parsed = UnitName::of("web", &name, false).expect("a systemd unit of web");
            assert_eq!(parsed.base, format!("web-{part}"));
        }
    }
}

/// The spelling the tests use is the one of the platform they are built for.
#[test]
fn the_unit_of_a_test_is_spelled_for_the_platform_it_is_built_for() {
    assert_eq!(
        unit("web", "gone"),
        unit_for(Platform::current(), "web", "gone")
    );
}
