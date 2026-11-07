//! What `install` refuses before it plans anything: a host that is not x86-64, and a SCEP
//! depot that holds part of a CA or a damaged one.

mod common;
mod install_support;

use common::{SVC, kit, ready_host};
use hostkeeper_mdm::MdmError;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, Mode};
use install_support::{
    CA, CA_KEY, DEPOT, PKEY, X509, ca_argv, ca_is_made_by_init, changes, install, whole_depot,
};

const UNAME: [&str; 2] = ["/usr/bin/uname", "-m"];
const READ_ONLY: [&str; 2] = ["/usr/bin/ip -o -4 addr show", "/usr/bin/ss -Hltn"];

/// Catches a host the pinned tools cannot run on being installed to: an aarch64 host is
/// refused (exit 2) first, before the archives are read (they are not even there) or any
/// other command runs, and nothing on the host changes.
#[test]
fn an_unsupported_architecture_is_refused_before_anything_else() {
    let kit = kit();
    let host = ready_host(&kit);
    for archive in kit.archives.keys() {
        host.remove(&format!("/srv/fetch/{archive}")).unwrap();
    }
    host.reply(&UNAME, 0, "aarch64\n");
    let before = host.paths();
    for mode in [Mode::DryRun, Mode::Apply] {
        host.clear_calls();
        let (result, shown) = install(&host, &kit, mode);
        let err = result.unwrap_err();
        assert_eq!(
            err.to_string(),
            "this host is aarch64 (uname -m): the pinned tools are linux-amd64 builds, for x86_64 only"
        );
        assert!(
            matches!(&err, MdmError::Architecture { machine } if machine == "aarch64"),
            "{err}"
        );
        assert_eq!(err.exit_code(), 2);
        assert_eq!(shown, "");
        assert_eq!(host.calls(), [UNAME.map(str::to_owned).to_vec()]);
        assert_eq!(host.paths(), before);
    }
}

/// Catches an x86-64 host refused for the newline `uname -m` prints: it is accepted and the
/// plan is made.
#[test]
fn an_x86_64_host_is_accepted() {
    let kit = kit();
    let host = ready_host(&kit);
    host.reply(&UNAME, 0, "x86_64\n");
    let (result, shown) = install(&host, &kit, Mode::DryRun);
    result.unwrap();
    assert!(
        shown.starts_with("archives: every one matches its pin\n"),
        "{shown}"
    );
}

/// Catches a `uname` that fails passing as a host of unknown kind: a non-zero exit fails the run
/// (exit 1) with the command named, a program that cannot be started fails it with the host's
/// error (exit 1), and no change is made.
#[test]
fn a_uname_that_fails_or_cannot_run_fails_the_run() {
    let kit = kit();
    let host = ready_host(&kit);
    host.reply(&UNAME, 3, "x86_64\n");
    let (result, _) = install(&host, &kit, Mode::Apply);
    let err = result.unwrap_err();
    assert_eq!(err.to_string(), "/usr/bin/uname -m exited with status 3");
    assert_eq!(err.exit_code(), 1);

    let bare = FakeHost::new(0);
    let (result, _) = install(&bare, &kit, Mode::Apply);
    let err = result.unwrap_err();
    assert!(matches!(&err, MdmError::Io(_)), "{err}");
    assert_eq!(err.exit_code(), 1);
    assert_eq!(bare.calls(), [UNAME.map(str::to_owned).to_vec()]);
    assert!(bare.paths().is_empty());
}

/// Runs a full apply on `host` and expects the refusal `message` (exit 2), with the host
/// untouched: no path made or changed, and no command but the read-only questions run.
fn refused_with(host: &FakeHost, message: &str) {
    let kit = kit();
    let before: Vec<_> = host.paths().iter().map(|p| host.node(p)).collect();
    host.clear_calls();
    let (result, shown) = install(host, &kit, Mode::Apply);
    let err = result.unwrap_err();
    assert_eq!(err.to_string(), message);
    assert_eq!(err.exit_code(), 2);
    assert_eq!(shown, "");
    let after: Vec<_> = host.paths().iter().map(|p| host.node(p)).collect();
    assert_eq!(before, after);
    assert_eq!(changes(host), READ_ONLY);
}

/// Catches a CA made over a depot with nothing in it but half of one: with only `ca.key` the
/// run is refused, naming both files, and nothing changes; `scepserver ca -init` is not run.
#[test]
fn a_depot_with_only_the_key_is_refused() {
    let kit = kit();
    let host = ready_host(&kit);
    host.put_file(CA_KEY, 0o400, (SVC, SVC), b"KEY");
    refused_with(
        &host,
        &format!(
            "{DEPOT} holds ca.key but no ca.pem: a CA is never made over part of one (it would re-enroll every Mac); restore ca.pem, or move the depot aside to make a new CA"
        ),
    );
    assert!(!host.calls().iter().any(|c| c[0] == "/usr/sbin/runuser"));
}

/// Catches the old behaviour of keeping a CA because `ca.pem` is there: with only `ca.pem` the
/// run is refused, and nothing changes.
#[test]
fn a_depot_with_only_the_certificate_is_refused() {
    let kit = kit();
    let host = ready_host(&kit);
    host.put_file(CA, 0o400, (SVC, SVC), b"CA");
    refused_with(
        &host,
        &format!(
            "{DEPOT} holds ca.pem but no ca.key: a CA is never made over part of one (it would re-enroll every Mac); restore ca.key, or move the depot aside to make a new CA"
        ),
    );
}

/// Catches a depot whose files are not regular files being trusted: a directory where
/// `ca.pem` goes, and a link where `ca.key` goes, are each refused, and nothing changes.
#[test]
fn a_depot_file_that_is_a_directory_or_a_link_is_refused() {
    let kit = kit();
    let host = ready_host(&kit);
    whole_depot(&host);
    host.put_dir(CA, 0o700, (SVC, SVC));
    refused_with(
        &host,
        &format!(
            "{DEPOT}: ca.pem is not a regular file: restore the depot from a backup, or move it aside to make a new CA"
        ),
    );

    let host = ready_host(&kit);
    whole_depot(&host);
    host.put_link(CA_KEY);
    refused_with(
        &host,
        &format!(
            "{DEPOT}: ca.key is not a regular file: restore the depot from a backup, or move it aside to make a new CA"
        ),
    );
}

/// Catches a certificate or key that openssl cannot read being kept: each failing alone is
/// refused with the check that failed, and the message holds nothing openssl printed.
#[test]
fn a_depot_openssl_cannot_read_is_refused() {
    let kit = kit();
    let host = ready_host(&kit);
    whole_depot(&host);
    host.reply(&X509, 1, "SECRET OUTPUT OF OPENSSL\n");
    refused_with(
        &host,
        &format!(
            "{DEPOT}: openssl cannot read ca.pem as a certificate: restore the depot from a backup, or move it aside to make a new CA"
        ),
    );

    let host = ready_host(&kit);
    whole_depot(&host);
    host.reply(&PKEY, 1, "SECRET OUTPUT OF OPENSSL\n");
    refused_with(
        &host,
        &format!(
            "{DEPOT}: openssl cannot read ca.key as a private key: restore the depot from a backup, or move it aside to make a new CA"
        ),
    );
}

/// Catches a certificate kept beside a key that is not its own: the public keys differ, the
/// run is refused, and neither key is shown.
#[test]
fn a_certificate_and_a_key_that_are_not_a_pair_are_refused() {
    let kit = kit();
    let host = ready_host(&kit);
    whole_depot(&host);
    host.reply(&PKEY, 0, "PUBLIC KEY OF ANOTHER KEY\n");
    refused_with(
        &host,
        &format!(
            "{DEPOT}: ca.pem and ca.key are not a pair (the certificate's public key is not the key's): restore the depot from a backup, or move it aside to make a new CA"
        ),
    );
}

/// Catches a whole, matching depot being refused or remade: it is kept (`SCEP CA: kept`), the
/// init command is not planned, and neither file is touched; with both files absent the init
/// is planned as before.
#[test]
fn a_whole_depot_is_kept_and_an_empty_one_is_made() {
    let kit = kit();
    let host = ready_host(&kit);
    whole_depot(&host);
    let (result, shown) = install(&host, &kit, Mode::Apply);
    result.unwrap();
    assert!(
        shown.contains(&format!("SCEP CA: kept ({CA})\n")),
        "{shown}"
    );
    assert!(!host.calls().iter().any(|c| c[0] == "/usr/sbin/runuser"));
    assert_eq!(host.node(CA).unwrap().bytes, b"EXISTING CA");
    assert_eq!(host.node(CA_KEY).unwrap().bytes, b"EXISTING KEY");

    let host = ready_host(&kit);
    ca_is_made_by_init(&host);
    let (result, shown) = install(&host, &kit, Mode::DryRun);
    result.unwrap();
    assert!(!shown.contains("SCEP CA: kept"), "{shown}");
    assert!(
        shown.contains(&format!("DRY-RUN: run {}\n", ca_argv().join(" "))),
        "{shown}"
    );
    assert!(host.node(CA).is_none() && host.node(CA_KEY).is_none());
}
