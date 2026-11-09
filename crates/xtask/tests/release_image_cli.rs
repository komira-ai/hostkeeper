//! `cargo xtask release image-index` and `release image-layout`, run as the built program on a
//! scratch directory holding two image tarballs written byte by byte as buildx writes them. The
//! exit status is 0 for a written index or layout, 1 for a refused one and 2 for a usage error.
//! No toolchain, no network.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use testdir::TestDir;

const VERSION: &str = "1.2.3";
const INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";

fn dir(name: &str) -> TestDir {
    TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("release-image-{name}"),
    )
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run xtask")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

fn digest_of(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn tar_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
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

fn blob_path(bytes: &[u8]) -> String {
    format!(
        "blobs/sha256/{}",
        digest_of(bytes).trim_start_matches("sha256:")
    )
}

/// An image tarball as buildx writes one: an OCI layout whose `index.json` names one manifest
/// of `arch`, which names a config and a layer holding `marker`.
fn image_tar(arch: &str, marker: &str, with_index: bool) -> (Vec<u8>, Vec<u8>) {
    let config = format!("{{\"architecture\":\"{arch}\",\"marker\":\"{marker}\"}}").into_bytes();
    let layer = format!("layer {marker}").into_bytes();
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {"digest": digest_of(&config), "size": config.len()},
        "layers": [{"digest": digest_of(&layer), "size": layer.len()}],
    }))
    .expect("manifest");
    let index = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": INDEX_MEDIA_TYPE,
        "manifests": [{
            "mediaType": MANIFEST_MEDIA_TYPE,
            "digest": digest_of(&manifest),
            "size": manifest.len(),
            "platform": {"architecture": arch, "os": "linux"},
        }],
    }))
    .expect("index");
    let mut files = vec![(
        "oci-layout".to_owned(),
        br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
    )];
    if with_index {
        files.push(("index.json".to_owned(), index));
    }
    files.push((blob_path(&config), config));
    files.push((blob_path(&layer), layer));
    files.push((blob_path(&manifest), manifest.clone()));
    (tar_of(&files), manifest)
}

struct Release {
    dir: TestDir,
    amd64_manifest: Vec<u8>,
    arm64_manifest: Vec<u8>,
}

fn tarball(dir: &TestDir, platform: &str) -> PathBuf {
    dir.join(format!(
        "hostkeeper-coordinator-{VERSION}-{platform}.image.tar"
    ))
}

/// A scratch directory holding the two image tarballs of the release.
fn release(name: &str) -> Release {
    let dir = dir(name);
    let (amd64, amd64_manifest) = image_tar("amd64", "x", true);
    let (arm64, arm64_manifest) = image_tar("arm64", "a", true);
    std::fs::write(tarball(&dir, "linux-x86_64"), amd64).expect("write amd64");
    std::fs::write(tarball(&dir, "linux-arm64"), arm64).expect("write arm64");
    Release {
        dir,
        amd64_manifest,
        arm64_manifest,
    }
}

fn index_path(dir: &TestDir) -> PathBuf {
    dir.join(format!("hostkeeper-coordinator-{VERSION}.image.json"))
}

fn image_index(dir: &TestDir) -> Output {
    run(
        dir,
        &[
            "release",
            "image-index",
            path_str(dir),
            "--version",
            VERSION,
        ],
    )
}

fn descriptor(manifest: &[u8], arch: &str) -> serde_json::Value {
    serde_json::json!({
        "mediaType": MANIFEST_MEDIA_TYPE,
        "size": manifest.len(),
        "digest": digest_of(manifest),
        "platform": {"architecture": arch, "os": "linux"},
    })
}

#[test]
fn image_index_writes_the_oci_index_naming_both_manifests_with_their_platforms() {
    let r = release("index");
    let o = image_index(&r.dir);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let bytes = std::fs::read(index_path(&r.dir)).expect("the index asset");
    let want = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": INDEX_MEDIA_TYPE,
        "manifests": [
            descriptor(&r.amd64_manifest, "amd64"),
            descriptor(&r.arm64_manifest, "arm64"),
        ],
    }))
    .expect("index");
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        String::from_utf8_lossy(&want)
    );
    assert_eq!(
        stdout(&o),
        format!(
            "{}: the image index of linux/amd64 and linux/arm64, {}\n",
            index_path(&r.dir).display(),
            digest_of(&bytes)
        )
    );
    assert_eq!(stderr(&o), "");
}

#[test]
fn image_index_refuses_to_replace_an_index_that_is_there() {
    let r = release("index-twice");
    assert_eq!(image_index(&r.dir).status.code(), Some(0));
    let first = std::fs::read(index_path(&r.dir)).expect("index");
    let o = image_index(&r.dir);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains(&format!("write {}", index_path(&r.dir).display())),
        "{}",
        stderr(&o)
    );
    assert_eq!(std::fs::read(index_path(&r.dir)).expect("index"), first);
}

#[test]
fn image_index_with_a_tarball_missing_is_a_failure_that_names_it() {
    let r = release("index-missing");
    let gone = tarball(&r.dir, "linux-arm64");
    std::fs::remove_file(&gone).expect("remove");
    let o = image_index(&r.dir);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains(&format!("read {}", gone.display())),
        "{}",
        stderr(&o)
    );
    assert!(!index_path(&r.dir).exists());
}

#[test]
fn image_index_with_a_tarball_that_has_no_manifest_is_a_failure_that_names_it() {
    let r = release("index-no-manifest");
    let (bare, _) = image_tar("arm64", "a", false);
    std::fs::write(tarball(&r.dir, "linux-arm64"), bare).expect("write");
    let o = image_index(&r.dir);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains(&format!(
            "hostkeeper-coordinator-{VERSION}-linux-arm64.image.tar: index.json is missing"
        )),
        "{}",
        stderr(&o)
    );
    assert!(!index_path(&r.dir).exists());
}

#[test]
fn image_index_with_a_tarball_of_the_other_architecture_is_a_failure() {
    let r = release("index-wrong-arch");
    let (wrong, _) = image_tar("arm64", "a", true);
    std::fs::write(tarball(&r.dir, "linux-x86_64"), wrong).expect("write");
    let o = image_index(&r.dir);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains(&format!(
            "hostkeeper-coordinator-{VERSION}-linux-x86_64.image.tar: the manifest is for \"linux\"/\"arm64\", not linux/amd64"
        )),
        "{}",
        stderr(&o)
    );
}

#[test]
fn image_index_and_layout_with_a_bad_version_or_no_version_are_usage_errors() {
    let r = release("usage");
    let d = path_str(&r.dir);
    let out = r.dir.join("layout");
    let cases: [&[&str]; 5] = [
        &["release", "image-index", d, "--version", "1.2"],
        &["release", "image-index", d],
        &[
            "release",
            "image-layout",
            d,
            "--version",
            "v1.2.3",
            "--out",
            path_str(&out),
        ],
        &["release", "image-layout", d, "--version", VERSION],
        &["release", "image-index"],
    ];
    for args in cases {
        let o = run(&r.dir, args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stdout(&o));
    }
    let o = run(&r.dir, cases[0]);
    assert!(
        stderr(&o).contains("not a version: \"1.2\""),
        "{}",
        stderr(&o)
    );
    assert!(!index_path(&r.dir).exists());
    assert!(!out.exists());
}

fn image_layout(r: &Release, out: &Path) -> Output {
    run(
        &r.dir,
        &[
            "release",
            "image-layout",
            path_str(&r.dir),
            "--version",
            VERSION,
            "--out",
            path_str(out),
        ],
    )
}

#[test]
fn image_layout_writes_the_blobs_and_an_index_that_names_the_assets_digest_under_the_version() {
    let r = release("layout");
    assert_eq!(image_index(&r.dir).status.code(), Some(0));
    let asset = std::fs::read(index_path(&r.dir)).expect("index");
    let out = r.dir.join("layout");
    let o = image_layout(&r, &out);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    // Two manifests, two configs, two layers and the image index.
    assert_eq!(
        stdout(&o),
        format!(
            "{}: the image {VERSION} ({}), 7 blobs\n",
            out.display(),
            digest_of(&asset)
        )
    );
    assert_eq!(
        std::fs::read_to_string(out.join("oci-layout")).expect("oci-layout"),
        r#"{"imageLayoutVersion":"1.0.0"}"#
    );
    let top: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("index.json")).expect("index.json"))
            .expect("JSON");
    assert_eq!(
        top,
        serde_json::json!({
            "schemaVersion": 2,
            "mediaType": INDEX_MEDIA_TYPE,
            "manifests": [{
                "mediaType": INDEX_MEDIA_TYPE,
                "size": asset.len(),
                "digest": digest_of(&asset),
                "annotations": {"org.opencontainers.image.ref.name": VERSION},
            }],
        })
    );
    let blobs = out.join("blobs").join("sha256");
    let names: Vec<String> = std::fs::read_dir(&blobs)
        .expect("blobs")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 7);
    for name in &names {
        let bytes = std::fs::read(blobs.join(name)).expect("blob");
        assert_eq!(digest_of(&bytes), format!("sha256:{name}"));
    }
    assert_eq!(
        std::fs::read(blobs.join(digest_of(&asset).trim_start_matches("sha256:")))
            .expect("the index blob"),
        asset
    );
    for manifest in [&r.amd64_manifest, &r.arm64_manifest] {
        assert!(names.contains(&digest_of(manifest).trim_start_matches("sha256:").to_owned()));
    }
}

#[test]
fn image_layout_is_refused_when_out_exists_or_the_index_asset_is_absent() {
    let r = release("layout-refused");
    let out = r.dir.join("layout");
    // No index asset yet.
    let o = image_layout(&r, &out);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains(&format!("read {}", index_path(&r.dir).display())),
        "{}",
        stderr(&o)
    );
    assert!(!out.exists());
    // An index asset that is not the one the tarballs make.
    std::fs::write(index_path(&r.dir), b"{}").expect("write");
    let o = image_layout(&r, &out);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains(&format!(
            "hostkeeper-coordinator-{VERSION}.image.json is not the image index of the two image tarballs"
        )),
        "{}",
        stderr(&o)
    );
    assert!(!out.exists());
    // The right asset, but an `out` that is there.
    std::fs::remove_file(index_path(&r.dir)).expect("remove");
    assert_eq!(image_index(&r.dir).status.code(), Some(0));
    std::fs::create_dir(&out).expect("out");
    let o = image_layout(&r, &out);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains(&format!("create {}", out.display())),
        "{}",
        stderr(&o)
    );
    assert_eq!(std::fs::read_dir(&out).expect("out").count(), 0);
}

#[test]
fn image_layout_with_env_file_appends_the_digest_of_the_index_as_image_digest() {
    let r = release("layout-env");
    assert_eq!(image_index(&r.dir).status.code(), Some(0));
    let asset = std::fs::read(index_path(&r.dir)).expect("index");
    let env = r.dir.join("github-env");
    let out = r.dir.join("layout");
    let o = run(
        &r.dir,
        &[
            "release",
            "image-layout",
            path_str(&r.dir),
            "--version",
            VERSION,
            "--out",
            path_str(&out),
            "--env-file",
            path_str(&env),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        std::fs::read_to_string(&env).expect("env file"),
        format!("IMAGE_DIGEST={}\n", digest_of(&asset))
    );
}
