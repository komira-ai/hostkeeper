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
fn a_release_is_four_debs_and_four_rpms_sorted() {
    assert_eq!(
        HOSTKEEPER.deb_files("1.2.3"),
        [
            "hostkeeper-coordinator_1.2.3_amd64.deb",
            "hostkeeper-coordinator_1.2.3_arm64.deb",
            "hostkeeper_1.2.3_amd64.deb",
            "hostkeeper_1.2.3_arm64.deb",
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
fn neither_package_requires_a_postun_script() {
    // nfpm writes %preun for `preremove` and nothing for %postun.
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
