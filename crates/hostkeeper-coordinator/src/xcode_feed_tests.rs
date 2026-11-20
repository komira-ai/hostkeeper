//! The list of Xcode releases: its parse over the real list's newest entries and over entries
//! each broken in one way, the order of versions, the release the alerts wait for, the address of
//! the list, and the in-memory and failing fakes of the list kept and of the Xcode store. The
//! coordinator's read and its job are tested over a fake origin elsewhere.

use std::cmp::Ordering;
use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    Channel, FeedError, FeedUrl, FeedUrlError, KeptRelease, KnownReleases, MemoryReleases,
    NoXcodeStore, Xcode, XcodeHeld, XcodeId, XcodeRelease, XcodeVersion, parse, waiting,
    waiting_message,
};

/// A copy of the 12 newest entries of the Xcode Releases site's `data.json`.
const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/xcode/data.json");

/// The version `text`.
fn version(text: &str) -> XcodeVersion {
    XcodeVersion::parse(text).expect("a version in a test")
}

/// The Xcode `version` of `build`.
fn id(number: &str, build: &str) -> XcodeId {
    XcodeId {
        version: version(number),
        build: build.to_owned(),
    }
}

/// A release of `channel` requiring macOS 26.6.
fn release(number: &str, build: &str, channel: Channel) -> XcodeRelease {
    XcodeRelease {
        id: id(number, build),
        channel,
        requires_macos: "26.6".to_owned(),
    }
}

/// An entry of the list with the given fields.
fn entry(number: &str, build: &str, kind: &str, requires: &str) -> Value {
    json!({
        "version": {"number": number, "build": build, "release": {(kind): true}},
        "requires": requires,
        "links": {"download": {"url": "https://example.invalid/Xcode.xip"}},
    })
}

/// The body of a list of `entries`.
fn body(entries: &[Value]) -> Vec<u8> {
    serde_json::to_vec(entries).expect("a list of entries as JSON")
}

/// Proves: the real list's 12 newest entries parse to exactly those 12 releases, newest version
/// first and, within one version, the higher build first and the candidate before the release of
/// one build, each with its version, build, channel and macOS, and none skipped; the type holds
/// no link, so the equality with these values is all there is to see.
///
/// Catches: an entry lost or doubled, a release type read as another, a field taken from the wrong
/// place (the SDK's version for the Xcode's), an order that is not newest first.
#[test]
fn the_fixture_parses_to_its_twelve_releases_newest_first() {
    let parsed = parse(FIXTURE).expect("the fixture parses");
    let expected = vec![
        release("27.3", "27C5035i", Channel::Beta),
        release("27.3", "27C5020n", Channel::Beta),
        release("27.2", "27B52", Channel::Release),
        release("27.2", "27B5038j", Channel::Beta),
        release("27.2", "27B5028f", Channel::Beta),
        release("27.2", "27B5019j", Channel::Beta),
        release("27.2", "27B48", Channel::ReleaseCandidate),
        release("27.1", "27A9275", Channel::ReleaseCandidate),
        release("27.1", "27A9275", Channel::Release),
        release("27.1", "27A9269", Channel::Beta),
        release("27.0", "27A266a", Channel::ReleaseCandidate),
        release("27.0", "27A266a", Channel::Release),
    ];
    assert_eq!(parsed.releases, expected);
    assert_eq!(parsed.skipped, 0);
}

/// Proves: the fixture's newest release is 27.2 (27B52), not the newer 27.3 betas nor the 27.2
/// release candidate, and it requires macOS 26.6.
///
/// Catches: a beta or a candidate named as the release the store lacks.
#[test]
fn the_fixture_waits_for_27_2() {
    let parsed = parse(FIXTURE).expect("the fixture parses");
    let newest = waiting(&parsed.releases, &[]).expect("a release waits");
    assert_eq!(newest.id, id("27.2", "27B52"));
    assert_eq!(newest.channel, Channel::Release);
    assert_eq!(newest.requires_macos, "26.6");
}

/// Proves: the list's other release types map as documented: `dp` is a beta, `gm` and `gmSeed`
/// are release candidates, and the spaces around a field are dropped.
///
/// Catches: a developer preview or a golden master taken for a release, which would raise the
/// alert for a build nobody can install.
#[test]
fn the_other_release_types_are_betas_and_candidates() {
    let parsed = parse(&body(&[
        entry("9.0", "9A1", "dp", "10.0"),
        entry("8.0", "8A1", "gm", "10.0"),
        entry("7.0", "7A1", "gmSeed", "10.0"),
        entry(" 6.0 ", " 6A1 ", "release", " 10.0 "),
        entry("5.0", "5A1", "beta", "10.0"),
        entry("4.0", "4A1", "rc", "10.0"),
    ]))
    .expect("the list parses");
    let seen: Vec<(String, Channel, String)> = parsed
        .releases
        .iter()
        .map(|r| (r.id.build.clone(), r.channel, r.requires_macos.clone()))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("9A1".to_owned(), Channel::Beta, "10.0".to_owned()),
            (
                "8A1".to_owned(),
                Channel::ReleaseCandidate,
                "10.0".to_owned()
            ),
            (
                "7A1".to_owned(),
                Channel::ReleaseCandidate,
                "10.0".to_owned()
            ),
            ("6A1".to_owned(), Channel::Release, "10.0".to_owned()),
            ("5A1".to_owned(), Channel::Beta, "10.0".to_owned()),
            (
                "4A1".to_owned(),
                Channel::ReleaseCandidate,
                "10.0".to_owned()
            ),
        ]
    );
    assert_eq!(parsed.skipped, 0);
}

/// Proves: an entry with an unknown release type, with two keys in `release`, with none, with a
/// field missing, with a version that is no number, with a build that holds a space, is empty or
/// is over 32 bytes, with a macOS that is empty or holds a space, or whose `version` is not an
/// object is skipped and counted, and the one good entry beside them is kept.
///
/// Catches: one odd entry making the whole list unreadable, or an odd entry read as a release
/// (a second release type taken for the first, a build with a space reaching the alert's text).
#[test]
fn an_entry_that_cannot_be_read_is_skipped_and_counted() {
    let long = "A".repeat(33);
    let mut broken = vec![
        entry("27.9", "27X1", "nightly", "26.6"),
        entry("27.9", "27X2", "release", "26.6 beta"),
        entry("27.9", "27X3", "release", ""),
        entry("27.9", "27X 4", "release", "26.6"),
        entry("27.9", "", "release", "26.6"),
        entry("27.9", &long, "release", "26.6"),
        entry("27.x", "27X7", "release", "26.6"),
        entry("27.", "27X8", "release", "26.6"),
    ];
    let mut two_keys = entry("27.9", "27X9", "release", "26.6");
    two_keys["version"]["release"] = json!({"release": true, "beta": 1});
    broken.push(two_keys);
    let mut no_key = entry("27.9", "27Y1", "release", "26.6");
    no_key["version"]["release"] = json!({});
    broken.push(no_key);
    for field in ["number", "build", "release"] {
        let mut missing = entry("27.9", "27Y2", "release", "26.6");
        missing["version"]
            .as_object_mut()
            .expect("an object")
            .remove(field);
        broken.push(missing);
    }
    let mut no_requires = entry("27.9", "27Y3", "release", "26.6");
    no_requires
        .as_object_mut()
        .expect("an object")
        .remove("requires");
    broken.push(no_requires);
    let mut no_version = entry("27.9", "27Y4", "release", "26.6");
    no_version
        .as_object_mut()
        .expect("an object")
        .remove("version");
    broken.push(no_version);
    let mut not_object = entry("27.9", "27Y5", "release", "26.6");
    not_object["version"] = json!("27.9");
    broken.push(not_object);
    let count = broken.len();
    let mut entries = broken;
    entries.push(entry("27.2", "27B52", "release", "26.6"));
    let parsed = parse(&body(&entries)).expect("the list parses");
    assert_eq!(
        parsed.releases,
        vec![release("27.2", "27B52", Channel::Release)]
    );
    assert_eq!(parsed.skipped, count);
}

/// Proves: an entry written twice is kept once, and the second is counted as skipped.
///
/// Catches: a release in the kept list twice.
#[test]
fn a_duplicated_entry_is_kept_once() {
    let one = entry("27.2", "27B52", "release", "26.6");
    let parsed = parse(&body(&[one.clone(), one])).expect("the list parses");
    assert_eq!(
        parsed.releases,
        vec![release("27.2", "27B52", Channel::Release)]
    );
    assert_eq!(parsed.skipped, 1);
}

/// Proves: a body that is not JSON, a JSON object, a JSON array of non-objects and an empty body
/// are `NotList`; an empty array and an array whose entries are all unreadable are `Empty`.
///
/// Catches: a page of HTML (a proxy's error page) read as an empty list that clears what was kept.
#[test]
fn a_body_that_is_no_list_is_refused() {
    for text in [&b"<html>no</html>"[..], b"{\"a\": 1}", b"[1, 2]", b""] {
        assert!(
            matches!(parse(text), Err(FeedError::NotList(_))),
            "{:?}",
            String::from_utf8_lossy(text)
        );
    }
    assert_eq!(parse(b"[]"), Err(FeedError::Empty));
    assert_eq!(parse(b"[{}, {\"version\": 1}]"), Err(FeedError::Empty));
    assert_eq!(
        parse(&body(&[entry("27.0", "27A1", "nightly", "26.6")])),
        Err(FeedError::Empty)
    );
}

/// Proves: each error says what it is, with the reason it carries.
///
/// Catches: a log line that does not say why the list was not used.
#[test]
fn feed_errors_say_what_they_are() {
    assert_eq!(
        FeedError::NotList("bad".to_owned()).to_string(),
        "the list is not a JSON array of releases: bad"
    );
    assert_eq!(
        FeedError::Empty.to_string(),
        "the list holds no entry with a version, a build, a release type and a macOS"
    );
    assert_eq!(
        FeedError::Read("down".to_owned()).to_string(),
        "the list could not be read: down"
    );
    assert_eq!(
        FeedError::Keep("full".to_owned()).to_string(),
        "the list could not be kept: full"
    );
}

/// Proves: versions are ordered by their numbers (27.10 after 27.9, 27.0.1 after 27), a missing
/// number counts as 0, two spellings of one number are ordered by their text and are not equal,
/// and one spelling equals itself.
///
/// Catches: a text order that puts 27.10 before 27.9, so a newer release is never waited for.
#[test]
fn versions_are_ordered_by_their_numbers() {
    assert!(version("27.10") > version("27.9"));
    assert!(version("27.0.1") > version("27"));
    assert!(version("16.4.1") < version("17"));
    assert_eq!(version("27").cmp(&version("27.0")), Ordering::Less);
    assert_eq!(version("27.0").cmp(&version("27")), Ordering::Greater);
    assert_ne!(version("27"), version("27.0"));
    assert_eq!(version("27.2").cmp(&version("27.2")), Ordering::Equal);
    assert_eq!(
        version("27.2").partial_cmp(&version("27.3")),
        Some(Ordering::Less)
    );
}

/// Proves: a version keeps its text, which is also how it is displayed.
///
/// Catches: a version shown or stored as its numbers (`27.0` stored for `27`).
#[test]
fn a_version_keeps_its_text() {
    assert_eq!(version("16.4.1").as_str(), "16.4.1");
    assert_eq!(version("27").to_string(), "27");
    assert_eq!(version("16.4.1.1000").to_string(), "16.4.1.1000");
}

/// Proves: a version that is empty, ends in a dot, has a part that is not decimal digits (letters,
/// a sign, a space), has five parts or a part over `u32` is refused, naming the text.
///
/// Catches: a version a store could not order read from the list.
#[test]
fn a_bad_version_is_refused() {
    for text in [
        "",
        "27.",
        ".27",
        "a.b",
        "+5",
        "-1",
        "2 7",
        "1.2.3.4.5",
        "1234567890123",
        "4294967296",
    ] {
        let why = XcodeVersion::parse(text).expect_err(text);
        assert_eq!(
            why.to_string(),
            format!("{text:?} is not a version number: dot-separated decimal numbers")
        );
    }
    assert!(XcodeVersion::parse("4294967295").is_ok());
    assert!(XcodeVersion::parse("16.4.1.1000").is_ok());
}

/// Proves: a channel has the token `release`, `beta` or `rc`, and the store's token is read back to
/// the same channel; any other token is none.
///
/// Catches: a token written by one build that another reads as nothing.
#[test]
fn a_channel_round_trips_through_its_token() {
    for (channel, token) in [
        (Channel::Release, "release"),
        (Channel::Beta, "beta"),
        (Channel::ReleaseCandidate, "rc"),
    ] {
        assert_eq!(channel.as_str(), token);
        assert_eq!(Channel::of_token(token), Some(channel));
    }
    assert_eq!(Channel::of_token("gm"), None);
    assert_eq!(Channel::of_token(""), None);
    assert_eq!(Channel::of_token("Release"), None);
}

/// A list of known releases for the tests of [`waiting`]: the fixture's.
fn fixture_releases() -> Vec<XcodeRelease> {
    parse(FIXTURE).expect("the fixture parses").releases
}

/// Proves: with nothing held the release waited for is the newest release, 27.2 (27B52), and a
/// store that holds 27.2 (27B52) waits for nothing.
///
/// Catches: an alert that never clears once the store holds the release, or one for a beta.
#[test]
fn nothing_waits_once_the_store_holds_the_newest_release() {
    let known = fixture_releases();
    assert_eq!(
        waiting(&known, &[]).map(|r| &r.id),
        Some(&id("27.2", "27B52"))
    );
    assert_eq!(waiting(&known, &[id("27.2", "27B52")]), None);
}

/// Proves: a store that holds only an older release (27.1) still waits for 27.2; one that holds a
/// newer one (27.3, a beta pushed by hand) waits for nothing; one that holds the same version at a
/// later build waits for nothing; one that holds the same version at an earlier build still waits.
///
/// Catches: an alert raised for a release the store already passed, or one missed for a store
/// that is behind.
#[test]
fn waiting_compares_with_every_held_xcode() {
    let known = fixture_releases();
    assert_eq!(
        waiting(&known, &[id("27.1", "27A9275"), id("27.0", "27A266a")]).map(|r| &r.id),
        Some(&id("27.2", "27B52"))
    );
    assert_eq!(waiting(&known, &[id("27.3", "27C5035i")]), None);
    assert_eq!(waiting(&known, &[id("27.2", "27B99")]), None);
    assert_eq!(
        waiting(&known, &[id("27.2", "27B48")]).map(|r| &r.id),
        Some(&id("27.2", "27B52"))
    );
    assert_eq!(
        waiting(&known, &[id("27.1", "27A1"), id("27.9", "27Z1")]),
        None
    );
}

/// Proves: a list of betas and candidates alone, and an empty list, wait for nothing, whatever
/// the store holds.
///
/// Catches: a first beta of a new Xcode raising the alert before there is a release to push.
#[test]
fn only_betas_and_candidates_wait_for_nothing() {
    let known = vec![
        release("28.0", "28A1", Channel::Beta),
        release("28.0", "28A2", Channel::ReleaseCandidate),
    ];
    assert_eq!(waiting(&known, &[]), None);
    assert_eq!(waiting(&[], &[]), None);
}

/// Proves: the alert's text names the version and build, the macOS it requires and the command that
/// pushes it, exactly.
///
/// Catches: an alert that does not say what to do.
#[test]
fn the_waiting_message_names_the_release_and_the_command() {
    assert_eq!(
        waiting_message(&release("27.2", "27B52", Channel::Release)),
        "Xcode 27.2 (27B52), which requires macOS 26.6 or later, is released and the \
         coordinator's Xcode store holds no release as new: push it from a Mac signed in to the \
         Apple developer account with `hk xcode-push 27.2`"
    );
}

/// Proves: an address of a file is split into the base the origin client reads under and the
/// file: at a host's root, in a nested path, with a port, and with a trailing path of one part.
///
/// Catches: a base that keeps the file, or a file that keeps part of the path.
#[test]
fn a_feed_url_is_a_base_and_a_file() {
    let cases = [
        (
            "https://xcodereleases.com/data.json",
            "https://xcodereleases.com",
            "data.json",
        ),
        (
            "https://mirror.example/feeds/xcode/data.json",
            "https://mirror.example/feeds/xcode",
            "data.json",
        ),
        (
            "https://mirror.example:8443/data.json",
            "https://mirror.example:8443",
            "data.json",
        ),
    ];
    for (text, base, file) in cases {
        let url = FeedUrl::parse(text).expect(text);
        assert_eq!(url.base(), base);
        assert_eq!(url.file(), file);
        assert_eq!(url.to_string(), text);
    }
}

/// Proves: `http://`, a query, a user, a fragment, a text that is no URL and an address with no
/// file (`https://h.example/`, `https://h.example`) are each refused with the error that names it.
///
/// Catches: a list read over plain HTTP, or an address whose file was lost, which would read the
/// host's root page.
#[test]
fn a_feed_url_that_is_not_an_https_file_is_refused() {
    assert!(matches!(
        FeedUrl::parse("http://h.example/data.json"),
        Err(FeedUrlError::Url(_))
    ));
    assert!(matches!(
        FeedUrl::parse("https://h.example/data.json?x=1"),
        Err(FeedUrlError::Url(_))
    ));
    assert!(matches!(
        FeedUrl::parse("https://user@h.example/data.json"),
        Err(FeedUrlError::Url(_))
    ));
    assert!(matches!(
        FeedUrl::parse("not a url"),
        Err(FeedUrlError::Url(_))
    ));
    assert_eq!(
        FeedUrl::parse("https://h.example/"),
        Err(FeedUrlError::NoFile("https://h.example/".to_owned()))
    );
    assert_eq!(
        FeedUrl::parse("https://h.example"),
        Err(FeedUrlError::NoFile("https://h.example".to_owned()))
    );
    assert_eq!(
        FeedUrl::parse("https://h.example/a/").map(|u| u.to_string()),
        Err(FeedUrlError::NoFile("https://h.example/a/".to_owned()))
    );
    assert_eq!(
        FeedUrl::parse("https://h.example")
            .expect_err("no file")
            .to_string(),
        "\"https://h.example\" names no file: the list's address ends with its file's name"
    );
}

/// Proves: the list in memory is empty at first, holds what `keep` gave it whole (a second keep
/// replaces the first), and a poisoned lock does not stop it.
///
/// Catches: a second read that adds to the first instead of replacing it, so a release the
/// list dropped is still waited for.
#[test]
fn the_memory_list_is_replaced_whole() {
    let memory = MemoryReleases::default();
    assert_eq!(memory.known(), Ok(Vec::new()));
    let first = vec![
        release("27.2", "27B52", Channel::Release),
        release("27.1", "27A9275", Channel::Release),
    ];
    memory.keep(&first).expect("kept");
    assert_eq!(memory.known(), Ok(first));
    let second = vec![release("27.3", "27C1", Channel::Release)];
    memory.keep(&second).expect("kept");
    assert_eq!(memory.known(), Ok(second.clone()));
    let poisoned = Arc::new(MemoryReleases::default());
    let holder = Arc::clone(&poisoned);
    let _ = std::thread::spawn(move || {
        let _guard = holder.0.lock();
        panic!("a thread dies holding the lock");
    })
    .join();
    poisoned.keep(&second).expect("kept after a poisoned lock");
    assert_eq!(poisoned.known(), Ok(second));
}

/// Proves: a coordinator with no Xcode store holds no release.
///
/// Catches: a store that holds something before anything was pushed.
#[test]
fn no_store_holds_nothing() {
    assert_eq!(NoXcodeStore.held(), Ok(Vec::new()));
}

/// A list of known releases that cannot be read.
struct UnreadableKnown;

impl KnownReleases for UnreadableKnown {
    fn keep(&self, _: &[XcodeRelease]) -> Result<(), String> {
        Ok(())
    }

    fn known(&self) -> Result<Vec<XcodeRelease>, String> {
        Err("the list is gone".to_owned())
    }
}

/// An Xcode store that holds `held`, or cannot be read.
struct FakeHeld(Result<Vec<XcodeId>, String>);

impl XcodeHeld for FakeHeld {
    fn held(&self) -> Result<Vec<XcodeId>, String> {
        self.0.clone()
    }
}

/// An Xcode holding `known` and `held`.
fn xcode(known: Vec<XcodeRelease>, held: Result<Vec<XcodeId>, String>) -> Xcode {
    let memory = MemoryReleases::default();
    memory.keep(&known).expect("kept");
    Xcode {
        feed: None,
        known: Arc::new(memory),
        held: Arc::new(FakeHeld(held)),
    }
}

/// Proves: the release the alert names is the newest release of the list kept when the store holds
/// none as new, none once the store holds it, and none with no list; a list or a store that cannot
/// be read is none (logged), never a release and never a panic.
///
/// Catches: an alert raised, or cleared, because a store was down for a moment.
#[test]
fn release_waiting_judges_the_list_against_the_store() {
    let newest = release("27.2", "27B52", Channel::Release);
    let known = vec![newest.clone(), release("27.1", "27A9275", Channel::Release)];
    assert_eq!(
        xcode(known.clone(), Ok(Vec::new())).release_waiting(),
        Some(newest)
    );
    assert_eq!(
        xcode(known, Ok(vec![id("27.2", "27B52")])).release_waiting(),
        None
    );
    assert_eq!(xcode(Vec::new(), Ok(Vec::new())).release_waiting(), None);
    assert_eq!(Xcode::default().release_waiting(), None);
    assert_eq!(
        xcode(
            vec![release("27.2", "27B52", Channel::Release)],
            Err("the store is down".to_owned())
        )
        .release_waiting(),
        None
    );
    let unreadable = Xcode {
        feed: None,
        known: Arc::new(UnreadableKnown),
        held: Arc::new(NoXcodeStore),
    };
    assert_eq!(unreadable.release_waiting(), None);
}

/// Proves: a default Xcode reads no list, and its debug text shows the address and nothing of the
/// stores.
///
/// Catches: a coordinator that reads the list without being told where it is.
#[test]
fn the_default_xcode_has_no_feed() {
    let xcode = Xcode::default();
    assert_eq!(xcode.feed, None);
    assert_eq!(format!("{xcode:?}"), "Xcode { feed: None, .. }");
}

/// Proves: a release as the stores keep it (four texts) reads back to the same release, for each
/// channel.
///
/// Catches: a field lost between the store and the alert.
#[test]
fn a_kept_release_round_trips() {
    for channel in [Channel::Release, Channel::Beta, Channel::ReleaseCandidate] {
        let original = release("27.2", "27B52", channel);
        let kept = KeptRelease::from(&original);
        assert_eq!(kept.version, "27.2");
        assert_eq!(kept.build, "27B52");
        assert_eq!(kept.channel, channel.as_str());
        assert_eq!(kept.requires_macos, "26.6");
        assert_eq!(kept.release(), Ok(original));
    }
}

/// Proves: a kept release whose version is not a number, or whose channel is not one of the three
/// tokens, is an error that names it, and is not turned into a release.
///
/// Catches: a row a newer build wrote read by this one as a release of some other kind.
#[test]
fn a_kept_release_that_cannot_be_read_is_an_error() {
    let good = KeptRelease::from(&release("27.2", "27B52", Channel::Release));
    let bad_version = KeptRelease {
        version: "27.".to_owned(),
        ..good.clone()
    };
    assert_eq!(
        bad_version.release(),
        Err("\"27.\" is not a version number: dot-separated decimal numbers".to_owned())
    );
    let bad_channel = KeptRelease {
        channel: "nightly".to_owned(),
        ..good
    };
    assert_eq!(
        bad_channel.release(),
        Err("\"nightly\" is not a channel of a kept release".to_owned())
    );
}
