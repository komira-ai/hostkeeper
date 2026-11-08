//! The directories `prepare` judges, on a real file tree laid out as the daemon lays out a
//! package.

use std::path::Path;

use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::package;
use hostkeeper_mdm_step::RealHost;
use hostkeeper_testkit::Scratch;

use super::package_dirs;
use crate::MdmError;
use crate::install::no_link_at;

/// A package example: the documented settings with the addresses filled in and every key the
/// package fixes left out.
fn config() -> MdmConfig {
    let example = include_str!("../../../hostkeeper-mdm-core/config.example");
    let text: String = example
        .replace(
            "\nMDM_BIND=\n",
            &format!("\nMDM_BIND={}\n", std::net::Ipv4Addr::new(10, 0, 0, 10)),
        )
        .replace(
            "\nMDM_LAN_CIDR=\n",
            &format!(
                "\nMDM_LAN_CIDR={}/24\n",
                std::net::Ipv4Addr::new(10, 0, 0, 0)
            ),
        )
        .lines()
        .filter(|l| {
            !package::FIXED
                .iter()
                .any(|(k, _)| l.starts_with(&format!("{k}=")))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    MdmConfig::parse(&package::settings(&text).expect("fits")).expect("parses")
}

/// Catches `prepare` refusing every update on a real host: under a tree where
/// `packages/mdm/current` is the daemon's link to a version, the walk to the package's binary
/// directory refuses that link, so the kit's check of `MDM_BIN` would end every `prepare`; the
/// directories the hook judges pass, and a link at one of them is still refused.
#[test]
fn the_hook_does_not_walk_the_daemons_current_link_and_still_refuses_a_linked_state() {
    let base = std::env::current_exe().expect("the test binary");
    let base = base.parent().expect("its directory");
    let scratch = Scratch::under(base, "hook-dirs").expect("a scratch directory");
    let root = scratch.path();
    let packages = root.join("var/lib/hostkeeper/packages/mdm");
    std::fs::create_dir_all(packages.join("1.0.0/bin")).expect("the version");
    std::os::unix::fs::symlink("1.0.0", packages.join("current")).expect("current");
    for dir in ["etc/mdm", "var/lib"] {
        std::fs::create_dir_all(root.join(dir)).expect("a directory");
    }
    let host = RealHost::under(root);
    let c = config();

    assert!(no_link_at(&host, &package_dirs(&c)).is_ok());
    let kit = no_link_at(&host, &[c.bin().to_string()]).expect_err("the walk refuses current");
    assert!(matches!(kit, MdmError::Io(_)), "{kit}");

    std::os::unix::fs::symlink(Path::new("/elsewhere"), root.join("var/lib/mdm")).expect("a link");
    let linked = no_link_at(&host, &package_dirs(&c)).expect_err("a linked state");
    assert_eq!(
        linked.to_string(),
        "/var/lib/mdm is a symbolic link: name the real directory"
    );
}
