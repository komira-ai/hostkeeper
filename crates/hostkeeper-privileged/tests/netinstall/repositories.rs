//! The repository entries the installed host gets (`hostkeeper-pxe serve --repositories`,
//! packages-repositories.md sections 3.1 and 3.2): a directory of the operator's, written by the
//! test with the entry `komira-testing.sources` and its keyring, and the check, over the
//! operator account's SSH, that the installed system holds exactly those files in
//! `/etc/hostkeeper/repositories/` as root's with mode 0644. That the daemon started on them is
//! shown by its heartbeats, since it refuses to start on an entry its loader refuses.

use std::path::Path;

use super::guest::Ssh;
use crate::support::pair::put;

/// The name of the keyring the entry names.
pub const KEYRING: &str = "komira-archive-keyring.gpg";

/// Where the installed host keeps them.
const HOST_DIR: &str = "/etc/hostkeeper/repositories";

/// The keyring's bytes: not text, with a zero byte and a high one. The loader does not read it.
const KEYRING_BYTES: [u8; 8] = [0x99, 0x01, 0x0d, 0x00, 0xff, 0x0a, 0x7f, 0x80];

/// The files of the directory, `(name, bytes)`, in name order.
pub fn files() -> Vec<(String, Vec<u8>)> {
    let entry = format!(
        "Types: deb\nURIs: https://packages.example.com/apt\nSuites: testing\nComponents: main\n\
         Signed-By: {HOST_DIR}/{KEYRING}\n"
    );
    vec![
        (KEYRING.to_owned(), KEYRING_BYTES.to_vec()),
        ("komira-testing.sources".to_owned(), entry.into_bytes()),
    ]
}

/// Writes the files into the directory `dir` (made, mode 0755), root's with mode 0644.
pub fn write(dir: &Path) {
    for (name, bytes) in files() {
        put(&dir.join(name), &bytes, 0o644);
    }
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the directory's mode");
}

/// Asserts that the installed system holds exactly [`files`] in `/etc/hostkeeper/repositories/`,
/// each root:root, mode 644, with the bytes given.
pub fn assert_installed(ssh: &Ssh) {
    let listing = ssh
        .output(&format!("sudo ls -A {HOST_DIR}"), b"")
        .expect("the host's repository directory");
    let want: Vec<String> = files().into_iter().map(|(name, _)| name).collect();
    let found: Vec<String> = String::from_utf8_lossy(&listing)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(found, want, "the files of {HOST_DIR}");
    for (name, bytes) in files() {
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
        assert_eq!(content, bytes, "{path}");
    }
}
