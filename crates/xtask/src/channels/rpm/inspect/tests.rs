use super::*;
use crate::channels::fixtures;
use crate::channels::spec::{ARCHES, HOSTKEEPER};

fn daemon() -> &'static Package {
    &HOSTKEEPER.packages[0]
}

fn coordinator() -> &'static Package {
    &HOSTKEEPER.packages[1]
}

#[test]
fn an_nfpm_rpm_without_postun_is_read_when_the_spec_does_not_require_one() {
    // Catches: a check that wants a %postun, which nfpm does not write, so that no signed release of this product could pass `check-signed`.
    for package in [daemon(), coordinator()] {
        let bytes = fixtures::rpm(package, "1.2.3", &ARCHES[0], false);
        let info = inspect(&bytes, package).expect("info");
        assert_eq!(
            info.nevra,
            (
                package.name.to_owned(),
                "1.2.3".to_owned(),
                "1".to_owned(),
                "x86_64".to_owned()
            )
        );
        assert!(
            info.paths
                .iter()
                .any(|p| p == &format!("/usr/bin/{}", package.name)),
            "{:?}",
            info.paths
        );
    }
}

#[test]
fn a_spec_that_requires_postun_refuses_an_rpm_without_one_and_takes_one_with_it() {
    let strict = Package {
        postun_required: true,
        ..*daemon()
    };
    let without = fixtures::rpm(daemon(), "1.2.3", &ARCHES[0], false);
    let e = inspect(&without, &strict).unwrap_err().to_string();
    assert!(e.starts_with("read %postun: "), "{e}");
    let with = fixtures::rpm(daemon(), "1.2.3", &ARCHES[0], true);
    inspect(&with, &strict).expect("with %postun");
    // A package that has one is as good where the spec does not ask for it.
    inspect(&with, daemon()).expect("optional");
}

#[test]
fn the_requirements_are_listed_without_the_rpmlib_ones() {
    let bytes = fixtures::rpm(coordinator(), "1.2.3", &ARCHES[1], false);
    let info = inspect(&bytes, coordinator()).expect("info");
    // The fixture's scriptlets run `/bin/sh`, which the rpm crate lists as a requirement.
    assert_eq!(info.requires, ["/bin/sh", "systemd"]);
    assert_eq!(info.nevra.3, "aarch64");
    let bytes = fixtures::rpm(daemon(), "1.2.3", &ARCHES[1], false);
    assert_eq!(
        inspect(&bytes, daemon()).expect("info").requires,
        ["/bin/sh"]
    );
}

#[test]
fn an_rpm_of_another_package_is_refused_by_its_name() {
    // Catches: the coordinator's package listed under the daemon's file name, or a foreign
    // package under either.
    let bytes = fixtures::rpm(coordinator(), "1.2.3", &ARCHES[0], false);
    let e = inspect(&bytes, daemon()).unwrap_err().to_string();
    assert_eq!(
        e,
        "the package is named \"hostkeeper-coordinator\", not \"hostkeeper\""
    );
}

#[test]
fn bytes_that_are_not_a_package_or_are_damaged_are_refused() {
    let e = inspect(b"not a package", daemon()).unwrap_err().to_string();
    assert!(e.starts_with("read the package: "), "{e}");
    // Flip a byte of the payload: the digests of the signature header no longer match.
    let mut damaged = fixtures::rpm(daemon(), "1.2.3", &ARCHES[0], false);
    let last = damaged.len() - 1;
    damaged[last] ^= 0xff;
    let e = inspect(&damaged, daemon()).unwrap_err().to_string();
    assert!(
        e.starts_with("check the digests: ") || e.starts_with("unpack the payload: "),
        "{e}"
    );
}
