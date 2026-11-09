use super::*;

#[test]
fn file_names_are_the_ones_the_release_assets_have() {
    let daemon = &HOSTKEEPER.packages[0];
    let coordinator = &HOSTKEEPER.packages[1];
    assert_eq!(
        daemon.deb_file("1.2.3", &ARCHES[0]),
        "hostkeeper_1.2.3_amd64.deb"
    );
    assert_eq!(
        coordinator.deb_file("1.2.3", &ARCHES[1]),
        "hostkeeper-coordinator_1.2.3_arm64.deb"
    );
    assert_eq!(
        daemon.rpm_file("1.2.3", &ARCHES[1]),
        "hostkeeper-1.2.3-1.aarch64.rpm"
    );
    assert_eq!(
        coordinator.rpm_file("1.2.3", &ARCHES[0]),
        "hostkeeper-coordinator-1.2.3-1.x86_64.rpm"
    );
}

#[test]
fn a_release_is_seven_debs_and_four_rpms_sorted() {
    // Catches: mdm's .deb asked for arm64 (it is built for amd64 alone), hello's or mdm's asked
    // as an rpm (neither is one), and a service's .deb left out of the pass that lists the
    // release whole.
    assert_eq!(
        HOSTKEEPER.deb_files("1.2.3"),
        [
            "hello_1.2.3_amd64.deb",
            "hello_1.2.3_arm64.deb",
            "hostkeeper-coordinator_1.2.3_amd64.deb",
            "hostkeeper-coordinator_1.2.3_arm64.deb",
            "hostkeeper_1.2.3_amd64.deb",
            "hostkeeper_1.2.3_arm64.deb",
            "mdm_1.2.3_amd64.deb",
        ]
    );
    assert_eq!(
        HOSTKEEPER.rpm_files("1.2.3"),
        [
            "hostkeeper-1.2.3-1.aarch64.rpm",
            "hostkeeper-1.2.3-1.x86_64.rpm",
            "hostkeeper-coordinator-1.2.3-1.aarch64.rpm",
            "hostkeeper-coordinator-1.2.3-1.x86_64.rpm",
        ]
    );
}

#[test]
fn the_pool_and_the_session_names_are_the_products_own() {
    assert_eq!(HOSTKEEPER.pool_dir, format!("pool/{}", HOSTKEEPER.name));
    assert!(HOSTKEEPER.session_prefix.starts_with(HOSTKEEPER.name));
}

#[test]
fn an_rpm_entry_pairs_each_file_with_the_package_it_is_the_file_of() {
    let entries = HOSTKEEPER.rpm_entries("1.2.3");
    assert_eq!(entries.len(), 4);
    for (package, file) in &entries {
        assert!(
            file.starts_with(&format!("{}-1.2.3-1.", package.name)),
            "{file}"
        );
    }
    let names: Vec<&str> = entries.iter().map(|(p, _)| p.name).collect();
    assert_eq!(
        names,
        [
            "hostkeeper",
            "hostkeeper",
            "hostkeeper-coordinator",
            "hostkeeper-coordinator"
        ]
    );
}

#[test]
fn no_package_requires_a_postun_script() {
    // nfpm writes %preun for `preremove` and nothing for %postun; the services' packages are
    // no rpms at all.
    assert!(HOSTKEEPER.packages.iter().all(|p| !p.postun_required));
}

#[test]
fn the_architecture_lists_of_the_trees_are_the_specs() {
    use crate::channels::{apt, rpm};
    let debs: Vec<&str> = ARCHES.iter().map(|a| a.deb).collect();
    assert_eq!(debs, apt::state::DEFAULT_ARCHS);
    let mut rpms: Vec<&str> = ARCHES.iter().map(|a| a.rpm).collect();
    rpms.sort_unstable();
    assert_eq!(rpms, rpm::state::ARCHS);
}

#[test]
fn both_trees_keep_the_pool_directory_of_the_spec() {
    use crate::channels::{apt, rpm};
    assert_eq!(apt::state::POOL_DIR, HOSTKEEPER.pool_dir);
    assert_eq!(rpm::state::POOL_DIR, HOSTKEEPER.pool_dir);
    assert_eq!(
        apt::state::pool_key("hostkeeper_1.2.3_amd64.deb").expect("key"),
        "apt/pool/hostkeeper/hostkeeper_1.2.3_amd64.deb"
    );
    assert_eq!(
        rpm::state::pool_key("hostkeeper-coordinator-1.2.3-1.x86_64.rpm").expect("key"),
        "rpm/pool/hostkeeper/hostkeeper-coordinator-1.2.3-1.x86_64.rpm"
    );
}

#[test]
fn the_daemon_enables_its_unit_installs_systemd_first_and_has_no_user() {
    let daemon = HOSTKEEPER.package("hostkeeper").expect("daemon");
    assert!(daemon.enabled_after_install);
    assert_eq!(daemon.prerequisites, ["systemd"]);
    assert_eq!(daemon.user, None);
    assert_eq!(daemon.program(), "hostkeeper");
    assert_eq!(daemon.comm(), "hostkeeper");
    assert_eq!(daemon.units, ["hostkeeper.service"]);
    assert!(daemon.stamp_commit);
    assert_eq!(daemon.files.len(), 7);
}

#[test]
fn the_coordinator_enables_nothing_has_a_user_and_depends_on_systemd_itself() {
    let coordinator = HOSTKEEPER
        .package("hostkeeper-coordinator")
        .expect("coordinator");
    assert!(!coordinator.enabled_after_install);
    assert!(coordinator.prerequisites.is_empty());
    assert_eq!(coordinator.user, Some("hostkeeper-coordinator"));
    assert_eq!(coordinator.program(), "hostkeeper-coordinator");
    // The kernel keeps 15 bytes of a process's name.
    assert_eq!(coordinator.comm(), "hostkeeper-coor");
    assert_eq!(coordinator.files.len(), 6);
    assert_eq!(HOSTKEEPER.package("other-product"), None);
}

#[test]
fn every_file_a_package_ships_is_absolute_and_the_binary_and_unit_are_among_them() {
    for package in HOSTKEEPER.packages {
        assert!(package.files.iter().all(|f| f.starts_with('/')));
        assert!(package.files.contains(&package.binary));
        assert!(!package.units.is_empty(), "{}", package.name);
        for unit in package.units {
            assert!(
                package
                    .files
                    .contains(&format!("/usr/lib/systemd/system/{unit}").as_str()),
                "{unit}"
            );
        }
    }
}

#[test]
fn hellos_package_is_the_files_of_its_deb_says_its_version_alone_and_enables_nothing() {
    // Catches: the check holding hello to a file its .deb does not install (or missing one it
    // does), to a `--version` with a commit hello does not print, or to enabled units, which no
    // service's native package enables (it has no script).
    use crate::release::native::hello;
    let package = HOSTKEEPER.package("hello").expect("hello");
    let built: Vec<String> = hello::deb_files(Vec::new(), false)
        .into_iter()
        .map(|f| f.path)
        .collect();
    let mut listed: Vec<&str> = package.files.to_vec();
    listed.sort_unstable();
    assert_eq!(listed, built);
    assert_eq!(package.binary, hello::DEB_PROGRAM);
    assert_eq!(package.comm(), "hello");
    assert!(!package.stamp_commit);
    assert!(!package.enabled_after_install);
    assert!(!package.rpm);
    assert_eq!(package.arches, ARCHES);
    assert_eq!(package.units, ["hello@a.service", "hello@b.service"]);
    assert!(package.prerequisites.is_empty());
    assert_eq!(package.user, None);
}

#[test]
fn mdms_package_is_amd64_alone_with_the_units_of_its_deb_and_enables_nothing() {
    // Catches: mdm's units out of step with the ones its .deb installs, an arm64 mdm asked of
    // the repository, and the program or hook at another path than the .deb's.
    use hostkeeper_mdm_core::package::{DEB_BIN, SERVICE, UNITS};
    let package = HOSTKEEPER.package("mdm").expect("mdm");
    let mut built: Vec<&str> = UNITS.iter().map(|u| u.name).collect();
    built.sort_unstable();
    assert_eq!(package.units, built);
    assert_eq!(package.arches, [AMD64]);
    assert!(!package.rpm);
    assert!(package.stamp_commit);
    assert!(!package.enabled_after_install);
    assert_eq!(package.binary, format!("{DEB_BIN}/{SERVICE}"));
    assert!(
        package
            .files
            .contains(&format!("{DEB_BIN}/hostkeeper-hook").as_str())
    );
    assert_eq!(package.files.len(), 2 + UNITS.len());
}

#[test]
fn the_installers_and_their_two_channels_are_named_from_the_product() {
    assert_eq!(
        HOSTKEEPER.pkg_files("1.2.3"),
        [
            "hello-1.2.3-macos-arm64.pkg",
            "hostkeeper-1.2.3-macos-arm64.pkg"
        ]
    );
    assert_eq!(HOSTKEEPER.pkg_prefix("1.2.3"), "pkg/hostkeeper/1.2.3/");
    assert_eq!(HOSTKEEPER.pkg_stable_key(), "pkg/hostkeeper/stable.json");
    assert_eq!(HOSTKEEPER.pkg_dir, format!("pkg/{}", HOSTKEEPER.name));
}

#[test]
fn architectures_are_found_by_either_name() {
    assert_eq!(arch_of_rpm("aarch64").map(|a| a.deb), Some("arm64"));
    assert_eq!(arch_of_deb("amd64").map(|a| a.rpm), Some("x86_64"));
    assert_eq!(arch_of_rpm("amd64"), None);
    assert_eq!(arch_of_deb("x86_64"), None);
}
