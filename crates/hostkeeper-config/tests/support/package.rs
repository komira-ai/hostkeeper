//! A version directory of tierstore 1.5.0 built from the example of design section 4.7, which
//! each manifest case breaks in one thing.

use std::num::NonZeroU64;
use std::path::PathBuf;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest, ManifestRefusal};
use hostkeeper_config::names::{ServiceName, Version};

use super::Scratch;

/// The example of design section 4.7, with one `[params]` entry.
pub const EXAMPLE: &str = r#"schema         = 1
name           = "tierstore"
version        = "1.5.0"
min_hostkeeper = "0.4.0"          # oldest daemon that can run this package

[hook]
command = ["bin/tierstore", "update-hook"]    # relative to the version directory
boot    = "restore"
verify_deadline_secs   = 1600
promotion_ceiling_secs = 3600
cleanup_retries        = 3
[hook.timeout_secs]
prepare = 0
cutover = 300
verify  = 1000
rollback = 600
cleanup = 1300
uninstall = 300

[units]
run = ["tierstore@9301.service", "tierstore@9302.service"]

[health]
url = "http://127.0.0.1:{port}/healthz"

[[files]]
source = "files/tierstore.conf"
path   = "/etc/tierstore/tierstore.conf"

[params]
promotion_bound_secs = 900
"#;

/// `EXAMPLE` with `from` written as `to`. Panics if `from` is not in it.
pub fn with(from: &str, to: &str) -> String {
    with_in(EXAMPLE, from, to)
}

pub fn with_in(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "{from:?} is not in the text");
    text.replacen(from, to, 1)
}

pub const RUNNING: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 4,
    patch: 0,
};

/// A version directory of tierstore 1.5.0: the manifest, `bin/tierstore`, `units/` with the
/// template, and `files/tierstore.conf`.
pub struct Package {
    pub scratch: Scratch,
}

impl Package {
    pub fn new(name: &str) -> Package {
        let scratch = Scratch::new(&format!("manifest-{name}"));
        scratch.file("bin/tierstore", "#!/bin/false\n", 0o555);
        scratch.file("units/tierstore@.service", "[Service]\n", 0o444);
        scratch.file("files/tierstore.conf", "x = 1\n", 0o444);
        let package = Package { scratch };
        package.manifest(EXAMPLE);
        package
    }

    pub fn dir(&self) -> PathBuf {
        self.scratch.path().to_path_buf()
    }

    pub fn manifest(&self, text: &str) {
        self.scratch.file("manifest.toml", text, 0o444);
    }

    pub fn load(&self) -> Result<Manifest, ManifestRefusal> {
        Manifest::load(&self.dir(), &expected("tierstore", "1.5.0"), &RUNNING)
    }

    pub fn load_text(&self, text: &str) -> Result<Manifest, ManifestRefusal> {
        self.manifest(text);
        self.load()
    }

    pub fn refusal(&self, text: &str) -> ManifestRefusal {
        self.load_text(text)
            .expect_err("the manifest must be refused")
    }
}

pub fn expected(name: &str, version: &str) -> Expected {
    Expected {
        name: ServiceName::new(name).unwrap(),
        version: Version::new(version).unwrap(),
    }
}

pub fn secs(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}

pub fn unknown(table: &str, key: &str) -> ManifestRefusal {
    ManifestRefusal::UnknownKey {
        table: table.to_owned(),
        key: key.to_owned(),
    }
}

pub fn invalid_key(refusal: ManifestRefusal) -> String {
    match refusal {
        ManifestRefusal::Invalid { key, .. } => key,
        other => panic!("not an invalid value: {other:?}"),
    }
}
