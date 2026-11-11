//! The MDM kit's output for a Mac holds what the macos-pkg installer reads
//! (docs/design/packages-repositories.md, section 4.7): the config profile the gate builds from
//! the coordinator's `--mdm-stores` and `--mdm-macos-pkg` files carries the domains
//! `dev.hostkeeper.stores` and `dev.hostkeeper.macos-pkg`; macOS writes each domain's forced
//! settings to its managed preferences file; the daemon's start renders that file
//! ([`hostkeeper_daemon::mac_files`]); and the loaders the installer's start uses read back
//! exactly what the coordinator's files said. Every path is in a scratch directory, and the
//! trusted owner is the uid the tests run as.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_daemon::mac_files::{self, Rendered};
use hostkeeper_install::macos_pkg::signers;
use hostkeeper_mdm_gate::{
    CommandUuid, ConfigProfile, DaemonProfile, MACOS_PKG_DOMAIN, MacosPkgProfile,
    ProfileIdentifier, STORES_DOMAIN, StoresProfile,
};
use hostkeeper_package::stores;
use hostkeeper_testkit::Scratch;
use plist::{Dictionary, Value};

/// The coordinator's `--mdm-daemon-config`.
const CONFIG: &str = "coordinator = \"https://coordinator.example:8443\"\n\
ca_sha256 = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n\
release_channel = \"https://releases.example/path\"\n\
[trusted_signers]\n\
organisation = \"komira-ai\"\n\
workflow = \".github/workflows/release.yml\"\n\
refs = [\"refs/tags/*\"]\n\
hosted_runner = true\n";

/// The coordinator's `--mdm-stores`: the two channel stores.
const STORES: &str = "[[store]]\n\
name = \"komira-testing\"\n\
kind = \"https\"\n\
base = \"https://packages.example.com/pkg/\"\n\
\n\
[[store]]\n\
name = \"komira-stable\"\n\
kind = \"https\"\n\
base = \"https://packages.example.com/pkg/\"\n\
redirects = 2\n";

/// The coordinator's `--mdm-macos-pkg`, during a rotation's overlap.
const SIGNERS: &str = "signers = [\n\
\"00000000000000000000000000000000000000000000000000000000000000a1\",\n\
\"00000000000000000000000000000000000000000000000000000000000000B2\",\n\
]\n";

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn uuid(last: char) -> CommandUuid {
    CommandUuid::parse(&format!("00000000-0000-0000-0000-00000000000{last}"))
        .expect("a UUID of the test")
}

/// The profile the gate sends for the coordinator's three files.
fn profile_bytes() -> Vec<u8> {
    let profile = ConfigProfile::new(DaemonProfile::from_toml(CONFIG).expect("the daemon config"))
        .with_stores(StoresProfile::from_toml(STORES).expect("the stores"))
        .with_macos_pkg(MacosPkgProfile::from_toml(SIGNERS).expect("the signers"));
    let identifier = ProfileIdentifier::parse("com.example.hostkeeper.mdm").expect("identifier");
    profile.profile(&identifier, [&uuid('1'), &uuid('2')])
}

/// What macOS writes to `/Library/Managed Preferences/<domain>.plist` from `profile`: the forced
/// settings of `domain` in its one preferences payload.
fn managed_settings(profile: &[u8], domain: &str) -> Dictionary {
    let root = Value::from_reader_xml(profile).expect("the profile is an XML property list");
    let payloads = root
        .as_dictionary()
        .and_then(|d| d.get("PayloadContent"))
        .and_then(Value::as_array)
        .expect("the profile's payloads");
    let [payload] = payloads.as_slice() else {
        panic!("one payload, not {}", payloads.len());
    };
    let forced = payload
        .as_dictionary()
        .and_then(|d| d.get("PayloadContent"))
        .and_then(Value::as_dictionary)
        .and_then(|d| d.get(domain))
        .and_then(Value::as_dictionary)
        .and_then(|d| d.get("Forced"))
        .and_then(Value::as_array)
        .expect("the domain's forced settings");
    let [entry] = forced.as_slice() else {
        panic!("one forced entry, not {}", forced.len());
    };
    entry
        .as_dictionary()
        .and_then(|d| d.get("mcx_preference_settings"))
        .and_then(Value::as_dictionary)
        .cloned()
        .expect("the domain's settings")
}

/// `settings` written as the managed preferences file `path`, root's (here the test's) and 0644.
fn write_managed(path: &Path, settings: Dictionary) {
    Value::Dictionary(settings)
        .to_file_binary(path)
        .expect("the managed file is written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("mode 0644");
}

#[test]
fn the_profile_for_a_mac_renders_to_the_signers_and_stores_the_installer_reads() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "mdm_profile_files")
        .expect("the scratch directory");
    let root = scratch.path();
    let profile = profile_bytes();
    let managed_signers = root.join("dev.hostkeeper.macos-pkg.plist");
    let managed_stores = root.join("dev.hostkeeper.stores.plist");
    write_managed(
        &managed_signers,
        managed_settings(&profile, MACOS_PKG_DOMAIN),
    );
    write_managed(&managed_stores, managed_settings(&profile, STORES_DOMAIN));
    let etc = root.join("etc").join("hostkeeper");
    let signers_file = etc.join("macos-pkg.toml");
    let stores_file = etc.join("stores.toml");

    assert_eq!(
        mac_files::macos_pkg(&managed_signers, &signers_file, me()).expect("signers rendered"),
        Rendered::Written
    );
    assert_eq!(
        mac_files::stores(&managed_stores, &stores_file, me()).expect("stores rendered"),
        Rendered::Written
    );

    // The installer's start reads the very signers and stores the coordinator's files named.
    let read = signers::load(&signers_file, me())
        .expect("the installer's loader accepts the file")
        .expect("the file is there");
    assert_eq!(
        read,
        signers::parse(SIGNERS.as_bytes()).expect("the coordinator's file")
    );
    assert_eq!(
        stores::load(&stores_file, me()).expect("the stores loader accepts the file"),
        stores::parse(STORES.as_bytes(), me()).expect("the coordinator's file")
    );

    // The same profile again, as a gate re-send of unchanged settings: nothing is written.
    assert_eq!(
        mac_files::macos_pkg(&managed_signers, &signers_file, me()).expect("signers again"),
        Rendered::Unchanged
    );
}
