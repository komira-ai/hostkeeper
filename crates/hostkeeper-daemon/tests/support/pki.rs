//! The test certificate authority, its leaves, the host's files and the pki directory of an
//! enrolled host, for the tests that run the shipped heartbeat link against the real coordinator
//! on loopback.

use std::path::Path;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_daemon::cli::HostFiles;
use hostkeeper_testkit::Scratch;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use sha2::{Digest, Sha256};

/// The node id of the host's certificate.
pub const HOST: &str = "d1";

/// A test certificate authority.
pub struct Ca {
    pub issuer: CertifiedIssuer<'static, KeyPair>,
    pub der: Vec<u8>,
    pub pem: String,
}

/// A certificate of a [`Ca`] and its key: the PEM of both, and the key as PKCS#8 DER, as
/// enrollment writes it into a generation of the pki directory.
pub struct Leaf {
    pub cert_pem: String,
    pub key_pem: String,
    pub key_der: Vec<u8>,
}

impl Ca {
    pub fn new(common_name: &str) -> Ca {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name = named(common_name);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let issuer = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("a CA key"))
            .expect("a CA certificate");
        Ca {
            der: issuer.der().to_vec(),
            pem: issuer.pem(),
            issuer,
        }
    }

    /// Its SHA-256 in hex, as the daemon config pins it.
    pub fn sha256_hex(&self) -> String {
        Sha256::digest(&self.der)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn leaf(
        &self,
        common_name: &str,
        names: Vec<SanType>,
        usage: ExtendedKeyUsagePurpose,
    ) -> Leaf {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.distinguished_name = named(common_name);
        params.subject_alt_names = names;
        params.extended_key_usages = vec![usage];
        // rcgen's default: a P-256 key, the only kind the pki directory holds.
        let key = KeyPair::generate().expect("a key");
        let cert = params.signed_by(&key, &self.issuer).expect("signed");
        Leaf {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            key_der: key.serialize_der(),
        }
    }

    /// The coordinator's certificate: for the loopback address, a server's only.
    pub fn coordinator(&self) -> Leaf {
        self.leaf(
            "coordinator",
            vec![SanType::IpAddress([127, 0, 0, 1].into())],
            ExtendedKeyUsagePurpose::ServerAuth,
        )
    }

    /// The certificate of the updater [`HOST`] on a host of role `daemon`, as the coordinator
    /// reads one by default.
    pub fn host(&self) -> Leaf {
        self.host_in("daemon")
    }

    /// The certificate of the updater [`HOST`] on a host of role `role`.
    pub fn host_in(&self, role: &str) -> Leaf {
        let uri = |text: &str| SanType::URI(text.to_owned().try_into().expect("a URI"));
        self.leaf(
            HOST,
            vec![
                uri("urn:hostkeeper:role:updater"),
                uri(&format!("urn:hostkeeper:host-role:{role}")),
                SanType::DnsName(HOST.to_owned().try_into().expect("a DNS name")),
            ],
            ExtendedKeyUsagePurpose::ClientAuth,
        )
    }
}

pub fn named(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}
/// A daemon config whose coordinator is `coordinator` and whose CA pin is `pin`.
pub fn config(coordinator: &str, pin: &str) -> DaemonConfig {
    let text = format!(
        "coordinator = \"{coordinator}\"\nca_sha256 = \"{pin}\"\n\
         release_channel = \"https://releases.example\"\n\n[trusted_signers]\n\
         organisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
         refs = [\"refs/tags/*\"]\nhosted_runner = true\n"
    );
    DaemonConfig::parse(&text).expect("a daemon config")
}

pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// The host's files under `root`, its pki directory `root/pki`.
pub fn files_under(root: &Path) -> HostFiles {
    HostFiles {
        repositories: None,
        os_root: root.join("os"),
        stores: root.join("stores.toml"),
        macos_pkg_signers: None,
        pki: root.join("pki"),
        console: None,
        managed_config: None,
        managed_stores: None,
        managed_macos_pkg: None,
    }
}

/// Writes a complete generation of the pki directory `pki`, as enrollment leaves it: `leaf`'s
/// certificate and key and `ca`'s bundle in `gen-1`, and `current` naming it.
pub fn enroll(pki: &Path, leaf: &Leaf, ca: &Ca) {
    let generation = pki.join("gen-1");
    std::fs::create_dir_all(&generation).expect("a generation");
    std::fs::write(generation.join("cert.pem"), &leaf.cert_pem).expect("the certificate");
    std::fs::write(generation.join("key"), &leaf.key_der).expect("the key");
    std::fs::write(generation.join("ca.pem"), &ca.pem).expect("the CA bundle");
    std::os::unix::fs::symlink("gen-1", pki.join("current")).expect("current");
}

/// Replaces the pair of the pki directory `pki` with `leaf`'s, as a renewal does: the next
/// generation is written and `current` is switched to it by rename.
pub fn renew(pki: &Path, leaf: &Leaf, ca: &Ca) {
    let generation = pki.join("gen-2");
    std::fs::create_dir_all(&generation).expect("a generation");
    std::fs::write(generation.join("cert.pem"), &leaf.cert_pem).expect("the certificate");
    std::fs::write(generation.join("key"), &leaf.key_der).expect("the key");
    std::fs::write(generation.join("ca.pem"), &ca.pem).expect("the CA bundle");
    std::os::unix::fs::symlink("gen-2", pki.join("current.tmp")).expect("a staged link");
    std::fs::rename(pki.join("current.tmp"), pki.join("current")).expect("current switched");
}
