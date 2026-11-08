//! The packages: `nfpm.yaml`, the files and the link it puts in the deb and the rpm, and the two
//! package scripts. Their file names are in `package_names.rs`.

use std::collections::BTreeMap;

use hostkeeper_daemon::cli::{GH_BINARY, SIGSTORE_TRUSTED_ROOT};
use hostkeeper_daemon::console::{ENROLLMENT_ISSUE, ISSUE_LINK};
use xtask::test_support::{LINUX_PKG_PROGRAMS, NFPM_STAGED};

use crate::attestation_files::staged;
use crate::nfpm::{self, Config};
use crate::script::{self, Command};
use crate::{INSTALLED_UNIT, NFPM, POSTINSTALL, PREREMOVE, altered, config, read, refuses};

pub(crate) const ENABLE: [&str; 3] = ["systemctl", "enable", "hostkeeper.service"];
pub(crate) const DISABLE: [&str; 3] = ["systemctl", "disable", "hostkeeper.service"];
/// The `case` patterns of a first install: deb `configure` with no previous version, rpm `1`.
const FIRST_INSTALL: [&str; 2] = ["configure:", "1:"];
/// The `case` patterns of a removal: deb `remove`, rpm `0`.
const ON_REMOVAL: [&str; 2] = ["remove", "0"];
/// Words that would start, restart or switch to the daemon.
const STARTING: [&str; 6] = [
    "start",
    "restart",
    "--now",
    "reload-or-restart",
    "try-restart",
    "isolate",
];
/// Every command the scripts may run.
const SCRIPT_COMMANDS: [&[&str]; 7] = [
    &["set", "-e"],
    &["systemd-tmpfiles", "--create", "hostkeeper.conf"],
    &["systemctl", "daemon-reload"],
    &ENABLE,
    &DISABLE,
    &["true"],
    &["exit", "0"],
];

fn check_nfpm_keys(text: &str) -> Result<(), String> {
    let config = Config::parse(text)?;
    let unknown = config.unknown_keys()?;
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

/// Catches a signing or repository section (release work, outside this file), a key for one
/// packager only, a dependency or a version suffix: any key nfpm.yaml does not need; a value
/// nfpm decodes as text written as a mapping, a list or null, or a platform other than Linux;
/// and a contents entry of another type than a link (a `config` file among them).
#[test]
fn nfpm_config_holds_only_the_keys_it_needs() {
    let text = read(NFPM);
    assert_eq!(check_nfpm_keys(&text), Ok(()));
    // The Arch package names the same packager as the deb's maintainer.
    let config = config();
    assert_eq!(
        config.archlinux_field("packager"),
        config.field("maintainer")
    );
    assert_eq!(config.archlinux_field("pkgbase"), Ok(None));
    let none = Config::parse(&altered(
        &text,
        "archlinux:\n  packager:",
        "other:\n  packager:",
    ))
    .expect("parses");
    assert_eq!(none.archlinux_field("packager"), Ok(None));
    let maintainer =
        "maintainer: \"The hostkeeper project <https://github.com/komira-ai/hostkeeper>\"";
    let cases = [
        (
            altered(&text, maintainer, "maintainer: {}"),
            "[\"maintainer is not text\"]",
        ),
        (
            altered(&text, maintainer, "maintainer: [a, b]"),
            "[\"maintainer is not text\"]",
        ),
        (
            altered(&text, maintainer, "maintainer:"),
            "[\"maintainer is not text\"]",
        ),
        (
            altered(&text, "vendor: komira-ai", "vendor: {name: komira-ai}"),
            "[\"vendor is not text\"]",
        ),
        (
            altered(&text, "version: \"${VERSION}\"", "version: [1]"),
            "[\"version is not text\"]",
        ),
        (
            altered(&text, "platform: linux", "platform: darwin"),
            "[\"platform is \\\"darwin\\\", not linux\"]",
        ),
        (
            format!("{text}deb:\n  signature:\n    key_file: release.gpg\n"),
            "[\"deb\"]",
        ),
        (
            format!("{text}rpm:\n  signature:\n    key_file: release.gpg\n"),
            "[\"rpm\"]",
        ),
        (
            format!("{text}overrides:\n  rpm:\n    depends: [systemd]\n"),
            "[\"overrides\"]",
        ),
        (
            altered(
                &text,
                "archlinux:\n  packager:",
                "archlinux:\n  pkgbase: hostkeeper\n  packager:",
            ),
            "[\"archlinux.pkgbase\"]",
        ),
        (
            altered(
                &text,
                "archlinux:\n  packager: \"The hostkeeper project <https://github.com/komira-ai/hostkeeper>\"",
                "archlinux:\n  packager: [a, b]",
            ),
            "[\"archlinux.packager is not text\"]",
        ),
        (format!("{text}release: \"2\"\n"), "[\"release\"]"),
        (format!("{text}depends:\n  - curl\n"), "[\"depends\"]"),
        (
            altered(
                &text,
                "  preremove:",
                "  postremove: packaging/linux/scripts/postremove.sh\n  preremove:",
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
            altered(&text, "    type: symlink\n", "    type: [symlink]\n"),
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
    refuses(check_nfpm_keys, &cases);
}

/// The seven files of the package, sorted as [`Config::files`] sorts them: the two programs
/// (the static build) at the paths `release check-linux-pkg` requires, and `gh` and the trusted
/// root at the daemon's own paths, all from where `release nfpm-stage` writes them.
pub fn package_files() -> Vec<nfpm::File> {
    let file = |dst: &str, src: &str, mode: &str| nfpm::File {
        dst: dst.to_owned(),
        src: src.to_owned(),
        mode: mode.to_owned(),
        owner: "root".to_owned(),
        group: "root".to_owned(),
    };
    let [(hostkeeper, hostkeeper_staged), (pxe, pxe_staged)] = LINUX_PKG_PROGRAMS;
    let mut files = vec![
        file(hostkeeper, &staged(hostkeeper_staged), "0755"),
        file(pxe, &staged(pxe_staged), "0755"),
        file(INSTALLED_UNIT, "packaging/linux/hostkeeper.service", "0644"),
        file(
            "/usr/lib/tmpfiles.d/hostkeeper.conf",
            "packaging/linux/hostkeeper.tmpfiles.conf",
            "0644",
        ),
        file(
            "/usr/share/hostkeeper/hostkeeper.toml.template",
            "packaging/linux/hostkeeper.toml.template",
            "0644",
        ),
        file(GH_BINARY, &staged(NFPM_STAGED[0]), "0755"),
        file(SIGSTORE_TRUSTED_ROOT, &staged(NFPM_STAGED[1]), "0644"),
    ];
    files.sort();
    files
}

/// The one link of the package: the console's issue link, to the file the daemon writes.
pub fn package_links() -> Vec<nfpm::Link> {
    vec![nfpm::Link {
        dst: ISSUE_LINK.to_owned(),
        target: ENROLLMENT_ISSUE.to_owned(),
    }]
}

/// Catches the check of the built packages (`release check-linux-pkg`, run on every deb and rpm
/// the artifacts workflow builds) holding them to another link than the one nfpm.yaml declares
/// and the daemon writes.
#[test]
fn the_built_package_check_requires_the_link_nfpm_yaml_declares() {
    let (dst, target) = xtask::test_support::LINUX_PKG_LINK;
    let declared: Vec<nfpm::Link> = config().links("deb").expect("links");
    assert_eq!(
        declared,
        [nfpm::Link {
            dst: dst.to_owned(),
            target: target.to_owned(),
        }]
    );
    assert_eq!(declared, package_links());
}

/// A name that is that of a key, a certificate, a token, a secret or the live config.
fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".key", ".pem", ".p12", ".pfx", ".crt"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
        || lower.starts_with("id_")
        || lower.contains("token")
        || lower.contains("secret")
        || lower == "hostkeeper.toml"
}

/// The files of the `packager` package are exactly [`package_files`], and none is a key, a
/// certificate, a token or the live config, by its name or (for a source in this repository,
/// read by `source`) by its text; its links are exactly [`package_links`].
fn check_manifest(
    text: &str,
    packager: &str,
    source: impl Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let files = Config::parse(text)?.files(packager)?;
    for file in &files {
        for path in [&file.src, &file.dst] {
            let name = path.rsplit('/').next().unwrap_or_default();
            if is_secret_name(name) {
                return Err(format!(
                    "the {packager} carries {path}, named as a key, certificate, token, secret or the live config"
                ));
            }
        }
        if source(file.src.as_str()).is_some_and(|content| content.contains("PRIVATE KEY-----")) {
            return Err(format!(
                "the {packager} carries {}, which holds a private key",
                file.src
            ));
        }
    }
    let wanted = package_files();
    if files != wanted {
        return Err(format!("the {packager} carries {files:?}, not {wanted:?}"));
    }
    let links = Config::parse(text)?.links(packager)?;
    let wanted = package_links();
    if links != wanted {
        return Err(format!(
            "the {packager} carries the links {links:?}, not {wanted:?}"
        ));
    }
    Ok(())
}

/// The text of a package source that is a file of this repository; `None` for the daemon,
/// which cargo builds.
fn repo_source(src: &str) -> Option<String> {
    src.strip_prefix("packaging/linux/").map(read)
}

/// One more `contents` entry, before `scripts:`.
fn with_content(text: &str, src: &str, dst: &str) -> String {
    altered(
        text,
        "scripts:\n",
        &format!(
            "  - src: {src}\n    dst: {dst}\n    file_info:\n      mode: 0600\n      owner: root\n      group: root\nscripts:\n"
        ),
    )
}

/// Catches a package that carries a key, a certificate, a token or a live `hostkeeper.toml`
/// (by name, or a private key in a source's text), a file too many or too few (hostkeeper-pxe
/// left out among them), a file with another mode, owner, source or place, and a console issue link that is missing, is somewhere else
/// than `/etc/issue.d`, or points at another file than the one the daemon writes.
#[test]
fn deb_manifest_lists_the_unit_the_binary_and_no_key_or_token() {
    let text = read(NFPM);
    assert_eq!(check_manifest(&text, "deb", repo_source), Ok(()));
    let check = |t: &str| check_manifest(t, "deb", repo_source);
    let cases = [
        (
            with_content(
                &text,
                "packaging/linux/dummy.key",
                "/etc/hostkeeper/dummy.key",
            ),
            "carries packaging/linux/dummy.key",
        ),
        (
            with_content(
                &text,
                "packaging/linux/hostkeeper.toml.template",
                "/etc/hostkeeper/hostkeeper.toml",
            ),
            "carries /etc/hostkeeper/hostkeeper.toml",
        ),
        (
            with_content(&text, "ca.pem", "/etc/hostkeeper/ca.pem"),
            "carries ca.pem",
        ),
        (
            with_content(&text, "host.p12", "/etc/hostkeeper/host.p12"),
            "carries host.p12",
        ),
        (
            with_content(&text, "id_ed25519", "/root/.ssh/id_ed25519"),
            "carries id_ed25519",
        ),
        (
            with_content(&text, "enroll.token", "/etc/hostkeeper/enroll.token"),
            "carries enroll.token",
        ),
        (
            with_content(&text, "api-secret.txt", "/etc/hostkeeper/api-secret.txt"),
            "carries api-secret.txt",
        ),
        (
            with_content(
                &text,
                "packaging/linux/README.md",
                "/usr/share/doc/hostkeeper/README.md",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "    dst: /usr/bin/hostkeeper\n    file_info:\n      mode: 0755\n",
                "    dst: /usr/bin/hostkeeper\n    file_info:\n      mode: 0700\n",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "    dst: /usr/bin/hostkeeper-pxe\n    file_info:\n      mode: 0755\n",
                "    dst: /usr/bin/hostkeeper-pxe\n    file_info:\n      mode: 0750\n",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "  - src: target/nfpm/hostkeeper-pxe\n    dst: /usr/bin/hostkeeper-pxe\n    file_info:\n      mode: 0755\n      owner: root\n      group: root\n",
                "",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "  - src: target/nfpm/hostkeeper-pxe\n",
                "  - src: target/nfpm/hostkeeper\n",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "  - src: target/nfpm/hostkeeper-pxe\n",
                "  - src: target/release/hostkeeper-pxe\n",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "  - src: target/nfpm/hostkeeper\n",
                "  - src: target/release/hostkeeper\n",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "    dst: /usr/lib/hostkeeper/gh\n    file_info:\n      mode: 0755\n",
                "    dst: /usr/lib/hostkeeper/gh\n    file_info:\n      mode: 0775\n",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "    dst: /usr/lib/hostkeeper/trusted_root.json\n",
                "    dst: /etc/hostkeeper/trusted_root.json\n",
            ),
            "not [",
        ),
        (
            altered(&text, "  - src: target/nfpm/gh\n", "  - src: /usr/bin/gh\n"),
            "not [",
        ),
        (
            altered(
                &text,
                "dst: /usr/lib/tmpfiles.d/hostkeeper.conf",
                "dst: /etc/tmpfiles.d/hostkeeper.conf",
            ),
            "not [",
        ),
        (
            altered(
                &text,
                "  - src: /run/hostkeeper/enrollment.issue\n    dst: /etc/issue.d/hostkeeper.issue\n    type: symlink\n",
                "",
            ),
            "carries the links [], not [",
        ),
        (
            altered(
                &text,
                "dst: /etc/issue.d/hostkeeper.issue",
                "dst: /etc/issue.d/other.issue",
            ),
            "carries the links [",
        ),
        (
            altered(
                &text,
                "src: /run/hostkeeper/enrollment.issue",
                "src: /run/hostkeeper/admin.sock",
            ),
            "carries the links [",
        ),
    ];
    refuses(check, &cases);
    // A source whose text holds a private key, whatever its name.
    let pem = concat!("-----BEGIN ", "PRIVATE KEY-----");
    let leaking =
        |src: &str| (src == "packaging/linux/hostkeeper.tmpfiles.conf").then(|| pem.to_owned());
    assert_eq!(
        check_manifest(&text, "deb", leaking),
        Err(
            "the deb carries packaging/linux/hostkeeper.tmpfiles.conf, which holds a private key"
                .to_owned()
        )
    );
    // The rpm and the Arch package are held to the same list.
    assert_eq!(check_manifest(&text, "rpm", repo_source), Ok(()));
    assert_eq!(check_manifest(&text, "archlinux", repo_source), Ok(()));
    let extra = "overrides:\n  archlinux:\n    contents:\n      - src: target/extra.conf\n        dst: /etc/extra.conf\n        file_info:\n          mode: 0644\n          owner: root\n          group: root\n";
    let refused = check_manifest(&format!("{text}{extra}"), "archlinux", repo_source);
    assert!(
        refused.is_err_and(|m| m.contains("the archlinux carries")),
        "an extra file in the Arch package alone is refused"
    );
}

fn check_one_list(text: &str) -> Result<(), String> {
    let config = Config::parse(text)?;
    for other in ["rpm", "archlinux"] {
        let (deb, rpm) = (config.files("deb")?, config.files(other)?);
        if deb != rpm {
            return Err(format!("the deb carries {deb:?} and the {other} {rpm:?}"));
        }
        let (deb, rpm) = (config.links("deb")?, config.links(other)?);
        if deb != rpm {
            return Err(format!(
                "the deb carries the links {deb:?} and the {other} {rpm:?}"
            ));
        }
        let (deb, rpm) = (config.scripts("deb")?, config.scripts(other)?);
        if deb != rpm {
            return Err(format!(
                "the deb runs the scripts {deb:?} and the {other} {rpm:?}"
            ));
        }
    }
    Ok(())
}

/// Catches an override that gives one packager (the deb, the rpm or the Arch package) a file, a
/// link or a script the others do not have.
#[test]
fn rpm_and_deb_share_one_file_list() {
    let text = read(NFPM);
    assert_eq!(check_one_list(&text), Ok(()));
    let extra_file = "overrides:\n  rpm:\n    contents:\n      - src: packaging/linux/extra.conf\n        dst: /etc/extra.conf\n        file_info:\n          mode: 0644\n          owner: root\n          group: root\n";
    let extra_script = "overrides:\n  deb:\n    scripts:\n      postremove: packaging/linux/scripts/postremove.sh\n";
    let other_script = "overrides:\n  rpm:\n    scripts:\n      preremove: packaging/linux/scripts/preremove-rpm.sh\n";
    let extra_link = "overrides:\n  deb:\n    contents:\n      - src: /run/hostkeeper/other\n        dst: /etc/issue.d/other.issue\n        type: symlink\n";
    let arch_file = extra_file.replace("rpm:", "archlinux:");
    let arch_script = other_script.replace("rpm:", "archlinux:");
    let arch_link = extra_link.replace("deb:", "archlinux:");
    let cases = [
        (format!("{text}{arch_file}"), "the deb carries"),
        (format!("{text}{arch_link}"), "the deb carries the links"),
        (format!("{text}{arch_script}"), "the deb runs the scripts"),
        (format!("{text}{extra_file}"), "the deb carries"),
        (format!("{text}{extra_link}"), "the deb carries the links"),
        (format!("{text}{extra_script}"), "the deb runs the scripts"),
        (format!("{text}{other_script}"), "the deb runs the scripts"),
    ];
    refuses(check_one_list, &cases);
}

fn check_enable(post: &str, pre: &str) -> Result<(), String> {
    let post = script::parse(post)?;
    let pre = script::parse(pre)?;
    for command in post.iter().chain(&pre) {
        if let Some(word) = command
            .words
            .iter()
            .find(|w| STARTING.contains(&w.as_str()))
        {
            return Err(format!(
                "line {}: `{}` would start the daemon ({word})",
                command.line,
                command.words.join(" ")
            ));
        }
    }
    let is = |c: &&Command, words: [&str; 3], arm: [&str; 2]| {
        c.words == words && c.arm.as_ref().is_some_and(|a| *a == arm)
    };
    let enables: Vec<&Command> = post.iter().filter(|c| c.words == ENABLE).collect();
    if enables.len() != 1 || !enables.iter().all(|c| is(c, ENABLE, FIRST_INSTALL)) {
        return Err(format!(
            "the post-install script enables the unit as {enables:?}, not once on a first install (`configure:` for the deb, `1:` for the rpm)"
        ));
    }
    let disables: Vec<&Command> = pre.iter().filter(|c| c.words == DISABLE).collect();
    if disables.is_empty() || !disables.iter().all(|c| is(c, DISABLE, ON_REMOVAL)) {
        return Err(format!(
            "the pre-remove script disables the unit as {disables:?}, not on removal only (`remove` for the deb, `0` for the rpm)"
        ));
    }
    if post.iter().any(|c| c.words == DISABLE) || pre.iter().any(|c| c.words == ENABLE) {
        return Err(
            "the post-install script disables, or the pre-remove script enables".to_owned(),
        );
    }
    Ok(())
}

/// Catches a script that starts the daemon (a host with no config yet would start a daemon
/// that refuses it), an enable on an upgrade, which overrides an operator's disable, or a
/// disable on an upgrade.
#[test]
fn postinstall_enables_but_never_starts_the_unit() {
    let post = read(POSTINSTALL);
    let pre = read(PREREMOVE);
    assert_eq!(check_enable(&post, &pre), Ok(()));
    let with_post = |text: String| {
        let pre = pre.clone();
        move |_: &str| check_enable(&text, &pre)
    };
    let enable = "systemctl enable hostkeeper.service";
    let post_cases = [
        (
            altered(&post, enable, "systemctl enable --now hostkeeper.service"),
            "(--now)",
        ),
        (
            altered(
                &post,
                "exit 0",
                "systemctl start hostkeeper.service\nexit 0",
            ),
            "(start)",
        ),
        (
            altered(
                &post,
                "exit 0",
                "systemctl restart hostkeeper.service\nexit 0",
            ),
            "(restart)",
        ),
        (
            altered(&post, "configure: | 1:)", "configure: | 1: | 2:)"),
            "not once on a first install",
        ),
        (
            altered(
                &post,
                "  configure: | 1:) systemctl enable hostkeeper.service ;;\n",
                "  configure: | 1:) true ;;\n",
            ),
            "not once on a first install",
        ),
        (
            altered(&post, "exit 0", &format!("{enable}\nexit 0")),
            "not once on a first install",
        ),
        (
            altered(
                &post,
                "exit 0",
                "systemctl disable hostkeeper.service\nexit 0",
            ),
            "the post-install script disables",
        ),
    ];
    for (text, words) in post_cases {
        refuses(with_post(text), &[(String::new(), words)]);
    }
    let pre_cases = [
        (
            altered(&pre, "remove | 0)", "remove | upgrade | 0 | 1)"),
            "not on removal only",
        ),
        (
            altered(
                &pre,
                "case \"$1\" in\n  remove | 0) systemctl disable hostkeeper.service || true ;;\nesac\n",
                "systemctl disable hostkeeper.service || true\n",
            ),
            "not on removal only",
        ),
        (
            altered(&pre, "systemctl disable hostkeeper.service", "true"),
            "not on removal only",
        ),
        (
            altered(
                &pre,
                "systemctl disable hostkeeper.service",
                "systemctl disable --now hostkeeper.service",
            ),
            "(--now)",
        ),
    ];
    refuses(|t: &str| check_enable(&post, t), &pre_cases);
}

fn check_commands(text: &str) -> Result<(), String> {
    for command in script::parse(text)? {
        if !SCRIPT_COMMANDS
            .iter()
            .any(|allowed| command.words == **allowed)
        {
            return Err(format!(
                "line {}: `{}` is not a command the package scripts may run",
                command.line,
                command.words.join(" ")
            ));
        }
    }
    Ok(())
}

/// Catches a script that does anything but make the daemon's directories, reload systemd and
/// enable or disable the unit: removing files, changing owners, creating an account, fetching,
/// or a construct of a shell other than POSIX sh.
#[test]
fn scripts_are_posix_sh_and_touch_only_the_unit_and_tmpfiles() {
    let post = read(POSTINSTALL);
    let pre = read(PREREMOVE);
    assert_eq!(check_commands(&post), Ok(()));
    assert_eq!(check_commands(&pre), Ok(()));
    let scripts = config().scripts("deb").expect("the scripts");
    let wanted: BTreeMap<String, String> = [("postinstall", POSTINSTALL), ("preremove", PREREMOVE)]
        .into_iter()
        .map(|(name, path)| (name.to_owned(), format!("packaging/linux/{path}")))
        .collect();
    assert_eq!(scripts, wanted);
    let before_exit = |line: &str| altered(&post, "exit 0", &format!("{line}\nexit 0"));
    let cases = [
        (
            before_exit("rm -rf /var/lib/updater"),
            "`rm -rf /var/lib/updater` is not",
        ),
        (
            before_exit("chown -R root /etc/hostkeeper"),
            "`chown -R root /etc/hostkeeper` is not",
        ),
        (
            before_exit("useradd --system hostkeeper"),
            "`useradd --system hostkeeper` is not",
        ),
        (
            before_exit("curl https://releases.example/hostkeeper"),
            "`curl https://releases.example/hostkeeper` is not",
        ),
        (
            before_exit("systemctl mask hostkeeper.service"),
            "`systemctl mask hostkeeper.service` is not",
        ),
        (
            before_exit("systemd-tmpfiles --create --remove"),
            "`systemd-tmpfiles --create --remove` is not",
        ),
        (
            before_exit("systemctl daemon-reload | true"),
            "the word \"|\"",
        ),
        (
            before_exit("[ -d /run/systemd/system ] && true"),
            "the word \"[\"",
        ),
        (before_exit("echo $(id -u)"), "the word \"$(id\""),
        (before_exit("echo `id -u`"), "the word \"`id\""),
        (
            before_exit("true > /etc/hostkeeper/hostkeeper.toml"),
            "the word \">\"",
        ),
        (
            before_exit("true; rm /etc/hostkeeper"),
            "the word \"true;\"",
        ),
        (
            altered(&post, "#!/bin/sh\n", "#!/bin/bash\n"),
            "does not start with `#!/bin/sh`",
        ),
        (
            altered(&post, "case \"$1:$2\" in", "case \"$3\" in"),
            "a `case` on",
        ),
        (altered(&post, "esac\nexit 0\n", ""), "not closed by `esac`"),
        (altered(&post, " ;;\n", "\n"), "does not end in `;;`"),
        (
            altered(&post, "configure: | 1:)", "configure:* | 1:)"),
            "the pattern \"configure:*\"",
        ),
    ];
    refuses(check_commands, &cases);
}

pub(crate) const TMPFILES_CREATE: [&str; 3] = ["systemd-tmpfiles", "--create", "hostkeeper.conf"];
pub(crate) const DAEMON_RELOAD: [&str; 2] = ["systemctl", "daemon-reload"];

/// One run of a package script: what the package manager passes, the program that fails (if
/// any), and the programs it must run and the status it must end with.
pub(crate) struct Scenario {
    pub(crate) name: &'static str,
    pub(crate) args: &'static [&'static str],
    pub(crate) failing: Option<&'static [&'static str]>,
    pub(crate) ran: &'static [&'static [&'static str]],
    pub(crate) status: i32,
}

/// The post-install script's runs: a first install enables the unit, an upgrade does not; a
/// systemd that is not running (a chroot) does not stop the enable; a failed tmpfiles run or
/// a failed enable fails the install.
const POSTINSTALL_RUNS: [Scenario; 7] = [
    Scenario {
        name: "deb first install",
        args: &["configure"],
        failing: None,
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD, &ENABLE],
        status: 0,
    },
    Scenario {
        name: "rpm first install",
        args: &["1"],
        failing: None,
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD, &ENABLE],
        status: 0,
    },
    Scenario {
        name: "deb upgrade",
        args: &["configure", "1.2.2"],
        failing: None,
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD],
        status: 0,
    },
    Scenario {
        name: "rpm upgrade",
        args: &["2"],
        failing: None,
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD],
        status: 0,
    },
    Scenario {
        name: "first install without a running systemd",
        args: &["configure"],
        failing: Some(&DAEMON_RELOAD),
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD, &ENABLE],
        status: 0,
    },
    Scenario {
        name: "tmpfiles fails",
        args: &["1"],
        failing: Some(&TMPFILES_CREATE),
        ran: &[&TMPFILES_CREATE],
        status: 1,
    },
    Scenario {
        name: "enable fails",
        args: &["1"],
        failing: Some(&ENABLE),
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD, &ENABLE],
        status: 1,
    },
];

/// The pre-remove script's runs: a removal disables the unit, an upgrade does nothing, and a
/// failed disable does not stop the removal.
const PREREMOVE_RUNS: [Scenario; 5] = [
    Scenario {
        name: "deb removal",
        args: &["remove"],
        failing: None,
        ran: &[&DISABLE],
        status: 0,
    },
    Scenario {
        name: "rpm removal",
        args: &["0"],
        failing: None,
        ran: &[&DISABLE],
        status: 0,
    },
    Scenario {
        name: "deb upgrade",
        args: &["upgrade", "1.2.4"],
        failing: None,
        ran: &[],
        status: 0,
    },
    Scenario {
        name: "rpm upgrade",
        args: &["1"],
        failing: None,
        ran: &[],
        status: 0,
    },
    Scenario {
        name: "disable fails",
        args: &["remove"],
        failing: Some(&DISABLE),
        ran: &[&DISABLE],
        status: 0,
    },
];

/// Runs `text` through every scenario of `runs`, with each program a fake that fails only
/// for the scenario's failing command.
pub(crate) fn check_runs(text: &str, runs: &[Scenario]) -> Result<(), String> {
    let commands = script::parse(text)?;
    for scenario in runs {
        let done = script::run(&commands, scenario.args, |words| {
            i32::from(scenario.failing.is_some_and(|failing| words == failing))
        });
        let wanted = script::Run {
            ran: scenario
                .ran
                .iter()
                .map(|words| words.iter().map(|w| (*w).to_owned()).collect())
                .collect(),
            status: scenario.status,
        };
        if done != wanted {
            return Err(format!(
                "{}: the script ran {:?} and ended {}, not {:?} and {}",
                scenario.name, done.ran, done.status, wanted.ran, wanted.status
            ));
        }
    }
    Ok(())
}

/// Catches a package script that skips a step (the directories, the reload, the enable or the
/// disable), runs one on the wrong install or removal, hides a failure that must fail the
/// install (no `set -e`), or fails an install on a host where systemd is not running (a
/// boot-image chroot), by running each script as sh runs it, with the package manager's
/// arguments and a fake for every program.
#[test]
fn package_scripts_run_their_steps_in_order_and_fail_as_the_package_manager_expects() {
    let post = read(POSTINSTALL);
    let pre = read(PREREMOVE);
    assert_eq!(check_runs(&post, &POSTINSTALL_RUNS), Ok(()));
    assert_eq!(check_runs(&pre, &PREREMOVE_RUNS), Ok(()));
    let post_cases = [
        (
            altered(&post, "systemd-tmpfiles --create hostkeeper.conf\n", ""),
            "deb first install: the script ran",
        ),
        (
            altered(&post, "systemctl daemon-reload || true\n", ""),
            "deb first install: the script ran",
        ),
        (
            altered(&post, "set -e\n", ""),
            "tmpfiles fails: the script ran",
        ),
        (
            altered(
                &post,
                "systemctl daemon-reload || true",
                "systemctl daemon-reload",
            ),
            "first install without a running systemd: the script ran",
        ),
        (
            altered(
                &post,
                "systemd-tmpfiles --create hostkeeper.conf",
                "systemd-tmpfiles --create hostkeeper.conf || true",
            ),
            "tmpfiles fails: the script ran",
        ),
        (
            altered(
                &post,
                "systemctl enable hostkeeper.service ;;",
                "systemctl enable hostkeeper.service || true ;;",
            ),
            "enable fails: the script ran",
        ),
        (
            altered(&post, "configure: | 1:)", "configure: | 1: | 2:)"),
            "rpm upgrade: the script ran",
        ),
        (
            altered(&post, "exit 0", "exit 1"),
            "deb first install: the script ran",
        ),
    ];
    refuses(|t: &str| check_runs(t, &POSTINSTALL_RUNS), &post_cases);
    let pre_cases = [
        (
            altered(&pre, "remove | 0)", "remove)"),
            "rpm removal: the script ran",
        ),
        (
            altered(&pre, "remove | 0)", "remove | 0 | upgrade | 1)"),
            "deb upgrade: the script ran",
        ),
        (
            altered(
                &pre,
                "systemctl disable hostkeeper.service || true",
                "systemctl disable hostkeeper.service",
            ),
            "disable fails: the script ran",
        ),
    ];
    refuses(|t: &str| check_runs(t, &PREREMOVE_RUNS), &pre_cases);
}
