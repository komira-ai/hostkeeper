use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::channels::apt::testing::{BUCKET, DENIED, FakeAws};
use crate::channels::pkg::testing::scratch;

const KEYRING_KEY: &str = "apt/komira-archive-keyring.gpg";
const DISTRIBUTION: &str = "E1ABCDEFGHIJK";

/// A keyring of one public key made from `seed`: a version 4 key packet in the new format.
fn keyring(seed: u8) -> Vec<u8> {
    let mut body = vec![4, 0, 0, 0, 1, 22];
    body.extend([seed; 8]);
    let mut packet = vec![0xc0 | 6, u8::try_from(body.len()).expect("short")];
    packet.extend(body);
    packet
}

fn print_of(seed: u8) -> String {
    fingerprints_of(&keyring(seed), "keyring").expect("a keyring")[0].clone()
}

/// A bucket holding the keyring of `seed`.
fn bucket(seed: u8) -> FakeAws {
    let aws = FakeAws::new();
    aws.put(KEYRING_KEY, &keyring(seed));
    aws
}

fn request<'a>(work: &'a Path, attempts: u32, distribution: &'a str) -> Request<'a> {
    Request {
        bucket: BUCKET,
        distribution_id: distribution,
        keyring_key: KEYRING_KEY,
        work,
        attempts,
    }
}

fn written(aws: &FakeAws) -> Value {
    serde_json::from_slice(&aws.get(rules::FILE).expect("the descriptor")).expect("JSON")
}

/// Another product's descriptor in the bucket: its own channel, package and an unknown key, over
/// the same key and layout.
fn other_products(seed: u8) -> Value {
    json!({
        "version": 1,
        "name": "komira",
        "description": "komira-ai packages",
        "channels": ["testing", "nightly"],
        "key": { "keyring": KEYRING_KEY, "fingerprints": [print_of(seed)] },
        "formats": {
            "apt": { "path": "apt", "components": ["main", "extra"], "architectures": ["arm64"] },
            "macos-pkg": {
                "path": "pkg",
                "platforms": ["macos-arm64"],
                "packages": { "tierstore": { "dir": "tierstore" } },
                "note": "kept"
            }
        },
        "later": { "kept": true }
    })
}

/// Proves: this product's contribution is its channels, the architectures its packages are built
/// for (debs for all, rpms for the two that are one), the macOS platform and the installers it
/// ships mapped to the directory of the installers, and passes the coordinator's own check.
///
/// Catches: a descriptor that claims a format or a platform the product does not publish, or
/// one the coordinator would refuse.
#[test]
fn the_contribution_lists_what_the_product_publishes_and_passes_the_coordinators_check() {
    let ours = contribution(&spec::HOSTKEEPER, KEYRING_KEY, &[print_of(1)]);
    assert_eq!(
        ours,
        json!({
            "version": 1,
            "name": "komira",
            "description": "komira-ai packages",
            "channels": ["testing", "stable"],
            "key": { "keyring": KEYRING_KEY, "fingerprints": [print_of(1)] },
            "formats": {
                "apt": { "path": "apt", "components": ["main"], "architectures": ["amd64", "arm64"] },
                "rpm": { "path": "rpm", "architectures": ["aarch64", "x86_64"] },
                "macos-pkg": {
                    "path": "pkg",
                    "platforms": ["macos-arm64"],
                    "packages": { "hello": { "dir": "hostkeeper" }, "hostkeeper": { "dir": "hostkeeper" } }
                }
            }
        })
    );
    let parsed = rules::check(ours.to_string().as_bytes()).expect("valid");
    assert_eq!(
        parsed.platforms(),
        ["linux-x86_64", "linux-arm64", "macos-arm64"]
    );
}

/// Proves: with no descriptor in the bucket the verb writes this product's contribution with
/// `If-None-Match: *`, the JSON content type and `max-age=60`, computes the fingerprints from the
/// keyring in the bucket, and invalidates `/hostkeeper-repository.json` when a distribution is given.
///
/// Catches: a descriptor written over another's, with the wrong cache header, or with fingerprints
/// the keyring does not have.
#[test]
fn write_creates_the_descriptor_when_there_is_none() {
    let d = scratch("descriptor-create");
    let aws = bucket(1);
    let said = write(
        &aws,
        &request(&d.join("w"), 3, DISTRIBUTION),
        &spec::HOSTKEEPER,
    )
    .expect("written");
    assert_eq!(
        said,
        [
            "hostkeeper-repository.json: written",
            "/hostkeeper-repository.json: invalidated"
        ]
    );
    let put = &aws.calls_of("put-object")[0];
    let flag = |name: &str| crate::channels::apt::testing::flag(put, name);
    assert_eq!(flag("--key").as_deref(), Some("hostkeeper-repository.json"));
    assert_eq!(flag("--if-none-match").as_deref(), Some("*"));
    assert_eq!(flag("--content-type").as_deref(), Some("application/json"));
    assert_eq!(
        flag("--cache-control").as_deref(),
        Some("public, max-age=60")
    );
    assert_eq!(written(&aws)["key"]["fingerprints"], json!([print_of(1)]));
    assert!(
        aws.calls_of("create-invalidation")[0]
            .iter()
            .any(|a| a == "/hostkeeper-repository.json")
    );
    let text = String::from_utf8(aws.get(rules::FILE).expect("descriptor")).expect("utf8");
    assert!(rules::check(text.as_bytes()).is_ok());
    assert!(text.ends_with("}\n"));
}

/// Proves: another product's descriptor is merged with, never overwritten: its packages, channels,
/// components, architectures, its own `note` and a key `later` this build does not know are kept,
/// this product's are added after them (channels and architectures united), the write carries
/// `If-Match` on the ETag read, and what is written is a valid descriptor.
///
/// Catches: a release that removes another product's packages or channels from the shared host.
#[test]
fn write_merges_into_what_another_product_wrote_and_removes_nothing() {
    let d = scratch("descriptor-merge");
    let aws = bucket(1);
    let before = serde_json::to_vec(&other_products(1)).expect("json");
    aws.put(rules::FILE, &before);
    let said = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect("written");
    assert_eq!(said, ["hostkeeper-repository.json: written"]);
    let merged = written(&aws);
    assert_eq!(merged["channels"], json!(["testing", "nightly", "stable"]));
    assert_eq!(merged["later"], json!({"kept": true}));
    assert_eq!(
        merged["formats"]["apt"]["components"],
        json!(["main", "extra"])
    );
    assert_eq!(
        merged["formats"]["apt"]["architectures"],
        json!(["arm64", "amd64"])
    );
    assert_eq!(
        merged["formats"]["rpm"]["architectures"],
        json!(["aarch64", "x86_64"])
    );
    assert_eq!(merged["formats"]["macos-pkg"]["note"], "kept");
    assert_eq!(
        merged["formats"]["macos-pkg"]["packages"],
        json!({
            "hello": { "dir": "hostkeeper" },
            "hostkeeper": { "dir": "hostkeeper" },
            "tierstore": { "dir": "tierstore" }
        })
    );
    assert!(rules::check(&aws.get(rules::FILE).expect("descriptor")).is_ok());
    let put = &aws.calls_of("put-object")[0];
    assert_eq!(
        crate::channels::apt::testing::flag(put, "--if-match"),
        Some(crate::channels::apt::testing::etag(&before))
    );
    // A second run finds this release's facts there and writes nothing.
    let again = write(
        &aws,
        &request(&d.join("w2"), 3, DISTRIBUTION),
        &spec::HOSTKEEPER,
    )
    .expect("again");
    assert_eq!(
        again,
        [
            "hostkeeper-repository.json: already holds this release's facts",
            "/hostkeeper-repository.json: invalidated"
        ]
    );
    assert_eq!(aws.calls_of("put-object").len(), 1);
    let quiet = write(&aws, &request(&d.join("w3"), 3, ""), &spec::HOSTKEEPER).expect("again");
    assert_eq!(
        quiet,
        ["hostkeeper-repository.json: already holds this release's facts"]
    );
}

/// Proves: a descriptor whose key is not the keyring's fingerprints is refused, naming both, and
/// nothing is written; likewise another keyring path, another path of a format, another directory
/// for a package of this product, a version this build does not read, and a descriptor that is no
/// object or no JSON.
///
/// Catches: the organisation's key, layout or another product's package silently changed by this
/// product's release.
#[test]
fn write_refuses_to_change_what_another_product_holds() {
    let d = scratch("descriptor-refuse");
    let rejected = |change: &dyn Fn(&mut Value), words: &[&str]| {
        let aws = bucket(1);
        let mut other = other_products(1);
        change(&mut other);
        aws.put(rules::FILE, &serde_json::to_vec(&other).expect("json"));
        let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER)
            .expect_err("refused")
            .to_string();
        for word in words {
            assert!(why.contains(word), "{word:?} not in {why}");
        }
        assert!(aws.calls_of("put-object").is_empty());
    };
    rejected(
        &|v| v["key"]["fingerprints"] = json!([print_of(2)]),
        &[
            "key.fingerprints",
            &print_of(2),
            &print_of(1),
            "not this one's to change",
        ],
    );
    rejected(
        &|v| v["key"]["keyring"] = json!("other/key.gpg"),
        &["key.keyring", "other/key.gpg"],
    );
    rejected(
        &|v| v["formats"]["apt"]["path"] = json!("debian"),
        &["formats.apt.path", "debian"],
    );
    rejected(
        &|v| v["formats"]["macos-pkg"]["packages"]["hello"] = json!({"dir": "elsewhere"}),
        &["the package hello", "elsewhere", "hostkeeper"],
    );
    rejected(
        &|v| v["version"] = json!(2),
        &["version 2", "newer than this build"],
    );
    rejected(
        &|v| v["channels"] = json!("testing"),
        &["channels is not a list"],
    );
    rejected(&|v| v["key"] = json!(7), &["key is not an object"]);
    rejected(
        &|v| v["formats"]["rpm"] = json!({"path": "rpm", "architectures": 3}),
        &["formats.rpm.architectures is not a list"],
    );

    let aws = bucket(1);
    aws.put(rules::FILE, b"[1]");
    let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert_eq!(
        why.to_string(),
        "the descriptor in the bucket is not a JSON object"
    );
    aws.put(rules::FILE, b"not json");
    let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert!(
        why.to_string()
            .starts_with("hostkeeper-repository.json in the bucket is not JSON")
    );
}

/// Proves: a merge that makes an invalid descriptor (another product's channel that breaks the
/// grammar) is refused by the coordinator's check before anything is written.
///
/// Catches: a descriptor the coordinator would refuse published to every operator's coordinator.
#[test]
fn write_judges_the_merged_descriptor_before_writing_it() {
    let d = scratch("descriptor-invalid");
    let aws = bucket(1);
    let mut other = other_products(1);
    other["channels"] = json!(["Testing"]);
    aws.put(rules::FILE, &serde_json::to_vec(&other).expect("json"));
    let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert!(
        why.to_string()
            .starts_with("the merged descriptor is not valid: channels:"),
        "{why}"
    );
    assert!(aws.calls_of("put-object").is_empty());
}

/// Proves: a keyring that is not in the bucket, one that is no keyring, are refused with their
/// own words and nothing is written; a read that AWS refuses for another reason fails naming the
/// key; an ETag that is missing from the answer fails.
///
/// Catches: fingerprints computed from nothing, or an error that hides what was refused.
#[test]
fn write_needs_the_keyring_and_a_readable_bucket() {
    let d = scratch("descriptor-keyring");
    let aws = FakeAws::new();
    let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert_eq!(
        why.to_string(),
        "the keyring apt/komira-archive-keyring.gpg is not in the bucket"
    );
    *aws.missing.borrow_mut() = DENIED.to_owned();
    assert!(write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).is_err());
    aws.put(KEYRING_KEY, b"junk");
    let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert!(
        why.to_string()
            .starts_with("apt/komira-archive-keyring.gpg is not a keyring of public keys"),
        "{why}"
    );
    let aws = bucket(1);
    aws.refuse_when(
        "get-object",
        "An error occurred (InternalError) when calling the GetObject operation: boom",
    );
    let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert!(
        why.to_string()
            .starts_with("read apt/komira-archive-keyring.gpg: "),
        "{why}"
    );
    let aws = bucket(1);
    *aws.stdout.borrow_mut() = Some("{}".to_owned());
    let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert_eq!(
        why.to_string(),
        "read apt/komira-archive-keyring.gpg: aws printed no ETag"
    );
}

/// Proves: a write that loses the race (another product's release changes the object between the
/// read and the put) reads and merges again and lands on the second attempt, with both products'
/// facts; with one attempt it fails saying so; any other refusal of the put fails naming the key.
///
/// Catches: another product's facts overwritten by a stale merge, or a lost race reported as green.
#[test]
fn write_reads_and_merges_again_after_a_lost_race() {
    let d = scratch("descriptor-race");
    let aws = bucket(1);
    aws.put(
        rules::FILE,
        &serde_json::to_vec(&other_products(1)).expect("json"),
    );
    let mut raced = other_products(1);
    raced["formats"]["macos-pkg"]["packages"]["js-agent"] = json!({"dir": "js-agent"});
    let bytes = serde_json::to_vec(&raced).expect("json");
    aws.before_put_of(
        rules::FILE,
        Box::new(move |store: &mut BTreeMap<String, Vec<u8>>| {
            store.insert(rules::FILE.to_owned(), bytes);
        }),
    );
    write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER)
        .expect("the second attempt lands");
    let merged = written(&aws);
    assert!(merged["formats"]["macos-pkg"]["packages"]["js-agent"].is_object());
    assert!(merged["formats"]["macos-pkg"]["packages"]["hostkeeper"].is_object());
    assert_eq!(aws.calls_of("put-object").len(), 2);

    let aws = bucket(1);
    aws.put(
        rules::FILE,
        &serde_json::to_vec(&other_products(1)).expect("json"),
    );
    aws.before_put_of(
        rules::FILE,
        Box::new(|store: &mut BTreeMap<String, Vec<u8>>| {
            store.insert(rules::FILE.to_owned(), b"{\"version\":1}".to_vec());
        }),
    );
    let why = write(&aws, &request(&d.join("w"), 1, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert_eq!(
        why.to_string(),
        "hostkeeper-repository.json was written by another release 1 times in a row; run the job again"
    );

    let aws = bucket(1);
    aws.refuse_when(
        "put-object",
        "An error occurred (InternalError) when calling the PutObject operation: boom",
    );
    let why = write(&aws, &request(&d.join("w"), 3, ""), &spec::HOSTKEEPER).expect_err("refused");
    assert!(
        why.to_string()
            .starts_with("put hostkeeper-repository.json: "),
        "{why}"
    );
    let aws = bucket(1);
    aws.refuse_when(
        "create-invalidation",
        "An error occurred (AccessDenied) when calling the CreateInvalidation operation",
    );
    let why = write(
        &aws,
        &request(&d.join("w"), 3, DISTRIBUTION),
        &spec::HOSTKEEPER,
    )
    .expect_err("refused");
    assert!(
        why.to_string()
            .starts_with("invalidate /hostkeeper-repository.json: "),
        "{why}"
    );
}

/// Proves: each flag out of its form is a usage error naming the flag, before any call.
///
/// Catches: a bucket, distribution, key or attempt count that reaches AWS malformed.
#[test]
fn write_judges_its_flags_before_it_calls_anything() {
    let d = scratch("descriptor-usage");
    let aws = bucket(1);
    let work = d.join("w");
    let cases: [(Request, &str); 4] = [
        (
            Request {
                bucket: "Bad Bucket",
                ..request(&work, 3, "")
            },
            "--bucket",
        ),
        (request(&work, 3, "lower"), "--distribution-id"),
        (
            Request {
                keyring_key: "/abs",
                ..request(&work, 3, "")
            },
            "--keyring-key",
        ),
        (request(&work, 0, ""), "--attempts"),
    ];
    for (r, flag) in cases {
        let why = write(&aws, &r, &spec::HOSTKEEPER).expect_err("refused");
        assert!(matches!(why, Error::Usage(_)), "{why}");
        assert!(why.to_string().contains(flag), "{why}");
    }
    let why = write(
        &aws,
        &Request {
            keyring_key: "",
            ..request(&work, 3, "")
        },
        &spec::HOSTKEEPER,
    )
    .expect_err("refused");
    assert!(why.to_string().contains("--keyring-key"));
    assert!(aws.calls.borrow().is_empty());
}

/// A `curl` that serves files from memory by URL.
#[derive(Default)]
struct FakeCurl {
    served: RefCell<BTreeMap<String, Vec<u8>>>,
}

impl Host for FakeCurl {
    fn run(&self, program: &Path, args: &[String]) -> Result<Answer, Error> {
        assert_eq!(program, Path::new("curl"));
        assert!(args.iter().any(|a| a == "--proto") && args.iter().any(|a| a == "=https"));
        let url = args.last().expect("a url");
        let at = args.iter().position(|a| a == "--output").expect("--output");
        match self.served.borrow().get(url) {
            Some(bytes) => {
                std::fs::write(&args[at + 1], bytes).expect("write");
                Ok(Answer {
                    ok: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
            None => Ok(Answer {
                ok: false,
                stdout: String::new(),
                stderr: "curl: (22) The requested URL returned error: 404\n".to_owned(),
            }),
        }
    }
}

const HOST_URL: &str = "https://packages.komira.ai";

fn served(seed: u8, descriptor: &Value) -> FakeCurl {
    let curl = FakeCurl::default();
    curl.served.borrow_mut().insert(
        format!("{HOST_URL}/hostkeeper-repository.json"),
        serde_json::to_vec(descriptor).expect("json"),
    );
    curl.served
        .borrow_mut()
        .insert(format!("{HOST_URL}/{KEYRING_KEY}"), keyring(seed));
    curl
}

/// Proves: `check` downloads the descriptor and the keyring it names over HTTPS, and answers one
/// line with the channels, the platforms and the keys; a 404 for either, a descriptor the
/// coordinator would refuse, a keyring that is no keyring and a claimed fingerprint the keyring
/// lacks each fail with their own words.
///
/// Catches: a published descriptor no coordinator can add, or whose key is not the keyring's.
#[test]
fn check_holds_what_the_host_serves_to_the_coordinators_rules() {
    let d = scratch("descriptor-check");
    let work = d.join("w");
    let ours = contribution(&spec::HOSTKEEPER, KEYRING_KEY, &[print_of(1)]);
    let line = check(&served(1, &ours), HOST_URL, &work, Path::new("curl")).expect("valid");
    assert_eq!(
        line,
        [format!(
            "https://packages.komira.ai/hostkeeper-repository.json: valid, 2 channels, platforms linux-x86_64 linux-arm64 macos-arm64, keys {}",
            print_of(1)
        )]
    );
    let run = |curl: &FakeCurl| {
        check(curl, HOST_URL, &work, Path::new("curl"))
            .expect_err("refused")
            .to_string()
    };
    assert!(
        run(&FakeCurl::default()).contains("hostkeeper-repository.json could not be downloaded")
    );
    let missing_key = served(1, &ours);
    missing_key
        .served
        .borrow_mut()
        .remove(&format!("{HOST_URL}/{KEYRING_KEY}"));
    assert!(run(&missing_key).contains("apt/komira-archive-keyring.gpg could not be downloaded"));
    assert!(run(&served(1, &json!({"version": 2}))).contains("newer than this coordinator reads"));
    let junk = served(1, &ours);
    junk.served
        .borrow_mut()
        .insert(format!("{HOST_URL}/{KEYRING_KEY}"), b"junk".to_vec());
    assert!(run(&junk).contains("is not a keyring of public keys"));
    assert!(run(&served(2, &ours)).contains(&format!(
        "the descriptor claims the fingerprint {}",
        print_of(1)
    )));
    let why = check(
        &served(1, &ours),
        "http://packages.komira.ai",
        &work,
        Path::new("curl"),
    )
    .expect_err("usage");
    assert!(matches!(why, Error::Usage(_)), "{why}");
}
