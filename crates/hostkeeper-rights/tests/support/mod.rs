//! Shared by the tests: a valid declaration, a scratch directory and a declaration made by hand.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_rights::{Declaration, Refusal};

/// A declaration that keeps every rule, shaped like the example of the design (the numbers and
/// the paths are the tierstore owner's), with only the keys the launch still reads.
pub const SAMPLE: &str = r#"
name = "tierstore"
binary = "tierstore"
state_dir = "/var/lib/updater/hooks/tierstore"

[hook]
user = "tierstore-update"
group = "tierstore-update"
capabilities = []
read_paths = ["/usr/libexec/kbf", "/usr/sbin/nft", "/etc/tierstore", "/etc/updater.d/tierstore.toml"]
write_paths = []
address_families = ["AF_UNIX", "AF_INET", "AF_INET6", "AF_NETLINK"]
environment = { }

[hook.step_capabilities]
prepare = ["CAP_NET_ADMIN"]
cutover = ["CAP_NET_ADMIN"]
verify = []
rollback = ["CAP_NET_ADMIN"]
cleanup = []

[hook.timeout_secs]
prepare = 600
cutover = 300
verify = 1000
rollback = 600
cleanup = 1300
"#;

/// `SAMPLE` with `from` written as `to`. Panics if `from` is not in it, so a test cannot pass by
/// changing nothing.
pub fn with(from: &str, to: &str) -> String {
    with_in(SAMPLE, from, to)
}

pub fn with_in(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "{from:?} is not in the text");
    text.replacen(from, to, 1)
}

pub fn sample() -> Declaration {
    Declaration::parse(SAMPLE).expect("the sample is a declaration")
}

/// The refusal of a text that must be refused.
pub fn refusal(text: &str) -> Refusal {
    Declaration::parse(text).expect_err("the text must be refused")
}

/// A directory under the target directory, unique to the test, removed when it is dropped.
pub struct Scratch(hostkeeper_testkit::Scratch);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let dir = hostkeeper_testkit::Scratch::under(base, &format!("hostkeeper-rights-{name}"))
            .expect("create the scratch directory");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).expect("set the mode");
        Scratch(dir)
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// A file of this mode, whatever the umask.
    pub fn file(&self, name: &str, text: &str, mode: u32) -> PathBuf {
        let path = self.0.path().join(name);
        let _ = fs::remove_file(&path);
        fs::write(&path, text).expect("write the file");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set the mode");
        path
    }

    /// A directory of this mode, whatever the umask.
    pub fn dir(&self, name: &str, mode: u32) -> PathBuf {
        let path = self.0.path().join(name);
        fs::create_dir_all(&path).expect("create the directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set the mode");
        path
    }
}

/// A declaration made by hand: its hook account has the uid `hook_uid` and the host declares
/// these `names` (the service's own among them).
pub fn declared(
    declaration: Declaration,
    hook_uid: u32,
    names: &[&str],
) -> hostkeeper_rights::Declared {
    hostkeeper_rights::Declared::by_hand(declaration, hook_uid, names)
}
