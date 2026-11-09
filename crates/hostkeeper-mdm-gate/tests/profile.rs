//! The profiles' inputs (docs/design/hostkeeper-mdm.md sections 4 and 5.3): the farm's profile
//! identifier, the daemon config the config profile carries (judged by the daemon's own
//! loader), and the nonce profile's keys. What each profile looks like on the wire is
//! `tests/enroll.rs`.

mod support;

use std::sync::Arc;

use hostkeeper_mdm_gate::{
    CommandUuid, ConfigProfile, DaemonProfile, ENROLLMENT_DOMAIN, MACOS_PKG_DOMAIN,
    MacosPkgProfile, ProfileIdentifier, STORES_DOMAIN, StoresProfile, nonce_profile,
};
use plist::{Dictionary, Value};
use support::enrollment::{CONFIG, FakeEnrollments, config, key_hash, side};

/// Catches: an identifier that is not reverse DNS taken (one label, an empty label, a label
/// with another character, a label of 64 or that starts or ends with `-`, more than sixteen
/// labels, more than 200 characters), or a good one refused; the two profiles named otherwise
/// than under it; a refusal that does not name the flag and the text.
#[test]
fn a_profile_identifier_is_a_reverse_dns_name() {
    let label = "a".repeat(63);
    let long = format!("{label}.{label}.{label}.abcdefgh");
    assert_eq!(long.len(), 200);
    let sixteen = vec!["a"; 16].join(".");
    for good in [
        "com.example.hostkeeper.mdm",
        "com.komira.hostkeeper.mdm",
        "a.b",
        "Com.Ex-ample.9",
        long.as_str(),
        sixteen.as_str(),
    ] {
        let identifier = ProfileIdentifier::parse(good).expect(good);
        assert_eq!(identifier.as_str(), good);
        assert_eq!(identifier.config(), format!("{good}.config"));
        assert_eq!(identifier.nonce(), format!("{good}.nonce"));
    }
    let too_long = format!("{long}x");
    let label64 = format!("com.{}", "a".repeat(64));
    let seventeen = vec!["a"; 17].join(".");
    for bad in [
        "",
        "hostkeeper",
        "com..example",
        ".com.example",
        "com.example.",
        "com.exa_mple",
        "com.exa mple",
        "com.-example",
        "com.example-",
        label64.as_str(),
        seventeen.as_str(),
        too_long.as_str(),
    ] {
        let error = ProfileIdentifier::parse(bad).expect_err(bad);
        let shown: String = bad.chars().take(200).collect();
        assert_eq!(
            error.to_string(),
            format!(
                "--mdm-profile-identifier: {shown:?} is not a reverse-DNS name (two or more labels of letters, digits and -, as com.example.hostkeeper.mdm)"
            )
        );
    }
}

/// Catches: a daemon config the daemon's loader would refuse on the Mac (an unknown key, a
/// missing key, a bad value) taken at start, or refused without the flag and the loader's
/// reason.
#[test]
fn a_daemon_config_the_loader_refuses_is_refused() {
    for (text, why) in [
        (
            CONFIG.replace("release_channel", "release_chanel"),
            "unknown",
        ),
        (
            CONFIG.replace("https://releases.example", "http://releases.example"),
            "https",
        ),
        (
            "coordinator = \"https://c.example\"\n".to_owned(),
            "ca_sha256",
        ),
    ] {
        let loader = hostkeeper_config::daemon::DaemonConfig::parse(&text)
            .expect_err("the loader refuses it")
            .to_string();
        assert!(loader.contains(why), "{loader}");
        let error = DaemonProfile::from_toml(&text).expect_err("refused");
        assert_eq!(error.to_string(), format!("--mdm-daemon-config: {loader}"));
    }
    DaemonProfile::from_toml(CONFIG).expect("the example is taken");
}

/// Catches: the nonce profile's keys under another domain or names, the nonce not as data, the
/// request id not its hex; the enrollment side's `Debug` showing its config.
#[test]
fn the_nonce_profile_holds_the_nonce_as_data_and_the_request_as_hex() {
    let identifier = ProfileIdentifier::parse("com.example.hostkeeper.mdm").expect("an id");
    let uuids = [
        CommandUuid::from_random([1; 16]),
        CommandUuid::from_random([2; 16]),
    ];
    let bytes = nonce_profile(
        &identifier,
        &[0xAB; 32],
        &key_hash(5),
        [&uuids[0], &uuids[1]],
    );
    let profile = Value::from_reader(std::io::Cursor::new(bytes))
        .expect("a plist")
        .into_dictionary()
        .expect("a dictionary");
    assert_eq!(
        profile["PayloadIdentifier"].as_string(),
        Some("com.example.hostkeeper.mdm.nonce")
    );
    assert_eq!(profile["PayloadUUID"].as_string(), Some(uuids[0].as_str()));
    let payload = profile["PayloadContent"].as_array().expect("payloads")[0]
        .as_dictionary()
        .expect("a payload");
    assert_eq!(payload["PayloadUUID"].as_string(), Some(uuids[1].as_str()));
    let settings = payload["PayloadContent"].as_dictionary().expect("domains")[ENROLLMENT_DOMAIN]
        .as_dictionary()
        .expect("the domain")["Forced"]
        .as_array()
        .expect("Forced")[0]
        .as_dictionary()
        .expect("an entry")["mcx_preference_settings"]
        .as_dictionary()
        .expect("settings");
    assert_eq!(settings.len(), 2);
    assert_eq!(settings["nonce"].as_data(), Some(&[0xAB; 32][..]));
    assert_eq!(
        settings["request_id"].as_string(),
        Some(key_hash(5).to_hex().as_str())
    );

    let shown = format!("{:?}", side(&Arc::new(FakeEnrollments::default())));
    assert_eq!(
        shown,
        "EnrollSide { identifier: ProfileIdentifier(\"com.example.hostkeeper.mdm\"), .. }"
    );
}

/// Two stores, one of them an OCI mirror, as `stores.toml` says them.
const STORES: &str = r#"[[store]]
name = "github"
kind = "https"
base = "https://github.com/"
redirects = 3

[[store]]
name = "lan"
kind = "oci"
base = "https://mirror.example/"
mirror = true
repository = "releases"
"#;

/// The fingerprint of a leaf certificate in the tests.
const SIGNER: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

/// The signers file naming [`SIGNER`].
fn signers() -> String {
    format!("signers = [\"{SIGNER}\"]\n")
}

fn full() -> ConfigProfile {
    config()
        .with_stores(StoresProfile::from_toml(STORES).expect("the loader accepts it"))
        .with_macos_pkg(MacosPkgProfile::from_toml(&signers()).expect("the loader accepts it"))
}

/// The settings each domain of the payload of the config profile `profile` forces.
fn domains_of(profile: &ConfigProfile) -> Vec<(String, Dictionary)> {
    let identifier = ProfileIdentifier::parse("com.example.hostkeeper.mdm").expect("an id");
    let uuids = [
        CommandUuid::from_random([1; 16]),
        CommandUuid::from_random([2; 16]),
    ];
    let bytes = profile.profile(&identifier, [&uuids[0], &uuids[1]]);
    let root = Value::from_reader(std::io::Cursor::new(bytes))
        .expect("a plist")
        .into_dictionary()
        .expect("a dictionary");
    assert_eq!(
        root["PayloadIdentifier"].as_string(),
        Some("com.example.hostkeeper.mdm.config")
    );
    let payloads = root["PayloadContent"].as_array().expect("payloads");
    assert_eq!(payloads.len(), 1, "one payload");
    payloads[0].as_dictionary().expect("a payload")["PayloadContent"]
        .as_dictionary()
        .expect("domains")
        .iter()
        .map(|(domain, forced)| {
            let entries = forced.as_dictionary().expect("a domain")["Forced"]
                .as_array()
                .expect("Forced");
            assert_eq!(entries.len(), 1);
            let settings = entries[0].as_dictionary().expect("an entry")["mcx_preference_settings"]
                .as_dictionary()
                .expect("settings")
                .clone();
            (domain.clone(), settings)
        })
        .collect()
}

/// Catches: the stores or the signers missing from the one payload, under another domain, with
/// keys other than the files' (a table of an array flattened, a boolean or a number made text),
/// or the daemon's config changed by them; a profile with neither carrying a domain for them.
#[test]
fn a_profile_with_stores_and_signers_holds_the_three_domains_with_the_keys_of_the_files() {
    let domains = domains_of(&full());
    let names: Vec<&str> = domains.iter().map(|(domain, _)| domain.as_str()).collect();
    assert_eq!(
        names,
        ["dev.hostkeeper.config", STORES_DOMAIN, MACOS_PKG_DOMAIN]
    );
    assert_eq!(
        (MACOS_PKG_DOMAIN, STORES_DOMAIN),
        ("dev.hostkeeper.macos-pkg", "dev.hostkeeper.stores")
    );
    let by = |name: &str| {
        domains
            .iter()
            .find(|(d, _)| d == name)
            .expect(name)
            .1
            .clone()
    };
    assert_eq!(by("dev.hostkeeper.config").len(), 5);

    let signers = by(MACOS_PKG_DOMAIN);
    assert_eq!(signers.keys().collect::<Vec<_>>(), ["signers"]);
    assert_eq!(signers["signers"], Value::Array(vec![Value::from(SIGNER)]));

    let stores = by(STORES_DOMAIN);
    assert_eq!(stores.keys().collect::<Vec<_>>(), ["store"]);
    let list = stores["store"].as_array().expect("an array of stores");
    assert_eq!(list.len(), 2);
    let github = list[0].as_dictionary().expect("a store");
    assert_eq!(github["name"].as_string(), Some("github"));
    assert_eq!(github["kind"].as_string(), Some("https"));
    assert_eq!(github["redirects"].as_signed_integer(), Some(3));
    let lan = list[1].as_dictionary().expect("a store");
    assert_eq!(lan["mirror"].as_boolean(), Some(true));
    assert_eq!(lan["repository"].as_string(), Some("releases"));

    let only: Vec<String> = domains_of(&config()).into_iter().map(|(d, _)| d).collect();
    assert_eq!(only, ["dev.hostkeeper.config"]);
}

/// Catches: a digest that changes with the UUIDs a profile is sent with (so every send would
/// look like a change), that does not change when a domain is added or a signer is changed (so
/// a change is never sent), or that is not 64 lower-case hex digits.
#[test]
fn the_settings_digest_follows_the_settings_and_nothing_else() {
    let digest = full().settings_sha256();
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );
    assert_eq!(full().settings_sha256(), digest, "the same settings");

    let alone = config().settings_sha256();
    let with_stores = config()
        .with_stores(StoresProfile::from_toml(STORES).expect("stores"))
        .settings_sha256();
    let other_signer = SIGNER.replace('a', "c");
    let rotated = config()
        .with_stores(StoresProfile::from_toml(STORES).expect("stores"))
        .with_macos_pkg(
            MacosPkgProfile::from_toml(&signers().replace(SIGNER, &other_signer)).expect("signers"),
        )
        .settings_sha256();
    let all = [&digest, &alone, &with_stores, &rotated];
    for (i, one) in all.iter().enumerate() {
        for other in &all[i + 1..] {
            assert_ne!(one, other);
        }
    }
    let changed = DaemonProfile::from_toml(&CONFIG.replace("2100", "2200")).expect("config");
    assert_ne!(ConfigProfile::new(changed).settings_sha256(), alone);
}

/// Catches: a stores file the Mac's daemon would refuse (an unknown key, a bad kind, text that
/// is not TOML, a `store` that is not a list) taken at start, or refused without the flag and
/// the loader's own reason; a good one, or one with no store, refused.
#[test]
fn a_stores_file_the_loader_refuses_is_refused_with_its_reason() {
    let unknown = STORES.replace("redirects", "redirect");
    let kind = STORES.replace("\"oci\"", "\"ftp\"");
    for text in [
        unknown.as_str(),
        kind.as_str(),
        "this is = = not toml",
        "store = 1\n",
    ] {
        let loader = hostkeeper_package::stores::parse(text.as_bytes(), 0)
            .expect_err("the loader refuses it")
            .to_string();
        let error = StoresProfile::from_toml(text).expect_err("refused");
        assert_eq!(error.to_string(), format!("--mdm-stores: {loader}"));
    }
    let none = StoresProfile::from_toml("").expect("no store is a file");
    assert_eq!(none, StoresProfile::from_toml("# nothing\n").expect("also"));
    StoresProfile::from_toml(STORES).expect("the example is taken");
}

/// Catches: a stores domain that names a credential file (a secret's path, a file that is not on
/// the Mac) taken, or refused without naming the flag, the store and the reason; a store of a
/// list whose first stores are fine named as another.
#[test]
fn a_store_that_names_a_credential_file_is_refused() {
    for key in ["basic_auth_file", "bearer_file"] {
        let text = format!(
            "{STORES}\n[[store]]\nname = \"vault\"\nkind = \"https\"\nbase = \"https://v.example/\"\n{key} = \"/etc/hostkeeper/stores/vault\"\n"
        );
        let error = StoresProfile::from_toml(&text).expect_err("refused");
        assert_eq!(
            error.to_string(),
            "--mdm-stores: store \"vault\" names a credential file (basic_auth_file or bearer_file): a profile carries no secret, and the file is not on the Mac"
        );
    }
    let unnamed = "[[store]]\nbearer_file = \"/x\"\n";
    let error = StoresProfile::from_toml(unnamed).expect_err("refused");
    assert!(
        error
            .to_string()
            .contains("store \"\" names a credential file")
    );
}

/// Catches: a signers file the Mac's daemon would refuse (no signer, five, a signer that is not
/// 64 hex digits, an unknown key, text that is not TOML) taken at start, or refused without the
/// flag and the loader's reason; two signers (a rotation's overlap) refused.
#[test]
fn a_signers_file_the_loader_refuses_is_refused_with_its_reason() {
    let five = format!(
        "signers = [{}]\n",
        vec![format!("\"{SIGNER}\""); 5].join(", ")
    );
    for text in [
        "signers = []\n".to_owned(),
        five,
        "signers = [\"abc\"]\n".to_owned(),
        format!("signers = [\"{SIGNER}\"]\nextra = 1\n"),
        "= not toml".to_owned(),
    ] {
        let loader = hostkeeper_install::macos_pkg::signers::parse(text.as_bytes())
            .expect_err("the loader refuses it")
            .to_string();
        let error = MacosPkgProfile::from_toml(&text).expect_err("refused");
        assert_eq!(
            error.to_string(),
            format!("--mdm-macos-pkg: the signers {loader}")
        );
    }
    let two = format!("signers = [\"{SIGNER}\", \"{}\"]\n", "0".repeat(64));
    MacosPkgProfile::from_toml(&two).expect("a rotation's overlap");
}
