//! The names of the coordinator's server certificate: the DNS names of `--server-name` and the
//! IP address of `--lan-listen`. A certificate that names other names than these is made again,
//! at the start and at a look of the reload tick, whatever is left of its validity; one that
//! names the same names, in whatever order, is kept. Time is the test's clock; no look waits.

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_testkit::Scratch;
use rcgen::{CertificateParams, KeyPair, SanType};
use x509_parser::extensions::GeneralName;

use super::support::{Files, FixedClock, NOW, SERVER_NAME};
use crate::enrollment::{Enrollment, os_random};
use crate::testutil::scratch;

/// The subjectAltNames of the certificate the server certificate file at `path` starts with,
/// in the certificate's order, as `dns:<name>` and `ip:<address>`.
fn sans_of(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).expect("the server certificate file");
    let (_, block) = x509_parser::pem::parse_x509_pem(&bytes).expect("PEM");
    let certificate = block.parse_x509().expect("a certificate");
    let san = certificate
        .subject_alternative_name()
        .expect("one extension")
        .expect("present");
    san.value
        .general_names
        .iter()
        .map(|name| match name {
            GeneralName::DNSName(dns) => format!("dns:{dns}"),
            GeneralName::IPAddress(bytes) => match <[u8; 4]>::try_from(*bytes) {
                Ok(v4) => format!("ip:{}", IpAddr::from(v4)),
                Err(_) => {
                    let v6: [u8; 16] = (*bytes).try_into().expect("16 bytes");
                    format!("ip:{}", IpAddr::from(v6))
                }
            },
            other => panic!("a name the coordinator never asks for: {other:?}"),
        })
        .collect()
}

/// The bytes of the server certificate file of `files`.
fn file_of(files: &Files) -> Vec<u8> {
    std::fs::read(&files.server_cert).expect("the server certificate file")
}

/// The files of `dir` made for the DNS names `names` and the addresses `ips`.
fn files_for(dir: &Scratch, names: &[&str], ips: &[&str]) -> Files {
    let mut files = Files::in_dir(dir);
    files.names = names.iter().map(|name| (*name).to_owned()).collect();
    files.ips = ips
        .iter()
        .map(|ip| ip.parse().expect("an address"))
        .collect();
    files
}

/// Enrollment started over `files` at [`NOW`].
fn start(files: &Files) -> Enrollment {
    files
        .start(Arc::new(RealFs), FixedClock::at(NOW), os_random())
        .expect("enrollment starts")
}

/// Proves: a start over a server certificate made for the same names, in whatever order, keeps
/// the file byte for byte; a start whose flags add the LAN listener's address, change it, drop
/// it, or change a DNS name makes the file again, and the new certificate names exactly the DNS
/// names and then the address the flags give.
///
/// Catches: a changed name list that takes effect only when someone deletes the file (the old
/// certificate, without the LAN address, would be served to a host that dials the address and
/// refuses it); an address named as a DNS name; and a certificate made again at every start
/// because its names were compared in order.
#[test]
fn a_start_makes_the_certificate_again_when_its_names_differ_from_the_flags() {
    let dir = scratch("server-names-start");
    let first = files_for(&dir, &["a.example", "b.example"], &[]);
    drop(start(&first));
    assert_eq!(
        sans_of(&first.server_cert),
        ["dns:a.example", "dns:b.example"]
    );
    let made = file_of(&first);

    drop(start(&files_for(&dir, &["b.example", "a.example"], &[])));
    assert_eq!(file_of(&first), made, "the same names in another order");

    let mut previous = made;
    for (names, ips, want) in [
        (
            &["a.example", "b.example"][..],
            &["192.0.2.7"][..],
            &["dns:a.example", "dns:b.example", "ip:192.0.2.7"][..],
        ),
        (
            &["a.example", "b.example"],
            &["2001:db8::7"],
            &["dns:a.example", "dns:b.example", "ip:2001:db8::7"],
        ),
        (
            &["a.example", "b.example"],
            &[],
            &["dns:a.example", "dns:b.example"],
        ),
        (
            &["a.example", "c.example"],
            &[],
            &["dns:a.example", "dns:c.example"],
        ),
    ] {
        let files = files_for(&dir, names, ips);
        drop(start(&files));
        assert_eq!(sans_of(&files.server_cert), want, "{names:?} {ips:?}");
        let now = file_of(&files);
        assert_ne!(now, previous, "made again for {names:?} {ips:?}");
        previous = now;
        drop(start(&files));
        assert_eq!(file_of(&files), previous, "kept for {names:?} {ips:?}");
    }
}

/// A server certificate file whose certificate is valid for the whole of the tests' time,
/// self-signed, naming `sans` (none: no subjectAltName extension at all).
fn foreign_file(sans: Vec<SanType>) -> String {
    let mut params = CertificateParams::default();
    params.subject_alt_names = sans;
    let key = KeyPair::generate().expect("a key");
    let certificate = params.self_signed(&key).expect("a certificate");
    format!("{}{}", certificate.pem(), key.serialize_pem())
}

/// Proves: one look of the reload tick makes the file again when the certificate in it names
/// other names than the flags, though its validity is far from due: one made for the DNS names
/// alone (the LAN address left out), one that names a URI besides the right names, and one with
/// no subjectAltName at all. After each, the certificate names exactly the flags' names, and
/// the next look keeps it.
///
/// Catches: a tick that looks at the validity alone, so that a file put back from before the
/// LAN listener (a restore, a copy) is served until it is due, two months on.
#[test]
fn the_tick_makes_again_a_file_whose_names_differ_from_the_flags() {
    let dir = scratch("server-names-tick");
    let files = files_for(&dir, &[SERVER_NAME], &["192.0.2.7"]);
    let enrollment = start(&files);
    let want = [format!("dns:{SERVER_NAME}"), "ip:192.0.2.7".to_owned()];
    assert_eq!(sans_of(&files.server_cert), want);

    let old = scratch("server-names-tick-old");
    let without_lan = files_for(&old, &[SERVER_NAME], &[]);
    drop(start(&without_lan));
    let dns = SanType::DnsName(SERVER_NAME.try_into().expect("a DNS name"));
    let ip = SanType::IpAddress("192.0.2.7".parse().expect("an address"));
    let uri = SanType::URI("spiffe://example/coordinator".try_into().expect("a URI"));
    for (what, text) in [
        (
            "DNS names alone",
            String::from_utf8(file_of(&without_lan)).expect("PEM"),
        ),
        ("a URI besides", foreign_file(vec![dns, ip, uri])),
        ("no names", foreign_file(Vec::new())),
    ] {
        std::fs::write(&files.server_cert, &text).expect("put in place");
        enrollment.renew_server();
        assert_eq!(
            enrollment.server_certificate().renewal_error,
            None,
            "{what}"
        );
        assert_eq!(sans_of(&files.server_cert), want, "{what}");
        let made = file_of(&files);
        enrollment.renew_server();
        assert_eq!(file_of(&files), made, "{what}: kept at the next look");
    }
}
