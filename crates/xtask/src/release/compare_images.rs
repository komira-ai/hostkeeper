//! `release compare-images`: two builds of the coordinator image must be byte for byte equal.
//!
//! The check is on the bytes of the two OCI image tarballs. When they differ, the report says
//! where: at the first layer whose digest differs, or, when the layers are equal, at the first
//! file of the tarballs whose bytes differ. The layers are read from the OCI image layout
//! (`oci-layout`, `index.json` and `blobs/sha256/<hex>`): the first manifest of the index, and
//! its list of layers.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Error;

/// The files of an image tarball by path.
type Files = BTreeMap<String, Vec<u8>>;

/// Passes when the tarballs `a` and `b` hold the same bytes; otherwise fails saying where the
/// images first differ.
pub(crate) fn compare_images(a: &Path, b: &Path) -> Result<Vec<String>, Error> {
    let bytes_a = read(a)?;
    let bytes_b = read(b)?;
    if bytes_a != bytes_b {
        return Err(Error::Failed(difference(a, &bytes_a, b, &bytes_b)));
    }
    Ok(vec![format!(
        "{} and {} are byte for byte equal: {} bytes, sha256 {}",
        a.display(),
        b.display(),
        bytes_a.len(),
        hex::encode(Sha256::digest(&bytes_a))
    )])
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(Error::io(format!("read {}", path.display())))
}

/// The files of the tarball `path` and the digests of its layers, or why they cannot be read.
fn image(path: &Path, bytes: &[u8]) -> Result<(Files, Vec<String>), String> {
    let whose = |why: String| format!("{}: {why}", path.display());
    let files = tar_files(bytes).map_err(whose)?;
    let layers = layers(&files).map_err(whose)?;
    Ok((files, layers))
}

/// Where the two different tarballs first differ, as a sentence.
fn difference(a: &Path, bytes_a: &[u8], b: &Path, bytes_b: &[u8]) -> String {
    let (files_a, layers_a, files_b, layers_b) = match (image(a, bytes_a), image(b, bytes_b)) {
        (Ok((files_a, layers_a)), Ok((files_b, layers_b))) => {
            (files_a, layers_a, files_b, layers_b)
        }
        (Err(why), _) | (_, Err(why)) => {
            return format!("the images differ, and their layers could not be read: {why}");
        }
    };
    let count = layers_a.len().max(layers_b.len());
    if let Some(i) = (0..count).find(|&i| layers_a.get(i) != layers_b.get(i)) {
        let digest = |layers: &[String]| layers.get(i).map_or("none", String::as_str).to_owned();
        return format!(
            "the images differ at layer {i}: {} in {}, {} in {}",
            digest(&layers_a),
            a.display(),
            digest(&layers_b),
            b.display()
        );
    }
    let paths: BTreeSet<&String> = files_a.keys().chain(files_b.keys()).collect();
    match paths
        .into_iter()
        .find(|p| files_a.get(*p) != files_b.get(*p))
    {
        Some(path) => format!(
            "the images have equal layers but differ at {path}: {} and {} hold other bytes there",
            a.display(),
            b.display()
        ),
        None => format!(
            "the images have equal layers and equal files but differ in the tar headers or the order of the entries: {} and {}",
            a.display(),
            b.display()
        ),
    }
}

/// Every entry of the tarball by path (a leading `./` removed).
fn tar_files(bytes: &[u8]) -> Result<Files, String> {
    let unreadable = |_: std::io::Error| "it is not a readable tar archive".to_owned();
    let mut files = Files::new();
    let mut archive = tar::Archive::new(bytes);
    for entry in archive.entries().map_err(unreadable)? {
        let mut entry = entry.map_err(unreadable)?;
        let path = entry
            .path()
            .map_err(unreadable)?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_owned();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(unreadable)?;
        files.insert(path, data);
    }
    Ok(files)
}

/// The file `name` of the image, parsed as JSON.
fn parse_json(files: &Files, name: &str, what: &str) -> Result<Value, String> {
    let bytes = files
        .get(name)
        .ok_or_else(|| format!("{name} is missing"))?;
    serde_json::from_slice(bytes).map_err(|e| format!("{what} is not JSON: {e}"))
}

/// The digests of the layers of the first manifest of the image, in order.
fn layers(files: &Files) -> Result<Vec<String>, String> {
    if !files.contains_key("oci-layout") {
        return Err("oci-layout is missing".to_owned());
    }
    let index = parse_json(files, "index.json", "index.json")?;
    let digest = index["manifests"][0]["digest"]
        .as_str()
        .ok_or_else(|| "index.json names no manifest".to_owned())?;
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("{digest:?} is not a sha256 digest"))?;
    let manifest = parse_json(files, &format!("blobs/sha256/{hex}"), "the manifest")?;
    let list = manifest["layers"]
        .as_array()
        .ok_or_else(|| "the manifest has no layers".to_owned())?;
    list.iter()
        .map(|layer| {
            let digest = layer["digest"].as_str();
            digest
                .map(str::to_owned)
                .ok_or_else(|| "a layer has no digest".to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests;
