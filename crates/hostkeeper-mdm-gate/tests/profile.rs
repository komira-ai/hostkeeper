//! The profiles' inputs (docs/design/hostkeeper-mdm.md sections 4 and 5.3): the farm's profile
//! identifier, the daemon config the config profile carries (judged by the daemon's own
//! loader), and the nonce profile's keys. What each profile looks like on the wire is
//! `tests/enroll.rs`.

mod support;

use std::sync::Arc;

use hostkeeper_mdm_gate::{
    CommandUuid, DaemonProfile, ENROLLMENT_DOMAIN, ProfileIdentifier, nonce_profile,
};
use plist::Value;
use support::enrollment::{CONFIG, FakeEnrollments, key_hash, side};

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
