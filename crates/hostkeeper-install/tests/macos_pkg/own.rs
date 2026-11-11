//! `stage` of hostkeeper's own `.pkg` (packages-native.md section 10.4): a package like any other,
//! with its script `postinstall` and its job not `Disabled`, whose payload is held to no list.

use hostkeeper_install::macos_pkg::own;

use super::*;

const OWN_SERVICE: &str = own::SERVICE;

const WITH_POSTINSTALL: &str = "<scripts><postinstall file=\"./postinstall\"/></scripts>";

/// The plist of the daemon's job, not `Disabled`, as the pkg installs it.
fn daemon_plist() -> String {
    job_plist(&own::identifier(), "/usr/local/bin/hostkeeper", None)
}

/// hostkeeper's own package at 1.0: the entries `cargo xtask release macos-pkg` lays out, with
/// the script `postinstall`, as a product archive.
fn own_pkg() -> FakePkg {
    let mut pkg = FakePkg::standard(OWN_SERVICE, &own::identifier(), "1.0");
    pkg.payload.clear();
    for dir in own::dirs() {
        pkg = pkg.put(&dir, Content::Dir, 0o755);
    }
    for (path, mode) in own::files() {
        let bytes = match path.ends_with(".plist") {
            true => daemon_plist().into_bytes(),
            false => path.clone().into_bytes(),
        };
        pkg = pkg.put(&path, Content::File(bytes), mode);
    }
    pkg.package_info = Some(
        pkg.package_info_text()
            .replace("<scripts/>", WITH_POSTINSTALL),
    );
    pkg.product = true;
    pkg.component_extras = vec!["Scripts".into()];
    pkg
}

fn order_for(rig: &Rig, service: &str, identifier: &str, run: &[&str]) -> Order {
    let mut order = rig.order_run("1.0", run);
    order.service = ServiceName::new(service).unwrap();
    order.identifier = identifier.into();
    order
}

/// Stages `pkg` as `service` with `identifier`: the failure it is refused with, the staging
/// directory left empty of anything but what the core removes, and nothing installed.
fn refused(what: &str, pkg: FakePkg, service: &str, identifier: &str) -> Failure {
    let rig = Rig::new("stage-own-refused");
    rig.mac.add(pkg);
    let order = order_for(&rig, service, identifier, &[]);
    let (result, _) = rig.try_stage(&order, "hostkeeper-1.0");
    let err = result.unwrap_err();
    assert_eq!(err.reason().class(), Class::Release, "{what}: {err}");
    assert_eq!(err.reason().outcome(), Outcome::Failed, "{what}");
    assert!(rig.snapshot().is_empty(), "{what}");
    assert!(rig.records.all().is_empty(), "{what}");
    no_install_ran(&rig);
    err
}

fn refused_as_own(what: &str, pkg: FakePkg) -> Failure {
    refused(what, pkg, OWN_SERVICE, &own::identifier())
}

fn staged_own(what: &str, pkg: FakePkg, run: &[&str]) {
    let rig = Rig::new("stage-own");
    rig.mac.add(pkg);
    let order = order_for(&rig, OWN_SERVICE, &own::identifier(), run);
    let (result, staging) = rig.try_stage(&order, "hostkeeper-1.0");
    result.unwrap_or_else(|e| panic!("{what}: {e}"));
    let tree = staging.join("tree");
    for (path, _) in own::files() {
        assert!(
            tree.join(path.trim_start_matches('/')).is_file(),
            "{what}: {path}"
        );
    }
}

/// Stages `pkg` as hostkeeper's own package, which must succeed.
fn staged_own_at(what: &str, pkg: FakePkg) {
    let rig = Rig::new("stage-own-at");
    rig.mac.add(pkg);
    let order = order_for(&rig, OWN_SERVICE, &own::identifier(), &[]);
    let (result, _) = rig.try_stage(&order, "hostkeeper-1.0");
    result.unwrap_or_else(|e| panic!("{what}: {e}"));
}

/// Proves hostkeeper's own `.pkg`, holding the entries the release lays out and the script
/// `postinstall` (in the component's `Scripts` and its `PackageInfo`), stages: as a product
/// archive and as a bare component, and with its job (a `run` label of the order) not `Disabled`.
#[test]
fn hostkeepers_own_pkg_with_its_entries_and_its_postinstall_stages() {
    staged_own("a product archive", own_pkg(), &[]);
    let mut component = own_pkg();
    component.product = false;
    staged_own("a bare component", component, &[]);
    let label = own::identifier();
    staged_own("the job run, not Disabled", own_pkg(), &[label.as_str()]);
}

/// Proves the package has no list of paths: a file beside the binary, a directory a service's
/// rule would not allow, the binary with another mode and a directory where the release has a
/// file stage as for any package.
#[test]
fn an_entry_outside_the_release_layout_stages() {
    let binary = "/usr/local/bin/hostkeeper";
    let cases: Vec<(&str, FakePkg)> = vec![
        (
            "a file beside the binary",
            own_pkg().put("/usr/local/bin/extra", Content::File(b"x".to_vec()), 0o755),
        ),
        (
            "a directory above a service's",
            own_pkg().put("/Library/Application Support", Content::Dir, 0o755),
        ),
        (
            "the binary with another mode",
            own_pkg().put(binary, Content::File(b"x".to_vec()), 0o700),
        ),
        (
            "a directory with another mode",
            own_pkg().put("/usr/local/lib", Content::Dir, 0o700),
        ),
        (
            "a directory where the release has a file",
            own_pkg().put(binary, Content::Dir, 0o755),
        ),
    ];
    for (what, pkg) in cases {
        staged_own_at(what, pkg);
    }
}

/// Proves the mode rule of the sealed tree still holds for the package: a setuid binary is
/// `manifest_invalid`, naming the entry.
#[test]
fn a_setuid_binary_is_manifest_invalid() {
    let binary = "/usr/local/bin/hostkeeper";
    let err = refused_as_own(
        "a setuid binary",
        own_pkg().put(binary, Content::File(b"x".to_vec()), 0o4755),
    );
    assert_eq!(err.reason(), Reason::ManifestInvalid, "{err}");
    assert_eq!(
        err.detail(),
        format!(
            "{binary} has mode 4755: a setuid, setgid or sticky bit, or writable by group or others"
        )
    );
}

/// Proves every script is allowed: a `preinstall` or `postupgrade` beside the `postinstall` or
/// alone in the component's `Scripts`, a `preinstall` or `preupgrade` element in the
/// `PackageInfo`, and a `Scripts` beside the component stage.
#[test]
fn every_script_is_allowed_to_hostkeepers_own_pkg() {
    let scripts_of = |files: &[&str]| {
        let mut pkg = own_pkg();
        pkg.script_files = files.iter().map(|f| (*f).to_owned()).collect();
        pkg
    };
    let element = |name: &str| {
        let mut pkg = own_pkg();
        let both = format!(
            "<scripts><postinstall file=\"./postinstall\"/><{name} file=\"./{name}\"/></scripts>"
        );
        pkg.package_info = Some(pkg.package_info_text().replace(WITH_POSTINSTALL, &both));
        pkg
    };
    let mut beside = own_pkg();
    beside.extras = vec!["Scripts".into()];
    let cases: Vec<(&str, FakePkg)> = vec![
        (
            "a preinstall beside the postinstall",
            scripts_of(&["postinstall", "preinstall"]),
        ),
        ("a postupgrade alone", scripts_of(&["postupgrade"])),
        ("a preinstall element", element("preinstall")),
        ("a preupgrade element", element("preupgrade")),
        ("a Scripts beside the component", beside),
    ];
    for (what, pkg) in cases {
        staged_own_at(what, pkg);
    }
}

/// Proves a Distribution script element is still `manifest_invalid` for hostkeeper's own
/// package: it decides what installs.
#[test]
fn a_distribution_script_element_is_manifest_invalid() {
    let mut pkg = own_pkg();
    pkg.distribution = Some(pkg.distribution_text().replace(
        "</installer-gui-script>",
        "<script>x</script></installer-gui-script>",
    ));
    let err = refused_as_own("a Distribution script element", pkg);
    assert_eq!(err.reason(), Reason::ManifestInvalid, "{err}");
    assert_eq!(err.detail(), "the Distribution has a <script> element");
}

/// Proves the package is no exception and no special case: the same package under another
/// service, and under the service `hostkeeper` with another identifier, stages like any other.
#[test]
fn the_package_of_another_service_or_identifier_stages_alike() {
    let id = own::identifier();
    for (what, service, identifier) in [
        ("another service", SERVICE, id.as_str()),
        ("another identifier", OWN_SERVICE, IDENTIFIER),
    ] {
        let mut pkg = own_pkg();
        pkg.service = service.into();
        pkg.identifier = identifier.into();
        pkg.package_info = Some(pkg.package_info_text().replace(
            &format!("identifier=\"{id}\""),
            &format!("identifier=\"{identifier}\""),
        ));
        let rig = Rig::new("stage-not-own");
        rig.mac.add(pkg);
        let order = order_for(&rig, service, identifier, &[]);
        let (result, _) = rig.try_stage(&order, "hostkeeper-1.0");
        result.unwrap_or_else(|e| panic!("{what}: {e}"));
    }
}
