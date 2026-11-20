use clap::Parser;
use clap::error::ErrorKind;

use super::*;

#[derive(Debug, Parser)]
struct Flags {
    #[command(flatten)]
    maintenance: MaintenanceFlags,
}

/// Proves: with no `--macos-update-via` the way is `daemon`, and `daemon` given is `daemon`.
///
/// Catches: a default other than the one way this build can apply a Mac's update.
#[test]
fn without_the_flag_or_with_daemon_the_way_is_the_daemon() {
    let cases: [&[&str]; 2] = [&["serve"], &["serve", "--macos-update-via", "daemon"]];
    for args in cases {
        let flags = Flags::try_parse_from(args.iter().copied()).expect("the flags parse");
        assert_eq!(
            flags.maintenance.macos_update_via,
            MacosUpdateVia::Daemon,
            "{args:?}"
        );
    }
    assert_eq!(MacosUpdateVia::default(), MacosUpdateVia::Daemon);
}

/// Proves: `apple-business` is refused by the flag as a value that fails validation, and the
/// refusal names the flag, the value and the reason (no call installs an update on one Mac).
///
/// Catches: a way that is accepted at start and then never runs, or a refusal that does not say
/// why.
#[test]
fn apple_business_is_refused_naming_the_flag_and_the_reason() {
    let refused = Flags::try_parse_from(["serve", "--macos-update-via", APPLE_BUSINESS])
        .expect_err("apple-business is refused");
    assert_eq!(refused.kind(), ErrorKind::ValueValidation);
    let text = refused.to_string();
    assert!(text.contains("--macos-update-via"), "{text}");
    assert!(text.contains("'apple-business'"), "{text}");
    assert!(
        text.contains(
            "apple-business is not built: Apple Business offers no call that installs an update \
             on one Mac; only daemon is taken"
        ),
        "{text}"
    );
}

/// Proves: any other value, the empty one and one the design does not name included, is refused
/// by the flag with the reason `only daemon is taken`.
///
/// Catches: a value that falls through to the default way.
#[test]
fn any_other_value_is_refused() {
    for text in ["mdm", "", "Daemon", "daemon "] {
        let refused = Flags::try_parse_from(["serve", "--macos-update-via", text])
            .expect_err("the value is refused");
        assert_eq!(refused.kind(), ErrorKind::ValueValidation, "{text:?}");
        let shown = refused.to_string();
        assert!(shown.contains("--macos-update-via"), "{shown}");
        assert!(shown.contains("only daemon is taken"), "{shown}");
        assert!(!shown.contains("is not built"), "{shown}");
    }
}
