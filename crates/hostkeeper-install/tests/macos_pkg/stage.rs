//! Tests of `Order::of`, `announces` and `stage`.

use super::*;

/// A `macos-pkg` entry of `svc`: an artifact entry whose manifest names the package, runs both
/// jobs and has its installer bounds.
fn entry() -> Target {
    Target {
        installer: "macos-pkg".into(),
        digest: "ab".repeat(32),
        bytes: 1,
        manifest: Some(Manifest {
            package: Some(IDENTIFIER.into()),
            run: BOTH.iter().map(|l| (*l).to_owned()).collect(),
            installer_bounds: Some(InstallerBounds {
                stage_secs: 60,
                activate_secs: 120,
                remove_secs: 30,
            }),
            ..Manifest::default()
        }),
        ..Target::default()
    }
}

fn order_of(entry: Option<&Target>) -> Result<Order, Failure> {
    let service = ServiceName::new(SERVICE).unwrap();
    let version = Version::new("1.0").unwrap();
    let digest = Sha256Hex::parse(&"ab".repeat(32)).unwrap();
    Order::of(&StageOrder {
        service: &service,
        version: &version,
        package_digest: &digest,
        entry,
    })
}

/// Catches: A6's stage order read with another identifier, other jobs, other bounds or another
/// `boot` than its entry's manifest gives.
#[test]
fn the_order_is_read_from_the_stage_orders_entry() {
    let mut order = Rig::new("order-of").order_run("1.0", &BOTH);
    order.bounds = Bounds {
        stage: Duration::from_secs(60),
        activate: Duration::from_secs(120),
        remove: Duration::from_secs(30),
    };
    assert_eq!(order_of(Some(&entry())).unwrap(), order);
    let mut booted = entry();
    if let Some(manifest) = booted.manifest.as_mut() {
        manifest.hook = Some(Hook {
            boot: Some("check".into()),
            ..Hook::default()
        });
    }
    assert!(order_of(Some(&booted)).unwrap().boot);
}

/// Catches: an order with no entry, or an entry naming a repository, a package version or an
/// attestation, being staged; each is `order_invalid` before anything runs.
#[test]
fn an_order_with_no_entry_or_a_repository_a_package_version_or_an_attestation_is_order_invalid() {
    let with = |f: fn(&mut Target)| {
        let mut entry = entry();
        f(&mut entry);
        entry
    };
    let cases = [
        (None, "a macos-pkg order carries no entry"),
        (
            Some(with(|e| e.repository = Some("komira".into()))),
            "a macos-pkg entry names a repository",
        ),
        (
            Some(with(|e| e.package_version = Some("1.0-1".into()))),
            "a macos-pkg entry names a package version",
        ),
        (
            Some(with(|e| e.attestation = Some(Blob::default()))),
            "a macos-pkg entry carries an attestation",
        ),
    ];
    for (entry, detail) in cases {
        let err = order_of(entry.as_ref()).unwrap_err();
        assert_eq!(err.reason(), Reason::OrderInvalid, "{detail}");
        assert_eq!(err.reason().class(), Class::Release);
        assert_eq!(err.reason().outcome(), Outcome::Failed);
        assert_eq!(err.detail(), detail);
    }
}

/// Catches: a manifest with no package, an empty package, a `run` entry that is no launchd label
/// or no installer bounds being read as an order.
#[test]
fn a_manifest_missing_what_macos_pkg_needs_is_manifest_invalid() {
    let with = |f: fn(&mut Manifest)| {
        let mut entry = entry();
        if let Some(manifest) = entry.manifest.as_mut() {
            f(manifest);
        }
        entry
    };
    let cases = [
        (
            Target {
                manifest: None,
                ..entry()
            },
            "the entry carries no manifest".to_owned(),
        ),
        (
            with(|m| m.package = None),
            "the manifest names no package".to_owned(),
        ),
        (
            with(|m| m.package = Some(String::new())),
            "the manifest names no package".to_owned(),
        ),
        (
            with(|m| m.run = vec!["a label".into()]),
            format!(
                "the run label \"a label\": {}",
                Label::parse("a label").unwrap_err()
            ),
        ),
        (
            with(|m| m.installer_bounds = None),
            "the manifest has no installer_bounds".to_owned(),
        ),
    ];
    for (entry, detail) in cases {
        let err = order_of(Some(&entry)).unwrap_err();
        assert_eq!(err.reason(), Reason::ManifestInvalid, "{detail}");
        assert_eq!(err.detail(), detail);
    }
}

/// Catches: a Mac announcing `macos-pkg` without `installer`, without `pkgutil` or without a signer.
#[test]
fn a_mac_announces_macos_pkg_only_with_both_tools_and_a_signer() {
    let rig = Rig::new("announce");
    let announced =
        |rig: &Rig, signers: bool| announces(&rig.host, signers.then_some(&rig.signers));
    assert!(!announced(&rig, true), "no tool present");
    rig.write("/usr/sbin/installer", b"", 0o755);
    assert!(!announced(&rig, true), "no pkgutil");
    rig.write("/usr/sbin/pkgutil", b"", 0o755);
    assert!(announced(&rig, true));
    assert!(!announced(&rig, false), "no signers file");
    rig.remove_file("/usr/sbin/installer");
    assert!(!announced(&rig, true), "no installer");
}

/// The calls `stage` makes of a package it accepts, as a Mac sees them.
fn stage_accepts(pkg: FakePkg, name: &str) {
    let rig = Rig::new(name);
    let order = rig.order_run("1.0", &BOTH);
    let expected_info = pkg.package_info_text();
    rig.mac.add(pkg);
    let dir = rig.stage(&order, "svc-1.0");
    assert_eq!(names(&dir), ["artifact", "macos-pkg", "tree"]);
    assert_eq!(fs::read(dir.join("artifact")).unwrap(), b"fake-pkg:svc-1.0");
    let mode = |p: &str| fs::metadata(dir.join(p)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode("artifact"), 0o444);
    assert_eq!(mode("macos-pkg/PackageInfo"), 0o444);
    assert_eq!(mode("macos-pkg/Bom"), 0o444);
    assert_eq!(
        names(&dir.join("macos-pkg")),
        ["Bom", "PackageInfo", "artifact.pkg"]
    );
    let ino = |p: &str| std::os::unix::fs::MetadataExt::ino(&fs::metadata(dir.join(p)).unwrap());
    assert_eq!(
        ino("macos-pkg/artifact.pkg"),
        ino("artifact"),
        "the .pkg name is the kept artifact itself, not a copy"
    );
    assert_eq!(
        fs::read_to_string(dir.join("macos-pkg/PackageInfo")).unwrap(),
        expected_info
    );
    assert_eq!(
        fs::read(dir.join("macos-pkg/Bom")).unwrap(),
        b"fake-bom:svc-1.0"
    );
    let tree = |rel: &str| fs::read_to_string(dir.join("tree").join(rel)).unwrap();
    assert_eq!(
        tree("Library/Application Support/svc/svc"),
        "program svc 1.0"
    );
    assert_eq!(tree("Library/Application Support/svc/old/legacy"), "legacy");
    assert!(tree("Library/LaunchDaemons/svc.worker.plist").contains("<string>svc.worker</string>"));
    assert_eq!(
        only_verbs(&rig, Tool::Pkgutil),
        ["--check-signature", "--expand-full"]
    );
    assert_eq!(
        rig.mac.calls_of(Tool::Pkgutil)[0][1],
        rig.downloads.join("svc-1.0.pkg").to_str().unwrap()
    );
    assert_eq!(rig.mac.calls_of(Tool::Lsbom).len(), 1);
    no_install_ran(&rig);
    assert!(
        rig.snapshot().is_empty(),
        "stage touches nothing of the host"
    );
    assert!(rig.records.all().is_empty());
}

/// Catches: a staged version missing its artifact, tree or meta files, a kept file that is writable, or a tool run that installs or fetches.
#[test]
fn stage_of_a_component_package_keeps_the_artifact_the_tree_and_the_two_meta_files() {
    stage_accepts(v1(), "stage-component");
}

/// Catches: a product archive's component not being found, or its extra entries being kept.
#[test]
fn stage_of_a_product_archive_with_one_component_keeps_the_same_files() {
    let mut pkg = v1().product();
    pkg.extras = vec!["Resources".into()];
    stage_accepts(pkg, "stage-product");
}

/// Catches: only the first signer of the file being honoured.
#[test]
fn stage_accepts_a_package_signed_by_the_second_signer_of_a_rotation() {
    let mut rig = Rig::new("stage-rotation");
    rig.signers = Signers::new(vec![
        Fingerprint::parse(&fp("7e")).unwrap(),
        Fingerprint::parse(&leaf()).unwrap(),
    ])
    .unwrap();
    rig.mac.add(v1());
    rig.stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
}

/// Catches: a package whose leaf is not a configured signer, whose chain the Mac does not
/// trust, or that is unsigned being expanded or installed; a signer match against an
/// intermediate certificate instead of the leaf.
#[test]
fn stage_refuses_a_signature_it_does_not_trust_and_expands_nothing() {
    let other_leaf = signed_output(&fp("7e"), TRUSTED);
    let cases: Vec<(&str, (i32, String), Option<String>)> = vec![
        (
            "unsigned",
            (1, "Package \"svc.pkg\":\n   Status: no signature\n".into()),
            None,
        ),
        (
            "untrusted chain",
            (
                1,
                signed_output(&leaf(), "signed by a certificate that is not trusted"),
            ),
            None,
        ),
        (
            "untrusted status with exit 0",
            (
                0,
                signed_output(&leaf(), "signed by a certificate that is untrusted"),
            ),
            None,
        ),
        ("leaf no signer names", (0, other_leaf), None),
        (
            "intermediate named as the signer",
            (0, signed_output(&leaf(), TRUSTED)),
            Some(fp("11")),
        ),
        ("no status line", (0, "Package \"svc.pkg\":\n".into()), None),
        (
            "no fingerprint",
            (
                0,
                "Package:\n   Status: signed by a certificate trusted by macOS\n".into(),
            ),
            None,
        ),
    ];
    for (what, check, signer) in cases {
        let mut rig = Rig::new("stage-signature");
        if let Some(signer) = signer {
            rig.signers = Signers::new(vec![Fingerprint::parse(&signer).unwrap()]).unwrap();
        }
        rig.mac.add(v1().with_check(check.0, &check.1));
        let (result, staging) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
        let err = result.unwrap_err();
        assert_eq!(err.reason(), Reason::Signature, "{what}: {err}");
        assert_eq!(err.reason().class(), Class::Environment, "{what}");
        assert!(names(&staging).is_empty(), "{what}: nothing expanded");
        let calls = rig.mac.calls();
        assert_eq!(calls.len(), 1, "{what}: only the signature check ran");
        assert_eq!(calls[0].args[0], "--check-signature");
        assert!(rig.snapshot().is_empty());
        assert!(rig.records.all().is_empty());
    }
}

/// Catches: the signature error hiding which fingerprint the package was signed by.
#[test]
fn a_leaf_no_signer_names_is_reported_with_its_fingerprint() {
    let rig = Rig::new("stage-leaf");
    rig.mac
        .add(v1().with_check(0, &signed_output(&fp("7e"), TRUSTED)));
    let (result, _) = rig.try_stage(&rig.order("1.0"), "svc-1.0");
    assert_eq!(
        result.unwrap_err().detail(),
        format!(
            "the leaf certificate {} is none of the host's signers",
            fp("7e")
        )
    );
}

/// Catches: a second service's package with the same identifier being staged, and any tool
/// running before that check.
#[test]
fn stage_refuses_an_identifier_another_service_records() {
    let rig = Rig::new("stage-conflict");
    rig.records.set("other", PkgRecord::new(IDENTIFIER, "3.0"));
    rig.mac.add(v1());
    let (result, staging) = rig.try_stage(&rig.order("1.0"), "svc-1.0");
    let err = result.unwrap_err();
    assert_eq!(err.reason(), Reason::PackageConflict);
    assert_eq!(
        err.detail(),
        "the service other records the package dev.svc.server"
    );
    assert!(rig.mac.calls().is_empty());
    assert!(names(&staging).is_empty());
}

/// Catches: a service's own record of the identifier being refused as `package_conflict` on its update.
#[test]
fn stage_of_a_service_that_records_the_identifier_itself_is_not_a_conflict() {
    let rig = Rig::new("stage-own");
    rig.records.set(SERVICE, PkgRecord::new(IDENTIFIER, "0.9"));
    rig.mac.add(v1());
    rig.stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
}

/// Catches: a record store that cannot be read, or a tool that cannot run or times out, being
/// reported as a defect of the package instead of `io_error`.
#[test]
fn stage_reports_io_error_when_the_records_or_a_tool_fail() {
    let rig = Rig::new("stage-io-records");
    rig.mac.add(v1());
    rig.records
        .fail_holder
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        reason(rig.try_stage(&rig.order("1.0"), "svc-1.0").0),
        Reason::IoError
    );

    for (tool, verb, fault) in [
        (Tool::Pkgutil, "--check-signature", Fault::Spawn),
        (Tool::Pkgutil, "--check-signature", Fault::TimedOut),
        (Tool::Pkgutil, "--expand-full", Fault::Spawn),
        (Tool::Lsbom, "-p", Fault::Spawn),
    ] {
        let rig = Rig::new("stage-io-tools");
        rig.mac.add(v1());
        rig.mac.fault(tool, &[verb], fault);
        let (result, _) = rig.try_stage(&rig.order("1.0"), "svc-1.0");
        let err = result.unwrap_err();
        assert_eq!(err.reason(), Reason::IoError, "{verb}: {err}");
        assert!(err.detail().starts_with(tool.path()), "{}", err.detail());
        no_install_ran(&rig);
    }
}

/// Catches: an expansion `pkgutil` could not make being read as a package.
#[test]
fn stage_refuses_a_package_pkgutil_cannot_expand() {
    let rig = Rig::new("stage-expand-fails");
    rig.mac.add(v1());
    rig.mac.exit(
        Tool::Pkgutil,
        &["--expand-full"],
        1,
        "Error: invalid xar archive\n",
    );
    let (result, _) = rig.try_stage(&rig.order("1.0"), "svc-1.0");
    let err = result.unwrap_err();
    assert_eq!(err.reason(), Reason::ManifestInvalid);
    assert!(
        err.detail().contains("invalid xar archive"),
        "{}",
        err.detail()
    );
}

fn tweak(f: impl FnOnce(&mut FakePkg)) -> FakePkg {
    let mut pkg = v1();
    f(&mut pkg);
    pkg
}

fn distribution_with(pkg: &FakePkg, from: &str, to: &str) -> Option<String> {
    Some(pkg.distribution_text().replace(from, to))
}

fn info_with(pkg: &FakePkg, from: &str, to: &str) -> Option<String> {
    Some(pkg.package_info_text().replace(from, to))
}

/// Catches: a package whose shape lets `installer` write something other than the `Bom` lists
/// (a second component, a component not selected by default, a `Distribution` script or
/// check, an entry beside the component or in it, another identifier or version, another install
/// location, a `relocate` of a bundle or a `bundle`) being staged.
#[test]
fn stage_refuses_each_shape_defect_as_manifest_invalid_and_changes_nothing() {
    let two = "<installer-gui-script><pkg-ref id=\"a\">#svc-1.0.pkg</pkg-ref><pkg-ref id=\"b\">#other.pkg</pkg-ref></installer-gui-script>";
    let end = "</installer-gui-script>";
    let cases: Vec<(&str, FakePkg)> = vec![
        (
            "two components",
            tweak(|p| {
                p.product = true;
                p.distribution = Some(two.into());
            }),
        ),
        (
            "not selected by default",
            tweak(|p| {
                p.product = true;
                p.distribution = distribution_with(
                    p,
                    "<choice id=\"default\"/>",
                    "<choice id=\"default\" start_selected=\"false\"/>",
                );
            }),
        ),
        (
            "a Distribution script",
            tweak(|p| {
                p.product = true;
                p.distribution =
                    distribution_with(p, end, &format!("<script>exit 0</script>{end}"));
            }),
        ),
        (
            "an installation-check",
            tweak(|p| {
                p.product = true;
                p.distribution = distribution_with(
                    p,
                    end,
                    &format!("<installation-check script=\"f()\"/>{end}"),
                );
            }),
        ),
        (
            "a volume-check",
            tweak(|p| {
                p.product = true;
                p.distribution =
                    distribution_with(p, end, &format!("<volume-check script=\"f()\"/>{end}"));
            }),
        ),
        (
            "a component path with no directory",
            tweak(|p| {
                p.product = true;
                p.distribution = distribution_with(p, "#svc-1.0.pkg", "#nowhere.pkg");
            }),
        ),
        (
            "an entry beside the component",
            tweak(|p| {
                p.product = true;
                p.extras = vec!["Stray".into()];
            }),
        ),
        (
            "another identifier",
            tweak(|p| {
                p.package_info = info_with(
                    p,
                    "identifier=\"dev.svc.server\"",
                    "identifier=\"dev.other\"",
                )
            }),
        ),
        (
            "another version",
            tweak(|p| p.package_info = info_with(p, "version=\"1.0\"", "version=\"9.9\"")),
        ),
        (
            "another install location",
            tweak(|p| {
                p.package_info = info_with(
                    p,
                    "install-location=\"/\"",
                    "install-location=\"/Applications\"",
                )
            }),
        ),
        (
            "an entry in the component",
            tweak(|p| p.component_extras = vec!["Resources".into()]),
        ),
        (
            "a relocate of a bundle",
            tweak(|p| {
                p.package_info =
                    info_with(p, "<relocate/>", "<relocate><bundle id=\"a\"/></relocate>")
            }),
        ),
        (
            "a bundle",
            tweak(|p| {
                p.package_info =
                    info_with(p, "</pkg-info>", "<bundle id=\"a\" path=\"b\"/></pkg-info>")
            }),
        ),
        (
            "no pkg-info",
            tweak(|p| p.package_info = Some("<other/>".into())),
        ),
        (
            "a PackageInfo that is not XML",
            tweak(|p| p.package_info = Some("<pkg-info".into())),
        ),
    ];
    for (what, pkg) in cases {
        let rig = Rig::new("stage-shape");
        rig.mac.add(pkg);
        let (result, _) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
        let err = result.unwrap_err();
        assert_eq!(err.reason(), Reason::ManifestInvalid, "{what}: {err}");
        assert_eq!(err.reason().class(), Class::Release);
        assert!(rig.snapshot().is_empty(), "{what}");
        assert!(rig.records.all().is_empty());
        no_install_ran(&rig);
    }
}

/// Catches: a package carrying a script that `installer` would run as root (a `Scripts` archive
/// in its component or beside it, a `preinstall` or `postinstall` in its `PackageInfo`, a
/// payload-free package with scripts) being staged, or refused for its shape (`manifest_invalid`)
/// rather than as `package_scripts`, or after its `Bom` is read.
#[test]
fn stage_refuses_a_package_with_a_script_as_package_scripts_before_its_bom_and_changes_nothing() {
    let scripts = "<scripts><preinstall file=\"./preinstall\"/></scripts>";
    let cases: Vec<(&str, FakePkg, &str)> = vec![
        (
            "a Scripts archive in the component",
            tweak(|p| p.component_extras = vec!["Scripts".into()]),
            "the package carries a script: the component holds \"Scripts\"",
        ),
        (
            "a Scripts archive in a product archive's component",
            tweak(|p| {
                p.product = true;
                p.component_extras = vec!["Scripts".into()];
            }),
            "the package carries a script: the component holds \"Scripts\"",
        ),
        (
            "a Scripts archive beside the component",
            tweak(|p| {
                p.product = true;
                p.extras = vec!["Scripts".into()];
            }),
            "the package carries a script: the product archive holds \"Scripts\"",
        ),
        (
            "a preinstall in the PackageInfo",
            tweak(|p| p.package_info = info_with(p, "<scripts/>", scripts)),
            "the package carries a script: the PackageInfo has a <preinstall> element",
        ),
        (
            "a postinstall in the PackageInfo",
            tweak(|p| {
                p.package_info = info_with(
                    p,
                    "<scripts/>",
                    "<scripts><postinstall file=\"./postinstall\"/></scripts>",
                )
            }),
            "the package carries a script: the PackageInfo has a <postinstall> element",
        ),
        (
            "a payload-free package with scripts",
            tweak(|p| {
                p.has_payload = false;
                p.component_extras = vec!["Scripts".into()];
            }),
            "the package carries a script: the component holds \"Scripts\"",
        ),
    ];
    for (what, pkg, detail) in cases {
        let rig = Rig::new("stage-scripts");
        rig.mac.add(pkg);
        let (result, _) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
        let err = result.unwrap_err();
        assert_eq!(err.reason(), Reason::PackageScripts, "{what}: {err}");
        assert_eq!(err.reason().class(), Class::Release);
        assert_eq!(err.reason().outcome(), Outcome::Failed);
        assert_eq!(err.detail(), detail, "{what}");
        assert!(
            rig.mac.calls_of(Tool::Lsbom).is_empty(),
            "{what}: the Bom was not read"
        );
        assert!(rig.snapshot().is_empty(), "{what}");
        assert!(rig.records.all().is_empty());
        no_install_ran(&rig);
    }
}

/// Catches: a hook at the fixed path that is a directory or not executable, or a `hook.boot`
/// with no hook in the package, being staged; or a package with an executable hook, with or
/// without `hook.boot`, being refused. (A hook that is a link is `hook`'s unit test: on Linux a
/// link's mode is 777, which the payload's mode rule refuses before the hook is looked at; on a
/// Mac a link is 755 and reaches the hook check.)
#[test]
fn stage_checks_the_hook_at_its_fixed_path() {
    let hook = home("hostkeeper-hook");
    let program = || Content::File(b"#!/bin/sh\nexit 0\n".to_vec());
    let refused: Vec<(&str, FakePkg, bool, String)> = vec![
        (
            "a directory",
            with_entry(&hook, Content::Dir, 0o755, &[]),
            false,
            format!("the hook {hook} is a directory"),
        ),
        (
            "not executable",
            with_entry(&hook, program(), 0o644, &[]),
            false,
            format!("the hook {hook} is not executable (mode 644)"),
        ),
        (
            "hook.boot with no hook",
            v1(),
            true,
            format!("the entry's hook.boot is set, but the package has no hook at {hook}"),
        ),
    ];
    for (what, pkg, boot, detail) in refused {
        let rig = Rig::new("stage-hook");
        rig.mac.add(pkg);
        let mut order = rig.order_run("1.0", &BOTH);
        order.boot = boot;
        let (result, _) = rig.try_stage(&order, "svc-1.0");
        let err = result.unwrap_err();
        assert_eq!(err.reason(), Reason::ManifestInvalid, "{what}: {err}");
        assert_eq!(err.detail(), detail, "{what}");
        assert!(rig.snapshot().is_empty(), "{what}");
        no_install_ran(&rig);
    }
    for boot in [false, true] {
        let rig = Rig::new("stage-hook-ok");
        rig.mac.add(with_entry(&hook, program(), 0o755, &[]));
        let mut order = rig.order_run("1.0", &BOTH);
        order.boot = boot;
        let (result, staging) = rig.try_stage(&order, "svc-1.0");
        result.unwrap();
        let staged = staging.join("tree").join(hook.trim_start_matches('/'));
        assert!(staged.is_file(), "boot {boot}: the hook is in the tree");
    }
}

/// Catches: the `PackageInfo` `pkgbuild` writes (its empty `<relocate/>` and `<scripts/>`) being
/// refused, so that no real package could be staged.
#[test]
fn stage_accepts_the_package_info_pkgbuild_writes() {
    let rig = Rig::new("stage-pkgbuild-form");
    let pkg = v1();
    let info = pkg.package_info_text();
    assert!(
        info.contains("<relocate/>") && info.contains("<scripts/>"),
        "{info}"
    );
    rig.mac.add(pkg);
    let (result, _) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
    result.unwrap();
}

fn with_entry(rel_path: &str, content: Content, mode: u32, dirs: &[&str]) -> FakePkg {
    let mut pkg = v1();
    for dir in dirs {
        pkg = pkg.put(dir, Content::Dir, 0o755);
    }
    pkg.put(rel_path, content, mode)
}

/// Catches: a payload the `Bom` does not describe, or an entry a service's package may not
/// install (setuid, setgid, sticky, group or other writable, a fifo, a path of another
/// service, of `/usr/local` or of an unlisted job), being staged.
#[test]
fn stage_refuses_each_payload_defect_as_manifest_invalid_and_changes_nothing() {
    let file = |mode| with_entry(&home("f"), Content::File(b"x".to_vec()), mode, &[]);
    let rig_for_bom = Rig::new("stage-bom-text");
    let derived = rig_for_bom.mac.bom_listing(&v1());
    let cases: Vec<(&str, FakePkg)> = vec![
        (
            "a Bom entry the payload lacks",
            tweak(|p| {
                p.lsbom = Some((
                    0,
                    format!("{derived}./Library/Application Support/svc/ghost\t100644\t0\t0\n"),
                ))
            }),
        ),
        (
            "a payload file the Bom lacks",
            tweak(|p| {
                p.lsbom = Some((
                    0,
                    derived
                        .lines()
                        .filter(|l| !l.contains("VERSION"))
                        .map(|l| format!("{l}\n"))
                        .collect(),
                ))
            }),
        ),
        (
            "a Bom that lists a path twice",
            tweak(|p| {
                p.lsbom = Some((0, format!("{derived}{}\n", derived.lines().nth(1).unwrap())))
            }),
        ),
        (
            "a Bom line that is no listing",
            tweak(|p| p.lsbom = Some((0, "garbage\n".into()))),
        ),
        (
            "a Bom that lsbom cannot print",
            tweak(|p| p.lsbom = Some((1, String::new()))),
        ),
        (
            "a Bom listing cut at the runner's cap",
            tweak(|p| p.lsbom = Some((0, "x".repeat(OUTPUT_MAX)))),
        ),
        (
            "a setuid file the Bom does not show",
            file(0o4755).bom_mode(&home("f"), 0o755),
        ),
        ("a setuid file", file(0o4755)),
        ("a setgid file", file(0o2755)),
        (
            "a sticky directory",
            with_entry(&home("tmp"), Content::Dir, 0o1755, &[]),
        ),
        ("a group-writable file", file(0o664)),
        ("an other-writable file", file(0o646)),
        (
            "a fifo",
            with_entry(&home("pipe"), Content::Fifo, 0o644, &[]),
        ),
        (
            "a path under /usr/local",
            with_entry(
                "/usr/local/bin/tool",
                Content::File(b"x".to_vec()),
                0o755,
                &["/usr", "/usr/local", "/usr/local/bin"],
            ),
        ),
        (
            "another service's directory",
            with_entry(
                "/Library/Application Support/other/f",
                Content::File(b"x".to_vec()),
                0o644,
                &["/Library/Application Support/other"],
            ),
        ),
        (
            "a plist of a label the order does not run",
            with_entry(
                "/Library/LaunchDaemons/extra.plist",
                Content::File(b"x".to_vec()),
                0o644,
                &[],
            ),
        ),
    ];
    for (what, pkg) in cases {
        let rig = Rig::new("stage-payload");
        rig.mac.add(pkg);
        let (result, _) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
        let err = result.unwrap_err();
        assert_eq!(err.reason(), Reason::ManifestInvalid, "{what}: {err}");
        assert!(rig.snapshot().is_empty(), "{what}");
        no_install_ran(&rig);
    }
}

/// Catches: an entry of a group other than `wheel` or `admin`, or one not owned by root, being
/// staged (the rig's accounts make the test user's own files such an entry).
#[test]
fn stage_refuses_an_entry_of_another_group_or_another_owner() {
    let other_group = |uid: u32, gid: u32| Ids {
        root: uid,
        wheel: gid + 1,
        admin: gid + 2,
    };
    let other_owner = |uid: u32, gid: u32| Ids {
        root: uid + 1,
        wheel: gid,
        admin: gid + 1,
    };
    for (what, ids) in [
        ("group staff", other_group as fn(u32, u32) -> Ids),
        ("not root", other_owner),
    ] {
        let rig = Rig::with_ids("stage-owner", ids);
        rig.mac.add(v1());
        let (result, _) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
        let err = result.unwrap_err();
        assert_eq!(err.reason(), Reason::ManifestInvalid, "{what}");
        assert!(
            err.detail().contains(if what == "not root" {
                "not root"
            } else {
                "neither wheel nor admin"
            }),
            "{}",
            err.detail()
        );
    }
}

/// Catches: a `run` label whose job plist is missing, has another `Label`, is not `Disabled`
/// or runs a program the payload lacks being staged.
#[test]
fn stage_refuses_each_job_defect_as_manifest_invalid_and_changes_nothing() {
    let program = format!("{HOME}/svc");
    let cases: Vec<(&str, FakePkg)> = vec![
        (
            "a label with no plist",
            v1().without("/Library/LaunchDaemons/svc.worker.plist"),
        ),
        (
            "another Label",
            v1().plist("svc.worker", &job_plist("other", &program, Some(true))),
        ),
        (
            "no Disabled",
            v1().plist("svc.worker", &job_plist("svc.worker", &program, None)),
        ),
        (
            "Disabled false",
            v1().plist(
                "svc.worker",
                &job_plist("svc.worker", &program, Some(false)),
            ),
        ),
        (
            "a program the payload lacks",
            v1().plist(
                "svc.worker",
                &job_plist("svc.worker", &home("missing"), Some(true)),
            ),
        ),
        (
            "a plist that is no plist",
            v1().plist("svc.worker", "not a property list"),
        ),
    ];
    for (what, pkg) in cases {
        let rig = Rig::new("stage-job");
        rig.mac.add(pkg);
        let (result, _) = rig.try_stage(&rig.order_run("1.0", &BOTH), "svc-1.0");
        assert_eq!(reason(result), Reason::ManifestInvalid, "{what}");
        assert!(rig.snapshot().is_empty(), "{what}");
        no_install_ran(&rig);
    }
}

/// Catches: a staging directory that already holds a name `stage` writes (or a failed copy)
/// being reported as a success.
#[test]
fn stage_reports_io_error_when_the_staging_directory_cannot_take_its_files() {
    for (what, make) in [
        ("macos-pkg", "dir"),
        ("tree", "full dir"),
        ("artifact", "dir"),
    ] {
        let rig = Rig::new("stage-io-staging");
        rig.mac.add(v1());
        let staging = rig.packages.join("staging");
        fs::create_dir_all(staging.join(what)).unwrap();
        if make == "full dir" {
            fs::write(staging.join(what).join("occupied"), b"x").unwrap();
        }
        let result = rig.pkg().stage(
            &rig.order_run("1.0", &BOTH),
            &rig.artifact("svc-1.0"),
            &staging,
        );
        assert_eq!(reason(result), Reason::IoError, "{what}");
    }
}
