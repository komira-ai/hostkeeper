//! `packaging/alpine/nfpm.yaml`: its keys, the files it puts in the apk, its dependency and
//! scripts, and the names of its files.

use std::collections::BTreeMap;
use std::path::PathBuf;

use hostkeeper_daemon::cli::{GH_BINARY, SIGSTORE_TRUSTED_ROOT};

use super::{APK_NFPM, APK_POSTINSTALL, APK_PREREMOVE, DEPENDS, read_alpine};
use crate::nfpm::{self, Config};
use crate::package::package_files;
use crate::{altered, refuses};

/// Whether `src`, a path from the repository root, is a file of the repository.
fn in_repository(src: &str) -> bool {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(src)
        .is_file()
}

/// The keys of the Linux packages' nfpm.yaml, and `depends`: the one key the apk has that they
/// do not. A signing section, a repository, an override for one packager or a version suffix is
/// none of them, and a value nfpm decodes as text is text.
fn check_keys(text: &str) -> Result<(), String> {
    let config = Config::parse(text)?;
    let unknown: Vec<String> = config
        .unknown_keys()?
        .into_iter()
        .filter(|key| key != "depends")
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "nfpm.yaml holds keys it does not need: {unknown:?}"
        ));
    }
    let bad = config.bad_values();
    if !bad.is_empty() {
        return Err(format!(
            "nfpm.yaml holds values nfpm does not take: {bad:?}"
        ));
    }
    Ok(())
}

/// Catches a signing or repository section, a key for one packager only, a version suffix, a
/// value nfpm decodes as text written as a mapping, a list or null, a platform other than
/// Linux, and a contents entry of another type than a link.
#[test]
fn apk_nfpm_config_holds_only_the_keys_it_needs() {
    let text = read_alpine(APK_NFPM);
    assert_eq!(check_keys(&text), Ok(()));
    let maintainer =
        "maintainer: \"The hostkeeper project <https://github.com/komira-ai/hostkeeper>\"";
    let cases = [
        (
            altered(&text, maintainer, "maintainer: {}"),
            "[\"maintainer is not text\"]",
        ),
        (
            altered(&text, "license: Apache-2.0", "license:"),
            "[\"license is not text\"]",
        ),
        (
            altered(&text, "platform: linux", "platform: darwin"),
            "[\"platform is \\\"darwin\\\", not linux\"]",
        ),
        (
            format!("{text}apk:\n  signature:\n    key_file: release.rsa\n"),
            "[\"apk\"]",
        ),
        (
            format!("{text}overrides:\n  apk:\n    depends: [curl]\n"),
            "[\"overrides\"]",
        ),
        (format!("{text}release: \"2\"\n"), "[\"release\"]"),
        (
            altered(
                &text,
                "  preremove:",
                "  postremove: packaging/alpine/scripts/postremove.sh\n  preremove:",
            ),
            "[\"scripts.postremove\"]",
        ),
        (
            altered(
                &text,
                "    dst: /usr/bin/hostkeeper\n",
                "    dst: /usr/bin/hostkeeper\n    type: config\n",
            ),
            "a contents entry's type is not symlink",
        ),
        (
            altered(
                &text,
                "    dst: /usr/bin/hostkeeper\n    file_info:\n      mode: 0755\n",
                "    dst: /usr/bin/hostkeeper\n    file_info:\n      mode: 0755\n      mtime: \"2027-01-01T00:00:00Z\"\n",
            ),
            "[\"contents[].file_info.mtime\"]",
        ),
    ];
    refuses(check_keys, &cases);
}

/// The files of the apk, sorted as [`Config::files`] sorts them: the three it shares with the
/// Linux packages (the config template, `gh` and the trusted root, at the daemon's own paths and
/// from where the Linux packages take them), the static musl daemon and the init script.
fn wanted_files() -> Vec<nfpm::File> {
    let shared = [
        "/usr/share/hostkeeper/hostkeeper.toml.template",
        GH_BINARY,
        SIGSTORE_TRUSTED_ROOT,
    ];
    let mut files: Vec<nfpm::File> = package_files()
        .into_iter()
        .filter(|file| shared.contains(&file.dst.as_str()))
        .collect();
    assert_eq!(files.len(), shared.len(), "{files:?}");
    let own = |dst: &str, src: &str| nfpm::File {
        dst: dst.to_owned(),
        src: src.to_owned(),
        mode: "0755".to_owned(),
        owner: "root".to_owned(),
        group: "root".to_owned(),
    };
    files.push(own("/usr/bin/hostkeeper", "target/nfpm/hostkeeper"));
    files.push(own(
        "/etc/init.d/hostkeeper",
        "packaging/alpine/hostkeeper.initd",
    ));
    files.sort();
    files
}

/// The apk carries exactly [`wanted_files`] and no link, depends on exactly [`DEPENDS`], runs
/// exactly the two scripts, and every source of the repository it names is there.
fn check_package(text: &str, exists: impl Fn(&str) -> bool) -> Result<(), String> {
    let config = Config::parse(text)?;
    let files = config.files("apk")?;
    let wanted = wanted_files();
    if files != wanted {
        return Err(format!("the apk carries {files:?}, not {wanted:?}"));
    }
    let links = config.links("apk")?;
    if !links.is_empty() {
        return Err(format!("the apk carries the links {links:?}, and no link"));
    }
    let depends = config.depends()?;
    if depends != DEPENDS {
        return Err(format!("the apk depends on {depends:?}, not {DEPENDS:?}"));
    }
    let scripts = config.scripts("apk")?;
    let wanted: BTreeMap<String, String> = [
        ("postinstall", APK_POSTINSTALL),
        ("preremove", APK_PREREMOVE),
    ]
    .into_iter()
    .map(|(name, path)| (name.to_owned(), format!("packaging/alpine/{path}")))
    .collect();
    if scripts != wanted {
        return Err(format!(
            "the apk runs the scripts {scripts:?}, not {wanted:?}"
        ));
    }
    for src in files.iter().map(|file| &file.src).chain(scripts.values()) {
        if src.starts_with("packaging/") && !exists(src) {
            return Err(format!(
                "{src}, a source of the apk, is not in the repository"
            ));
        }
    }
    Ok(())
}

/// One more `contents` entry, before `scripts:`.
fn with_content(text: &str, src: &str, dst: &str) -> String {
    altered(
        text,
        "scripts:\n",
        &format!(
            "  - src: {src}\n    dst: {dst}\n    file_info:\n      mode: 0644\n      owner: root\n      group: root\nscripts:\n"
        ),
    )
}

/// Catches an apk that carries another file than the daemon, its init script, the config
/// template, `gh` and the trusted root (the systemd unit, the tmpfiles rules, `hostkeeper-pxe`,
/// a key, a certificate or a config among them), a link, a file at another mode or owner, a
/// source that is not in the repository, a dependency on an OpenRC older than 0.62 or on none,
/// and a script that is not the Alpine one.
#[test]
fn apk_carries_the_daemon_its_init_script_and_the_files_it_checks_packages_with() {
    let text = read_alpine(APK_NFPM);
    assert_eq!(check_package(&text, in_repository), Ok(()));
    let check = |t: &str| check_package(t, |_| true);
    let initd = "    dst: /etc/init.d/hostkeeper\n    file_info:\n      mode: 0755\n";
    let daemon = "    dst: /usr/bin/hostkeeper\n    file_info:\n      mode: 0755\n      owner: root\n      group: root\n";
    let cases = [
        (
            altered(&text, initd, &initd.replace("0755", "0644")),
            "the apk carries",
        ),
        (
            altered(
                &text,
                daemon,
                &daemon.replace("owner: root", "owner: nobody"),
            ),
            "the apk carries",
        ),
        (
            altered(
                &text,
                daemon,
                &daemon.replace("group: root", "group: wheel"),
            ),
            "the apk carries",
        ),
        (
            altered(
                &text,
                "src: target/nfpm/hostkeeper",
                "src: target/release/hostkeeper",
            ),
            "the apk carries",
        ),
        (
            altered(
                &text,
                "src: packaging/alpine/hostkeeper.initd",
                "src: packaging/linux/hostkeeper.service",
            ),
            "the apk carries",
        ),
        (
            altered(&text, "src: target/nfpm/gh", "src: /usr/bin/gh"),
            "the apk carries",
        ),
        (
            with_content(
                &text,
                "packaging/linux/hostkeeper.service",
                "/usr/lib/systemd/system/hostkeeper.service",
            ),
            "the apk carries",
        ),
        (
            with_content(
                &text,
                "packaging/linux/hostkeeper.tmpfiles.conf",
                "/usr/lib/tmpfiles.d/hostkeeper.conf",
            ),
            "the apk carries",
        ),
        (
            with_content(
                &text,
                "target/release/hostkeeper-pxe",
                "/usr/bin/hostkeeper-pxe",
            ),
            "the apk carries",
        ),
        (
            with_content(
                &text,
                "packaging/alpine/hostkeeper.toml",
                "/etc/hostkeeper/hostkeeper.toml",
            ),
            "the apk carries",
        ),
        (
            altered(
                &text,
                "scripts:\n",
                "  - src: /run/hostkeeper/enrollment.issue\n    dst: /etc/issue.d/hostkeeper.issue\n    type: symlink\nscripts:\n",
            ),
            "the apk carries the links",
        ),
        (
            altered(&text, "depends:\n  - openrc>=0.62\n", ""),
            "the apk depends on []",
        ),
        (
            altered(&text, "openrc>=0.62", "openrc>=0.61"),
            "the apk depends on",
        ),
        (
            altered(&text, "openrc>=0.62", "openrc"),
            "the apk depends on",
        ),
        (
            altered(&text, "  - openrc>=0.62\n", "  - openrc>=0.62\n  - curl\n"),
            "the apk depends on",
        ),
        (
            altered(
                &text,
                "postinstall: packaging/alpine/scripts/postinstall.sh",
                "postinstall: packaging/linux/scripts/postinstall.sh",
            ),
            "the apk runs the scripts",
        ),
        (
            altered(
                &text,
                "  preremove: packaging/alpine/scripts/preremove.sh\n",
                "",
            ),
            "the apk runs the scripts",
        ),
    ];
    refuses(check, &cases);
    let missing = check_package(&text, |src| src != "packaging/alpine/hostkeeper.initd");
    assert_eq!(
        missing,
        Err(
            "packaging/alpine/hostkeeper.initd, a source of the apk, is not in the repository"
                .to_owned()
        )
    );
    let no_script = check_package(&text, |src| !src.ends_with("scripts/preremove.sh"));
    assert!(
        no_script.is_err_and(|e| e.contains("scripts/preremove.sh, a source")),
        "a missing script is accepted"
    );
}

/// The file names of the apk of the two Linux platforms, for version 1.2.3.
fn file_names(text: &str) -> Result<Vec<String>, String> {
    let config = Config::parse(text)?;
    ["amd64", "arm64"]
        .into_iter()
        .map(|arch| config.file_name("apk", "1.2.3", arch))
        .collect()
}

/// Catches a package name, version or architecture that does not come out as
/// `hostkeeper_<version>_<arch>.apk`, in the apk architectures `x86_64` and `aarch64`, or a
/// value read from any other variable of the environment.
#[test]
fn apk_file_names_follow_the_pattern() {
    let text = read_alpine(APK_NFPM);
    assert_eq!(
        file_names(&text),
        Ok(vec![
            "hostkeeper_1.2.3_x86_64.apk".to_owned(),
            "hostkeeper_1.2.3_aarch64.apk".to_owned(),
        ])
    );
    let fixed = file_names(&altered(&text, "arch: \"${ARCH}\"", "arch: amd64"));
    assert_eq!(
        fixed,
        Ok(vec![
            "hostkeeper_1.2.3_x86_64.apk".to_owned(),
            "hostkeeper_1.2.3_x86_64.apk".to_owned(),
        ])
    );
    for (from, to, words) in [
        (
            "version: \"${VERSION}\"",
            "version: \"${VERSION}-${GIT_SHA}\"",
            "version is",
        ),
        (
            "arch: \"${ARCH}\"",
            "arch: \"${HOSTKEEPER_ARCH}\"",
            "arch is",
        ),
        ("name: hostkeeper\n", "", "nfpm.yaml has no name"),
    ] {
        let refused = file_names(&altered(&text, from, to)).expect_err(to);
        assert!(refused.contains(words), "{refused:?} lacks {words:?}");
    }
    let config = Config::parse(&text).expect("nfpm.yaml parses");
    for (version, arch, refusal) in [
        (
            "v1.2.3",
            "amd64",
            "VERSION \"v1.2.3\" is not MAJOR.MINOR.PATCH in digits",
        ),
        ("1.2.3", "x86_64", "ARCH \"x86_64\" is not amd64 or arm64"),
    ] {
        assert_eq!(
            config.file_name("apk", version, arch),
            Err(refusal.to_owned())
        );
    }
}
