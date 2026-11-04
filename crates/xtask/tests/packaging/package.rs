//! The packages: `nfpm.yaml`, the files it puts in the deb and the rpm and their names, and the
//! two package scripts.

use std::collections::BTreeMap;

use crate::nfpm::{self, Config};
use crate::script::{self, Command};
use crate::{INSTALLED_UNIT, NFPM, POSTINSTALL, PREREMOVE, altered, config, read, refuses};

const ENABLE: [&str; 3] = ["systemctl", "enable", "hostkeeper.service"];
const DISABLE: [&str; 3] = ["systemctl", "disable", "hostkeeper.service"];
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
    let unknown = Config::parse(text)?.unknown_keys()?;
    if !unknown.is_empty() {
        return Err(format!(
            "nfpm.yaml holds keys it does not need: {unknown:?}"
        ));
    }
    Ok(())
}

/// Catches a signing or repository section (release work, outside this file), a key for one
/// packager only, a dependency or a version suffix: any key nfpm.yaml does not need.
#[test]
fn nfpm_config_holds_only_the_keys_it_needs() {
    let text = read(NFPM);
    assert_eq!(check_nfpm_keys(&text), Ok(()));
    let cases = [
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
            "[\"contents[].type\"]",
        ),
        (
            altered(
                &text,
                "      mode: 0755\n",
                "      mode: 0755\n      mtime: \"2027-01-01T00:00:00Z\"\n",
            ),
            "[\"contents[].file_info.mtime\"]",
        ),
    ];
    refuses(check_nfpm_keys, &cases);
}

/// The four files of the package, sorted as [`Config::files`] sorts them.
fn package_files() -> Vec<nfpm::File> {
    let file = |dst: &str, src: &str, mode: &str| nfpm::File {
        dst: dst.to_owned(),
        src: src.to_owned(),
        mode: mode.to_owned(),
        owner: "root".to_owned(),
        group: "root".to_owned(),
    };
    let mut files = vec![
        file("/usr/bin/hostkeeper", "target/release/hostkeeper", "0755"),
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
    ];
    files.sort();
    files
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
/// read by `source`) by its text.
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
/// (by name, or a private key in a source's text), a file too many or too few, or a file with
/// another mode, owner or place.
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
            altered(&text, "      mode: 0755\n", "      mode: 0700\n"),
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
    // The rpm is held to the same list.
    assert_eq!(check_manifest(&text, "rpm", repo_source), Ok(()));
}

fn check_one_list(text: &str) -> Result<(), String> {
    let config = Config::parse(text)?;
    let (deb, rpm) = (config.files("deb")?, config.files("rpm")?);
    if deb != rpm {
        return Err(format!("the deb carries {deb:?} and the rpm {rpm:?}"));
    }
    let (deb, rpm) = (config.scripts("deb")?, config.scripts("rpm")?);
    if deb != rpm {
        return Err(format!(
            "the deb runs the scripts {deb:?} and the rpm {rpm:?}"
        ));
    }
    Ok(())
}

/// Catches an override that gives one packager a file or a script the other does not have.
#[test]
fn rpm_and_deb_share_one_file_list() {
    let text = read(NFPM);
    assert_eq!(check_one_list(&text), Ok(()));
    let extra_file = "overrides:\n  rpm:\n    contents:\n      - src: packaging/linux/extra.conf\n        dst: /etc/extra.conf\n        file_info:\n          mode: 0644\n          owner: root\n          group: root\n";
    let extra_script = "overrides:\n  deb:\n    scripts:\n      postremove: packaging/linux/scripts/postremove.sh\n";
    let other_script = "overrides:\n  rpm:\n    scripts:\n      preremove: packaging/linux/scripts/preremove-rpm.sh\n";
    let cases = [
        (format!("{text}{extra_file}"), "the deb carries"),
        (format!("{text}{extra_script}"), "the deb runs the scripts"),
        (format!("{text}{other_script}"), "the deb runs the scripts"),
    ];
    refuses(check_one_list, &cases);
}

const ARCHES: [(&str, &str); 4] = [
    ("deb", "amd64"),
    ("deb", "arm64"),
    ("rpm", "amd64"),
    ("rpm", "arm64"),
];

fn file_names(text: &str) -> Result<Vec<String>, String> {
    let config = Config::parse(text)?;
    ARCHES
        .iter()
        .map(|(packager, arch)| config.file_name(packager, "1.2.3", arch))
        .collect()
}

/// Catches a package name, version or architecture that does not come out as
/// `hostkeeper_<version>_<arch>.deb` and `hostkeeper-<version>-1.<arch>.rpm` for both
/// architectures, or a value read from any other variable of the environment.
#[test]
fn package_file_names_follow_the_pattern() {
    let text = read(NFPM);
    let wanted = [
        "hostkeeper_1.2.3_amd64.deb",
        "hostkeeper_1.2.3_arm64.deb",
        "hostkeeper-1.2.3-1.x86_64.rpm",
        "hostkeeper-1.2.3-1.aarch64.rpm",
    ];
    assert_eq!(file_names(&text), Ok(wanted.map(str::to_owned).to_vec()));
    let names = |t: &str| file_names(t).map(|names| names.join(" "));
    // A fixed architecture names the arm64 packages amd64.
    let fixed = names(&altered(&text, "arch: \"${ARCH}\"", "arch: amd64"));
    assert_eq!(
        fixed,
        Ok("hostkeeper_1.2.3_amd64.deb hostkeeper_1.2.3_amd64.deb hostkeeper-1.2.3-1.x86_64.rpm hostkeeper-1.2.3-1.x86_64.rpm".to_owned())
    );
    let typo = names(&altered(&text, "name: hostkeeper\n", "name: hostkeper\n"));
    assert!(
        typo.as_ref().is_ok_and(|n| !n.contains("hostkeeper_")),
        "{typo:?}"
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
        let refused = names(&altered(&text, from, to)).expect_err(to);
        assert!(refused.contains(words), "{refused:?} lacks {words:?}");
    }
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
