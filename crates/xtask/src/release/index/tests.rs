use super::*;
use crate::release::fakes::scratch;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn version() -> Version {
    "1.5.0".parse().expect("a version")
}

fn two() -> ReleaseIndex {
    ReleaseIndex {
        service: "tierstore".to_owned(),
        version: version(),
        packages: vec![
            IndexEntry {
                platform: Platform::LinuxArm64,
                file: "tierstore-1.5.0-linux-arm64.tar.zst".to_owned(),
                sha256: A.to_owned(),
                bytes: 10,
            },
            IndexEntry {
                platform: Platform::LinuxX86_64,
                file: "tierstore-1.5.0-linux-x86_64.tar.zst".to_owned(),
                sha256: B.to_owned(),
                bytes: 20,
            },
        ],
    }
}

/// The canonical text of `two()`.
fn two_text() -> String {
    format!(
        "{{\"packages\":[\
         {{\"bytes\":10,\"file\":\"tierstore-1.5.0-linux-arm64.tar.zst\",\"platform\":\"linux-arm64\",\"sha256\":\"{A}\"}},\
         {{\"bytes\":20,\"file\":\"tierstore-1.5.0-linux-x86_64.tar.zst\",\"platform\":\"linux-x86_64\",\"sha256\":\"{B}\"}}\
         ],\"schema\":1,\"service\":\"tierstore\",\"version\":\"1.5.0\"}}"
    )
}

fn parse(text: &str) -> Result<ReleaseIndex, IndexInvalid> {
    ReleaseIndex::parse(text, "tierstore", &version())
}

fn invalid(why: &str) -> Result<ReleaseIndex, IndexInvalid> {
    Err(IndexInvalid(why.to_owned()))
}

/// The canonical text of `two()` with `from` replaced by `to` (once).
fn edited(from: &str, to: &str) -> String {
    let text = two_text();
    assert!(text.contains(from), "{from}");
    text.replacen(from, to, 1)
}

#[test]
fn the_index_is_written_canonically_and_reads_back() {
    // Catches: keys out of order, white space, a field renamed, or a text the reader refuses.
    assert_eq!(two().to_json(), two_text());
    assert_eq!(parse(&two_text()), Ok(two()));
    // Spacing is not part of what is read; the canonical form is what is written.
    let spaced = two_text().replace(",\"schema\"", ",\n  \"schema\"");
    assert_eq!(parse(&spaced), Ok(two()));
}

#[test]
fn an_index_with_other_fields_or_another_schema_service_or_version_is_invalid() {
    // Catches: a reader that ignores an unknown field, a missing one, another schema, or an
    // index of another service or version (which the daemon must refuse, not install).
    let fields = "[\"packages\", \"schema\", \"service\", \"version\"]";
    assert_eq!(parse("[]"), invalid("the index is not a JSON object"));
    assert_eq!(
        parse("{}"),
        Err(IndexInvalid(format!(
            "the index has the fields [], not exactly {fields}"
        )))
    );
    assert_eq!(
        parse(&edited("\"schema\":1,", "\"schema\":1,\"signed\":true,")),
        Err(IndexInvalid(format!(
            "the index has the fields [\"packages\", \"schema\", \"service\", \"signed\", \"version\"], not exactly {fields}"
        )))
    );
    assert_eq!(
        parse(&edited("\"schema\":1", "\"schema\":2")),
        invalid("the schema is 2, not 1")
    );
    assert_eq!(
        parse(&edited("\"schema\":1", "\"schema\":\"1\"")),
        invalid("the schema is \"1\", not 1")
    );
    assert_eq!(
        parse(&edited(
            "\"service\":\"tierstore\"",
            "\"service\":\"other\""
        )),
        invalid("the index is of other 1.5.0, not tierstore 1.5.0")
    );
    assert_eq!(
        parse(&edited("\"version\":\"1.5.0\"", "\"version\":\"1.5.1\"")),
        invalid("the index is of tierstore 1.5.1, not tierstore 1.5.0")
    );
    assert_eq!(
        parse(&edited("\"service\":\"tierstore\"", "\"service\":7")),
        invalid("the index: service is not a string")
    );
    assert_eq!(
        parse("not json").map_err(|e| e.0.starts_with("not JSON: ")),
        Err(true)
    );
}

#[test]
fn a_package_entry_that_is_not_exactly_the_package_is_invalid() {
    // Catches: an unknown platform, a file of another service, version or platform, a digest
    // that is not a SHA-256, a size that is not a number, or an entry with other fields.
    let fields = "[\"bytes\", \"file\", \"platform\", \"sha256\"]";
    assert_eq!(
        parse(&edited("\"packages\":[{", "\"packages\":[7,{")),
        invalid("packages[0] is not a JSON object")
    );
    assert_eq!(
        parse(&edited("{\"bytes\":10,", "{\"bytes\":10,\"arch\":\"v3\",")),
        Err(IndexInvalid(format!(
            "packages[0] has the fields [\"arch\", \"bytes\", \"file\", \"platform\", \"sha256\"], not exactly {fields}"
        )))
    );
    assert_eq!(
        parse(&edited(
            "\"platform\":\"linux-arm64\"",
            "\"platform\":\"linux-riscv64\""
        )),
        invalid("packages[0]: \"linux-riscv64\" is not a platform")
    );
    assert_eq!(
        parse(&edited("\"platform\":\"linux-arm64\"", "\"platform\":null")),
        invalid("packages[0]: platform is not a string")
    );
    assert_eq!(
        parse(&edited(
            "tierstore-1.5.0-linux-arm64.tar.zst",
            "other-1.5.0-linux-arm64.tar.zst"
        )),
        invalid(
            "packages[0]: the file is \"other-1.5.0-linux-arm64.tar.zst\", not \"tierstore-1.5.0-linux-arm64.tar.zst\""
        )
    );
    let upper = A.to_ascii_uppercase();
    assert_eq!(
        parse(&edited(A, &upper)),
        Err(IndexInvalid(format!(
            "packages[0]: {upper:?} is not a SHA-256 (64 lowercase hex digits)"
        )))
    );
    assert_eq!(
        parse(&edited(A, "aa")),
        invalid("packages[0]: \"aa\" is not a SHA-256 (64 lowercase hex digits)")
    );
    assert_eq!(
        parse(&edited("\"bytes\":20", "\"bytes\":-1")),
        invalid("packages[1]: bytes is not a whole number")
    );
}

#[test]
fn packages_must_be_one_or_more_each_platform_once_in_order() {
    // Catches: an index with no package, one platform listed twice (two files, two digests for
    // one host), or platforms out of order.
    let text = two_text();
    let (head, rest) = text.split_once("[").expect("the list");
    let (list, tail) = rest.split_once("]").expect("the list");
    let (first, second) = list.split_once("},{").expect("two entries");
    let with = |inner: &str| format!("{head}[{inner}]{tail}");
    assert_eq!(
        parse(&with("")),
        invalid("the platforms [] are not one or more, each once, in order")
    );
    assert_eq!(
        parse(&with(&format!("{{{second},{first}}}"))),
        invalid(
            "the platforms [\"linux-x86_64\", \"linux-arm64\"] are not one or more, each once, in order"
        )
    );
    assert_eq!(
        parse(&with(&format!("{first}}},{first}}}"))),
        invalid(
            "the platforms [\"linux-arm64\", \"linux-arm64\"] are not one or more, each once, in order"
        )
    );
    assert_eq!(
        parse(&format!("{head}7{tail}")),
        invalid("packages is not a list")
    );
    // One package alone is an index.
    let one = with(&format!("{first}}}"));
    assert_eq!(parse(&one).map(|i| i.packages.len()), Ok(1));
}

#[test]
fn the_index_of_a_directory_lists_each_package_there_by_platform_with_size_and_digest() {
    // Catches: an index of a file that is not there, a size or digest of another file, the
    // platforms unsorted, or a platform listed twice when it is asked for twice.
    let dir = scratch("index-of-dir");
    let v = version();
    std::fs::write(dir.join("hostkeeper-1.5.0-linux-x86_64.tar.zst"), b"x86").expect("write");
    std::fs::write(dir.join("hostkeeper-1.5.0-macos-arm64.tar.zst"), b"mac!").expect("write");
    std::fs::write(dir.join("hostkeeper-1.5.0-linux-arm64.tar.gz"), b"not it").expect("write");
    let index = ReleaseIndex::of_dir(
        &dir,
        "hostkeeper",
        &v,
        &[
            Platform::MacosArm64,
            Platform::LinuxX86_64,
            Platform::LinuxArm64,
            Platform::MacosArm64,
        ],
    )
    .expect("index");
    let digest = |name: &str| file_sha256(&dir.join(name)).expect("hash");
    assert_eq!(
        index,
        ReleaseIndex {
            service: "hostkeeper".to_owned(),
            version: v,
            packages: vec![
                IndexEntry {
                    platform: Platform::LinuxX86_64,
                    file: "hostkeeper-1.5.0-linux-x86_64.tar.zst".to_owned(),
                    sha256: digest("hostkeeper-1.5.0-linux-x86_64.tar.zst"),
                    bytes: 3,
                },
                IndexEntry {
                    platform: Platform::MacosArm64,
                    file: "hostkeeper-1.5.0-macos-arm64.tar.zst".to_owned(),
                    sha256: digest("hostkeeper-1.5.0-macos-arm64.tar.zst"),
                    bytes: 4,
                },
            ],
        }
    );
    assert_eq!(
        ReleaseIndex::parse(&index.to_json(), "hostkeeper", &v),
        Ok(index)
    );
}

#[test]
fn a_directory_without_a_package_has_no_index() {
    // Catches: an empty index written for a build that produced no package.
    let dir = scratch("index-of-empty-dir");
    assert_eq!(
        ReleaseIndex::of_dir(&dir, "hostkeeper", &version(), &Platform::ALL),
        Err(Error::Failed(format!(
            "{} holds no package of hostkeeper 1.5.0",
            dir.display()
        )))
    );
    assert_eq!(
        write_index(&dir, Service::Hostkeeper, &version()),
        Err(Error::Failed(format!(
            "{} holds no package of hostkeeper 1.5.0",
            dir.display()
        )))
    );
    assert!(!dir.join("index.json").exists());
}

#[test]
fn the_index_verb_writes_the_canonical_index_beside_the_packages() {
    // Catches: an index written elsewhere, not canonically, or naming platforms that have no
    // package.
    let dir = scratch("index-verb");
    std::fs::write(dir.join("hostkeeper-1.5.0-linux-arm64.tar.zst"), b"arm").expect("write");
    let path = dir.join("index.json");
    assert_eq!(
        write_index(&dir, Service::Hostkeeper, &version()),
        Ok(vec![format!(
            "{}: the packages of hostkeeper 1.5.0 for linux-arm64",
            path.display()
        )])
    );
    let written = std::fs::read_to_string(&path).expect("read the index");
    let expected =
        ReleaseIndex::of_dir(&dir, "hostkeeper", &version(), &Platform::ALL).expect("index");
    assert_eq!(written, expected.to_json());
}

#[test]
fn an_index_that_cannot_be_written_is_an_io_error() {
    // Catches: a failed write reported as success.
    let dir = scratch("index-unwritable");
    std::fs::write(dir.join("hostkeeper-1.5.0-linux-arm64.tar.zst"), b"arm").expect("write");
    std::fs::create_dir(dir.join("index.json")).expect("a directory in the way");
    assert!(matches!(
        write_index(&dir, Service::Hostkeeper, &version()),
        Err(Error::Io { what, .. }) if what == format!("write {}", dir.join("index.json").display())
    ));
}

#[test]
fn an_invalid_index_names_the_refusal_token() {
    // Catches: an error text without the token the daemon reports (`index_invalid`).
    assert_eq!(
        IndexInvalid("the schema is 2, not 1".to_owned()).to_string(),
        "index_invalid: the schema is 2, not 1"
    );
    assert_eq!(
        package_file("tierstore", &version(), Platform::MacosArm64),
        "tierstore-1.5.0-macos-arm64.tar.zst"
    );
}

#[test]
fn each_service_indexes_its_own_packages_under_its_own_file_name() {
    // Catches: an index of one service that lists the packages of the other, written to the
    // other's file name, or whose refusal for want of a package names the wrong service.
    let dir = scratch("index-per-service");
    std::fs::write(dir.join("hello-1.5.0-linux-arm64.tar.zst"), b"hello arm").expect("write");
    std::fs::write(dir.join("hostkeeper-1.5.0-linux-x86_64.tar.zst"), b"hk x86").expect("write");
    let hello = dir.join("hello-index.json");
    assert_eq!(
        write_index(&dir, Service::Hello, &version()),
        Ok(vec![format!(
            "{}: the packages of hello 1.5.0 for linux-arm64",
            hello.display()
        )])
    );
    assert!(!dir.join("index.json").exists());
    let written = std::fs::read_to_string(&hello).expect("read the index");
    let parsed = ReleaseIndex::parse(&written, "hello", &version()).expect("a hello index");
    assert_eq!(parsed.service, "hello");
    assert_eq!(
        parsed
            .packages
            .iter()
            .map(|e| e.file.as_str())
            .collect::<Vec<_>>(),
        ["hello-1.5.0-linux-arm64.tar.zst"]
    );
    assert_eq!(
        ReleaseIndex::parse(&written, "hostkeeper", &version()),
        invalid("the index is of hello 1.5.0, not hostkeeper 1.5.0")
    );
    assert_eq!(
        write_index(&dir, Service::Hostkeeper, &version()),
        Ok(vec![format!(
            "{}: the packages of hostkeeper 1.5.0 for linux-x86_64",
            dir.join("index.json").display()
        )])
    );

    let empty = scratch("index-hello-none");
    std::fs::write(empty.join("hostkeeper-1.5.0-linux-arm64.tar.zst"), b"hk").expect("write");
    assert_eq!(
        write_index(&empty, Service::Hello, &version()),
        Err(Error::Failed(format!(
            "{} holds no package of hello 1.5.0",
            empty.display()
        )))
    );
    assert!(!empty.join("hello-index.json").exists());
}
