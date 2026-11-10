//! The native entry's failure and boundary cases: the native package, its bundle and its
//! `control`.

use super::*;

#[test]
fn the_program_a_package_installs_is_where_its_builder_writes_it() {
    // Catches: a hook command naming a path the `.deb` does not install: hello's and mdm's each
    // hold an executable file at the program path, and hostkeeper names none.
    let installed = |files: Vec<native_hello::File>, program: &str| {
        files.iter().any(|f| f.path == program && f.mode == 0o755)
    };
    let hello = manifest::program(Package::Hello).expect("a program");
    assert!(installed(
        native_hello::deb_files(b"p".to_vec(), false),
        &hello
    ));
    assert_eq!(manifest::program(Package::HelloFailReady), Some(hello));
    let mdm = manifest::program(Package::Mdm).expect("a program");
    assert!(installed(
        crate::release::native::mdm::deb_files(b"p".to_vec(), Vec::new()),
        &mdm
    ));
    assert_eq!(manifest::program(Package::Hostkeeper), None);
}

#[test]
fn a_deb_whose_control_is_not_the_releases_is_named_field_by_field() {
    // Catches: a `.deb` of another package, version or architecture under the release's name
    // given an entry: the host's package manager installs it by its control fields and the
    // daemon holds `package_version` to its `Version`, so the host would refuse it at stage.
    let files = native_hello::deb_files(b"a program".to_vec(), false);
    let deb = |name: &str, version: &str, arch: &str| {
        native_deb::build(name, version, arch, "a service", &files, 1_800_000_000).expect("a .deb")
    };
    assert_eq!(
        control_problems(&deb("hello", "0.1.5", "amd64"), "hello", "0.1.5", "amd64"),
        Vec::<String>::new()
    );
    assert_eq!(
        control_problems(
            &deb("tierstore", "0.1.4", "arm64"),
            "hello",
            "0.1.5",
            "amd64"
        ),
        [
            "its control names the Package tierstore, not hello",
            "its control names the Version 0.1.4, not 0.1.5",
            "its control names the Architecture arm64, not amd64",
        ]
    );
    assert_eq!(
        control_problems(b"not a package", "hello", "0.1.5", "amd64"),
        ["not a Debian package: no ar magic"]
    );
}

/// The `.deb` of hello for `arch` whose control holds `control` alone, as bytes.
fn deb_with_control(control: &str) -> Vec<u8> {
    let mut header = tar::Header::new_gnu();
    header.set_size(control.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    let mut tar = tar::Builder::new(Vec::new());
    tar.append_data(&mut header, "./control", control.as_bytes())
        .expect("append control");
    let tar = tar.into_inner().expect("the tar");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tar).expect("gzip");
    let gz = gz.finish().expect("gzip");
    let mut ar = b"!<arch>\n".to_vec();
    ar.extend(
        format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            "control.tar.gz",
            0,
            0,
            0,
            644,
            gz.len()
        )
        .bytes(),
    );
    ar.extend(&gz);
    if gz.len() % 2 == 1 {
        ar.push(b'\n');
    }
    ar
}

#[test]
fn a_deb_whose_control_lacks_a_field_names_the_field() {
    // Catches: a control without `Version` (or `Package`, `Architecture`) read as matching.
    let deb = deb_with_control("Package: hello\nArchitecture: amd64\n");
    assert_eq!(
        control_problems(&deb, "hello", "0.1.5", "amd64"),
        ["its control has no Version"]
    );
}

/// Writes `bytes` as the native package `name` of `dist` with a bundle listing its SHA-256.
fn replace_native(dist: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(dist.join(name), bytes).expect("rewrite the native package");
    let digest = file_sha256(&dist.join(name)).expect("hash");
    std::fs::write(dist.join(bundle_of(name)), bundle_listing(&digest)).expect("its bundle");
}

#[test]
fn every_problem_of_the_native_packages_is_named_at_once_and_nothing_is_written() {
    // Catches: a native package missing (its hosts would be ordered a pin no repository holds),
    // its bundle missing or listing other bytes (the published package would not be the one
    // attested), a `.deb` of another version under the release's name, or the verb stopping at
    // the first of these; a `.pkg` whose bundle lists other bytes among them.
    let (root, dist) = dist("targets-native-problems");
    built(&dist, Package::Hello);
    let output = root.join("github_output");
    // linux-arm64: a .deb of another version, attested.
    let files = native_hello::deb_files(b"a program".to_vec(), false);
    let other = native_deb::build(
        "hello",
        "0.1.4",
        "arm64",
        "a service",
        &files,
        1_800_000_000,
    )
    .expect("a .deb");
    replace_native(&dist, "hello_0.1.5_arm64.deb", &other);
    // ... whose bundle lists other bytes.
    let arm = "hello_0.1.5_arm64.deb";
    std::fs::write(dist.join(bundle_of(arm)), bundle_listing(&"0".repeat(64))).expect("rewrite");
    // linux-x86_64: no .deb and no bundle.
    std::fs::remove_file(dist.join("hello_0.1.5_amd64.deb")).expect("remove");
    std::fs::remove_file(dist.join("hello_0.1.5_amd64.deb.sigstore.json")).expect("remove");
    // macos-arm64: a .pkg whose bundle lists other bytes.
    let pkg = "hello-0.1.5-macos-arm64.pkg";
    std::fs::write(dist.join(bundle_of(pkg)), bundle_listing(&"0".repeat(64))).expect("rewrite");
    let got = targets(&request(&dist, Package::Hello, Some(output.clone())));
    let arm_digest = file_sha256(&dist.join(arm)).expect("hash");
    let pkg_digest = file_sha256(&dist.join(pkg)).expect("hash");
    assert_eq!(
        got,
        Err(Error::Refused(vec![
            format!(
                "{arm}.sigstore.json: {}",
                bundle_problem(&bundle_listing(&"0".repeat(64)), arm, &arm_digest)
                    .expect("a problem")
            ),
            "hello_0.1.5_arm64.deb: its control names the Version 0.1.4, not 0.1.5".to_owned(),
            "hello_0.1.5_amd64.deb: missing: the release ships the native package of every \
             platform it lists"
                .to_owned(),
            "hello_0.1.5_amd64.deb.sigstore.json: missing: every native package carries its \
             attestation bundle"
                .to_owned(),
            format!(
                "{pkg}.sigstore.json: {}",
                bundle_problem(&bundle_listing(&"0".repeat(64)), pkg, &pkg_digest)
                    .expect("a problem")
            ),
        ]))
    );
    assert!(!output.exists(), "nothing is written on a refusal");
}

#[test]
fn hostkeepers_release_without_its_installer_or_with_another_packages_deb_is_refused() {
    // Catches: hostkeeper's list written with the coordinator's `.deb` under the daemon's name
    // (both are built by nfpm into one directory), which a host's apt would install as another
    // package; and a release missing its installer `.pkg` given a macOS list without it.
    let (root, dist) = dist("targets-hostkeeper-native-problems");
    built(&dist, Package::Hostkeeper);
    let output = root.join("github_output");
    let files = vec![native_hello::File {
        path: "/usr/bin/hostkeeper-coordinator".to_owned(),
        mode: 0o755,
        data: b"a program".to_vec(),
    }];
    let coordinator = native_deb::build(
        "hostkeeper-coordinator",
        "0.1.5",
        "amd64",
        "a coordinator",
        &files,
        1_800_000_000,
    )
    .expect("a .deb");
    replace_native(&dist, "hostkeeper_0.1.5_amd64.deb", &coordinator);
    std::fs::remove_file(dist.join("hostkeeper-0.1.5-macos-arm64.pkg")).expect("remove");
    assert_eq!(
        targets(&request(&dist, Package::Hostkeeper, Some(output.clone()))),
        Err(Error::Refused(vec![
            "hostkeeper_0.1.5_amd64.deb: its control names the Package hostkeeper-coordinator, \
             not hostkeeper"
                .to_owned(),
            "hostkeeper-0.1.5-macos-arm64.pkg: missing: the release ships the native package of \
             every platform it lists"
                .to_owned(),
        ]))
    );
    assert!(!output.exists(), "nothing is written on a refusal");
}

#[test]
fn a_bundle_without_its_native_package_is_not_read() {
    // Catches: a bundle checked against a package that is not there (there are no bytes to hold
    // it to), naming a second problem for the one missing file.
    let (_root, dist) = dist("targets-native-bundle-alone");
    built(&dist, Package::Hello);
    std::fs::remove_file(dist.join("hello_0.1.5_amd64.deb")).expect("remove");
    assert_eq!(
        targets(&request(&dist, Package::Hello, None)),
        Err(Error::Refused(vec![
            "hello_0.1.5_amd64.deb: missing: the release ships the native package of every \
             platform it lists"
                .to_owned()
        ]))
    );
}
