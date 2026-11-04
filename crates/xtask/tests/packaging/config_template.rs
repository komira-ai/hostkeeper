//! The config template, `hostkeeper.toml.template`, loaded with the daemon config's own loader.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use hostkeeper_config::daemon::{Bounds, ConfigError, DaemonConfig, TrustedSigners};

use crate::template;
use crate::testdir::TestDir;
use crate::{TEMPLATE, altered, read};

/// Each placeholder of the template, the dotted key it stands for, and a valid value as TOML.
const PLACEHOLDERS: [(&str, &str, &str); 7] = [
    (
        "COORDINATOR",
        "coordinator",
        "\"https://coordinator.example:8443\"",
    ),
    (
        "CA_SHA256",
        "ca_sha256",
        "\"9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\"",
    ),
    (
        "RELEASE_CHANNEL",
        "release_channel",
        "\"https://releases.example\"",
    ),
    (
        "SIGNER_ORGANISATION",
        "trusted_signers.organisation",
        "\"example-org\"",
    ),
    (
        "SIGNER_WORKFLOW",
        "trusted_signers.workflow",
        "\".github/workflows/release.yml\"",
    ),
    ("SIGNER_REFS", "trusted_signers.refs", "\"refs/tags/*\""),
    (
        "SIGNER_HOSTED_RUNNER",
        "trusted_signers.hosted_runner",
        "true",
    ),
];

fn values(left: Option<&str>) -> BTreeMap<&'static str, String> {
    PLACEHOLDERS
        .iter()
        .map(|(name, _, value)| {
            let value = if Some(*name) == left {
                format!("\"@@{name}@@ left unfilled\"")
            } else {
                (*value).to_owned()
            };
            (*name, value)
        })
        .collect()
}

fn filled(text: &str, left: Option<&str>) -> String {
    let values = values(left);
    let borrowed: BTreeMap<&str, &str> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
    template::fill(text, &borrowed).expect("every placeholder has a value")
}

/// Loads `text` with the daemon config's loader, from a file of mode 0600 that the account
/// running the test owns; the loader is told that account is the trusted one, as root is on a
/// host. No owner is changed.
fn load(text: &str) -> Result<DaemonConfig, ConfigError> {
    let dir = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "packaging-config");
    let path = dir.join("hostkeeper.toml");
    fs::write(&path, text).expect("write the config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("set its mode");
    let owner = fs::metadata(&path).expect("its owner").uid();
    DaemonConfig::load(&path, owner)
}

fn refused_key(result: Result<DaemonConfig, ConfigError>) -> String {
    match result {
        Err(ConfigError::Invalid { key, .. }) => key,
        other => panic!("not refused for a value: {other:?}"),
    }
}

/// Catches a placeholder that the loader would take as a value, so that a host whose config
/// was not filled in would start with it: the template as committed is refused, and so is the
/// template with any one placeholder left and every other filled, each naming its own key.
#[test]
fn config_template_with_placeholders_is_refused_by_the_loader() {
    let text = read(TEMPLATE);
    let key = refused_key(load(&text));
    assert!(PLACEHOLDERS.iter().any(|(_, k, _)| *k == key), "{key}");
    for (name, key, _) in PLACEHOLDERS {
        assert_eq!(refused_key(load(&filled(&text, Some(name)))), key, "{name}");
    }
}

/// Catches a template the loader refuses once it is filled in (a key it does not know, a
/// missing key, a bound that breaks its rules), bounds other than the design's, or a
/// placeholder added or removed without this list.
#[test]
fn config_template_loads_once_placeholders_are_filled() {
    let text = read(TEMPLATE);
    let names: BTreeSet<String> = PLACEHOLDERS.iter().map(|(n, ..)| (*n).to_owned()).collect();
    assert_eq!(template::tokens(&text), names);
    let done = filled(&text, None);
    assert_eq!(template::tokens(&done), BTreeSet::new());
    let config = load(&done).expect("the filled template loads");
    assert_eq!(
        config.coordinator.as_str(),
        "https://coordinator.example:8443"
    );
    assert_eq!(
        config.ca_sha256.as_str(),
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    );
    assert_eq!(config.release_channel.as_str(), "https://releases.example");
    assert_eq!(
        config.trusted_signers,
        TrustedSigners {
            organisation: "example-org".to_owned(),
            workflow: ".github/workflows/release.yml".to_owned(),
            refs: vec!["refs/tags/*".to_owned()],
            hosted_runner: true,
        }
    );
    // The design's numbers, written out: a changed default of the loader does not hide a
    // changed template.
    assert_eq!(
        config.bounds,
        Bounds {
            max_package_bytes: 536_870_912,
            max_unpacked_bytes: 2_147_483_648,
            max_entries: 20_000,
            fetch_timeout_secs: 2_100,
            attestation_timeout_secs: 60,
            force_start_secs: 60,
        }
    );
    let other_bound = filled(
        &altered(
            &text,
            "max_entries              = 20000",
            "max_entries              = 30000",
        ),
        None,
    );
    assert_ne!(load(&other_bound).expect("loads").bounds, config.bounds);
    let token = altered(&text, "@@CA_SHA256@@", "@@CA_DIGEST@@");
    assert_ne!(template::tokens(&token), names);
    assert_eq!(
        template::fill(&token, &BTreeMap::new()),
        Err("no value for @@COORDINATOR@@".to_owned())
    );
}

/// Catches a secret pasted into the template: a PEM block, a long hex or base64 run, a bearer
/// token or a token of a published format. The `[bounds]` numbers are not such a run.
#[test]
fn config_template_has_no_secret_shaped_value() {
    let text = read(TEMPLATE);
    assert_eq!(template::secret_shapes(&text), Vec::<String>::new());
    let pem = concat!("-----BEGIN ", "PRIVATE KEY-----");
    let github = format!("ghp_{}", "a1B2".repeat(9));
    let cases = [
        (format!("{text}# {pem}\n"), "a PEM block"),
        (
            format!("{text}token = \"{github}\"\n"),
            "a token starting \"ghp_\"",
        ),
        (
            format!("{text}# Authorization: Bearer abc\n"),
            "a bearer token",
        ),
        (
            format!("{text}key = \"{}\"\n", "0123456789abcdef".repeat(4)),
            "a run of 64 key characters",
        ),
        (
            format!("{text}key = \"{}\"\n", "Zm9vYmFyYmF6cXV4".repeat(3)),
            "a run of 48 key characters",
        ),
        (
            format!("{text}jwt = \"eyJhbGciOiJIUzI1NiJ9\"\n"),
            "a token starting \"eyJ\"",
        ),
    ];
    for (altered_text, words) in cases {
        let found = template::secret_shapes(&altered_text);
        assert!(
            found.iter().any(|f| f.contains(words)),
            "{found:?} lacks {words:?}"
        );
    }
}

/// Catches a template that would need the loader to learn, or to forget, a key: the filled
/// template loads, and the loader refuses a key it does not know in each table of the
/// template, so the load proves every key of the template is one the loader knows.
#[test]
fn template_keys_are_a_subset_of_the_loaders_known_keys() {
    let done = filled(&read(TEMPLATE), None);
    load(&done).expect("the filled template loads");
    let unknown = |result: Result<DaemonConfig, ConfigError>| match result {
        Err(ConfigError::UnknownKey { table, key }) => (table, key),
        other => panic!("not refused for an unknown key: {other:?}"),
    };
    let cases = [
        (format!("services = []\n{done}"), ("", "services")),
        (
            altered(
                &done,
                "[trusted_signers]\n",
                "[trusted_signers]\nrepository = \"x\"\n",
            ),
            ("trusted_signers", "repository"),
        ),
        (
            altered(&done, "[bounds]\n", "[bounds]\nmax_files = 1\n"),
            ("bounds", "max_files"),
        ),
    ];
    for (text, (table, key)) in cases {
        assert_eq!(unknown(load(&text)), (table.to_owned(), key.to_owned()));
    }
}
