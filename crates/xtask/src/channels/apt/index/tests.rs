use super::*;
use crate::channels::fixtures;
use crate::channels::spec::{ARCHES, HOSTKEEPER};

// The goldens are the text of `Packages` and of `Release` for the package hostkeeper at 1.2.2 and
// 1.2.3. The index is shared with the organisation's other products, which write the same form,
// so this module's text changes only on purpose. The gzip lines of the `Release` are not
// compared: they list the compressed files, which depend on the compressor.
const PACKAGES_AMD64: &str = include_str!("golden/Packages_amd64");
const PACKAGES_ARM64: &str = include_str!("golden/Packages_arm64");
const RELEASE: &str = include_str!("golden/Release");

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn deb_of(version: &str, arch: &str) -> Vec<u8> {
    let arch = ARCHES
        .iter()
        .find(|a| a.deb == arch)
        .expect("an architecture");
    fixtures::deb(&HOSTKEEPER.packages[0], version, arch)
}

fn stanza(version: &str, arch: &str, sha: &str) -> Stanza {
    from_fields(
        &format!("Package: hostkeeper\nVersion: {version}\nArchitecture: {arch}\nSHA256: {sha}\n"),
        Suite::Stable,
    )
    .expect("stanza")
}

fn versions(merged: &Merged) -> Vec<String> {
    merged
        .stanzas
        .iter()
        .map(|s| {
            format!(
                "{}.{}.{}/{}",
                s.version.0, s.version.1, s.version.2, s.architecture
            )
        })
        .collect()
}

#[test]
fn the_packages_and_release_text_is_the_golden_form_the_shared_index_has() {
    // Catches: a format drift in the text of an index that other products' tools also write.
    let amd = parse_packages(PACKAGES_AMD64, Suite::Stable).expect("amd64");
    let arm = parse_packages(PACKAGES_ARM64, Suite::Stable).expect("arm64");
    assert_eq!((amd.len(), arm.len()), (2, 1));
    let (new_amd, old_amd) = (amd[0].clone(), amd[1].clone());
    let first = merge(&[], &[old_amd], 25).expect("first");
    let second = merge(&first.stanzas, &[new_amd, arm[0].clone()], 25).expect("second");
    assert_eq!(second.added, 2);
    let info = ReleaseInfo {
        origin: Some("komira".to_owned()),
        label: Some("komira".to_owned()),
        description: Some("komira packages".to_owned()),
        date: 1_790_000_000,
    };
    let files = tree(
        Suite::Stable,
        &info,
        &second.stanzas,
        &strings(&["arm64", "amd64"]),
    )
    .expect("tree");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "dists/stable/main/binary-amd64/Packages",
            "dists/stable/main/binary-amd64/Packages.gz",
            "dists/stable/main/binary-arm64/Packages",
            "dists/stable/main/binary-arm64/Packages.gz",
            "dists/stable/Release"
        ]
    );
    assert_eq!(files[0].data, PACKAGES_AMD64.as_bytes());
    assert_eq!(files[2].data, PACKAGES_ARM64.as_bytes());
    // Every line of the Release but the two gzip lines is the golden's; the gzip lines list
    // the hash and size of the file written.
    let release = String::from_utf8(files[4].data.clone()).expect("utf8");
    let plain = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| !l.ends_with(".gz"))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(plain(&release), plain(RELEASE));
    for file in [&files[1], &files[3]] {
        let rel = file
            .path
            .strip_prefix("dists/stable/")
            .expect("below stable");
        let want = format!(" {} {} {rel}", sha256_hex(&file.data), file.data.len());
        assert!(release.lines().any(|l| l == want), "{want}");
    }
}

#[test]
fn the_stanza_of_a_package_names_its_pool_file_its_size_and_the_sha256_of_its_bytes() {
    // Catches: an index entry that does not match the file, or a Description that is not last.
    let bytes = deb_of("1.2.3", "amd64");
    let s = stanza_of(&bytes, Suite::Stable).expect("stanza");
    assert_eq!(
        (s.package.as_str(), s.version, s.architecture.as_str()),
        ("hostkeeper", (1, 2, 3), "amd64")
    );
    assert_eq!(s.sha256, sha256_hex(&bytes));
    let fields = control::parse(&s.text).expect("parse");
    assert_eq!(
        control::get(&fields, "Filename"),
        Some("pool/hostkeeper/hostkeeper_1.2.3_amd64.deb")
    );
    assert_eq!(
        control::get(&fields, "Size"),
        Some(bytes.len().to_string().as_str())
    );
    assert_eq!(fields[0].name, "Package");
    assert_eq!(fields.last().expect("last").name, "Description");
    assert!(s.text.ends_with('\n'));
}

/// A package whose control file is `control`.
fn deb_with_control(control: &str) -> Vec<u8> {
    fixtures::deb_with_control(&HOSTKEEPER.packages[0], "amd64", control)
}

#[test]
fn a_package_that_would_forge_its_entry_or_leave_the_pool_is_refused() {
    // Catches: a control file naming its own Filename, SHA256 or Size, a name or architecture
    // that is a path, a version that is not X.Y.Z, and a control file missing a field.
    let base = "Package: hostkeeper\nVersion: 1.2.3\nArchitecture: amd64\n";
    assert!(stanza_of(&deb_with_control(base), Suite::Stable).is_ok());
    // Debian's other name characters are names.
    assert!(
        stanza_of(
            &deb_with_control("Package: lib2-host.keeper+x\nVersion: 1.2.3\nArchitecture: all\n"),
            Suite::Stable
        )
        .is_ok()
    );
    for (control, why) in [
        (format!("{base}sha256: forged\n"), "writes itself"),
        (format!("{base}Filename: ../../x\n"), "writes itself"),
        (
            "Package: ../x\nVersion: 1.2.3\nArchitecture: amd64\n".to_owned(),
            "cannot be a file",
        ),
        (
            "Package: hostkeeper\nVersion: 1.2\nArchitecture: amd64\n".to_owned(),
            "cannot be a file",
        ),
        (
            "Package: hostkeeper\nVersion: 1.2.3\nArchitecture: a/b\n".to_owned(),
            "cannot be a file",
        ),
        (
            "Package: hostkeeper\nVersion: 1.2.3\n".to_owned(),
            "no Architecture",
        ),
        (
            "Package: A\nVersion: 1.2.3\nArchitecture: amd64\n".to_owned(),
            "cannot be a file",
        ),
    ] {
        let e = stanza_of(&deb_with_control(&control), Suite::Stable)
            .unwrap_err()
            .to_string();
        assert!(e.contains(why), "{control:?}: {e}");
    }
    assert!(stanza_of(b"junk", Suite::Stable).is_err());
}

#[test]
fn a_packages_stanza_needs_its_name_version_architecture_and_checksum() {
    // Catches: an index file from another product that is half-read.
    for text in [
        "Version: 1.2.3\nArchitecture: amd64\nSHA256: a\n",
        "Package: p\nVersion: 1.2\nArchitecture: amd64\nSHA256: a\n",
        "Package: p\nVersion: 1.2.3\nArchitecture: amd64\n",
        "garbage\n",
    ] {
        assert!(parse_packages(text, Suite::Stable).is_err(), "{text:?}");
    }
    assert!(parse_packages("", Suite::Stable).expect("empty").is_empty());
}

#[test]
fn new_versions_are_added_newest_first_and_the_same_file_again_is_a_no_op() {
    // Catches: 1.10.0 sorted before 1.9.0 as text, a re-run adding a second entry, and an
    // architecture mixed into another's slot.
    let one = merge(&[], &[stanza("1.9.0", "amd64", "a")], 25).expect("one");
    assert_eq!((one.added, one.dropped), (1, 0));
    let two = merge(
        &one.stanzas,
        &[
            stanza("1.10.0", "amd64", "b"),
            stanza("1.0.0", "arm64", "c"),
        ],
        25,
    )
    .expect("two");
    assert_eq!(
        versions(&two),
        ["1.10.0/amd64", "1.9.0/amd64", "1.0.0/arm64"]
    );
    let again = merge(&two.stanzas, &[stanza("1.10.0", "amd64", "b")], 25).expect("again");
    assert_eq!((again.added, again.stanzas), (0, two.stanzas));
}

#[test]
fn an_older_version_or_other_bytes_for_a_listed_version_are_refused() {
    // Catches: the channel moving down, and a rebuilt file replacing a listed one in the index.
    let listed = merge(&[], &[stanza("1.2.3", "amd64", "a")], 25)
        .expect("listed")
        .stanzas;
    let down = merge(&listed, &[stanza("1.2.2", "amd64", "x")], 25).unwrap_err();
    assert!(down.to_string().contains("move the channel down"));
    let other = merge(&listed, &[stanza("1.2.3", "amd64", "x")], 25).unwrap_err();
    assert!(other.to_string().contains("other bytes"));
    // Another architecture's older version is a different slot.
    assert!(merge(&listed, &[stanza("1.0.0", "arm64", "x")], 25).is_ok());
    assert!(merge(&listed, &[], 0).is_err());
}

#[test]
fn only_the_newest_keep_versions_of_a_slot_stay_listed() {
    // Catches: an index that grows with every release, and one that drops the newest.
    let mut stanzas = Vec::new();
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        stanzas = merge(&stanzas, &[stanza(v, "amd64", v)], 25)
            .expect("merge")
            .stanzas;
    }
    stanzas.push(stanza("1.0.0", "arm64", "arm"));
    let kept = merge(&stanzas, &[stanza("1.3.0", "amd64", "n")], 2).expect("keep");
    assert_eq!(
        versions(&kept),
        ["1.3.0/amd64", "1.2.0/amd64", "1.0.0/arm64"]
    );
    assert_eq!((kept.added, kept.dropped), (1, 2));
}

#[test]
fn a_slot_the_merge_does_not_add_to_is_not_trimmed_to_keep() {
    // Catches: hostkeeper's publish dropping the older versions of another product's package
    // because the shared index lists more of them than hostkeeper's own `--keep`.
    let foreign = |v: u64| {
        from_fields(
            &format!("Package: example\nVersion: 1.{v}.0\nArchitecture: amd64\nSHA256: e{v}\n"),
            Suite::Stable,
        )
        .expect("foreign stanza")
    };
    let mut existing: Vec<Stanza> = (0..5).map(foreign).collect();
    existing.extend([stanza("1.0.0", "amd64", "a"), stanza("1.1.0", "amd64", "b")]);
    let merged = merge(&existing, &[stanza("1.2.0", "amd64", "c")], 2).expect("merge");
    let foreign_kept = merged
        .stanzas
        .iter()
        .filter(|s| s.package == "example")
        .count();
    assert_eq!(foreign_kept, 5);
    assert_eq!(
        merged
            .stanzas
            .iter()
            .filter(|s| s.package == "hostkeeper")
            .map(|s| s.version)
            .collect::<Vec<_>>(),
        [(1, 2, 0), (1, 1, 0)]
    );
    assert_eq!((merged.added, merged.dropped), (1, 1));
}

#[test]
fn an_entry_of_another_product_outside_the_shared_format_refuses_the_index() {
    // Catches: an entry the index cannot list (a Debian revision, an architecture it has no file
    // for) being dropped or rewritten instead of refused, which the repository document promises.
    let revision = "Package: example\nVersion: 2.0.1-1\nArchitecture: amd64\nSHA256: z\n";
    assert_eq!(
        parse_packages(revision, Suite::Stable)
            .unwrap_err()
            .to_string(),
        "version \"2.0.1-1\" is not X.Y.Z"
    );
    let all = parse_packages(
        "Package: example\nVersion: 2.0.1\nArchitecture: all\nSHA256: z\n",
        Suite::Stable,
    )
    .expect("parses");
    let info = ReleaseInfo {
        origin: None,
        label: None,
        description: None,
        date: 0,
    };
    let e = tree(Suite::Testing, &info, &all, &strings(&["amd64", "arm64"]))
        .unwrap_err()
        .to_string();
    assert_eq!(e, "example is for all, which the index does not list");
}

#[test]
fn another_products_entries_are_kept_untouched() {
    // Catches: the shared index losing a product when hostkeeper publishes.
    let other = from_fields(
        "Package: example\nVersion: 9.9.9\nArchitecture: amd64\nSHA256: z\nDescription: x\n",
        Suite::Stable,
    )
    .expect("other");
    let merged = merge(
        std::slice::from_ref(&other),
        &[stanza("1.0.0", "amd64", "a")],
        25,
    )
    .expect("merge");
    assert_eq!(merged.stanzas[0], other);
    assert_eq!(merged.stanzas[1].package, "hostkeeper");
}

#[test]
fn the_release_of_a_shared_repository_keeps_its_own_origin_label_and_description() {
    // Catches: hostkeeper renaming a repository other products publish into.
    let info = ReleaseInfo {
        origin: Some("o".to_owned()),
        label: Some("l".to_owned()),
        description: Some("d".to_owned()),
        date: 5,
    };
    assert_eq!(keep_shared_fields(None, info.clone()).expect("none"), info);
    assert_eq!(
        keep_shared_fields(Some(""), info.clone()).expect("empty"),
        info
    );
    let kept =
        keep_shared_fields(Some("Origin: komira\nSuite: stable\n"), info.clone()).expect("kept");
    assert_eq!(
        kept,
        ReleaseInfo {
            origin: Some("komira".to_owned()),
            label: None,
            description: None,
            date: 5
        }
    );
    assert!(keep_shared_fields(Some("no colon"), info).is_err());
}

#[test]
fn a_release_without_a_label_or_description_omits_them() {
    // Catches: empty Label or Description lines that a control file cannot hold.
    let info = ReleaseInfo {
        origin: None,
        label: None,
        description: None,
        date: 1_788_220_800,
    };
    let files = tree(Suite::Stable, &info, &[], &strings(&["amd64"])).expect("tree");
    let release = String::from_utf8(files[2].data.clone()).expect("utf8");
    assert!(release.starts_with("Suite: stable\nCodename: stable\nDate: Tue, 01 Sep 2026 00:00:00 UTC\nArchitectures: amd64\nComponents: main\nAcquire-By-Hash: yes\nSHA256:\n "));
    // An empty index is a Packages of no bytes and its gzip.
    assert!(files[0].data.is_empty() && !files[1].data.is_empty());
}

#[test]
fn an_architecture_that_is_not_a_name_or_is_listed_twice_or_a_stanza_without_one_is_refused() {
    // Catches: a Packages written under a directory the Release does not list.
    let info = ReleaseInfo {
        origin: None,
        label: None,
        description: None,
        date: 0,
    };
    assert!(tree(Suite::Stable, &info, &[], &strings(&["a/b"])).is_err());
    assert!(tree(Suite::Stable, &info, &[], &strings(&["amd64", "amd64"])).is_err());
    let e = tree(
        Suite::Stable,
        &info,
        &[stanza("1.0.0", "s390x", "a")],
        &strings(&["amd64"]),
    )
    .unwrap_err();
    assert!(e.to_string().contains("s390x"));
}

#[test]
fn the_date_is_rfc_2822_in_utc() {
    // Catches: a Date apt rejects: a wrong weekday, month or leap day.
    assert_eq!(format_date(1_788_220_800), "Tue, 01 Sep 2026 00:00:00 UTC");
    assert_eq!(format_date(1_790_000_000), "Mon, 21 Sep 2026 14:13:20 UTC");
    assert_eq!(format_date(1835395200), "Tue, 29 Feb 2028 00:00:00 UTC");
    assert_eq!(format_date(1_798_761_599), "Thu, 31 Dec 2026 23:59:59 UTC");
    assert_eq!(format_date(1_798_761_600), "Fri, 01 Jan 2027 00:00:00 UTC");
}
