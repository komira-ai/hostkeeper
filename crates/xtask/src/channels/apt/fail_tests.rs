//! The suite `testing-fail`: hello's failing build alone, under the Debian version
//! `X.Y.Z~fail.ready`, beside the `testing` suite that lists releases.

use std::path::PathBuf;

use super::build::{self, KEEP};
use super::index::{parse_packages, stanza_of};
use super::read::read;
use super::testing::*;
use super::write::{self, Pool};
use super::*;
use crate::channels::fixtures;
use crate::channels::spec::HOSTKEEPER;
use crate::testdir::TestDir;

const DATE: u64 = 1_790_000_000;

fn hello() -> &'static spec::Package {
    HOSTKEEPER.package("hello").expect("hello")
}

/// The failing build's `.deb`s of release `version`, named as the release's assets are
/// (`hello_<v>-fail-ready_<arch>.deb`) and carrying the Debian version `<v>~fail.ready`.
fn fail_ready_files(dir: &Path, version: &str) {
    for arch in hello().arches {
        std::fs::write(
            dir.join(hello().deb_file(&format!("{version}-fail-ready"), arch)),
            fixtures::deb(hello(), &format!("{version}~fail.ready"), arch),
        )
        .expect("write");
    }
}

/// A repository, and a directory of release assets: the release `1.2.3`'s `.deb`s and its
/// failing build's.
struct Lab {
    dir: TestDir,
    debs: TestDir,
    aws: FakeAws,
}

impl Lab {
    fn new(name: &str) -> Lab {
        let (dir, debs) = (scratch(name), scratch(&format!("{name}-debs")));
        release_files(&debs, "1.2.3");
        fail_ready_files(&debs, "1.2.3");
        Lab {
            dir,
            debs,
            aws: FakeAws::new(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    /// The index of `suite` built for release `version` from `debs`, as the release's three jobs
    /// do (read, index, sign, write), with the signatures as text; the lines of the index step.
    fn publish(
        &self,
        suite: Suite,
        tag: &str,
        debs: &Path,
        version: &str,
    ) -> Result<Vec<String>, Error> {
        let (current, next) = (
            self.path(&format!("{tag}-current")),
            self.path(&format!("{tag}-next")),
        );
        read(&self.aws, suite, BUCKET, &current).expect("read");
        let said = build::build(&build::Request {
            suite,
            current: &current,
            out: &next,
            debs,
            version,
            origin: "komira",
            label: "komira",
            description: "komira packages",
            date: DATE,
            keep: KEEP,
        })?;
        for (name, text) in [("Release.gpg", "signature"), ("InRelease", "clear signed")] {
            std::fs::write(next.join("dists").join(suite.name()).join(name), text).expect("sign");
        }
        write::write(
            &self.aws,
            &write::Request {
                suite,
                bucket: BUCKET,
                distribution_id: "E123ABC",
                hostname: "packages-test.example",
                next: &next,
                pool: Some(Pool { debs, version }),
                last: true,
            },
            None,
        )
        .expect("write");
        Ok(said)
    }

    /// The `Packages` of `arch` that was written for `suite`.
    fn packages(&self, suite: Suite, arch: &str) -> String {
        let key = format!("apt/dists/{}/main/binary-{arch}/Packages", suite.name());
        String::from_utf8(self.aws.get(&key).expect("a Packages")).expect("text")
    }
}

#[test]
fn the_suites_are_named_by_their_directory_and_the_command_line() {
    // Catches: a suite whose name or directory differs from the one `--suite` is given.
    use clap::ValueEnum;
    for (name, suite) in [
        ("stable", Suite::Stable),
        ("testing", Suite::Testing),
        ("testing-fail", Suite::TestingFail),
    ] {
        assert_eq!(Suite::from_str(name, false), Ok(suite));
        assert_eq!(suite.name(), name);
        assert_eq!(suite.dir(), format!("dists/{name}/"));
    }
    assert_eq!(Suite::Testing.form(), "X.Y.Z");
    assert_eq!(Suite::TestingFail.form(), "X.Y.Z~fail.ready");
}

#[test]
fn the_testing_suite_lists_the_release_alone_even_when_the_assets_hold_the_failing_build() {
    // Catches: the failing build listed in `testing` or `stable`, where a host would take it for
    // the release's hello.
    let lab = Lab::new("fail-testing-excludes");
    let said = lab
        .publish(Suite::Testing, "t", &lab.debs, "1.2.3")
        .expect("publish");
    assert_eq!(said, ["apt-index: added 9, dropped 0, listed 9"]);
    for arch in ["amd64", "arm64"] {
        let packages = lab.packages(Suite::Testing, arch);
        assert!(!packages.contains("fail"), "{packages}");
        let stanzas = parse_packages(&packages, Suite::Testing).expect("parse");
        assert!(stanzas.iter().all(|s| s.version == (1, 2, 3)));
        assert!(stanzas.iter().any(|s| s.package == "hello"));
    }
    assert!(lab.aws.keys_put().iter().all(|k| !k.contains("fail")));
    let names = package_names("1.2.3", Suite::Stable).expect("names");
    assert!(names.iter().all(|n| !n.contains("fail")), "{names:?}");
}

#[test]
fn the_failing_builds_debs_are_listed_alone_in_testing_fail_under_their_debian_version() {
    // Catches: a failing build listed under its service version `X.Y.Z-fail-ready` (which apt
    // would order above the release), under a pool name that is not its control file's, or beside
    // the release's packages.
    let lab = Lab::new("fail-lists-alone");
    let said = lab
        .publish(Suite::TestingFail, "f", &lab.debs, "1.2.3")
        .expect("publish");
    assert_eq!(said, ["apt-index: added 2, dropped 0, listed 2"]);
    for arch in ["amd64", "arm64"] {
        let packages = lab.packages(Suite::TestingFail, arch);
        let stanzas = parse_packages(&packages, Suite::TestingFail).expect("parse");
        assert_eq!(stanzas.len(), 1, "{packages}");
        assert_eq!(stanzas[0].package, "hello");
        assert_eq!(stanzas[0].version, (1, 2, 3));
        assert!(
            packages.contains("Version: 1.2.3~fail.ready\n"),
            "{packages}"
        );
        assert!(
            packages.contains(&format!(
                "Filename: pool/hostkeeper/hello_1.2.3~fail.ready_{arch}.deb\n"
            )),
            "{packages}"
        );
    }
    let release = String::from_utf8(
        lab.aws
            .get("apt/dists/testing-fail/Release")
            .expect("release"),
    )
    .expect("text");
    assert!(release.contains("Suite: testing-fail\n"), "{release}");
    assert!(release.contains("Codename: testing-fail\n"), "{release}");
}

#[test]
fn testing_fail_writes_the_pool_files_by_their_debian_version_and_its_index_below_its_own_directory()
 {
    // Catches: a pool object named by the asset (`-fail-ready`), the bytes of another file, or an
    // index object written into `testing` or `stable`.
    let lab = Lab::new("fail-write");
    lab.publish(Suite::TestingFail, "f", &lab.debs, "1.2.3")
        .expect("publish");
    let (pool, index): (Vec<String>, Vec<String>) = lab
        .aws
        .keys_put()
        .into_iter()
        .partition(|k| k.starts_with("apt/pool/"));
    assert_eq!(
        pool,
        [
            "apt/pool/hostkeeper/hello_1.2.3~fail.ready_amd64.deb",
            "apt/pool/hostkeeper/hello_1.2.3~fail.ready_arm64.deb"
        ]
    );
    assert!(!index.is_empty());
    assert!(
        index
            .iter()
            .all(|k| k.starts_with("apt/dists/testing-fail/")),
        "{index:?}"
    );
    for arch in ["amd64", "arm64"] {
        assert_eq!(
            lab.aws
                .get(&format!(
                    "apt/pool/hostkeeper/hello_1.2.3~fail.ready_{arch}.deb"
                ))
                .expect("pool object"),
            std::fs::read(lab.debs.join(format!("hello_1.2.3-fail-ready_{arch}.deb")))
                .expect("asset")
        );
    }
}

#[test]
fn read_takes_testing_fail_from_its_own_directory() {
    // Catches: a read of `testing-fail` that copies another suite's index.
    let lab = Lab::new("fail-read");
    lab.aws
        .put("apt/dists/testing-fail/Release", b"Origin: komira\n");
    lab.aws.put("apt/dists/testing/Release", b"Origin: other\n");
    let out = lab.path("read");
    read(&lab.aws, Suite::TestingFail, BUCKET, &out).expect("read");
    assert_eq!(
        std::fs::read_to_string(out.join("dists/testing-fail/Release")).expect("release"),
        "Origin: komira\n"
    );
    assert!(lab.aws.calls_of("get-object").iter().all(|c| {
        flag(c, "--key")
            .expect("key")
            .starts_with("apt/dists/testing-fail/")
    }));
}

#[test]
fn the_failing_builds_files_are_found_by_asset_name_and_a_missing_one_is_named() {
    // Catches: a pool name looked for in the assets (the asset has no `~`), and an index built
    // for one architecture.
    let lab = Lab::new("fail-files");
    let files = package_files(&lab.debs, "1.2.3", Suite::TestingFail).expect("files");
    let listed: Vec<(&str, PathBuf)> = files.iter().map(|(n, p)| (n.as_str(), p.clone())).collect();
    assert_eq!(
        listed,
        [
            (
                "hello_1.2.3~fail.ready_amd64.deb",
                lab.debs.join("hello_1.2.3-fail-ready_amd64.deb")
            ),
            (
                "hello_1.2.3~fail.ready_arm64.deb",
                lab.debs.join("hello_1.2.3-fail-ready_arm64.deb")
            )
        ]
    );
    std::fs::remove_file(lab.debs.join("hello_1.2.3-fail-ready_arm64.deb")).expect("remove");
    assert_eq!(
        package_files(&lab.debs, "1.2.3", Suite::TestingFail)
            .unwrap_err()
            .to_string(),
        format!(
            "hello_1.2.3-fail-ready_arm64.deb is not in {}",
            lab.debs.display()
        )
    );
    // The release's own files are there, and testing does not need the failing build's.
    assert!(package_files(&lab.debs, "1.2.3", Suite::Testing).is_ok());
    assert!(matches!(
        package_files(&lab.debs, "v1.2.3", Suite::TestingFail),
        Err(Error::Usage(_))
    ));
}

#[test]
fn an_entry_of_the_wrong_form_or_package_for_its_suite_is_refused_naming_it() {
    // Catches: a plain release stanza, or another package, listed in `testing-fail`, and a
    // failing build's stanza listed in `testing` or `stable`.
    let stanza = |package: &str, version: &str| {
        format!("Package: {package}\nVersion: {version}\nArchitecture: amd64\nSHA256: a\n")
    };
    let refused =
        |text: &str, suite: Suite| parse_packages(text, suite).expect_err(text).to_string();
    assert_eq!(
        refused(&stanza("hello", "1.2.3"), Suite::TestingFail),
        "version \"1.2.3\" is not X.Y.Z~fail.ready"
    );
    assert_eq!(
        refused(&stanza("hello", "1.2.3~fail.readyx"), Suite::TestingFail),
        "version \"1.2.3~fail.readyx\" is not X.Y.Z~fail.ready"
    );
    assert_eq!(
        refused(&stanza("hello", "1.2~fail.ready"), Suite::TestingFail),
        "version \"1.2~fail.ready\" is not X.Y.Z~fail.ready"
    );
    assert_eq!(
        refused(
            &stanza("hostkeeper", "1.2.3~fail.ready"),
            Suite::TestingFail
        ),
        "hostkeeper is not listed in testing-fail: the suite lists hello alone"
    );
    for suite in [Suite::Testing, Suite::Stable] {
        assert_eq!(
            refused(&stanza("hello", "1.2.3~fail.ready"), suite),
            "version \"1.2.3~fail.ready\" is not X.Y.Z"
        );
        assert!(parse_packages(&stanza("hello", "1.2.3"), suite).is_ok());
    }
    let ok = parse_packages(&stanza("hello", "1.2.3~fail.ready"), Suite::TestingFail)
        .expect("a failing build's stanza");
    assert_eq!(ok[0].version, (1, 2, 3));
}

#[test]
fn a_package_file_of_the_wrong_form_or_package_for_the_suite_gets_no_entry() {
    // Catches: the failing build's `.deb` entered into `testing`, a release's hello entered into
    // `testing-fail`, and another package under the failing build's version.
    let arch = &spec::ARCHES[0];
    let failing = fixtures::deb(hello(), "1.2.3~fail.ready", arch);
    let release = fixtures::deb(hello(), "1.2.3", arch);
    let daemon = fixtures::deb(&HOSTKEEPER.packages[0], "1.2.3~fail.ready", arch);
    assert!(stanza_of(&failing, Suite::TestingFail).is_ok());
    assert!(stanza_of(&release, Suite::Testing).is_ok());
    assert_eq!(
        stanza_of(&failing, Suite::Testing).unwrap_err().to_string(),
        "hello_1.2.3~fail.ready_amd64 cannot be a file of the pool of testing: its version must be X.Y.Z"
    );
    assert_eq!(
        stanza_of(&release, Suite::TestingFail)
            .unwrap_err()
            .to_string(),
        "hello_1.2.3_amd64 cannot be a file of the pool of testing-fail: its version must be X.Y.Z~fail.ready"
    );
    assert_eq!(
        stanza_of(&daemon, Suite::TestingFail)
            .unwrap_err()
            .to_string(),
        "hostkeeper is not listed in testing-fail: the suite lists hello alone"
    );
}

#[test]
fn a_name_or_architecture_that_cannot_be_a_pool_path_is_named_without_blaming_the_version() {
    // Catches: a refusal that tells the operator to fix a version that is in its suite's form.
    let named = spec::Package {
        name: "../x",
        ..*hello()
    };
    let slashed = spec::Arch {
        deb: "a/b",
        rpm: "a/b",
    };
    for (deb, said) in [
        (
            fixtures::deb(&named, "1.2.3~fail.ready", &spec::ARCHES[0]),
            "../x_1.2.3~fail.ready_amd64 cannot be a file of the pool of testing-fail",
        ),
        (
            fixtures::deb(hello(), "1.2.3~fail.ready", &slashed),
            "hello_1.2.3~fail.ready_a/b cannot be a file of the pool of testing-fail",
        ),
    ] {
        assert_eq!(
            stanza_of(&deb, Suite::TestingFail).unwrap_err().to_string(),
            said
        );
    }
}

#[test]
fn an_existing_testing_fail_index_with_a_release_entry_refuses_the_build() {
    // Catches: an index that was written wrongly being merged into, and so republished.
    let lab = Lab::new("fail-existing");
    lab.aws.put(
        "apt/dists/testing-fail/Release",
        b"Origin: komira\nSHA256:\n aa 1 main/binary-amd64/Packages\n",
    );
    lab.aws.put(
        "apt/dists/testing-fail/main/binary-amd64/Packages",
        b"Package: hello\nVersion: 1.2.3\nArchitecture: amd64\nSHA256: a\n\n",
    );
    let e = lab
        .publish(Suite::TestingFail, "f", &lab.debs, "1.2.3")
        .unwrap_err()
        .to_string();
    assert!(
        e.ends_with("version \"1.2.3\" is not X.Y.Z~fail.ready"),
        "{e}"
    );
    assert!(e.contains("testing-fail/main/binary-amd64/Packages"), "{e}");
}

#[test]
fn testing_fail_keeps_the_order_of_the_numbers_an_older_build_is_refused_and_the_same_again_is_a_no_op()
 {
    // Catches: a channel that moves down, an order by text (1.2.10 below 1.2.9), and a second
    // publish of the same build that changes the index.
    let lab = Lab::new("fail-order");
    for version in ["1.2.4", "1.2.9", "1.2.10"] {
        fail_ready_files(&lab.debs, version);
    }
    lab.publish(Suite::TestingFail, "a", &lab.debs, "1.2.9")
        .expect("1.2.9");
    let said = lab
        .publish(Suite::TestingFail, "b", &lab.debs, "1.2.10")
        .expect("1.2.10 is above 1.2.9");
    assert_eq!(said, ["apt-index: added 2, dropped 0, listed 4"]);
    let again = lab
        .publish(Suite::TestingFail, "c", &lab.debs, "1.2.10")
        .expect("the same build");
    assert_eq!(again, ["apt-index: added 0, dropped 0, listed 4"]);
    let e = lab
        .publish(Suite::TestingFail, "d", &lab.debs, "1.2.4")
        .unwrap_err()
        .to_string();
    assert_eq!(
        e,
        "refused: hello 1.2.4 (amd64) is older than the newest listed, which would move the channel down"
    );
}
