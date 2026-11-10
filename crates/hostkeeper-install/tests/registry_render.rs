//! `hostkeeper_release::registry::render` against the loader: the files the coordinator's set
//! renders for a host are a directory `config::load_with_keyrings` accepts (packages-repositories.md
//! sections 3.1 and 3.2), so a pushed set the daemon writes is one it loads.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_install::repository::config::load_with_keyrings;
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::registry::{MIRROR_DIR, render};
use hostkeeper_testkit::Scratch;

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn entry(name: &str, suites: &[&str], keyring: &[u8]) -> Repository {
    Repository {
        name: name.into(),
        kind: "apt".into(),
        uri: "https://repo.example.com/debian".into(),
        suites: suites.iter().map(|s| (*s).to_owned()).collect(),
        components: vec!["main".into(), "contrib".into()],
        fingerprints: Vec::new(),
        keyring: keyring.to_vec(),
        ..Repository::default()
    }
}

/// Proves the files `render` makes, written 0644 in a 0755 directory its owner owns, load: each
/// suite is an entry named `<name>-<suite>`, its components in order, its keyring under the
/// directory `render` was given, and the bytes the loader read are the entry's keyring.
#[test]
fn rendered_files_load_as_one_entry_per_channel_with_the_entrys_keyring() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "registry-render")
        .expect("a scratch directory");
    let dir = scratch.path().join("repositories");
    fs::create_dir(&dir).expect("the directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("the mode");
    let komira = entry(
        "komira",
        &["testing", "stable"],
        &[0x99, 0x01, 0x0d, 0x04, 0x00],
    );
    let other = entry("other", &["main"], &[0x98, 0x0a, 0xff, 0x01]);
    let entries = [komira.clone(), other.clone()];
    let placed = dir.to_str().expect("a utf-8 path");
    for file in render(&entries, placed, MIRROR_DIR) {
        let path = dir.join(&file.name);
        fs::write(&path, &file.bytes).expect("the file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("the mode");
    }
    let loaded = load_with_keyrings(&dir, &dir, me()).expect("the rendered directory loads");
    let names: Vec<String> = ["komira-stable", "komira-testing", "other-main"]
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    for name in &names {
        let found = loaded.repositories.get(name).expect("the channel's entry");
        let stem = name.split('-').next().expect("a stem");
        assert_eq!(found.keyring, dir.join(format!("{stem}.gpg")));
        assert_eq!(found.uri, "https://repo.example.com/debian");
        assert_eq!(found.components, ["main", "contrib"]);
    }
    assert!(loaded.repositories.get("komira").is_none());
    let keyrings: Vec<(PathBuf, &Vec<u8>)> = loaded
        .keyrings
        .iter()
        .map(|(p, b)| (p.clone(), b))
        .collect();
    assert_eq!(
        keyrings,
        vec![
            (dir.join("komira.gpg"), &komira.keyring),
            (dir.join("other.gpg"), &other.keyring),
        ]
    );
}
