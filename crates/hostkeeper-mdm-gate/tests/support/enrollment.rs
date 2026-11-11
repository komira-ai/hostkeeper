//! What the enrollment side's tests share: a fake of the coordinator's enrollment (the
//! allowlist, the waiting requests, the last approval, the nonces it issues and the audit lines
//! it is given), a gate with the enrollment side over a scratch directory that a test can
//! restart, with the pkg install when a test gives it an allowlist, and readers of the
//! profiles and pkg installs in the commands NanoHUB heard.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_enrollment::fs::{FileLock, FileStat, Fs, RealFs};
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_mdm_gate::{
    Baseline, ConfigProfile, DaemonProfile, EnrollSide, Enrollments, EnrollmentsError, Gate,
    Install, MdmHost, Nonce, PkgAllowlist, ProfileIdentifier, Random, StoreError, Waiting,
};
use plist::{Dictionary, Value};

use super::{Clock, FakeNanoHub, Heard, Rig, UDID, counting, open_on};

/// The MDM host's name in the tests.
pub const HOST: &str = "mdm.example.com";

/// The farm's profile identifier in the tests.
pub const IDENTIFIER: &str = "com.example.hostkeeper.mdm";

/// A daemon config the daemon's loader accepts: design 4.1's example.
pub const CONFIG: &str = r#"coordinator     = "https://coordinator.example:8443"
ca_sha256       = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow     = ".github/workflows/release.yml"
refs         = ["refs/tags/*"]
hosted_runner = true

[bounds]
max_package_bytes      = 536870912
max_unpacked_bytes     = 2147483648
max_entries            = 20000
fetch_timeout_secs     = 2100
attestation_timeout_secs = 60
force_start_secs       = 60
"#;

#[derive(Default)]
struct State {
    allowed: BTreeSet<String>,
    groups: BTreeMap<String, String>,
    waiting: BTreeMap<String, Vec<Waiting>>,
    approved: BTreeMap<String, u64>,
    fail_reads: bool,
    fail_waiting: bool,
    fail_issue: bool,
    issued: Vec<(String, KeyHash, Vec<u8>)>,
    audit: Vec<(String, String)>,
}

/// The coordinator's enrollment, as a test sets it.
#[derive(Default)]
pub struct FakeEnrollments(Mutex<State>);

impl FakeEnrollments {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `serial` is on the allowlist from now on.
    pub fn allow(&self, serial: &str) {
        self.state().allowed.insert(serial.to_owned());
    }

    /// `serial`'s allowlist entry names the host group `group`.
    pub fn group(&self, serial: &str, group: &str) {
        self.state()
            .groups
            .insert(serial.to_owned(), group.to_owned());
    }

    /// `request` waits for an operator, claiming `serial`.
    pub fn wait(&self, serial: &str, request: Waiting) {
        self.state()
            .waiting
            .entry(serial.to_owned())
            .or_default()
            .push(request);
    }

    /// A request claiming `serial` was approved at `at`.
    pub fn approve(&self, serial: &str, at: u64) {
        self.state().approved.insert(serial.to_owned(), at);
    }

    /// From now on the reads fail (or not).
    pub fn fail_reads(&self, fail: bool) {
        self.state().fail_reads = fail;
    }

    /// From now on the list of waiting requests fails (or not).
    pub fn fail_waiting(&self, fail: bool) {
        self.state().fail_waiting = fail;
    }

    /// From now on no nonce is issued (or one is).
    pub fn fail_issue(&self, fail: bool) {
        self.state().fail_issue = fail;
    }

    /// The nonces issued: serial, request, bytes.
    pub fn issued(&self) -> Vec<(String, KeyHash, Vec<u8>)> {
        self.state().issued.clone()
    }

    /// The audit lines: action, reason.
    pub fn audit_lines(&self) -> Vec<(String, String)> {
        self.state().audit.clone()
    }
}

impl Enrollments for FakeEnrollments {
    fn allowed(&self, serial: &str) -> bool {
        self.state().allowed.contains(serial)
    }

    fn host_group(&self, serial: &str) -> Option<String> {
        self.state().groups.get(serial).cloned()
    }

    fn waiting(&self, serial: &str) -> Result<Vec<Waiting>, EnrollmentsError> {
        let state = self.state();
        if state.fail_reads || state.fail_waiting {
            return Err(EnrollmentsError("the store cannot be read".to_owned()));
        }
        Ok(state.waiting.get(serial).cloned().unwrap_or_default())
    }

    fn last_approval_unix(&self, serial: &str) -> Result<Option<u64>, EnrollmentsError> {
        let state = self.state();
        if state.fail_reads {
            return Err(EnrollmentsError("the store cannot be read".to_owned()));
        }
        Ok(state.approved.get(serial).copied())
    }

    fn issue_nonce(&self, serial: &str, request_id: &KeyHash) -> Result<Nonce, EnrollmentsError> {
        let mut state = self.state();
        if state.fail_issue {
            return Err(EnrollmentsError("no fresh random bytes".to_owned()));
        }
        // Bytes no other text of a test holds: 0xE0 and up, one value per nonce.
        let n = u8::try_from(state.issued.len()).expect("few nonces");
        let bytes = vec![0xE0 + n; 32];
        state
            .issued
            .push((serial.to_owned(), *request_id, bytes.clone()));
        Ok(Nonce::new(bytes))
    }

    fn audit(&self, action: &str, reason: &str) {
        self.state()
            .audit
            .push((action.to_owned(), reason.to_owned()));
    }
}

/// The key hash whose 32 bytes are all `n`.
pub fn key_hash(n: u8) -> KeyHash {
    KeyHash::from_bytes(&[n; 32]).expect("32 bytes")
}

/// A request `n` (key hash of bytes `n`) claiming the platform UUID `uuid`, posted at `posted`.
pub fn request(n: u8, uuid: &str, posted: u64) -> Waiting {
    Waiting {
        request_id: key_hash(n),
        platform_uuid: uuid.to_owned(),
        posted_unix: posted,
        duplicate_claim: false,
    }
}

/// The enrollment side over `fake`.
pub fn side(fake: &Arc<FakeEnrollments>) -> EnrollSide {
    side_of(fake, config())
}

/// The config profile of [`CONFIG`] alone.
pub fn config() -> ConfigProfile {
    ConfigProfile::new(DaemonProfile::from_toml(CONFIG).expect("the loader accepts it"))
}

/// The enrollment side over `fake` with the config profile `config`.
pub fn side_of(fake: &Arc<FakeEnrollments>, config: ConfigProfile) -> EnrollSide {
    EnrollSide {
        identifier: ProfileIdentifier::parse(IDENTIFIER).expect("an identifier"),
        config,
        enrollments: Arc::clone(fake) as Arc<dyn Enrollments>,
    }
}

/// A gate with the enrollment side over a fake enrollment.
pub struct EnrollRig {
    pub rig: Rig,
    pub fake: Arc<FakeEnrollments>,
    /// The pkg allowlist's text, when the gate has the pkg install.
    allowlist: Option<String>,
    /// The file system the gate keeps its records through.
    fs: Arc<dyn Fs>,
    /// The config profile the gate sends.
    config: ConfigProfile,
    /// The enrollment baseline the gate applies, when a test gives it one.
    baseline: Option<Baseline>,
}

impl EnrollRig {
    pub async fn new(name: &str) -> Self {
        Self::with_random(name, counting()).await
    }

    /// A rig whose gate draws its UUIDs from `random`.
    pub async fn with_random(name: &str, random: Random) -> Self {
        Self::on(name, Arc::new(RealFs), random).await
    }

    /// A rig whose gate keeps its records through `fs`.
    pub async fn on_fs(name: &str, fs: Arc<dyn Fs>) -> Self {
        Self::on(name, fs, counting()).await
    }

    async fn on(name: &str, fs: Arc<dyn Fs>, random: Random) -> Self {
        let mut rig = Rig::new(name).await;
        let fake = Arc::new(FakeEnrollments::default());
        let config = config();
        rig.gate = gate_on(&rig, &fake, random, Arc::clone(&fs), None, &config, None)
            .expect("the gate opens");
        Self {
            rig,
            fake,
            allowlist: None,
            fs,
            config,
            baseline: None,
        }
    }

    /// The same rig whose gate applies `baseline` (`None`: none), as a new gate over the same
    /// directory and fake, as after a restart of the coordinator with another baseline.
    pub fn baseline(&mut self, baseline: Option<Baseline>) {
        self.baseline = baseline;
        self.restart().expect("the gate opens");
    }

    /// The same rig whose gate has the pkg install of the allowlist `text`, as a new gate over
    /// the same directory and fake.
    pub fn install(&mut self, text: &str) {
        self.install_with(text, counting());
    }

    /// [`Self::install`], the new gate drawing its UUIDs from `random`.
    pub fn install_with(&mut self, text: &str, random: Random) {
        self.allowlist = Some(text.to_owned());
        self.rig.gate = gate_on(
            &self.rig,
            &self.fake,
            random,
            Arc::clone(&self.fs),
            self.allowlist.as_deref(),
            &self.config,
            self.baseline.clone(),
        )
        .expect("the gate opens");
    }

    /// A new gate over the same directory and fake whose config profile is `config`, drawing
    /// its UUIDs from `random`, as after a restart of the coordinator with other flags.
    pub fn reconfigure(&mut self, config: ConfigProfile, random: Random) {
        self.config = config;
        self.rig.gate = gate_on(
            &self.rig,
            &self.fake,
            random,
            Arc::clone(&self.fs),
            self.allowlist.as_deref(),
            &self.config,
            self.baseline.clone(),
        )
        .expect("the gate opens");
    }

    /// A new gate with the enrollment side over the same directory and fake, as after a
    /// restart of the coordinator: its memory is gone.
    pub fn restart(&mut self) -> Result<(), StoreError> {
        self.rig.gate = gate_on(
            &self.rig,
            &self.fake,
            counting(),
            Arc::new(RealFs),
            self.allowlist.as_deref(),
            &self.config,
            self.baseline.clone(),
        )?;
        Ok(())
    }

    pub fn gate(&self) -> &Gate {
        &self.rig.gate
    }

    pub fn hub(&self) -> &FakeNanoHub {
        &self.rig.hub
    }

    pub fn clock(&self) -> &Clock {
        &self.rig.clock
    }

    /// Enrolls the device [`UDID`] at the clock's time, with `serial` and `supervised`, and
    /// forgets the queries it sent.
    pub async fn enroll(&self, serial: &str, supervised: bool) {
        self.rig.enroll(UDID, serial, supervised).await;
    }

    /// The profile commands NanoHUB heard: `InstallProfile` and `RemoveProfile`, in order.
    pub fn profile_commands(&self) -> Vec<Heard> {
        self.hub()
            .heard()
            .into_iter()
            .filter(|heard| {
                let (kind, _) = heard.command();
                kind == "InstallProfile" || kind == "RemoveProfile"
            })
            .collect()
    }

    /// The profiles installed, in order, as (identifier, settings of its one payload).
    pub fn installed(&self) -> Vec<(String, Dictionary)> {
        self.profile_commands()
            .iter()
            .filter(|heard| heard.command().0 == "InstallProfile")
            // The gate's own profiles; a baseline profile carries other settings.
            .filter(|heard| {
                profile_dict(heard)["PayloadIdentifier"]
                    .as_string()
                    .is_some_and(|identifier| identifier.starts_with(IDENTIFIER))
            })
            .map(profile_of)
            .collect()
    }

    /// The identifiers of the profiles removed, in order.
    pub fn removed(&self) -> Vec<String> {
        self.profile_commands()
            .iter()
            .filter(|heard| heard.command().0 == "RemoveProfile")
            .map(|heard| {
                command_dict(heard)["Identifier"]
                    .as_string()
                    .expect("an identifier")
                    .to_owned()
            })
            .collect()
    }

    /// The nonce profiles installed, in order, as (nonce bytes, request id).
    pub fn nonces_pushed(&self) -> Vec<(Vec<u8>, String)> {
        self.installed()
            .into_iter()
            .filter(|(identifier, _)| identifier == &format!("{IDENTIFIER}.nonce"))
            .map(|(_, settings)| {
                let nonce = settings["nonce"].as_data().expect("data").to_vec();
                let request = settings["request_id"].as_string().expect("text").to_owned();
                (nonce, request)
            })
            .collect()
    }

    /// The `InstallEnterpriseApplication` commands NanoHUB heard, in order, as their
    /// `Command` dictionaries.
    pub fn installs(&self) -> Vec<Dictionary> {
        self.hub()
            .heard()
            .iter()
            .filter(|heard| heard.command().0 == "InstallEnterpriseApplication")
            .map(command_dict)
            .collect()
    }

    /// The UUID of the last command of `kind` NanoHUB heard.
    pub fn last_uuid(&self, kind: &str) -> String {
        self.hub()
            .commands()
            .into_iter()
            .filter(|(heard, _)| heard == kind)
            .map(|(_, uuid)| uuid)
            .next_back()
            .expect("a command of that kind")
    }
}

fn gate_on(
    rig: &Rig,
    fake: &Arc<FakeEnrollments>,
    random: Random,
    fs: Arc<dyn Fs>,
    allowlist: Option<&str>,
    config: &ConfigProfile,
    baseline: Option<Baseline>,
) -> Result<Gate, StoreError> {
    let install = allowlist.map(|text| Install {
        allowlist: PkgAllowlist::parse(text).expect("the allowlist parses"),
        host: MdmHost::parse(HOST).expect("a host"),
        fs: Arc::new(RealFs),
    });
    Ok(open_on(fs, &rig.dir(), &rig.hub, &rig.clock, random)?
        .with_enrollment(side_of(fake, config.clone()))
        .with_install(install)
        .with_baseline(baseline))
}

/// A file system whose `read_private` gives the bytes of the real file with the stat a test
/// sets: a file of root's cannot be made without root.
pub struct StatFs(Mutex<FileStat>);

impl StatFs {
    pub fn new(uid: u32, mode: u32, is_file: bool) -> Self {
        Self(Mutex::new(FileStat { is_file, uid, mode }))
    }

    /// A regular file of root's, mode 0644.
    pub fn root() -> Self {
        Self::new(0, 0o644, true)
    }
}

impl Fs for StatFs {
    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        RealFs.read(path)
    }
    fn read_private(&self, path: &Path) -> std::io::Result<Option<(Vec<u8>, FileStat)>> {
        let stat = *self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(RealFs.read_private(path)?.map(|(bytes, _)| (bytes, stat)))
    }
    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
        RealFs.write_new(path, bytes, mode)
    }
    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
        RealFs.sync_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        RealFs.rename(from, to)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        RealFs.remove(path)
    }
    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        RealFs.sync_dir(dir)
    }
    fn create_dir(&self, dir: &Path) -> std::io::Result<()> {
        RealFs.create_dir(dir)
    }
    fn list(&self, dir: &Path) -> std::io::Result<Vec<String>> {
        RealFs.list(dir)
    }
    fn lock(&self, path: &Path) -> std::io::Result<FileLock> {
        RealFs.lock(path)
    }
}

/// The `Command` dictionary of a command NanoHUB heard.
pub fn command_dict(heard: &Heard) -> Dictionary {
    Value::from_reader(std::io::Cursor::new(&heard.body))
        .expect("a plist")
        .into_dictionary()
        .expect("a dictionary")["Command"]
        .as_dictionary()
        .expect("a Command")
        .clone()
}

/// The whole profile of an `InstallProfile` NanoHUB heard.
pub fn profile_dict(heard: &Heard) -> Dictionary {
    let payload = command_dict(heard)["Payload"]
        .as_data()
        .expect("a Payload")
        .to_vec();
    Value::from_reader(std::io::Cursor::new(payload))
        .expect("a profile plist")
        .into_dictionary()
        .expect("a dictionary")
}

/// The identifier of an `InstallProfile`'s profile, and the settings its one payload forces.
pub fn profile_of(heard: &Heard) -> (String, Dictionary) {
    let profile = profile_dict(heard);
    let identifier = profile["PayloadIdentifier"]
        .as_string()
        .expect("an identifier")
        .to_owned();
    let payload = profile["PayloadContent"].as_array().expect("an array")[0]
        .as_dictionary()
        .expect("a payload")
        .clone();
    let domains = payload["PayloadContent"].as_dictionary().expect("domains");
    let (_, domain) = domains.iter().next().expect("one domain");
    let settings = domain.as_dictionary().expect("a domain")["Forced"]
        .as_array()
        .expect("Forced")[0]
        .as_dictionary()
        .expect("a forced entry")["mcx_preference_settings"]
        .as_dictionary()
        .expect("settings")
        .clone();
    (identifier, settings)
}
