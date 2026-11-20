//! The tests of the host's store: how the client of the coordinator is made from a daemon config
//! in a scratch directory, without reaching any coordinator (a host with no certificate makes no
//! call).

use std::fs;
use std::os::unix::fs::PermissionsExt;

use hostkeeper_testkit::Scratch;

use super::*;
use crate::testing::FakeClock;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A daemon config naming the coordinator `url`, owned by the test's user, mode 0600.
fn config(dir: &Path, url: &str) -> PathBuf {
    let path = dir.join("hostkeeper.toml");
    let text = format!(
        "coordinator = \"{url}\"\nca_sha256 = \"{}\"\nrelease_channel = \"https://releases.test\"\n\n\
         [trusted_signers]\norganisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
         refs = [\"refs/tags/*\"]\nhosted_runner = true\n",
        "ab".repeat(32)
    );
    fs::write(&path, text).expect("the config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("its mode");
    path
}

fn read(store: &HostStore) -> Result<Got, StoreError> {
    let limit = Limit {
        max_bytes: 1,
        deadline_secs: 1,
    };
    store.get_from("/v1/xcode/x", 0, limit, &mut |_| Ok(()))
}

fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Proves: a daemon config that is not there, or whose coordinator address cannot be dialed, is
/// `Unusable` naming why, and a host with no certificate in its pki directory is `NotEnrolled`:
/// the store is reached only by an enrolled host, through the coordinator its config names.
#[test]
fn the_host_store_needs_a_config_a_dialable_coordinator_and_a_certificate() {
    let dir = scratch("xcs-host-store");
    let clock: Arc<dyn Clock> = FakeClock::new();
    let pki = dir.path().join("pki");
    let missing = dir.path().join("missing.toml");
    let refused = read(&HostStore::new(&missing, uid(), &pki, Arc::clone(&clock)));
    let Err(StoreError::Unusable(detail)) = refused else {
        panic!("{refused:?}");
    };
    assert!(
        detail.starts_with(&format!("{}: ", missing.display())),
        "{detail}"
    );

    let undialable = config(dir.path(), "https://coordinator.test:8443/x");
    let refused = read(&HostStore::new(
        &undialable,
        uid(),
        &pki,
        Arc::clone(&clock),
    ));
    assert!(
        matches!(refused, Err(StoreError::Unusable(_))),
        "{refused:?}"
    );

    let good = config(dir.path(), "https://coordinator.test:8443");
    let refused = read(&HostStore::new(&good, uid(), &pki, clock));
    assert_eq!(refused, Err(StoreError::Get(GetError::NotEnrolled)));
    Quiet.line("nothing");
}
