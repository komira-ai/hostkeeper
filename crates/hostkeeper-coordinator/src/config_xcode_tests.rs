use clap::Parser;
use clap::error::ErrorKind;

use super::*;

#[derive(Debug, Parser)]
struct Flags {
    #[command(flatten)]
    xcode: XcodeFlags,
}

/// Proves: with no `--xcode-feed-url`, or an empty one, the flag is the empty text and the list
/// has no address, so the job is off.
///
/// Catches: a default address that makes every coordinator read a third party.
#[test]
fn without_the_flag_or_with_an_empty_one_the_job_is_off() {
    for args in [vec!["serve"], vec!["serve", "--xcode-feed-url="]] {
        let flags = Flags::try_parse_from(args.iter().copied()).expect("the flags parse");
        assert_eq!(flags.xcode.xcode_feed_url, "", "{args:?}");
        assert_eq!(flags.xcode.feed(), None, "{args:?}");
    }
}

/// Proves: an `https://` URL of a file is the address of the list, as its base and its file.
///
/// Catches: an address split at the wrong slash.
#[test]
fn an_https_url_of_a_file_is_the_address_of_the_list() {
    let flags = Flags::try_parse_from([
        "serve",
        "--xcode-feed-url=https://xcodereleases.com/data.json",
    ])
    .expect("the flags parse");
    let feed = flags.xcode.feed().expect("an address");
    assert_eq!(feed.base(), "https://xcodereleases.com");
    assert_eq!(feed.file(), "data.json");
}

/// Proves: a URL that is not `https://`, or that names no file, is refused by the flag, with the
/// flag's name and the reason in the refusal.
///
/// Catches: a plain HTTP address that is read, and an address with no file.
#[test]
fn a_url_that_is_not_https_or_names_no_file_is_refused() {
    for text in [
        "http://xcodereleases.com/data.json",
        "xcodereleases.com/data.json",
        "https://xcodereleases.com",
        "https://xcodereleases.com/",
    ] {
        let refused = Flags::try_parse_from(["serve", "--xcode-feed-url", text])
            .expect_err("the address is refused");
        assert_eq!(refused.kind(), ErrorKind::ValueValidation, "{text}");
        assert!(
            refused.to_string().contains("--xcode-feed-url"),
            "{refused}"
        );
        assert!(refused.to_string().contains(text), "{refused}");
    }
}

/// Proves: without `--xcode-dir` there is no directory (so no store), and with it the flag is the
/// path given.
///
/// Catches: a default directory that opens a store on every coordinator.
#[test]
fn the_store_is_opened_only_where_a_directory_is_given() {
    let flags = Flags::try_parse_from(["serve"]).expect("the flags parse");
    assert_eq!(flags.xcode.xcode_dir, None);
    let flags =
        Flags::try_parse_from(["serve", "--xcode-dir", "/srv/xcode"]).expect("the flags parse");
    assert_eq!(flags.xcode.xcode_dir, Some(PathBuf::from("/srv/xcode")));
}

/// Proves: `--xcode-max-bytes` is the store's default (20 GiB) when it is not given, a value
/// given is that value, and 0, which would refuse every archive, is refused with the flag's name.
///
/// Catches: a default that differs from the store's, and a limit of 0 that starts.
#[test]
fn the_most_bytes_of_an_archive_defaults_to_the_stores_and_is_at_least_one() {
    let flags = Flags::try_parse_from(["serve"]).expect("the flags parse");
    assert_eq!(flags.xcode.xcode_max_bytes, DEFAULT_MAX_BYTES);
    let flags =
        Flags::try_parse_from(["serve", "--xcode-max-bytes", "1"]).expect("the flags parse");
    assert_eq!(flags.xcode.xcode_max_bytes, 1);
    let flags =
        Flags::try_parse_from(["serve", "--xcode-max-bytes=64424509440"]).expect("the flags parse");
    assert_eq!(flags.xcode.xcode_max_bytes, 64_424_509_440);
    for text in ["0", "-1", "many"] {
        let refused = Flags::try_parse_from(["serve", &format!("--xcode-max-bytes={text}")])
            .expect_err("the limit is refused");
        assert_eq!(refused.kind(), ErrorKind::ValueValidation, "{text}");
        assert!(
            refused.to_string().contains("--xcode-max-bytes"),
            "{refused}"
        );
    }
}
