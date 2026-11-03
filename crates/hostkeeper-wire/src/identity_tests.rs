use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, SanType};

use crate::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, Identity, IdentityError, PrefixError,
    SanPrefixes,
};
use crate::names::{HostId, RoleName};

/// The prefixes of the certificates below: the ones kbf's CA made.
fn prefixes() -> SanPrefixes {
    SanPrefixes::new("urn:kbf:role:", "urn:kbf:host-role:").expect("prefixes")
}

fn identity_of(der: &[u8]) -> Result<Identity, IdentityError> {
    crate::identity::identity_of(der, &prefixes())
}

fn id(text: &str) -> HostId {
    text.parse().expect("a host id")
}

fn role(text: &str) -> RoleName {
    text.parse().expect("a role name")
}

fn uri(text: &str) -> SanType {
    SanType::URI(text.try_into().expect("an IA5 name"))
}

fn dns(name: &str) -> SanType {
    SanType::DnsName(name.try_into().expect("an IA5 name"))
}

fn cert(sans: Vec<SanType>) -> Vec<u8> {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = sans;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("key");
    params.self_signed(&key).expect("sign").der().to_vec()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// Catches: an updater certificate read as another host or role than it names: every host
/// role (any well-formed name, not a fixed list) is read from its own URI, the host id from the one DNS name, in any order of the
/// subjectAltNames.
#[test]
fn an_updater_certificate_names_its_host_and_its_host_role() {
    for name in [
        "frontend",
        "storage",
        "daemon",
        "mac-daemon",
        "cache",
        "gpu-builder",
    ] {
        let role = role(name);
        let host_role = format!("urn:kbf:host-role:{role}");
        let sans = vec![dns("host-7"), uri(&host_role), uri("urn:kbf:role:updater")];
        assert_eq!(
            identity_of(&cert(sans)),
            Ok(Identity {
                host_id: id("host-7"),
                role
            })
        );
    }
    let reversed = vec![
        uri("urn:kbf:role:updater"),
        uri("urn:kbf:host-role:storage"),
        dns("s-1"),
    ];
    assert_eq!(
        identity_of(&cert(reversed)).expect("read").role,
        role("storage")
    );
}

/// Catches: a certificate of another role being taken for an updater's: the farm's own
/// roles, the coordinator's, a role in another letter case, a role that is only a DNS name,
/// two roles, and none.
#[test]
fn a_certificate_of_another_role_is_no_updater() {
    let host = |name: &str| vec![dns("host-7"), uri("urn:kbf:host-role:storage"), uri(name)];
    for (role, named) in [
        ("urn:kbf:role:storage", &["storage"][..]),
        ("urn:kbf:role:frontend", &["frontend"]),
        ("urn:kbf:role:daemon", &["daemon"]),
        ("urn:kbf:role:coordinator", &["coordinator"]),
        ("urn:kbf:role:Updater", &["Updater"]),
        ("urn:kbf:role:admin", &["admin"]),
        ("urn:kbf:role:", &[""]),
    ] {
        assert_eq!(
            identity_of(&cert(host(role))),
            Err(IdentityError::NotAnUpdater(strings(named))),
            "{role}"
        );
    }
    let mut two = host("urn:kbf:role:updater");
    two.push(uri("urn:kbf:role:storage"));
    assert_eq!(
        identity_of(&cert(two)),
        Err(IdentityError::NotAnUpdater(strings(&[
            "updater", "storage"
        ])))
    );
    let mut dns_only = vec![dns("updater"), uri("urn:kbf:host-role:storage")];
    dns_only.push(dns("host-7"));
    assert_eq!(
        identity_of(&cert(dns_only)),
        Err(IdentityError::NotAnUpdater(vec![]))
    );
    let wrong_scheme = vec![dns("host-7"), uri("urn:other:role:updater")];
    assert_eq!(
        identity_of(&cert(wrong_scheme)),
        Err(IdentityError::NotAnUpdater(vec![]))
    );
}

/// Catches: an updater certificate with a host role that is missing, repeated, malformed or in
/// another letter case being given a role to be limited and ordered by.
#[test]
fn an_updater_must_name_exactly_one_well_formed_host_role() {
    let with = |host_roles: &[&str]| {
        let mut sans = vec![dns("host-7"), uri("urn:kbf:role:updater")];
        sans.extend(
            host_roles
                .iter()
                .map(|r| uri(&format!("urn:kbf:host-role:{r}"))),
        );
        cert(sans)
    };
    for names in [
        &[][..],
        &["storage", "daemon"],
        &["storage", "storage"],
        &["Storage"],
        &["mac_daemon"],
        &[""],
    ] {
        assert_eq!(
            identity_of(&with(names)),
            Err(IdentityError::HostRoleName(strings(names))),
            "{names:?}"
        );
    }
}

/// Catches: a certificate with no host id, two, one that is not a host id (upper case,
/// underscore, an address) being given an identity.
#[test]
fn an_updater_must_name_exactly_one_host_id() {
    let with = |sans: Vec<SanType>| {
        let mut all = vec![uri("urn:kbf:role:updater"), uri("urn:kbf:host-role:daemon")];
        all.extend(sans);
        cert(all)
    };
    let ip = SanType::IpAddress("203.0.113.5".parse().expect("an address"));
    for (sans, named) in [
        (vec![], &[][..]),
        (vec![ip], &[]),
        (vec![dns("a"), dns("b")], &["a", "b"]),
        (vec![dns("HOST")], &["HOST"]),
        (vec![dns("host_1")], &["host_1"]),
        (vec![dns("-host")], &["-host"]),
        (vec![dns("a.b")], &["a.b"]),
    ] {
        assert_eq!(
            identity_of(&with(sans)),
            Err(IdentityError::HostId(strings(named))),
            "{named:?}"
        );
    }
}

/// Catches: bytes that are no certificate being read as one, and error texts that do not say
/// what was wrong.
#[test]
fn bytes_that_are_no_certificate_name_no_one() {
    let why = identity_of(b"not DER").expect_err("refused");
    assert!(matches!(why, IdentityError::NotACertificate(_)));
    assert!(
        why.to_string()
            .starts_with("the client certificate does not parse")
    );
    assert_eq!(
        IdentityError::NotAnUpdater(strings(&["storage"])).to_string(),
        r#"the client certificate's role is not `updater` (it names ["storage"])"#
    );
    assert!(
        IdentityError::HostRoleName(vec![])
            .to_string()
            .contains("host role")
    );
    assert!(
        IdentityError::HostId(vec![])
            .to_string()
            .contains("host id")
    );
}

fn kbf_cert() -> Vec<u8> {
    cert(vec![
        uri("urn:kbf:role:updater"),
        uri("urn:kbf:host-role:storage"),
        dns("s1"),
    ])
}

fn hostkeeper_cert() -> Vec<u8> {
    cert(vec![
        uri("urn:hostkeeper:role:updater"),
        uri("urn:hostkeeper:host-role:storage"),
        dns("s1"),
    ])
}

/// Catches: the prefixes ignored, or one pair hard-wired: a certificate under the kbf prefixes
/// is an identity under those prefixes and no updater under the defaults, and a certificate
/// under the default prefixes is the reverse.
#[test]
fn the_prefixes_decide_which_certificates_are_read() {
    let want = Identity {
        host_id: id("s1"),
        role: role("storage"),
    };
    let kbf = prefixes();
    let default = SanPrefixes::default();
    assert_eq!(
        crate::identity::identity_of(&kbf_cert(), &kbf),
        Ok(want.clone())
    );
    assert_eq!(
        crate::identity::identity_of(&kbf_cert(), &default),
        Err(IdentityError::NotAnUpdater(vec![]))
    );
    assert_eq!(
        crate::identity::identity_of(&hostkeeper_cert(), &default),
        Ok(want)
    );
    assert_eq!(
        crate::identity::identity_of(&hostkeeper_cert(), &kbf),
        Err(IdentityError::NotAnUpdater(vec![]))
    );
}

/// Catches: a host role read under the role prefix, or the other way round: the two namespaces
/// are told apart even when a certificate carries both pairs.
#[test]
fn each_prefix_reads_only_its_own_uris() {
    let both = cert(vec![
        uri("urn:kbf:role:updater"),
        uri("urn:kbf:host-role:storage"),
        uri("urn:hostkeeper:role:other"),
        uri("urn:hostkeeper:host-role:daemon"),
        dns("s1"),
    ]);
    let got = crate::identity::identity_of(&both, &prefixes()).expect("kbf pair");
    assert_eq!(got.role, role("storage"));
    assert_eq!(
        crate::identity::identity_of(&both, &SanPrefixes::default()),
        Err(IdentityError::NotAnUpdater(strings(&["other"])))
    );
}

/// Catches: the default prefixes spelled other than the namespace the CA documents, and
/// accessors that swap the two.
#[test]
fn the_default_prefixes_are_the_hostkeeper_namespace() {
    let default = SanPrefixes::default();
    assert_eq!(default.role(), "urn:hostkeeper:role:");
    assert_eq!(default.host_role(), "urn:hostkeeper:host-role:");
    assert_eq!(default.role(), DEFAULT_ROLE_URI_PREFIX);
    assert_eq!(default.host_role(), DEFAULT_HOST_ROLE_URI_PREFIX);
    assert_eq!(
        SanPrefixes::new(DEFAULT_ROLE_URI_PREFIX, DEFAULT_HOST_ROLE_URI_PREFIX),
        Ok(default)
    );
}

/// Catches: a pair of prefixes accepted that would make a URI of both namespaces, or a prefix
/// that cannot appear in a URI, and a refusal that does not say which rule broke.
#[test]
fn prefixes_are_refused_when_empty_invisible_or_overlapping() {
    for (role, host_role) in [("", "urn:b:"), ("urn:a:", "")] {
        assert_eq!(SanPrefixes::new(role, host_role), Err(PrefixError::Empty));
    }
    for (role, host_role, bad) in [
        ("urn a:", "urn:b:", "urn a:"),
        ("urn:a:", "urn:b\n:", "urn:b\n:"),
        ("urn:\u{e9}:", "urn:b:", "urn:\u{e9}:"),
        ("urn:a:", "\t", "\t"),
    ] {
        assert_eq!(
            SanPrefixes::new(role, host_role),
            Err(PrefixError::NotVisible(bad.to_owned()))
        );
    }
    for (role, host_role) in [
        ("urn:a:", "urn:a:"),
        ("urn:a:", "urn:a:b:"),
        ("urn:a:b:", "urn:a:"),
    ] {
        assert_eq!(
            SanPrefixes::new(role, host_role),
            Err(PrefixError::Overlap),
            "{role} {host_role}"
        );
    }
    assert!(SanPrefixes::new("urn:a:", "urn:b:").is_ok());
    assert_eq!(PrefixError::Empty.to_string(), "a URI prefix is empty");
    assert_eq!(
        PrefixError::NotVisible("a b".to_owned()).to_string(),
        "a URI prefix must be visible ASCII without spaces: \"a b\""
    );
    assert_eq!(
        PrefixError::Overlap.to_string(),
        "the role prefix and the host-role prefix must differ, and neither may start the other"
    );
}

/// Catches: the roles being a closed list again: a host role no list names, and one the old
/// reader refused, is an identity, while a malformed one is still refused.
#[test]
fn a_host_role_is_any_well_formed_name() {
    let with = |name: &str| {
        cert(vec![
            uri("urn:kbf:role:updater"),
            uri(&format!("urn:kbf:host-role:{name}")),
            dns("h1"),
        ])
    };
    for name in ["cache", "gpu-builder", "gateway"] {
        assert_eq!(identity_of(&with(name)).expect("read").role, role(name));
    }
    for name in ["Frontend", "a--b", "a-", "1abc", &"a".repeat(33)] {
        assert_eq!(
            identity_of(&with(name)),
            Err(IdentityError::HostRoleName(strings(&[name]))),
            "{name}"
        );
    }
}
