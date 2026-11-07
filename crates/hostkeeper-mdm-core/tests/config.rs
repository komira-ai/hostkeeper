//! The settings: the documented example, the defaults and every refusal.

mod common;

use std::net::Ipv4Addr;

use common::{config, example, example_keys, ip, lan, minimal, with, without};
use hostkeeper_mdm_core::config::{
    Cidr, CidrError, ConfigError, DEFAULT_SKIP_ITEMS, KEYS, MdmConfig, lan_address,
};
use hostkeeper_mdm_core::settings::SettingsError;

fn refusal(text: &str) -> String {
    MdmConfig::parse(text).expect_err("refused").to_string()
}

/// Catches an example that leaves a key undocumented (the operator never learns it
/// exists) or documents one the parser refuses, a key set in a different order than the
/// closed list, and settings read into the wrong field.
#[test]
fn the_example_documents_every_key_and_parses_into_its_fields() {
    assert_eq!(example_keys(), KEYS);
    let c = config();
    assert_eq!(c.dns_name(), "mdm.example.com");
    assert_eq!(
        (c.bind(), c.lan().addr(), c.lan().bits()),
        (Ipv4Addr::new(10, 0, 0, 10), Ipv4Addr::new(10, 0, 0, 0), 24)
    );
    assert_eq!(c.lan().to_string(), lan());
    assert_eq!(c.user(), "hostkeeper-mdm");
    let paths = (c.state().as_str(), c.etc().as_str(), c.bin().as_str());
    assert_eq!(
        paths,
        (
            "/var/lib/hostkeeper-mdm",
            "/etc/hostkeeper-mdm",
            "/opt/hostkeeper-mdm/bin"
        )
    );
    assert_eq!(
        (c.nanohub_port(), c.scep_port(), c.dep_port()),
        (9004, 9005, 9001)
    );
    let caps = (c.memory_max(), c.cpu_quota(), c.tasks_max());
    assert_eq!(caps, ("1G", "100%", "256"));
    assert_eq!(
        (c.unit_prefix(), c.slice()),
        ("hostkeeper-mdm", "hostkeeper-mdm.slice")
    );
    assert_eq!(
        (c.dep_name(), c.org(), c.profile_id()),
        ("example", "Example", "com.example.hostkeeper.mdm")
    );
    assert_eq!(c.profile_display_name(), "Example MDM");
    assert_eq!(c.ade().profile_name(), "Example MDM");
    assert_eq!(c.ade().org_magic(), "hostkeeper-mdm");
    assert_eq!((c.ade().language(), c.ade().region()), ("en", "US"));
    assert_eq!(c.ade().skip_items(), DEFAULT_SKIP_ITEMS);
    assert_eq!(c.scep_ca().common_name(), "Example MDM SCEP CA");
    assert_eq!(c.scep_ca().organizational_unit(), "hostkeeper-mdm");
    assert_eq!(c.scep_ca().years(), 10);
    assert_eq!(c.dep_cert().common_name(), "hostkeeper-mdm");
    assert_eq!(c.dep_cert().days(), 7);
    assert_eq!(c.signature_namespace(), "hostkeeper-mdm-erase");
}

/// Catches a default that drifted from what `config.example` says it is: a file with
/// only the keys that have no default must read as the fully written example.
#[test]
fn the_defaults_are_the_values_the_example_documents() {
    let defaulted = MdmConfig::parse(&minimal()).expect("the required keys are enough");
    assert_eq!(defaulted, config());
}

/// Catches a default that ignores the unit prefix or the organisation (a site that sets
/// only those would get another site's names), and a key that is read but not applied.
#[test]
fn the_derived_defaults_follow_the_prefix_and_the_organisation() {
    let text = format!(
        "{}MDM_UNIT_PREFIX=acme-mdm\nMDM_ORG=Acme Labs\n",
        minimal().replace("MDM_ORG=Example\n", "")
    );
    let c = MdmConfig::parse(&text).expect("valid");
    assert_eq!((c.unit_prefix(), c.slice()), ("acme-mdm", "acme-mdm.slice"));
    assert_eq!(c.profile_display_name(), "Acme Labs MDM");
    assert_eq!(c.ade().profile_name(), "Acme Labs MDM");
    assert_eq!(c.ade().org_magic(), "acme-mdm");
    assert_eq!(c.scep_ca().common_name(), "Acme Labs MDM SCEP CA");
    assert_eq!(c.scep_ca().organizational_unit(), "acme-mdm");
    assert_eq!(c.dep_cert().common_name(), "acme-mdm");
    assert_eq!(c.signature_namespace(), "acme-mdm-erase");
}

/// Catches a new key that is accepted and then ignored: each is set to a value that is
/// not its default and must arrive in its field.
#[test]
fn every_new_key_can_be_set_away_from_its_default() {
    let mut text = minimal();
    for line in [
        "MDM_UNIT_PREFIX=site-a",
        "MDM_SLICE=site-a-b.slice",
        "MDM_PROFILE_DISPLAY_NAME=Site A Macs",
        "MDM_ADE_PROFILE_NAME=Site A enrollment",
        "MDM_ADE_ORG_MAGIC=site.a_1",
        "MDM_ADE_SKIP_ITEMS=\"Siri  TOS\"",
        "MDM_LANGUAGE=de",
        "MDM_REGION=DE",
        "MDM_SCEP_CA_CN=Site A CA",
        "MDM_SCEP_CA_OU=Site A OU",
        "MDM_SCEP_CA_YEARS=30",
        "MDM_DEP_CERT_CN=site-a.cert",
        "MDM_DEP_CERT_DAYS=365",
        "MDM_SIGNATURE_NAMESPACE=site.a-erase",
    ] {
        text.push_str(line);
        text.push('\n');
    }
    let c = MdmConfig::parse(&text).expect("valid");
    assert_eq!((c.unit_prefix(), c.slice()), ("site-a", "site-a-b.slice"));
    assert_eq!(c.profile_display_name(), "Site A Macs");
    assert_eq!(c.ade().profile_name(), "Site A enrollment");
    assert_eq!(c.ade().org_magic(), "site.a_1");
    assert_eq!(c.ade().skip_items(), ["Siri", "TOS"]);
    assert_eq!((c.ade().language(), c.ade().region()), ("de", "DE"));
    assert_eq!(c.scep_ca().common_name(), "Site A CA");
    assert_eq!(c.scep_ca().organizational_unit(), "Site A OU");
    assert_eq!(c.scep_ca().years(), 30);
    assert_eq!(c.dep_cert().common_name(), "site-a.cert");
    assert_eq!(c.dep_cert().days(), 365);
    assert_eq!(c.signature_namespace(), "site.a-erase");
}

/// Each row is one planted bad setting; the parser must refuse it naming the key,
/// before any verb reads the host.
#[test]
fn a_bad_setting_is_refused_naming_its_key() {
    let long = "a".repeat(41);
    let long_user = "a".repeat(32);
    // Addresses are built at run time: a public file carries no private or public one.
    let public = ip(8, 8, 4, 4);
    let other_lan = ip(10, 2, 0, 1);
    let ten = |rest: &str| format!("{}{rest}", ip(10, 0, 0, 0));
    let (ten_7, ten_31, ten_99, ten_x) = (ten("/7"), ten("/31"), ten("/99"), ten("/x"));
    let ten_bare = ten("");
    let public_net = format!("{}/24", ip(8, 8, 4, 0));
    let wide_192 = format!("{}/8", ip(192, 168, 0, 0));
    let wide_172 = format!("{}/8", ip(172, 16, 0, 0));
    let wide_172_11 = format!("{}/11", ip(172, 16, 0, 0));
    let cases = [
        ("MDM_DNS_NAME", "MDM.example.com"),
        ("MDM_DNS_NAME", "localhost"),
        ("MDM_DNS_NAME", "-a.example.com"),
        ("MDM_DNS_NAME", "a-.example.com"),
        ("MDM_DNS_NAME", "a..com"),
        ("MDM_DNS_NAME", "a_b.example.com"),
        ("MDM_BIND", "0.0.0.0"),
        ("MDM_BIND", "127.0.0.1"),
        ("MDM_BIND", public.as_str()),
        ("MDM_BIND", "not-an-address"),
        ("MDM_BIND", other_lan.as_str()),
        ("MDM_LAN_CIDR", ten_7.as_str()),
        ("MDM_LAN_CIDR", ten_31.as_str()),
        ("MDM_LAN_CIDR", ten_99.as_str()),
        ("MDM_LAN_CIDR", ten_x.as_str()),
        ("MDM_LAN_CIDR", ten_bare.as_str()),
        ("MDM_LAN_CIDR", public_net.as_str()),
        ("MDM_USER", "Js"),
        ("MDM_USER", "1js"),
        ("MDM_USER", "js mdm"),
        ("MDM_USER", long_user.as_str()),
        ("MDM_STATE", "/var/lib/js mdm"),
        ("MDM_ETC", "etc/hostkeeper-mdm"),
        ("MDM_BIN", "/opt/../bin"),
        ("MDM_NANOHUB_PORT", "0"),
        ("MDM_SCEP_PORT", "65536"),
        ("MDM_DEP_PORT", "x"),
        ("MDM_MEMORY_MAX", "1T"),
        ("MDM_MEMORY_MAX", "G"),
        ("MDM_MEMORY_MAX", "01G"),
        ("MDM_CPU_QUOTA", "100"),
        ("MDM_CPU_QUOTA", "1x%"),
        ("MDM_TASKS_MAX", "0"),
        ("MDM_TASKS_MAX", "25%"),
        ("MDM_UNIT_PREFIX", "Js-mdm"),
        ("MDM_UNIT_PREFIX", "1js"),
        ("MDM_UNIT_PREFIX", "js_mdm"),
        ("MDM_UNIT_PREFIX", "js-"),
        ("MDM_UNIT_PREFIX", "js--mdm"),
        ("MDM_SLICE", "a--b.slice"),
        ("MDM_SLICE", "system.slice"),
        ("MDM_SLICE", "user.slice"),
        ("MDM_SLICE", "machine.slice"),
        ("MDM_SLICE", "init.slice"),
        ("MDM_SLICE", "system-js.slice"),
        ("MDM_SLICE", "-.slice"),
        ("MDM_SLICE", "other.slice"),
        ("MDM_SLICE", "hostkeeper-mdmx.slice"),
        ("MDM_USER", "root"),
        ("MDM_USER", "toor"),
        ("MDM_LAN_CIDR", wide_192.as_str()),
        ("MDM_LAN_CIDR", wide_172.as_str()),
        ("MDM_LAN_CIDR", wide_172_11.as_str()),
        ("MDM_LAN_CIDR", ten_7.as_str()),
        ("MDM_DEP_NAME", "-h"),
        ("MDM_DEP_NAME", "-"),
        ("MDM_DEP_NAME", "."),
        ("MDM_DEP_NAME", ".."),
        ("MDM_DEP_CERT_CN", "-x"),
        ("MDM_DEP_CERT_CN", ".x"),
        ("MDM_ADE_ORG_MAGIC", "-x"),
        ("MDM_ADE_ORG_MAGIC", ".x"),
        ("MDM_SIGNATURE_NAMESPACE", "-x"),
        ("MDM_SIGNATURE_NAMESPACE", ".x"),
        ("MDM_SLICE", "a-.slice"),
        ("MDM_UNIT_PREFIX", long.as_str()),
        ("MDM_SLICE", "hostkeeper-mdm"),
        ("MDM_SLICE", ".slice"),
        ("MDM_SLICE", "-js.slice"),
        ("MDM_SLICE", "Js.slice"),
        ("MDM_SLICE", "js mdm.slice"),
        ("MDM_DEP_NAME", "a b"),
        ("MDM_DEP_NAME", "a%n"),
        ("MDM_ORG", "a\u{7}b"),
        ("MDM_ORG", ""),
        ("MDM_PROFILE_ID", "Com.Example"),
        ("MDM_PROFILE_ID", "hostkeeper-mdm"),
        ("MDM_PROFILE_DISPLAY_NAME", "a\u{7}b"),
        ("MDM_PROFILE_DISPLAY_NAME", ""),
        ("MDM_ADE_PROFILE_NAME", "a\u{7}b"),
        ("MDM_ADE_ORG_MAGIC", "a b"),
        ("MDM_ADE_ORG_MAGIC", "a/b"),
        ("MDM_ADE_SKIP_ITEMS", "   "),
        ("MDM_ADE_SKIP_ITEMS", "Siri Sk-ip"),
        ("MDM_LANGUAGE", "EN"),
        ("MDM_LANGUAGE", "e"),
        ("MDM_LANGUAGE", "eng"),
        ("MDM_LANGUAGE", "e1"),
        ("MDM_REGION", "us"),
        ("MDM_REGION", "U"),
        ("MDM_REGION", "USA"),
        ("MDM_SCEP_CA_CN", "a\u{7}b"),
        ("MDM_SCEP_CA_OU", "a\u{7}b"),
        ("MDM_SCEP_CA_YEARS", "0"),
        ("MDM_SCEP_CA_YEARS", "31"),
        ("MDM_SCEP_CA_YEARS", "x"),
        ("MDM_SCEP_CA_YEARS", "-1"),
        ("MDM_DEP_CERT_CN", "a&b"),
        ("MDM_DEP_CERT_CN", "a b"),
        ("MDM_DEP_CERT_DAYS", "0"),
        ("MDM_DEP_CERT_DAYS", "366"),
        ("MDM_SIGNATURE_NAMESPACE", "a b"),
    ];
    for (key, value) in cases {
        let got = refusal(&with(key, value));
        assert!(got.starts_with(key), "{key}={value:?}: {got:?}");
        let short = key.strip_prefix("MDM_").expect("prefix");
        let line = format!("MDM_{short}={value}");
        assert!(
            got.starts_with(&line) || value.is_empty(),
            "{line}: {got:?}"
        );
    }
}

/// Catches a refusal that names the wrong key or says nothing of the cause: a few
/// complete messages, one for each way a value can be wrong.
#[test]
fn refusals_say_which_key_and_why() {
    let other_lan = ip(10, 2, 0, 1);
    assert_eq!(
        refusal(&with("MDM_BIND", &other_lan)),
        format!(
            "MDM_BIND={other_lan}: not inside MDM_LAN_CIDR (a container bridge's or another network's address?)"
        )
    );
    assert_eq!(
        refusal(&with("MDM_SCEP_CA_YEARS", "31")),
        "MDM_SCEP_CA_YEARS=31: not a whole number in the allowed range"
    );
    assert_eq!(
        refusal(&with("MDM_SLICE", "hostkeeper-mdm")),
        "MDM_SLICE=hostkeeper-mdm: not a slice name: lower-case letters, digits and - from a letter, then .slice"
    );
    assert_eq!(
        refusal(&without("MDM_DNS_NAME")),
        "MDM_DNS_NAME is not set (settings file)"
    );
}

/// Catches a required key that may be left out or empty, and a defaulted key that an
/// empty value would turn into the default.
#[test]
fn a_missing_required_key_or_an_empty_value_is_refused() {
    for key in [
        "MDM_DNS_NAME",
        "MDM_BIND",
        "MDM_LAN_CIDR",
        "MDM_USER",
        "MDM_STATE",
        "MDM_ETC",
        "MDM_BIN",
        "MDM_NANOHUB_PORT",
        "MDM_SCEP_PORT",
        "MDM_DEP_PORT",
        "MDM_MEMORY_MAX",
        "MDM_CPU_QUOTA",
        "MDM_TASKS_MAX",
        "MDM_DEP_NAME",
        "MDM_ORG",
        "MDM_PROFILE_ID",
    ] {
        assert_eq!(
            refusal(&without(key)),
            format!("{key} is not set (settings file)")
        );
        assert_eq!(
            refusal(&with(key, "")),
            format!("{key} is not set (settings file)")
        );
    }
    for key in [
        "MDM_UNIT_PREFIX",
        "MDM_SLICE",
        "MDM_ADE_ORG_MAGIC",
        "MDM_SCEP_CA_YEARS",
        "MDM_LANGUAGE",
    ] {
        assert_eq!(
            refusal(&with(key, "")),
            format!("{key} is not set (settings file)")
        );
    }
}

/// Catches a file with a typo'd key or a repeated key being read: the settings are a
/// closed set, so the error is the settings layer's.
#[test]
fn unknown_and_repeated_keys_are_refused() {
    let unknown = format!("{}MDM_ORGS=x\n", example());
    assert!(matches!(
        MdmConfig::parse(&unknown),
        Err(ConfigError::Settings(SettingsError::Unknown { .. }))
    ));
    let repeated = format!("{}MDM_ORG=x\n", example());
    assert!(matches!(
        MdmConfig::parse(&repeated),
        Err(ConfigError::Settings(SettingsError::Repeated { .. }))
    ));
}

/// Catches a network check that compares the wrong bits (an address just outside a /24
/// must be outside, the network's own edges inside) and a private-range check that
/// takes a neighbouring range.
#[test]
fn an_address_is_inside_a_network_by_its_prefix_bits() {
    let net = Cidr::new(Ipv4Addr::new(10, 1, 0, 0), 24).expect("a private /24");
    assert!(net.contains(Ipv4Addr::new(10, 1, 0, 255)));
    assert!(net.contains(Ipv4Addr::new(10, 1, 0, 1)));
    assert!(!net.contains(Ipv4Addr::new(10, 1, 1, 0)));
    let wide = Cidr::new(Ipv4Addr::new(10, 0, 0, 0), 8).expect("the whole 10/8");
    assert!(wide.contains(Ipv4Addr::new(10, 255, 0, 1)));
    for (a, private) in [
        (Ipv4Addr::new(172, 16, 0, 1), true),
        (Ipv4Addr::new(172, 31, 255, 1), true),
        (Ipv4Addr::new(172, 32, 0, 1), false),
        (Ipv4Addr::new(192, 168, 0, 1), true),
        (Ipv4Addr::new(100, 64, 0, 1), false),
    ] {
        assert_eq!(lan_address(&a.to_string()).is_some(), private, "{a}");
    }
}

/// Catches a name check that refuses what systemd and DNS take: a hyphen or a digit
/// inside a DNS label, a user name with `_`, `-` and digits, a unit prefix with digits.
#[test]
fn names_with_hyphens_digits_and_underscores_are_taken() {
    let text = format!(
        "{}MDM_UNIT_PREFIX=hostkeeper-mdm-2\n",
        with("MDM_DNS_NAME", "mdm-1.site2.example.com")
            .replace("MDM_USER=hostkeeper-mdm", "MDM_USER=_js_mdm-2")
            .replace("MDM_UNIT_PREFIX=hostkeeper-mdm\n", "")
            .replace("MDM_SLICE=hostkeeper-mdm.slice\n", "")
    );
    let c = MdmConfig::parse(&text).expect("valid");
    assert_eq!(
        (c.dns_name(), c.user(), c.slice()),
        (
            "mdm-1.site2.example.com",
            "_js_mdm-2",
            "hostkeeper-mdm-2.slice"
        )
    );
}

/// Catches directories that overlap: the state is the service user's to write, so a
/// binary directory inside it (or equal to it, or around it) would let the user replace
/// pinned binaries, and the same for the unit inputs. Each row sets one path; the
/// refusal names the key that clashes. A name that only shares a prefix is not nested.
#[test]
fn directories_that_equal_nest_or_surround_each_other_are_refused() {
    let cases = [
        ("MDM_ETC", "/var/lib/hostkeeper-mdm", "MDM_ETC"),
        ("MDM_ETC", "/var/lib/hostkeeper-mdm/etc", "MDM_ETC"),
        ("MDM_ETC", "/var", "MDM_ETC"),
        ("MDM_BIN", "/var/lib/hostkeeper-mdm", "MDM_BIN"),
        ("MDM_BIN", "/var/lib/hostkeeper-mdm/bin", "MDM_BIN"),
        ("MDM_BIN", "/etc/hostkeeper-mdm", "MDM_BIN"),
        ("MDM_BIN", "/etc/hostkeeper-mdm/bin", "MDM_BIN"),
        ("MDM_STATE", "/opt", "MDM_BIN"),
        ("MDM_STATE", "/etc/hostkeeper-mdm/state", "MDM_ETC"),
    ];
    for (key, value, clash) in cases {
        let got = refusal(&with(key, value));
        assert!(
            got.starts_with(&format!("{clash}=")),
            "{key}={value}: {got}"
        );
        assert!(got.contains("equal to, inside or around MDM_"), "{got}");
    }
    let text = with("MDM_ETC", "/var/lib/hostkeeper-mdm-etc");
    let c = MdmConfig::parse(&text).expect("a shared prefix is not nesting");
    assert_eq!(c.etc().as_str(), "/var/lib/hostkeeper-mdm-etc");
}

/// Catches a network check that answers wrongly at the ends of the prefix range a
/// [`Cidr`] may have: a /8 holds its whole block and nothing past it, a /30 its four
/// addresses only.
#[test]
fn a_network_of_eight_or_thirty_bits_holds_exactly_its_addresses() {
    let block = Cidr::new(Ipv4Addr::new(10, 9, 9, 9), 8).expect("the 10/8 block");
    assert!(block.contains(Ipv4Addr::new(10, 0, 0, 0)));
    assert!(block.contains(Ipv4Addr::new(10, 255, 255, 255)));
    assert!(!block.contains(Ipv4Addr::new(11, 0, 0, 0)));
    assert!(!block.contains(Ipv4Addr::new(9, 255, 255, 255)));
    let four = Cidr::new(Ipv4Addr::new(10, 0, 0, 4), 30).expect("a /30");
    for last in 4..=7 {
        assert!(four.contains(Ipv4Addr::new(10, 0, 0, last)), "{last}");
    }
    assert!(!four.contains(Ipv4Addr::new(10, 0, 0, 3)));
    assert!(!four.contains(Ipv4Addr::new(10, 0, 0, 8)));
}

/// Catches a [`Cidr`] that can be built without the parser's checks (komira-ai/kbf#2248:
/// its fields were public, so a public or too-wide network reached the proxy's
/// `IPAddressAllow=`): [`Cidr::new`] and `parse` refuse what `MDM_LAN_CIDR` refuses, each
/// with its reason, and give the network back through read-only accessors.
#[test]
fn a_network_is_built_only_through_its_checks() {
    let a = |a: u8, b: u8, c: u8, d: u8| Ipv4Addr::new(a, b, c, d);
    let net = Cidr::new(a(10, 1, 0, 0), 24).expect("a private /24");
    assert_eq!((net.addr(), net.bits()), (a(10, 1, 0, 0), 24));
    assert_eq!(net.to_string().parse::<Cidr>(), Ok(net));
    let refused = [
        (a(8, 8, 4, 0), 24, CidrError::NotPrivate(a(8, 8, 4, 0))),
        (
            a(100, 64, 0, 0),
            16,
            CidrError::NotPrivate(a(100, 64, 0, 0)),
        ),
        (a(127, 0, 0, 0), 8, CidrError::NotPrivate(a(127, 0, 0, 0))),
    ];
    for (addr, bits, want) in refused {
        assert_eq!(Cidr::new(addr, bits), Err(want), "{addr}/{bits}");
    }
    for (addr, bits, min) in [
        (a(10, 0, 0, 0), 0, 8),
        (a(10, 0, 0, 0), 7, 8),
        (a(10, 0, 0, 0), 31, 8),
        (a(10, 0, 0, 0), 32, 8),
        (a(172, 16, 0, 0), 11, 12),
        (a(192, 168, 0, 0), 15, 16),
        (a(192, 168, 0, 0), 8, 16),
    ] {
        assert_eq!(
            Cidr::new(addr, bits),
            Err(CidrError::Bits { addr, bits, min }),
            "{addr}/{bits}"
        );
    }
    for (addr, min) in [
        (a(10, 0, 0, 0), 8),
        (a(172, 16, 0, 0), 12),
        (a(192, 168, 0, 0), 16),
    ] {
        let lo = Cidr::new(addr, min).expect("the block itself");
        let hi = Cidr::new(addr, 30).expect("a /30");
        assert_eq!((lo.bits(), hi.bits()), (min, 30));
    }
    let ten = a(10, 0, 0, 0);
    for text in [
        String::new(),
        ten.to_string(),
        format!("{ten}/"),
        format!("{ten}/x"),
        format!("{ten}/+24"),
        format!("{ten}/ 24"),
        format!("{ten}/256"),
        "not-an-address/24".to_owned(),
        format!("{ten}/24/1"),
    ] {
        assert_eq!(
            text.parse::<Cidr>(),
            Err(CidrError::Malformed(text.clone())),
            "{text:?}"
        );
    }
    assert_eq!(
        format!("{}/8", a(192, 168, 0, 0)).parse::<Cidr>(),
        Err(CidrError::Bits {
            addr: a(192, 168, 0, 0),
            bits: 8,
            min: 16
        })
    );
    assert_eq!(
        CidrError::Malformed("x".to_owned()).to_string(),
        "\"x\" is not a.b.c.d/bits"
    );
    assert_eq!(
        CidrError::NotPrivate(a(8, 8, 4, 0)).to_string(),
        format!("{} is not in a private (RFC 1918) block", a(8, 8, 4, 0))
    );
    assert_eq!(
        CidrError::Bits {
            addr: a(192, 168, 0, 0),
            bits: 8,
            min: 16
        }
        .to_string(),
        format!(
            "{}/8: the prefix is not from 16 to 30 bits",
            a(192, 168, 0, 0)
        )
    );
}

/// Catches a LAN network that widens past private space, and one that is refused though
/// it is exactly a private block: systemd reads the /8 of an address in 192.168/16 as the /8
/// of 192, so the
/// proxy would be allowed to talk to public addresses. The exact /8, /12 and /16 are
/// accepted, and so is a narrower network inside each.
#[test]
fn a_lan_network_stays_inside_its_private_block() {
    let net = |a: u8, b: u8, c: u8, d: u8, bits: u8| format!("{}/{bits}", ip(a, b, c, d));
    let ok = [
        (ip(10, 0, 0, 10), net(10, 0, 0, 0, 8)),
        (ip(10, 0, 0, 10), net(10, 0, 0, 8, 30)),
        (ip(172, 16, 0, 5), net(172, 16, 0, 0, 12)),
        (ip(172, 31, 255, 5), net(172, 16, 0, 0, 12)),
        (ip(172, 20, 1, 5), net(172, 20, 0, 0, 16)),
        (ip(192, 168, 1, 5), net(192, 168, 0, 0, 16)),
        (ip(192, 168, 1, 5), net(192, 168, 1, 0, 24)),
    ];
    for (bind, net) in ok {
        let text = with("MDM_BIND", &bind).replace(
            &format!("MDM_LAN_CIDR={}", lan()),
            &format!("MDM_LAN_CIDR={net}"),
        );
        let c = MdmConfig::parse(&text).unwrap_or_else(|e| panic!("{bind} {net}: {e}"));
        assert_eq!(c.lan().to_string(), net);
    }
}

/// Catches two services given one loopback port (komira-ai/kbf#2248: the three ports were
/// checked one at a time): each pair set equal is refused naming the later key and the port
/// it repeats, three equal ports are refused at the first pair, and ports that differ by one
/// are taken.
#[test]
fn the_three_loopback_ports_are_checked_together() {
    let ports = |nanohub: &str, scep: &str, dep: &str| {
        with("MDM_NANOHUB_PORT", nanohub)
            .replace("MDM_SCEP_PORT=9005", &format!("MDM_SCEP_PORT={scep}"))
            .replace("MDM_DEP_PORT=9001", &format!("MDM_DEP_PORT={dep}"))
    };
    let cases = [
        (
            ("9004", "9004", "9001"),
            "MDM_SCEP_PORT=9004: the same port as MDM_NANOHUB_PORT",
        ),
        (
            ("9004", "9005", "9004"),
            "MDM_DEP_PORT=9004: the same port as MDM_NANOHUB_PORT",
        ),
        (
            ("9004", "9005", "9005"),
            "MDM_DEP_PORT=9005: the same port as MDM_SCEP_PORT",
        ),
        (
            ("9004", "9004", "9004"),
            "MDM_SCEP_PORT=9004: the same port as MDM_NANOHUB_PORT",
        ),
        (
            ("1", "65535", "1"),
            "MDM_DEP_PORT=1: the same port as MDM_NANOHUB_PORT",
        ),
    ];
    for ((nanohub, scep, dep), want) in cases {
        assert_eq!(
            refusal(&ports(nanohub, scep, dep)),
            want,
            "{nanohub} {scep} {dep}"
        );
    }
    let c = MdmConfig::parse(&ports("9004", "9005", "9006")).expect("three ports");
    assert_eq!(
        (c.nanohub_port(), c.scep_port(), c.dep_port()),
        (9004, 9005, 9006)
    );
    let c = MdmConfig::parse(&ports("1", "65535", "2")).expect("the ends of the range");
    assert_eq!(
        (c.nanohub_port(), c.scep_port(), c.dep_port()),
        (1, 65535, 2)
    );
}

/// Catches a DNS name or profile identifier longer than DNS allows (komira-ai/kbf#2248): the
/// certificate, the URLs the profiles carry and the Macs' lookups all fail on such a name.
/// A label of 63 characters and a name of 253 are taken, a label of 64 and a name of 254
/// refused, for both keys.
#[test]
fn dns_names_are_within_the_label_and_name_length_limits() {
    let name = |labels: &[usize]| -> String {
        let parts: Vec<String> = labels.iter().map(|n| "a".repeat(*n)).collect();
        parts.join(".")
    };
    // 63 + 1 + 63 + 1 + 63 + 1 + 61 = 253.
    let longest = name(&[63, 63, 63, 61]);
    let too_long = name(&[63, 63, 63, 62]);
    assert_eq!((longest.len(), too_long.len()), (253, 254));
    for key in ["MDM_DNS_NAME", "MDM_PROFILE_ID"] {
        for ok in [name(&[63, 1]), name(&[1, 63]), longest.clone()] {
            let c = MdmConfig::parse(&with(key, &ok))
                .unwrap_or_else(|e| panic!("{key} of {}: {e}", ok.len()));
            let got = if key == "MDM_DNS_NAME" {
                c.dns_name()
            } else {
                c.profile_id()
            };
            assert_eq!(got, ok);
        }
        for bad in [name(&[64, 1]), name(&[1, 64]), too_long.clone()] {
            let got = refusal(&with(key, &bad));
            assert!(got.starts_with(&format!("{key}={bad}: ")), "{key}: {got}");
        }
    }
    assert_eq!(
        refusal(&with("MDM_DNS_NAME", &name(&[64, 1]))),
        format!(
            "MDM_DNS_NAME={}: not a lower-case DNS name with a dot (at most 63 characters a label, 253 in all)",
            name(&[64, 1])
        )
    );
}
