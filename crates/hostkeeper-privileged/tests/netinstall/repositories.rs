//! The repository entries the installed host gets (`hostkeeper-pxe serve --repositories`,
//! packages-repositories.md sections 3.1 and 3.2): a directory of the operator's, written by the
//! test with the files the coordinator's registry renders for the entry [`NAME`] (the rig's apt
//! repository, served by the channel on the LAN, which the rig puts in the registry):
//! `komira-hk.sources` and the keyring `komira.gpg` (the repository's key), so that the daemon,
//! pushed the registry, adopts them; and the check, over the operator account's
//! SSH, that the installed system holds exactly those files in `/etc/hostkeeper/repositories/` as
//! root's with mode 0644. That the daemon started on them is shown by its heartbeats, since it
//! refuses to start on an entry its loader refuses; that it installs from them, by the fixture
//! service the guest is ordered.

use std::path::Path;

use super::guest::Ssh;
use crate::support::pair::put;

/// The name of the repository in the coordinator's registry, which the rollout file names as the
/// fixture service's `repository`.
pub const NAME: &str = "komira";
/// The name of the keyring the entry names: `<name>.gpg`.
pub const KEYRING: &str = "komira.gpg";
/// The channel of the entry, `<name>-<suite>`: the stem of its `.sources` file, which an order's
/// entry names as its `repository`.
pub const ENTRY: &str = "komira-hk";

/// Where the installed host keeps them.
pub const HOST_DIR: &str = "/etc/hostkeeper/repositories";

/// The files of the directory, `(name, bytes)`, in name order: the keyring `keyring` (binary, as
/// the loader requires) and the entry `entry`, the text of `<ENTRY>.sources`.
pub fn files(entry: &str, keyring: &[u8]) -> Vec<(String, Vec<u8>)> {
    vec![
        (KEYRING.to_owned(), keyring.to_vec()),
        (format!("{ENTRY}.sources"), entry.as_bytes().to_vec()),
    ]
}

/// Writes `files` into the directory `dir` (made, mode 0755), root's with mode 0644.
pub fn write(dir: &Path, files: &[(String, Vec<u8>)]) {
    for (name, bytes) in files {
        put(&dir.join(name), bytes, 0o644);
    }
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the directory's mode");
}

/// Asserts that the installed system holds exactly `files` in `/etc/hostkeeper/repositories/`,
/// each root:root, mode 644, with the bytes given.
pub fn assert_installed(ssh: &Ssh, files: &[(String, Vec<u8>)]) {
    let listing = ssh
        .output(&format!("sudo ls -A {HOST_DIR}"), b"")
        .expect("the host's repository directory");
    let want: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
    let found: Vec<String> = String::from_utf8_lossy(&listing)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(found, want, "the files of {HOST_DIR}");
    for (name, bytes) in files {
        let path = format!("{HOST_DIR}/{name}");
        let stat = ssh
            .output(&format!("sudo stat -c '%U:%G %a' {path}"), b"")
            .expect("stat");
        assert_eq!(
            String::from_utf8_lossy(&stat).trim(),
            "root:root 644",
            "{path}"
        );
        let content = ssh
            .output(&format!("sudo cat {path}"), b"")
            .expect("the file");
        assert_eq!(&content, bytes, "{path}");
    }
}
