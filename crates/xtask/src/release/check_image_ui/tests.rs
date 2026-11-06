use std::io::Write;
use std::path::PathBuf;

use serde_json::json;
use sha2::{Digest, Sha256};

use super::*;
use crate::release::fakes::scratch;
use crate::testdir::TestDir;

const ROOT: &str = "usr/share/hostkeeper-coordinator/ui";
const INDEX: &[u8] = b"<!doctype html>";
const FLEET: &[u8] = b"export const fleet = 1;";

/// One entry of a layer: its path, its type, its mode, its owner and its bytes.
struct Entry {
    path: String,
    kind: tar::EntryType,
    mode: u32,
    owner: u64,
    data: Vec<u8>,
}

fn dir(path: &str) -> Entry {
    Entry {
        path: format!("{path}/"),
        kind: tar::EntryType::Directory,
        mode: 0o755,
        owner: 0,
        data: Vec::new(),
    }
}

fn file(path: &str, data: &[u8]) -> Entry {
    Entry {
        path: path.to_owned(),
        kind: tar::EntryType::Regular,
        mode: 0o644,
        owner: 0,
        data: data.to_vec(),
    }
}

/// A tar of `entries`.
fn layer(entries: &[Entry]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry.kind);
        header.set_mode(entry.mode);
        header.set_uid(entry.owner);
        header.set_gid(entry.owner);
        header.set_size(entry.data.len() as u64);
        if entry.kind == tar::EntryType::Symlink {
            header.set_link_name("elsewhere").expect("a link name");
        }
        builder
            .append_data(&mut header, &entry.path, entry.data.as_slice())
            .expect("append");
    }
    builder.into_inner().expect("tar")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).expect("gzip");
    encoder.finish().expect("gzip")
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// An OCI image layout tarball whose one manifest lists `layers` in order, each named by
/// `digest` (`sha256:<hex of its bytes>` unless given).
fn image_of(layers: &[(Vec<u8>, Option<&str>)]) -> Vec<u8> {
    let digests: Vec<String> = layers
        .iter()
        .map(|(bytes, named)| named.map_or_else(|| format!("sha256:{}", sha(bytes)), str::to_owned))
        .collect();
    let manifest = json!({
        "schemaVersion": 2,
        "layers": digests.iter().map(|d| json!({"digest": d})).collect::<Vec<_>>(),
    })
    .to_string();
    let index = json!({"manifests": [{"digest": format!("sha256:{}", sha(manifest.as_bytes()))}]})
        .to_string();
    let mut files = vec![
        file("oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#),
        file("index.json", index.as_bytes()),
        file(
            &format!("blobs/sha256/{}", sha(manifest.as_bytes())),
            manifest.as_bytes(),
        ),
    ];
    for (bytes, _) in layers {
        files.push(file(&format!("blobs/sha256/{}", sha(bytes)), bytes));
    }
    layer(&files)
}

/// The base and the web app as the Dockerfile makes them: a base layer (plain tar) and the
/// web app's layer (gzip), `index.html` and `pages/fleet.js`.
fn good_layers() -> Vec<(Vec<u8>, Option<&'static str>)> {
    let base = layer(&[dir("usr"), dir("usr/bin"), file("usr/bin/x", b"base")]);
    let app = gzip(&layer(&[
        dir("usr/share/hostkeeper-coordinator"),
        dir(ROOT),
        file(&format!("{ROOT}/index.html"), INDEX),
        dir(&format!("{ROOT}/pages")),
        file(&format!("{ROOT}/pages/fleet.js"), FLEET),
    ]));
    vec![(base, None), (app, None)]
}

/// A scratch directory holding `ui/` (`index.html` and `pages/fleet.js`) and the image
/// tarball `bytes` as `image.tar`; answers both paths.
fn setup(name: &str, bytes: &[u8]) -> (TestDir, PathBuf, PathBuf) {
    let dir = scratch(name);
    let ui = dir.join("ui");
    std::fs::create_dir_all(ui.join("pages")).expect("ui");
    std::fs::write(ui.join("index.html"), INDEX).expect("written");
    std::fs::write(ui.join("pages/fleet.js"), FLEET).expect("written");
    let image = dir.join("image.tar");
    std::fs::write(&image, bytes).expect("written");
    (dir, image, ui)
}

/// The reasons `check_image_ui` refuses the image of `layers` against the `ui/` of [`setup`].
fn refusal(name: &str, layers: &[(Vec<u8>, Option<&str>)]) -> Vec<String> {
    let (_dir, image, ui) = setup(name, &image_of(layers));
    match check_image_ui(&image, &ui) {
        Err(Error::Refused(problems)) => problems,
        other => panic!("not refused for every reason: {other:?}"),
    }
}

/// `good_layers` with a third layer of `entries` (plain tar).
fn with_layer(entries: &[Entry]) -> Vec<(Vec<u8>, Option<&'static str>)> {
    let mut layers = good_layers();
    layers.push((layer(entries), None));
    layers
}

fn at(relative: &str) -> String {
    format!("{IMAGE_UI_DIR}/{relative}")
}

#[test]
fn an_image_holding_the_web_app_as_ui_has_it_passes() {
    // Catches: an image that holds the web app refused, a gzip layer not read, or the layers
    // not applied in order (the base's files are outside the web app and are not counted).
    let (_dir, image, ui) = setup("ui-good", &image_of(&good_layers()));
    assert_eq!(
        check_image_ui(&image, &ui),
        Ok(vec![format!(
            "{}: {IMAGE_UI_DIR} holds the 2 files of {}, owned by root, files 644 and directories 755",
            image.display(),
            ui.display()
        )])
    );
}

#[test]
fn a_file_missing_changed_extra_or_of_another_kind_is_refused_by_its_path() {
    // Catches: an image released without a file of the web app, with a stale copy of one, with
    // a file the commit's ui/ does not hold, or with a link where a file or directory must be.
    let ui = |name: &str| format!("ui/{name}");
    let missing = with_layer(&[file(&format!("{ROOT}/.wh.index.html"), b"")]);
    let problems = refusal("ui-missing", &missing);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!("{} is missing: ", at("index.html"))),
        "{problems:?}"
    );
    assert!(
        problems[0].ends_with(&format!("{} is not in the image", ui("index.html"))),
        "{problems:?}"
    );

    let changed = with_layer(&[file(&format!("{ROOT}/index.html"), b"stale")]);
    let problems = refusal("ui-changed", &changed);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!(
            "{} is not a file with the bytes of ",
            at("index.html")
        )),
        "{problems:?}"
    );

    let extra = with_layer(&[file(&format!("{ROOT}/debug.js"), b"x")]);
    let problems = refusal("ui-extra", &extra);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!("{} is in the image but not in ", at("debug.js"))),
        "{problems:?}"
    );

    let link = Entry {
        kind: tar::EntryType::Symlink,
        mode: 0o777,
        ..file(&format!("{ROOT}/index.html"), b"")
    };
    let problems = refusal("ui-link", &with_layer(&[link]));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!(
            "{} is not a file with the bytes of ",
            at("index.html")
        )),
        "{problems:?}"
    );

    // `pages` replaced by a file of that name: it is not a directory, and what was below it
    // is gone with it.
    let flat = with_layer(&[file(&format!("{ROOT}/pages"), b"x")]);
    let problems = refusal("ui-flat", &flat);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!("{} is not a directory, as ", at("pages"))),
        "{problems:?}"
    );
    assert!(
        problems[1].starts_with(&format!("{} is missing: ", at("pages/fleet.js"))),
        "{problems:?}"
    );
}

#[test]
fn an_owner_other_than_root_or_a_mode_other_than_0644_and_0755_is_refused() {
    // Catches: a web app the coordinator's user could change (owned by it, or writable by group
    // or others), or a directory it could not enter.
    let layers = with_layer(&[
        Entry {
            owner: 65532,
            ..file(&format!("{ROOT}/index.html"), INDEX)
        },
        Entry {
            mode: 0o664,
            ..file(&format!("{ROOT}/pages/fleet.js"), FLEET)
        },
        Entry {
            mode: 0o700,
            ..dir(&format!("{ROOT}/pages"))
        },
        Entry {
            mode: 0o444,
            ..dir(ROOT)
        },
    ]);
    assert_eq!(
        refusal("ui-modes", &layers),
        vec![
            format!("{IMAGE_UI_DIR} has mode 0444, not 0755"),
            format!(
                "{} is owned by 65532:65532, not by root (0:0)",
                at("index.html")
            ),
            format!("{} has mode 0700, not 0755", at("pages")),
            format!("{} has mode 0664, not 0644", at("pages/fleet.js")),
        ]
    );
}

#[test]
fn an_image_without_the_directory_is_refused_once() {
    // Catches: an image built from a Dockerfile that copies the web app elsewhere, or not at
    // all, passing; or a whiteout or opaque marker of a later layer that removes the web app
    // not applied.
    let want = vec![format!("the image has no directory {IMAGE_UI_DIR}")];
    let base = layer(&[dir("usr"), dir("usr/bin"), file("usr/bin/x", b"base")]);
    assert_eq!(refusal("ui-none", &[(base, None)]), want);
    let file_there = with_layer(&[file(ROOT, b"x")]);
    assert_eq!(refusal("ui-file", &file_there), want);
    let removed = with_layer(&[file(".wh.usr", b"")]);
    assert_eq!(refusal("ui-removed", &removed), want);
    let emptied = with_layer(&[file("usr/share/hostkeeper-coordinator/.wh..wh..opq", b"")]);
    assert_eq!(refusal("ui-emptied", &emptied), want);
    let everything = with_layer(&[file(".wh..wh..opq", b"")]);
    assert_eq!(refusal("ui-everything", &everything), want);
    // A link over an ancestor directory takes the web app below it away.
    let linked = with_layer(&[Entry {
        kind: tar::EntryType::Symlink,
        mode: 0o777,
        ..file("usr/share", b"")
    }]);
    assert_eq!(refusal("ui-linked-away", &linked), want);
}

#[test]
fn an_unreadable_image_or_ui_is_a_failure_naming_it() {
    // Catches: an image that cannot be read reported as one that holds the web app, or a
    // web app read with something other than plain files and directories in it.
    let (dir, image, ui) = setup("ui-unreadable", b"not a tar");
    let whose = |why: &str| Err(Error::Failed(format!("{}: {why}", image.display())));
    assert_eq!(
        check_image_ui(&image, &ui),
        whose("it is not a readable tar archive")
    );

    let cases = [
        (layer(&[]), "oci-layout is missing"),
        (
            image_of(&[(b"not a tar at all".repeat(64), None)]),
            "layer 0: it is not a readable tar archive",
        ),
        (
            image_of(&[([&ZSTD_MAGIC[..], b"frame"].concat(), None)]),
            "layer 0: it is compressed with zstd, which this check does not read",
        ),
        (
            image_of(&[(layer(&[]), Some("md5:00"))]),
            "layer 0: \"md5:00\" is not a sha256 digest",
        ),
        (
            image_of(&[(layer(&[]), Some("sha256:00"))]),
            "layer 0: blobs/sha256/00 is missing",
        ),
    ];
    for (bytes, why) in cases {
        std::fs::write(&image, bytes).expect("written");
        assert_eq!(check_image_ui(&image, &ui), whose(why));
    }

    let absent = dir.join("absent.tar");
    let Err(Error::Io { what, .. }) = check_image_ui(&absent, &ui) else {
        panic!("a missing image read")
    };
    assert_eq!(what, format!("read {}", absent.display()));

    std::fs::write(&image, image_of(&good_layers())).expect("written");
    let no_ui = dir.join("no-ui");
    let Err(Error::Io { what, .. }) = check_image_ui(&image, &no_ui) else {
        panic!("a missing ui read")
    };
    assert_eq!(what, format!("read {}", no_ui.display()));

    let link = ui.join("pages/link.js");
    std::os::unix::fs::symlink("fleet.js", &link).expect("a link");
    assert_eq!(
        check_image_ui(&image, &ui),
        Err(Error::Failed(format!(
            "{} is neither a file nor a directory: the web app is files and directories only",
            link.display()
        )))
    );
}

#[test]
fn the_dockerfile_copies_the_web_app_to_the_checked_path() {
    // Catches: the Dockerfile copying ui/ to one path while the image check (and the
    // coordinator's default) look at another.
    let text = include_str!("../../../../../packaging/container/Dockerfile");
    let copies: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("COPY ui "))
        .collect();
    assert_eq!(copies, [IMAGE_UI_DIR]);
}
