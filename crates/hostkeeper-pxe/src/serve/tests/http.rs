//! HTTP, end to end: every sealed file at `/` and its name, the fixed `grub.cfg` and the menus
//! under `/grub/`, by `GET` and `HEAD` only; everything else refused.

use hostkeeper_pxe_seed::inputs::LinuxPlatform;
use hostkeeper_pxe_seed::menu::LOCAL_DISK_MENU;

use super::kit::{
    ENROLLED, ISO_BYTES, Kit, NOW, Running, SHIM_BYTES, client, content, http, iso_name,
};

fn start(name: &str) -> (Kit, Running) {
    let kit = Kit::new(name);
    let sink = client();
    let run = kit.start(kit.args("proxy", sink.local_addr().expect("its address"), &[]));
    (kit, run)
}

/// The value of the header `name` in `head`.
fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// A GET of each sealed file is its exact bytes, with its Content-Length: the kernel, the
/// initrd, the deb, the shim and GRUB of each platform, and the ISO, larger than one chunk,
/// streamed whole. A HEAD gives the same Content-Length and no body. Each request is logged.
///
/// Catches a file cut at a chunk, a body of another file, and a HEAD that sends the ISO.
#[test]
fn every_sealed_file_is_served_whole_by_get_and_sized_by_head() {
    let (_kit, mut run) = start("http-files");
    for platform in LinuxPlatform::ALL {
        let arch = platform.deb_arch();
        let efi = match platform {
            LinuxPlatform::X86_64 => "x64",
            LinuxPlatform::Arm64 => "aa64",
        };
        for (path, part, len) in [
            (format!("/vmlinuz-{platform}"), "kernel", 200),
            (format!("/initrd-{platform}"), "initrd", 400),
            (format!("/deb/hostkeeper_{arch}.deb"), "deb", 300),
            (format!("/hostkeeper/shim{efi}.efi"), "shim", SHIM_BYTES),
            (format!("/hostkeeper/grub{efi}.efi"), "grub", 700),
            (format!("/{}", iso_name(platform)), "iso", ISO_BYTES),
        ] {
            let got = http(run.http, "GET", &path);
            assert_eq!(got.status, 200, "{path}");
            assert_eq!(
                header(&got.head, "content-length"),
                Some(len.to_string().as_str())
            );
            assert!(got.body == content(platform, part, len), "{path}");
            let line = run.wait(&format!(": GET {path}: "));
            assert!(
                line.starts_with(&format!("{NOW} http 127.0.0.1:")),
                "{line}"
            );
            assert!(line.ends_with(&format!(": GET {path}: 200")), "{line}");
        }
    }
    let iso = format!("/{}", iso_name(LinuxPlatform::X86_64));
    let head = http(run.http, "HEAD", &iso);
    assert_eq!(head.status, 200);
    assert_eq!(
        header(&head.head, "content-length"),
        Some(ISO_BYTES.to_string().as_str())
    );
    assert!(head.body.is_empty());
    assert_eq!(run.stop().0, Ok(()));
}

/// The header names of an answer are in title case, as GRUB's HTTP client reads them: it matches
/// `Content-Length: ` byte for byte, and a GRUB that finds none loads no kernel ("out of memory").
///
/// Catches a server whose answers spell `content-length` in lower case, which a browser or curl
/// reads and GRUB does not.
#[test]
fn the_size_of_a_file_is_spelt_as_grub_reads_it() {
    let (_kit, run) = start("http-grub-headers");
    let got = http(run.http, "GET", "/vmlinuz-linux-x86_64");
    assert_eq!(got.status, 200);
    let names: Vec<&str> = got
        .head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':').map(|(name, _)| name))
        .collect();
    assert!(
        got.head.lines().any(|line| line == "Content-Length: 200"),
        "{}",
        got.head
    );
    assert!(
        names.iter().all(|name| name
            .split('-')
            .all(|word| word.chars().next().is_some_and(char::is_uppercase))),
        "every header name is in title case: {names:?}"
    );
    assert_eq!(run.stop().0, Ok(()));
}

/// Every method but GET and HEAD is answered 405 with `Allow: GET, HEAD`; an unknown path, the
/// seal itself (in the directory, but not a sealed file), a path with a query, and `/` are
/// answered 404.
///
/// Catches an HTTP server that accepts a write, serves a file of the directory that was not
/// sealed, or reads a query as part of a name.
#[test]
fn every_other_request_is_refused() {
    let (_kit, mut run) = start("http-refused");
    for method in ["POST", "PUT", "DELETE"] {
        let got = http(run.http, method, "/vmlinuz-linux-x86_64");
        assert_eq!(got.status, 405, "{method}");
        assert_eq!(header(&got.head, "allow"), Some("GET, HEAD"), "{method}");
        assert!(got.body.is_empty());
        assert!(
            run.wait(&format!(": {method} ")).ends_with(": 405"),
            "{method}"
        );
    }
    for path in [
        "/missing",
        "/seal.json",
        "/vmlinuz-linux-x86_64?x=1",
        "/",
        "/seed/not-a-mac/user-data",
    ] {
        let got = http(run.http, "GET", path);
        assert_eq!(got.status, 404, "{path}");
        assert!(got.body.is_empty(), "{path}");
    }
    assert_eq!(run.stop().0, Ok(()));
}

/// The fixed `grub.cfg` and a MAC's menu are served by HTTP too, at the paths GRUB reads them
/// by TFTP, for a GRUB that UEFI HTTP boot loaded.
///
/// Catches an HTTP-booted GRUB that finds no `grub.cfg`.
#[test]
fn grub_cfg_and_the_menus_are_served_by_http() {
    let (_kit, mut run) = start("http-grub");
    let cfg = http(run.http, "GET", "/grub/grub.cfg");
    assert_eq!(cfg.status, 200);
    assert_eq!(
        cfg.body,
        b"configfile ${prefix}/hostkeeper/${grub_cpu}/${net_default_mac}\n"
    );
    let menu = http(
        run.http,
        "GET",
        &format!("/grub/hostkeeper/arm64/{ENROLLED}"),
    );
    assert_eq!(menu.status, 200);
    assert_eq!(menu.body, LOCAL_DISK_MENU.as_bytes());
    run.wait(&format!(
        "menu grub/hostkeeper/arm64/{ENROLLED}: local disk"
    ));
    assert_eq!(run.stop().0, Ok(()));
}
