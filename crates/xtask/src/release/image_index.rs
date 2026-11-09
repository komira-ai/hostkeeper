//! `release image-index` and `release image-layout`: the coordinator's multi-platform image, made
//! of the two image tarballs the build made, byte for byte.
//!
//! Each image tarball `hostkeeper-coordinator-<version>-<linux-platform>.image.tar` is an OCI image
//! layout whose `index.json` names exactly one manifest, of the platform's architecture. The
//! **image index** is the OCI image index (`application/vnd.oci.image.index.v1+json`) that lists
//! those two manifests, linux-x86_64 first, each by its media type, size, digest and platform, as
//! the canonical JSON of [`index_bytes`]: one line, keys in order of their names, no final line
//! break. It is a pure function of the two tarballs, so the build writes it beside them
//! (`image-index`, the release asset `hostkeeper-coordinator-<version>.image.json`), the
//! build's `attest` job attests it like every other asset, and `release check` makes it again and
//! compares. Its SHA-256 is the digest of the multi-platform image in a registry: the attestation of
//! the file is the attestation of that digest, and `docker pull <repository>@sha256:<that digest>`
//! pulls these very manifests and layers, which a registry addresses by their digests.
//!
//! `image-layout` writes the OCI image layout a registry push copies from, with no rebuild: every
//! blob of the two tarballs (each checked against the digest it is named by), the image index as a
//! blob too, checked to be the release's asset byte for byte, and an `index.json` that names the
//! image index under the release's version. `skopeo copy --all --preserve-digests` of that layout
//! (`.github/workflows/release.yml`, job `image`) pushes it, refusing any change of a digest.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::Error;
use super::compare_images::{Files, tar_files};
use super::contract::{COORDINATOR, Platform, Version};

/// The registry repository the `image` job of release.yml pushes the image to, tagged with the
/// release's version.
pub(crate) const IMAGE_REPOSITORY: &str = "ghcr.io/komira-ai/hostkeeper-coordinator";

/// The media type of the image index.
pub(crate) const INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";

/// The platforms of the image, in the order the image index lists them, each with its OCI
/// architecture.
const IMAGE_PLATFORMS: [(Platform, &str); 2] = [
    (Platform::LinuxX86_64, "amd64"),
    (Platform::LinuxArm64, "arm64"),
];

/// The `oci-layout` file of a layout.
const OCI_LAYOUT: &str = r#"{"imageLayoutVersion":"1.0.0"}"#;

/// The name of the image tarball of `platform`.
pub(crate) fn image_name(version: &Version, platform: Platform) -> String {
    format!("{COORDINATOR}-{version}-{}.image.tar", platform.name())
}

/// The name of the image index of the release. It does not end in `-index.json`, which
/// `release channel-sync` takes for a service's release index.
pub(crate) fn index_name(version: &Version) -> String {
    format!("{COORDINATOR}-{version}.image.json")
}

/// The `sha256:<hex>` digest of `bytes`.
pub(crate) fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// One image tarball: its files, and the descriptor of its one manifest as the image index lists
/// it.
struct Image {
    files: Files,
    descriptor: Value,
}

/// Reads the image tarball of `platform` in `dir`, refusing one whose `index.json` does not name
/// exactly one manifest of the platform, held in the tarball at its digest and size.
fn image(
    dir: &Path,
    version: &Version,
    (platform, arch): (Platform, &str),
) -> Result<Image, Error> {
    let name = image_name(version, platform);
    let path = dir.join(&name);
    let bytes = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
    let refuse = |why: String| Error::Failed(format!("{name}: {why}"));
    let files = tar_files(&bytes).map_err(refuse)?;
    let index: Value = files
        .get("index.json")
        .ok_or_else(|| refuse("index.json is missing".to_owned()))
        .and_then(|b| {
            serde_json::from_slice(b).map_err(|e| refuse(format!("index.json is not JSON: {e}")))
        })?;
    let manifests = index["manifests"].as_array().map_or(&[][..], Vec::as_slice);
    let [entry] = manifests else {
        return Err(refuse(format!(
            "index.json names {} manifests, not one",
            manifests.len()
        )));
    };
    let field = |key: &str| {
        entry[key]
            .as_str()
            .ok_or_else(|| refuse(format!("the manifest's {key} is not a string")))
    };
    let media_type = field("mediaType")?;
    let digest = field("digest")?;
    let size = entry["size"]
        .as_u64()
        .ok_or_else(|| refuse("the manifest's size is not a number".to_owned()))?;
    let (os, cpu) = (&entry["platform"]["os"], &entry["platform"]["architecture"]);
    if os != "linux" || cpu != arch {
        return Err(refuse(format!(
            "the manifest is for {os}/{cpu}, not linux/{arch}"
        )));
    }
    let blob = digest
        .strip_prefix("sha256:")
        .and_then(|hex| files.get(&format!("blobs/sha256/{hex}")))
        .ok_or_else(|| refuse(format!("the manifest {digest} is not in the tarball")))?;
    if digest_of(blob) != digest || blob.len() as u64 != size {
        return Err(refuse(format!(
            "the manifest named {digest} of {size} bytes is {} of {} bytes",
            digest_of(blob),
            blob.len()
        )));
    }
    let descriptor = json!({
        "mediaType": media_type,
        "size": size,
        "digest": digest,
        "platform": { "architecture": arch, "os": "linux" },
    });
    Ok(Image { files, descriptor })
}

/// The two images of the release, in the order of [`IMAGE_PLATFORMS`].
fn images(dir: &Path, version: &Version) -> Result<Vec<Image>, Error> {
    IMAGE_PLATFORMS
        .iter()
        .map(|&each| image(dir, version, each))
        .collect()
}

/// The canonical bytes of the image index of `images`.
fn index_of(images: &[Image]) -> Vec<u8> {
    let manifests: Vec<&Value> = images.iter().map(|i| &i.descriptor).collect();
    let index = json!({
        "schemaVersion": 2,
        "mediaType": INDEX_MEDIA_TYPE,
        "manifests": manifests,
    });
    // A `Value` holds only strings, numbers, arrays and objects, which always serialize.
    serde_json::to_vec(&index).unwrap_or_default()
}

/// The image index of the two image tarballs of `version` in `dir`.
pub(crate) fn index_bytes(dir: &Path, version: &Version) -> Result<Vec<u8>, Error> {
    Ok(index_of(&images(dir, version)?))
}

/// `release image-index`: writes the image index into `dir`, which must not hold one yet.
pub(crate) fn write_index(dir: &Path, version: &Version) -> Result<Vec<String>, Error> {
    let bytes = index_bytes(dir, version)?;
    let path = dir.join(index_name(version));
    write_new(&path, &bytes)?;
    Ok(vec![format!(
        "{}: the image index of linux/amd64 and linux/arm64, {}",
        path.display(),
        digest_of(&bytes)
    )])
}

/// `release check`'s part: the image index in `dir` is the one the two tarballs make. Returns its
/// digest.
pub(crate) fn check_index(dir: &Path, version: &Version) -> Result<String, Error> {
    let want = index_bytes(dir, version)?;
    let name = index_name(version);
    let path = dir.join(&name);
    let have = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
    if have != want {
        return Err(Error::Failed(format!(
            "{name} is not the image index of the two image tarballs: it is {}, they make {}",
            digest_of(&have),
            digest_of(&want)
        )));
    }
    Ok(digest_of(&have))
}

/// `release image-layout`: the OCI image layout of the multi-platform image at `out`, which must
/// not exist, as the module says.
pub(crate) fn write_layout(
    dir: &Path,
    version: &Version,
    out: &Path,
    env_file: Option<&Path>,
) -> Result<Vec<String>, Error> {
    let digest = check_index(dir, version)?;
    let index = std::fs::read(dir.join(index_name(version)))
        .map_err(Error::io(format!("read {}", index_name(version))))?;
    let mut blobs: BTreeMap<String, &[u8]> = BTreeMap::new();
    let images = images(dir, version)?;
    for (image, (platform, _)) in images.iter().zip(IMAGE_PLATFORMS) {
        for (path, bytes) in &image.files {
            let Some(hex) = path.strip_prefix("blobs/sha256/").filter(|h| !h.is_empty()) else {
                continue;
            };
            if digest_of(bytes) != format!("sha256:{hex}") {
                return Err(Error::Failed(format!(
                    "{}: the blob {path} holds other bytes ({})",
                    image_name(version, platform),
                    digest_of(bytes)
                )));
            }
            blobs.insert(hex.to_owned(), bytes);
        }
    }
    let index_hex = digest.trim_start_matches("sha256:").to_owned();
    blobs.insert(index_hex, &index);
    std::fs::create_dir(out).map_err(Error::io(format!("create {}", out.display())))?;
    let blob_dir = out.join("blobs").join("sha256");
    std::fs::create_dir_all(&blob_dir)
        .map_err(Error::io(format!("create {}", blob_dir.display())))?;
    for (hex, bytes) in &blobs {
        write_new(&blob_dir.join(hex), bytes)?;
    }
    write_new(&out.join("oci-layout"), OCI_LAYOUT.as_bytes())?;
    let top = json!({
        "schemaVersion": 2,
        "mediaType": INDEX_MEDIA_TYPE,
        "manifests": [{
            "mediaType": INDEX_MEDIA_TYPE,
            "size": index.len(),
            "digest": digest,
            "annotations": { "org.opencontainers.image.ref.name": version.to_string() },
        }],
    });
    write_new(
        &out.join("index.json"),
        &serde_json::to_vec(&top).unwrap_or_default(),
    )?;
    if let Some(env_file) = env_file {
        append_digest(env_file, &digest)?;
    }
    Ok(vec![format!(
        "{}: the image {version} ({digest}), {} blobs",
        out.display(),
        blobs.len()
    )])
}

/// Appends `IMAGE_DIGEST=<digest>` to the env file (`GITHUB_ENV`) a caller names. (release.yml
/// no longer reads it: `release push-image` takes the digest from the image index.)
fn append_digest(env_file: &Path, digest: &str) -> Result<(), Error> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(env_file)
        .and_then(|mut f| f.write_all(format!("IMAGE_DIGEST={digest}\n").as_bytes()))
        .map_err(Error::io(format!("append to {}", env_file.display())))
}

/// Writes `bytes` to the new file `path`, refusing one that exists.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(Error::io(format!("write {}", path.display())))
}

/// An image tarball as buildx writes one, for the tests of the verbs that read a whole release:
/// one manifest of `arch`, whose config and one layer hold `marker`.
#[cfg(test)]
pub(crate) fn fake_image(arch: &str, marker: &str) -> Vec<u8> {
    let config = format!("{{\"architecture\":\"{arch}\",\"marker\":\"{marker}\"}}").into_bytes();
    let layer = format!("layer {marker}").into_bytes();
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {"digest": digest_of(&config), "size": config.len()},
        "layers": [{"digest": digest_of(&layer), "size": layer.len()}],
    }))
    .unwrap_or_default();
    let index = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": INDEX_MEDIA_TYPE,
        "manifests": [{
            "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
            "digest": digest_of(&manifest),
            "size": manifest.len(),
            "platform": {"architecture": arch, "os": "linux"},
        }],
    }))
    .unwrap_or_default();
    let mut builder = tar::Builder::new(Vec::new());
    let blob = |bytes: &[u8]| format!("blobs/sha256/{}", hex::encode(Sha256::digest(bytes)));
    let files = [
        ("oci-layout".to_owned(), OCI_LAYOUT.as_bytes()),
        ("index.json".to_owned(), index.as_slice()),
        (blob(&config), config.as_slice()),
        (blob(&layer), layer.as_slice()),
        (blob(&manifest), manifest.as_slice()),
    ];
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        builder
            .append_data(&mut header, path, data)
            .expect("append");
    }
    builder.into_inner().expect("tar")
}

#[cfg(test)]
mod tests;
