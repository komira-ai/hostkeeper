//! `stage` of hostkeeper's own `.pkg` (packages-native.md section 10.4): the script `postinstall`
//! allowed, the payload held to its own list, and the job need not be `Disabled`; and every
//! other package keeping the checks of section 4.7.

use hostkeeper_install::macos_pkg::own;

use super::*;

const OWN_SERVICE: &str = own::SERVICE;

const WITH_POSTINSTALL: &str = "<scripts><postinstall file=\"./postinstall\"/></scripts>";

/// The plist of the daemon's job, not `Disabled`, as the pkg installs it.
fn daemon_plist() -> String {
    job_plist(&own::identifier(), "/usr/local/bin/hostkeeper", None)
}

/// hostkeeper's own package at 1.0: its list with the script `postinstall`, as a product archive.
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

/// Proves hostkeeper's own `.pkg`, holding exactly its list and the script `postinstall` (in the
/// component's `Scripts` and its `PackageInfo`), stages: as a product archive and as a bare
/// component, and with its job (a `run` label of the order) not `Disabled`.
#[test]
fn hostkeepers_own_pkg_with_its_list_and_its_postinstall_stages() {
    staged_own("a product archive", own_pkg(), &[]);
    let mut component = own_pkg();
    component.product = false;
    staged_own("a bare component", component, &[]);
    let label = own::identifier();
    staged_own("the job run, not Disabled", own_pkg(), &[label.as_str()]);
}

/// Proves the list is exact: a path outside it, a listed path of another mode or kind, and an
/// entry the path rule of a service would allow are `manifest_invalid`, naming the entry.
#[test]
fn an_entry_outside_the_own_list_is_manifest_invalid() {
    let binary = "/usr/local/bin/hostkeeper";
    let cases: Vec<(&str, FakePkg, String)> = vec![
        (
            "a file beside the binary",
            own_pkg().put("/usr/local/bin/extra", Content::File(b"x".to_vec()), 0o755),
            "/usr/local/bin/extra is not on hostkeeper's own package's list".into(),
        ),
        (
            "a directory of the rule for a service",
            own_pkg().put("/Library/Application Support", Content::Dir, 0o755),
            "/Library/Application Support is not on hostkeeper's own package's list".into(),
        ),
        (
            "a setuid binary",
            own_pkg().put(binary, Content::File(b"x".to_vec()), 0o4755),
            format!(
                "{binary} has mode 4755: a setuid, setgid or sticky bit, or writable by group or others"
            ),
        ),
        (
            "the binary with another mode",
            own_pkg().put(binary, Content::File(b"x".to_vec()), 0o700),
            format!(
                "{binary} is File with mode 700, and hostkeeper's own package lists File with mode 755"
            ),
        ),
        (
            "a directory with another mode",
            own_pkg().put("/usr/local/lib", Content::Dir, 0o700),
            "/usr/local/lib is Dir with mode 700, and hostkeeper's own package lists Dir with mode 755"
                .into(),
        ),
        (
            "a directory where the list has a file",
            own_pkg().put(binary, Content::Dir, 0o755),
            format!(
                "{binary} is Dir with mode 755, and hostkeeper's own package lists File with mode 755"
            ),
        ),
    ];
    for (what, pkg, detail) in cases {
        let err = refused_as_own(what, pkg);
        assert_eq!(err.reason(), Reason::ManifestInvalid, "{what}: {err}");
        assert_eq!(err.detail(), detail, "{what}");
    }
}

/// Proves only `postinstall` is allowed: a `preinstall` or `postupgrade` beside it or alone in
/// the component's `Scripts`, a `preinstall` or `preupgrade` element in the `PackageInfo`, and a
/// `Scripts` beside the component are `package_scripts`, and a `Distribution` script element is
/// `manifest_invalid`, for hostkeeper's own package.
#[test]
fn only_the_postinstall_script_is_allowed_to_hostkeepers_own_pkg() {
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
    let mut script_element = own_pkg();
    script_element.distribution = Some(script_element.distribution_text().replace(
        "</installer-gui-script>",
        "<script>x</script></installer-gui-script>",
    ));
    let cases: Vec<(&str, FakePkg, Reason, &str)> = vec![
        (
            "a preinstall beside the postinstall",
            scripts_of(&["postinstall", "preinstall"]),
            Reason::PackageScripts,
            "the package carries a script: the component's \"Scripts\" holds \"postinstall\", \"preinstall\", not \"postinstall\" alone",
        ),
        (
            "a postupgrade alone",
            scripts_of(&["postupgrade"]),
            Reason::PackageScripts,
            "the package carries a script: the component's \"Scripts\" holds \"postupgrade\", not \"postinstall\" alone",
        ),
        (
            "a preinstall element",
            element("preinstall"),
            Reason::PackageScripts,
            "the package carries a script: the PackageInfo has a <preinstall> element",
        ),
        (
            "a preupgrade element",
            element("preupgrade"),
            Reason::PackageScripts,
            "the package carries a script: the PackageInfo has a <preupgrade> element",
        ),
        (
            "a Scripts beside the component",
            beside,
            Reason::PackageScripts,
            "the package carries a script: the product archive holds \"Scripts\"",
        ),
        (
            "a Distribution script element",
            script_element,
            Reason::ManifestInvalid,
            "the Distribution has a <script> element",
        ),
    ];
    for (what, pkg, reason, detail) in cases {
        let err = refused_as_own(what, pkg);
        assert_eq!(err.reason(), reason, "{what}: {err}");
        assert_eq!(err.detail(), detail, "{what}");
    }
}

/// Proves the exception is for hostkeeper's own package alone: its script in a package of another
/// service with the daemon's identifier, and in the service `hostkeeper` under another
/// identifier, is `package_scripts`, and its payload under either is held to the rule of section
/// 4.7 (the daemon's plist is not a job the order runs).
#[test]
fn the_exception_is_not_another_packages() {
    let id = own::identifier();
    let scripted = |service: &str, identifier: &str| {
        let mut pkg = own_pkg();
        pkg.service = service.into();
        pkg.identifier = identifier.into();
        pkg.package_info = Some(pkg.package_info_text().replace(
            &format!("identifier=\"{id}\""),
            &format!("identifier=\"{identifier}\""),
        ));
        pkg
    };
    for (what, service, identifier) in [
        ("another service", SERVICE, id.as_str()),
        ("another identifier", OWN_SERVICE, IDENTIFIER),
    ] {
        let err = refused(what, scripted(service, identifier), service, identifier);
        assert_eq!(err.reason(), Reason::PackageScripts, "{what}: {err}");
        assert_eq!(
            err.detail(),
            "the package carries a script: the component holds \"Scripts\"",
            "{what}"
        );
        let mut bare = scripted(service, identifier);
        bare.component_extras.clear();
        bare.package_info = Some(
            bare.package_info_text()
                .replace(WITH_POSTINSTALL, "<scripts/>"),
        );
        let err = refused(what, bare, service, identifier);
        assert_eq!(err.reason(), Reason::ManifestInvalid, "{what}: {err}");
        let home = format!("/Library/Application Support/{service}");
        assert_eq!(
            err.detail(),
            format!(
                "/Library/LaunchDaemons/{id}.plist is outside {home}/ and the run labels' plists"
            ),
            "{what}"
        );
    }
}
