//! The image index of the coordinator's two image tarballs, and the layout a push copies from:
//! the exact bytes of the index, and every way a tarball, the index or the layout's target is
//! refused, each by its full message.

use std::path::Path;

use serde_json::json;

use super::*;
use crate::release::fakes::scratch;

const VERSION: &str = "0.1.5";

fn version() -> Version {
    VERSION.parse().expect("a version")
}

fn hex_of(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A tar of `files`, with the directory entries a buildx tarball has.
fn tar_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for dir in ["blobs/", "blobs/sha256/"] {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        builder
            .append_data(&mut header, dir, &[][..])
            .expect("append");
    }
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        builder
            .append_data(&mut header, path, data.as_slice())
            .expect("append");
    }
    builder.into_inner().expect("tar")
}

/// The manifest of an image of `arch` whose one layer is `layer`, and the blobs it names.
fn manifest_of(arch: &str, layer: &[u8]) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let config = format!("{{\"architecture\":\"{arch}\"}}").into_bytes();
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {"digest": format!("sha256:{}", hex_of(&config)), "size": config.len()},
        "layers": [{"digest": format!("sha256:{}", hex_of(layer)), "size": layer.len()}],
    }))
    .expect("json");
    let blobs = vec![
        (format!("blobs/sha256/{}", hex_of(&config)), config),
        (format!("blobs/sha256/{}", hex_of(layer)), layer.to_vec()),
        (
            format!("blobs/sha256/{}", hex_of(&manifest)),
            manifest.clone(),
        ),
    ];
    (manifest, blobs)
}

/// The `index.json` of a buildx tarball naming `entries`.
fn index_json(entries: Vec<serde_json::Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": entries,
    }))
    .expect("json")
}

/// The index entry buildx writes for `manifest` of `arch`.
fn entry(manifest: &[u8], arch: &str) -> serde_json::Value {
    json!({
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "digest": format!("sha256:{}", hex_of(manifest)),
        "size": manifest.len(),
        "annotations": {"org.opencontainers.image.ref.name": VERSION},
        "platform": {"architecture": arch, "os": "linux"},
    })
}

/// A good image tarball of `arch`, whose one layer `shared` is the same in both images, and its
/// manifest.
fn good(arch: &str) -> (Vec<u8>, Vec<u8>) {
    let (manifest, mut blobs) = manifest_of(arch, b"the shared base layer");
    blobs.push(("oci-layout".to_owned(), OCI_LAYOUT.as_bytes().to_vec()));
    blobs.push((
        "index.json".to_owned(),
        index_json(vec![entry(&manifest, arch)]),
    ));
    (tar_of(&blobs), manifest)
}

/// Writes the two good tarballs into `dir`; returns the two manifests, amd64 first.
fn write_good(dir: &Path) -> (Vec<u8>, Vec<u8>) {
    let (amd, amd_manifest) = good("amd64");
    let (arm, arm_manifest) = good("arm64");
    std::fs::write(dir.join(image_name(&version(), Platform::LinuxX86_64)), amd).expect("write");
    std::fs::write(dir.join(image_name(&version(), Platform::LinuxArm64)), arm).expect("write");
    (amd_manifest, arm_manifest)
}

fn write_arm(dir: &Path, tar: Vec<u8>) {
    std::fs::write(dir.join(image_name(&version(), Platform::LinuxArm64)), tar).expect("write");
}

#[test]
fn the_index_lists_both_manifests_amd64_first_as_one_canonical_line() {
    let dir = scratch("image-index-bytes");
    let (amd, arm) = write_good(&dir);
    let bytes = index_bytes(&dir, &version()).expect("an index");
    let want = format!(
        concat!(
            r#"{{"manifests":["#,
            r#"{{"digest":"sha256:{}","mediaType":"application/vnd.docker.distribution.manifest.v2+json","platform":{{"architecture":"amd64","os":"linux"}},"size":{}}},"#,
            r#"{{"digest":"sha256:{}","mediaType":"application/vnd.docker.distribution.manifest.v2+json","platform":{{"architecture":"arm64","os":"linux"}},"size":{}}}"#,
            r#"],"mediaType":"application/vnd.oci.image.index.v1+json","schemaVersion":2}}"#
        ),
        hex_of(&amd),
        amd.len(),
        hex_of(&arm),
        arm.len()
    );
    assert_eq!(String::from_utf8(bytes).expect("utf-8"), want);
    assert_eq!(
        index_name(&version()),
        "hostkeeper-coordinator-0.1.5.image.json"
    );
}

#[test]
fn the_index_is_written_once_and_checked_against_the_tarballs() {
    let dir = scratch("image-index-write");
    write_good(&dir);
    let said = write_index(&dir, &version()).expect("written");
    let bytes = std::fs::read(dir.join(index_name(&version()))).expect("read");
    let digest = format!("sha256:{}", hex_of(&bytes));
    assert_eq!(
        said,
        [format!(
            "{}: the image index of linux/amd64 and linux/arm64, {digest}",
            dir.join(index_name(&version())).display()
        )]
    );
    assert_eq!(check_index(&dir, &version()), Ok(digest.clone()));

    let again = write_index(&dir, &version()).expect_err("exists");
    assert!(
        again.to_string().starts_with(&format!(
            "write {}: ",
            dir.join(index_name(&version())).display()
        )),
        "{again}"
    );

    std::fs::write(dir.join(index_name(&version())), b"{}").expect("write");
    assert_eq!(
        check_index(&dir, &version())
            .expect_err("other bytes")
            .to_string(),
        format!(
            "hostkeeper-coordinator-0.1.5.image.json is not the image index of the two image tarballs: it is sha256:{}, they make {digest}",
            hex_of(b"{}")
        )
    );
}

#[test]
fn a_missing_index_file_is_refused_by_check() {
    let dir = scratch("image-index-missing");
    write_good(&dir);
    let refused = check_index(&dir, &version()).expect_err("missing");
    assert!(
        refused.to_string().starts_with(&format!(
            "read {}: ",
            dir.join(index_name(&version())).display()
        )),
        "{refused}"
    );
}

#[test]
fn every_tarball_that_is_not_one_image_of_its_platform_is_refused_naming_it() {
    let (manifest, blobs) = manifest_of("arm64", b"layer");
    let with = |index: Vec<u8>| {
        let mut files = blobs.clone();
        files.push(("index.json".to_owned(), index));
        tar_of(&files)
    };
    let arm = "hostkeeper-coordinator-0.1.5-linux-arm64.image.tar";
    let digest = format!("sha256:{}", hex_of(&manifest));
    let mut wrong_size = entry(&manifest, "arm64");
    wrong_size["size"] = json!(1);
    let mut no_type = entry(&manifest, "arm64");
    no_type["mediaType"] = json!(7);
    let mut no_digest = entry(&manifest, "arm64");
    no_digest["digest"] = json!(null);
    let mut no_size = entry(&manifest, "arm64");
    no_size["size"] = json!("big");
    let mut absent = entry(&manifest, "arm64");
    absent["digest"] = json!(format!("sha256:{}", hex_of(b"absent")));
    let mut not_sha = entry(&manifest, "arm64");
    not_sha["digest"] = json!("md5:00");
    let mut other_os = entry(&manifest, "arm64");
    other_os["platform"]["os"] = json!("windows");
    let cases: Vec<(Vec<u8>, String)> = vec![
        (
            b"not a tar".to_vec(),
            format!("{arm}: it is not a readable tar archive"),
        ),
        (tar_of(&blobs), format!("{arm}: index.json is missing")),
        (
            with(b"{".to_vec()),
            format!(
                "{arm}: index.json is not JSON: EOF while parsing an object at line 1 column 1"
            ),
        ),
        (
            with(index_json(vec![])),
            format!("{arm}: index.json names 0 manifests, not one"),
        ),
        (
            with(b"{}".to_vec()),
            format!("{arm}: index.json names 0 manifests, not one"),
        ),
        (
            with(index_json(vec![
                entry(&manifest, "arm64"),
                entry(&manifest, "arm64"),
            ])),
            format!("{arm}: index.json names 2 manifests, not one"),
        ),
        (
            with(index_json(vec![no_type])),
            format!("{arm}: the manifest's mediaType is not a string"),
        ),
        (
            with(index_json(vec![no_digest])),
            format!("{arm}: the manifest's digest is not a string"),
        ),
        (
            with(index_json(vec![no_size])),
            format!("{arm}: the manifest's size is not a number"),
        ),
        (
            with(index_json(vec![entry(&manifest, "amd64")])),
            format!("{arm}: the manifest is for \"linux\"/\"amd64\", not linux/arm64"),
        ),
        (
            with(index_json(vec![other_os])),
            format!("{arm}: the manifest is for \"windows\"/\"arm64\", not linux/arm64"),
        ),
        (
            with(index_json(vec![absent])),
            format!(
                "{arm}: the manifest sha256:{} is not in the tarball",
                hex_of(b"absent")
            ),
        ),
        (
            with(index_json(vec![not_sha])),
            format!("{arm}: the manifest md5:00 is not in the tarball"),
        ),
        (
            with(index_json(vec![wrong_size])),
            format!(
                "{arm}: the manifest named {digest} of 1 bytes is {digest} of {} bytes",
                manifest.len()
            ),
        ),
    ];
    for (tar, why) in cases {
        let dir = scratch("image-index-refused");
        write_good(&dir);
        write_arm(&dir, tar);
        assert_eq!(
            index_bytes(&dir, &version())
                .expect_err("refused")
                .to_string(),
            why
        );
    }
}

#[test]
fn a_manifest_blob_of_other_bytes_than_its_name_is_refused() {
    let (manifest, mut blobs) = manifest_of("arm64", b"layer");
    let name = format!("blobs/sha256/{}", hex_of(&manifest));
    for (path, data) in &mut blobs {
        if *path == name {
            data.push(b' ');
        }
    }
    let mut lying = entry(&manifest, "arm64");
    lying["size"] = json!(manifest.len() + 1);
    blobs.push(("index.json".to_owned(), index_json(vec![lying])));
    let dir = scratch("image-index-lying");
    write_good(&dir);
    write_arm(&dir, tar_of(&blobs));
    let mut spaced = manifest.clone();
    spaced.push(b' ');
    assert_eq!(
        index_bytes(&dir, &version())
            .expect_err("refused")
            .to_string(),
        format!(
            "hostkeeper-coordinator-0.1.5-linux-arm64.image.tar: the manifest named sha256:{} of {} bytes is sha256:{} of {} bytes",
            hex_of(&manifest),
            manifest.len() + 1,
            hex_of(&spaced),
            spaced.len()
        )
    );
}

#[test]
fn a_missing_tarball_is_refused_naming_its_path() {
    let dir = scratch("image-index-absent");
    let refused = index_bytes(&dir, &version()).expect_err("absent");
    assert!(
        refused.to_string().starts_with(&format!(
            "read {}: ",
            dir.join(image_name(&version(), Platform::LinuxX86_64))
                .display()
        )),
        "{refused}"
    );
}

#[test]
fn the_layout_holds_every_blob_once_and_names_the_index_under_the_version() {
    let dir = scratch("image-layout");
    let (amd, arm) = write_good(&dir);
    write_index(&dir, &version()).expect("written");
    let index = std::fs::read(dir.join(index_name(&version()))).expect("read");
    let out = dir.join("layout");
    let said = write_layout(&dir, &version(), &out).expect("written");
    // Two configs, one shared layer, two manifests and the index.
    assert_eq!(
        said,
        [format!(
            "{}: the image 0.1.5 (sha256:{}), 6 blobs",
            out.display(),
            hex_of(&index)
        )]
    );
    assert_eq!(
        std::fs::read_to_string(out.join("oci-layout")).expect("read"),
        r#"{"imageLayoutVersion":"1.0.0"}"#
    );
    assert_eq!(
        std::fs::read_to_string(out.join("index.json")).expect("read"),
        format!(
            r#"{{"manifests":[{{"annotations":{{"org.opencontainers.image.ref.name":"0.1.5"}},"digest":"sha256:{}","mediaType":"application/vnd.oci.image.index.v1+json","size":{}}}],"mediaType":"application/vnd.oci.image.index.v1+json","schemaVersion":2}}"#,
            hex_of(&index),
            index.len()
        )
    );
    let mut names: Vec<String> = std::fs::read_dir(out.join("blobs/sha256"))
        .expect("blobs")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for bytes in [&index, &amd, &arm] {
        let blob = std::fs::read(out.join("blobs/sha256").join(hex_of(bytes))).expect("a blob");
        assert_eq!(&blob, bytes);
    }
    assert_eq!(names.len(), 6);

    let again = write_layout(&dir, &version(), &out).expect_err("exists");
    assert!(
        again
            .to_string()
            .starts_with(&format!("create {}: ", out.display())),
        "{again}"
    );
}

#[test]
fn a_layout_is_refused_for_a_stale_index_or_a_blob_that_is_not_its_digest() {
    let dir = scratch("image-layout-stale");
    write_good(&dir);
    std::fs::write(dir.join(index_name(&version())), b"{}").expect("write");
    let out = dir.join("layout");
    let refused = write_layout(&dir, &version(), &out).expect_err("stale");
    assert!(
        refused
            .to_string()
            .starts_with("hostkeeper-coordinator-0.1.5.image.json is not the image index"),
        "{refused}"
    );
    assert!(!out.exists(), "nothing is written");

    let dir = scratch("image-layout-blob");
    let (manifest, mut blobs) = manifest_of("arm64", b"layer");
    let bad = hex_of(b"what the name says");
    blobs.push((format!("blobs/sha256/{bad}"), b"other bytes".to_vec()));
    blobs.push((
        "index.json".to_owned(),
        index_json(vec![entry(&manifest, "arm64")]),
    ));
    write_good(&dir);
    write_arm(&dir, tar_of(&blobs));
    write_index(&dir, &version()).expect("written");
    let out = dir.join("layout");
    assert_eq!(
        write_layout(&dir, &version(), &out)
            .expect_err("blob")
            .to_string(),
        format!(
            "hostkeeper-coordinator-0.1.5-linux-arm64.image.tar: the blob blobs/sha256/{bad} holds other bytes (sha256:{})",
            hex_of(b"other bytes")
        )
    );
    assert!(!out.exists(), "nothing is written");
}
