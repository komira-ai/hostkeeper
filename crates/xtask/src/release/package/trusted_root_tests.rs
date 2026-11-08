//! The trusted root of hostkeeper's package: carried on every platform at the path the daemon
//! reads, byte for byte, and refused for hello.

use super::tests::{TRUSTED_ROOT, UNIT, built, history, read_back, request, version};
use super::*;
use crate::release::fakes::scratch;

#[test]
fn the_hostkeeper_package_carries_the_trusted_root_where_the_daemon_reads_it() {
    // Catches: a hostkeeper package for any platform without the trusted root, with it under
    // another name than the daemon's `files/trusted_root.json` below `current`, with its bytes
    // changed, or writable; and a manifest that places the file elsewhere instead.
    for (platform, out) in [
        (Platform::LinuxX86_64, "x86"),
        (Platform::LinuxArm64, "arm"),
        (Platform::MacosArm64, "mac"),
    ] {
        let dir = scratch(&format!("package-trusted-root-{out}"));
        let (history, commit) = history();
        let r = request(&dir, UNIT.as_bytes(), &commit, platform, out);
        package(&history, &r).expect("package");
        let seen = read_back(&built(&r));
        let root = seen
            .iter()
            .find(|e| e.name == TRUSTED_ROOT_ENTRY)
            .expect("the trusted root");
        assert_eq!(
            (root.kind, root.mode, root.data.as_slice()),
            (tar::EntryType::Regular, 0o444, TRUSTED_ROOT),
            "{platform:?}"
        );
        assert!(seen.iter().any(|e| e.name == "files/" && e.kind.is_dir()));
        let manifest = seen
            .iter()
            .find(|e| e.name == "manifest.toml")
            .expect("the manifest");
        assert!(!String::from_utf8_lossy(&manifest.data).contains("[[files]]"));
    }
    // The daemon reads it below the version `current` names in the packages directory
    // (hostkeeper_daemon::trusted_root::PACKAGED_TRUSTED_ROOT).
    assert_eq!(
        format!("hostkeeper/current/{TRUSTED_ROOT_ENTRY}"),
        "hostkeeper/current/files/trusted_root.json"
    );
}

#[test]
fn a_hostkeeper_package_needs_its_trusted_root_and_hello_takes_none() {
    // Catches: a hostkeeper package built without a trusted root on any platform (every host
    // would then keep the root it has), a trusted root put into hello's package, a refusal that
    // still writes a package, and a root file that cannot be read passed over.
    let dir = scratch("package-trusted-root-refused");
    let (history, commit) = history();
    for platform in [
        Platform::LinuxX86_64,
        Platform::LinuxArm64,
        Platform::MacosArm64,
    ] {
        let mut r = request(&dir, UNIT.as_bytes(), &commit, platform, "none");
        r.trusted_root = None;
        assert_eq!(
            package(&history, &r),
            Err(Error::Failed(NEEDS_TRUSTED_ROOT.to_owned()))
        );
        assert!(!r.out.exists(), "nothing is written on a refusal");
    }
    let mut hello = request(
        &dir,
        UNIT.as_bytes(),
        &commit,
        Platform::LinuxX86_64,
        "hello",
    );
    hello.service = Service::Hello;
    hello.unit = None;
    assert_eq!(
        package(&history, &hello),
        Err(Error::Failed(TRUSTED_ROOT_REFUSED.to_owned()))
    );
    assert!(!hello.out.exists(), "nothing is written on a refusal");

    let mut unreadable = request(
        &dir,
        UNIT.as_bytes(),
        &commit,
        Platform::LinuxX86_64,
        "gone",
    );
    let missing = dir.join("no-such-root.json");
    unreadable.trusted_root = Some(missing.clone());
    match package(&history, &unreadable) {
        Err(Error::Io { what, .. }) => assert_eq!(what, format!("read {}", missing.display())),
        other => panic!("{other:?}"),
    }
    assert!(
        !unreadable.out.exists(),
        "nothing is written when the root is not read"
    );
}

#[test]
fn the_entry_lists_of_hostkeeper_with_the_trusted_root_pass_the_shape_check() {
    // Catches: an entry list of hostkeeper's package, with its unit or without, that the shape
    // rules of design section 5.5 refuse, such as a trusted root outside the four top-level names
    // or with a mode other than 0444.
    let with_unit = Contents {
        manifest: manifest(&version(), Platform::LinuxX86_64, true),
        units: vec![Unit {
            file: UNIT_FILE.to_owned(),
            origin: String::new(),
            data: Vec::new(),
        }],
        trusted_root: Some(TRUSTED_ROOT.to_vec()),
        binaries: Vec::new(),
    };
    assert_eq!(
        check_entries(&entries("hostkeeper", Vec::new(), with_unit)),
        Ok(())
    );
    let without_unit = Contents {
        manifest: manifest(&version(), Platform::MacosArm64, false),
        units: Vec::new(),
        trusted_root: Some(TRUSTED_ROOT.to_vec()),
        binaries: Vec::new(),
    };
    assert_eq!(
        check_entries(&entries("hostkeeper", Vec::new(), without_unit)),
        Ok(())
    );
}
