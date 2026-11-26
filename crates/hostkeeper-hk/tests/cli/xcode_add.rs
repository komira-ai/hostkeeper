//! `hk xcode add`: the run from the archive to the answer: the upload and its progress, what each
//! refusal of the store says and exits, the answers of `Xcode.List`, the token, the settings file,
//! the modes of what it makes, and its usage errors. Which archive it takes is in
//! `xcode_add_find`, the checks before the upload are in `xcode_add_checks`.

use std::os::unix::fs::PermissionsExt as _;

use crate::add_rig::{Add, Store, TOOLS, archive_bytes, puts, sha256};
use crate::rig::{Opts, Server, TOKEN, args, closed_port};

/// The text a good run prints on stdout for the archive of the tests.
fn stored() -> String {
    format!(
        "stored: Xcode 27.2 (27B52), sha256 {}, {} bytes\n",
        sha256(&archive_bytes()),
        archive_bytes().len()
    )
}

/// Proves: a good run uploads one `PUT /v1/xcode/<sha256>?version=27.2&build=27B52` of exactly the
/// archive's bytes with `Content-Length`, `Content-Type: application/octet-stream`,
/// `Expect: 100-continue` and the token as the bearer, asks `Xcode.List` (with `{}`) before it
/// and again before the upload, then `Hosts.List` for the summary of the Macs, prints only `stored: ...` on stdout and the steps on stderr, in
/// order, and leaves nothing beside the archive.
#[test]
fn a_good_run_uploads_the_archive_and_says_stored() {
    let add = Add::new("good");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!((ran.code, ran.stdout), (0, stored()), "{:?}", ran.stderr);
    let seen = server.seen();
    let order: Vec<&str> = seen.iter().map(|s| s.method.as_str()).collect();
    assert_eq!(order, ["POST", "POST", "GET", "PUT", "POST"]);
    for list in &seen[..2] {
        assert_eq!(list.target, crate::add_rig::LIST);
        assert_eq!(list.text(), "{}");
    }
    assert_eq!(seen[4].target, crate::add_rig::FLEET[0]);
    let put = &seen[3];
    assert_eq!(
        seen[2].target,
        format!("/v1/xcode/{}/upload", sha256(&archive_bytes()))
    );
    assert_eq!(
        put.target,
        format!(
            "/v1/xcode/{}?version=27.2&build=27B52",
            sha256(&archive_bytes())
        )
    );
    assert_eq!(put.body, archive_bytes());
    assert_eq!(
        put.header("content-length"),
        Some(archive_bytes().len().to_string().as_str())
    );
    assert_eq!(put.header("content-type"), Some("application/octet-stream"));
    assert_eq!(put.header("expect"), Some("100-continue"));
    assert_eq!(
        put.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    let lines: Vec<&str> = ran.stderr.lines().collect();
    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle}: {lines:#?}"))
    };
    assert!(
        at("the file given") < at("the store holds no archive of these bytes")
            && at("the store holds no archive of these bytes") < at("signed Apple Software")
            && at("signed Apple Software") < at("read from the expanded app")
            && at("read from the expanded app") < at("uploading to https://127.0.0.1:")
    );
    assert_eq!(add.beside(), ["Xcode_27.2.xip"]);
}

/// The stderr of a run of a 200 000 byte archive, uploaded to a store that takes it.
fn big_run(terminal: bool) -> (Add, crate::rig::Ran) {
    let add = Add::new(if terminal { "progress-tty" } else { "progress" });
    let bytes = [b"xar!".as_slice(), &vec![7_u8; 200_000 - 4]].concat();
    let path = add.file("Xcode_27.2.xip", &bytes, 100);
    let server = Store::default().serve(&add.rig);
    let opts = if terminal {
        Opts::real().terminal()
    } else {
        Opts::real()
    };
    let ran = add.run_with(opts, server.port, &["xcode", "add", &path]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(puts(&server)[0].body, bytes);
    (add, ran)
}

/// Proves: off a terminal the upload writes one `hk: uploading <done> of 200.00 KB (<n>%), <rate>/s`
/// line each time a tenth of the archive is passed (at most ten, the percent rising, the last
/// 100%), no line is rewritten (no carriage return), nothing is said of hashing, and stdout holds
/// the `stored:` line alone.
#[test]
fn off_a_terminal_progress_is_one_line_per_tenth() {
    let (_add, ran) = big_run(false);
    assert!(!ran.stderr.contains('\r'), "{ran:?}");
    assert!(!ran.stderr.contains("hashing"), "{ran:?}");
    let lines: Vec<&str> = ran
        .stderr
        .lines()
        .filter(|l| l.starts_with("hk: uploading ") && l.contains(" of 200.00 KB"))
        .collect();
    assert!((1..=10).contains(&lines.len()), "{lines:#?}");
    let percents: Vec<u64> = lines
        .iter()
        .map(|l| {
            let at = l.find('(').expect("a percent") + 1;
            l[at..]
                .split('%')
                .next()
                .expect("a number")
                .parse()
                .expect("a number")
        })
        .collect();
    assert!(
        percents.windows(2).all(|w| w[0] / 10 < w[1] / 10),
        "{percents:?}"
    );
    assert_eq!(percents.last(), Some(&100));
    assert!(
        lines
            .last()
            .expect("a line")
            .starts_with("hk: uploading 200.00 of 200.00 KB (100%), "),
        "{lines:#?}"
    );
    assert!(lines.iter().all(|l| l.ends_with("/s")), "{lines:#?}");
    assert!(ran.stdout.starts_with("stored: "), "{ran:?}");
    assert_eq!(ran.stdout.lines().count(), 1);
}

/// Proves: on a terminal the hashing and the upload each rewrite one line in place (a carriage
/// return before it, the clear-to-end-of-line sequence after, a newline when it ends), the lines
/// say `hashing` and `uploading` with the sizes, the percent and the rate, and stdout is still
/// the `stored:` line alone.
#[test]
fn on_a_terminal_progress_is_one_line_rewritten_in_place() {
    let (_add, ran) = big_run(true);
    let hashing = ran
        .stderr
        .split('\r')
        .filter(|part| part.starts_with("hashing "))
        .count();
    assert!(hashing >= 1, "{ran:?}");
    assert!(
        ran.stderr
            .contains("\rhashing 200.00 of 200.00 KB (100%), ")
            && ran
                .stderr
                .contains("\rhashing 200.00 of 200.00 KB (100%), "),
        "{ran:?}"
    );
    assert!(
        ran.stderr
            .contains("\ruploading 200.00 of 200.00 KB (100%), "),
        "{ran:?}"
    );
    assert!(ran.stderr.contains("/s\x1b[K\n"), "{ran:?}");
    assert!(!ran.stderr.contains("hk: uploading 2"), "{ran:?}");
    assert_eq!(ran.stdout.lines().count(), 1, "{ran:?}");
}

/// Proves: every refusal of the store is said in words with what to do, then
/// `hk: the coordinator refused the upload (HTTP <n>)` and `refused: <code>`, and exits as design
/// section 2.6 says: 3 for `unauthorized` and `forbidden`, 2 for `not_listed`, `not_kept`,
/// `already_held`, `xcode_version_taken`, `upload_in_progress`, `partial_differs`, `too_large`
/// and `digest_mismatch`; a code the program does not know has no words line; the run prints
/// nothing on stdout.
#[test]
fn each_refusal_of_the_store_is_said_in_words_and_exits_as_listed() {
    let add = Add::new("refusals");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let cases: [(u16, &str, u8, &str); 11] = [
        (
            401,
            "unauthorized",
            3,
            "the coordinator does not take the token; check the file --token-file (or token_file of the settings file) names, and that it holds an operator's token",
        ),
        (
            403,
            "forbidden",
            3,
            "the token is a CI token, which the Xcode store does not take; use an operator's token",
        ),
        (
            409,
            "not_listed",
            2,
            "the coordinator's list of Xcode releases does not name Xcode 27.2 (27B52), and the store takes only a release the list names; wait for the daily list to name it, or check what hk xcode list shows as waiting",
        ),
        (
            409,
            "not_kept",
            2,
            "the store keeps only the newest releases, and Xcode 27.2 is older than they (hk xcode list says how many it keeps); to keep more, raise xcode_keep of a host group",
        ),
        (
            409,
            "already_held",
            2,
            "the store holds an archive of Xcode 27.2 already, from other bytes; hk xcode list names it, and hk xcode remove --sha256 <its sha256> --reason <why> removes it if these bytes are the ones to keep",
        ),
        (
            409,
            "xcode_version_taken",
            2,
            "the store holds another build of Xcode 27.2; hk xcode list names it, and hk xcode remove --sha256 <its sha256> --reason <why> removes it if build 27B52 is the one to keep",
        ),
        (
            409,
            "upload_in_progress",
            2,
            "another upload of Xcode 27.2 is in progress; wait for it to end, then run hk xcode add again",
        ),
        (
            409,
            "partial_differs",
            2,
            "the store keeps a partial upload of these bytes with another size, version or build than Xcode 27.2 (27B52); remove it with hk xcode remove --sha256 <this archive's sha256> --reason <why>, then run hk xcode add again",
        ),
        (
            413,
            "too_large",
            2,
            "the archive is larger than the store takes one upload to be (hk xcode list shows max_bytes); the coordinator's --xcode-max-bytes sets it",
        ),
        (
            400,
            "digest_mismatch",
            2,
            "the bytes sent are not the ones whose SHA-256 was asked to be stored: the archive changed after it was hashed; run hk xcode add again",
        ),
        (500, "store_failed", 1, ""),
    ];
    for (status, code, exit, words) in cases {
        let server = Store::default()
            .answering(status, &format!("{{\"error\":\"{code}\"}}"))
            .serve(&add.rig);
        let ran = add.add(&server, &[&path]);
        let said = if words.is_empty() {
            String::new()
        } else {
            format!("hk: {words}\n")
        };
        let tail = format!(
            "{said}hk: the coordinator refused the upload (HTTP {status})\nrefused: {code}\n"
        );
        assert_eq!(ran.code, exit, "{code}: {ran:?}");
        assert_eq!(ran.stdout, "", "{code}");
        assert!(ran.stderr.ends_with(&tail), "{code}: {ran:?}");
        assert_eq!(puts(&server).len(), 1, "{code}");
    }
    // An answer that is not the store's is exit 1 saying so; no answer at all is exit 1 saying the
    // coordinator keeps nothing of an upload that does not end.
    let server = Store::default().answering(502, "<html>").serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert!(
        ran.stderr
            .ends_with("hk: the answer (HTTP 502) is not the Xcode store's\n"),
        "{ran:?}"
    );
    assert_eq!(ran.code, 1);
}

/// Proves: the token is in no argument and no environment variable of any program the run starts
/// (`pkgutil`, `xip`, `plutil`, `codesign`, `df`), in no line on stdout or stderr, and is the
/// bearer of every request to the coordinator: both `Xcode.List` calls, the query of the upload, the upload and the
/// `Hosts.List` of the summary.
#[test]
fn the_token_is_in_no_argument_environment_or_output_line() {
    let add = Add::new("token");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(ran.code, 0, "{ran:?}");
    for tool in TOOLS.into_iter().filter(|t| *t != "xcodes") {
        let argv = add.record(&format!("{tool}.args"));
        let env = add.record(&format!("{tool}.env"));
        assert!(!argv.is_empty() && !env.is_empty(), "{tool} did not run");
        assert!(!argv.contains(TOKEN), "{tool}'s arguments hold the token");
        assert!(!env.contains(TOKEN), "{tool}'s environment holds the token");
    }
    assert!(!ran.stdout.contains(TOKEN));
    assert!(!ran.stderr.contains(TOKEN));
    let seen = server.seen();
    assert_eq!(seen.len(), 5);
    for request in seen {
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
    }
}

/// Proves: the answers of `Xcode.List` that end the run: a coordinator with no store
/// (`configured` false) is exit 1 saying it has none, before anything is expanded; an answer that
/// is not JSON is exit 1 saying so; a refusal of the gateway is its exit code with the code and the
/// reason (a bad token is 3); no answer is exit 1; a command table with no `Xcode.List` is exit 1
/// saying so.
#[test]
fn the_answers_of_the_list_that_end_the_run() {
    let add = Add::new("list");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let none = Store {
        unconfigured: true,
        ..Store::default()
    }
    .serve(&add.rig);
    let ran = add.add(&none, &[&path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(
            "hk: the coordinator has no Xcode store: it was started without --xcode-dir\n"
        ),
        "{ran:?}"
    );
    assert_eq!(add.runs("pkgutil"), 0);
    let text = Server::canned(&add.rig.pki, 200, "not json");
    let ran = add.add(&text, &[&path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr
            .contains("hk: the answer of Xcode.List is not the coordinator's JSON ("),
        "{ran:?}"
    );
    let refused = Server::canned(
        &add.rig.pki,
        401,
        "{\"code\":\"UNAUTHENTICATED\",\"reason\":\"unauthorized\"}",
    );
    let ran = add.add(&refused, &[&path]);
    assert_eq!(
        (ran.code, ran.stderr.lines().last().unwrap_or_default()),
        (3, "refused: unauthorized"),
        "{ran:?}"
    );
    let ran = add.run(closed_port(), &["xcode", "add", &path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains("hk: no answer from https://127.0.0.1:"),
        "{ran:?}"
    );
    assert_eq!(add.runs("pkgutil"), 0);
    let ran = add.rig.run(
        Opts::fixture().mac(&add.mac),
        &crate::rig::call(&add.rig, none.port, &["xcode-push", &path]),
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr
            .ends_with("hk: this hk has no Xcode.List method to ask the store with\n"),
        "{ran:?}"
    );
}

/// Proves: a flag wins over the settings file, a key the file lacks leaves the flag required, and
/// with the file alone `hk xcode add` reaches the store: the file's endpoint, CA and token file
/// are the ones used (the request carries the token the file's token file holds).
#[test]
fn the_settings_file_gives_it_its_flags() {
    let add = Add::new("settings");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let ran = add.bare(&["xcode", "add", &path]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hk: hk xcode add needs --endpoint, --ca-file and --token-file\n"
        )
    );
    let server = Store::default().serve(&add.rig);
    let settings = format!(
        "endpoint = \"https://127.0.0.1:{}\"\nca_file = \"{}\"\ntoken_file = \"{}\"\n",
        server.port,
        add.rig.path("ca.pem").display(),
        add.rig.path("token").display()
    );
    let dir = add.rig.path("home/.hostkeeper");
    std::fs::create_dir_all(dir.join("xcode")).expect("directories");
    for d in [dir.clone(), dir.join("xcode")] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).expect("a mode");
    }
    std::fs::write(dir.join("hk.toml"), settings).expect("settings");
    std::fs::set_permissions(dir.join("hk.toml"), std::fs::Permissions::from_mode(0o600))
        .expect("a mode");
    let ran = add.bare(&["xcode", "add", &path]);
    assert_eq!((ran.code, ran.stdout), (0, stored()), "{:?}", ran.stderr);
    assert_eq!(
        puts(&server)[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    // A flag wins: another port, where nothing listens.
    let ran = add.run(closed_port(), &["xcode", "add", &path]);
    assert_eq!(ran.code, 1, "{ran:?}");
}

/// Proves: `hk xcode add` off macOS refuses at once (exit 1) naming the verb and the system, with
/// no program run and nothing asked; and with a home that is not set it says where it keeps its
/// records.
#[test]
fn off_macos_and_without_a_home_it_refuses() {
    let mut add = Add::new("refuse");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(ran.code, 0);
    add.mac.os = "linux";
    let ran = add.add(&server, &[&path]);
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
    assert!(
        ran.stderr.ends_with(
            "hk: hk xcode add runs on macOS only, on the Mac signed in to the Apple developer \
             account: it runs xcodes, pkgutil and xip, which are macOS programs, and this system \
             is linux\n"
        ),
        "{ran:?}"
    );
    add.mac.os = "macos";
    let ran = add.run_with(
        Opts::real().without_home(),
        server.port,
        &["xcode", "add", &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(
        ran.stderr,
        "hk: hk xcode add keeps its records and its lock under $HOME, which is not set\n"
    );
}

/// Proves: the usage errors of `hk xcode add` exit 2 before anything runs: two words, a flag it
/// does not have, a credential other than the token file; and `--help` prints its help (exit 0).
#[test]
fn its_usage_errors_exit_2_before_anything_runs() {
    let add = Add::new("usage");
    let server = Store::default().serve(&add.rig);
    let one = "hk: hk xcode add takes at most one Xcode version (27.2) or the path of an archive \
               (~/Downloads/Xcode_27.2.xip)\n";
    let token = "hk: hk xcode add takes --token-file only: --token-stdin, --github-oidc-audience \
                 and --response-file are not for it (stdin is the terminal xcodes asks a \
                 two-factor code on)\n";
    let cases: [(&[&str], &str); 4] = [
        (&["xcode", "add", "27.2", "27.1"], one),
        (
            &["xcode", "add", "--force"],
            "hk: hk xcode add has no flag --force\n",
        ),
        (&["xcode", "add", "--token-stdin"], token),
        (&["xcode", "add", "--response-file", "r"], token),
    ];
    for (words, says) in cases {
        let ran = add.run(server.port, words);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (2, "", says),
            "{words:?}"
        );
    }
    assert!(server.seen().is_empty());
    for tool in TOOLS {
        assert_eq!(add.runs(tool), 0, "{tool}");
    }
    let ran = add
        .rig
        .run(Opts::real(), &args(&["xcode", "add", "--help"]));
    assert_eq!(ran.code, 0);
    assert!(
        ran.stdout
            .starts_with("hk xcode add [<version> | <archive.xip>]\n"),
        "{ran:?}"
    );
}
