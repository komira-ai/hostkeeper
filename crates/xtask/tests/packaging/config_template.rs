//! The config template, `hostkeeper.toml.template`, loaded with the daemon config's own loader.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use hostkeeper_config::daemon::{Bounds, ConfigError, DaemonConfig, TrustedSigners};

use crate::template;
use crate::testdir::TestDir;
use crate::{TEMPLATE, altered, read, refuses};

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
        "\".github/workflows/artifacts.yml\"",
    ),
    ("SIGNER_REFS", "trusted_signers.refs", "\"refs/heads/main\""),
    (
        "SIGNER_HOSTED_RUNNER",
        "trusted_signers.hosted_runner",
        "true",
    ),
];

/// `text` with every placeholder filled but `left`, which keeps its value exactly as written.
fn filled(text: &str, left: Option<&str>) -> String {
    let values: BTreeMap<&str, &str> = PLACEHOLDERS
        .iter()
        .filter(|(name, ..)| Some(*name) != left)
        .map(|(name, _, value)| (*name, *value))
        .collect();
    template::fill(text, &values, left).expect("every placeholder has a value")
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

/// The template as written is refused for one of its placeholders, and so is the template
/// with any one placeholder left exactly as written and every other filled, each for its own
/// key.
fn check_placeholders_refused(text: &str) -> Result<(), String> {
    match load(text) {
        Err(ConfigError::Invalid { key, .. }) if PLACEHOLDERS.iter().any(|(_, k, _)| *k == key) => {
        }
        other => return Err(format!("the template as written gives {other:?}")),
    }
    for (name, key, _) in PLACEHOLDERS {
        match load(&filled(text, Some(name))) {
            Err(ConfigError::Invalid { key: refused, .. }) if refused == key => {}
            other => {
                return Err(format!(
                    "with only @@{name}@@ left the loader gives {other:?}, not a refusal of {key}"
                ));
            }
        }
    }
    Ok(())
}

/// Catches a placeholder that the loader would take as a value, so that a host whose config
/// was not filled in would start with it: the template as committed is refused, and so is the
/// template with any one placeholder left as shipped and every other filled, each naming its
/// own key. A bare `"@@NAME@@"` is a word the loader accepts for the signer's organisation,
/// workflow and refs, which is why each placeholder carries a hint with white space.
#[test]
fn config_template_with_placeholders_is_refused_by_the_loader() {
    let text = read(TEMPLATE);
    assert_eq!(check_placeholders_refused(&text), Ok(()));
    let bare = |name: &str, hint: &str| {
        altered(
            &text,
            &format!("\"@@{name}@@ ({hint})\""),
            &format!("\"@@{name}@@\""),
        )
    };
    let cases = [
        (
            bare("SIGNER_ORGANISATION", "a GitHub organisation"),
            "with only @@SIGNER_ORGANISATION@@ left the loader gives Ok(",
        ),
        (
            bare("SIGNER_WORKFLOW", "a workflow path"),
            "with only @@SIGNER_WORKFLOW@@ left the loader gives Ok(",
        ),
        (
            bare("SIGNER_REFS", "one or more ref globs"),
            "with only @@SIGNER_REFS@@ left the loader gives Ok(",
        ),
    ];
    refuses(check_placeholders_refused, &cases);
}

/// The bounds the template writes out: every one, so a host's config states each bound even
/// though the loader would default a missing one to the same number.
const BOUNDS: [&str; 6] = [
    "attestation_timeout_secs",
    "fetch_timeout_secs",
    "force_start_secs",
    "max_entries",
    "max_package_bytes",
    "max_unpacked_bytes",
];

/// The keys written in the `[bounds]` table of `text`.
fn bound_keys(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| *line != "[bounds]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, _)| key.trim().to_owned())
        .collect()
}

fn check_bounds_written(text: &str) -> Result<(), String> {
    let found = bound_keys(text);
    let wanted: BTreeSet<String> = BOUNDS.iter().map(|k| (*k).to_owned()).collect();
    if found != wanted {
        return Err(format!(
            "[bounds] writes {found:?}, not every bound {wanted:?}"
        ));
    }
    Ok(())
}

/// Catches a bound dropped from the template: the loader would supply its default, so the
/// loaded value alone cannot show that the template still writes it.
#[test]
fn config_template_writes_every_bound() {
    let text = read(TEMPLATE);
    assert_eq!(check_bounds_written(&text), Ok(()));
    let cases: Vec<(String, &str)> = BOUNDS
        .iter()
        .map(|key| {
            let line = text
                .lines()
                .find(|line| line.starts_with(key))
                .unwrap_or_else(|| panic!("{key} is in the template"));
            (altered(&text, &format!("{line}\n"), ""), "[bounds] writes")
        })
        .collect();
    refuses(check_bounds_written, &cases);
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
    assert_eq!(
        config.release_channel.as_ref().map(|u| u.as_str()),
        Some("https://releases.example")
    );
    assert_eq!(
        config.trusted_signers,
        TrustedSigners {
            organisation: "example-org".to_owned(),
            workflow: ".github/workflows/artifacts.yml".to_owned(),
            refs: vec!["refs/heads/main".to_owned()],
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
        template::fill(&token, &BTreeMap::new(), None),
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
    let run = |n: usize, alphabet: &str| format!("line 1: a run of {n} {alphabet} characters");
    let cases: [(String, Vec<String>); 9] = [
        (format!("# {pem}"), vec!["line 1: a PEM block".to_owned()]),
        (
            format!("token = \"{github}\""),
            vec![
                run(36, "base64"),
                run(40, "base64url"),
                "line 1: a token starting \"ghp_\"".to_owned(),
            ],
        ),
        (
            "# Authorization: Bearer abc".to_owned(),
            vec!["line 1: a bearer token".to_owned()],
        ),
        (
            format!("key = \"{}\"", "0123456789abcdef".repeat(4)),
            vec![run(64, "base64"), run(64, "base64url")],
        ),
        // Base64 of letters only, with no digit in it.
        (
            format!("key = \"{}\"", "YWJj".repeat(12)),
            vec![run(48, "base64"), run(48, "base64url")],
        ),
        // Padded: `=` ends the base64url run one character short of the base64 one.
        (
            format!("key = \"{}YQ==\"", "YWJj".repeat(8)),
            vec![run(36, "base64"), run(34, "base64url")],
        ),
        // The length is the boundary: 31 characters pass, 32 do not.
        (format!("key = \"{}\"", "a".repeat(31)), vec![]),
        (
            format!("key = \"{}\"", "a".repeat(32)),
            vec![run(32, "base64"), run(32, "base64url")],
        ),
        (
            "jwt = \"eyJhbGciOiJIUzI1NiJ9\"".to_owned(),
            vec!["line 1: a token starting \"eyJ\"".to_owned()],
        ),
    ];
    for (line, wanted) in cases {
        assert_eq!(template::secret_shapes(&line), wanted, "{line}");
    }
    // Appended to the template, a pasted key is found on its own line.
    let pasted = format!("{text}key = \"{}\"\n", "YWJj".repeat(12));
    let last = text.lines().count() + 1;
    assert_eq!(
        template::secret_shapes(&pasted),
        vec![
            format!("line {last}: a run of 48 base64 characters"),
            format!("line {last}: a run of 48 base64url characters"),
        ]
    );
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
