//! The native entry's failure and boundary cases: the native package, its bundle and its
//! `control`, and the native manifest made of the archive's.

use super::*;

/// `text` as the archive's manifest table [`native_manifest`] reads.
fn table(text: &str) -> toml::Table {
    toml::from_str(text).expect("TOML")
}

/// A hook every case below starts from: what an archive's manifest holds.
const HOOK: &str = "[hook]\ncommand = [\"bin/hello\", \"update-hook\"]\nverify_deadline_secs = 120\n\n\
                    [hook.timeout_secs]\ncutover = 60\nverify = 30\nrollback = 60\ncleanup = 30\n";

#[test]
fn a_manifest_the_native_entry_cannot_carry_whole_is_refused_by_its_key() {
    // Catches: an archive's manifest turned into a native one with a field dropped on the way
    // (the paths it places, a key of the hook or its bounds the entry has no field for), or with
    // a value of the wrong kind sent on as zero or empty instead of named.
    let cases = [
        (
            format!("{HOOK}[[files]]\nsource = \"a\"\npath = \"/b\"\n"),
            "files has no counterpart in a native entry, whose package installs its own files",
        ),
        ("name = \"hello\"\n".to_owned(), "there is no [hook]"),
        ("hook = 7\n".to_owned(), "hook is not a table"),
        (
            "[hook]\nverify_deadline_secs = 120\n".to_owned(),
            "there is no [hook.timeout_secs]",
        ),
        (
            format!("{HOOK}[units]\nrun = [\"hello@a.service\"]\nfiles = []\n"),
            "units.files is not a key a native entry carries",
        ),
        (
            HOOK.replace("[hook.timeout_secs]", "retries = 1\n[hook.timeout_secs]"),
            "hook.retries is not a key a native entry carries",
        ),
        (
            format!("{HOOK}stop = 1\n"),
            "hook.timeout_secs.stop is not a key a native entry carries",
        ),
        (
            HOOK.replace("cutover = 60\n", ""),
            "hook.timeout_secs.cutover is missing",
        ),
        (
            HOOK.replace("cutover = 60", "cutover = -1"),
            "hook.timeout_secs.cutover is not a whole number of 0 or more",
        ),
        (
            HOOK.replace("cutover = 60", "cutover = \"60\""),
            "hook.timeout_secs.cutover is not a whole number of 0 or more",
        ),
        (
            HOOK.replace("verify_deadline_secs = 120\n", ""),
            "hook.verify_deadline_secs is missing",
        ),
        (
            HOOK.replace("[hook.timeout_secs]", "boot = 7\n[hook.timeout_secs]"),
            "hook.boot is not a string",
        ),
        (
            HOOK.replace(
                "[hook.timeout_secs]",
                "cleanup_retries = 4294967296\n[hook.timeout_secs]",
            ),
            "hook.cleanup_retries is larger than 4294967295",
        ),
        (
            format!("{HOOK}[units]\nrun = \"hello@a.service\"\n"),
            "run of [units] is not a list of strings",
        ),
        (
            format!("{HOOK}[units]\nrun = [7]\n"),
            "run of [units] is not a list of strings",
        ),
        (
            format!("{HOOK}[health]\nurl = \"http://127.0.0.1:9310/readyz\"\npath = \"/\"\n"),
            "health.path is not a key a native entry carries",
        ),
        (
            format!("{HOOK}[health]\nurl = 9310\n"),
            "health.url is not a string",
        ),
        (
            format!("{HOOK}[params]\nports = [9310]\n"),
            "params.ports is not a string, an integer, a float or a boolean",
        ),
        (format!("units = 1\n{HOOK}"), "units is not a table"),
    ];
    for (text, why) in cases {
        assert_eq!(
            native_manifest(&table(&text), "hello", "0.1.0"),
            Err(why.to_owned()),
            "{text}"
        );
    }
}

#[test]
fn every_optional_field_of_the_archives_hook_and_every_kind_of_param_is_carried() {
    // Catches: a boot step, a promotion ceiling, a cleanup retry count, an uninstall bound or a
    // float or boolean param of the archive's manifest lost from the native entry (the host would
    // run the native version with the hook's defaults), a missing `prepare` bound sent as
    // anything but 0, or an archive with no units or health URL given some.
    let text = HOOK.replace(
        "[hook.timeout_secs]",
        "boot = \"seed\"\npromotion_ceiling_secs = 900\ncleanup_retries = 2\n[hook.timeout_secs]\nuninstall = 45",
    ) + "[params]\nratio = 0.5\nquiet = true\nname = \"a\"\ncount = -3\n";
    let got = native_manifest(&table(&text), "hello", "0.1.2").expect("a manifest");
    let mut params = BTreeMap::new();
    for (key, value) in [
        ("count", param::Value::Integer(-3)),
        ("name", param::Value::Text("a".to_owned())),
        ("quiet", param::Value::Flag(true)),
        ("ratio", param::Value::Float(0.5)),
    ] {
        params.insert(key.to_owned(), Param { value: Some(value) });
    }
    assert_eq!(
        got,
        Manifest {
            package: Some("hello".to_owned()),
            hook: Some(Hook {
                command: Vec::new(),
                boot: Some("seed".to_owned()),
                timeout_secs: Some(StepBounds {
                    prepare: 0,
                    cutover: 60,
                    verify: 30,
                    rollback: 60,
                    cleanup: 30,
                    uninstall: Some(45),
                }),
                verify_deadline_secs: 120,
                promotion_ceiling_secs: Some(900),
                cleanup_retries: Some(2),
            }),
            run: Vec::new(),
            health_url: None,
            place: Vec::new(),
            params: params.into_iter().collect(),
            min_hostkeeper: "0.1.2".to_owned(),
            containers: Vec::new(),
            installer_bounds: Some(INSTALLER_BOUNDS),
        }
    );
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
    // the first of these; and a `.pkg` whose bundle lists other bytes passed over.
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
    // Catches: hostkeeper's list written with a pin no store holds (its installer missing), or
    // with the coordinator's `.deb` under the daemon's name (both are built by nfpm into one
    // directory), which a host's apt would install as another package; and the verb stopping at
    // the first of the two.
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
    std::fs::remove_file(dist.join("hello-0.1.5-macos-arm64.pkg")).expect("remove");
    assert_eq!(
        targets(&request(&dist, Package::Hello, None)),
        Err(Error::Refused(vec![
            "hello_0.1.5_amd64.deb: missing: the release ships the native package of every \
             platform it lists"
                .to_owned(),
            "hello-0.1.5-macos-arm64.pkg: missing: the release ships the native package of \
             every platform it lists"
                .to_owned(),
        ]))
    );
}

#[test]
fn an_archive_manifest_with_files_gives_no_native_entry_and_is_named() {
    // Catches: the paths an archive places dropped from the native entry without a word (the
    // native version would lack them), the refusal naming the native package instead of the
    // archive whose manifest it is.
    let manifest = format!(
        "{}\n[[files]]\nsource = \"files/a\"\npath = \"/etc/mdm/a\"\n",
        mdm_manifest("mdm", "0.1.5", "linux-x86_64", "0.1.3")
    );
    let archive = archive_of(&[(
        "manifest.toml",
        tar::EntryType::Regular,
        manifest.as_bytes(),
    )]);
    assert_eq!(
        mdm_with("targets-native-files", &archive),
        refused(
            "manifest.toml: files has no counterpart in a native entry, whose package installs \
             its own files"
        )
    );
}

#[test]
fn a_native_manifest_the_coordinator_would_refuse_is_never_written() {
    // Catches: a native entry sent on whose manifest the coordinator refuses (a run unit not of
    // the service, `target_invalid`, failing the whole set), which an archive entry, carrying
    // the floor alone, never shows: entry 0, the native one, is named.
    let manifest = mdm_manifest("mdm", "0.1.5", "linux-x86_64", "0.1.3")
        .replace("\"mdm-scep.service\"", "\"other.service\"");
    let archive = archive_of(&[(
        "manifest.toml",
        tar::EntryType::Regular,
        manifest.as_bytes(),
    )]);
    let got = mdm_with("targets-native-run", &archive);
    assert_eq!(
        got,
        Err(Error::Failed(
            "target_invalid: entry 0 of linux-x86_64: the run entry \"other.service\" does not \
             begin with the service name, alone or followed by `-`"
                .to_owned()
        ))
    );
}
