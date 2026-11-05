use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::release::fakes::scratch;

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A tar of `files`, every entry dated `mtime`.
fn tar_of(files: &[(&str, &[u8])], mtime: u64) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_mtime(mtime);
        header.set_size(data.len() as u64);
        builder
            .append_data(&mut header, path, *data)
            .expect("append");
    }
    builder.into_inner().expect("tar")
}

/// An OCI image layout tarball with `layers`, a config, and the `extra` files. Returns it with
/// the digest of each layer as the manifest spells it.
fn image_with(
    layers: &[&[u8]],
    config: &[u8],
    extra: &[(&str, &[u8])],
    mtime: u64,
) -> (Vec<u8>, Vec<String>) {
    let digests: Vec<String> = layers
        .iter()
        .map(|l| format!("sha256:{}", sha(l)))
        .collect();
    let manifest = json!({
        "schemaVersion": 2,
        "config": {"digest": format!("sha256:{}", sha(config))},
        "layers": digests.iter().map(|d| json!({"digest": d})).collect::<Vec<_>>(),
    })
    .to_string();
    let index = json!({
        "schemaVersion": 2,
        "manifests": [{"digest": format!("sha256:{}", sha(manifest.as_bytes()))}],
    })
    .to_string();
    let blob = |data: &[u8]| format!("blobs/sha256/{}", sha(data));
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (
            "oci-layout".to_owned(),
            br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
        ),
        ("index.json".to_owned(), index.into_bytes()),
        (blob(manifest.as_bytes()), manifest.as_bytes().to_vec()),
        (blob(config), config.to_vec()),
    ];
    files.extend(layers.iter().map(|l| (blob(l), l.to_vec())));
    files.extend(extra.iter().map(|(p, d)| ((*p).to_owned(), d.to_vec())));
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, d)| (p.as_str(), d.as_slice()))
        .collect();
    (tar_of(&refs, mtime), digests)
}

fn image(layers: &[&[u8]], config: &[u8]) -> (Vec<u8>, Vec<String>) {
    image_with(layers, config, &[], 1)
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write a tarball");
    path
}

/// One file of a tarball, for `tar_of`.
fn e<'a>(name: &'a str, data: &'a [u8]) -> (&'a str, &'a [u8]) {
    (name, data)
}

const L0: &[u8] = b"layer zero";
const L1: &[u8] = b"layer one";
const CONFIG: &[u8] = br#"{"os":"linux"}"#;

#[test]
fn reproducible_coordinator_image_builds_compare_equal() {
    // Catches: two builds from the same inputs that compare unequal, or two images with a
    // changed layer that compare equal.
    let dir = scratch("compare-equal");
    let (one, _) = image(&[L0, L1], CONFIG);
    let (two, _) = image(&[L0, L1], CONFIG);
    let a = put(&dir, "a.tar", &one);
    let b = put(&dir, "b.tar", &two);
    assert_eq!(
        compare_images(&a, &b),
        Ok(vec![format!(
            "{} and {} are byte for byte equal: {} bytes, sha256 {}",
            a.display(),
            b.display(),
            one.len(),
            sha(&one)
        )])
    );

    let (changed, digests) = image(&[L0, b"layer one, changed"], CONFIG);
    let (_, original) = image(&[L0, L1], CONFIG);
    let c = put(&dir, "c.tar", &changed);
    assert_ne!(original[1], digests[1]);
    assert_eq!(
        compare_images(&a, &c),
        Err(Error::Failed(format!(
            "the images differ at layer 1: {} in {}, {} in {}",
            original[1],
            a.display(),
            digests[1],
            c.display()
        )))
    );
}

#[test]
fn bytes_that_are_not_an_image_compare_equal_when_they_are_equal() {
    // Catches: a comparison that parses the images before comparing the bytes.
    let dir = scratch("compare-equal-garbage");
    let a = put(&dir, "a.tar", b"not a tar");
    let b = put(&dir, "b.tar", b"not a tar");
    assert_eq!(
        compare_images(&a, &b),
        Ok(vec![format!(
            "{} and {} are byte for byte equal: 9 bytes, sha256 {}",
            a.display(),
            b.display(),
            sha(b"not a tar")
        )])
    );
}

#[test]
fn a_shorter_layer_list_differs_at_the_first_missing_layer() {
    // Catches: a layer list that is compared only as far as the shorter one.
    let dir = scratch("compare-shorter");
    let (long, digests) = image(&[L0, L1], CONFIG);
    let (short, _) = image(&[L0], CONFIG);
    let a = put(&dir, "a.tar", &long);
    let b = put(&dir, "b.tar", &short);
    assert_eq!(
        compare_images(&a, &b),
        Err(Error::Failed(format!(
            "the images differ at layer 1: {} in {}, none in {}",
            digests[1],
            a.display(),
            b.display()
        )))
    );
    assert_eq!(
        compare_images(&b, &a),
        Err(Error::Failed(format!(
            "the images differ at layer 1: none in {}, {} in {}",
            b.display(),
            digests[1],
            a.display()
        )))
    );
}

#[test]
fn equal_layers_with_another_config_name_the_first_differing_file() {
    // Catches: a difference outside the layers that is reported as no difference, or without a
    // path to look at.
    let dir = scratch("compare-config");
    let (one, _) = image(&[L0], CONFIG);
    let other_config: &[u8] = br#"{"os":"linux","user":"x"}"#;
    let (two, _) = image(&[L0], other_config);
    let a = put(&dir, "a.tar", &one);
    let b = put(&dir, "b.tar", &two);
    let blob = |data: &[u8]| format!("blobs/sha256/{}", sha(data));
    let manifest = |config: &[u8]| {
        json!({
            "schemaVersion": 2,
            "config": {"digest": format!("sha256:{}", sha(config))},
            "layers": [{"digest": format!("sha256:{}", sha(L0))}],
        })
        .to_string()
    };
    let mut differing = [
        blob(CONFIG),
        blob(other_config),
        blob(manifest(CONFIG).as_bytes()),
        blob(manifest(other_config).as_bytes()),
    ];
    differing.sort();
    assert_eq!(
        compare_images(&a, &b),
        Err(Error::Failed(format!(
            "the images have equal layers but differ at {}: {} and {} hold other bytes there",
            differing[0],
            a.display(),
            b.display()
        )))
    );
}

#[test]
fn a_file_only_one_image_holds_is_named() {
    // Catches: an extra file in one tarball that goes unnoticed because the layers are equal.
    let dir = scratch("compare-extra");
    let (one, _) = image(&[L0], CONFIG);
    let (two, _) = image_with(&[L0], CONFIG, &[e("extra.txt", b"x")], 1);
    let a = put(&dir, "a.tar", &one);
    let b = put(&dir, "b.tar", &two);
    assert_eq!(
        compare_images(&a, &b),
        Err(Error::Failed(format!(
            "the images have equal layers but differ at extra.txt: {} and {} hold other bytes there",
            a.display(),
            b.display()
        )))
    );
}

#[test]
fn tarballs_that_differ_only_in_headers_are_reported_as_such() {
    // Catches: a difference in the tar headers alone that is reported as a file difference or
    // as equal.
    let dir = scratch("compare-headers");
    let (one, _) = image_with(&[L0], CONFIG, &[], 1);
    let (two, _) = image_with(&[L0], CONFIG, &[], 2);
    let a = put(&dir, "a.tar", &one);
    let b = put(&dir, "b.tar", &two);
    assert_eq!(
        compare_images(&a, &b),
        Err(Error::Failed(format!(
            "the images have equal layers and equal files but differ in the tar headers or the order of the entries: {} and {}",
            a.display(),
            b.display()
        )))
    );
}

#[test]
fn a_tarball_that_is_not_an_oci_layout_is_reported_with_the_reason() {
    // Catches: an unreadable image that is reported as a plain difference with no cause, or
    // that panics.
    let dir = scratch("compare-unreadable");
    let (good, _) = image(&[L0], CONFIG);
    let good_path = put(&dir, "good.tar", &good);
    let junk = vec![b'x'; 1024];
    let missing_manifest = json!({"manifests": [{"digest": "sha256:abc"}]}).to_string();
    let not_sha = json!({"manifests": [{"digest": "md5:abc"}]}).to_string();
    let index_for = |manifest: &[u8]| {
        json!({"manifests": [{"digest": format!("sha256:{}", sha(manifest))}]}).to_string()
    };
    let json_error = |text: &[u8]| {
        serde_json::from_slice::<Value>(text)
            .expect_err("not JSON")
            .to_string()
    };
    let layout: &[u8] = b"{}";
    let cases: Vec<(Vec<u8>, String)> = vec![
        (junk, "it is not a readable tar archive".to_owned()),
        (tar_of(&[], 1), "oci-layout is missing".to_owned()),
        (
            tar_of(&[e("oci-layout", layout)], 1),
            "index.json is missing".to_owned(),
        ),
        (
            tar_of(&[e("oci-layout", layout), e("index.json", b"nope")], 1),
            format!("index.json is not JSON: {}", json_error(b"nope")),
        ),
        (
            tar_of(&[e("oci-layout", layout), e("index.json", b"{}")], 1),
            "index.json names no manifest".to_owned(),
        ),
        (
            tar_of(
                &[e("oci-layout", layout), e("index.json", not_sha.as_bytes())],
                1,
            ),
            "\"md5:abc\" is not a sha256 digest".to_owned(),
        ),
        (
            tar_of(
                &[
                    e("oci-layout", layout),
                    e("index.json", missing_manifest.as_bytes()),
                ],
                1,
            ),
            "blobs/sha256/abc is missing".to_owned(),
        ),
        (
            {
                let index = index_for(b"nope");
                let path = format!("blobs/sha256/{}", sha(b"nope"));
                tar_of(
                    &[
                        e("oci-layout", layout),
                        e("index.json", index.as_bytes()),
                        e(path.as_str(), b"nope"),
                    ],
                    1,
                )
            },
            format!("the manifest is not JSON: {}", json_error(b"nope")),
        ),
        (
            {
                let index = index_for(b"{}");
                let path = format!("blobs/sha256/{}", sha(b"{}"));
                tar_of(
                    &[
                        e("oci-layout", layout),
                        e("index.json", index.as_bytes()),
                        e(path.as_str(), b"{}"),
                    ],
                    1,
                )
            },
            "the manifest has no layers".to_owned(),
        ),
        (
            {
                let manifest = br#"{"layers":[{}]}"#;
                let index = index_for(manifest);
                let path = format!("blobs/sha256/{}", sha(manifest));
                tar_of(
                    &[
                        e("oci-layout", layout),
                        e("index.json", index.as_bytes()),
                        e(path.as_str(), manifest),
                    ],
                    1,
                )
            },
            "a layer has no digest".to_owned(),
        ),
    ];
    for (n, (bytes, why)) in cases.iter().enumerate() {
        let bad = put(&dir, &format!("bad{n}.tar"), bytes);
        assert_eq!(
            compare_images(&bad, &good_path),
            Err(Error::Failed(format!(
                "the images differ, and their layers could not be read: {}: {why}",
                bad.display()
            ))),
            "case {n}"
        );
    }
    // The second image unreadable: the report names it.
    let bad = put(&dir, "bad.tar", &tar_of(&[e("hello", b"x")], 1));
    assert_eq!(
        compare_images(&good_path, &bad),
        Err(Error::Failed(format!(
            "the images differ, and their layers could not be read: {}: oci-layout is missing",
            bad.display()
        )))
    );
}

#[test]
fn a_leading_dot_slash_in_entry_paths_is_ignored() {
    // Catches: an image written with `./` paths that is read as having no layout.
    let dir = scratch("compare-dot-slash");
    let (plain, _) = image(&[L0], CONFIG);
    let files = tar_files(&plain).expect("files");
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        let name = format!("./{path}");
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder.append(&header, data.as_slice()).expect("append");
    }
    let prefixed = builder.into_inner().expect("tar");
    let (other, digests) = image(&[L0, L1], CONFIG);
    let a = put(&dir, "a.tar", &prefixed);
    let b = put(&dir, "b.tar", &other);
    assert_eq!(
        compare_images(&a, &b),
        Err(Error::Failed(format!(
            "the images differ at layer 1: none in {}, {} in {}",
            a.display(),
            digests[1],
            b.display()
        )))
    );
}

#[test]
fn a_missing_tarball_is_an_io_error_naming_the_path() {
    // Catches: a missing input that is compared as empty, or an error that omits the file.
    let dir = scratch("compare-missing");
    let present = put(&dir, "present.tar", b"x");
    let absent = dir.join("absent.tar");
    let why = std::fs::read(&absent).expect_err("absent").to_string();
    let io = Error::Io {
        what: format!("read {}", absent.display()),
        why,
    };
    assert_eq!(compare_images(&absent, &present), Err(io.clone()));
    assert_eq!(compare_images(&present, &absent), Err(io));
}
