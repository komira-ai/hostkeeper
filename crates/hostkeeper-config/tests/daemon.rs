//! The daemon config: its format (design section 4.1), the rules between its bounds, and the
//! file it is read from.

mod support;

use std::os::unix::fs::symlink;

use hostkeeper_config::daemon::{
    Bounds, ConfigError, DaemonConfig, MIN_FETCH_BYTES_PER_SEC, TrustedSigners, ref_matches,
};
use hostkeeper_config::trust::TrustError;
use support::{Scratch, fifo, my_uid};

const CA: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

/// The example of design section 4.1, with a real digest in place of its placeholder.
const EXAMPLE: &str = r#"coordinator     = "https://coordinator.example:8443"
ca_sha256       = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
release_channel = "https://releases.example"

[trusted_signers]          # one org-level entry; no per-service pins
organisation = "komira-ai"
workflow     = ".github/workflows/release.yml"
refs         = ["refs/tags/*"]
hosted_runner = true

[bounds]
max_package_bytes      = 536870912
max_unpacked_bytes     = 2147483648
max_entries            = 20000
fetch_timeout_secs     = 2100   # total fetch bound
attestation_timeout_secs = 60
force_start_secs       = 60
"#;

/// `EXAMPLE` with `from` written as `to`. Panics if `from` is not in it, so a case cannot pass by
/// changing nothing.
fn with(from: &str, to: &str) -> String {
    assert!(EXAMPLE.contains(from), "{from:?} is not in the example");
    EXAMPLE.replacen(from, to, 1)
}

fn refusal(text: &str) -> ConfigError {
    DaemonConfig::parse(text).expect_err("the text must be refused")
}

fn unknown(table: &str, key: &str) -> (String, String) {
    (table.to_owned(), key.to_owned())
}

fn unknown_of(error: ConfigError) -> (String, String) {
    match error {
        ConfigError::UnknownKey { table, key } => (table, key),
        other => panic!("not an unknown key: {other:?}"),
    }
}

fn invalid_key(error: ConfigError) -> String {
    match error {
        ConfigError::Invalid { key, .. } => key,
        other => panic!("not an invalid value: {other:?}"),
    }
}

/// The bounds of the example, with `max_package_bytes` and `fetch_timeout_secs` set.
fn fetch(max_package_bytes: u64, fetch_timeout_secs: u64) -> Bounds {
    Bounds {
        max_package_bytes,
        max_unpacked_bytes: u64::MAX,
        fetch_timeout_secs,
        ..Bounds::default()
    }
}

/// B8, previous release on new data: the daemon config as the previous release's loader required it
/// (with `release_channel`) loads with this loader, and every other field is what it was.
#[test]
fn a_config_the_previous_release_required_to_carry_release_channel_still_loads() {
    let config = DaemonConfig::parse(EXAMPLE).expect("the previous release's config loads");
    assert_eq!(
        config.release_channel.as_ref().map(|u| u.as_str()),
        Some("https://releases.example")
    );
    assert_eq!(config.ca_sha256.as_str(), CA);
}

/// B8, new release on the data the next release writes: a config with no `release_channel` loads
/// and equals the previous release's config in every other field. (The previous release's loader
/// refuses that config, which is why the generated configs keep the key for one more release.)
#[test]
fn a_config_without_release_channel_loads_and_differs_only_in_that_field() {
    let without: String = EXAMPLE
        .lines()
        .filter(|line| !line.starts_with("release_channel"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(!without.contains("release_channel"), "{without}");
    let new = DaemonConfig::parse(&without).expect("a config without release_channel loads");
    let old = DaemonConfig::parse(EXAMPLE).expect("the example loads");
    assert_eq!(new.release_channel, None);
    assert_eq!(
        DaemonConfig {
            release_channel: old.release_channel.clone(),
            ..new
        },
        old
    );
}

/// A `release_channel` that is present is still checked: a wrong type is refused with its name.
#[test]
fn a_release_channel_of_the_wrong_type_is_refused_by_name() {
    let text = with(
        "release_channel = \"https://releases.example\"",
        "release_channel = 1",
    );
    assert_eq!(invalid_key(refusal(&text)), "release_channel");
}

/// Catches a renamed key or a changed default: the example parses and every field is its value.
#[test]
fn design_example_loads_with_every_value() {
    let config = DaemonConfig::parse(EXAMPLE).expect("the design's example loads");
    assert_eq!(
        config.coordinator.as_str(),
        "https://coordinator.example:8443"
    );
    assert_eq!(config.ca_sha256.as_str(), CA);
    assert_eq!(
        config.release_channel.as_ref().map(|u| u.as_str()),
        Some("https://releases.example")
    );
    assert_eq!(
        config.trusted_signers,
        TrustedSigners {
            organisation: "komira-ai".to_owned(),
            workflow: ".github/workflows/release.yml".to_owned(),
            refs: vec!["refs/tags/*".to_owned()],
            hosted_runner: true,
        }
    );
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
    assert_eq!(config.bounds, Bounds::default());
    let other = with("hosted_runner = true", "hosted_runner = false");
    assert!(
        !DaemonConfig::parse(&other)
            .unwrap()
            .trusted_signers
            .hosted_runner
    );
}

/// Catches a table that ignores a key it does not know: an extra key at the top level, in
/// `[trusted_signers]` and in `[bounds]` is `UnknownKey` naming the key and its table.
#[test]
fn unknown_key_is_refused_naming_it_in_every_table() {
    let top = format!("extra = 1\n{EXAMPLE}");
    assert_eq!(unknown_of(refusal(&top)), unknown("", "extra"));
    let signers = with(
        "hosted_runner = true",
        "hosted_runner = true\nrepository = \"x/y\"",
    );
    assert_eq!(
        unknown_of(refusal(&signers)),
        unknown("trusted_signers", "repository")
    );
    let bounds = with(
        "force_start_secs       = 60",
        "force_start_secs = 60\nmax_files = 3",
    );
    assert_eq!(unknown_of(refusal(&bounds)), unknown("bounds", "max_files"));
    let table = format!("{EXAMPLE}\n[extras]\nx = 1\n");
    assert_eq!(unknown_of(refusal(&table)), unknown("", "extras"));
    let error = refusal(&bounds).to_string();
    assert!(
        error.contains("\"max_files\"") && error.contains("\"bounds\""),
        "{error}"
    );
}

/// Catches the declaration format surviving: `user`, `capabilities` and `services` at the top
/// level are unknown keys.
#[test]
fn old_declaration_keys_are_refused() {
    for line in [
        "user = \"root\"",
        "capabilities = [\"CAP_NET_ADMIN\"]",
        "services = [\"tierstore\"]",
    ] {
        let key = line.split(' ').next().unwrap();
        let text = format!("{line}\n{EXAMPLE}");
        assert_eq!(unknown_of(refusal(&text)), unknown("", key), "{line}");
    }
}

/// Catches an unsigned mode: a config with no `[trusted_signers]` is refused, naming it.
#[test]
fn missing_trusted_signers_is_refused() {
    let start = EXAMPLE.find("[trusted_signers]").unwrap();
    let end = EXAMPLE.find("[bounds]").unwrap();
    let text = format!("{}{}", &EXAMPLE[..start], &EXAMPLE[end..]);
    let error = refusal(&text);
    assert!(error.to_string().contains("no unsigned mode"), "{error}");
    assert_eq!(invalid_key(error), "trusted_signers");
    let not_a_table = text.replace(
        "release_channel = \"https://releases.example\"",
        "release_channel = \"https://releases.example\"\ntrusted_signers = 1",
    );
    assert_eq!(invalid_key(refusal(&not_a_table)), "trusted_signers");
}

/// Catches a required key that defaults silently: each one, left out, is refused with its
/// dotted name; and a key of the wrong type is refused with its name too.
#[test]
fn missing_required_key_names_the_key() {
    for (line, key) in [
        (
            "coordinator     = \"https://coordinator.example:8443\"",
            "coordinator",
        ),
        ("ca_sha256       = \"", "ca_sha256"),
        (
            "organisation = \"komira-ai\"",
            "trusted_signers.organisation",
        ),
        (
            "workflow     = \".github/workflows/release.yml\"",
            "trusted_signers.workflow",
        ),
        ("refs         = [\"refs/tags/*\"]", "trusted_signers.refs"),
        ("hosted_runner = true", "trusted_signers.hosted_runner"),
    ] {
        let text: String = EXAMPLE
            .lines()
            .filter(|l| !l.starts_with(line))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_ne!(text, EXAMPLE, "{line}");
        let error = refusal(&text);
        assert!(error.to_string().contains("is missing"), "{error}");
        assert_eq!(invalid_key(error), key, "{line}");
    }
    for (from, to, key) in [
        (
            "hosted_runner = true",
            "hosted_runner = \"yes\"",
            "trusted_signers.hosted_runner",
        ),
        (
            "refs         = [\"refs/tags/*\"]",
            "refs = \"refs/tags/*\"",
            "trusted_signers.refs",
        ),
        (
            "refs         = [\"refs/tags/*\"]",
            "refs = [1]",
            "trusted_signers.refs",
        ),
        (
            "max_entries            = 20000",
            "max_entries = \"20000\"",
            "bounds.max_entries",
        ),
        (
            "max_entries            = 20000",
            "max_entries = -1",
            "bounds.max_entries",
        ),
    ] {
        let error = refusal(&with(from, to));
        assert!(error.to_string().contains(" is "), "{error}");
        assert_eq!(invalid_key(error), key, "{to}");
    }
    let start = EXAMPLE.find("[bounds]").unwrap();
    let bounds = format!("bounds = 5\n{}", &EXAMPLE[..start]);
    assert_eq!(invalid_key(refusal(&bounds)), "bounds");
}

/// Catches a default that differs from the design's example, or a partial `[bounds]` that drops
/// the defaults of the keys it leaves out.
#[test]
fn bounds_defaults_apply_when_bounds_table_absent() {
    let start = EXAMPLE.find("[bounds]").unwrap();
    let config = DaemonConfig::parse(&EXAMPLE[..start]).expect("loads without [bounds]");
    assert_eq!(config.bounds, Bounds::default());
    let partial = format!("{}[bounds]\nmax_entries = 7\n", &EXAMPLE[..start]);
    let config = DaemonConfig::parse(&partial).expect("loads with one bound");
    assert_eq!(
        config.bounds,
        Bounds {
            max_entries: 7,
            ..Bounds::default()
        }
    );
}

/// Catches an off-by-one or a wrong rate in the fetch rule: a package of exactly 256 KiB per
/// second of the timeout is accepted, in the check and in a file.
#[test]
fn fetch_bound_exactly_at_256_kib_per_second_is_accepted() {
    assert_eq!(MIN_FETCH_BYTES_PER_SEC, 256 * 1024);
    for secs in [1, 2_100, 3_600] {
        assert!(fetch(secs * 262_144, secs).check().is_ok(), "{secs}");
    }
    let text = with("fetch_timeout_secs     = 2100", "fetch_timeout_secs = 2048");
    let config = DaemonConfig::parse(&text).expect("536870912 bytes in 2048 s is 256 KiB/s");
    assert_eq!(config.bounds.fetch_timeout_secs, 2_048);
}

/// The other side of the boundary: one byte more, or one second less, is `FetchBoundTooSmall`
/// with both numbers.
#[test]
fn one_byte_over_is_refused_with_fetch_bound_too_small() {
    for (bytes, secs) in [(262_145, 1), (2_100 * 262_144 + 1, 2_100)] {
        match fetch(bytes, secs).check() {
            Err(ConfigError::FetchBoundTooSmall {
                max_package_bytes,
                fetch_timeout_secs,
            }) => assert_eq!((max_package_bytes, fetch_timeout_secs), (bytes, secs)),
            other => panic!("{bytes} in {secs}: {other:?}"),
        }
    }
    let text = with("fetch_timeout_secs     = 2100", "fetch_timeout_secs = 2047");
    let error = refusal(&text);
    assert!(error.to_string().contains("256 KiB/s"), "{error}");
    assert!(matches!(
        error,
        ConfigError::FetchBoundTooSmall {
            max_package_bytes: 536_870_912,
            fetch_timeout_secs: 2_047
        }
    ));
}

/// Catches a bound check that overflows: `u64::MAX` everywhere is refused, not a panic; so is a
/// timeout whose product with the rate does not fit, from a file; and a number TOML cannot hold
/// is a syntax error.
#[test]
fn huge_values_do_not_overflow_the_bound_check() {
    let huge = Bounds {
        max_package_bytes: u64::MAX,
        max_unpacked_bytes: u64::MAX,
        max_entries: u64::MAX,
        fetch_timeout_secs: u64::MAX,
        attestation_timeout_secs: u64::MAX,
        force_start_secs: u64::MAX,
    };
    let error = huge.check().expect_err("refused");
    assert!(error.to_string().contains("too large"), "{error}");
    assert_eq!(invalid_key(error), "bounds.fetch_timeout_secs");
    let text = with(
        "fetch_timeout_secs     = 2100",
        &format!("fetch_timeout_secs = {}", i64::MAX),
    );
    assert_eq!(invalid_key(refusal(&text)), "bounds.fetch_timeout_secs");
    let text = with(
        "max_entries            = 20000",
        &format!("max_entries = {}", u64::MAX),
    );
    assert!(matches!(refusal(&text), ConfigError::Syntax { .. }));
    let big = fetch(u64::MAX / 2, u64::MAX / MIN_FETCH_BYTES_PER_SEC);
    assert!(big.check().is_ok());
}

/// Catches a bound of 0: each of the six is refused, naming it.
#[test]
fn zero_bounds_are_refused() {
    for key in [
        "max_package_bytes",
        "max_unpacked_bytes",
        "max_entries",
        "fetch_timeout_secs",
        "attestation_timeout_secs",
        "force_start_secs",
    ] {
        let line = EXAMPLE.lines().find(|line| line.starts_with(key)).unwrap();
        let text = with(line, &format!("{key} = 0"));
        let error = refusal(&text);
        assert!(error.to_string().contains("is 0"), "{error}");
        assert_eq!(invalid_key(error), format!("bounds.{key}"));
    }
}

/// Catches an unpacked bound below the package bound; equal is accepted.
#[test]
fn unpacked_smaller_than_package_is_refused() {
    let text = with(
        "max_unpacked_bytes     = 2147483648",
        "max_unpacked_bytes = 536870911",
    );
    assert_eq!(invalid_key(refusal(&text)), "bounds.max_unpacked_bytes");
    let equal = with(
        "max_unpacked_bytes     = 2147483648",
        "max_unpacked_bytes = 536870912",
    );
    assert!(DaemonConfig::parse(&equal).is_ok());
}

/// Catches a CA digest that is not one, a URL that is not `https://`, and a signer value that
/// is empty or holds white space.
#[test]
fn bad_ca_digest_and_non_https_urls_are_refused() {
    for digest in [
        "",
        &CA[..63],
        &format!("{CA}0"),
        &CA.replacen('9', "g", 1),
        &CA.replacen('9', " ", 1),
    ] {
        let text = with(CA, digest);
        assert_eq!(invalid_key(refusal(&text)), "ca_sha256", "{digest:?}");
    }
    let upper = with(CA, &CA.to_ascii_uppercase());
    assert_eq!(DaemonConfig::parse(&upper).unwrap().ca_sha256.as_str(), CA);
    for url in [
        "http://coordinator.example:8443",
        "coordinator.example",
        "https://",
        "https://user@coordinator.example",
        "https://coordinator.example:0",
        "https://coordinator.example:65536",
        "https://coordinator.example:",
        "https://coordinator.example:{port}",
        "https://coordinator.example/a b",
        "https://coordinator.example/?x=1",
        "https://coordinator.example#top",
        "https://[]",
        "ftp://coordinator.example",
    ] {
        let text = with("https://coordinator.example:8443", url);
        assert_eq!(invalid_key(refusal(&text)), "coordinator", "{url}");
        let text = with("https://releases.example", url);
        assert_eq!(invalid_key(refusal(&text)), "release_channel", "{url}");
    }
    for url in [
        "https://releases.example/channel/stable",
        "https://192.0.2.1:443",
        "https://[2001:db8::1]:8443/x",
        "https://[2001:db8::1]",
    ] {
        let text = with("https://releases.example", url);
        let config = DaemonConfig::parse(&text).unwrap_or_else(|e| panic!("{url}: {e}"));
        assert_eq!(
            config.release_channel.as_ref().map(|u| u.as_str()),
            Some(url)
        );
    }
    for (from, to, key) in [
        ("\"komira-ai\"", "\"\"", "trusted_signers.organisation"),
        (
            "\"komira-ai\"",
            "\"komira ai\"",
            "trusted_signers.organisation",
        ),
        (
            "\".github/workflows/release.yml\"",
            "\"a\\tb\"",
            "trusted_signers.workflow",
        ),
        ("[\"refs/tags/*\"]", "[]", "trusted_signers.refs"),
        (
            "[\"refs/tags/*\"]",
            "[\"refs/tags/*\", \"\"]",
            "trusted_signers.refs",
        ),
        (
            "[\"refs/tags/*\"]",
            "[\"refs/heads/a b\"]",
            "trusted_signers.refs",
        ),
    ] {
        let text = with(from, to);
        assert_eq!(invalid_key(refusal(&text)), key, "{to}");
    }
}

/// Catches an IPv6 literal judged by its characters only, and a path grammar narrower or wider
/// than a URL path: a bracketed host must parse as an IPv6 address; a path may hold escapes,
/// sub-delimiters, `:` and `@`, and a `%` must start two hex digits.
#[test]
fn bracketed_hosts_and_url_paths_are_checked_as_url_parts() {
    for url in [
        "https://[1]",
        "https://[::1::]",
        "https://[g::1]",
        "https://[1:2:3:4:5:6:7:8:9]",
        "https://[:::]",
        "https://[192.0.2.1]",
        "https://releases.example/a%2",
        "https://releases.example/a%zz",
        "https://releases.example/a%",
        "https://releases.example/{x}",
        "https://releases.example/a\\b",
    ] {
        let text = with("https://releases.example", url);
        assert_eq!(invalid_key(refusal(&text)), "release_channel", "{url}");
    }
    for url in [
        "https://[0:0:0:0:0:0:0:1]:8443",
        "https://[2001:db8:0:0:0:0:0:1]",
        "https://[::ffff:192.0.2.1]",
        "https://releases.example/a%20b/c+d",
        "https://releases.example/x;v=1,2",
        "https://releases.example/~u/@x/a:b/!$&'()*=",
        "https://releases.example/%7E",
    ] {
        let text = with("https://releases.example", url);
        let config = DaemonConfig::parse(&text).unwrap_or_else(|e| panic!("{url}: {e}"));
        assert_eq!(
            config.release_channel.as_ref().map(|u| u.as_str()),
            Some(url)
        );
    }
}

/// The boundaries of the signer strings: 256 printable ASCII bytes are accepted for the
/// organisation, the workflow and each ref glob, 257 are refused, and so are a non-ASCII and a
/// control character, each naming its key.
#[test]
fn signer_strings_are_1_to_256_printable_ascii_bytes() {
    let at = |key: &str, value: &str| {
        let text = match key {
            "organisation" => with("\"komira-ai\"", &format!("\"{value}\"")),
            "workflow" => with("\".github/workflows/release.yml\"", &format!("\"{value}\"")),
            _ => with(
                "[\"refs/tags/*\"]",
                &format!("[\"refs/tags/*\", \"{value}\"]"),
            ),
        };
        DaemonConfig::parse(&text)
    };
    for key in ["organisation", "workflow", "refs"] {
        let longest = "a".repeat(256);
        let config = at(key, &longest).unwrap_or_else(|e| panic!("{key}: {e}"));
        let found = match key {
            "organisation" => config.trusted_signers.organisation,
            "workflow" => config.trusted_signers.workflow,
            _ => config.trusted_signers.refs[1].clone(),
        };
        assert_eq!(found, longest, "{key}");
        for bad in ["a".repeat(257), "\u{e9}".to_owned(), "a\\u0007b".to_owned()] {
            let error = at(key, &bad).expect_err("refused");
            assert_eq!(
                invalid_key(error),
                format!("trusted_signers.{key}"),
                "{key} {bad:?}"
            );
        }
    }
}

/// Catches an unknown key reported after a value is judged: an unknown key and an invalid
/// value together are the unknown key.
#[test]
fn an_unknown_key_is_reported_before_an_invalid_value() {
    let text = format!(
        "extra = 1\n{}",
        with("https://releases.example", "http://releases.example")
    );
    assert_eq!(unknown_of(refusal(&text)), unknown("", "extra"));
    let text = with(
        "max_entries            = 20000",
        "max_entries = 0\nmax_files = 1",
    );
    assert_eq!(unknown_of(refusal(&text)), unknown("bounds", "max_files"));
}

/// Catches a parse error that does not say where: the line of the error is given.
#[test]
fn a_syntax_error_names_its_line() {
    let text = with("max_entries            = 20000", "max_entries = = 1");
    let line = EXAMPLE
        .lines()
        .position(|line| line.starts_with("max_entries"))
        .unwrap()
        + 1;
    match refusal(&text) {
        error @ ConfigError::Syntax { .. } => {
            let text = error.to_string();
            assert!(
                text.starts_with(&format!("the daemon config is not TOML: line {line}: ")),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        refusal("x = "),
        ConfigError::Syntax { line: 1, .. }
    ));
}

/// B8: the format grows only through an explicit step. Today an unknown key is refused in every
/// table, so a later release that adds a key must also add a schema value (`schema = 2`) that
/// an older daemon refuses by name, never a key an older daemon would silently ignore. The
/// daemon config has no `schema` key yet, so `schema` itself is an unknown key.
#[test]
fn config_files_accept_a_future_additive_key_only_through_a_new_schema_value() {
    let future = format!("min_free_bytes = 1\n{EXAMPLE}");
    assert_eq!(unknown_of(refusal(&future)), unknown("", "min_free_bytes"));
    let future = with(
        "force_start_secs       = 60",
        "force_start_secs = 60\nmin_free_bytes = 1",
    );
    assert_eq!(
        unknown_of(refusal(&future)),
        unknown("bounds", "min_free_bytes")
    );
    let schema = format!("schema = 2\n{EXAMPLE}");
    assert_eq!(unknown_of(refusal(&schema)), unknown("", "schema"));
}

/// Catches a file the host does not trust being read: owned by another account.
#[test]
fn file_not_owned_by_trusted_uid_is_refused() {
    let scratch = Scratch::new("daemon-owner");
    let file = scratch.file("hostkeeper.toml", EXAMPLE, 0o644);
    let config = DaemonConfig::load(&file, my_uid()).expect("the owner's file loads");
    assert_eq!(config, DaemonConfig::parse(EXAMPLE).unwrap());
    match DaemonConfig::load(&file, my_uid() + 1) {
        Err(ConfigError::Untrusted(TrustError::Owner { found, wanted })) => {
            assert_eq!((found, wanted), (my_uid(), my_uid() + 1));
        }
        other => panic!("{other:?}"),
    }
}

/// Catches a file the group can write being read.
#[test]
fn group_writable_file_is_refused() {
    let scratch = Scratch::new("daemon-group");
    let file = scratch.file("hostkeeper.toml", EXAMPLE, 0o664);
    assert!(matches!(
        DaemonConfig::load(&file, my_uid()),
        Err(ConfigError::Untrusted(TrustError::Writable { mode: 0o664 }))
    ));
}

/// Catches a file others can write being read.
#[test]
fn other_writable_file_is_refused() {
    let scratch = Scratch::new("daemon-other");
    let file = scratch.file("hostkeeper.toml", EXAMPLE, 0o646);
    let error = DaemonConfig::load(&file, my_uid()).expect_err("refused");
    assert!(
        error.to_string().contains("not a file the host trusts"),
        "{error}"
    );
    assert!(matches!(
        error,
        ConfigError::Untrusted(TrustError::Writable { mode: 0o646 })
    ));
}

/// Catches a link being followed to a file that would itself be trusted.
#[test]
fn symlinked_file_is_refused() {
    let scratch = Scratch::new("daemon-link");
    let file = scratch.file("real.toml", EXAMPLE, 0o644);
    let link = scratch.path().join("hostkeeper.toml");
    symlink(&file, &link).unwrap();
    assert!(matches!(
        DaemonConfig::load(&link, my_uid()),
        Err(ConfigError::Untrusted(TrustError::Io(_)))
    ));
}

/// Catches a named pipe being waited on: the load returns at once, refused.
#[test]
fn fifo_is_refused_without_blocking() {
    let scratch = Scratch::new("daemon-fifo");
    let pipe = scratch.path().join("hostkeeper.toml");
    fifo(&pipe);
    assert!(matches!(
        DaemonConfig::load(&pipe, my_uid()),
        Err(ConfigError::Untrusted(TrustError::NotRegular))
    ));
}

/// Catches bytes that are not text being read as a config, and a missing file reported as
/// anything but untrusted.
#[test]
fn a_file_that_is_not_text_or_is_missing_is_refused() {
    let scratch = Scratch::new("daemon-bytes");
    let file = scratch.path().join("hostkeeper.toml");
    std::fs::write(&file, b"coordinator = \"\xff\"\n").unwrap();
    std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let error = DaemonConfig::load(&file, my_uid()).expect_err("refused");
    assert!(matches!(error, ConfigError::Unreadable), "{error}");
    assert!(error.to_string().contains("UTF-8"));
    assert!(matches!(
        DaemonConfig::load(&scratch.path().join("missing.toml"), my_uid()),
        Err(ConfigError::Untrusted(TrustError::Io(_)))
    ));
}

/// The glob table: `*` stays within one component, matches any run including none, backtracks,
/// and everything else is literal.
/// Catches a `*` that crosses a `/`, a missing backtrack and a leftover pattern accepted.
#[test]
fn ref_matches_follows_the_glob_rules() {
    let table = [
        ("refs/tags/*", "refs/tags/v1.2.3", true),
        ("refs/tags/*", "refs/tags", false),
        ("refs/tags/*", "refs/tags/a/b", false),
        ("refs/tags/*", "refs/heads/main", false),
        ("refs/tags/v*.3", "refs/tags/v1.2.3", true),
        ("refs/tags/v*.3", "refs/tags/v1.2.4", false),
        ("refs/heads/main", "refs/heads/main", true),
        ("refs/heads/main", "refs/heads/mainx", false),
        ("a", "b", false),
        ("a", "", false),
        ("ab", "a", false),
        ("a", "ab", false),
        ("", "", true),
        ("*", "", true),
        ("*a", "b", false),
        ("a*", "a", true),
        ("**", "x", true),
        ("a*b*c", "axxbyyc", true),
        ("a*b*c", "axxbyy", false),
        ("*.yml", "x.yml.yml", true),
        ("refs/*/x", "refs/y/x", true),
        ("refs/*", "refs/", true),
        ("refs//x", "refs//x", true),
    ];
    for (pattern, text, want) in table {
        assert_eq!(ref_matches(pattern, text), want, "{pattern} against {text}");
    }
}

/// A signer allows a ref when any one of its globs matches it, and no other.
/// Catches a check of the first glob only, or of all of them at once.
#[test]
fn a_signer_allows_a_ref_any_of_its_globs_matches() {
    let signers = TrustedSigners {
        organisation: "komira-ai".to_owned(),
        workflow: ".github/workflows/release.yml".to_owned(),
        refs: vec!["refs/heads/main".to_owned(), "refs/tags/v*".to_owned()],
        hosted_runner: true,
    };
    assert!(signers.allows_ref("refs/heads/main"));
    assert!(signers.allows_ref("refs/tags/v1.2.3"));
    assert!(!signers.allows_ref("refs/heads/dev"));
    assert!(!signers.allows_ref("refs/tags/1.2.3"));
}
