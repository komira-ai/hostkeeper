//! Hello's failing build: its entry is written only when asked for, names the suite
//! `testing-fail` and the Debian version `X.Y.Z~fail.ready`, and is never taken for the release's.

use super::*;

/// Both of hello's builds in one directory, as a release's assets are.
fn both(name: &str) -> (TestDir, PathBuf) {
    let (root, dist) = dist(name);
    built(&dist, Package::Hello);
    built(&dist, Package::HelloFailReady);
    (root, dist)
}

/// The entry the verb must write for the failing build's `.deb` on the Linux `platform`: the
/// release's entry with the failing build's repository entry, Debian version and digest.
fn expected_fail_ready(dist: &Path, platform: Platform) -> Value {
    let name = Package::HelloFailReady
        .native_name(&release(), platform)
        .expect("a native package");
    let mut entry = expected_native(dist, platform);
    entry["digest"] = json!(file_sha256(&dist.join(name)).expect("hash"));
    entry["repository"] = json!("komira-testing-fail");
    entry["packageVersion"] = json!("0.1.5~fail.ready");
    entry
}

/// The `.deb` of the failing build for `platform`, rebuilt with a `control` that says `version`,
/// and its bundle rewritten to list the new bytes; its file name.
fn rebuilt_with(dist: &Path, platform: Platform, version: &str) -> String {
    let name = Package::HelloFailReady
        .native_name(&release(), platform)
        .expect("a native package");
    let files = native_hello::deb_files(b"a program".to_vec(), true);
    let arch = native_deb::arch(platform).expect("a Linux platform");
    let bytes = native_deb::build("hello", version, arch, "a service", &files, 1_800_000_000)
        .expect("build the .deb");
    std::fs::write(dist.join(&name), bytes).expect("rewrite the .deb");
    let digest = file_sha256(&dist.join(&name)).expect("hash");
    std::fs::write(dist.join(bundle_of(&name)), bundle_listing(&digest)).expect("rewrite");
    name
}

#[test]
fn the_failing_builds_entry_is_never_picked_for_a_normal_set() {
    // Catches: a normal hello call that reads or lists the failing build's `.deb` (its entry
    // would name `komira-testing-fail` or `0.1.5~fail.ready`) because its files are in the same
    // directory.
    let (_root, dist) = both("targets-normal-with-fail-ready");
    let got = answered(&targets(&request(&dist, Package::Hello, None)).expect("the targets"));
    let entry = |platform: Platform| alone(expected_native(&dist, platform));
    assert_eq!(
        got,
        json!({
            "linux-arm64": entry(Platform::LinuxArm64),
            "linux-x86_64": entry(Platform::LinuxX86_64),
            "macos-arm64": entry(Platform::MacosArm64),
        })
    );
    let text = got.to_string();
    assert!(!text.contains("komira-testing-fail"), "{text}");
    assert!(!text.contains("fail.ready"), "{text}");
    read_back_as_the_coordinator("hello", &got);
}

#[test]
fn the_failing_builds_entry_names_its_suite_its_tilde_version_and_its_debs_digest() {
    // Catches: the failing build's entry naming the release's repository entry or version, the
    // release's `.deb` digest, a list for macOS (it has no `.pkg`), or a manifest other than the
    // failing build's archive's.
    let (_root, dist) = both("targets-fail-ready");
    let got =
        answered(&targets(&request(&dist, Package::HelloFailReady, None)).expect("the targets"));
    let entry = |platform: Platform| alone(expected_fail_ready(&dist, platform));
    assert_eq!(
        got,
        json!({
            "linux-arm64": entry(Platform::LinuxArm64),
            "linux-x86_64": entry(Platform::LinuxX86_64),
        })
    );
    let text = got.to_string();
    assert!(!text.contains("\"komira-testing\""), "{text}");
    assert!(!text.contains("\"0.1.5\""), "{text}");
    let release_deb = Package::Hello
        .native_name(&release(), Platform::LinuxArm64)
        .expect("a .deb");
    let release_digest = file_sha256(&dist.join(release_deb)).expect("hash");
    assert!(!text.contains(&release_digest), "{text}");
    read_back_as_the_coordinator("hello", &got);
}

#[test]
fn a_failing_deb_whose_control_names_another_version_is_refused_naming_the_field() {
    // Catches: a failing build's `.deb` taken whose control says the release's version or the
    // service version (apt would install it as the release, or order it above the release), the
    // daemon then holding `package_version` to a `Version` it does not have.
    for said in ["0.1.5", "0.1.5-fail-ready"] {
        let (_root, dist) = dist("targets-fail-ready-version");
        built(&dist, Package::HelloFailReady);
        let name = rebuilt_with(&dist, Platform::LinuxArm64, said);
        assert_eq!(
            targets(&request(&dist, Package::HelloFailReady, None)),
            Err(Error::Refused(vec![format!(
                "{name}: its control names the Version {said}, not 0.1.5~fail.ready"
            )])),
            "{said}"
        );
    }
}

#[test]
fn a_missing_failing_deb_or_bundle_is_refused_by_name() {
    // Catches: a failing build set with a platform's `.deb` or attestation bundle absent.
    let (_root, dist) = dist("targets-fail-ready-missing");
    built(&dist, Package::HelloFailReady);
    let name = |p: Platform| {
        Package::HelloFailReady
            .native_name(&release(), p)
            .expect("a .deb")
    };
    std::fs::remove_file(dist.join(name(Platform::LinuxArm64))).expect("remove");
    std::fs::remove_file(dist.join(bundle_of(&name(Platform::LinuxX86_64)))).expect("remove");
    assert_eq!(
        targets(&request(&dist, Package::HelloFailReady, None)),
        Err(Error::Refused(vec![
            format!(
                "{}: missing: the release ships the native package of every platform it lists",
                name(Platform::LinuxArm64)
            ),
            format!(
                "{}: missing: every native package carries its attestation bundle",
                bundle_of(&name(Platform::LinuxX86_64))
            ),
        ]))
    );
}

#[test]
fn the_verb_with_fail_ready_writes_the_failing_builds_entries() {
    // Catches: `--fail-ready` not reaching the verb's request (the normal entries written), or
    // refused for hello now that its failing build has a `.deb`.
    let (_root, dist) = both("targets-fail-ready-verb");
    let failing = crate::release::run(verb(&dist, &["--service", "hello", "--fail-ready"]), || {
        Fakes::over(FakeHistory::default())
    });
    assert_eq!(
        failing,
        targets(&request(&dist, Package::HelloFailReady, None))
    );
    assert!(failing.is_ok());
    let normal = crate::release::run(verb(&dist, &["--service", "hello"]), || {
        Fakes::over(FakeHistory::default())
    });
    assert_ne!(failing, normal);
}
