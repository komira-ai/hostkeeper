//! The refusals of section 3.4, the gateway's null step and the message limits.
//!
//! Every refusal is called over gRPC through the listeners' router and read from its trailers; a
//! refused set must change no file of the coordinator (the state, the releases beside it, the ops
//! file and its audit), and its version must still have no release.

use std::collections::BTreeMap;

use hostkeeper_config::names::ServiceName;
use hostkeeper_proto::hostkeeper::release::v1::{Target, TargetList};
use hostkeeper_release::target;
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{
    GET_RELEASE, OPERATOR, Rig, SET_DESIRED, ci_token, gateway, grpc, proto, rig, set, set_json,
    targets,
};
use crate::support::targets::{
    Answered, digest, entry, grpc_bytes, list, location, repository_entry,
};

/// `INVALID_ARGUMENT`.
pub(super) const INVALID_ARGUMENT: i32 = 3;
/// `NOT_FOUND`.
const NOT_FOUND: i32 = 5;

type Targets = BTreeMap<String, TargetList>;

/// Every file of the coordinator's directory, by name: the state, the releases, the ops file and
/// its audit.
fn files(rig: &Rig) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(rig.dir.path())
        .expect("the directory")
        .map(|found| found.expect("an entry").path())
        .filter(|path| path.is_file())
        .map(|path| {
            let name = path
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read(&path).expect("a file"))
        })
        .collect()
}

/// `GetRelease` of `service` at `version` with the operator's token.
async fn get(rig: &Rig, service: &str, version: &str) -> Answered {
    let ask = proto::GetReleaseRequest {
        service: service.to_owned(),
        version: version.to_owned(),
    };
    grpc(rig, GET_RELEASE, Some(OPERATOR), &ask).await
}

/// Sets `request` with the operator's token.
async fn operator_sets(rig: &Rig, request: &proto::SetDesiredRequest) -> Answered {
    grpc(rig, SET_DESIRED, Some(OPERATOR), request).await
}

/// A rig that has kept `web` at 1.0 with targets, so that the state and the audit it leaves
/// are not empty.
pub(super) async fn rig_with_web(name: &str) -> Rig {
    let rig = rig(name);
    let answered = operator_sets(&rig, &set("web", "1.0", targets("web"))).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    rig
}

/// Sets `request` as CI and asserts that it is refused `(code, token)`, that no file of the
/// coordinator changed, and that `service` at `version` has no release.
pub(super) async fn refused(
    rig: &Rig,
    request: &proto::SetDesiredRequest,
    refusal: (i32, &str),
    why: &str,
) {
    let before = files(rig);
    let answered = grpc(rig, SET_DESIRED, Some(&ci_token()), request).await;
    assert_eq!(answered.refusal(), refusal, "{why}: {answered:?}");
    assert_eq!(files(rig), before, "{why}: a refused set changed a file");
    let version = request.desired.as_ref().expect("a state").version.as_str();
    let read = get(rig, &request.service, version).await;
    assert_eq!(read.refusal(), (NOT_FOUND, "release_unknown"), "{why}");
}

/// `service`'s name.
fn name(service: &str) -> ServiceName {
    ServiceName::new(service).expect("a service name")
}

/// The targets of `web` with `change` applied to the first entry of `linux-x86_64`, a `deb`.
fn with_first(change: impl FnOnce(&mut Target)) -> Targets {
    let mut all = targets("web");
    change(&mut all.get_mut("linux-x86_64").expect("a list").entries[0]);
    all
}

/// The targets of `web` with `change` applied to the list of `linux-x86_64`.
fn with_list(change: impl FnOnce(&mut Vec<Target>)) -> Targets {
    let mut all = targets("web");
    change(&mut all.get_mut("linux-x86_64").expect("a list").entries);
    all
}

/// An entry of the installer `installer` for `web`.
fn web_entry(installer: &str, c: char) -> Target {
    entry("web", installer, c, 1000)
}

/// What `hostkeeper_release::target::check` says of `all`, for `service`, as its message.
pub(super) fn judged(service: &str, all: &Targets) -> String {
    target::check(&name(service), all)
        .expect_err("the targets break a rule")
        .to_string()
}

/// A request for `service` at 2.0 with `all`.
fn request(service: &str, all: Targets) -> proto::SetDesiredRequest {
    set(service, "2.0", all)
}

/// The targets of `hostkeeper`: its native entry on each of `packages`' platforms, `apt` from a
/// repository on Linux and `macos-pkg` on macOS, of the pin made of the character.
fn self_targets(packages: &[(&str, char, u64)]) -> Targets {
    packages
        .iter()
        .map(|(platform, c, bytes)| {
            let own = if platform.starts_with("macos") {
                let mut pkg = entry("hostkeeper", "macos-pkg", *c, *bytes);
                pkg.manifest.as_mut().expect("a manifest").run = vec!["hostkeeper".to_owned()];
                pkg
            } else {
                repository_entry("hostkeeper", "2.0-1", *c)
            };
            ((*platform).to_owned(), list(vec![own]))
        })
        .collect()
}

/// The release index of `service` at 2.0 listing `packages` (platform, digest character, size),
/// in the canonical form the channel writes.
fn index_text(service: &str, packages: &[(&str, char, u64)]) -> String {
    let listed: Vec<Value> = packages
        .iter()
        .map(|(platform, c, bytes)| {
            json!({
                "bytes": bytes,
                "file": format!("{service}-2.0-{platform}.tar.zst"),
                "platform": platform,
                "sha256": digest(*c),
            })
        })
        .collect();
    json!({"packages": listed, "schema": 1, "service": service, "version": "2.0"}).to_string()
}

/// The lower-case hex SHA-256 of `text`.
fn sha256_of(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `request` with the release index `text` and its true digest.
fn with_index(mut request: proto::SetDesiredRequest, text: &str) -> proto::SetDesiredRequest {
    request.release_index = Some(text.to_owned());
    request.release_index_digest = Some(sha256_of(text));
    request
}

/// The three platforms of `hostkeeper`'s release.
const SELF_THREE: [(&str, char, u64); 3] = [
    ("linux-arm64", 'b', 2000),
    ("linux-x86_64", 'a', 1000),
    ("macos-arm64", 'c', 3000),
];

/// Proves that a set whose targets break a rule of section 3.4 is refused `target_invalid`
/// (`INVALID_ARGUMENT`) for each rule: a platform key that is none of the three, a list of no
/// entry and of nine, an installer of 33 bytes and one with an upper-case letter, two entries of
/// one installer, an artifact digest that is not 64 lower-case hex characters, a size of 0, no
/// attestation, an attestation of 0 bytes, nine locations, a locator that leaves its store, more
/// than 2 KiB of locators in an entry, an entry over 24 KiB, a manifest that breaks a rule, and
/// for `hostkeeper` a list with a tarball entry, one with two entries and one with its archive
/// entry before its native one; and of a native entry (packages-native.md, A18), a `macos-pkg`
/// entry with an attestation and an entry of `macos-pkg` or `apt` with a hook command, and a
/// boot step without a command on any other. Each refusal is the
/// rule the row names, which the check of `hostkeeper-release` states, and changes no file, adds
/// no audit entry and leaves the version without a release.
///
/// Catches: a rule of the release that the coordinator forgot to ask of the targets of a set.
#[tokio::test]
async fn each_rule_of_section_3_4_refuses_the_set_target_invalid() {
    let rig = rig_with_web("targets-rules").await;
    let long_locator = "a".repeat(1000);
    let heavy_run: Vec<String> = (0..64)
        .map(|n| format!("{n:03}{}", "u".repeat(400)))
        .collect();
    let mut over_entry = web_entry("deb", 'a');
    over_entry.manifest.as_mut().expect("a manifest").run = heavy_run;
    let over_bytes = over_entry.encoded_len();
    let nine: Vec<Target> = (1..=9).map(|n| web_entry(&format!("i{n}"), 'a')).collect();
    let mut four_platforms = targets("web");
    four_platforms.insert(
        "windows-x86_64".to_owned(),
        list(vec![web_entry("deb", 'a')]),
    );

    let rows: Vec<(&str, &str, Targets, String)> = vec![
        (
            "a fourth platform key",
            "web",
            four_platforms,
            "the targets key \"windows-x86_64\" is not a platform: one of linux-x86_64, linux-arm64, macos-arm64".to_owned(),
        ),
        (
            "a list of no entry",
            "web",
            with_list(Vec::clear),
            "the list of linux-x86_64 has 0 entries, not 1 to 8".to_owned(),
        ),
        (
            "a list of nine entries",
            "web",
            with_list(|entries| *entries = nine),
            "the list of linux-x86_64 has 9 entries, not 1 to 8".to_owned(),
        ),
        (
            "an installer of 33 bytes",
            "web",
            with_first(|e| e.installer = "a".repeat(33)),
            format!(
                "entry 0 of linux-x86_64: the installer {:?} is not 1 to 32 bytes of a-z, 0-9 and `-`",
                "a".repeat(33)
            ),
        ),
        (
            "an installer with an upper-case letter",
            "web",
            with_first(|e| e.installer = "Deb".to_owned()),
            "entry 0 of linux-x86_64: the installer \"Deb\" is not 1 to 32 bytes of a-z, 0-9 and `-`"
                .to_owned(),
        ),
        (
            "two entries of one installer",
            "web",
            with_list(|entries| entries.push(web_entry("deb", 'f'))),
            "the list of linux-x86_64 has two entries of the installer \"deb\"".to_owned(),
        ),
        (
            "an artifact digest in upper case",
            "web",
            with_first(|e| e.digest = "A".repeat(64)),
            format!(
                "entry 0 of linux-x86_64: the artifact digest {:?} is not 64 lower-case hex characters",
                "A".repeat(64)
            ),
        ),
        (
            "an artifact digest of 63 characters",
            "web",
            with_first(|e| e.digest = "a".repeat(63)),
            format!(
                "entry 0 of linux-x86_64: the artifact digest {:?} is not 64 lower-case hex characters",
                "a".repeat(63)
            ),
        ),
        (
            "an artifact of 0 bytes",
            "web",
            with_first(|e| e.bytes = 0),
            "entry 0 of linux-x86_64: the artifact's bytes is 0".to_owned(),
        ),
        (
            "no attestation",
            "web",
            with_first(|e| e.attestation = None),
            "entry 0 of linux-x86_64: the entry has no attestation".to_owned(),
        ),
        (
            "an attestation of 0 bytes",
            "web",
            with_first(|e| e.attestation.as_mut().expect("an attestation").bytes = 0),
            "entry 0 of linux-x86_64: the attestation's bytes is 0, not 1 to 1048576".to_owned(),
        ),
        (
            "nine locations",
            "web",
            with_first(|e| e.locations = vec![e.locations[0].clone(); 9]),
            "entry 0 of linux-x86_64: the artifact has 9 locations, not 1 to 8".to_owned(),
        ),
        (
            "a locator that leaves its store",
            "web",
            with_first(|e| e.locations[0].locator = "../escape".to_owned()),
            "entry 0 of linux-x86_64: the locator \"../escape\" has a `.` or `..` segment".to_owned(),
        ),
        (
            "more than 2 KiB of locators",
            "web",
            with_first(|e| {
                e.locations = vec![location(&long_locator); 3];
            }),
            "entry 0 of linux-x86_64: the entry's locators are 3056 bytes in all, more than 2048"
                .to_owned(),
        ),
        (
            "an entry over 24 KiB",
            "web",
            with_first(|e| *e = over_entry),
            format!("entry 0 of linux-x86_64: the entry is {over_bytes} bytes encoded, more than 24576"),
        ),
        (
            "a manifest that breaks a rule",
            "web",
            with_first(|e| e.manifest.as_mut().expect("a manifest").min_hostkeeper = "x".to_owned()),
            "entry 0 of linux-x86_64: min_hostkeeper \"x\" is not major.minor.patch: three whole numbers without leading zeros or a suffix".to_owned(),
        ),
        (
            "hostkeeper with a tarball entry",
            "hostkeeper",
            BTreeMap::from([(
                "linux-x86_64".to_owned(),
                list(vec![entry("hostkeeper", "tarball", 'a', 1000)]),
            )]),
            "the list of linux-x86_64 for the service hostkeeper is not one native entry"
                .to_owned(),
        ),
        (
            "hostkeeper with two entries",
            "hostkeeper",
            BTreeMap::from([(
                "linux-x86_64".to_owned(),
                list(vec![
                    entry("hostkeeper", "hostkeeper-archive", 'a', 1000),
                    entry("hostkeeper", "tarball", 'b', 1000),
                ]),
            )]),
            "the list of linux-x86_64 for the service hostkeeper is not one native entry"
                .to_owned(),
        ),
    ];
    let mac = |change: fn(&mut Target)| {
        let mut pkg = entry("web", "macos-pkg", 'a', 1000);
        pkg.manifest.as_mut().expect("a manifest").run = vec!["web".to_owned()];
        change(&mut pkg);
        BTreeMap::from([("macos-arm64".to_owned(), list(vec![pkg]))])
    };
    let native_rows: Vec<(&str, &str, Targets, String)> = vec![
        (
            "a macos-pkg entry with an attestation",
            "web",
            mac(|pkg| pkg.attestation = web_entry("deb", 'a').attestation),
            "entry 0 of macos-arm64: the macos-pkg entry has an attestation; a package's signature and the pin are its trust, and no daemon checks one".to_owned(),
        ),
        (
            "a macos-pkg entry with a hook command",
            "web",
            mac(|pkg| {
                let hook = pkg.manifest.as_mut().and_then(|m| m.hook.as_mut());
                hook.expect("a hook").command = vec!["bin/web".to_owned()];
            }),
            "entry 0 of macos-arm64: hook.command is set on a native entry; a native package's hook is its program at its fixed path".to_owned(),
        ),
        (
            "an apt entry with a hook command",
            "web",
            BTreeMap::from([(
                "linux-x86_64".to_owned(),
                list(vec![{
                    let mut apt = repository_entry("web", "2.0-1", 'a');
                    let hook = apt.manifest.as_mut().and_then(|m| m.hook.as_mut());
                    hook.expect("a hook").command = vec!["bin/web".to_owned()];
                    apt
                }]),
            )]),
            "entry 0 of linux-x86_64: hook.command is set on a native entry; a native package's hook is its program at its fixed path".to_owned(),
        ),
        (
            "a boot step without a command on a deb entry",
            "web",
            with_first(|e| {
                let hook = e.manifest.as_mut().and_then(|m| m.hook.as_mut()).expect("a hook");
                hook.command.clear();
                hook.boot = Some("restore".to_owned());
            }),
            "entry 0 of linux-x86_64: hook.boot \"restore\" needs a hook.command; the built-in steps have no boot step".to_owned(),
        ),
        (
            "hostkeeper with its archive entry after its native one",
            "hostkeeper",
            BTreeMap::from([(
                "linux-x86_64".to_owned(),
                list(vec![
                    repository_entry("hostkeeper", "2.0-1", 'b'),
                    entry("hostkeeper", "hostkeeper-archive", 'a', 1000),
                ]),
            )]),
            "the list of linux-x86_64 for the service hostkeeper is not one native entry"
                .to_owned(),
        ),
        (
            "hostkeeper with its archive entry alone",
            "hostkeeper",
            BTreeMap::from([(
                "linux-x86_64".to_owned(),
                list(vec![entry("hostkeeper", "hostkeeper-archive", 'a', 1000)]),
            )]),
            "the list of linux-x86_64 for the service hostkeeper is not one native entry"
                .to_owned(),
        ),
        (
            "hostkeeper with its archive entry before its native one",
            "hostkeeper",
            BTreeMap::from([(
                "linux-x86_64".to_owned(),
                list(vec![
                    entry("hostkeeper", "hostkeeper-archive", 'a', 1000),
                    repository_entry("hostkeeper", "2.0-1", 'b'),
                ]),
            )]),
            "the list of linux-x86_64 for the service hostkeeper is not one native entry"
                .to_owned(),
        ),
    ];
    for (why, service, broken, message) in rows.into_iter().chain(native_rows) {
        assert_eq!(judged(service, &broken), message, "{why}: the rule");
        refused(
            &rig,
            &request(service, broken),
            (INVALID_ARGUMENT, "target_invalid"),
            why,
        )
        .await;
    }
}

/// Proves the boundaries of the rules are inside them: a list of eight entries and an
/// installer of 32 bytes are kept, and read back whole.
///
/// Catches: a limit read as exclusive.
#[tokio::test]
async fn eight_entries_and_an_installer_of_32_bytes_are_accepted() {
    let rig = rig("targets-boundaries");
    let mut eight: Vec<Target> = (1..=7).map(|n| web_entry(&format!("i{n}"), 'a')).collect();
    eight.push(web_entry(&"z".repeat(32), 'b'));
    let all = BTreeMap::from([("linux-x86_64".to_owned(), list(eight))]);
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &request("web", all.clone()),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let read: proto::GetReleaseResponse = get(&rig, "web", "2.0").await.decoded();
    assert_eq!(read.targets, all);
    assert_eq!(read.release_index_digest, None);
}

/// Proves that a package digest beside targets is refused `index_invalid`, and that nothing is
/// changed by the refusal.
#[tokio::test]
async fn a_package_digest_beside_targets_is_refused_index_invalid() {
    let rig = rig_with_web("targets-package-digest").await;
    let mut beside = request("web", targets("web"));
    beside.package_digest = Some(digest('f'));
    refused(
        &rig,
        &beside,
        (INVALID_ARGUMENT, "index_invalid"),
        "a package digest",
    )
    .await;
}

/// Proves that the text of a release index set beside targets is not read: an index that does not
/// list the archive entries of the targets (one digest that differs, one size that differs, a
/// platform the targets lack, a platform the index lacks, another service's, and a text that is no
/// index at all) is accepted and kept as its digest, as an index that lists them is; and an index
/// whose digest is not the SHA-256 of its text is refused `index_digest_mismatch`, as a set
/// without targets is, with no file changed.
///
/// Catches: an index parsed or compared with the targets, which would refuse a set the release
/// job no longer has an index for, and a digest taken without being held to its text.
#[tokio::test]
async fn an_index_beside_the_targets_is_kept_as_its_digest_whatever_its_text() {
    let rig = rig_with_web("targets-index-kept").await;
    let all = self_targets(&SELF_THREE);
    let index = |packages: &[(&str, char, u64)]| index_text("hostkeeper", packages);
    let two = [SELF_THREE[0], SELF_THREE[1]];
    let mut other_digest = SELF_THREE;
    other_digest[1].1 = 'd';
    let mut other_size = SELF_THREE;
    other_size[2].2 = 3001;
    let rows = [
        ("one digest", index(&other_digest)),
        ("one size", index(&other_size)),
        ("a platform the index lacks", index(&two)),
        (
            "an index of another service",
            index_text("web", &SELF_THREE),
        ),
        ("an index that is no index", "{}".to_owned()),
        ("the index that agrees", index(&SELF_THREE)),
    ];
    for (n, (why, text)) in rows.iter().enumerate() {
        let version = format!("2.{n}");
        let asked = with_index(set("hostkeeper", &version, all.clone()), text);
        let answered = grpc(&rig, SET_DESIRED, Some(&ci_token()), &asked).await;
        assert_eq!(answered.code, 0, "{why}: {answered:?}");
        let read = get(&rig, "hostkeeper", &version).await;
        assert_eq!(read.code, 0, "{why}: {read:?}");
        let kept: proto::GetReleaseResponse = read.decoded();
        assert_eq!(kept.release_index_digest, Some(sha256_of(text)), "{why}");
    }
    // The targets lack a platform the index lists: kept all the same.
    let fewer = with_index(
        set("hostkeeper", "3.0", self_targets(&two)),
        &index(&SELF_THREE),
    );
    let answered = grpc(&rig, SET_DESIRED, Some(&ci_token()), &fewer).await;
    assert_eq!(answered.code, 0, "{answered:?}");

    // The digest of another text.
    let mut wrong = with_index(set("hostkeeper", "4.0", all), &index(&SELF_THREE));
    wrong.release_index_digest = Some(digest('0'));
    refused(
        &rig,
        &wrong,
        (INVALID_ARGUMENT, "index_digest_mismatch"),
        "a wrong digest",
    )
    .await;
}

/// Proves that an index that lists exactly the archive entries is accepted: the three
/// platforms of `hostkeeper`; two platforms with the third left out of the targets; and, for
/// another service, a list whose archive entry is its second, next to a `deb`. The release is
/// kept with the index's digest and the targets as set.
///
/// Catches: an agreement test that needs every list to be one entry, or the archive entry first.
#[tokio::test]
async fn an_index_that_agrees_with_the_archive_entries_is_accepted() {
    let rig = rig("targets-index-accepted");

    let all = self_targets(&SELF_THREE);
    let text = index_text("hostkeeper", &SELF_THREE);
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &with_index(request("hostkeeper", all.clone()), &text),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let read: proto::GetReleaseResponse = get(&rig, "hostkeeper", "2.0").await.decoded();
    assert_eq!(read.targets, all);
    assert_eq!(read.release_index_digest, Some(sha256_of(&text)));

    let two = [SELF_THREE[0], SELF_THREE[1]];
    let mut at_3 = request("hostkeeper", self_targets(&two));
    at_3.desired.as_mut().expect("a state").version = "3.0".to_owned();
    let text = index_text("hostkeeper", &two).replace("2.0", "3.0");
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &with_index(at_3, &text),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let read: proto::GetReleaseResponse = get(&rig, "hostkeeper", "3.0").await.decoded();
    assert_eq!(
        read.targets.keys().collect::<Vec<_>>(),
        ["linux-arm64", "linux-x86_64"]
    );
    assert_eq!(read.release_index_digest, Some(sha256_of(&text)));

    let mut web = targets("web");
    web.get_mut("linux-x86_64")
        .expect("a list")
        .entries
        .insert(1, entry("web", "hostkeeper-archive", 'e', 777));
    let text = index_text("web", &[("linux-x86_64", 'e', 777)]);
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &with_index(request("web", web.clone()), &text),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let read: proto::GetReleaseResponse = get(&rig, "web", "2.0").await.decoded();
    assert_eq!(read.targets, web);
    assert_eq!(read.release_index_digest, Some(sha256_of(&text)));
}

/// Proves that on the gateway a platform whose value is `null` and a platform left out give
/// one release: the two sets differ only there, and `GetRelease` answers the same targets for
/// both.
///
/// Catches: a null platform kept as an empty list, or refused as a list of no entry.
#[tokio::test]
async fn a_null_platform_and_a_platform_left_out_make_one_release() {
    let rig = rig("targets-null-step");
    let mut with_null = set_json(&set("web", "2.0", targets("web")));
    with_null["targets"]["linux-arm64"] = Value::Null;
    let (code, answer) = gateway(&rig, SET_DESIRED, Some(OPERATOR), &with_null.to_string()).await;
    assert_eq!(code, 200, "{answer}");
    let left_out = set_json(&set("web", "2.1", targets("web")));
    assert!(left_out["targets"].get("linux-arm64").is_none());
    let (code, answer) = gateway(&rig, SET_DESIRED, Some(OPERATOR), &left_out.to_string()).await;
    assert_eq!(code, 200, "{answer}");
    let nulled = get(&rig, "web", "2.0").await;
    let absent = get(&rig, "web", "2.1").await;
    assert_eq!(nulled, absent);
    let read: proto::GetReleaseResponse = nulled.decoded();
    assert_eq!(read.targets, targets("web"));
}

/// Proves that a `targets` object of nothing but `null`s and an empty one are refused
/// `target_invalid` (HTTP 400, `INVALID_ARGUMENT`) on the gateway, with nothing changed; that a
/// `null` beside an invalid list still leaves the list to be refused; and that a body that is
/// no JSON, a body with no `targets` and a `targets` that is no object pass the null step
/// unchanged, to be judged as today.
///
/// Catches: a null step that treats an empty targets map as a set without targets.
#[tokio::test]
async fn a_targets_object_that_names_no_platform_is_refused_on_the_gateway() {
    let rig = rig_with_web("targets-null-all").await;
    let target_invalid = json!({"code": "INVALID_ARGUMENT", "reason": "target_invalid"});
    let invalid_request = json!({"code": "INVALID_ARGUMENT", "reason": "invalid_request"});
    let plain = set_json(&set("web", "2.0", BTreeMap::new()));
    let before = files(&rig);
    for (why, object) in [
        (
            "all null",
            json!({"linux-x86_64": null, "macos-arm64": null}),
        ),
        ("empty", json!({})),
    ] {
        let mut body = plain.clone();
        body["targets"] = object;
        let (code, answer) = gateway(&rig, SET_DESIRED, Some(OPERATOR), &body.to_string()).await;
        assert_eq!((code, &answer), (400, &target_invalid), "{why}");
        assert_eq!(files(&rig), before, "{why}");
    }
    let mut broken = set_json(&request("web", with_first(|e| e.bytes = 0)));
    broken["targets"]["macos-arm64"] = Value::Null;
    let (code, answer) = gateway(&rig, SET_DESIRED, Some(OPERATOR), &broken.to_string()).await;
    assert_eq!((code, &answer), (400, &target_invalid));
    assert_eq!(files(&rig), before);

    let mut listed = plain.clone();
    listed["targets"] = json!([]);
    for (why, body) in [
        ("not JSON", "targets".to_owned()),
        ("targets that is no object", listed.to_string()),
    ] {
        let (code, answer) = gateway(&rig, SET_DESIRED, Some(OPERATOR), &body).await;
        assert_eq!((code, &answer), (400, &invalid_request), "{why}");
    }
    assert_eq!(files(&rig), before);
    let (code, answer) = gateway(&rig, SET_DESIRED, Some(OPERATOR), &plain.to_string()).await;
    assert_eq!(code, 200, "a set with no targets passes: {answer}");
}

/// `message`'s bytes with a field the message does not have, field 15, added so that the
/// whole is `total` bytes. The decoder skips the field.
fn padded(message: &impl Message, total: usize) -> Vec<u8> {
    padded_bytes(message.encode_to_vec(), total)
}

/// `bytes`, a message, with the field added so that the whole is `total` bytes.
fn padded_bytes(mut bytes: Vec<u8>, total: usize) -> Vec<u8> {
    let extra = total
        .checked_sub(bytes.len())
        .expect("a message under the total");
    let varint_len = |n: usize| match n {
        0..128 => 1,
        128..16384 => 2,
        _ => 3,
    };
    let n = (1..=3)
        .find_map(|k| extra.checked_sub(1 + k).filter(|n| varint_len(*n) == k))
        .expect("a field that fills the gap");
    bytes.push((15 << 3) | 2);
    prost::encoding::encode_varint(n as u64, &mut bytes);
    bytes.resize(total, 0);
    bytes
}

/// `body` followed by white space, `total` bytes in all.
fn spaced(body: &str, total: usize) -> String {
    format!("{body}{}", " ".repeat(total - body.len()))
}

/// The status of a gRPC call refused for the size of its request: tonic's `OUT_OF_RANGE` (11),
/// with its message percent-encoded.
fn too_large(limit: usize, found: usize) -> (i32, String) {
    (
        11,
        format!(
            "Error,%20decoded%20message%20length%20too%20large:%20found%20{found}%20bytes,%20the%20limit%20is:%20{limit}%20bytes"
        ),
    )
}

/// Proves that `SetDesired` reads a request of exactly 256 KiB on both routes and refuses one
/// byte more: over gRPC the unknown bytes of the message count, on the gateway the white space
/// of the body does. The request at the limit is kept; the refused one changes no file.
///
/// Catches: a limit of 256 KiB applied to one route only, or applied off by one.
#[tokio::test]
async fn set_desired_reads_256_kib_and_not_one_byte_more_on_both_routes() {
    const LIMIT: usize = 256 * 1024;
    let rig = rig("targets-limit-set");
    let before = files(&rig);
    let over = grpc_bytes(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &padded(&set("web", "2.0", targets("web")), LIMIT + 1),
    )
    .await;
    assert_eq!(
        (over.code, over.message.clone()),
        too_large(LIMIT, LIMIT + 1)
    );
    assert_eq!(files(&rig), before);
    let at = grpc_bytes(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &padded(&set("web", "2.0", targets("web")), LIMIT),
    )
    .await;
    assert_eq!(at.code, 0, "{at:?}");
    let read: proto::GetReleaseResponse = get(&rig, "web", "2.0").await.decoded();
    assert_eq!(read.targets, targets("web"));

    let body = set_json(&set("web", "2.1", targets("web"))).to_string();
    let invalid_request = json!({"code": "INVALID_ARGUMENT", "reason": "invalid_request"});
    let before = files(&rig);
    let (code, answer) = gateway(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &spaced(&body, LIMIT + 1),
    )
    .await;
    assert_eq!((code, &answer), (400, &invalid_request));
    assert_eq!(files(&rig), before);
    let (code, answer) = gateway(&rig, SET_DESIRED, Some(&ci_token()), &spaced(&body, LIMIT)).await;
    assert_eq!(code, 200, "{answer}");
    assert_eq!(
        get(&rig, "web", "2.1")
            .await
            .decoded::<proto::GetReleaseResponse>()
            .targets,
        targets("web")
    );
}

/// Proves that every other method of `Releases` still reads 4 KiB and no more, on both routes:
/// `Converge`, `GetRelease` and `Status` are answered at 4096 bytes and refused at 4097, over
/// gRPC and on the gateway.
///
/// Catches: the larger limit of `SetDesired` leaking to the service's other methods.
#[tokio::test]
async fn every_other_releases_method_still_reads_4_kib() {
    const LIMIT: usize = 4096;
    const BASE: &str = "/hostkeeper.operator.v1.Releases/";
    let rig = rig("targets-limit-others");
    let invalid_request = json!({"code": "INVALID_ARGUMENT", "reason": "invalid_request"});
    let converge = proto::ConvergeRequest {
        service: "web".to_owned(),
    };
    let get_release = proto::GetReleaseRequest {
        service: "web".to_owned(),
        version: "9.9".to_owned(),
    };
    let status = proto::StatusRequest::default();
    let calls: [(&str, Vec<u8>, String); 3] = [
        (
            "Converge",
            converge.encode_to_vec(),
            json!({"service": "web"}).to_string(),
        ),
        (
            "GetRelease",
            get_release.encode_to_vec(),
            json!({"service": "web", "version": "9.9"}).to_string(),
        ),
        ("Status", status.encode_to_vec(), "{}".to_owned()),
    ];
    for (method, message, body) in calls {
        let path = format!("{BASE}{method}");
        let plain = grpc_bytes(&rig, &path, Some(OPERATOR), &message).await;
        let message = |total| padded_bytes(message.clone(), total);
        let at = grpc_bytes(&rig, &path, Some(OPERATOR), &message(LIMIT)).await;
        assert_eq!(at, plain, "{method} over gRPC at 4096");
        let over = grpc_bytes(&rig, &path, Some(OPERATOR), &message(LIMIT + 1)).await;
        assert_eq!(
            (over.code, over.message),
            too_large(LIMIT, LIMIT + 1),
            "{method} over gRPC"
        );

        let plain = gateway(&rig, &path, Some(OPERATOR), &body).await;
        let at = gateway(&rig, &path, Some(OPERATOR), &spaced(&body, LIMIT)).await;
        assert_eq!(at, plain, "{method} on the gateway at 4096");
        let (code, answer) = gateway(&rig, &path, Some(OPERATOR), &spaced(&body, LIMIT + 1)).await;
        assert_eq!(
            (code, &answer),
            (400, &invalid_request),
            "{method} on the gateway"
        );
    }
}

/// Proves that the frozen `POST /v1/desired` still reads 4 KiB: a body of 4096 bytes is kept
/// and one of 4097 is refused, and the larger limit of `SetDesired` does not reach it.
///
/// Catches: the frozen route sharing the constant of the new method.
#[tokio::test]
async fn the_frozen_desired_route_still_reads_4_kib() {
    let rig = rig("targets-limit-frozen");
    let body = r#"{"service":"web","version":"2.0"}"#;
    let before = files(&rig);
    let (code, answer) = rig.desired(&spaced(body, 4097)).await;
    assert_eq!(code, 400, "{answer}");
    assert_eq!(files(&rig), before);
    let (code, answer) = rig.desired(&spaced(body, 4096)).await;
    assert_eq!(code, 200, "{answer}");
}
