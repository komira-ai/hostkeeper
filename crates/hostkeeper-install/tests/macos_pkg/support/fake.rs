//! The fake package and the fake Mac: `pkgutil`, `installer`, `lsbom` and `launchctl` over a
//! scratch host root.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use hostkeeper_install::host::Ids;
use hostkeeper_install::lock::InstallerLock;
use hostkeeper_install::tools::{Tool, ToolError, ToolOutput, Tools};

use super::{TRUSTED, job_plist, leaf, signed_output};

// ---------------------------------------------------------------------------------------------
// The fake package
// ---------------------------------------------------------------------------------------------

/// What an entry of a payload is.
#[derive(Clone, Debug)]
pub enum Content {
    File(Vec<u8>),
    Dir,
    Link(String),
    Fifo,
}

/// One payload entry: its logical path, content and permission bits, and the permission bits
/// the Bom lists when they are not the real ones.
#[derive(Clone, Debug)]
pub struct Entry {
    pub content: Content,
    pub mode: u32,
    pub bom_mode: Option<u32>,
}

/// What `installer` does with a package.
#[derive(Clone, Debug)]
pub enum Install {
    /// Installs the payload and records the receipt.
    Normal,
    /// Exits with `code`, printing `stdout` and `stderr`, installing nothing.
    Fails {
        code: i32,
        stdout: String,
        stderr: String,
    },
    /// Installs the payload, but the receipt keeps what it was.
    KeepsOldReceipt,
    /// Installs the payload and the receipt, but one file with other bytes.
    OtherBytes { path: String, bytes: Vec<u8> },
}

/// A package the fake Mac knows.
#[derive(Clone, Debug)]
pub struct FakePkg {
    pub name: String,
    pub service: String,
    pub identifier: String,
    pub version: String,
    /// `pkgutil --check-signature`'s exit code and standard output.
    pub check: (i32, String),
    pub product: bool,
    pub distribution: Option<String>,
    /// Entries a product archive's expansion holds beside its component.
    pub extras: Vec<String>,
    /// Directories the component's expansion holds beside its `Bom`, `PackageInfo` and `Payload`
    /// (`Scripts` for a package with scripts).
    pub component_extras: Vec<String>,
    /// Whether the component has a `Payload` (`pkgbuild --nopayload` makes one without).
    pub has_payload: bool,
    pub package_info: Option<String>,
    pub payload: BTreeMap<String, Entry>,
    pub lsbom: Option<(i32, String)>,
    pub install: Install,
}

impl FakePkg {
    /// A signed component package of `service` at `version`: the directories above, the
    /// service's directory with its program and a `VERSION` file, and the plist of the job
    /// named `service`.
    pub fn standard(service: &str, identifier: &str, version: &str) -> FakePkg {
        let mut pkg = FakePkg {
            name: format!("{service}-{version}"),
            service: service.into(),
            identifier: identifier.into(),
            version: version.into(),
            check: (0, signed_output(&leaf(), TRUSTED)),
            product: false,
            distribution: None,
            extras: Vec::new(),
            component_extras: Vec::new(),
            has_payload: true,
            package_info: None,
            payload: BTreeMap::new(),
            lsbom: None,
            install: Install::Normal,
        };
        for dir in [
            "/Library",
            "/Library/Application Support",
            "/Library/LaunchDaemons",
        ] {
            pkg = pkg.put(dir, Content::Dir, 0o755);
        }
        let home = pkg.home();
        pkg = pkg.put(&home, Content::Dir, 0o755);
        let program = format!("program {service} {version}");
        pkg = pkg.file(service, &program, 0o755);
        pkg = pkg.file("VERSION", version, 0o644);
        pkg.label(service)
    }

    /// `/Library/Application Support/<service>`.
    pub fn home(&self) -> String {
        format!("/Library/Application Support/{}", self.service)
    }

    /// The logical path of `rel` under the service's directory.
    pub fn path(&self, rel: &str) -> String {
        format!("{}/{rel}", self.home())
    }

    pub fn put(mut self, path: &str, content: Content, mode: u32) -> FakePkg {
        let entry = Entry {
            content,
            mode,
            bom_mode: None,
        };
        self.payload.insert(path.into(), entry);
        self
    }

    /// A file under the service's directory, with the directories between listed too.
    pub fn file(mut self, rel: &str, text: &str, mode: u32) -> FakePkg {
        let parts: Vec<&str> = rel.split('/').collect();
        for n in 1..parts.len() {
            let dir = self.path(&parts[..n].join("/"));
            self.payload.entry(dir).or_insert(Entry {
                content: Content::Dir,
                mode: 0o755,
                bom_mode: None,
            });
        }
        let path = self.path(rel);
        self.put(&path, Content::File(text.as_bytes().to_vec()), mode)
    }

    /// A directory under the service's directory.
    pub fn dir(self, rel: &str) -> FakePkg {
        let path = self.path(rel);
        self.put(&path, Content::Dir, 0o755)
    }

    /// The plist of a job named `label`, disabled, running the service's program.
    pub fn label(self, label: &str) -> FakePkg {
        let program = self.path(&self.service);
        let text = job_plist(label, &program, Some(true));
        self.plist(label, &text)
    }

    /// The plist of `label` with `text` as its content.
    pub fn plist(self, label: &str, text: &str) -> FakePkg {
        let path = format!("/Library/LaunchDaemons/{label}.plist");
        self.put(&path, Content::File(text.as_bytes().to_vec()), 0o644)
    }

    pub fn without(mut self, path: &str) -> FakePkg {
        self.payload.remove(path);
        self
    }

    /// The Bom lists `mode` for `path` where the payload has another.
    pub fn bom_mode(mut self, path: &str, mode: u32) -> FakePkg {
        if let Some(entry) = self.payload.get_mut(path) {
            entry.bom_mode = Some(mode);
        }
        self
    }

    pub fn with_install(mut self, install: Install) -> FakePkg {
        self.install = install;
        self
    }

    pub fn with_check(mut self, code: i32, stdout: &str) -> FakePkg {
        self.check = (code, stdout.into());
        self
    }

    pub fn product(mut self) -> FakePkg {
        self.product = true;
        self
    }

    /// The `PackageInfo` the package carries: its own, or the default of its identity in the form
    /// `pkgbuild` writes, with its empty `bundle-version`, `relocate` and `scripts`.
    pub fn package_info_text(&self) -> String {
        self.package_info.clone().unwrap_or_else(|| {
            format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<pkg-info overwrite-permissions=\"true\" relocatable=\"false\" identifier=\"{}\" postinstall-action=\"none\" version=\"{}\" format-version=\"2\" generator-version=\"InstallCmds-865 (24A335)\" install-location=\"/\" auth=\"root\">\n    <payload numberOfFiles=\"{}\" installKBytes=\"1\"/>\n    <bundle-version/>\n    <upgrade-bundle/>\n    <update-bundle/>\n    <atomic-update-bundle/>\n    <strict-identifier/>\n    <relocate/>\n    <scripts/>\n</pkg-info>\n",
                self.identifier,
                self.version,
                self.payload.len()
            )
        })
    }

    /// The `Distribution` of a product archive: its own, or one component selected by default.
    pub fn distribution_text(&self) -> String {
        self.distribution.clone().unwrap_or_else(|| {
            format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<installer-gui-script minSpecVersion=\"1\"><pkg-ref id=\"{id}\"/><options customize=\"never\" require-scripts=\"false\"/><choices-outline><line choice=\"default\"><line choice=\"{id}\"/></line></choices-outline><choice id=\"default\"/><choice id=\"{id}\" visible=\"false\"><pkg-ref id=\"{id}\"/></choice><pkg-ref id=\"{id}\" version=\"{v}\" onConclusion=\"none\">#{name}.pkg</pkg-ref></installer-gui-script>\n",
                id = self.identifier,
                v = self.version,
                name = self.name,
            )
        })
    }

    /// The files a receipt of this package lists: the payload's files and links, relative.
    pub fn receipt_files(&self) -> Vec<String> {
        self.payload
            .iter()
            .filter(|(_, e)| matches!(e.content, Content::File(_) | Content::Link(_)))
            .map(|(path, _)| path.trim_start_matches('/').to_owned())
            .collect()
    }
}

fn type_bits(content: &Content) -> u32 {
    match content {
        Content::File(_) => 0o100000,
        Content::Dir => 0o040000,
        Content::Link(_) => 0o120000,
        Content::Fifo => 0o010000,
    }
}

// ---------------------------------------------------------------------------------------------
// The fake Mac
// ---------------------------------------------------------------------------------------------

/// One tool run, as the fake saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub tool: Tool,
    pub args: Vec<String>,
}

/// What a faulty tool does instead of answering.
#[derive(Clone)]
pub enum Fault {
    Output(ToolOutput),
    Spawn,
    TimedOut,
    /// Runs the closure when the tool is called, then answers as the tool does: a change of the
    /// host between two steps.
    Hook(std::sync::Arc<dyn Fn() + Send + Sync>),
}

impl Fault {
    /// A hook that runs `change`.
    pub fn hook(change: impl Fn() + Send + Sync + 'static) -> Fault {
        Fault::Hook(std::sync::Arc::new(change))
    }

    /// A hook that makes the host root a file: every later look at the host fails with
    /// `ENOTDIR`, which is no "not found".
    pub fn root_becomes_a_file(root: &Path) -> Fault {
        let root = root.to_owned();
        Fault::hook(move || {
            fs::rename(&root, root.with_extension("moved")).unwrap();
            fs::write(&root, b"").unwrap();
        })
    }
}

struct Rule {
    tool: Tool,
    prefix: Vec<String>,
    skip: usize,
    remaining: usize,
    fault: Fault,
}

/// A receipt the fake keeps: the version, the install prefix (always `/`) and the files, relative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeReceipt {
    pub version: String,
    pub files: Vec<String>,
}

#[derive(Default)]
struct State {
    pkgs: BTreeMap<String, FakePkg>,
    receipts: BTreeMap<String, FakeReceipt>,
    calls: Vec<Call>,
    rules: Vec<Rule>,
    enabled: BTreeMap<String, bool>,
}

/// `pkgutil`, `installer`, `lsbom` and `launchctl` over a scratch host root.
pub struct FakeMac {
    root: PathBuf,
    lock_path: PathBuf,
    ids: Mutex<Ids>,
    euid: u32,
    egid: u32,
    state: Mutex<State>,
}

impl FakeMac {
    pub fn new(root: &Path, lock_path: &Path, ids: Ids, euid: u32, egid: u32) -> FakeMac {
        FakeMac {
            root: root.to_owned(),
            lock_path: lock_path.to_owned(),
            ids: Mutex::new(ids),
            euid,
            egid,
            state: Mutex::new(State::default()),
        }
    }

    /// The package the Mac knows as `name`.
    pub fn pkg_named(&self, name: &str) -> FakePkg {
        self.state()
            .pkgs
            .get(name)
            .unwrap_or_else(|| panic!("the fake Mac knows no package {name}"))
            .clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn real(&self, logical: &str) -> PathBuf {
        self.root.join(logical.trim_start_matches('/'))
    }

    pub fn set_ids(&self, ids: Ids) {
        *self.ids.lock().unwrap() = ids;
    }

    /// Teaches the Mac a package.
    pub fn add(&self, pkg: FakePkg) {
        self.state().pkgs.insert(pkg.name.clone(), pkg);
    }

    /// Changes a package the Mac knows.
    pub fn edit(&self, name: &str, change: impl FnOnce(&mut FakePkg)) {
        change(self.state().pkgs.get_mut(name).unwrap());
    }

    /// The tool runs so far.
    pub fn calls(&self) -> Vec<Call> {
        self.state().calls.clone()
    }

    /// The arguments of each run of `tool`.
    pub fn calls_of(&self, tool: Tool) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.tool == tool)
            .map(|c| c.args)
            .collect()
    }

    /// The runs of `tool` whose first argument is `verb`.
    pub fn runs_of(&self, tool: Tool, verb: &str) -> usize {
        self.calls_of(tool)
            .iter()
            .filter(|args| args.first().map(String::as_str) == Some(verb))
            .count()
    }

    pub fn clear_calls(&self) {
        self.state().calls.clear();
    }

    /// Makes every run of `tool` whose arguments start with `prefix` end as `fault`.
    pub fn fault(&self, tool: Tool, prefix: &[&str], fault: Fault) {
        self.fault_nth(tool, prefix, 0, usize::MAX, fault);
    }

    /// Lets `skip` matching runs pass, then makes the next `times` end as `fault`.
    pub fn fault_nth(&self, tool: Tool, prefix: &[&str], skip: usize, times: usize, fault: Fault) {
        self.state().rules.push(Rule {
            tool,
            prefix: prefix.iter().map(|s| (*s).to_owned()).collect(),
            skip,
            remaining: times,
            fault,
        });
    }

    /// Makes the matching runs exit with `code` printing `stderr`.
    pub fn exit(&self, tool: Tool, prefix: &[&str], code: i32, stderr: &str) {
        self.fault(tool, prefix, Fault::Output(ToolOutput::exit(code, stderr)));
    }

    pub fn clear_faults(&self) {
        self.state().rules.clear();
    }

    pub fn receipts(&self) -> BTreeMap<String, FakeReceipt> {
        self.state().receipts.clone()
    }

    pub fn receipt(&self, identifier: &str) -> Option<FakeReceipt> {
        self.state().receipts.get(identifier).cloned()
    }

    /// A receipt of `identifier` as if installed by another package: no files are made.
    pub fn set_receipt(&self, identifier: &str, version: &str, files: &[&str]) {
        let receipt = FakeReceipt {
            version: version.into(),
            files: files.iter().map(|f| (*f).to_owned()).collect(),
        };
        self.write_receipt(identifier, &receipt);
        self.state().receipts.insert(identifier.into(), receipt);
    }

    /// `pkgutil --forget` by hand.
    pub fn forget(&self, identifier: &str) {
        self.state().receipts.remove(identifier);
        let _ = fs::remove_file(self.receipt_path(identifier));
    }

    /// Whether `launchctl enable` (true) or `disable` (false) last ran for `label`.
    pub fn enabled(&self, label: &str) -> Option<bool> {
        self.state().enabled.get(label).copied()
    }

    /// `installer -pkg` by hand: installs the package `name` as `installer` does, with no lock
    /// asserted and no run recorded.
    pub fn install_by_hand(&self, name: &str) {
        let mut st = self.state();
        let pkg = st.pkgs.get(name).unwrap().clone();
        self.install_payload(&mut st, &pkg, true);
    }

    fn receipt_path(&self, identifier: &str) -> PathBuf {
        self.real(&format!("/var/db/receipts/{identifier}.plist"))
    }

    fn write_receipt(&self, identifier: &str, receipt: &FakeReceipt) {
        let path = self.receipt_path(identifier);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>PackageIdentifier</key><string>{identifier}</string><key>PackageVersion</key><string>{}</string><key>InstallPrefixPath</key><string>/</string></dict></plist>\n",
            receipt.version
        );
        fs::write(path, text).unwrap();
    }

    /// The package an artifact file names.
    fn pkg_of(st: &State, file: &str) -> FakePkg {
        let text = fs::read_to_string(file).unwrap();
        let name = text.strip_prefix("fake-pkg:").unwrap().trim();
        st.pkgs
            .get(name)
            .unwrap_or_else(|| panic!("the fake Mac knows no package {name}"))
            .clone()
    }

    /// Writes the package's expansion at `dir`, as `pkgutil --expand-full` does.
    pub fn materialize(&self, pkg: &FakePkg, dir: &Path) {
        let comp = if pkg.product {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("Distribution"), pkg.distribution_text()).unwrap();
            for extra in &pkg.extras {
                fs::create_dir_all(dir.join(extra)).unwrap();
            }
            dir.join(format!("{}.pkg", pkg.name))
        } else {
            dir.to_owned()
        };
        fs::create_dir_all(&comp).unwrap();
        fs::write(comp.join("PackageInfo"), pkg.package_info_text()).unwrap();
        fs::write(comp.join("Bom"), format!("fake-bom:{}", pkg.name)).unwrap();
        for extra in &pkg.component_extras {
            fs::create_dir_all(comp.join(extra)).unwrap();
            fs::write(comp.join(extra).join("postinstall"), b"#!/bin/sh\n").unwrap();
        }
        if !pkg.has_payload {
            return;
        }
        let payload = comp.join("Payload");
        fs::create_dir_all(&payload).unwrap();
        for (path, entry) in &pkg.payload {
            let real = payload.join(path.trim_start_matches('/'));
            match &entry.content {
                Content::Dir => fs::create_dir_all(&real).unwrap(),
                Content::File(bytes) => {
                    fs::create_dir_all(real.parent().unwrap()).unwrap();
                    fs::write(&real, bytes).unwrap();
                    fs::set_permissions(&real, fs::Permissions::from_mode(entry.mode)).unwrap();
                }
                Content::Link(target) => {
                    fs::create_dir_all(real.parent().unwrap()).unwrap();
                    symlink(target, &real).unwrap();
                }
                Content::Fifo => {
                    fs::create_dir_all(real.parent().unwrap()).unwrap();
                    hostkeeper_testkit::fifo(&real, entry.mode as u16).unwrap();
                }
            }
        }
        for (path, entry) in pkg.payload.iter().rev() {
            if matches!(entry.content, Content::Dir) {
                let real = payload.join(path.trim_start_matches('/'));
                fs::set_permissions(real, fs::Permissions::from_mode(entry.mode)).unwrap();
            }
        }
    }

    /// The listing `lsbom -p fmug` prints for the Bom of `pkg`.
    pub fn bom_listing(&self, pkg: &FakePkg) -> String {
        let ids = *self.ids.lock().unwrap();
        let (uid, gid) = (ids.uid(self.euid), ids.gid(self.egid));
        let mut out = format!(".\t40755\t{uid}\t{gid}\n");
        for (path, entry) in &pkg.payload {
            let mode = type_bits(&entry.content) | entry.bom_mode.unwrap_or(entry.mode);
            out.push_str(&format!(".{path}\t{mode:o}\t{uid}\t{gid}\n"));
        }
        out
    }

    /// What `installer` puts on the host for `pkg`, and the receipt it records.
    fn install_payload(&self, st: &mut State, pkg: &FakePkg, receipt: bool) {
        for (path, entry) in &pkg.payload {
            let real = self.real(path);
            match &entry.content {
                Content::Dir => {
                    if !real.exists() {
                        mkdir_p(&real);
                        fs::set_permissions(&real, fs::Permissions::from_mode(entry.mode)).unwrap();
                    }
                }
                Content::File(bytes) => {
                    mkdir_p(real.parent().unwrap());
                    let _ = fs::remove_file(&real);
                    fs::write(&real, bytes).unwrap();
                    fs::set_permissions(&real, fs::Permissions::from_mode(entry.mode)).unwrap();
                }
                Content::Link(target) => {
                    mkdir_p(real.parent().unwrap());
                    let _ = fs::remove_file(&real);
                    symlink(target, &real).unwrap();
                }
                Content::Fifo => {}
            }
        }
        if let Install::OtherBytes { path, bytes } = &pkg.install {
            fs::write(self.real(path), bytes).unwrap();
        }
        if receipt {
            let recorded = FakeReceipt {
                version: pkg.version.clone(),
                files: pkg.receipt_files(),
            };
            self.write_receipt(&pkg.identifier, &recorded);
            st.receipts.insert(pkg.identifier.clone(), recorded);
        }
    }

    fn take_fault(st: &mut State, tool: Tool, args: &[String]) -> Option<Fault> {
        for rule in &mut st.rules {
            if rule.tool != tool || !args.starts_with(&rule.prefix) || rule.remaining == 0 {
                continue;
            }
            if rule.skip > 0 {
                rule.skip -= 1;
                continue;
            }
            rule.remaining -= 1;
            return Some(rule.fault.clone());
        }
        None
    }

    fn pkgutil(&self, st: &mut State, args: &[String]) -> ToolOutput {
        let arg = |n: usize| args.get(n).map(String::as_str).unwrap_or_default();
        match arg(0) {
            "--check-signature" => {
                let pkg = Self::pkg_of(st, arg(1));
                ToolOutput {
                    code: Some(pkg.check.0),
                    stdout: pkg.check.1.into_bytes(),
                    stderr: Vec::new(),
                }
            }
            "--expand-full" => {
                let pkg = Self::pkg_of(st, arg(1));
                self.materialize(&pkg, Path::new(arg(2)));
                ToolOutput::success("")
            }
            "--pkg-info-plist" => match st.receipts.get(arg(1)) {
                Some(receipt) => ToolOutput::success(&format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>pkgid</key><string>{}</string><key>pkg-version</key><string>{}</string><key>volume</key><string>/</string><key>install-location</key><string>/</string></dict></plist>\n",
                    arg(1),
                    receipt.version
                )),
                None => no_receipt(arg(1)),
            },
            "--file-info-plist" => {
                let path = arg(1);
                let owners: Vec<&String> = st
                    .receipts
                    .iter()
                    .filter(|(_, r)| r.files.iter().any(|f| format!("/{f}") == path))
                    .map(|(id, _)| id)
                    .collect();
                let infos: String = owners
                    .iter()
                    .map(|id| format!("<dict><key>pkgid</key><string>{id}</string></dict>"))
                    .collect();
                let info = match owners.is_empty() {
                    true => String::new(),
                    false => format!("<key>path-info</key><array>{infos}</array>"),
                };
                ToolOutput::success(&format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>path</key><string>{path}</string>{info}</dict></plist>\n"
                ))
            }
            "--only-files" => match st.receipts.get(arg(2)) {
                Some(receipt) => ToolOutput::success(&format!("{}\n", receipt.files.join("\n"))),
                None => no_receipt(arg(2)),
            },
            "--forget" => match st.receipts.remove(arg(1)) {
                Some(_) => {
                    let _ = fs::remove_file(self.receipt_path(arg(1)));
                    ToolOutput::success(&format!("Forgot package '{}' on '/'.\n", arg(1)))
                }
                None => no_receipt(arg(1)),
            },
            other => panic!("the fake pkgutil does not know {other}"),
        }
    }

    fn installer(&self, st: &mut State, args: &[String]) -> ToolOutput {
        let [allow, flag, pkg, target, volume] = args else {
            panic!("installer ran with {args:?}");
        };
        assert_eq!(
            [
                allow.as_str(),
                flag.as_str(),
                target.as_str(),
                volume.as_str()
            ],
            ["-allowUntrusted", "-pkg", "-target", "/"],
            "installer must run with -allowUntrusted -pkg <path> -target /"
        );
        let held = InstallerLock::new(&self.lock_path).try_hold().unwrap();
        assert!(
            held.is_none(),
            "installer ran without the installer lock held"
        );
        if !pkg.ends_with(".pkg") {
            return ToolOutput {
                code: Some(1),
                stdout: Vec::new(),
                stderr: format!(
                    "installer: Error - the package path specified was invalid: '{pkg}'.\n"
                )
                .into_bytes(),
            };
        }
        let pkg = Self::pkg_of(st, pkg);
        match &pkg.install {
            Install::Fails {
                code,
                stdout,
                stderr,
            } => ToolOutput {
                code: Some(*code),
                stdout: stdout.clone().into_bytes(),
                stderr: stderr.clone().into_bytes(),
            },
            Install::KeepsOldReceipt => {
                self.install_payload(st, &pkg, false);
                ToolOutput::success("installer: The install was successful.\n")
            }
            Install::Normal | Install::OtherBytes { .. } => {
                self.install_payload(st, &pkg, true);
                ToolOutput::success("installer: The install was successful.\n")
            }
        }
    }

    fn lsbom(&self, st: &State, args: &[String]) -> ToolOutput {
        assert_eq!(
            &args[..2],
            ["-p", "fmug"],
            "lsbom must list path, mode, uid, gid"
        );
        let bom = fs::read_to_string(&args[2]).unwrap();
        let name = bom.strip_prefix("fake-bom:").unwrap();
        let pkg = &st.pkgs[name];
        match &pkg.lsbom {
            Some((code, text)) => ToolOutput {
                code: Some(*code),
                stdout: text.clone().into_bytes(),
                stderr: Vec::new(),
            },
            None => ToolOutput::success(&self.bom_listing(pkg)),
        }
    }

    fn launchctl(st: &mut State, args: &[String]) -> ToolOutput {
        let label = args[1].strip_prefix("system/").unwrap();
        match args[0].as_str() {
            verb @ ("enable" | "disable") => {
                st.enabled.insert(label.into(), verb == "enable");
                ToolOutput::success("")
            }
            other => panic!("the fake launchctl does not know {other}"),
        }
    }
}

/// Creates `path` and the directories above it that are missing, each 0755 whatever the umask.
pub fn mkdir_p(path: &Path) {
    if path.exists() {
        return;
    }
    mkdir_p(path.parent().unwrap());
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn no_receipt(identifier: &str) -> ToolOutput {
    ToolOutput::exit(1, &format!("No receipt for '{identifier}' found at '/'.\n"))
}

impl Tools for FakeMac {
    fn run(
        &self,
        tool: Tool,
        args: &[String],
        _deadline: Duration,
    ) -> Result<ToolOutput, ToolError> {
        let mut st = self.state();
        st.calls.push(Call {
            tool,
            args: args.to_vec(),
        });
        if let Some(fault) = Self::take_fault(&mut st, tool, args) {
            match fault {
                Fault::Output(out) => return Ok(out),
                Fault::Spawn => {
                    return Err(ToolError::Spawn {
                        args: args.join(" "),
                        why: "no such file".into(),
                    });
                }
                Fault::TimedOut => {
                    return Err(ToolError::TimedOut {
                        args: args.join(" "),
                    });
                }
                Fault::Hook(change) => change(),
            }
        }
        Ok(match tool {
            Tool::Pkgutil => self.pkgutil(&mut st, args),
            Tool::Installer => self.installer(&mut st, args),
            Tool::Lsbom => self.lsbom(&st, args),
            Tool::Launchctl => Self::launchctl(&mut st, args),
        })
    }
}
