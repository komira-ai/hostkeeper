//! The host's id and CA bundle, read from the files of the pki directory's current generation.

use std::path::Path;

use hostkeeper_enroll::{CurrentPaths, HostId, HostTrust, PkiTrust};

use crate::support::{Ca, scratch};

fn certificate_for(ca: &Ca, subject: &str) -> String {
    ca.leaf_pem(subject)
}

fn paths(dir: &Path) -> CurrentPaths {
    CurrentPaths {
        cert: dir.join("cert.pem"),
        ca: dir.join("ca.pem"),
        key: dir.join("key"),
    }
}

/// Catches the host's id being read from anywhere but the subject `host:<id>` of its certificate,
/// and the CA bundle being any file but `ca.pem`.
#[test]
fn the_host_id_is_the_subject_of_its_certificate_and_the_bundle_is_its_ca_file() {
    let dir = scratch("trust-reads");
    let ca = Ca::new(1);
    std::fs::write(
        dir.path().join("cert.pem"),
        certificate_for(&ca, "CN=host:web-1"),
    )
    .expect("write");
    std::fs::write(dir.path().join("ca.pem"), &ca.pem).expect("write");
    let trust = PkiTrust::new(paths(dir.path())).read().expect("the trust");
    assert_eq!(trust.host_id, HostId::new("web-1").expect("a host id"));
    assert_eq!(trust.ca_bundle_pem, ca.pem);
}

/// Catches each way the files fail: absent, not a certificate, a certificate that is not a
/// host's, and a host id that is not one.
#[test]
fn files_that_are_not_a_hosts_are_refused_with_the_reason() {
    let dir = scratch("trust-refuses");
    let paths = paths(dir.path());
    let trust = PkiTrust::new(paths.clone());
    let shown = |name: &str| dir.path().join(name).display().to_string();
    assert_eq!(
        trust.read(),
        Err(format!(
            "cannot read {}: No such file or directory (os error 2)",
            shown("cert.pem")
        ))
    );
    let ca = Ca::new(1);
    std::fs::write(&paths.cert, certificate_for(&ca, "CN=host:web-1")).expect("write");
    assert_eq!(
        trust.read(),
        Err(format!(
            "cannot read {}: No such file or directory (os error 2)",
            shown("ca.pem")
        ))
    );
    std::fs::write(&paths.ca, &ca.pem).expect("write");
    std::fs::write(&paths.cert, "not a certificate").expect("write");
    assert_eq!(
        trust.read(),
        Err(format!("{} is not a PEM certificate", shown("cert.pem")))
    );
    for subject in ["CN=service:js-agent", "CN=host:bad id"] {
        std::fs::write(&paths.cert, certificate_for(&ca, subject)).expect("write");
        assert_eq!(
            trust.read(),
            Err(format!(
                "{} is not the certificate of a host",
                shown("cert.pem")
            )),
            "{subject}"
        );
    }
}
