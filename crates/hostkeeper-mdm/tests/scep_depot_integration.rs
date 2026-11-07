//! The SCEP depot check against what the pinned `scepserver ca -init` writes, read by the real
//! `/usr/bin/openssl` (see `real_openssl`): the CA the kit makes is kept by a rerun of
//! `install` and of `migrate`, after a failure at any step of the move, and a key that is not
//! the certificate's, one with another password, or a damaged file is still refused.

mod common;
mod install_support;
mod migrate_support;
mod real_openssl;

use common::{FakeNet, SVC, kit, ready_host};
use hostkeeper_mdm::MdmError;
use hostkeeper_mdm::install::{Install, run};
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, Mode};
use install_support::{CA, CA_KEY, DEPOT};
use migrate_support::{
    ask, host_before_the_move_making, migrate_on, old_text, rerun_converges_after_each_step,
};
use real_openssl::{CA_A, CA_B, CA_OTHER_PASSWORD, Ca, RealOpenssl};

/// `install` of the example settings on `host`, with the real openssl.
fn install(host: &FakeHost, mode: Mode, name: &str) -> (Result<(), MdmError>, String) {
    let kit = kit();
    let real = RealOpenssl::new(host, name);
    let net = FakeNet::new(Default::default());
    let a = Install {
        config: common::config(),
        bin_dir: "/srv/fetch".to_owned(),
        max_archive: 1 << 30,
        replacing: None,
    };
    let mut out = Vec::new();
    let result = common::with_env_on(&real, &net, |env| run(env, &a, &kit.pins, mode, &mut out));
    (result, String::from_utf8(out).unwrap())
}

/// A ready host whose depot holds `key` as `ca.key` and `pem` as `ca.pem`, as the
/// service user leaves them (0400).
fn depot_of(key: &[u8], pem: &[u8]) -> FakeHost {
    let host = ready_host(&kit());
    host.put_file(CA_KEY, 0o400, (SVC, SVC), key);
    host.put_file(CA, 0o400, (SVC, SVC), pem);
    host
}

/// `install` on a depot holding `key` and `pem` is refused with `why`, changing nothing.
fn refused(key: &[u8], pem: &[u8], why: &str, name: &str) {
    let host = depot_of(key, pem);
    let before = migrate_support::snapshot(&host);
    for mode in [Mode::DryRun, Mode::Apply] {
        let (result, shown) = install(&host, mode, name);
        let err = result.unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "{DEPOT}: {why}: restore the depot from a backup, or move it aside to make a new CA"
            )
        );
        assert_eq!(err.exit_code(), 2);
        assert_eq!(shown, "");
        assert_eq!(migrate_support::snapshot(&host), before);
    }
}

/// Catches fixtures that are not the form the pinned scepserver writes: each key is a PKCS#1
/// RSA key in PEM, encrypted (DES-EDE3-CBC, the PEM header scepserver 2.3.0 writes), and each
/// certificate a PEM certificate.
#[test]
fn the_fixtures_are_the_encrypted_pem_scepserver_ca_init_writes() {
    for (name, ca) in [("a", CA_A), ("b", CA_B), ("password", CA_OTHER_PASSWORD)] {
        let key = std::str::from_utf8(ca.key).unwrap();
        assert!(
            key.starts_with(
                "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: DES-EDE3-CBC,"
            ),
            "{name}"
        );
        assert!(key.ends_with("-----END RSA PRIVATE KEY-----\n"), "{name}");
        assert!(
            ca.pem.starts_with(b"-----BEGIN CERTIFICATE-----\n"),
            "{name}"
        );
    }
}

/// Catches the depot check refusing the CA `scepserver ca -init` made (the refusal of the live
/// rerun, "openssl cannot read ca.key as a private key": the key is encrypted with the empty
/// password and openssl was given none): a depot holding that CA is kept by a dry run and by
/// an apply, and the CA is not made again.
#[test]
fn the_ca_scepserver_made_is_kept_by_a_rerun_of_install() {
    let host = depot_of(CA_A.key, CA_A.pem);
    for mode in [Mode::DryRun, Mode::Apply] {
        host.clear_calls();
        let (result, shown) = install(&host, mode, "kept");
        result.unwrap_or_else(|e| panic!("{e}\n{shown}"));
        assert!(
            shown.contains(&format!("SCEP CA: kept ({CA})\n")),
            "{shown}"
        );
        assert!(!host.calls().iter().any(|c| c[0] == "/usr/sbin/runuser"));
    }
    assert_eq!(host.node(CA_KEY).unwrap().bytes, CA_A.key);
    assert_eq!(host.node(CA).unwrap().bytes, CA_A.pem);
}

/// Catches a check that accepts any key openssl reads: one CA's certificate beside another
/// CA's key (both as scepserver writes them) is refused as not a pair.
#[test]
fn a_certificate_beside_another_cas_key_is_refused() {
    refused(
        CA_B.key,
        CA_A.pem,
        "ca.pem and ca.key are not a pair (the certificate's public key is not the key's)",
        "pair",
    );
}

/// Catches a check that skips the key's encryption rather than reading it with the kit's
/// password: a CA scepserver made with `-key-password other` is refused as unreadable, since
/// the SCEP unit (no `-capass`) could not read it either.
#[test]
fn a_key_with_another_password_is_refused() {
    let Ca { key, pem } = CA_OTHER_PASSWORD;
    refused(
        key,
        pem,
        "openssl cannot read ca.key as a private key",
        "password",
    );
}

/// Catches a damaged file being kept: the key cut in half is refused as unreadable, and so is
/// the certificate cut in half, each beside the other file whole.
#[test]
fn a_key_or_a_certificate_cut_short_is_refused() {
    let half = |b: &[u8]| b[..b.len() / 2].to_vec();
    refused(
        &half(CA_A.key),
        CA_A.pem,
        "openssl cannot read ca.key as a private key",
        "cut-key",
    );
    refused(
        CA_A.key,
        &half(CA_A.pem),
        "openssl cannot read ca.pem as a certificate",
        "cut-pem",
    );
}

/// Catches a rerun of the move refusing what an apply that stopped part way made, as the live
/// rerun refused its SCEP CA: with `scepserver ca -init` leaving the CA the pinned scepserver
/// writes and the real openssl checking it, an apply that fails at any step is followed by a
/// dry run that is accepted and an apply that leaves the host as one apply that did not fail.
#[test]
fn a_rerun_of_the_move_after_a_failure_at_any_step_converges_with_the_real_ca() {
    let kit = kit();
    let a = ask(&old_text());
    rerun_converges_after_each_step(
        &|| host_before_the_move_making(&a, CA_A.key, CA_A.pem),
        &|host: &dyn Host, mode| migrate_on(&RealOpenssl::new(host, "move"), &kit, &a, mode),
    );
}
