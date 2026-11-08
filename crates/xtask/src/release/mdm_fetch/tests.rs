use std::path::{Path, PathBuf};

use hostkeeper_mdm_core::package::TOOLS;
use hostkeeper_mdm_core::pins::{Pins, PinsError, Tool};
use sha2::{Digest, Sha256};

use super::*;
use crate::release::fakes::{FakeDownload, FakeExec, FakeHistory, scratch};
use crate::release::ports::Adapters;
use crate::release::tests::{text, verb};
use crate::release::{run, run_with};

/// A pin table over three-byte archives named for each tool, so a test knows every byte: the
/// pins and what each URL serves.
pub(crate) fn fixture_pins(serve: impl Fn(Tool) -> Vec<u8>) -> (Pins, FakeDownload) {
    let mut text = String::new();
    let mut download = FakeDownload::default();
    for tool in Tool::ALL {
        let bytes = serve(tool);
        let url = format!("https://example.com/{tool}/v1.0.0/{tool}.tar.gz");
        let sum = hex::encode(Sha256::digest(&bytes));
        text.push_str(&format!("{tool} 1.0.0 {url} {sum}\n"));
        download.served.insert(url, bytes);
    }
    (Pins::parse(&text).expect("the fixture pins"), download)
}

fn bytes_of(tool: Tool) -> Vec<u8> {
    format!("{tool} archive").into_bytes()
}

fn archive(dir: &Path, tool: Tool) -> PathBuf {
    dir.join(archive_name(tool))
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read the directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn every_pinned_archive_of_the_package_is_downloaded_checked_and_named_as_its_url() {
    // Catches: a tool of the package not fetched, MicroMDM's mdmctl fetched though the package
    // does not carry it, an archive written under another name than its URL's, a download that
    // is not first written beside its final name, or one left as `.download`.
    let dir = scratch("mdm-fetch-all");
    let (pins, download) = fixture_pins(bytes_of);
    let lines = fetch_with(&download, &pins, &dir.join("archives")).expect("fetched");
    let archives = dir.join("archives");
    let calls: Vec<(String, PathBuf)> = download.calls.borrow().clone();
    let want: Vec<(String, PathBuf)> = TOOLS
        .iter()
        .map(|t| {
            (
                format!("https://example.com/{t}/v1.0.0/{t}.tar.gz"),
                archives.join(format!("{t}.tar.gz.download")),
            )
        })
        .collect();
    assert_eq!(calls, want);
    assert_eq!(
        names(&archives),
        [
            "caddy.tar.gz",
            "lego.tar.gz",
            "nanodep.tar.gz",
            "nanohub.tar.gz",
            "scepserver.tar.gz"
        ]
    );
    for tool in TOOLS {
        assert_eq!(
            std::fs::read(archive(&archives, tool)).expect("read"),
            bytes_of(tool)
        );
    }
    assert_eq!(lines.len(), 5);
    let sum = hex::encode(Sha256::digest(bytes_of(Tool::Nanohub)));
    assert_eq!(
        lines[0],
        format!(
            "{}: downloaded, sha256 {sum}",
            archive(&archives, Tool::Nanohub).display()
        )
    );
}

#[test]
fn an_archive_already_there_with_its_pins_digest_is_kept_and_not_downloaded_again() {
    // Catches: a rerun that downloads every archive again, or one that keeps an archive whose
    // bytes are not its pin's.
    let dir = scratch("mdm-fetch-kept");
    let (pins, download) = fixture_pins(bytes_of);
    std::fs::write(archive(&dir, Tool::Caddy), bytes_of(Tool::Caddy)).expect("write");
    std::fs::write(archive(&dir, Tool::Lego), b"an older lego").expect("write");
    let lines = fetch_with(&download, &pins, &dir).expect("fetched");
    let fetched: Vec<String> = download
        .calls
        .borrow()
        .iter()
        .map(|c| c.0.clone())
        .collect();
    assert_eq!(fetched.len(), 4);
    assert!(
        !fetched.iter().any(|u| u.contains("/caddy/")),
        "{fetched:?}"
    );
    assert!(fetched.iter().any(|u| u.contains("/lego/")), "{fetched:?}");
    let sum = hex::encode(Sha256::digest(bytes_of(Tool::Caddy)));
    assert!(lines.contains(&format!(
        "{}: kept, sha256 {sum}",
        archive(&dir, Tool::Caddy).display()
    )));
    assert_eq!(
        std::fs::read(archive(&dir, Tool::Lego)).expect("read"),
        bytes_of(Tool::Lego)
    );
}

#[test]
fn a_download_that_is_not_its_pin_is_kept_as_rejected_and_fails_the_verb_naming_it() {
    // Catches: a download whose digest differs from the pin kept under the archive's name (the
    // package would then be built from it, or the next run would keep it), the verb stopping at
    // the first bad archive, or the failure not naming the URL, the two digests and the file.
    let dir = scratch("mdm-fetch-rejected");
    let (pins, mut download) = fixture_pins(bytes_of);
    let url = "https://example.com/nanodep/v1.0.0/nanodep.tar.gz".to_owned();
    download
        .served
        .insert(url.clone(), b"not the pinned bytes".to_vec());
    let err = fetch_with(&download, &pins, &dir).expect_err("refused");
    let got = hex::encode(Sha256::digest(b"not the pinned bytes"));
    let want = pins.get(Tool::Nanodep).sha256_hex();
    let rejected = dir.join("nanodep.tar.gz.rejected");
    assert_eq!(
        err,
        Error::Refused(vec![format!(
            "{url}: its SHA-256 is {got}, the pin of nanodep is {want}; kept as {}",
            rejected.display()
        )])
    );
    assert_eq!(
        std::fs::read(&rejected).expect("read"),
        b"not the pinned bytes"
    );
    assert!(!archive(&dir, Tool::Nanodep).exists());
    // The others were still fetched.
    assert!(archive(&dir, Tool::Lego).is_file());
    assert_eq!(download.calls.borrow().len(), 5);
}

/// The file name of `tool`'s archive in the fixture pins.
fn archive_name(tool: Tool) -> String {
    format!("{}.tar.gz", tool.name())
}

#[test]
fn a_download_that_fails_leaves_no_archive_and_fails_the_verb() {
    // Catches: a failed download that leaves a partial file under the archive's name or as
    // `.download`, or a verb that succeeds without every archive.
    let dir = scratch("mdm-fetch-failed");
    let (pins, mut download) = fixture_pins(bytes_of);
    let url = "https://example.com/scepserver/v1.0.0/scepserver.tar.gz";
    download.served.remove(url);
    let partial = dir.join(format!("{}.download", archive_name(Tool::Scepserver)));
    std::fs::write(&partial, b"partial").expect("write");
    let err = fetch_with(&download, &pins, &dir).expect_err("refused");
    assert_eq!(
        err,
        Error::Refused(vec![format!(
            "{}: curl {url}: exit status 22",
            archive(&dir, Tool::Scepserver).display()
        )])
    );
    assert!(!archive(&dir, Tool::Scepserver).exists());
    assert!(!partial.exists());
}

#[test]
fn a_directory_that_cannot_be_made_fails_before_any_download() {
    // Catches: downloads attempted into a path that is not a directory.
    let dir = scratch("mdm-fetch-no-dir");
    let file = dir.join("a-file");
    std::fs::write(&file, b"").expect("write");
    let (pins, download) = fixture_pins(bytes_of);
    let err = fetch_with(&download, &pins, &file.join("archives")).expect_err("refused");
    assert!(
        matches!(&err, Error::Io { what, .. } if what.starts_with("create ")),
        "{err:?}"
    );
    assert!(download.calls.borrow().is_empty());
}

#[test]
fn the_verb_fetches_the_built_in_pins() {
    // Catches: the verb fetching other pins than hostkeeper-mdm-core's pins.txt, or another set
    // of tools than the package's.
    let dir = scratch("mdm-fetch-built-in");
    let download = FakeDownload::default();
    let err = fetch(&download, &dir).expect_err("nothing is served");
    let pins = Pins::built_in().expect("the built-in pins");
    let urls: Vec<String> = download
        .calls
        .borrow()
        .iter()
        .map(|c| c.0.clone())
        .collect();
    let want: Vec<String> = TOOLS.iter().map(|t| pins.get(*t).url.to_string()).collect();
    assert_eq!(urls, want);
    assert!(matches!(err, Error::Refused(lines) if lines.len() == 5));
}

#[test]
fn a_pin_table_that_does_not_read_is_named_as_hostkeeper_mdm_cores() {
    // Catches: a failure of the built-in pins reported without saying whose pins they are.
    assert_eq!(
        pins_failed(PinsError::Missing(Tool::Lego)),
        Error::Failed("the pins of hostkeeper-mdm-core: the pins have no lego".to_owned())
    );
    assert!(built_in_pins().is_ok());
}

#[test]
fn curl_is_run_https_only_with_no_curlrc_and_the_url_after_the_options() {
    // Catches: a download that reads a `.curlrc`, follows a redirect to plain HTTP, accepts TLS
    // below 1.2, takes an HTTP error page as the archive, or reads the URL as an option.
    let url = "https://example.com/v1/a.zip";
    let line = [
        "curl",
        "-q",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--tlsv1.2",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-redirs",
        "5",
        "--output",
        "/d/a.zip.download",
        "--",
        url,
    ];
    let exec = FakeExec::default().on(&line, true, "", "");
    let curl = CurlCli::new(exec);
    assert_eq!(curl.download(url, Path::new("/d/a.zip.download")), Ok(()));
    assert_eq!(
        *curl.exec.calls.borrow(),
        [line.map(str::to_owned).to_vec()]
    );

    let failing = CurlCli::new(FakeExec::default().on(
        &line,
        false,
        "",
        "curl: (22) The requested URL returned error: 404\n",
    ));
    assert_eq!(
        failing.download(url, Path::new("/d/a.zip.download")),
        Err(PortError::new(
            format!("curl {url}"),
            "curl: (22) The requested URL returned error: 404"
        ))
    );
}

#[test]
fn curl_refuses_a_url_that_is_not_https_and_a_path_that_is_not_utf8_and_names_a_missing_curl() {
    // Catches: curl started for a plain-HTTP URL, a path passed lossily, or a missing curl
    // reported without the URL.
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let curl = CurlCli::new(FakeExec::default());
    assert_eq!(
        curl.download("http://example.com/v1/a.zip", Path::new("/d/a")),
        Err(PortError::new(
            "curl http://example.com/v1/a.zip",
            "not an https:// URL"
        ))
    );
    let url = "https://example.com/v1/a.zip";
    assert_eq!(
        curl.download(url, Path::new(OsStr::from_bytes(b"/d/\xff"))),
        Err(PortError::new(
            format!("curl {url}"),
            "the output path is not UTF-8"
        ))
    );
    assert!(curl.exec.calls.borrow().is_empty());
    let err = curl
        .download(url, Path::new("/d/a"))
        .expect_err("not scripted");
    assert_eq!(err.what, format!("curl {url}"));
    assert!(err.why.starts_with("not scripted: curl -q"), "{}", err.why);
}

#[test]
fn package_and_mdm_fetch_reach_the_mdm_verbs_with_their_directory_of_archives() {
    // Catches: `--service mdm` or `--archives` not reaching the package verb (an mdm package
    // refused for want of archives it was given, or built from none), or `mdm-fetch` not
    // reaching the download port with its directory.
    let mut h = FakeHistory::default();
    let c = h.commit(None, &[("VERSION", Some("0.1\n"))]);
    let d = scratch("release-package-mdm");
    let binary = d.join("mdm");
    std::fs::write(&binary, b"the mdm program").expect("write");
    let archives = d.join("archives");
    let out_dir = d.join("out");
    let args = [
        "package",
        "--service",
        "mdm",
        "--platform",
        "linux-x86_64",
        "--version",
        "0.1.2",
        "--commit",
        c.as_str(),
        "--binary",
        text(&binary),
        "--archives",
        text(&archives),
        "--out",
        text(&out_dir),
    ];
    let out = run(verb(&args), || crate::release::fakes::Fakes::over(h));
    // The built-in pins are read and the first archive they name is not in the directory.
    assert!(
        matches!(&out, Err(Error::Io { what, .. })
            if what.starts_with(&format!("read {}", archives.display()))),
        "{out:?}"
    );

    let fakes = crate::release::tests::empty();
    let out = run_with(
        verb(&["mdm-fetch", "--dir", text(&archives)]),
        &fakes.ports(),
    );
    assert!(matches!(out, Err(Error::Refused(lines)) if lines.len() == 5));
    let calls = fakes.download.calls.borrow();
    assert_eq!(calls.len(), 5);
    assert!(calls.iter().all(|(_, file)| file.starts_with(&archives)));
}
