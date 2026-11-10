//! What a manifest's reader and the release checks share: the daemon's version floor, the class
//! and reason of each refusal, the name of a boot step, the health URL and the paths a package
//! may not place a file at.

use hostkeeper_config::manifest::{
    BadHostkeeperVersion, Class, HealthUrlError, HostkeeperVersion, MAX_BOOT_NAME, ManifestRefusal,
    REFUSED_PREFIXES, STEP_NAMES, check_health_url, is_boot_step,
};
use hostkeeper_config::names::is_within;

fn version(text: &str) -> HostkeeperVersion {
    HostkeeperVersion::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn run(units: &[&str]) -> Vec<String> {
    units.iter().map(|unit| (*unit).to_owned()).collect()
}

/// Catches a version read other than as three whole numbers: each of the three is kept, and the
/// text prints back as it came.
#[test]
fn a_hostkeeper_version_is_three_whole_numbers_and_prints_as_it_came() {
    for text in ["0.4.0", "1.2.3", "0.0.0", "10.20.30"] {
        assert_eq!(version(text).to_string(), text);
    }
    assert_eq!(
        version("1.20.300"),
        HostkeeperVersion {
            major: 1,
            minor: 20,
            patch: 300
        }
    );
}

/// Catches versions ordered as text: the numbers are compared in turn, so `0.10.0` is above
/// `0.9.9` and a patch only decides between two equal minors.
#[test]
fn hostkeeper_versions_are_ordered_by_the_three_numbers_in_turn() {
    assert!(version("0.10.0") > version("0.9.99"));
    assert!(version("1.0.0") > version("0.99.99"));
    assert!(version("0.4.1") > version("0.4.0"));
    assert_eq!(version("0.4.0"), version("0.4.0"));
}

/// Catches a malformed floor read as a version: a pre-release, a build suffix, two or four
/// numbers, a leading zero, an empty part or a number past 64 bits is refused, naming the text.
#[test]
fn a_malformed_hostkeeper_version_is_refused_naming_the_text() {
    for text in [
        "9.0.0-rc.1",
        "9.0.0+build",
        "9.0",
        "1.2.3.4",
        "192.0.2.1.5",
        "09.0.0",
        "v9.0.0",
        "",
        "9..0",
        "99999999999999999999.0.0",
        "-1.0.0",
        "1.2.x",
    ] {
        let refused = HostkeeperVersion::parse(text).expect_err(text);
        assert_eq!(refused, BadHostkeeperVersion(text.to_owned()), "{text}");
        assert!(refused.to_string().contains("major.minor.patch"), "{text}");
    }
}

/// Catches a refusal of another class or reason than the design's: `DaemonTooOld` alone is
/// `Environment` with `daemon_too_old`, and every refusal of the package itself is `Release`
/// with `manifest_mismatch` (design 5.5); each prints what was wrong.
#[test]
fn each_refusal_has_its_class_and_reason_token_and_says_what_was_wrong() {
    let unreadable = ManifestRefusal::Unreadable {
        why: "it is not UTF-8 text".to_owned(),
    };
    let invalid = ManifestRefusal::Invalid {
        key: "hook.timeout_secs.verify".to_owned(),
        why: "is 0".to_owned(),
    };
    let too_old = ManifestRefusal::DaemonTooOld {
        needs: version("9.0.0"),
        running: version("0.4.0"),
    };
    for (refusal, class, reason, text) in [
        (
            unreadable,
            Class::Release,
            "manifest_mismatch",
            "the manifest cannot be read: it is not UTF-8 text",
        ),
        (
            invalid,
            Class::Release,
            "manifest_mismatch",
            "the manifest's hook.timeout_secs.verify is 0",
        ),
        (
            too_old,
            Class::Environment,
            "daemon_too_old",
            "the package needs hostkeeper 9.0.0; this daemon is 0.4.0",
        ),
    ] {
        assert_eq!(refusal.class(), class, "{refusal:?}");
        assert_eq!(refusal.reason(), reason, "{refusal:?}");
        assert_eq!(refusal.to_string(), text);
    }
}

/// Catches a boot step named outside the grammar: 1 to 64 bytes of `a-z`, `0-9` and `_`, and none
/// of the step names an update has.
#[test]
fn a_boot_step_is_a_reason_token_that_is_none_of_the_steps() {
    for good in ["restore", "a", "step_2", "0", &"a".repeat(MAX_BOOT_NAME)] {
        assert!(is_boot_step(good), "{good}");
    }
    for bad in [
        "",
        "Restore",
        "re-store",
        "re store",
        "restore\n",
        &"a".repeat(MAX_BOOT_NAME + 1),
    ] {
        assert!(!is_boot_step(bad), "{bad:?}");
    }
    for step in STEP_NAMES {
        assert!(!is_boot_step(step), "{step}");
    }
    assert_eq!(STEP_NAMES.len(), 6);
}

/// Catches a health URL that is not `http` to a loopback literal being passed: the IPv4 loopback
/// and `::1` in any spelling pass, and so does a URL with a port or a path.
#[test]
fn a_health_url_to_a_loopback_literal_passes() {
    for url in [
        "http://127.0.0.1/healthz",
        "http://127.0.0.1:8080/healthz",
        "http://[::1]:8080",
        "http://[::1]:{port}/",
        "http://[0:0:0:0:0:0:0:1]:{port}/healthz",
        "http://[0::1]:{port}/",
    ] {
        assert_eq!(check_health_url("tierstore", &[], url), Ok(()), "{url}");
    }
}

/// Catches another scheme, another host or a loose URL being passed: each is refused for the rule
/// it breaks, the loopback rule or the URL grammar.
#[test]
fn a_health_url_that_is_not_http_to_a_loopback_literal_is_refused() {
    for url in [
        "https://127.0.0.1:{port}/healthz",
        "http://localhost:{port}/healthz",
        "http://127.0.0.2:{port}/healthz",
        "http://[::2]:{port}/",
        "http://192.0.2.1:{port}/healthz",
        "http://[2001:db8::1]:{port}/",
        "http://[::ffff:127.0.0.1]:{port}/",
        "http://[::]:{port}/",
        "http://0.0.0.0:{port}/",
    ] {
        assert_eq!(
            check_health_url("tierstore", &[], url),
            Err(HealthUrlError::NotLoopback),
            "{url}"
        );
    }
    for url in [
        "http://[::1:{port}/",
        "http://127.0.0.1:{port}/{port}",
        "http://127.0.0.1:{port}/a?b",
        "127.0.0.1:{port}",
        "http://127.0.0.1:0/",
        "",
    ] {
        assert!(
            matches!(
                check_health_url("tierstore", &[], url),
                Err(HealthUrlError::Syntax(_))
            ),
            "{url}"
        );
    }
    let syntax = check_health_url("tierstore", &[], "127.0.0.1").expect_err("no scheme");
    assert_eq!(syntax.to_string(), "has no `://`");
    assert_eq!(
        HealthUrlError::NotLoopback.to_string(),
        "is not an http:// URL to a loopback address (127.0.0.1 or [::1])"
    );
}

/// Catches a `{port}` URL passed with a run unit that has no port: it is refused naming the first
/// such unit, and passes when every unit is an instance named by its port or when nothing runs.
#[test]
fn a_port_placeholder_needs_every_run_unit_to_be_an_instance_named_by_its_port() {
    let url = "http://127.0.0.1:{port}/healthz";
    assert_eq!(
        check_health_url(
            "tierstore",
            &run(&["tierstore@9301.service", "tierstore@9302.service"]),
            url
        ),
        Ok(())
    );
    assert_eq!(check_health_url("tierstore", &[], url), Ok(()));
    for unit in [
        "tierstore-cache.service",
        "tierstore-a@main.service",
        "tierstore@0.service",
        "tierstore.service",
    ] {
        assert_eq!(
            check_health_url("tierstore", &run(&["tierstore@9301.service", unit]), url),
            Err(HealthUrlError::PortWithoutInstance {
                unit: unit.to_owned()
            }),
            "{unit}"
        );
    }
    assert_eq!(
        HealthUrlError::PortWithoutInstance {
            unit: "tierstore.service".to_owned()
        }
        .to_string(),
        "has {port}, but the run unit \"tierstore.service\" is not an instance with a port number"
    );
    assert_eq!(
        check_health_url(
            "tierstore",
            &run(&["tierstore.service"]),
            "http://127.0.0.1:8080/healthz"
        ),
        Ok(())
    );
}

/// Catches a prefix list that lets a package place a file in hostkeeper's own directories, the
/// unit directory or the system's: every prefix refuses itself and what lies below it, and no
/// sibling whose name only starts the same.
#[test]
fn the_refused_prefixes_cover_the_directories_a_package_may_not_place_a_file_in() {
    for prefix in REFUSED_PREFIXES {
        assert!(is_within(prefix, prefix), "{prefix}");
        assert!(is_within(&format!("{prefix}/x/y"), prefix), "{prefix}");
        assert!(!is_within(&format!("{prefix}x"), prefix), "{prefix}");
    }
    for own in [
        "/etc/hostkeeper",
        "/var/lib/hostkeeper",
        "/etc/systemd/system",
        "/usr",
        "/boot",
        "/var/lib",
    ] {
        assert!(REFUSED_PREFIXES.contains(&own), "{own}");
    }
}
