//! The host's side of the request: the key made from the entropy port, a key on disk that cannot
//! be read, and the Linux claims read from a temporary tree.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use hostkeeper_enroll::{
    Claims, ClaimsSource, Enroller, EntropyError, Error, KEY_DRAWS, KeyError, LinuxClaims, Outcome,
};

use crate::support::{ScriptedEntropy, World};

/// The result of a first step with an enroller whose entropy gives `draws`, and how many draws
/// it asked for.
fn first_step(world: &World, draws: Vec<u8>) -> (Result<Outcome, Error>, usize) {
    let calls = Arc::new(Mutex::new(0));
    let entropy = ScriptedEntropy {
        draws,
        calls: Arc::clone(&calls),
    };
    let ports = world.ports_with(None, Some(Box::new(entropy)));
    let mut enroller = Enroller::open(world.settings(), ports).expect("open");
    let result = enroller.run_once();
    let calls = *calls.lock().expect("calls");
    (result, calls)
}

/// Catches a draw that is not a P-256 scalar being used as a key, or drawing without end.
#[test]
fn a_key_is_drawn_again_until_it_is_a_scalar_and_no_more_than_key_draws_times() {
    // Zero and 0xff..ff are not scalars; 0x07..07 is.
    let world = World::new("draw-again");
    let (result, calls) = first_step(&world, vec![0x00, 0xff, 0x07]);
    assert_eq!(result.expect("a key"), Outcome::Pending);
    assert_eq!(calls, 3);
    assert!(world.pki().join("pending/key").is_file());

    let world = World::new("never-a-scalar");
    let (result, calls) = first_step(&world, vec![0x00; KEY_DRAWS + 1]);
    let refused = result.expect_err("no key");
    assert!(
        matches!(refused, Error::Key(KeyError::NoKey)),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        "the host key: 8 draws of random bytes gave no P-256 key"
    );
    assert_eq!(calls, KEY_DRAWS);
    assert!(!world.pki().join("pending").exists());
    assert!(world.coordinator().enrolls.is_empty());
}

/// Catches a failure of the entropy source being hidden behind a weak key.
#[test]
fn entropy_that_fails_makes_no_key() {
    let world = World::new("no-entropy");
    let (result, calls) = first_step(&world, Vec::new());
    let refused = result.expect_err("no entropy");
    assert!(
        matches!(&refused, Error::Key(KeyError::Entropy(EntropyError(why))) if why == "no more draws"),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        "the host key: the operating system gave no random bytes: no more draws"
    );
    assert_eq!(calls, 1);
    assert!(world.coordinator().enrolls.is_empty());
}

/// Catches a pending key that cannot be parsed being replaced by a new one: the approval of the
/// first request would be lost. The step fails and the file is left for the operator.
#[test]
fn a_pending_key_that_is_not_a_key_is_kept_and_stops_the_step() {
    let world = World::new("bad-pending-key");
    let mut enroller = world.open();
    fs::create_dir_all(world.pki().join("pending")).expect("pending");
    crate::support::write_private(&world.pki().join("pending/key"), b"not a key");
    let refused = enroller.run_once().expect_err("not a key");
    assert!(
        matches!(&refused, Error::Key(KeyError::Unreadable(_))),
        "{refused:?}"
    );
    assert!(
        refused
            .to_string()
            .starts_with("the host key: the key on disk is not a P-256 PKCS#8 key: "),
        "{refused}"
    );
    assert_eq!(
        fs::read(world.pki().join("pending/key")).expect("key"),
        b"not a key"
    );
    assert!(world.coordinator().enrolls.is_empty());
}

/// Writes `text` to `root/rel`, making its directories.
fn put(root: &Path, rel: &str, text: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    fs::write(path, text).expect("write");
}

fn claims_under(root: &Path) -> Claims {
    LinuxClaims::new(
        &root.join("etc/machine-id"),
        &root.join("dmi/product_serial"),
        &root.join("dmi/product_uuid"),
        &root.join("net"),
    )
    .claims()
}

/// Catches claims read untrimmed, a loopback or duplicate address claimed, or addresses in the
/// order the directory lists them (which would change the request between restarts).
#[test]
fn linux_claims_are_trimmed_sorted_and_without_loopback() {
    let world = World::new("claims");
    let root = world.dir.path();
    put(
        root,
        "etc/machine-id",
        b"0123456789abcdef0123456789abcdef\n",
    );
    put(root, "dmi/product_serial", b"  SN-42 \n");
    put(
        root,
        "dmi/product_uuid",
        b"4c4c4544-0000-1000-8000-000000000001\n",
    );
    put(root, "net/eth1/address", b"02:00:00:00:00:02\n");
    put(root, "net/eth0/address", b"02:00:00:00:00:01\n");
    put(root, "net/bond0/address", b"02:00:00:00:00:01\n");
    put(root, "net/lo/address", b"00:00:00:00:00:00\n");
    fs::create_dir_all(root.join("net/tun0")).expect("an interface without an address");
    assert_eq!(
        claims_under(root),
        Claims {
            machine_id: Some("0123456789abcdef0123456789abcdef".into()),
            product_serial: Some("SN-42".into()),
            product_uuid: Some("4c4c4544-0000-1000-8000-000000000001".into()),
            network_addresses: vec!["02:00:00:00:00:01".into(), "02:00:00:00:00:02".into()],
        }
    );
}

/// Catches a claim that cannot be read failing the request: a virtual machine has no DMI data,
/// a daemon that is not root cannot read the serial, a blank or binary file says nothing.
#[test]
fn an_unreadable_claim_is_absent() {
    let world = World::new("no-claims");
    let root = world.dir.path();
    assert_eq!(claims_under(root), Claims::default());
    put(root, "etc/machine-id", b"   \n");
    put(root, "dmi/product_serial", &[0xff, 0xfe, 0x00]);
    fs::create_dir_all(root.join("dmi/product_uuid")).expect("a directory");
    fs::write(root.join("net"), "not a directory").expect("net");
    assert_eq!(claims_under(root), Claims::default());
}

/// Catches the host's claims being read from anywhere but the files design section 5.7 names.
#[test]
fn the_host_claims_are_read_from_the_host_files() {
    let host = format!("{:?}", LinuxClaims::host());
    for path in [
        "/etc/machine-id",
        "/sys/class/dmi/id/product_serial",
        "/sys/class/dmi/id/product_uuid",
        "/sys/class/net",
    ] {
        assert!(host.contains(&format!("{path:?}")), "{host} lacks {path}");
    }
    assert_eq!(LinuxClaims::host(), LinuxClaims::host());
}
